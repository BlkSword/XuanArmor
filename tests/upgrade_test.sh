#!/bin/bash
# 平滑升级回归（v0.5.1）：
#   1) persist_link=true：进程退出后 XDP 仍挂载（数据面不中断，fail-closed）
#   2) 新进程启动时检测到旧程序并原子替换（replaced_existing）
#   3) `xuanarmor upgrade`（--no-systemd 模式）完成替换、健康检查通过
#   4) 无效二进制升级失败且不影响正在运行的服务（自动中止/回滚）
#   5) `xuanarmor detach` 能真正摘除 XDP
#
# 用法：sudo SKIP_BUILD=1 bash tests/upgrade_test.sh
set -u

if [ "$EUID" -ne 0 ]; then
    echo "Please run as root"
    exit 1
fi
if [ -z "${CARGO:-}" ]; then
    for c in "$(command -v cargo 2>/dev/null)" "$HOME/.cargo/bin/cargo" /root/.cargo/bin/cargo; do
        [ -x "$c" ] && CARGO="$c" && break
    done
fi
[ -n "${CARGO:-}" ] || { echo "FAIL: cargo not found"; exit 1; }
export PATH="$(dirname "$CARGO"):$PATH"
cd "$(dirname "$0")/.." || exit 1

if [ -z "${SKIP_BUILD:-}" ]; then
    "$CARGO" +"${XUANARMOR_EBPF_TOOLCHAIN:-nightly-2026-07-31}" build --package xuanarmor-ebpf \
        --target bpfel-unknown-none -Z build-std=core --release -q || exit 1
    "$CARGO" build --package xuanarmor --target x86_64-unknown-linux-musl --release -q || exit 1
fi

NS_S=up-server; NS_C=up-client
PORT=18771; API="http://127.0.0.1:${PORT}"
WORK="/tmp/xuanarmor-upgrade-test.$$"
BIN="$WORK/xuanarmor"
NEWBIN="$WORK/xuanarmor.new"
CFG="$WORK/config.toml"
FAILS=0
check() { if [ "$2" = "1" ]; then echo "[PASS] $3"; else echo "[FAIL] $3"; FAILS=$((FAILS+1)); fi; }

