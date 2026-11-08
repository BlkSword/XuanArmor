#!/bin/bash
# XuanArmor 一键安装脚本
# 用法:
#   curl -fsSL https://raw.githubusercontent.com/BlkSword/XuanArmor/main/scripts/install.sh | sudo bash
#   curl -fsSL https://raw.githubusercontent.com/BlkSword/XuanArmor/main/scripts/install.sh | sudo bash -s -- --version 0.5.1
#   sudo bash scripts/install.sh --build   # 从当前源码构建并安装
#   sudo bash scripts/install.sh --no-start  # 只安装/升级，不启动服务
set -e

# ── 颜色输出 ──
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
info()  { echo -e "${GREEN}[OK]${NC} $1"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
err()   { echo -e "${RED}[FAIL]${NC} $1"; }

# ── 参数解析（v0.5.0：安装 / 升级 / 回滚 / 卸载） ──
ACTION="install"
BUILD_MODE=0
NO_START=0
FORCE_NO_SYSTEMD=0
PURGE=0
ROLLBACK_DIR=""
EXPECTED_SHA="${XUANARMOR_EXPECTED_SHA:-${ESHIELD_EXPECTED_SHA:-}}"
_argv=("$@")
for ((i = 0; i < ${#_argv[@]}; i++)); do
    case "${_argv[i]}" in
        --build) BUILD_MODE=1 ;;
        --no-start) NO_START=1 ;;
        --no-systemd) FORCE_NO_SYSTEMD=1 ;;
        --uninstall) ACTION="uninstall" ;;
        --purge) PURGE=1 ;;
        --version) VERSION="${_argv[i + 1]:-}"; VERSION_EXPLICIT=1; i=$((i + 1)) ;;
        --version=*) VERSION="${_argv[i]#*=}"; VERSION_EXPLICIT=1 ;;
        --sha256) EXPECTED_SHA="${_argv[i + 1]:-}"; i=$((i + 1)) ;;
        --sha256=*) EXPECTED_SHA="${_argv[i]#*=}" ;;
        --rollback) ACTION="rollback"; ROLLBACK_DIR="${_argv[i + 1]:-}"; i=$((i + 1)) ;;
        --rollback=*) ACTION="rollback"; ROLLBACK_DIR="${_argv[i]#*=}" ;;
        -h | --help)
            cat <<'USAGE'
用法: sudo bash scripts/install.sh [选项]

  --build               从当前源码构建并安装（默认从 GitHub Release 下载）
  --version <x.y.z>     指定安装版本（默认取 GitHub 最新 Release）
  --no-start            安装/升级后不启动服务（便于先改配置）
  --no-systemd          不注册/不启动 systemd 服务（容器或手工管理场景）
  --sha256 <hex>        校验下载产物 SHA256（也可用 XUANARMOR_EXPECTED_SHA）
  --rollback [dir]      回滚到备份目录（省略则使用最近一次备份）
  --uninstall           卸载（保留配置与数据）
  --purge               配合 --uninstall：同时删除 /etc/xuanarmor 与持久化数据
  -h, --help            显示本帮助

升级时脚本会自动把当前二进制/配置/service 备份到
/var/lib/xuanarmor/backup/<时间戳>/，可用 --rollback 回退。

一键安装（管道方式）:
  curl -fsSL https://raw.githubusercontent.com/BlkSword/XuanArmor/main/scripts/install.sh | sudo bash
  curl -fsSL .../install.sh | sudo bash -s -- --version 0.5.1
USAGE
            exit 0
            ;;
        *) err "未知参数: ${_argv[i]}（用 --help 查看用法）"; exit 1 ;;
    esac
done

# GitHub 仓库（与 scripts/publish-release.sh 保持一致；可用 XUANARMOR_REPO 覆盖）
# 生成控制台访问密码（32 字节随机 hex）
gen_console_token() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
    else
        head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'
    fi
}

