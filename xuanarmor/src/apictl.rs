//! 上游应用远程控制 API（v0.5.2）。
//!
//! 提供编排系统所需的只读发现、策略原子下发、事件游标/SSE 与 Token 管理能力：
//!
//! - `GET  /api/version`：版本、API schema 与能力清单；
//! - `GET  /api/openapi.json`：机器可读接口描述；
//! - `POST /api/config/validate`：下发前校验配置（TOML 或 JSON）；
//! - `GET  /api/policy` / `POST /api/policy/apply`：读取/原子下发策略（支持 dry-run 与幂等键）；
//! - `GET  /api/events` / `GET /api/events/stream`：攻击事件游标查询与 SSE 推送；
//! - `GET/POST /api/tokens`、`DELETE /api/tokens/{name}`：多 Token 管理（仅 admin）。

use axum::{
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use xuanarmor_common::DropEvent;

use crate::auth::{self, AuthCtx, Role};
use crate::config::{
    ApiTokenConfig, Config, GeoIpConfig, L7ScanConfig, PortAclItem, PortRateLimitConfig,
    ProtectionProject, RateLimitConfig,
};
use crate::web::{monotonic_to_wall_ns, rule_display_name, wall_clock_offset_ns, WebState};

/// 当前 API schema 版本：新增字段向后兼容时递增。
pub const API_SCHEMA: u32 = 1;

/// 幂等缓存条目：(写入时间, 状态码, 响应体)。
pub type IdemEntry = (Instant, u16, Value);

pub fn router() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/version", get(version_handler))
        .route("/api/openapi.json", get(openapi_handler))
        .route("/api/config/validate", post(config_validate_handler))
        .route("/api/policy", get(policy_get_handler))
        .route("/api/policy/apply", post(policy_apply_handler))
        .route("/api/events", get(events_handler))
        .route("/api/events/stream", get(events_stream_handler))
        .route(
            "/api/tokens",
            get(tokens_list_handler).post(tokens_create_handler),
        )
        .route("/api/tokens/:name", delete(tokens_delete_handler))
        .route(
            "/api/notify",
            get(notify_get_handler).put(notify_put_handler),
        )
        .route("/api/notify/test", post(notify_test_handler))
        .route("/api/notify/deliveries", get(notify_deliveries_handler))
}

fn error_response(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": message.into(),
            "code": code,
        })),
    )
        .into_response()
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ───────────────────────── 发现接口 ─────────────────────────

/// `GET /api/version`：版本、API schema、能力清单与运行信息。
async fn version_handler(State(state): State<Arc<WebState>>) -> Json<Value> {
    let snapshot = state.control.runtime.read().await.clone();
    let xdp_mode = state.stats.xdp_mode_str();
    let tokens = state.auth.list_tokens().await.len();
    Json(json!({
        "name": "XuanArmor",
        "version": env!("CARGO_PKG_VERSION"),
        "api_schema": API_SCHEMA,
        "capabilities": [
            "policy.read",
            "policy.apply",
            "policy.dry_run",
            "config.validate",
            "events.poll",
            "events.stream",
            "tokens.manage",
            "idempotency_key",
            "openapi",
        ],
        "scopes": ["read", "write", "policy", "events", "policy", "system", "*"],
        "started_unix": state.started_unix,
        "uptime_s": state.started_at.elapsed().as_secs(),
        "pid": std::process::id(),
        "tokens": tokens,
        "runtime": {
            "version": snapshot.version,
            "interface": snapshot.interface,
            "web_port": snapshot.web_port,
            "web_bind": snapshot.web_bind,
            "xdp_mode": xdp_mode,
            "xdp_attached": state.stats.xdp_attached.load(std::sync::atomic::Ordering::Relaxed),
        },
    }))
}

