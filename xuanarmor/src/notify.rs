//! 告警通知（SMTP 邮件）。
//!
//! 配置保存在 `<数据目录>/notify.json`（0600），支持运行时修改与测试发送；
//! 触发来源：DROP 速率超过阈值（按冷却时间限频）、手工测试。
//! 发送记录保留最近 200 条。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

const MAX_DELIVERIES: usize = 200;
const SEND_TIMEOUT_S: u64 = 15;

fn default_smtp_port() -> u16 {
    587
}
fn default_tls_mode() -> String {
    "starttls".to_string()
}
fn default_threshold() -> u64 {
    1000
}
fn default_cooldown() -> u64 {
    60
}

/// SMTP 邮件通知配置。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmailConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// 发件人，例如 `XuanArmor <alert@example.com>`
    #[serde(default)]
    pub from: String,
    /// 收件人列表
    #[serde(default)]
    pub to: Vec<String>,
    /// `tls`（465 隐式 TLS）| `starttls`（587）| `none`（明文，仅内网测试）
    #[serde(default = "default_tls_mode")]
    pub security: String,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: default_smtp_port(),
            username: String::new(),
            password: String::new(),
            from: String::new(),
            to: Vec::new(),
            security: default_tls_mode(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotifyConfig {
    #[serde(default)]
    pub enabled: bool,
    /// DPS 阈值（0 表示不触发阈值告警）
    #[serde(default = "default_threshold")]
    pub threshold_dps: u64,
    #[serde(default = "default_cooldown")]
    pub cooldown_s: u64,
    #[serde(default)]
    pub email: EmailConfig,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_dps: default_threshold(),
            cooldown_s: default_cooldown(),
            email: EmailConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Delivery {
    pub timestamp: u64,
    pub event: String,
    pub channel: String,
    pub kind: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub latency_ms: u64,
}

pub struct NotifyState {
    path: PathBuf,
    config: RwLock<NotifyConfig>,
    deliveries: Mutex<VecDeque<Delivery>>,
    last_alert_s: AtomicU64,
}

impl NotifyState {
    /// 从 `<store_path 父目录>/notify.json` 加载；文件不存在时使用默认配置。
    pub fn load(store_path: &str) -> Arc<Self> {
        let dir = std::path::Path::new(store_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/var/lib/xuanarmor"));
        let path = dir.join("notify.json");
        let config = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<NotifyConfig>(&bytes).unwrap_or_else(|e| {
                tracing::warn!("解析通知配置失败（{e}），使用默认配置");
                NotifyConfig::default()
            }),
            Err(_) => NotifyConfig::default(),
        };
        Arc::new(Self {
            path,
            config: RwLock::new(config),
            deliveries: Mutex::new(VecDeque::new()),
            last_alert_s: AtomicU64::new(0),
        })
    }

    /// 读取配置（密码以布尔形式返回，不回传明文）。
    pub async fn public_config(&self) -> Value {
        let cfg = self.config.read().await;
        json!({
            "enabled": cfg.enabled,
            "threshold_dps": cfg.threshold_dps,
            "cooldown_s": cfg.cooldown_s,
            "email": {
                "enabled": cfg.email.enabled,
                "host": cfg.email.host,
                "port": cfg.email.port,
                "username": cfg.email.username,
                "has_password": !cfg.email.password.is_empty(),
                "from": cfg.email.from,
                "to": cfg.email.to,
                "security": cfg.email.security,
            },
        })
    }

    /// 保存配置：空密码表示保留原密码。
    pub async fn update(&self, mut incoming: NotifyConfig) -> anyhow::Result<Value> {
        if incoming.threshold_dps > 0 && incoming.threshold_dps < 10 {
            anyhow::bail!("threshold_dps 过小（应 >= 10 或 0 表示不启用阈值告警）");
        }
        if incoming.email.enabled {
            if incoming.email.host.trim().is_empty() {
                anyhow::bail!("SMTP 服务器地址不能为空");
            }
            if incoming.email.from.trim().is_empty() {
                anyhow::bail!("发件人不能为空");
            }
            if incoming.email.to.is_empty() {
                anyhow::bail!("至少配置一个收件人");
            }
        }
        if incoming.email.password.is_empty() {
            incoming.email.password = self.config.read().await.email.password.clone();
        }
        let bytes = serde_json::to_vec_pretty(&incoming)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&self.path)?;
        file.write_all(&bytes)?;
        *self.config.write().await = incoming;
        Ok(self.public_config().await)
    }

    pub async fn deliveries(&self, limit: usize) -> Vec<Delivery> {
        let guard = self.deliveries.lock().await;
        guard.iter().rev().take(limit).cloned().collect()
    }

    async fn record(&self, delivery: Delivery) {
        let mut guard = self.deliveries.lock().await;
        if guard.len() >= MAX_DELIVERIES {
            guard.pop_front();
        }
        guard.push_back(delivery);
    }

    /// 发送测试邮件。
    pub async fn send_test(&self) -> Value {
        let cfg = self.config.read().await.clone();
        if !cfg.email.enabled {
            return json!({"sent": 0, "results": [], "error": "邮件通知未启用"});
        }
        let payload = json!({
            "event": "test",
            "message": "这是一条测试邮件。如收到说明 SMTP 配置正确。",
            "timestamp": now_secs(),
        });
        let result = self.send_email(&cfg.email, "test", &payload).await;
        json!({"sent": 1, "results": [result]})
    }

    /// 阈值告警：按冷却时间限制频率后发送邮件。
    pub async fn send_attack_alert(&self, payload: Value) {
        let cfg = self.config.read().await.clone();
        if !cfg.enabled || !cfg.email.enabled {
            return;
        }
        let now = now_secs();
        let last = self.last_alert_s.load(Ordering::Relaxed);
        if now.saturating_sub(last) < cfg.cooldown_s {
            return;
        }
        self.last_alert_s.store(now, Ordering::Relaxed);
        self.send_email(&cfg.email, "attack_threshold", &payload)
            .await;
    }

    async fn send_email(&self, email: &EmailConfig, event: &str, payload: &Value) -> Value {
        let started = Instant::now();
        let (ok, status, error) = match build_transport(email) {
            Ok(transport) => match build_message(email, event, payload) {
                Ok(message) => match transport.send(message).await {
                    Ok(resp) => (true, resp.code().to_string().parse::<u16>().ok(), None),
                    Err(e) => (false, None, Some(e.to_string())),
                },
                Err(e) => (false, None, Some(e.to_string())),
            },
            Err(e) => (false, None, Some(e.to_string())),
        };
        let delivery = Delivery {
            timestamp: now_secs(),
            event: event.to_string(),
            channel: "email".to_string(),
            kind: "smtp".to_string(),
            ok,
            status,
            error,
            latency_ms: started.elapsed().as_millis() as u64,
        };
        let value = serde_json::to_value(&delivery).unwrap_or_default();
        self.record(delivery).await;
        value
    }
}

fn build_transport(email: &EmailConfig) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
    let mode = email.security.to_ascii_lowercase();
    let builder = match mode.as_str() {
        "tls" | "ssl" => AsyncSmtpTransport::<Tokio1Executor>::relay(&email.host)?.port(email.port),
        "none" | "plain" => {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&email.host).port(email.port)
        }
        _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&email.host)?.port(email.port),
    };
    let builder = if email.username.is_empty() {
        builder
    } else {
        builder.credentials(Credentials::new(
            email.username.clone(),
            email.password.clone(),
        ))
    };
    Ok(builder
        .timeout(Some(Duration::from_secs(SEND_TIMEOUT_S)))
        .build())
}

