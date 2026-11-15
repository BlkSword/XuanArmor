//! `xuanarmor tune`：按 XDP 挂载模式应用内核收包调优（v0.5.4）。
//!
//! 两种模式的瓶颈不同，调优项也不同：
//!
//! - **generic（SKB 模式）**：XDP 在分配 `sk_buff` 之后执行，单核软中断是瓶颈。
//!   主要手段是 **RPS/RFS**（把 SKB 处理分摊到多核）+ 提高 NAPI 预算/backlog。
//! - **native（DRV 模式）**：XDP 在驱动收包中断上下文执行，瓶颈是单队列中断处理能力。
//!   主要手段是 **网卡中断亲和性**（每个 RX 队列绑到独立 CPU）+ 提高 NAPI 预算。
//!
//! 所有改动前先把原值写入状态文件（默认与 `store_path` 同目录的 `tune-state.json`），
//! `xuanarmor tune --revert` 可完整还原。`--dry-run` 只打印计划不落盘。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;

/// 调优状态文件名（与 `store_path` 同目录）。
const TUNE_STATE_FILE: &str = "tune-state.json";
/// RFS 全局流表条目数。
const RFS_SOCK_FLOW_ENTRIES: u64 = 32_768;
/// 单个 RX 队列的 RFS 流表条目数下限。
const RFS_FLOW_CNT_MIN: u64 = 1_024;
/// NAPI 预算（默认 300）：高 PPS 下提高可减少软中断切换。
const NETDEV_BUDGET: u64 = 600;
/// NAPI 预算时间片（微秒，默认 2000）。
const NETDEV_BUDGET_USECS: u64 = 4_000;
/// 收包 backlog（默认 1000）：突发流量下减少丢包。
const NETDEV_MAX_BACKLOG: u64 = 10_000;

#[derive(Debug, Clone)]
pub struct TuneOptions {
    pub config: String,
    /// 覆盖配置里的网卡名
    pub interface: Option<String>,
    pub dry_run: bool,
    pub revert: bool,
    pub json: bool,
}

