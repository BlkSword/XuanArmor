//! API 认证与授权（v0.5.2 扩展）：
//!
//! - 支持多 Token：每个 Token 有名称、角色（admin / operator / readonly）与作用域；
//! - 兼容旧配置：`api_token` 视为 admin，`readonly_token` 视为 readonly；
//! - 本机回环请求（无反向代理头）自动获得 admin 身份，供 CLI 使用；
//! - 只读角色仅允许安全方法；Token 管理接口仅 admin 可访问；
//! - `AuthCtx` 会注入到请求扩展，便于审计与按 Token 统计。

use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{header::ACCEPT, header::AUTHORIZATION, header::COOKIE, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::ApiTokenConfig;

/// 会话 Cookie 名（控制台登录使用）。
pub const COOKIE_NAME: &str = "xuanarmor-token";

/// 访问角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    Operator,
    Readonly,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Readonly => "readonly",
        }
    }

    pub fn parse(value: &str) -> Option<Role> {
        match value.trim().to_ascii_lowercase().as_str() {
            "admin" => Some(Role::Admin),
            "operator" | "write" | "rw" => Some(Role::Operator),
            "readonly" | "read" | "ro" => Some(Role::Readonly),
            _ => None,
        }
    }

    pub fn can_write(&self) -> bool {
        !matches!(self, Role::Readonly)
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Role::Admin)
    }

    /// 未显式配置作用域时的默认值。
    fn default_scopes(&self) -> Vec<String> {
        match self {
            Role::Admin => vec!["*".to_string()],
            Role::Operator => vec![
                "read".to_string(),
                "write".to_string(),
                "policy".to_string(),
                "events".to_string(),
            ],
            Role::Readonly => vec!["read".to_string(), "events".to_string()],
        }
    }
}

/// 已认证请求的身份信息（注入到请求扩展）。
#[derive(Clone, Debug)]
pub struct AuthCtx {
    pub token_name: String,
    pub role: Role,
    pub scopes: Vec<String>,
}

impl AuthCtx {
    pub fn loopback() -> Self {
        Self {
            token_name: "loopback".to_string(),
            role: Role::Admin,
            scopes: vec!["*".to_string()],
        }
    }

    pub fn allows(&self, scope: &str) -> bool {
        self.scopes
            .iter()
            .any(|s| s == "*" || s.eq_ignore_ascii_case(scope))
    }
}

/// Token 元数据（不含密钥），供 `/api/tokens` 返回。
#[derive(Clone, Debug)]
pub struct TokenInfo {
    pub name: String,
    pub role: Role,
    pub scopes: Vec<String>,
    /// `config`（来自配置文件）或 `runtime`（运行时创建）
    pub source: &'static str,
}

impl TokenInfo {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "role": self.role.as_str(),
            "scopes": self.scopes,
            "source": self.source,
        })
    }
}

#[derive(Clone, Debug)]
struct TokenEntry {
    name: String,
    role: Role,
    scopes: Vec<String>,
    secret: Arc<str>,
    runtime: bool,
}

/// 认证状态：保存所有 Token，支持运行时创建/吊销。
#[derive(Clone, Debug, Default)]
pub struct AuthState {
    entries: Arc<RwLock<Vec<TokenEntry>>>,
}

impl AuthState {
    /// 从配置构建：兼容 `api_token` / `readonly_token`，并加载 `[[api_tokens]]`。
    pub fn from_config(
        token: Option<String>,
        readonly: Option<String>,
        api_tokens: &[ApiTokenConfig],
    ) -> Self {
        let mut entries = Vec::new();

        if let Some(secret) = token.filter(|s| !s.is_empty()) {
            entries.push(TokenEntry {
                name: "admin".to_string(),
                role: Role::Admin,
                scopes: Role::Admin.default_scopes(),
                secret: Arc::from(secret),
                runtime: false,
            });
        }
        if let Some(secret) = readonly.filter(|s| !s.is_empty()) {
            entries.push(TokenEntry {
                name: "readonly".to_string(),
                role: Role::Readonly,
                scopes: Role::Readonly.default_scopes(),
                secret: Arc::from(secret),
                runtime: false,
            });
        }
        for item in api_tokens {
            let secret = item.token.trim();
            if secret.is_empty() {
                continue;
            }
            let name = if item.name.trim().is_empty() {
                format!("token-{}", entries.len() + 1)
            } else {
                item.name.trim().to_string()
            };
            if entries.iter().any(|e| e.name == name) {
                tracing::warn!("重复的 API Token 名称 {name}，忽略后出现的配置项");
                continue;
            }
            let role = Role::parse(&item.role).unwrap_or(Role::Operator);
            let scopes = if item.scopes.is_empty() {
                role.default_scopes()
            } else {
                item.scopes.clone()
            };
            entries.push(TokenEntry {
                name,
                role,
                scopes,
                secret: Arc::from(secret),
                runtime: false,
            });
        }

        Self {
            entries: Arc::new(RwLock::new(entries)),
        }
    }

    /// 返回凭据对应身份；未匹配返回 None。
    pub async fn role_of(&self, provided: &str) -> Option<AuthCtx> {
        if provided.is_empty() {
            return None;
        }
        let guard = self.entries.read().await;
        guard
            .iter()
            .find(|e| constant_time_eq(&e.secret, provided))
            .map(|e| AuthCtx {
                token_name: e.name.clone(),
                role: e.role,
                scopes: if e.scopes.is_empty() && e.role.is_admin() {
                    vec!["*".to_string()]
                } else {
                    e.scopes.clone()
                },
            })
    }