fn build_message(model: &EmailConfig, event: &str, payload: &Value) -> anyhow::Result<Message> {
    let from: Mailbox = model
        .from
        .parse()
        .map_err(|e| anyhow::anyhow!("发件人格式错误: {e}"))?;
    let mut builder = Message::builder()
        .from(from)
        .subject(format!("[XuanArmor] {}", subject_for(event)))
        .header(lettre::message::header::ContentType::TEXT_PLAIN);
    for addr in &model.to {
        let mailbox: Mailbox = addr
            .parse()
            .map_err(|e| anyhow::anyhow!("收件人格式错误 {addr}: {e}"))?;
        builder = builder.to(mailbox);
    }
    Ok(builder.body(render_body(event, payload))?)
}

fn subject_for(event: &str) -> &'static str {
    match event {
        "attack_threshold" => "攻击流量告警",
        "test" => "测试通知",
        _ => "通知",
    }
}

fn render_body(event: &str, payload: &Value) -> String {
    let mut lines = Vec::new();
    lines.push(format!("事件: {event}"));
    if let Some(message) = payload.get("message").and_then(|v| v.as_str()) {
        lines.push(format!("描述: {message}"));
    }
    for (key, value) in payload.as_object().into_iter().flatten() {
        if key == "message" || key == "event" {
            continue;
        }
        lines.push(format!("{key}: {value}"));
    }
    lines.push(String::new());
    lines.push("由 XuanArmor 自动发送。".to_string());
    lines.join("\n")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