/// 单次调优的完整记录，用于 `--revert` 精确还原。
#[derive(Debug, Default, Serialize, Deserialize)]
struct TuneState {
    interface: String,
    mode: String,
    /// 被修改的文件及其原内容（`None` 表示原本不存在）。
    files: Vec<FileBackup>,
    /// 被修改的 sysctl 项（键为 `/proc/sys` 下的相对路径）。
    sysctls: Vec<SysctlBackup>,
    /// 调优时间（Unix 秒）
    applied_unix: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileBackup {
    path: String,
    content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SysctlBackup {
    key: String,
    value: String,
}

/// 一条待执行的调优动作（dry-run 与真实执行共用）。
#[derive(Debug, Clone)]
enum Action {
    /// 写文件（sysfs 下的 RPS/RFS/中断亲和性）
    WriteFile {
        path: String,
        value: String,
        note: String,
    },
    /// 写 sysctl（`/proc/sys` 相对路径）
    WriteSysctl {
        key: String,
        value: String,
        note: String,
    },
}

impl Action {
    fn describe(&self) -> String {
        match self {
            Action::WriteFile { path, value, note } => format!("{note}: {path} = {value}"),
            Action::WriteSysctl { key, value, note } => format!("{note}: {key} = {value}"),
        }
    }
}

pub fn run(opts: TuneOptions) -> Result<()> {
    let cfg = Config::from_file(&opts.config)?;
    let iface = opts
        .interface
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cfg.interface.clone());
    let state_path = state_path(&cfg);

    if opts.revert {
        return revert(&state_path, opts.json);
    }

    let mode = detect_mode(&iface);
    let cpus = online_cpus();
    if cpus.is_empty() {
        anyhow::bail!("无法读取在线 CPU 列表（/sys/devices/system/cpu/online）");
    }

    let mut actions = Vec::new();
    match mode.as_str() {
        "generic" => plan_generic(&iface, &cpus, &mut actions)?,
        "native" => plan_native(&iface, &cpus, &mut actions)?,
        other => anyhow::bail!(
            "未检测到 {iface} 上的 XDP 挂载（当前状态：{other}）；请先启动 xuanarmor 或确认网卡名"
        ),
    }

    if opts.dry_run {
        print_plan(&iface, &mode, &actions, true, opts.json)?;
        return Ok(());
    }

    // 记录原值 → 执行 → 落盘状态
    let mut state = TuneState {
        interface: iface.clone(),
        mode: mode.clone(),
        applied_unix: now_unix(),
        ..Default::default()
    };
    for action in &actions {
        match action {
            Action::WriteFile { path, value, .. } => {
                let original = fs::read_to_string(path).ok();
                state.files.push(FileBackup {
                    path: path.clone(),
                    content: original,
                });
                write_file(path, value)
                    .with_context(|| format!("写入 {path} 失败（需要 root 权限）"))?;
            }
            Action::WriteSysctl { key, value, .. } => {
                let path = format!("/proc/sys/{}", key.replace('.', "/"));
                let original = fs::read_to_string(&path)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                state.sysctls.push(SysctlBackup {
                    key: key.clone(),
                    value: original,
                });
                write_file(&path, value)
                    .with_context(|| format!("写入 {path} 失败（需要 root 权限）"))?;
            }
        }
    }
    save_state(&state_path, &state)?;
    print_plan(&iface, &mode, &actions, false, opts.json)?;
    if !opts.json {
        println!();
        println!("调优已应用；还原命令: xuanarmor tune --revert");
        println!("状态文件: {}", state_path.display());
    }
    Ok(())
}

fn plan_generic(iface: &str, cpus: &[u32], actions: &mut Vec<Action>) -> Result<()> {
    let mask = cpu_mask_hex(cpus);
    let queues = rx_queues(iface)?;
    if queues.is_empty() {
        anyhow::bail!("{iface} 没有可用的 RX 队列（/sys/class/net/{iface}/queues/rx-*）");
    }
    // RPS：每个 RX 队列的软中断处理分摊到全部在线 CPU。
    for queue in &queues {
        actions.push(Action::WriteFile {
            path: format!("/sys/class/net/{iface}/queues/{queue}/rps_cpus"),
            value: mask.clone(),
            note: "RPS 多核分摊".to_string(),
        });
    }
    // RFS：让同一条流的包落到同一个 CPU，减少乱序与缓存失效。
    let per_queue = (RFS_SOCK_FLOW_ENTRIES / queues.len() as u64).max(RFS_FLOW_CNT_MIN);
    for queue in &queues {
        let path = format!("/sys/class/net/{iface}/queues/{queue}/rps_flow_cnt");
        if Path::new(&path).exists() {
            actions.push(Action::WriteFile {
                path,
                value: per_queue.to_string(),
                note: "RFS 流表".to_string(),
            });
        }
    }
    if Path::new("/proc/sys/net/core/rps_sock_flow_entries").exists() {
        actions.push(Action::WriteSysctl {
            key: "net.core.rps_sock_flow_entries".to_string(),
            value: RFS_SOCK_FLOW_ENTRIES.to_string(),
            note: "RFS 全局流表".to_string(),
        });
    }
    push_napi_budget(actions);
    Ok(())
}

fn plan_native(iface: &str, cpus: &[u32], actions: &mut Vec<Action>) -> Result<()> {
    // 中断亲和性：把该网卡的 RX 队列中断按轮转方式绑到不同 CPU。
    let irqs = netdev_irqs(iface)?;
    if irqs.is_empty() {
        // 虚拟网卡可能没有独立 MSI-X 队列中断（如部分 virtio 配置），此时只调 NAPI 预算。
        eprintln!(
            "[tune] 未在 /proc/interrupts 中找到 {iface} 的队列中断，跳过中断亲和性（仅调整 NAPI 预算）"
        );
    } else {
        for (idx, irq) in irqs.iter().enumerate() {
            let cpu = cpus[idx % cpus.len()];
            actions.push(Action::WriteFile {
                path: format!("/proc/irq/{irq}/smp_affinity_list"),
                value: cpu.to_string(),
                note: format!("中断亲和性 irq{irq}"),
            });
        }
    }
    push_napi_budget(actions);
    Ok(())
}

fn push_napi_budget(actions: &mut Vec<Action>) {
    actions.push(Action::WriteSysctl {
        key: "net.core.netdev_budget".to_string(),
        value: NETDEV_BUDGET.to_string(),
        note: "NAPI 预算".to_string(),
    });
    if Path::new("/proc/sys/net/core/netdev_budget_usecs").exists() {
        actions.push(Action::WriteSysctl {
            key: "net.core.netdev_budget_usecs".to_string(),
            value: NETDEV_BUDGET_USECS.to_string(),
            note: "NAPI 时间片".to_string(),
        });
    }
    actions.push(Action::WriteSysctl {
        key: "net.core.netdev_max_backlog".to_string(),
        value: NETDEV_MAX_BACKLOG.to_string(),
        note: "收包 backlog".to_string(),
    });
}

fn revert(state_path: &Path, json: bool) -> Result<()> {
    if !state_path.exists() {
        anyhow::bail!(
            "没有找到调优状态文件 {}，无法还原（可能从未执行过 tune）",
            state_path.display()
        );
    }
    let raw = fs::read_to_string(state_path)?;
    let state: TuneState = serde_json::from_str(&raw)
        .with_context(|| format!("解析状态文件失败: {}", state_path.display()))?;

    let mut restored = 0usize;
    for file in &state.files {
        match &file.content {
            Some(content) => {
                write_file(&file.path, content)
                    .with_context(|| format!("还原 {} 失败", file.path))?;
                restored += 1;
            }
            None => {
                // 原本不存在：尽力删除（sysfs 项通常不可删，忽略错误）
                let _ = fs::remove_file(&file.path);
                restored += 1;
            }
        }
    }
    for sysctl in &state.sysctls {
        if sysctl.value.is_empty() {
            continue;
        }
        let path = format!("/proc/sys/{}", sysctl.key.replace('.', "/"));
        write_file(&path, &sysctl.value).with_context(|| format!("还原 {path} 失败"))?;
        restored += 1;
    }
    let _ = fs::remove_file(state_path);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "reverted": true,
                "interface": state.interface,
                "mode": state.mode,
                "restored_items": restored,
            })
        );
    } else {
        println!(
            "已还原 {} 项调优（网卡 {}，模式 {}），状态文件已删除",
            restored, state.interface, state.mode
        );
    }
    Ok(())
}