REPO="${XUANARMOR_REPO:-${ESHIELD_REPO:-BlkSword/XuanArmor}}"
# 优先从环境变量读取版本；未设置时依次尝试 workspace / xuanarmor crate 的 Cargo.toml；最后回退到默认版本。
DEFAULT_VERSION="0.5.2"
if [ -z "${VERSION:-}" ]; then
    if [ -f "Cargo.toml" ] && grep -qE '^\[workspace\]' Cargo.toml 2>/dev/null; then
        # workspace 根 Cargo.toml 不含 version，版本在 xuanarmor/Cargo.toml
        if [ -f "xuanarmor/Cargo.toml" ]; then
            VERSION=$(grep -m1 -E '^version\s*=' xuanarmor/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
        fi
    elif [ -f "Cargo.toml" ]; then
        VERSION=$(grep -m1 -E '^version\s*=' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
    fi
    VERSION="${VERSION:-$DEFAULT_VERSION}"
fi
# 管道安装（curl | sudo bash）时本地没有 Cargo.toml，DEFAULT_VERSION 可能尚未发布；
# 这里优先解析 GitHub 最新 Release，失败再回退 DEFAULT_VERSION。
lookup_latest_release() {
    command -v curl >/dev/null 2>&1 || return 0
    curl -fsSL --max-time 8 "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\([^"]*\)".*/\1/p' | head -1
}
if [ -z "${VERSION_EXPLICIT:-}" ] && [ ! -f "Cargo.toml" ] && [ ! -f "xuanarmor/Cargo.toml" ]; then
    LATEST_VERSION="$(lookup_latest_release || true)"
    if [ -n "${LATEST_VERSION}" ]; then
        VERSION="${LATEST_VERSION}"
    fi
fi
# 安装路径可用环境变量覆盖（便于无副作用地验证安装脚本本身）
INSTALL_BIN="${XUANARMOR_INSTALL_BIN:-${ESHIELD_INSTALL_BIN:-/usr/local/bin/xuanarmor}}"
INSTALL_CFG="${XUANARMOR_INSTALL_CFG:-${ESHIELD_INSTALL_CFG:-/etc/xuanarmor/config.toml}}"
STORE_PATH="${XUANARMOR_STORE_PATH:-${ESHIELD_STORE_PATH:-/var/lib/xuanarmor/rules.redb}}"
SERVICE_FILE="${XUANARMOR_SERVICE_FILE:-${ESHIELD_SERVICE_FILE:-/etc/systemd/system/xuanarmor.service}}"
SKIP_SYSTEMD="${XUANARMOR_INSTALL_SKIP_SYSTEMD:-${ESHIELD_INSTALL_SKIP_SYSTEMD:-0}}"
WEB_PORT=8720

# ── 环境检测 ──
echo "========================================="
echo " XuanArmor v${VERSION} 安装程序"
echo "========================================="
echo ""

# 1. Root 检查
if [ "$(id -u)" -ne 0 ]; then
    err "此脚本需要 root 权限运行，请使用: sudo bash scripts/install.sh"
    exit 1
fi
info "root 权限检查通过"

# 管道安装需要 curl；缺失时直接给出可执行的安装提示
if ! command -v curl >/dev/null 2>&1; then
    err "未检测到 curl，无法下载发布产物"
    err "  Debian/Ubuntu: apt-get install -y curl"
    err "  RHEL/CentOS:   dnf install -y curl"
    exit 1
fi

# systemd 可用性：容器/非 systemd 环境下自动降级，不阻塞安装
if [ "$FORCE_NO_SYSTEMD" = "1" ]; then
    SKIP_SYSTEMD=1
elif ! command -v systemctl >/dev/null 2>&1 || [ ! -d /run/systemd/system ]; then
    warn "未检测到运行中的 systemd，将只安装二进制与配置（不注册服务）"
    SKIP_SYSTEMD=1
fi

# 2. 架构检查
ARCH=$(uname -m)
case "$ARCH" in
    x86_64) TARGET="x86_64-unknown-linux-musl" ;;
    aarch64) TARGET="aarch64-unknown-linux-musl"
             if [ "$BUILD_MODE" != "1" ]; then
                 warn "官方 Release 目前仅提供 x86_64 产物；aarch64 建议使用源码构建:"
                 warn "  sudo bash scripts/install.sh --build"
             fi ;;
    *) err "不支持的架构: $ARCH（目前仅支持 x86_64 和 aarch64）"; exit 1 ;;
esac
info "CPU 架构: $ARCH → $TARGET"

# 3. 内核版本检查
KERNEL_MAJOR=$(uname -r | cut -d. -f1)
KERNEL_MINOR=$(uname -r | cut -d. -f2)
if [ "$KERNEL_MAJOR" -lt 5 ] || \
   ([ "$KERNEL_MAJOR" -eq 5 ] && [ "$KERNEL_MINOR" -lt 10 ]); then
    err "内核版本需要 5.10+，当前为 $(uname -r)"
    exit 1
fi
info "内核版本: $(uname -r) OK"

# 4. BTF 检查
if [ ! -f /sys/kernel/btf/vmlinux ]; then
    warn "未检测到 BTF 支持 (/sys/kernel/btf/vmlinux)"
    warn "  eBPF CO-RE 可能无法工作。请确认内核编译时启用了 CONFIG_DEBUG_INFO_BTF"
    warn "  检查: zgrep CONFIG_DEBUG_INFO_BTF /proc/config.gz 2>/dev/null || true"