/// `GET /api/openapi.json`：最小但可用的 OpenAPI 3 描述，便于生成 SDK。
async fn openapi_handler() -> Json<Value> {
    Json(json!({
        "openapi": "3.0.3",
        "info": {
            "title": "XuanArmor API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "XuanArmor 主机级 L3-L4 防护：上游编排/远程控制接口。",
        },
        "servers": [{"url": "http://127.0.0.1:8720", "description": "本机 / 控制台地址"}],
        "components": {
            "securitySchemes": {
                "bearer": {"type": "http", "scheme": "bearer"},
                "cookie": {"type": "apiKey", "in": "cookie", "name": auth::COOKIE_NAME},
            }
        },
        "security": [{"bearer": []}],
        "paths": {
            "/api/version": {"get": {"summary": "版本与能力清单", "responses": {"200": {"description": "OK"}}}},
            "/api/stats": {"get": {"summary": "运行统计", "responses": {"200": {"description": "OK"}}}},
            "/api/policy": {"get": {"summary": "读取当前策略与 etag", "responses": {"200": {"description": "OK"}}}},
            "/api/policy/apply": {"post": {
                "summary": "原子下发策略（支持 dry_run 与 Idempotency-Key）",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object"}}}},
                "responses": {"200": {"description": "已应用或 dry-run 结果"}, "400": {"description": "配置非法"}}
            }},
            "/api/config/validate": {"post": {
                "summary": "校验配置（TOML 文本或 JSON 对象）",
                "responses": {"200": {"description": "校验结果"}}
            }},
            "/api/events": {"get": {
                "summary": "攻击事件游标查询（after_seq 升序）",
                "parameters": [
                    {"name": "after_seq", "in": "query", "schema": {"type": "integer"}},
                    {"name": "limit", "in": "query", "schema": {"type": "integer", "maximum": 500}}
                ],
                "responses": {"200": {"description": "OK"}}
            }},
            "/api/events/stream": {"get": {
                "summary": "攻击事件 SSE 推送（event: attack）",
                "responses": {"200": {"description": "text/event-stream"}}
            }},
            "/api/tokens": {
                "get": {"summary": "列出 Token（不含密钥）", "responses": {"200": {"description": "OK"}}},
                "post": {"summary": "运行时创建 Token（返回一次密钥）", "responses": {
                    "200": {"description": "OK"}, "403": {"description": "需要 admin"}}}
            },
            "/api/tokens/{name}": {
                "delete": {"summary": "吊销运行时 Token", "responses": {
                    "204": {"description": "已吊销"}, "404": {"description": "不存在或来自配置文件"}}}
            }
        }
    }))
}

// ───────────────────────── 配置校验 ─────────────────────────

/// `POST /api/config/validate`：支持 `{"toml": "..."}` 或 `{"config": {...}}`。
async fn config_validate_handler(Json(body): Json<Value>) -> Json<Value> {
    if let Some(text) = body.get("toml").and_then(|v| v.as_str()) {
        return match Config::from_toml_str(text) {
            Ok(config) => Json(json!({
                "valid": true,
                "errors": [],
                "summary": {
                    "interface": config.interface,
                    "whitelist": config.whitelist.len(),
                    "blacklist": config.blacklist.len(),
                    "port_acl": config.port_acl.len(),
                    "projects": config.protection_projects.len(),
                    "l7_patterns": config.l7_scan.patterns.len(),
                }
            })),
            Err(err) => Json(json!({
                "valid": false,
                "errors": [format!("{err:#}")],
            })),
        };
    }
    if let Some(value) = body.get("config") {
        return match serde_json::from_value::<Config>(value.clone()) {
            Ok(config) => match config.validate() {
                Ok(()) => Json(json!({"valid": true, "errors": []})),
                Err(err) => Json(json!({"valid": false, "errors": [format!("{err:#}")]})),
            },
            Err(err) => Json(json!({"valid": false, "errors": [format!("{err}")]})),
        };
    }
    Json(json!({
        "valid": false,
        "errors": ["请求体需包含 `toml`（字符串）或 `config`（对象）"],
    }))
}

// ───────────────────────── 策略读取 / 下发 ─────────────────────────

fn policy_sections(config: &Config) -> Value {
    json!({
        "whitelist": config.whitelist,
        "blacklist": config.blacklist,
        "port_acl": config.port_acl,
        "protection_projects": config.protection_projects,
        "l7_scan": config.l7_scan,
        "geoip": config.geoip,
        "rate_limit": config.rate_limit,
        "port_rate_limit": config.port_rate_limit,
    })
}

fn policy_etag(config: &Config) -> String {
    sha256_hex(&policy_sections(config).to_string())
}

async fn policy_get_handler(State(state): State<Arc<WebState>>) -> Response {
    let config = match state.control.current_config().await {
        Ok(config) => config,
        Err(err) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_read_failed",
                format!("读取配置失败: {err:#}"),
            )
        }
    };
    Json(json!({
        "policy": policy_sections(&config),
        "etag": policy_etag(&config),
        "source": "runtime",
    }))
    .into_response()
}