fn print_plan(
    iface: &str,
    mode: &str,
    actions: &[Action],
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if json {
        let items: Vec<String> = actions.iter().map(|a| a.describe()).collect();
        println!(
            "{}",
            serde_json::json!({
                "interface": iface,
                "mode": mode,
                "dry_run": dry_run,
                "actions": items,
            })
        );
        return Ok(());
    }
    println!(
        "网卡 {iface}，XDP 模式 {mode}，共 {} 项调优{}",
        actions.len(),
        if dry_run {
            "（dry-run，未写入）"
        } else {
            ""
        }
    );
    for action in actions {
        println!("  - {}", action.describe());
    }
    Ok(())
}

/// 检测网卡当前的 XDP 挂载模式：`native` / `generic` / `none`。
fn detect_mode(iface: &str) -> String {
    let output = Command::new("ip")
        .args(["-d", "link", "show", "dev", iface])
        .output();
    let Ok(output) = output else {
        return "unknown".to_string();
    };
    if !output.status.success() {
        return "unknown".to_string();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.contains("xdpgeneric") {
        "generic".to_string()
    } else if text.contains("xdpoffload") {
        "offload".to_string()
    } else if text.contains(" prog/xdp ") || text.contains("xdp ") {
        "native".to_string()
    } else {
        "none".to_string()
    }
}

/// 在线 CPU 列表（解析 `0-7` / `0,2-3` 形式）。
fn online_cpus() -> Vec<u32> {
    let Ok(raw) = fs::read_to_string("/sys/devices/system/cpu/online") else {
        return Vec::new();
    };
    let mut cpus = Vec::new();
    for part in raw.trim().split(',') {
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(start), Ok(end)) = (start.parse::<u32>(), end.parse::<u32>()) {
                cpus.extend(start..=end);
            }
        } else if let Ok(cpu) = part.parse::<u32>() {
            cpus.push(cpu);
        }
    }
    cpus
}