else
    info "BTF 支持: 已启用"
fi

# 5. XDP 支持检查（通过检查是否有网卡已挂载 XDP 推断）
if ip link show 2>/dev/null | grep -q "xdp"; then
    info "XDP 支持: 已可用（检测到现有 XDP 挂载）"
else
    info "XDP 支持: 内核已识别（需网卡驱动支持 native mode）"
fi

# 6. 网卡自动检测
DEFAULT_IFACE="eth0"
DETECTED_IFACE=$(ip -o link show 2>/dev/null | grep -v "lo:" | grep "state UP" | head -1 | awk -F': ' '{print $2}' || echo "")
if [ -n "$DETECTED_IFACE" ] && [ "$DETECTED_IFACE" != "lo" ]; then
    DEFAULT_IFACE="$DETECTED_IFACE"
    info "检测到活动网卡: $DEFAULT_IFACE"
else
    warn "未检测到活动网卡，将使用默认值 eth0"
    warn "  可用网卡列表:"
    ip -o link show 2>/dev/null | grep -v "lo:" | awk '{print "    " $2}' || true
fi

# 7. 内存检查
TOTAL_MEM=$(grep MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo 0)
if [ "$TOTAL_MEM" -lt 262144 ]; then  # < 256 MB
    warn "系统内存较低 ($(( TOTAL_MEM / 1024 )) MB)，建议至少 512 MB"
    warn "  eBPF Map 需要约 50-100 MB 内存"
else
    info "系统内存: $(( TOTAL_MEM / 1024 )) MB"
fi

# 8. 端口检查
if ss -tlnp 2>/dev/null | grep -q ":$WEB_PORT "; then
    warn "端口 $WEB_PORT 已被占用:"
    ss -tlnp 2>/dev/null | grep ":$WEB_PORT " | head -3
    warn "  安装将继续，但 XuanArmor 可能无法绑定该端口"
    warn "  可在 /etc/xuanarmor/config.toml 中修改 web_bind"
else
    info "端口 $WEB_PORT: 可用"
fi

# 9. 构建依赖检查（仅 --build 模式）
if [ "$BUILD_MODE" = "1" ]; then
    echo ""
    echo "── 构建依赖检查 ──"
    if ! command -v rustup >/dev/null 2>&1; then
        err "未检测到 rustup，请先安装 Rust: https://rustup.rs"
        exit 1
    fi
    info "rustup: $(rustup --version 2>/dev/null || echo '已安装')"

    if ! command -v cargo >/dev/null 2>&1; then
        err "未检测到 cargo"
        exit 1
    fi
    info "cargo: $(cargo --version)"

    if ! command -v clang >/dev/null 2>&1; then
        warn "未检测到 clang，eBPF 编译需要 LLVM/clang"
        warn "  Debian/Ubuntu: sudo apt install llvm clang libelf-dev"
        warn "  RHEL/CentOS:   sudo dnf install clang llvm elfutils-libelf-devel"
    else
        info "clang: $(clang --version 2>/dev/null | head -1 || echo '已安装')"
    fi

    if ! command -v bpf-linker >/dev/null 2>&1; then
        warn "未检测到 bpf-linker，eBPF 链接需要此工具"
        warn "  安装: cargo install bpf-linker"
    else
        info "bpf-linker: 已安装"
    fi
fi

echo ""

build_local() {
    echo "正在从源码构建 XuanArmor..."
    if ! command -v rustup >/dev/null 2>&1; then
        echo "错误: 未检测到 rustup，请先安装 Rust"
        exit 1
    fi
    rustup toolchain install "${XUANARMOR_EBPF_TOOLCHAIN:-nightly-2026-07-31}" >/dev/null 2>&1 || true
    rustup target add bpfel-unknown-none --toolchain "${XUANARMOR_EBPF_TOOLCHAIN:-nightly-2026-07-31}" >/dev/null 2>&1 || true
    rustup component add rust-src --toolchain "${XUANARMOR_EBPF_TOOLCHAIN:-nightly-2026-07-31}" >/dev/null 2>&1 || true
    cargo +"${XUANARMOR_EBPF_TOOLCHAIN:-nightly-2026-07-31}" build --package xuanarmor-ebpf --target bpfel-unknown-none -Z build-std=core --release
    cargo build --package xuanarmor --target "$TARGET" --release
    NEW_BIN="target/$TARGET/release/xuanarmor"
    if smooth_upgrade "$NEW_BIN"; then
        info "平滑升级完成（旧进程退出期间 XDP 继续挂载）"
        exit 0
    fi
    install -m 0755 "$NEW_BIN" "$INSTALL_BIN"
}

