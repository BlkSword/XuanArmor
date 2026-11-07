//! 告警通知（多通道）：webhook / 钉钉 / 企业微信 / 飞书 / Slack。
//!
//! 配置独立保存在 `<数据目录>/notify.json`（0600），可通过 API 读取/修改/测试发送，
//! 触发来源包括：DPS 阈值告警、手工测试。发送结果保留最近 200 条供控制台查看。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

const MAX_DELIVERIES: usize = 200;
const HUB_TIMEOUT_S: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Generic,
    Dingtalk,
    Wecom,
    Feishu,
    Slack,
}

impl ChannelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelKind::Generic => "generic",
            ChannelKind::Dingtalk => "dingtalk",
            ChannelKind::Wecom => "wecom",
            ChannelKind::Feishu => "feishu",
            ChannelKind::Slack => "slack",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotifyChannel {
    pub name: String,
    pub kind: ChannelKind,
    pub url: String,
    /// 可选签名/密钥（不同平台用途不同，原样保存）
    #[serde(default)]
    pub secret: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
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
    pub channels: Vec<NotifyChannel>,
}

fn default_threshold() -> u64 {
    1000
}
fn default_cooldown() -> u64 {
    60
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_dps: default_threshold(),
            cooldown_s: default_cooldown(),
            channels: Vec::new(),
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
    client: reqwest::Client,
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
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(HUB_TIMEOUT_S))
                .build()
                .unwrap_or_default(),
            last_alert_s: AtomicU64::new(0),
        })
    }

    /// 读取配置（密钥以布尔形式返回，不回传明文）。
    pub async fn public_config(&self) -> Value {
        let cfg = self.config.read().await;
        json!({
            "enabled": cfg.enabled,
            "threshold_dps": cfg.threshold_dps,
            "cooldown_s": cfg.cooldown_s,
            "channels": cfg.channels.iter().map(|c| json!({
                "name": c.name,
                "kind": c.kind.as_str(),
                "url": c.url,
                "has_secret": !c.secret.is_empty(),
                "enabled": c.enabled,
            })).collect::<Vec<_>>(),
        })
    }

    /// 保存配置：空 secret 表示保留原密钥（同名同 kind）。
    pub async fn update(&self, mut incoming: NotifyConfig) -> anyhow::Result<Value> {
        if incoming.threshold_dps > 0 && incoming.threshold_dps < 10 {
            anyhow::bail!("threshold_dps 过小（应 >= 10 或 0 表示不启用阈值告警）");
        }
        {
            let old = self.config.read().await;
            for ch in incoming.channels.iter_mut() {
                if ch.secret.is_empty() {
                    if let Some(prev) = old
                        .channels
                        .iter()
                        .find(|p| p.name == ch.name && p.kind.as_str() == ch.kind.as_str())
                    {
                        ch.secret = prev.secret.clone();
                    }
                }
            }
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

    /// 发送测试消息到所有已启用通道，返回每个通道结果。
    pub async fn send_test(&self) -> Value {
        let cfg = self.config.read().await.clone();
        let payload = json!({
            "event": "test",
            "title": "XuanArmor 测试通知",
            "message": "这是一条测试通知。如收到说明通知通道配置正确。",
            "timestamp": now_secs(),
        });
        let mut results = Vec::new();
        for channel in cfg.channels.iter().filter(|c| c.enabled) {
            results.push(self.send_to(channel, "test", &payload).await);
        }
        json!({
            "sent": results.len(),
            "results": results,
        })
    }

    /// 阈值告警：按 cooldown 限制触发频率后群发。
    pub async fn send_attack_alert(&self, payload: Value) {
        let cfg = self.config.read().await.clone();
        if !cfg.enabled {
            return;
        }
        let now = now_secs();
        let last = self.last_alert_s.load(Ordering::Relaxed);
        if now.saturating_sub(last) < cfg.cooldown_s {
            return;
        }
        self.last_alert_s.store(now, Ordering::Relaxed);
        for channel in cfg.channels.iter().filter(|c| c.enabled) {
            self.send_to(channel, "attack_threshold", &payload).await;
        }
    }

    async fn send_to(&self, channel: &NotifyChannel, event: &str, payload: &Value) -> Value {
        let text = render_text(event, payload);
        let body = match channel.kind {
            ChannelKind::Generic => payload.clone(),
            ChannelKind::Dingtalk => json!({"msgtype": "text", "text": {"content": text}}),
            ChannelKind::Wecom => json!({"msgtype": "text", "text": {"content": text}}),
            ChannelKind::Feishu => json!({"msg_type": "text", "content": {"text": text}}),
            ChannelKind::Slack => json!({"text": text}),
        };
        let started = Instant::now();
        let mut request = self.client.post(&channel.url).json(&body);
        if !channel.secret.is_empty() && channel.kind == ChannelKind::Slack {
            request = request.header("Authorization", format!("Bearer {}", channel.secret));
        }
        let (ok, status, error) = match request.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if resp.status().is_success() {
                    (true, Some(status), None)
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    (
                        false,
                        Some(status),
                        Some(body.chars().take(200).collect::<String>()),
                    )
                }
            }
            Err(e) => (false, None, Some(e.to_string())),
        };
        let delivery = Delivery {
            timestamp: now_secs(),
            event: event.to_string(),
            channel: channel.name.clone(),
            kind: channel.kind.as_str().to_string(),
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn render_text(event: &str, payload: &Value) -> String {
    if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
        return format!("[XuanArmor] {event}: {msg}");
    }
    format!("[XuanArmor] {event}: {payload}")
}