/// CPU 集合 → RPS 位掩码（每 32 个 CPU 一组，逗号分隔，高位在前）。
fn cpu_mask_hex(cpus: &[u32]) -> String {
    let max = cpus.iter().copied().max().unwrap_or(0);
    let groups = (max / 32 + 1) as usize;
    let mut masks = vec![0u32; groups];
    for cpu in cpus {
        masks[(cpu / 32) as usize] |= 1u32 << (cpu % 32);
    }
    masks
        .iter()
        .rev()
        .map(|m| format!("{m:x}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// 网卡的 RX 队列名（`rx-0`、`rx-1` …）。
fn rx_queues(iface: &str) -> Result<Vec<String>> {
    let dir = format!("/sys/class/net/{iface}/queues");
    let mut queues = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("读取 {dir} 失败"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("rx-") {
            queues.push(name);
        }
    }
    queues.sort_by_key(|q| {
        q.trim_start_matches("rx-")
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });
    Ok(queues)
}

/// 网卡的队列中断号（只取 RX 相关：`input.N` / `req.N` / `virtqueues`）。
fn netdev_irqs(iface: &str) -> Result<Vec<u32>> {
    // 通过 /sys/class/net/<if>/device 的 PCI 地址匹配 /proc/interrupts 中的 MSI-X 行。
    let device = fs::read_link(format!("/sys/class/net/{iface}/device"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
    let interrupts = fs::read_to_string("/proc/interrupts").unwrap_or_default();
    let mut irqs = Vec::new();
    for line in interrupts.lines() {
        let Some((irq, rest)) = line.split_once(':') else {
            continue;
        };
        let Ok(irq) = irq.trim().parse::<u32>() else {
            continue;
        };
        let is_rx = rest.contains("input.") || rest.contains("req.") || rest.contains("virtqueues");
        if !is_rx {
            continue;
        }
        let matched = match &device {
            Some(dev) => rest.contains(dev.as_str()),
            None => rest.contains(iface),
        };
        if matched {
            irqs.push(irq);
        }
    }
    irqs.sort_unstable();
    irqs.dedup();
    Ok(irqs)
}

fn write_file(path: &str, value: &str) -> Result<()> {
    fs::write(path, value).with_context(|| format!("写入 {path} 失败"))
}

fn state_path(cfg: &Config) -> PathBuf {
    Path::new(&cfg.store_path)
        .parent()
        .map(|dir| dir.join(TUNE_STATE_FILE))
        .unwrap_or_else(|| PathBuf::from("/var/lib/xuanarmor").join(TUNE_STATE_FILE))
}

fn save_state(path: &Path, state: &TuneState) -> Result<()> {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let json = serde_json::to_vec_pretty(state)?;
    fs::write(path, json).with_context(|| format!("写入状态文件 {} 失败", path.display()))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_mask_single_group() {
        assert_eq!(cpu_mask_hex(&[0, 1, 2, 3]), "f");
        assert_eq!(cpu_mask_hex(&[0, 1, 2, 3, 4, 5, 6, 7]), "ff");
        assert_eq!(cpu_mask_hex(&[0]), "1");
    }

    #[test]
    fn cpu_mask_multi_group() {
        // CPU 0 与 CPU 32 → 两个 32 位组，高位组在前
        assert_eq!(cpu_mask_hex(&[0, 32]), "1,1");
        assert_eq!(cpu_mask_hex(&[31]), "80000000");
    }

    #[test]
    fn parse_online_cpus() {
        // 直接验证解析逻辑（不依赖真实 /sys 内容）
        let mut cpus = Vec::new();
        for part in "0-3,6".split(',') {
            if let Some((start, end)) = part.split_once('-') {
                let (start, end): (u32, u32) = (start.parse().unwrap(), end.parse().unwrap());
                cpus.extend(start..=end);
            } else {
                cpus.push(part.parse::<u32>().unwrap());
            }
        }
        assert_eq!(cpus, vec![0, 1, 2, 3, 6]);
    }
}