BACKUP_ROOT="${XUANARMOR_BACKUP_DIR:-${ESHIELD_BACKUP_DIR:-/var/lib/xuanarmor/backup}}"

# 备份当前安装（升级/回滚用）：二进制 + 配置 + systemd 单元 + 版本号
backup_current() {
    local ts dir
    ts="$(date +%Y%m%d-%H%M%S)"
    dir="${BACKUP_ROOT}/${ts}"
    mkdir -p "$dir" || return 0
    [ -f "$INSTALL_BIN" ] && cp -a "$INSTALL_BIN" "$dir/xuanarmor" 2>/dev/null || true
    [ -f "$INSTALL_CFG" ] && cp -a "$INSTALL_CFG" "$dir/config.toml" 2>/dev/null || true
    [ -f "$SERVICE_FILE" ] && cp -a "$SERVICE_FILE" "$dir/xuanarmor.service" 2>/dev/null || true
    echo "${VERSION}" > "$dir/VERSION" 2>/dev/null || true
    # 最多保留 5 个备份，避免 /var/lib 无限增长
    ls -1dt "${BACKUP_ROOT}"/*/ 2>/dev/null | tail -n +6 | xargs -r rm -rf
    echo "$dir"
}

do_uninstall() {
    echo "── 卸载 XuanArmor ──"
    if [ "$SKIP_SYSTEMD" != "1" ]; then
        systemctl stop xuanarmor 2>/dev/null || true
        systemctl disable xuanarmor 2>/dev/null || true
        rm -f "$SERVICE_FILE"
        systemctl disable --now 'run-xuanarmor\x2dbpf.mount' 2>/dev/null || true
        rm -f '/etc/systemd/system/run-xuanarmor\x2dbpf.mount'
        systemctl daemon-reload 2>/dev/null || true
    fi
    rm -f "$INSTALL_BIN"
    info "已移除二进制与服务单元（配置与数据默认保留）"
    if [ "$PURGE" = "1" ]; then
        rm -rf /etc/xuanarmor "$(dirname "$STORE_PATH")" /var/log/xuanarmor
        info "已删除配置目录 /etc/xuanarmor、数据目录 $(dirname "$STORE_PATH")、日志目录 /var/log/xuanarmor"
    else
        warn "配置保留在 ${INSTALL_CFG}；如需彻底删除请加 --purge"
    fi
}

do_rollback() {
    local dir="${ROLLBACK_DIR}"
    if [ -z "$dir" ]; then
        dir="$(ls -1dt "${BACKUP_ROOT}"/*/ 2>/dev/null | head -1)"
        dir="${dir%/}"
    fi
    if [ -z "$dir" ] || [ ! -d "$dir" ]; then
        err "没有可用的备份目录（${BACKUP_ROOT}）"
        exit 1
    fi
    echo "── 回滚到备份：${dir} ──"
    [ -f "$dir/xuanarmor" ] || { err "备份中缺少二进制"; exit 1; }
    if [ "$SKIP_SYSTEMD" != "1" ]; then
        systemctl stop xuanarmor 2>/dev/null || true
    fi
    install -m 0755 "$dir/xuanarmor" "$INSTALL_BIN"
    [ -f "$dir/config.toml" ] && install -m 0644 "$dir/config.toml" "$INSTALL_CFG"
    [ -f "$dir/xuanarmor.service" ] && install -m 0644 "$dir/xuanarmor.service" "$SERVICE_FILE"
    if [ "$SKIP_SYSTEMD" != "1" ]; then
        systemctl daemon-reload
        systemctl start xuanarmor
    fi
    info "回滚完成：$([ -f "$dir/VERSION" ] && cat "$dir/VERSION" || echo unknown)"
}