/// `POST /api/policy/apply` 请求体：仅覆盖出现的字段。
#[derive(Debug, Default, Deserialize)]
pub struct PolicyApplyRequest {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub whitelist: Option<Vec<String>>,
    #[serde(default)]
    pub blacklist: Option<Vec<String>>,
    #[serde(default)]
    pub port_acl: Option<Vec<PortAclItem>>,
    #[serde(default)]
    pub protection_projects: Option<Vec<ProtectionProject>>,
    #[serde(default)]
    pub l7_scan: Option<L7ScanConfig>,
    #[serde(default)]
    pub geoip: Option<GeoIpConfig>,
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    pub port_rate_limit: Option<PortRateLimitConfig>,
    #[serde(default)]
    pub api_tokens: Option<Vec<ApiTokenConfig>>,
}

impl PolicyApplyRequest {
    fn is_empty(&self) -> bool {
        self.whitelist.is_none()
            && self.blacklist.is_none()
            && self.port_acl.is_none()
            && self.protection_projects.is_none()
            && self.l7_scan.is_none()
            && self.geoip.is_none()
            && self.rate_limit.is_none()
            && self.port_rate_limit.is_none()
            && self.api_tokens.is_none()
    }

    fn apply_to(&self, config: &mut Config) {
        if let Some(v) = &self.whitelist {
            config.whitelist = v.clone();
        }
        if let Some(v) = &self.blacklist {
            config.blacklist = v.clone();
        }
        if let Some(v) = &self.port_acl {
            config.port_acl = v.clone();
        }
        if let Some(v) = &self.protection_projects {
            config.protection_projects = v.clone();
        }
        if let Some(v) = &self.l7_scan {
            config.l7_scan = v.clone();
        }
        if let Some(v) = &self.geoip {
            config.geoip = v.clone();
        }
        if let Some(v) = &self.rate_limit {
            config.rate_limit = v.clone();
        }
        if let Some(v) = &self.port_rate_limit {
            config.port_rate_limit = v.clone();
        }
        if let Some(v) = &self.api_tokens {
            config.api_tokens = v.clone();
        }
    }
}

fn json_set<T: Serialize>(items: &[T]) -> HashSet<String> {
    items
        .iter()
        .filter_map(|item| serde_json::to_string(item).ok())
        .collect()
}

fn diff_section<T: Serialize>(old: &[T], new: &[T]) -> Value {
    let old_set = json_set(old);
    let new_set = json_set(new);
    let added: Vec<Value> = new_set
        .difference(&old_set)
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    let removed: Vec<Value> = old_set
        .difference(&new_set)
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    json!({"added": added, "removed": removed, "unchanged": new_set.len().saturating_sub(added.len())})
}

fn policy_diff(old: &Config, new: &Config) -> Value {
    json!({
        "whitelist": diff_section(&old.whitelist, &new.whitelist),
        "blacklist": diff_section(&old.blacklist, &new.blacklist),
        "port_acl": diff_section(&old.port_acl, &new.port_acl),
        "protection_projects": diff_section(&old.protection_projects, &new.protection_projects),
        "l7_patterns": diff_section(&old.l7_scan.patterns, &new.l7_scan.patterns),
        "geoip_changed": serde_json::to_string(&old.geoip).ok() != serde_json::to_string(&new.geoip).ok(),
        "rate_limit_changed": serde_json::to_string(&old.rate_limit).ok() != serde_json::to_string(&new.rate_limit).ok(),
        "port_rate_limit_changed": serde_json::to_string(&old.port_rate_limit).ok() != serde_json::to_string(&new.port_rate_limit).ok(),
        "api_tokens_changed": old.api_tokens.len() != new.api_tokens.len(),
    })
}

/// 读取幂等缓存；命中时返回之前的结果。
fn idempotency_lookup(state: &WebState, headers: &HeaderMap) -> Option<(u16, Value)> {
    let key = headers.get("idempotency-key")?.to_str().ok()?.trim();
    if key.is_empty() {
        return None;
    }
    let cache = state.idem.lock().ok()?;
    cache.get(key).and_then(|(at, status, body)| {
        if at.elapsed() < Duration::from_secs(600) {
            Some((*status, body.clone()))
        } else {
            None
        }
    })
}

fn idempotency_store(state: &WebState, headers: &HeaderMap, status: u16, body: &Value) {
    if let Some(key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        if let Ok(mut cache) = state.idem.lock() {
            cache.retain(|_, (at, _, _)| at.elapsed() < Duration::from_secs(600));
            cache.insert(key.to_string(), (Instant::now(), status, body.clone()));
        }
    }
}