cleanup() {
    [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null
    ip netns del "$NS_C" 2>/dev/null
    ip netns del "$NS_S" 2>/dev/null
    if [ -z "${KEEP_WORK:-}" ]; then
        rm -rf "$WORK"
    else
        echo "[KEEP_WORK] 保留测试目录 $WORK"
    fi
}
trap cleanup EXIT

mkdir -p "$WORK"

# pinned XDP link 需要 bpffs，且必须在 host mount namespace 挂载：
# `ip netns exec` 内的 mount 属于该进程的私有 mount namespace，进程退出即消失。
# 真实部署里 /sys/fs/bpf 已由 systemd 挂好；测试/容器环境按需准备。
mount_bpffs() {
    mkdir -p /run/xuanarmor-bpf
    if ! mountpoint -q /run/xuanarmor-bpf; then
        mount -t bpf bpf /run/xuanarmor-bpf 2>/dev/null || true
    fi
    mountpoint -q /run/xuanarmor-bpf || echo "[WARN] /run/xuanarmor-bpf 不是 bpffs，pinned link 测试可能退化"
}
mount_bpffs
# 清理上一次运行可能残留的实例（它们会占着 redb 锁）
pkill -9 -f "^/tmp/xuanarmor-upgrade-test" 2>/dev/null || true
# 清理上次运行遗留的 pinned link（网卡已重建时它对应的 link 已失效）
rm -f /run/xuanarmor-bpf/xuanarmor_xdp_link /sys/fs/bpf/xuanarmor_xdp_link 2>/dev/null || true

ip netns del "$NS_C" 2>/dev/null; ip netns del "$NS_S" 2>/dev/null
ip netns add "$NS_S"; ip netns add "$NS_C"
ip link add veth-up type veth peer name veth-upc
ip link set veth-up netns "$NS_S"; ip link set veth-upc netns "$NS_C"
ip -n "$NS_S" addr add 10.77.0.1/24 dev veth-up
ip -n "$NS_C" addr add 10.77.0.2/24 dev veth-upc
ip -n "$NS_S" link set veth-up up; ip -n "$NS_C" link set veth-upc up
ip -n "$NS_S" link set lo up

cat > "$CFG" <<TOML
interface = "veth-up"
web_bind = "127.0.0.1:${PORT}"
log_level = "info"
store_path = "${WORK}/rules.redb"
whitelist = ["127.0.0.1/32"]
blacklist = []

[xdp]
mode = "generic"
persist_link = true
TOML
cp target/x86_64-unknown-linux-musl/release/xuanarmor "$BIN" && chmod +x "$BIN"
cp "$BIN" "$NEWBIN"

xdp_attached() { ip netns exec "$NS_S" ip -d link show veth-up 2>/dev/null | grep -q "xdp" && echo 1 || echo 0; }
ready() { ip netns exec "$NS_S" curl -s -m 2 -o /dev/null -w '%{http_code}' "$API/ready"; }

start_bin() { # $1=binary $2=log
    ip netns exec "$NS_S" "$1" start --config "$CFG" >"$2" 2>&1 &
    PID=$!
    for _ in $(seq 1 80); do
        [ "$(ready)" = "200" ] && return 0
        sleep 0.25
    done
    echo "启动失败:"; tail -20 "$2"
    return 1
}

echo "=== 1) 首次启动：XDP 已挂载 ==="
start_bin "$BIN" "$WORK/run1.log" || exit 1
check "1a" "$(xdp_attached)" "首次启动后 XDP 已挂载"
check "1b" "$([ "$(ready)" = "200" ] && echo 1 || echo 0)" "/ready=200"

echo "=== 2) SIGTERM：进程退出但 XDP 保持挂载（防护不中断）==="
kill -TERM "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
sleep 0.8
check "2a" "$([ -d "/proc/$PID" ] && echo 0 || echo 1)" "进程已退出"
check "2b" "$(xdp_attached)" "进程退出后 XDP 仍挂载（fail-closed）"

echo "=== 3) 重新启动：检测到旧程序并原子替换 ==="
start_bin "$BIN" "$WORK/run2.log" || exit 1
check "3a" "$(xdp_attached)" "重启后 XDP 仍挂载"
check "3b" "$(grep -q 'replaced_existing: true' "$WORK/run2.log" && echo 1 || echo 0)" "日志记录 replaced_existing=true"

echo "=== 4) 平滑升级（--no-systemd --pid）==="
OLD_PID=$PID
"$BIN" upgrade \
    --binary "$NEWBIN" \
    --config "$CFG" \
    --install-bin "$BIN" \
    --no-systemd --pid "$OLD_PID" \
    --log-file "$WORK/new.log" \
    --skip-config-check \
    --exec-prefix "ip netns exec up-server" \
    --backup-dir "$WORK/backup" \
    --timeout 20 >"$WORK/upgrade.log" 2>&1
UP_RC=$?
sleep 1
NEW_PID=$(pgrep -f "$BIN start" | head -1)
check "4a" "$([ "$UP_RC" = "0" ] && echo 1 || echo 0)" "upgrade 返回 0（rc=$UP_RC）"
check "4b" "$([ "$(ready)" = "200" ] && echo 1 || echo 0)" "升级后 /ready=200"
check "4c" "$(xdp_attached)" "升级后 XDP 仍挂载"
check "4d" "$([ -n "$NEW_PID" ] && echo 1 || echo 0)" "新进程已运行（PID=$NEW_PID）"
check "4e" "$([ -f "$WORK/backup"/*/xuanarmor ] && echo 1 || echo 0)" "已生成二进制备份"
if [ "$UP_RC" != "0" ]; then echo "--- upgrade log ---"; tail -20 "$WORK/upgrade.log"; fi

echo "=== 5) 无效二进制升级：必须失败且不影响现有服务 ==="
BEFORE_PID=$(pgrep -f "$BIN start" | head -1)
"$BIN" upgrade --binary /bin/false --config "$CFG" --install-bin "$BIN" \
    --no-systemd --pid "$BEFORE_PID" --log-file "$WORK/bad.log" \
    --skip-config-check --backup-dir "$WORK/backup2" --timeout 5 \
    --exec-prefix "ip netns exec up-server" \
    >"$WORK/upgrade_bad.log" 2>&1
BAD_RC=$?
AFTER_PID=$(pgrep -f "$BIN start" | head -1)
check "5a" "$([ "$BAD_RC" != "0" ] && echo 1 || echo 0)" "无效二进制升级被拒绝（rc=$BAD_RC）"
check "5b" "$([ "$BEFORE_PID" = "$AFTER_PID" ] && echo 1 || echo 0)" "原进程未被替换"
check "5c" "$([ "$(ready)" = "200" ] && echo 1 || echo 0)" "服务仍可用"

echo "=== 6) xuanarmor detach 摘除 XDP ==="
kill -TERM "$AFTER_PID" 2>/dev/null
sleep 0.5
ip netns exec "$NS_S" "$BIN" detach --config "$CFG" >/dev/null 2>&1
sleep 0.3
check "6" "$([ "$(xdp_attached)" = "0" ] && echo 1 || echo 0)" "detach 后 XDP 已摘除"

echo ""
if [ "$FAILS" -eq 0 ]; then
    echo "=== 平滑升级回归全部通过 ==="
else
    echo "=== 失败 $FAILS 项 ==="
fi
exit "$FAILS"