download_release() {
    URL="https://github.com/${REPO}/releases/download/v${VERSION}/xuanarmor-${TARGET}"
    TMP_BIN="$(mktemp /tmp/xuanarmor-download.XXXXXX)"
    echo "下载 XuanArmor v${VERSION} (${TARGET})..."
    echo "  $URL"
    # -f：HTTP 错误直接失败（否则 404 的 HTML 会被当成二进制装进 /usr/local/bin）
    if ! curl -fsSL --retry 3 --retry-delay 2 "$URL" -o "$TMP_BIN"; then
        rm -f "$TMP_BIN"
        err "下载失败: $URL"
        err "  请检查版本号 v${VERSION} 是否存在该架构产物，或改用源码构建:"
        err "  sudo bash scripts/install.sh --build"
        exit 1
    fi
    if [ ! -s "$TMP_BIN" ] || [ "$(head -c 4 "$TMP_BIN" | od -An -tx1 | tr -d ' \n')" != "7f454c46" ]; then
        rm -f "$TMP_BIN"
        err "下载内容不是 ELF 可执行文件（可能命中了错误页面），已中止安装"
        exit 1
    fi

    # 若 release 提供了 SHA256SUMS 则校验；缺失时给出明确警告而不是静默安装。
    SUMS_URL="https://github.com/${REPO}/releases/download/v${VERSION}/SHA256SUMS"
    if curl -fsSL --retry 2 "$SUMS_URL" -o /tmp/xuanarmor-SHA256SUMS 2>/dev/null; then
        EXPECTED=$(awk -v f="xuanarmor-${TARGET}" '$2 == f || $2 == "*"f {print $1; exit}' /tmp/xuanarmor-SHA256SUMS)
        ACTUAL=$(sha256sum "$TMP_BIN" | awk '{print $1}')
        if [ -n "$EXPECTED" ] && [ "$EXPECTED" != "$ACTUAL" ]; then
            rm -f "$TMP_BIN" /tmp/xuanarmor-SHA256SUMS
            err "SHA256 校验失败（期望 $EXPECTED，实际 $ACTUAL），已中止安装"
            exit 1
        fi
        [ -n "$EXPECTED" ] && info "SHA256 校验通过"
        rm -f /tmp/xuanarmor-SHA256SUMS
    else
        warn "未找到 SHA256SUMS，跳过二进制校验（建议核对下载来源）"
    fi

    if [ -n "$EXPECTED_SHA" ]; then
        ACTUAL_SHA=$(sha256sum "$TMP_BIN" | awk '{print $1}')
        if [ "$EXPECTED_SHA" != "$ACTUAL_SHA" ]; then
            rm -f "$TMP_BIN"
            err "SHA256 校验失败（期望 ${EXPECTED_SHA}，实际 ${ACTUAL_SHA}），已中止安装"
            exit 1
        fi
        info "SHA256 校验通过（--sha256）"
    fi

    if smooth_upgrade "$TMP_BIN"; then
        rm -f "$TMP_BIN"
        info "平滑升级完成（旧进程退出期间 XDP 继续挂载）"
        exit 0
    fi

    install -m 0755 "$TMP_BIN" "$INSTALL_BIN"
    rm -f "$TMP_BIN"
}

# 平滑升级：当前版本若支持 `upgrade` 子命令（v0.5.1+），交给它完成替换 + 健康检查 + 自动回滚；
# 旧版本不认识该子命令，回退为"停服 → 安装 → 启动"。
smooth_upgrade() {
    local new_bin="$1"
    chmod 755 "$new_bin" 2>/dev/null || true
    if [ ! -x "$INSTALL_BIN" ] || ! "$INSTALL_BIN" upgrade --help >/dev/null 2>&1; then
        return 1
    fi
    if [ "$SKIP_SYSTEMD" = "1" ]; then
        return 1
    fi
    info "检测到已安装版本支持平滑升级（数据面不中断），使用 upgrade 子命令"
    "$INSTALL_BIN" upgrade \
        --binary "$new_bin" \
        --config "$INSTALL_CFG" \
        --install-bin "$INSTALL_BIN" \
        --backup-dir "$BACKUP_ROOT" \
        --timeout "${XUANARMOR_UPGRADE_TIMEOUT:-30}"
}

if [ "$ACTION" = "uninstall" ]; then
    do_uninstall
    exit 0
fi
if [ "$ACTION" = "rollback" ]; then
    do_rollback
    exit 0
fi