    pub async fn verify(&self, provided: &str) -> bool {
        self.role_of(provided).await.is_some()
    }

    /// 是否已配置任何 Token（未配置时兼容旧部署：全部放行）。
    pub async fn is_configured(&self) -> bool {
        !self.entries.read().await.is_empty()
    }

    pub async fn set_token(&self, token: String) {
        if token.is_empty() {
            return;
        }
        let mut guard = self.entries.write().await;
        if let Some(entry) = guard.iter_mut().find(|e| e.name == "admin" && !e.runtime) {
            entry.secret = Arc::from(token.into_boxed_str());
        } else {
            guard.push(TokenEntry {
                name: "admin".to_string(),
                role: Role::Admin,
                scopes: Role::Admin.default_scopes(),
                secret: Arc::from(token.into_boxed_str()),
                runtime: false,
            });
        }
    }

    /// 生成新的管理 Token 并返回。
    pub async fn reset_token(&self) -> String {
        let token = generate_token();
        self.set_token(token.clone()).await;
        token
    }

    pub async fn list_tokens(&self) -> Vec<TokenInfo> {
        let guard = self.entries.read().await;
        guard
            .iter()
            .map(|e| TokenInfo {
                name: e.name.clone(),
                role: e.role,
                scopes: e.scopes.clone(),
                source: if e.runtime { "runtime" } else { "config" },
            })
            .collect()
    }

    /// 运行时创建 Token；名称重复时返回错误。
    pub async fn create_runtime_token(
        &self,
        name: &str,
        role: Role,
        scopes: Option<Vec<String>>,
    ) -> anyhow::Result<String> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("token name must not be empty");
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            anyhow::bail!("token name may only contain alphanumeric characters, '-', '_' or '.'");
        }
        let mut guard = self.entries.write().await;
        if guard.iter().any(|e| e.name == name) {
            anyhow::bail!("token name already exists: {name}");
        }
        let secret = generate_token();
        let scopes = scopes.unwrap_or_else(|| role.default_scopes());
        guard.push(TokenEntry {
            name: name.to_string(),
            role,
            scopes,
            secret: Arc::from(secret.clone().into_boxed_str()),
            runtime: true,
        });
        Ok(secret)
    }

    /// 吊销运行时 Token；配置文件中的 Token 不可通过 API 删除。
    pub async fn revoke_runtime_token(&self, name: &str) -> bool {
        let mut guard = self.entries.write().await;
        let before = guard.len();
        guard.retain(|e| !(e.runtime && e.name == name));
        guard.len() != before
    }
}

fn generate_token() -> String {
    format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    )
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// axum 中间件：对非本机来源的请求强制校验 Token。
pub async fn auth_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    // 本机请求直接放行；但带反向代理头时必须走 Token 校验。
    let proxy_headers_present = request.headers().contains_key("x-forwarded-for")
        || request.headers().contains_key("x-real-ip")
        || request.headers().contains_key("forwarded");
    if addr.ip().is_loopback() && !proxy_headers_present {
        request.extensions_mut().insert(AuthCtx::loopback());
        request.extensions_mut().insert(Role::Admin);
        return next.run(request).await;
    }

    let is_html_root = request.uri().path() == "/"
        && request
            .headers()
            .get(ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.contains("text/html"))
            .unwrap_or(false);

    if !state.is_configured().await {
        // 未配置任何 Token：兼容旧部署，视为管理身份。
        request.extensions_mut().insert(AuthCtx::loopback());
        request.extensions_mut().insert(Role::Admin);
        return next.run(request).await;
    }

    let header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let provided_from_header = header.and_then(|h| h.strip_prefix("Bearer "));
    let provided_from_cookie = request
        .headers()
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let mut parts = cookie.trim().splitn(2, '=');
                if parts.next()? == COOKIE_NAME {
                    parts.next()
                } else {
                    None
                }
            })
        });
    let provided = provided_from_header.or(provided_from_cookie);

    let Some(ctx) = (match provided {
        Some(token) => state.role_of(token).await,
        None => None,
    }) else {
        return if is_html_root {
            Response::builder()
                .status(StatusCode::FOUND)
                .header("Location", "/login")
                .body(Body::empty())
                .unwrap()
                .into_response()
        } else {
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Unauthorized",
                    "code": "unauthorized",
                })),
            )
                .into_response()
        };
    };

    let method = request.method().clone();
    let safe = method == Method::GET || method == Method::HEAD || method == Method::OPTIONS;
    let path = request.uri().path().to_string();

    // 只读角色：禁止写操作（登出除外）。
    if !ctx.role.can_write() && !safe && path != "/api/auth/logout" {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "read-only token: write operations are not permitted",
                "code": "forbidden",
                "role": ctx.role.as_str(),
            })),
        )
            .into_response();
    }

    // Token 管理仅 admin。
    if path.starts_with("/api/tokens") && !ctx.role.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "token management requires admin role",
                "code": "forbidden",
                "role": ctx.role.as_str(),
            })),
        )
            .into_response();
    }

    request.extensions_mut().insert(ctx.role);
    request.extensions_mut().insert(ctx);
    next.run(request).await
}

/// 将 Token 列表渲染成稳定 JSON（按名称排序）。
pub async fn tokens_json(state: &AuthState) -> serde_json::Value {
    let mut tokens: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for info in state.list_tokens().await {
        tokens.insert(info.name.clone(), info.to_json());
    }
    serde_json::json!({
        "tokens": tokens.values().cloned().collect::<Vec<_>>(),
        "count": tokens.len(),
    })
}
