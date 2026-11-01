//! 平滑升级（v0.5.1）：替换二进制 + 重启进程，但**数据面不中断**。
//!
//! 前置条件：`[xdp].persist_link = true`（默认）。旧进程退出时会把 XDP link 留在
//! bpffs 中，旧程序继续过滤流量；新进程启动后复用该 link 并原子替换为新程序，
//! 因此整个升级过程没有"无防护窗口"。
//!
//! 流程：
//!   1. 校验新二进制（`--version`、可选 `check --config`）；
//!   2. 备份当前二进制到 `<backup_dir>/<时间戳>/`；
//!   3. 停止旧服务 → 安装新二进制 → 启动新服务；
//!   4. 轮询 `/ready`（含 XDP 挂载状态）直到超时；
//!   5. 失败自动回滚到备份二进制并重新启动。
//!
//! 控制面（HTTP/控制台）在重启瞬间有 <1s 不可用；数据面防护不中断。

use anyhow::{anyhow, bail, Context};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::config::Config;

pub struct UpgradeOptions {
    /// 新二进制路径（可以是任意路径；区分于安装路径）
    pub binary: PathBuf,
    pub config: PathBuf,
    pub install_bin: PathBuf,
    pub timeout_s: u64,
    /// 不通过 systemd 管理：用 --pid 停止旧进程，并直接 spawn 新进程
    pub no_systemd: bool,
    pub pid: Option<u32>,
    pub log_file: PathBuf,
    /// 跳过 `check --config`（测试用：配置里的网卡可能只存在于其它 netns）
    pub skip_config_check: bool,
    pub backup_root: PathBuf,
    /// 启动新进程时的命令前缀（如 `ip netns exec ns`），按空格切分
    pub exec_prefix: String,
}