# 迁移 v0.5.x 旧命名安装（eshield -> xuanarmor）：
# 停止旧服务、备份旧二进制/配置、复制并改写配置与数据到新路径，并保留兼容软链。
migrate_legacy() {
    local old_bin="/usr/local/bin/eshield"
    local old_cfg="/etc/eshield/config.toml"
    local old_store="/var/lib/eshield"
    local old_log="/var/log/eshield"
    local old_unit="/etc/systemd/system/eshield.service"
    [ -e "$old_bin" ] || [ -e "$old_cfg" ] || [ -e "$old_unit" ] || return 0

    warn "检测到旧版 eshield 安装，开始迁移到 xuanarmor"
    if [ "$SKIP_SYSTEMD" != "1" ]; then
        systemctl stop eshield 2>/dev/null || true
        systemctl disable eshield 2>/dev/null || true
    fi
    mkdir -p "${BACKUP_ROOT}/legacy-$(date +%Y%m%d-%H%M%S)"
    local bdir
    bdir="$(ls -1dt "${BACKUP_ROOT}"/legacy-* 2>/dev/null | head -1)"
    [ -e "$old_bin" ] && cp -a "$old_bin" "$bdir/eshield" 2>/dev/null || true
    [ -e "$old_cfg" ] && cp -a "$old_cfg" "$bdir/config.toml" 2>/dev/null || true
    [ -e "$old_unit" ] && cp -a "$old_unit" "$bdir/eshield.service" 2>/dev/null || true

    mkdir -p /etc/xuanarmor
    if [ -f "$old_cfg" ] && [ ! -f "$INSTALL_CFG" ]; then
        sed -e 's#/etc/eshield#/etc/xuanarmor#g' \
            -e 's#/var/lib/eshield#/var/lib/xuanarmor#g' \
            -e 's#/var/log/eshield#/var/log/xuanarmor#g' \
            "$old_cfg" > "$INSTALL_CFG"
        info "已迁移配置: $old_cfg -> $INSTALL_CFG"
    fi
    if [ -d "$old_store" ] && [ ! -d /var/lib/xuanarmor ]; then
        cp -a "$old_store" /var/lib/xuanarmor
        info "已迁移数据目录: $old_store -> /var/lib/xuanarmor"
    fi
    if [ -d "$old_log" ] && [ ! -d /var/log/xuanarmor ]; then
        cp -a "$old_log" /var/log/xuanarmor 2>/dev/null || true
    fi
    [ -f "$old_unit" ] && rm -f "$old_unit"
    [ "$SKIP_SYSTEMD" != "1" ] && systemctl daemon-reload 2>/dev/null || true
    # 旧命令名兼容软链（保留一个版本周期，便于既有脚本过渡）
    if [ -e "$old_bin" ] && [ ! -e /usr/local/bin/eshield.compat ]; then
        rm -f "$old_bin"
        ln -sf /usr/local/bin/xuanarmor /usr/local/bin/eshield 2>/dev/null || true
    fi
    info "旧安装迁移完成（旧二进制/配置备份在 $bdir）"
}

migrate_legacy

# 升级前：停止旧服务并备份（避免覆盖正在运行的二进制）
if [ -f "$INSTALL_BIN" ]; then
    UPGRADING=1
    BACKUP_DIR="$(backup_current)"
    info "检测到已安装版本，已备份到：${BACKUP_DIR}"
    if [ "$SKIP_SYSTEMD" != "1" ] && systemctl is-active --quiet xuanarmor 2>/dev/null; then
        systemctl stop xuanarmor
        info "已停止旧版本服务，准备升级"
    fi
fi

if [ "$BUILD_MODE" = "1" ]; then
    build_local
else
    download_release
fi

mkdir -p /etc/xuanarmor
mkdir -p "$(dirname "$STORE_PATH")"
# systemd 单元会以 ReadWritePaths/StateDirectory/LogsDirectory 引用这些目录；
# 目录不存在会导致服务启动失败（status=226/NAMESPACE），必须先创建。
mkdir -p /var/lib/xuanarmor /var/log/xuanarmor
chmod 755 /var/lib/xuanarmor /var/log/xuanarmor 2>/dev/null || true

# XDP link 需要 pin 到 bpffs 才能在重启/升级期间保持挂载；
# 精简镜像/容器里 /sys/fs/bpf 常常没有挂载，这里准备独立 bpffs 挂载。
if [ "$SKIP_SYSTEMD" != "1" ]; then
    mkdir -p /run/xuanarmor-bpf
    if ! mountpoint -q /run/xuanarmor-bpf; then
        # 注意：mount 单元名必须与挂载点对应，路径中的 '-' 需转义为 \x2d
        cat > '/etc/systemd/system/run-xuanarmor\x2dbpf.mount' <<'MOUNTUNIT'
[Unit]
Description=bpffs for XuanArmor XDP link pinning

[Mount]
What=bpf
Where=/run/xuanarmor-bpf
Type=bpf
Options=mode=700

[Install]
WantedBy=multi-user.target
MOUNTUNIT
        systemctl daemon-reload
        systemctl enable --now 'run-xuanarmor\x2dbpf.mount' >/dev/null 2>&1 || warn "bpffs 挂载单元启用失败，XDP link 可能无法持久化"
        info "已准备 bpffs: /run/xuanarmor-bpf"
    fi
fi

CONSOLE_TOKEN=""
CONSOLE_TOKEN_NEW=0
if [ -f "$INSTALL_CFG" ]; then
    CONSOLE_TOKEN="$(sed -nE 's/^[[:space:]]*api_token[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' "$INSTALL_CFG" | head -1)"
fi
if [ -z "$CONSOLE_TOKEN" ]; then
    CONSOLE_TOKEN="$(gen_console_token)"
    CONSOLE_TOKEN_NEW=1
