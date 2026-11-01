//! XDP 挂载持久化与平滑升级（v0.5.1）。
//!
//! 内核 ≥5.9 上 aya 通过 **bpf_link** 挂载 XDP：进程退出会关闭 link fd，link 随之销毁、
//! 程序被摘除。因此要做到"进程重启期间防护不中断"，必须把 link **pin** 到 bpffs：
//! - 启动时 attach 成功后 `FdLink::pin("/sys/fs/bpf/xuanarmor_xdp_link")`；
//! - 进程退出时保留 pin 文件 → link 存活 → 旧程序继续过滤（fail-closed）；
//! - 新进程启动时 `PinnedLink::from_pin` → `attach_to_link`（内核 `bpf_link_update`
//!   原子替换程序）→ 重新 pin，全程没有"无防护"窗口；
//! - 需要真正卸载时执行 `xuanarmor detach`（删除 pin 文件 + `ip link set … xdp off`）。
//!
//! bpffs 不允许 `mkdir`，所以 pin 文件直接放在 `[xdp].pin_path`（默认 `/sys/fs/bpf`）根下；
//! 网卡/模式/版本等元数据放在数据目录的 `link.meta`（不写 bpffs）。

use aya::programs::links::{FdLink, PinnedLink};
use aya::programs::xdp::{Xdp, XdpFlags, XdpLink, XdpLinkId};
use aya::Ebpf;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::config::Config;

const PROGRAM_NAME: &str = "xuanarmor";
const LINK_FILE: &str = "xuanarmor_xdp_link";
const META_FILE: &str = "link.meta";

/// 当前进程的 XDP 挂载句柄。
pub enum XdpHandle {
    /// link 已 pin：跨进程存活，退出时不 detach。
    Pinned,
    /// 普通挂载（未 pin），只在当前进程存活。
    Legacy { _link_id: XdpLinkId },
}

pub struct XdpState {
    pub handle: XdpHandle,
    pub mode: &'static str,
    pub reused: bool,
    pub replaced_existing: bool,
}

