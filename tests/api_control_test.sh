#!/bin/bash
# 上游编排 API 回归测试（v0.5.2）：
#   多 Token/角色、配置校验、策略 dry-run/原子下发、幂等键、事件游标、Token 管理。
#
# 需要 root + Linux；默认自动构建，SKIP_BUILD=1 可跳过。
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BIN="$ROOT/target/x86_64-unknown-linux-musl/release/xuanarmor"
WORK="$(mktemp -d /tmp/xuanarmor-api-test.XXXXXX)"
SRV_NS="xa-api-srv"
CLI_NS="xa-api-cli"
PORT=18795
BASE="http://10.88.0.1:${PORT}"
ADMIN="test-admin-token-0123456789abcdef"
READONLY="test-readonly-token-0123456789"
OPS="test-ops-token-0123456789abcdef"
PASS=0
FAIL=0
DAEMON_PID=""

log() { printf '%s\n' "$*"; }
ok() { PASS=$((PASS + 1)); log "  PASS: $1"; }
bad() { FAIL=$((FAIL + 1)); log "  FAIL: $1"; }

cleanup() {
    [ -n "${DAEMON_PID}" ] && kill "${DAEMON_PID}" 2>/dev/null
    sleep 0.3
    [ -n "${DAEMON_PID}" ] && kill -9 "${DAEMON_PID}" 2>/dev/null
    ip netns del "$CLI_NS" 2>/dev/null
    ip netns del "$SRV_NS" 2>/dev/null
    [ "${KEEP_WORK:-0}" = "1" ] || rm -rf "$WORK"
}
trap cleanup EXIT

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    log "==> 构建 eBPF + 用户态（release）"
    cargo +nightly-2026-07-31 build -p xuanarmor-ebpf --target bpfel-unknown-none -Z build-std=core --release -q || exit 1
    cargo build -p xuanarmor --target x86_64-unknown-linux-musl --release -q || exit 1
fi
[ -x "$BIN" ] || { log "缺少二进制：$BIN"; exit 1; }
EXPECTED_VERSION="$("$BIN" --version | awk '{print $2}')"

log "==> 准备 network namespace"
ip netns add "$SRV_NS" || exit 1
ip netns add "$CLI_NS" || exit 1
ip link add veth-api0 type veth peer name veth-api1 || exit 1
ip link set veth-api0 netns "$SRV_NS"
ip link set veth-api1 netns "$CLI_NS"
ip netns exec "$SRV_NS" ip addr add 10.88.0.1/24 dev veth-api0
ip netns exec "$CLI_NS" ip addr add 10.88.0.2/24 dev veth-api1
ip netns exec "$SRV_NS" ip link set veth-api0 up
ip netns exec "$CLI_NS" ip link set veth-api1 up
ip netns exec "$SRV_NS" ip link set lo up
ip netns exec "$CLI_NS" ip link set lo up

cat > "$WORK/config.toml" <<EOF
interface = "veth-api0"
whitelist = []
blacklist = []
log_level = "warn"
web_bind = "0.0.0.0:${PORT}"
store_path = "$WORK/rules.redb"

api_token = "${ADMIN}"
readonly_token = "${READONLY}"

[[api_tokens]]
name = "ops"
token = "${OPS}"
role = "operator"
scopes = ["read", "write", "policy", "events"]

[xdp]
mode = "generic"
persist_link = false
EOF

log "==> 启动守护进程"
ip netns exec "$SRV_NS" "$BIN" start --config "$WORK/config.toml" > "$WORK/daemon.log" 2>&1 &
DAEMON_PID=$!
READY=""
for _ in $(seq 1 50); do
    CODE="$(ip netns exec "$CLI_NS" curl -s -o /dev/null -m 1 -w '%{http_code}' "$BASE/ready" 2>/dev/null || true)"
    if [ "$CODE" = "200" ]; then READY=1; break; fi
    sleep 0.2
done
if [ -z "$READY" ]; then
    log "守护进程未就绪，日志："
    tail -30 "$WORK/daemon.log"
    exit 1
fi