fi

if [ ! -f "$INSTALL_CFG" ]; then
    cat > "$INSTALL_CFG" <<'EOF'
# 要挂载 XDP 的网卡
interface = "eth0"

log_level = "info"
log_json = false
ebpf_log_enabled = false

udp_flood_enabled = true
icmp_flood_enabled = true
tcp_reset_on_drop = true

web_bind = "0.0.0.0:8720"
# 未设置时进程会启动时生成随机 Token 并在日志输出前缀，外部访问仍必须携带；
# 本机 127.0.0.1/::1 免校验（供 CLI/TUI 使用）。
# api_token = "changeme"

store_path = "/var/lib/xuanarmor/rules.redb"

# 只读 Token（可选，v0.5.0）：仅允许 GET/HEAD，适合监控系统 / 只读大屏
# readonly_token = "readonly-changeme"

# 告警 Webhook（可选）
# alert_webhook_url = "https://hooks.example.com/xuanarmor"
alert_webhook_type = "generic"
alert_threshold_dps = 1000
alert_cooldown_s = 60

whitelist = ["127.0.0.1/32", "10.0.0.0/8"]
blacklist = []

# 包日志（v0.5.2 起默认开启，内存环形缓冲，超限自动清理最旧条目）
[packet_log]
enabled = true
sample_rate = 16
memory_max_entries = 10000
max_bytes = 8388608

[xdp]
mode = "auto"
# 非首片 IPv4 分片策略（v0.5.0）：rate_limit（默认，按协议聚合限速）| drop | pass
fragment_policy = "rate_limit"
# 平滑升级（v0.5.1）：退出时保留 XDP 挂载（fail-closed），升级/重启期间防护不中断
persist_link = true
# pinned link 所在 bpffs；安装脚本会创建并挂载 /run/xuanarmor-bpf
pin_path = "/run/xuanarmor-bpf"

[rate_limit]
enabled = true
threshold = 2000
tick_ms = 100
decay_num = 7
decay_den = 8
block_duration_s = 120
# 超限处置（v0.5.0）：hard=写黑名单（默认），soft=只丢弃不拉黑（NAT 出口推荐）
block_mode = "hard"

# 协议 + 目的端口聚合限速（v0.5.0 起默认开启，防伪造源绕过 per-IP 计数）
[port_rate_limit]
enabled = true
threshold = 2000
tick_ms = 100

# 启动资源预检（v0.5.0）：低于 min 告警，低于 refuse 拒绝启动
[limits]
min_memory_mb = 512
refuse_memory_mb = 256

[syn_proxy]
enabled = true

[l7_scan]
enabled = true
patterns = []

[adaptive]
enabled = true
threshold = 10
window_s = 5
block_duration_s = 300

[geoip]
enabled = false
country_blocks_csv = "/etc/xuanarmor/geoip_country.csv"
block_countries = ["XX"]
default_action = "pass"

[threat_intel]
enabled = false
EOF

    # 写入安装时生成的控制台密码（仅在安装输出中显示一次）
    if [ "$CONSOLE_TOKEN_NEW" = "1" ]; then
        sed -i "s|^# api_token = \"changeme\"|api_token = \"${CONSOLE_TOKEN}\"|" "$INSTALL_CFG"
    fi    # 使用探测到的活动网卡，而不是硬编码 eth0
    if [ "$DEFAULT_IFACE" != "eth0" ]; then
        sed -i "s/^interface = \"eth0\"/interface = \"${DEFAULT_IFACE}\"/" "$INSTALL_CFG"
    fi
    echo "已创建默认配置文件: $INSTALL_CFG"
fi

# 已有配置但没有 api_token：写入安装时生成的控制台密码
if [ -f "$INSTALL_CFG" ] && ! grep -qE '^[[:space:]]*api_token[[:space:]]*=' "$INSTALL_CFG"; then
    sed -i "/^interface[[:space:]]*=/a api_token = \"${CONSOLE_TOKEN}\"" "$INSTALL_CFG"
    info "已向现有配置写入控制台密码"
fi

if [ -f "packaging/xuanarmor.service" ]; then
    install -m 0644 "packaging/xuanarmor.service" "$SERVICE_FILE"
elif curl -fsSL --max-time 10 "https://raw.githubusercontent.com/${REPO}/v${VERSION}/packaging/xuanarmor.service" -o "$SERVICE_FILE" 2>/dev/null; then
    chmod 644 "$SERVICE_FILE"
else
    warn "未能下载 systemd 单元，使用脚本内置默认单元"
    cat > "$SERVICE_FILE" <<'UNIT'