async fn policy_apply_handler(
    State(state): State<Arc<WebState>>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Json(req): Json<PolicyApplyRequest>,
) -> Response {
    if !ctx.allows("policy") {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "token 缺少 policy 作用域",
        );
    }
    if req.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "empty_policy",
            "请求未包含任何策略字段",
        );
    }
    // 幂等重放
    if let Some((status, body)) = idempotency_lookup(&state, &headers) {
        let mut resp = (
            StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
            Json(body),
        )
            .into_response();
        resp.headers_mut()
            .insert("x-idempotent-replay", "true".parse().unwrap());
        return resp;
    }

    let old = match state.control.current_config().await {
        Ok(config) => config,
        Err(err) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_read_failed",
                format!("读取当前配置失败: {err:#}"),
            )
        }
    };
    let mut new_config = old.clone();
    req.apply_to(&mut new_config);
    if let Err(err) = new_config.validate() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_config",
            format!("策略校验失败: {err:#}"),
        );
    }
    let changes = policy_diff(&old, &new_config);
    let etag_old = policy_etag(&old);
    let etag_new = policy_etag(&new_config);

    if req.dry_run {
        let body = json!({
            "applied": false,
            "dry_run": true,
            "etag_before": etag_old,
            "etag_after": etag_new,
            "changes": changes,
        });
        idempotency_store(&state, &headers, 200, &body);
        return Json(body).into_response();
    }

    if let Err(err) = state.control.apply_runtime_config(&new_config).await {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "apply_failed",
            format!("策略应用失败: {err:#}"),
        );
    }
    state
        .control
        .audit_policy_applied(
            &format!("api:{}", ctx.token_name),
            json!({
                "action": "policy_apply",
                "actor": ctx.token_name,
                "role": ctx.role.as_str(),
                "etag_before": etag_old,
                "etag_after": etag_new,
                "changes": changes,
            }),
        )
        .await;

    let body = json!({
        "applied": true,
        "dry_run": false,
        "etag_before": etag_old,
        "etag_after": etag_new,
        "changes": changes,
        "note": "策略已应用到运行时；如重启后需保留，请同步写入配置文件并触发 reload",
    });
    idempotency_store(&state, &headers, 200, &body);
    Json(body).into_response()
}

// ───────────────────────── 事件 ─────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn event_to_json(seq: u64, event: &DropEvent, offset_ns: i128) -> Value {
    let src_ip = match xuanarmor_common::IpFamily::from_u8(event.family) {
        Some(xuanarmor_common::IpFamily::Ipv4) => {
            let o = [
                event.src_ip[12],
                event.src_ip[13],
                event.src_ip[14],
                event.src_ip[15],
            ];
            format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3])
        }
        Some(xuanarmor_common::IpFamily::Ipv6) => {
            crate::ip::format_ip_key(&xuanarmor_common::IpKey::from_ipv6(event.src_ip))
        }
        None => "unknown".to_string(),
    };
    json!({
        "seq": seq,
        "timestamp_ns": monotonic_to_wall_ns(event.timestamp_ns, offset_ns),
        "src_ip": src_ip,
        "protocol": event.protocol,
        "rule_id": event.rule_id,
        "rule_name": rule_display_name(event.rule_id),
        "dst_port": event.dst_port,
    })
}

/// `GET /api/events?after_seq=N&limit=M`：按序号升序返回攻击事件。
async fn events_handler(
    State(state): State<Arc<WebState>>,
    Query(q): Query<EventsQuery>,
) -> Json<Value> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let after = q.after_seq.unwrap_or(0);
    let offset = wall_clock_offset_ns();
    let events: Vec<Value> = state
        .stats
        .attack_events_since(after, limit)
        .iter()
        .map(|(seq, event)| event_to_json(*seq, event, offset))
        .collect();
    let latest = state.stats.attack_seq();
    let oldest_seq = latest.saturating_sub(state.stats.attack_events(1000).len() as u64);
    Json(json!({
        "events": events,
        "count": events.len(),
        "after_seq": after,
        "latest_seq": latest,
        "oldest_available_seq": oldest_seq,
        "has_more": events.len() == limit,
    }))
}