# api <method> <path> <token|-> [json] [extra curl args...]
api() {
    local method="$1" path="$2" token="$3" body="${4:-}"
    local args=(-sS -o "$WORK/resp.json" -w '%{http_code}' -X "$method" "$BASE$path")
    [ "$token" != "-" ] && args+=(-H "Authorization: Bearer $token")
    if [ -n "$body" ]; then
        args+=(-H 'Content-Type: application/json' --data "$body")
    fi
    if [ $# -ge 5 ]; then
        shift 4
        args+=("$@")
    fi
    ip netns exec "$CLI_NS" curl "${args[@]}"
}
jget() { jq -r "$1" "$WORK/resp.json" 2>/dev/null; }
last_status=""

log "==> 1. 发现接口"
CODE="$(api GET /api/version "$ADMIN")"
[ "$CODE" = "200" ] && ok "GET /api/version 200" || bad "GET /api/version 期望 200，实际 $CODE"
[ "$(jget .version)" = "$EXPECTED_VERSION" ] && ok "version=$EXPECTED_VERSION" || bad "version 不匹配: $(jget .version)"
[ "$(jget .api_schema)" = "1" ] && ok "api_schema=1" || bad "api_schema 异常: $(jget .api_schema)"
jq -e '.capabilities | index("policy.apply")' "$WORK/resp.json" >/dev/null 2>&1 \
    && ok "capabilities 含 policy.apply" || bad "capabilities 缺少 policy.apply"

CODE="$(api GET /api/version -)"
[ "$CODE" = "401" ] && ok "无 Token 访问返回 401" || bad "无 Token 期望 401，实际 $CODE"

CODE="$(api GET /api/openapi.json "$ADMIN")"
[ "$CODE" = "200" ] && ok "GET /api/openapi.json 200" || bad "openapi 期望 200，实际 $CODE"
jq -e '.paths["/api/policy/apply"]' "$WORK/resp.json" >/dev/null 2>&1 \
    && ok "openapi 含 /api/policy/apply" || bad "openapi 缺少 policy/apply"

log "==> 2. 角色权限"
CODE="$(api GET /api/version "$READONLY")"
[ "$CODE" = "200" ] && ok "readonly 可读" || bad "readonly 读期望 200，实际 $CODE"
CODE="$(api POST /api/policy/apply "$READONLY" '{"dry_run":true,"blacklist":["203.0.113.9/32"]}')"
[ "$CODE" = "403" ] && ok "readonly 写操作 403" || bad "readonly 写期望 403，实际 $CODE"

log "==> 3. 配置校验"
VALID_TOML_BODY="$(jq -n --arg t $'interface = "veth-api0"\nwhitelist = []\nblacklist = []\napi_token = "abc"\n' '{toml: $t}')"
CODE="$(api POST /api/config/validate "$ADMIN" "$VALID_TOML_BODY")"
[ "$CODE" = "200" ] && [ "$(jget .valid)" = "true" ] && ok "合法配置校验通过" || bad "合法配置校验失败"
CODE="$(api POST /api/config/validate "$ADMIN" '{"toml":"interface ="}')"
[ "$CODE" = "200" ] && [ "$(jget .valid)" = "false" ] && ok "非法配置校验拒绝" || bad "非法配置未被拒绝"

log "==> 4. 策略读取/预演/下发"
CODE="$(api GET /api/policy "$ADMIN")"
ETAG_BEFORE="$(jget .etag)"
[ "$CODE" = "200" ] && [ -n "$ETAG_BEFORE" ] && [ "$ETAG_BEFORE" != "null" ] \
    && ok "GET /api/policy 返回 etag" || bad "GET /api/policy 异常"

CODE="$(api POST /api/policy/apply "$OPS" '{"dry_run":true,"blacklist":["203.0.113.9/32"]}')"
[ "$CODE" = "200" ] && [ "$(jget .dry_run)" = "true" ] && [ "$(jget .applied)" = "false" ] \
    && [ "$(jq '.changes.blacklist.added | length' "$WORK/resp.json")" = "1" ] \
    && ok "dry-run 返回变更预览" || bad "dry-run 结果异常"

CODE="$(api POST /api/policy/apply "$OPS" '{"blacklist":["203.0.113.9/32"]}')"
[ "$CODE" = "200" ] && [ "$(jget .applied)" = "true" ] && ok "策略原子下发成功" || bad "策略下发失败"
CODE="$(api GET /api/policy "$ADMIN")"
jq -e '.policy.blacklist | index("203.0.113.9/32")' "$WORK/resp.json" >/dev/null 2>&1 \
    && ok "下发后策略已生效" || bad "下发后策略未生效"

log "==> 5. 幂等重放"
CODE="$(api POST /api/policy/apply "$ADMIN" '{"whitelist":["10.88.0.0/24"]}' -H 'Idempotency-Key: test-key-001')"
ETAG1="$(jget .etag_after)"
CODE2="$(api POST /api/policy/apply "$ADMIN" '{"whitelist":["10.88.0.0/24"]}' -H 'Idempotency-Key: test-key-001')"
REPLAY="$(ip netns exec "$CLI_NS" curl -sS -D - -o /dev/null -X POST "$BASE/api/policy/apply" \
    -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' \
    -H 'Idempotency-Key: test-key-001' --data '{"whitelist":["10.88.0.0/24"]}' 2>/dev/null | tr -d '\r' | awk 'tolower($1)=="x-idempotent-replay:"{print $2}')"
ETAG2="$(jget .etag_after)"
[ "$CODE2" = "200" ] && [ "$REPLAY" = "true" ] && [ "$ETAG1" = "$ETAG2" ] \
    && ok "Idempotency-Key 重放命中缓存" || bad "幂等重放异常（replay=$REPLAY）"

log "==> 6. Token 管理"
CODE="$(api POST /api/tokens "$OPS" '{"name":"runtime-1","role":"readonly"}')"
[ "$CODE" = "403" ] && ok "operator 不能管理 Token" || bad "operator 管理 Token 期望 403，实际 $CODE"

CODE="$(api POST /api/tokens "$ADMIN" '{"name":"runtime-1","role":"readonly"}')"
RUNTIME_TOKEN="$(jget .token)"
[ "$CODE" = "200" ] && [ -n "$RUNTIME_TOKEN" ] && [ "$RUNTIME_TOKEN" != "null" ] \
    && ok "admin 创建运行时 Token" || bad "创建运行时 Token 失败"
CODE="$(api GET /api/version "$RUNTIME_TOKEN")"
[ "$CODE" = "200" ] && ok "新 Token 可用" || bad "新 Token 不可用"
CODE="$(api DELETE /api/tokens/runtime-1 "$ADMIN")"
[ "$CODE" = "204" ] && ok "吊销运行时 Token" || bad "吊销运行时 Token 期望 204，实际 $CODE"
CODE="$(api GET /api/version "$RUNTIME_TOKEN")"
[ "$CODE" = "401" ] && ok "吊销后 Token 失效" || bad "吊销后 Token 仍可用（$CODE）"

log "==> 7. 事件与 SSE"
CODE="$(api GET /api/events "$ADMIN")"
[ "$CODE" = "200" ] && jq -e '.latest_seq | type == "number"' "$WORK/resp.json" >/dev/null 2>&1 \
    && jq -e '.events | type == "array"' "$WORK/resp.json" >/dev/null 2>&1 \
    && ok "GET /api/events 结构正确" || bad "GET /api/events 异常"

log "==> 8. 重置密码"
CODE="$(api POST /api/auth/reset-token "$ADMIN")"
NEW_TOKEN="$(jget .token)"
if [ "$CODE" = "200" ] && [ -n "$NEW_TOKEN" ] && [ "$NEW_TOKEN" != "null" ]; then
    ok "reset-token 返回新密码"
else
    bad "reset-token 失败（$CODE）"
fi
if grep -qF "$NEW_TOKEN" "$WORK/config.toml"; then
    ok "新密码已写入配置文件"
else
    bad "新密码未写入配置文件"
fi
CODE="$(api GET /api/version "$ADMIN")"
[ "$CODE" = "401" ] && ok "旧密码立即失效" || bad "旧密码仍可用（$CODE）"
CODE="$(api GET /api/version "$NEW_TOKEN")"
[ "$CODE" = "200" ] && ok "新密码可访问" || bad "新密码不可用（$CODE）"

log
log "================ API 测试结果 ================"
log "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