[Unit]
Description=XuanArmor eBPF/XDP L3-L4 DDoS Shield
Documentation=https://github.com/BlkSword/XuanArmor
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/xuanarmor start --config /etc/xuanarmor/config.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=3
LimitNOFILE=1048576
LimitMEMLOCK=infinity
StateDirectory=xuanarmor
LogsDirectory=xuanarmor
ReadWritePaths=/var/lib/xuanarmor /var/log/xuanarmor /sys/fs/bpf /run/xuanarmor-bpf
CapabilityBoundingSet=CAP_BPF CAP_NET_ADMIN CAP_NET_RAW CAP_PERFMON CAP_IPC_LOCK CAP_SYS_ADMIN
AmbientCapabilities=CAP_BPF CAP_NET_ADMIN CAP_NET_RAW CAP_PERFMON CAP_IPC_LOCK CAP_SYS_ADMIN
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
UNIT
    chmod 644 "$SERVICE_FILE"
fi

if [ "$SKIP_SYSTEMD" = "1" ]; then
    warn "XUANARMOR_INSTALL_SKIP_SYSTEMD=1，跳过 systemd 注册（用于安装脚本自测）"
else
    systemctl daemon-reload
    systemctl enable xuanarmor

    if [ "$NO_START" = "1" ]; then
        warn "已按 --no-start 跳过服务启动（修改配置后执行: sudo systemctl start xuanarmor）"
    # 升级场景必须 restart（reload 只热加载配置，不会替换正在运行的二进制）
    elif systemctl is-active --quiet xuanarmor; then
        if [ "${UPGRADING:-0}" = "1" ]; then
            systemctl restart xuanarmor
        else
            systemctl reload xuanarmor
        fi
    else
        systemctl start xuanarmor
    fi

    # 安装后健康检查：/ready 同时反映 XDP 是否真正挂载
    if [ "$NO_START" != "1" ]; then
        READY_OK=0
        for _ in $(seq 1 15); do
            CODE=$(curl -s -o /dev/null -m 2 -w '%{http_code}' "http://127.0.0.1:${WEB_PORT}/ready" 2>/dev/null || true)
            if [ "$CODE" = "200" ]; then READY_OK=1; break; fi
            sleep 2
        done
        if [ "$READY_OK" = "1" ]; then
            info "服务健康检查通过（/ready=200，XDP 已挂载）"
        else
            warn "服务未在 30 秒内就绪，请检查配置与日志："
            warn "  sudo journalctl -u xuanarmor -n 50 --no-pager"
            warn "  常见原因：网卡名不存在 / 端口被占用 / XDP 模式与驱动不兼容"
        fi
    fi
fi

echo ""
echo "========================================="
echo -e " ${GREEN}XuanArmor v${VERSION} 安装完成${NC}"
echo "========================================="
echo ""
echo "  控制台地址:  http://$(hostname -I 2>/dev/null | awk '{print $1}' || echo '<服务器IP>'):${WEB_PORT}"
if [ -n "$CONSOLE_TOKEN" ]; then
    echo ""
    echo -e "  ${GREEN}控制台密码:  ${CONSOLE_TOKEN}${NC}"
    echo "  （仅在本次安装输出一次，请立即保存；忘记可执行: sudo xuanarmor reset-token）"
fi
echo "  配置文件:    ${INSTALL_CFG}"
echo "  二进制路径:  ${INSTALL_BIN}"
echo "  持久化存储:  ${STORE_PATH}"
echo ""
echo "  管理命令:"
echo "    sudo systemctl status xuanarmor     # 查看状态"
echo "    sudo systemctl stop xuanarmor       # 停止服务"
echo "    sudo systemctl restart xuanarmor    # 重启服务"
echo "    sudo systemctl reload xuanarmor     # 热加载配置 (SIGHUP)"
echo "    sudo journalctl -u xuanarmor -f     # 实时日志"
echo ""
echo "  CLI 命令（本机免认证）:"
echo "    xuanarmor status                    # 查看运行状态"
echo "    xuanarmor block <IP> -d <秒>        # 封禁 IP"
echo "    xuanarmor unblock <IP>              # 解封 IP"
echo "    xuanarmor reload                    # 重载配置"
echo "    xuanarmor tui                       # TUI 仪表盘"
echo ""
if [ "$DEFAULT_IFACE" != "eth0" ]; then
    echo -e "  ${YELLOW}注意: 网卡已自动设置为 ${DEFAULT_IFACE}，如需修改请编辑 ${INSTALL_CFG}${NC}"
fi
if [ ! -f /sys/kernel/btf/vmlinux ]; then
    echo -e "  ${YELLOW}注意: BTF 未启用，部分功能可能受限${NC}"
fi
echo ""