/// `GET /api/events/stream`：SSE 推送新事件（默认从当前最新序号开始，可在 query 指定 after_seq）。
async fn events_stream_handler(
    State(state): State<Arc<WebState>>,
    Query(q): Query<EventsQuery>,
) -> Response {
    let initial = q.after_seq.unwrap_or_else(|| state.stats.attack_seq());
    let stream_state = state.clone();
    let stream = futures::stream::unfold((stream_state, initial), |(state, last_seq)| async move {
        loop {
            let events = state.stats.attack_events_since(last_seq, 100);
            if !events.is_empty() {
                let next_seq = events.last().map(|(s, _)| *s).unwrap_or(last_seq);
                let offset = wall_clock_offset_ns();
                let payloads: Vec<Value> = events
                    .iter()
                    .map(|(s, e)| event_to_json(*s, e, offset))
                    .collect();
                let data = json!({
                    "events": payloads,
                    "count": payloads.len(),
                    "latest_seq": state.stats.attack_seq(),
                })
                .to_string();
                let event = Event::default().event("attack").data(data);
                return Some((Ok::<_, Infallible>(event), (state, next_seq)));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ───────────────────────── Token 管理 ─────────────────────────

async fn tokens_list_handler(State(state): State<Arc<WebState>>) -> Json<Value> {
    Json(auth::tokens_json(&state.auth).await)
}

#[derive(Debug, Deserialize)]
pub struct TokenCreateRequest {
    pub name: String,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

fn default_role() -> String {
    "operator".to_string()
}

async fn tokens_create_handler(
    State(state): State<Arc<WebState>>,
    Extension(ctx): Extension<AuthCtx>,
    Json(req): Json<TokenCreateRequest>,
) -> Response {
    if !ctx.role.is_admin() {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "创建 Token 需要 admin 角色",
        );
    }
    let Some(role) = Role::parse(&req.role) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_role",
            format!("非法角色: {}（可选 admin/operator/readonly）", req.role),
        );
    };
    match state
        .auth
        .create_runtime_token(&req.name, role, req.scopes.clone())
        .await
    {
        Ok(token) => {
            state
                .control
                .audit_policy_applied(
                    &format!("api:{}", ctx.token_name),
                    json!({"action": "token_create", "name": req.name, "role": role.as_str()}),
                )
                .await;
            Json(json!({
                "name": req.name,
                "role": role.as_str(),
                "scopes": req.scopes.unwrap_or_default(),
                "token": token,
                "note": "密钥仅返回一次；服务重启后运行时 Token 失效",
            }))
            .into_response()
        }
        Err(err) => error_response(
            StatusCode::CONFLICT,
            "token_create_failed",
            format!("{err:#}"),
        ),
    }
}

async fn tokens_delete_handler(
    State(state): State<Arc<WebState>>,
    Extension(ctx): Extension<AuthCtx>,
    Path(name): Path<String>,
) -> Response {
    if !ctx.role.is_admin() {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "吊销 Token 需要 admin 角色",
        );
    }
    if state.auth.revoke_runtime_token(&name).await {
        state
            .control
            .audit_policy_applied(
                &format!("api:{}", ctx.token_name),
                json!({"action": "token_revoke", "name": name}),
            )
            .await;
        (StatusCode::NO_CONTENT, "").into_response()
    } else {
        error_response(
            StatusCode::NOT_FOUND,
            "token_not_found",
            format!("未找到运行时 Token: {name}（配置文件中的 Token 不可通过 API 删除）"),
        )
    }
}

// ───────────────────────── 告警通知 ─────────────────────────

async fn notify_get_handler(State(state): State<Arc<WebState>>) -> Json<Value> {
    Json(state.notify.public_config().await)
}

async fn notify_put_handler(
    State(state): State<Arc<WebState>>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<Value>,
) -> Response {
    if !ctx.role.can_write() {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "只读凭据不能修改通知配置",
        );
    }
    let config: crate::notify::NotifyConfig = match serde_json::from_value(body) {
        Ok(config) => config,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_config",
                format!("通知配置解析失败: {e}"),
            )
        }
    };
    match state.notify.update(config).await {
        Ok(json) => Json(json!({"ok": true, "config": json})).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "save_failed",
            format!("保存通知配置失败: {e:#}"),
        ),
    }
}

async fn notify_test_handler(
    State(state): State<Arc<WebState>>,
    Extension(ctx): Extension<AuthCtx>,
) -> Response {
    if !ctx.role.can_write() {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "只读凭据不能发送测试通知",
        );
    }
    Json(state.notify.send_test().await).into_response()
}

#[derive(Debug, Default, Deserialize)]
struct DeliveryQuery {
    #[serde(default)]
    limit: Option<usize>,
}

async fn notify_deliveries_handler(
    State(state): State<Arc<WebState>>,
    Query(q): Query<DeliveryQuery>,
) -> Json<Value> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let items = state.notify.deliveries(limit).await;
    Json(json!({"deliveries": items, "count": items.len()}))
}