pub fn run(opts: UpgradeOptions) -> anyhow::Result<()> {
    crate::logging::init_tracing("info", false);
    if !opts.binary.exists() {
        bail!("新二进制不存在: {}", opts.binary.display());
    }
    let new_version = run_version(&opts.binary)?;
    info!(
        "准备升级到 {new_version}（新二进制 {}）",
        opts.binary.display()
    );
    eprintln!(
        "[upgrade] 新版本 {new_version}，配置 {}",
        opts.config.display()
    );

    if !opts.skip_config_check {
        let out = Command::new(&opts.binary)
            .arg("check")
            .arg("--config")
            .arg(&opts.config)
            .output()
            .with_context(|| "无法执行新二进制的配置校验")?;
        if !out.status.success() {
            bail!(
                "新二进制配置校验失败，已中止升级:\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        info!("新二进制配置校验通过");
    }

    let base_url = probe_base_url(&opts.config)?;
    let was_running = health_ok(&base_url, &opts.exec_prefix);

    // 备份当前安装
    let ts = timestamp();
    let backup_dir = opts.backup_root.join(&ts);
    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("无法创建备份目录 {}", backup_dir.display()))?;
    let backup_bin = backup_dir.join("xuanarmor");
    if opts.install_bin.exists() {
        fs::copy(&opts.install_bin, &backup_bin)
            .with_context(|| format!("备份二进制失败: {}", backup_bin.display()))?;
        fs::set_permissions(&backup_bin, fs::Permissions::from_mode(0o755))?;
    }
    let _ = fs::copy(&opts.config, backup_dir.join("config.toml"));
    fs::write(
        backup_dir.join("VERSION"),
        current_version(&opts.install_bin),
    )?;
    info!("已备份当前版本到 {}", backup_dir.display());
    eprintln!("[upgrade] 已备份当前版本到 {}", backup_dir.display());

    // 停止旧服务（旧进程会把 XDP link 留在 bpffs，防护不中断）
    if was_running {
        stop_service(&opts)?;
        info!("旧进程已退出；pinned XDP link 保持挂载，防护持续生效");
    } else {
        warn!("升级前服务未运行（/ready 不可用），跳过停止步骤");
    }

    install_binary(&opts)?;

    // 启动新服务（此时旧进程已退出、redb 锁已释放；pinned link 保持挂载）
    start_service(&opts)?;
    let deadline = Instant::now() + Duration::from_secs(opts.timeout_s.max(5));
    while Instant::now() < deadline {
        if health_ok(&base_url, &opts.exec_prefix) {
            eprintln!("[upgrade] 健康检查通过，升级完成");
            info!(
                "升级完成：{new_version}（{}），数据面在切换期间未中断",
                opts.install_bin.display()
            );
            info!(
                "回滚命令：xuanarmor upgrade --rollback {}",
                backup_dir.display()
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    // 健康检查失败 → 自动回滚
    warn!(
        "新版本在 {}s 内未通过健康检查，开始自动回滚（备份：{}）",
        opts.timeout_s,
        backup_dir.display()
    );
    let _ = stop_service(&opts);
    if backup_bin.exists() {
        install_from(&backup_bin, &opts.install_bin)?;
    }
    start_service(&opts)?;
    let rollback_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < rollback_deadline {
        if health_ok(&base_url, &opts.exec_prefix) {
            bail!("升级失败已回滚：服务已恢复到旧版本并重新通过健康检查");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    bail!(
        "升级失败且回滚后服务仍未通过健康检查，请查看 {} 与系统日志",
        opts.log_file.display()
    )
}

/// `xuanarmor upgrade --rollback <dir>`：从指定备份目录恢复。
pub fn rollback(opts: UpgradeOptions, backup_dir: &Path) -> anyhow::Result<()> {
    crate::logging::init_tracing("info", false);
    let backup_bin = backup_dir.join("xuanarmor");
    if !backup_bin.exists() {
        bail!("备份目录中没有 xuanarmor 二进制: {}", backup_dir.display());
    }
    let base_url = probe_base_url(&opts.config)?;
    let _ = stop_service(&opts);
    install_from(&backup_bin, &opts.install_bin)?;
    if backup_dir.join("config.toml").exists() {
        let _ = fs::copy(backup_dir.join("config.toml"), &opts.config);
    }
    start_service(&opts)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if health_ok(&base_url, &opts.exec_prefix) {
            info!("已回滚到备份 {}", backup_dir.display());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    bail!("回滚后服务未通过健康检查，请人工检查")
}

// ───────────────────────── helpers ─────────────────────────

fn run_version(bin: &Path) -> anyhow::Result<String> {
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .with_context(|| format!("无法执行 {}", bin.display()))?;
    if !out.status.success() {
        bail!(
            "新二进制 --version 校验失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn current_version(bin: &Path) -> String {
    run_version(bin).unwrap_or_else(|_| "unknown".to_string())
}

fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn probe_base_url(config_path: &Path) -> anyhow::Result<String> {
    let config = Config::from_file(config_path)
        .with_context(|| format!("读取配置失败: {}", config_path.display()))?;
    let bind = config
        .web_bind
        .clone()
        .unwrap_or_else(|| format!("0.0.0.0:{}", config.web_port));
    let port = bind
        .rsplit_once(':')
        .map(|(_, p)| p)
        .unwrap_or("8720")
        .trim_matches(']');
    Ok(format!("http://127.0.0.1:{port}"))
}

fn health_ok(base: &str, exec_prefix: &str) -> bool {
    // netns/容器场景：健康检查必须在该命名空间内执行（127.0.0.1 属于不同 netns）
    let prefix: Vec<&str> = exec_prefix
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect();
    if !prefix.is_empty() {
        let mut cmd = Command::new(prefix[0]);
        for part in &prefix[1..] {
            cmd.arg(part);
        }
        cmd.args([
            "curl",
            "-sf",
            "-m",
            "2",
            "-o",
            "/dev/null",
            &format!("{base}/ready"),
        ]);
        if let Ok(out) = cmd.output() {
            return out.status.success();
        }
        return false;
    }
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(format!("{base}/ready")).send() {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

fn systemd_available() -> bool {
    Command::new("systemctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn stop_service(opts: &UpgradeOptions) -> anyhow::Result<()> {
    if !opts.no_systemd && systemd_available() {
        info!("systemctl stop xuanarmor");
        let status = Command::new("systemctl")
            .args(["stop", "xuanarmor"])
            .status()
            .context("无法执行 systemctl stop")?;
        if !status.success() {
            warn!("systemctl stop 返回非零（服务可能未运行）");
        }
        wait_stopped(None, 15);
        return Ok(());
    }
    let pid = opts
        .pid
        .ok_or_else(|| anyhow!("--no-systemd 模式需要提供 --pid <旧进程 PID>"))?;
    info!("发送 SIGTERM 到 {}", pid);
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    if !wait_stopped(Some(pid), 15) {
        // 优雅退出失败（例如卡在清理阶段）→ 强制结束，避免新进程抢不到 redb 锁
        warn!("旧进程 {pid} 在 15s 内未退出，发送 SIGKILL");
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
        if !wait_stopped(Some(pid), 5) {
            anyhow::bail!("无法停止旧进程 {pid}，升级中止（服务保持原状）");
        }
    }
    Ok(())
}

fn wait_stopped(pid: Option<u32>, timeout_s: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    while Instant::now() < deadline {
        let alive = match pid {
            Some(p) => Path::new(&format!("/proc/{p}")).exists(),
            None => false,
        };
        if !alive {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !matches!(pid, Some(p) if Path::new(&format!("/proc/{p}")).exists())
}

fn install_binary(opts: &UpgradeOptions) -> anyhow::Result<()> {
    install_from(&opts.binary, &opts.install_bin)
}

/// 原子安装：先写同目录临时文件，chmod 后 rename 覆盖。
fn install_from(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("安装路径非法: {}", dst.display()))?;
    fs::create_dir_all(parent).ok();
    let tmp = parent.join(format!(
        ".{}.new.{}",
        dst.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("xuanarmor"),
        std::process::id()
    ));
    fs::copy(src, &tmp).with_context(|| format!("复制到 {} 失败", tmp.display()))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp, dst).with_context(|| format!("替换 {} 失败", dst.display()))?;
    info!("已安装二进制到 {}", dst.display());
    eprintln!("[upgrade] 已安装 {}", dst.display());
    Ok(())
}

fn start_service(opts: &UpgradeOptions) -> anyhow::Result<()> {
    if !opts.no_systemd && systemd_available() {
        info!("systemctl start xuanarmor");
        let status = Command::new("systemctl")
            .args(["start", "xuanarmor"])
            .status()
            .context("无法执行 systemctl start")?;
        if !status.success() {
            warn!("systemctl start 返回非零，将依赖健康检查判断");
        }
        return Ok(());
    }
    if let Some(parent) = opts.log_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&opts.log_file)
        .with_context(|| format!("无法打开日志 {}", opts.log_file.display()))?;
    let log_err = log.try_clone().with_context(|| "无法复制日志文件句柄")?;
    info!("以独立会话启动新进程（日志：{}）", opts.log_file.display());
    let mut cmd = Command::new("setsid");
    let prefix: Vec<&str> = opts
        .exec_prefix
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect();
    if !prefix.is_empty() {
        cmd = Command::new(prefix[0]);
        for part in &prefix[1..] {
            cmd.arg(part);
        }
        cmd.arg("setsid");
    }
    let child = cmd
        .arg(&opts.install_bin)
        .arg("start")
        .arg("--config")
        .arg(&opts.config)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("启动 {} 失败", opts.install_bin.display()))?;
    info!("新进程 PID {}", child.id());
    eprintln!("[upgrade] 新进程 PID {}", child.id());
    Ok(())
}