impl XdpState {
    pub fn pinned(&self) -> bool {
        matches!(self.handle, XdpHandle::Pinned)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LinkMeta {
    interface: String,
    mode: String,
    version: String,
    updated_at: u64,
}

/// pinned link 文件路径（bpffs 根下单文件；bpffs 不支持 mkdir）。
///
/// 优先使用 `[xdp].pin_path`；若该路径不是可写 bpffs（例如容器 / `ip netns exec`
/// 的 mount namespace 里看不到 host 的 /sys/fs/bpf），自动回退到 `/run/xuanarmor-bpf`，
/// 必要时创建目录并挂载 bpffs。
pub fn link_path(config: &Config) -> PathBuf {
    resolve_pin_dir(config)
        .unwrap_or_else(|| PathBuf::from(&config.xdp.pin_path))
        .join(LINK_FILE)
}

const BPF_FS_MAGIC: i64 = 0xcafe_4a11;
const FALLBACK_PIN_DIR: &str = "/run/xuanarmor-bpf";

fn is_bpf_fs(path: &Path) -> bool {
    let Ok(cpath) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
        return false;
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(cpath.as_ptr(), &mut st) };
    rc == 0 && st.f_type as i64 == BPF_FS_MAGIC
}

/// 解析一个真正可用的 bpffs 目录；不存在/不可用时返回 None。
pub fn resolve_pin_dir(config: &Config) -> Option<PathBuf> {
    let primary = Path::new(&config.xdp.pin_path);
    if primary.is_dir() && is_bpf_fs(primary) {
        return Some(primary.to_path_buf());
    }

    let fallback = Path::new(FALLBACK_PIN_DIR);
    if !fallback.is_dir() {
        let _ = std::fs::create_dir_all(fallback);
    }
    if !is_bpf_fs(fallback) {
        let _ = std::process::Command::new("mount")
            .args(["-t", "bpf", "bpf", FALLBACK_PIN_DIR])
            .status();
    }
    if is_bpf_fs(fallback) {
        warn!(
            "{} 不是可写的 bpffs（可能在 netns/容器的 mount namespace 中不可见），自动使用 {}",
            primary.display(),
            fallback.display()
        );
        return Some(fallback.to_path_buf());
    }
    None
}

/// 元数据路径：数据目录（与 redb 同目录），不写 bpffs。
fn meta_path(config: &Config) -> PathBuf {
    Path::new(&config.store_path)
        .parent()
        .unwrap_or_else(|| Path::new("/var/lib/xuanarmor"))
        .join(META_FILE)
}

fn read_meta(config: &Config) -> Option<LinkMeta> {
    serde_json::from_str(&std::fs::read_to_string(meta_path(config)).ok()?).ok()
}

fn write_meta(config: &Config, mode: &str) -> anyhow::Result<()> {
    let meta = LinkMeta {
        interface: config.interface.clone(),
        mode: mode.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        updated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let path = meta_path(config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, serde_json::to_vec_pretty(&meta)?)?;
    Ok(())
}

/// 期望的挂载模式：解析 `[xdp].mode` 与 `XUANARMOR_XDP_MODE` 覆盖。
pub fn want_mode(config: &Config) -> String {
    let env_mode = crate::config::env_compat("XUANARMOR_XDP_MODE", "ESHIELD_XDP_MODE")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match env_mode.as_str() {
        "skb" | "generic" => "generic".to_string(),
        "native" | "drv" => "native".to_string(),
        _ => config.xdp.mode.to_ascii_lowercase(),
    }
}

/// 检测网卡上是否已有 XDP 程序（`ip -d link` 输出含 xdp 标记）。
pub fn interface_has_xdp(iface: &str) -> bool {
    match std::process::Command::new("ip")
        .args(["-d", "link", "show", iface])
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains("xdp"),
        Err(_) => false,
    }
}

fn attach_with_mode(
    program: &mut Xdp,
    config: &Config,
) -> anyhow::Result<(XdpLinkId, &'static str)> {
    let want = want_mode(config);
    if want == "generic" {
        let id = program
            .attach(&config.interface, XdpFlags::SKB_MODE)
            .map_err(|e| anyhow::anyhow!("attach XDP on {}: {e}", config.interface))?;
        return Ok((id, "generic"));
    }
    match program.attach(&config.interface, XdpFlags::DRV_MODE) {
        Ok(id) => Ok((id, "native")),
        Err(first_err) => {
            if config.xdp.disable_offloads && disable_native_blockers(&config.interface).is_ok() {
                if let Ok(id) = program.attach(&config.interface, XdpFlags::DRV_MODE) {
                    return Ok((id, "native"));
                }
            }
            if want == "native" {
                anyhow::bail!(
                    "native XDP attach failed on {}: {first_err}；{}",
                    config.interface,
                    native_attach_hint(&config.interface)
                );
            }
            warn!(
                "native XDP attach 失败（{first_err}），回退 generic；{}",
                native_attach_hint(&config.interface)
            );
            let id = program
                .attach(&config.interface, XdpFlags::SKB_MODE)
                .map_err(|e| anyhow::anyhow!("attach XDP on {}: {e}", config.interface))?;
            Ok((id, "generic"))
        }
    }
}

/// 复用上次留下的 pinned link：`bpf_link_update` 原子替换为新程序。
fn try_reuse(ebpf: &mut Ebpf, config: &Config) -> anyhow::Result<Option<XdpState>> {
    let path = link_path(config);
    if !path.exists() {
        return Ok(None);
    }
    if let Some(meta) = read_meta(config) {
        if meta.interface != config.interface {
            warn!(
                "pinned XDP link 属于网卡 {}，当前配置为 {}：重新挂载（有短暂空窗）",
                meta.interface, config.interface
            );
            detach_pinned(config)?;
            return Ok(None);
        }
        let want = want_mode(config);
        if want != "auto" && meta.mode != want {
            warn!(
                "pinned XDP link 模式为 {}，当前期望 {}：重新挂载（有短暂空窗）",
                meta.mode, want
            );
            detach_pinned(config)?;
            return Ok(None);
        }
    }

    let pinned = match PinnedLink::from_pin(&path) {
        Ok(p) => p,
        Err(e) => {
            warn!("打开 pinned link {} 失败（{e}）：重新挂载", path.display());
            detach_pinned(config)?;
            return Ok(None);
        }
    };
    let fd_link = match pinned.unpin() {
        Ok(fd) => fd,
        Err(e) => {
            warn!("unpin {} 失败（{e}）：重新挂载", path.display());
            detach_pinned(config)?;
            return Ok(None);
        }
    };
    let xdp_link = match XdpLink::try_from(fd_link) {
        Ok(link) => link,
        Err(e) => {
            warn!("转换 pinned link 为 XDP link 失败（{e}）：重新挂载");
            return Ok(None);
        }
    };

    let program: &mut Xdp = ebpf
        .program_mut(PROGRAM_NAME)
        .ok_or_else(|| anyhow::anyhow!("program '{PROGRAM_NAME}' not found"))?
        .try_into()
        .map_err(|e| anyhow::anyhow!("program is not XDP: {e}"))?;

    let link_id = match program.attach_to_link(xdp_link) {
        Ok(id) => id,
        Err(e) => {
            // link 已随错误路径释放，旧程序会在这时被摘除 → 立即重新挂载
            warn!("bpf_link_update 失败（{e}）：重新挂载（有短暂空窗）");
            return legacy_state(program, config, false);
        }
    };
    let mode = match read_meta(config).map(|m| m.mode) {
        Some(m) if m == "native" => "native",
        _ => "generic",
    };
    let link = match program.take_link(link_id) {
        Ok(link) => link,
        Err(e) => {
            warn!("take_link 失败（{e}）：本次为普通挂载");
            return legacy_state(program, config, false);
        }
    };
    let fd = match FdLink::try_from(link) {
        Ok(fd) => fd,
        Err(e) => {
            warn!("pinned link 不是 BPF fd link（{e}）：本次为普通挂载");
            return legacy_state(program, config, false);
        }
    };
    match fd.pin(&path) {
        Ok(pinned) => {
            drop(pinned);
            write_meta(config, mode).ok();
            info!(
                "复用 pinned XDP link 并原子替换程序（bpf_link_update），{} 上防护未中断",
                config.interface
            );
            Ok(Some(XdpState {
                handle: XdpHandle::Pinned,
                mode,
                reused: true,
                replaced_existing: true,
            }))
        }
        Err(e) => {
            warn!("重新 pin XDP link 失败（{e}）：本次为普通挂载");
            legacy_state(program, config, false)
        }
    }
}

/// 回退为普通挂载（link 未持久化）。
fn legacy_state(
    program: &mut Xdp,
    config: &Config,
    reused: bool,
) -> anyhow::Result<Option<XdpState>> {
    let (id, mode) = attach_with_mode(program, config)?;
    Ok(Some(XdpState {
        handle: XdpHandle::Legacy { _link_id: id },
        mode,
        reused,
        replaced_existing: true,
    }))
}

/// 启动时挂载：优先复用 pinned link 做原子替换，否则挂载并 pin。
pub fn attach_or_reuse(ebpf: &mut Ebpf, config: &Config) -> anyhow::Result<XdpState> {
    let replaced_existing = interface_has_xdp(&config.interface);

    if config.xdp.persist_link {
        match try_reuse(ebpf, config) {
            Ok(Some(state)) => return Ok(state),
            Ok(None) => {}
            Err(e) => warn!("复用 pinned XDP link 失败（{e}）：改为重新挂载"),
        }
    }

    let program: &mut Xdp = ebpf
        .program_mut(PROGRAM_NAME)
        .ok_or_else(|| anyhow::anyhow!("program '{PROGRAM_NAME}' not found"))?
        .try_into()
        .map_err(|e| anyhow::anyhow!("program is not XDP: {e}"))?;
    let (link_id, mode) = attach_with_mode(program, config)?;

    if !config.xdp.persist_link {
        return Ok(XdpState {
            handle: XdpHandle::Legacy { _link_id: link_id },
            mode,
            reused: false,
            replaced_existing,
        });
    }

    let Some(pin_dir) = resolve_pin_dir(config) else {
        warn!("没有可用的 bpffs（pin_path 与 /run/xuanarmor-bpf 均不可用）：本次为普通挂载");
        return Ok(XdpState {
            handle: XdpHandle::Legacy { _link_id: link_id },
            mode,
            reused: false,
            replaced_existing,
        });
    };
    let path = pin_dir.join(LINK_FILE);

    // 清理陈旧 pin（例如上次运行留下的、对应旧网卡实例的 link），
    // 否则 fd.pin 会因 EEXIST 失败并把本次降级为普通挂载。
    let _ = std::fs::remove_file(&path);

    match program.take_link(link_id) {
        Ok(link) => match FdLink::try_from(link) {
            Ok(fd) => match fd.pin(&path) {
                Ok(pinned) => {
                    drop(pinned);
                    write_meta(config, mode).ok();
                    info!(
                        "XDP link 已 pin 到 {}（进程重启/升级期间防护不中断）",
                        path.display()
                    );
                    Ok(XdpState {
                        handle: XdpHandle::Pinned,
                        mode,
                        reused: false,
                        replaced_existing,
                    })
                }
                Err(e) => {
                    warn!(
                        "pin XDP link 失败（{e:?}）：{} 可能不可写或非 bpffs；本次为普通挂载",
                        path.display()
                    );
                    let (id, mode) = attach_with_mode(program, config)?;
                    Ok(XdpState {
                        handle: XdpHandle::Legacy { _link_id: id },
                        mode,
                        reused: false,
                        replaced_existing,
                    })
                }
            },
            Err(e) => {
                warn!("link 不是 BPF fd link（{e}）：本次为普通挂载");
                let (id, mode) = attach_with_mode(program, config)?;
                Ok(XdpState {
                    handle: XdpHandle::Legacy { _link_id: id },
                    mode,
                    reused: false,
                    replaced_existing,
                })
            }
        },
        Err(e) => {
            warn!("take_link 失败（{e}）：本次为普通挂载");
            let (id, mode) = attach_with_mode(program, config)?;
            Ok(XdpState {
                handle: XdpHandle::Legacy { _link_id: id },
                mode,
                reused: false,
                replaced_existing,
            })
        }
    }
}

/// 退出：persist_link 时保留 pinned link（旧程序继续过滤）；否则显式卸载。
pub fn shutdown_detach(_ebpf: &mut Ebpf, state: XdpState, config: &Config) {
    if config.xdp.persist_link {
        if state.pinned() {
            info!(
                "persist_link=true：保留 pinned XDP link（{}），进程退出期间防护继续生效",
                link_path(config).display()
            );
        } else {
            info!("persist_link=true 但本次为普通挂载：退出即停止防护");
        }
        return;
    }
    match detach_pinned(config) {
        Ok(true) => info!("XDP program detached gracefully"),
        Ok(false) => info!("没有找到需要摘除的 XDP 挂载"),
        Err(e) => warn!("failed to detach XDP program: {e}"),
    }
}

/// 摘除 pinned link（删除 pin 文件；没有进程持有 link fd 时 link 立即销毁）。
pub fn detach_pinned(config: &Config) -> anyhow::Result<bool> {
    let path = link_path(config);
    let mut detached = false;
    if path.exists() {
        if let Ok(pinned) = PinnedLink::from_pin(&path) {
            let fd_link = pinned.unpin()?;
            drop(fd_link);
            detached = true;
        } else {
            std::fs::remove_file(&path).ok();
            detached = true;
        }
    }
    let _ = std::fs::remove_file(meta_path(config));
    // 兜底：程序仍挂在网卡上（例如其它进程持有 link）时用 iproute2 摘除
    if interface_has_xdp(&config.interface) {
        let _ = detach_external(config);
    }
    Ok(detached || !interface_has_xdp(&config.interface))
}

/// 用 iproute2 强制摘除网卡 XDP（服务停止后手工清理用）。
pub fn detach_external(config: &Config) -> anyhow::Result<bool> {
    if !interface_has_xdp(&config.interface) {
        return Ok(false);
    }
    let mut last_err = String::new();
    let iface = config.interface.as_str();
    for args in [
        vec!["link", "set", "dev", iface, "xdp", "off"],
        vec!["link", "set", "dev", iface, "xdpgeneric", "off"],
        vec!["link", "set", "dev", iface, "xdpdrv", "off"],
    ] {
        match std::process::Command::new("ip").args(&args).output() {
            Ok(out) if out.status.success() => return Ok(true),
            Ok(out) => last_err = String::from_utf8_lossy(&out.stderr).trim().to_string(),
            Err(e) => last_err = format!("ip command failed: {e}"),
        }
    }
    anyhow::bail!(
        "无法摘除 {iface} 上的 XDP 程序: {last_err}；可手动执行 `ip link set dev {iface} xdp off`"
    )
}

fn disable_native_blockers(iface: &str) -> anyhow::Result<()> {
    for (feat, val) in [
        ("rx-gro-hw", "off"),
        ("tx-checksum-ip-generic", "off"),
        ("tx-checksum-ipv4", "off"),
    ] {
        let out = std::process::Command::new("ethtool")
            .args(["-K", iface, feat, val])
            .output();
        match out {
            Ok(_) => {}
            Err(e) => return Err(anyhow::anyhow!("ethtool not available: {e}")),
        }
    }
    Ok(())
}

/// native 挂载失败时的排查提示（面向运维日志）。
pub fn native_attach_hint(iface: &str) -> String {
    format!(
        "可执行 `ethtool -k {iface} | grep -E 'rx-gro-hw|tx-checksum'` 检查卸载项；\
         若 rx-gro-hw 为 on [fixed]（virtio 宿主机固定开启），guest 侧无法 native，\
         请改用 [xdp].mode = \"generic\" 或保持 auto"
    )
}
