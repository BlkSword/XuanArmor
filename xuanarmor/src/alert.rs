use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use crate::ip::format_ip_key;
use crate::state::Stats;

/// 告警配置
#[derive(Clone, Debug, Default)]
pub struct AlertConfig {
    pub webhook_url: Option<String>,
    pub webhook_type: String,
    /// 每秒 DROP 包数阈值
    pub threshold_dps: u64,
    /// 告警最小间隔（秒）
    pub cooldown_s: u64,
    /// 监控网卡
    pub interface: String,
}

/// 攻击者摘要
#[derive(Clone, Debug, Serialize)]
pub struct AlertAttacker {
    pub ip: String,
    pub count: u64,
}

/// 告警事件
#[derive(Clone, Debug, Serialize)]
pub struct AlertEvent {
    pub timestamp: u64,
    pub interface: String,
    pub alert_type: String,
    pub total_dropped: u64,
    pub dps: u64,
    pub threshold_dps: u64,
    pub cooldown_s: u64,
    pub message: String,
    pub rule_breakdown: HashMap<String, u64>,
    pub top_attackers: Vec<AlertAttacker>,
}

pub struct AlertManager {
    config: AlertConfig,
    client: reqwest::Client,
    last_alert_s: Mutex<u64>,
    /// map 水位告警独立冷却时间戳（v0.5.4），与 DPS 告警互不影响。
    last_map_alert_s: Mutex<u64>,
    notifier: std::sync::Mutex<Option<Arc<crate::notify::NotifyState>>>,
}

/// eBPF map 水位告警阈值（v0.5.4）：使用率达到该值即告警。
pub const MAP_USAGE_WARN_THRESHOLD: f64 = 0.8;
/// map 水位告警最小间隔（秒）：持续高水位时每 30 分钟最多告警一次。
const MAP_USAGE_ALERT_COOLDOWN_S: u64 = 1800;

/// 单张 map 的水位采样项，由 `sync_map_entries` 后台任务在采样后传入。
#[derive(Clone, Debug)]
pub struct MapWatermark {
    pub name: String,
    pub entries: u64,
    pub max_entries: u64,
    /// entries 是否已达到采样上限（真实值可能更大）——此时即使比例没到阈值也告警。
    pub capped: bool,
}

impl AlertManager {
    pub fn new(config: AlertConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            client: reqwest::Client::new(),
            last_alert_s: Mutex::new(0),
            last_map_alert_s: Mutex::new(0),
            notifier: std::sync::Mutex::new(None),
        })
    }

    /// 挂载通知通道：阈值告警会同时推送到已启用的通知通道。
    pub fn attach_notifier(&self, notifier: Arc<crate::notify::NotifyState>) {
        if let Ok(mut guard) = self.notifier.lock() {
            *guard = Some(notifier);
        }
    }

    /// 根据当前总 DROP 数检查是否触发告警。调用方应周期性传入 window_s 内的增量。
    pub async fn check(&self, stats: &Stats, delta: u64, window_s: u64) {
        if window_s == 0 || self.config.webhook_url.is_none() {
            return;
        }
        let dps = delta / window_s;
        if dps < self.config.threshold_dps {
            return;
        }

        let now_s = now_s();
        let mut last = self.last_alert_s.lock().await;
        if now_s.saturating_sub(*last) < self.config.cooldown_s {
            return;
        }
        *last = now_s;
        drop(last);

        let rule_breakdown = {
            let mut m = HashMap::new();
            m.insert(
                "blacklist".to_string(),
                stats.blacklist_blocked.load(Ordering::Relaxed),
            );
            m.insert(
                "rate_limit".to_string(),
                stats.rate_limited.load(Ordering::Relaxed),
            );
            m.insert(
                "syn_flood".to_string(),
                stats.syn_flood_blocked.load(Ordering::Relaxed),
            );
            m.insert("l7".to_string(), stats.l7_blocked.load(Ordering::Relaxed));
            m.insert(
                "adaptive".to_string(),
                stats.adaptive_blocked.load(Ordering::Relaxed),
            );
            m.insert(
                "udp_flood".to_string(),
                stats.udp_flood_blocked.load(Ordering::Relaxed),
            );
            m.insert(
                "icmp_flood".to_string(),
                stats.icmp_flood_blocked.load(Ordering::Relaxed),
            );
            m.insert(
                "geoip".to_string(),
                stats.geoip_blocked.load(Ordering::Relaxed),
            );
            m
        };

        let mut top_attackers: Vec<AlertAttacker> = stats
            .top_attackers
            .iter()
            .map(|e| AlertAttacker {
                ip: format_ip_key(e.key()),
                count: e.value().load(Ordering::Relaxed),
            })
            .collect();
        top_attackers.sort_by_key(|a| std::cmp::Reverse(a.count));
        top_attackers.truncate(5);

        let event = AlertEvent {
            timestamp: now_s,
            interface: self.config.interface.clone(),
            alert_type: "drop_rate".to_string(),
            total_dropped: stats.total_dropped.load(Ordering::Relaxed),
            dps,
            threshold_dps: self.config.threshold_dps,
            cooldown_s: self.config.cooldown_s,
            message: format!(
                "XuanArmor [{}] 告警：DROP 速率 {} pps 超过阈值 {} pps",
                self.config.interface, dps, self.config.threshold_dps
            ),
            rule_breakdown,
            top_attackers,
        };

        if let Some(url) = &self.config.webhook_url {
            let payload = self.format_payload(&event);
            if let Err(e) = self.client.post(url).json(&payload).send().await {
                tracing::warn!("failed to send alert webhook: {}", e);
            }
        }

        // 同步推送到新的多通道通知（webhook/钉钉/企业微信/飞书/Slack）。
        let notifier = self.notifier.lock().ok().and_then(|g| g.clone());
        if let Some(notifier) = notifier {
            let payload = serde_json::json!({
                "event": "attack_threshold",
                "interface": event.interface,
                "dps": event.dps,
                "threshold_dps": event.threshold_dps,
                "total_dropped": event.total_dropped,
                "rule_breakdown": event.rule_breakdown,
                "top_attackers": event.top_attackers,
                "message": event.message,
                "timestamp": event.timestamp,
            });
            tokio::spawn(async move {
                notifier.send_attack_alert(payload).await;
            });
        }
    }

    /// 检查 eBPF map 水位并在超阈值时告警（v0.5.4）。
    ///
    /// 由 `sync_map_entries` 后台任务在每次采样后调用（`metrics_interval_s` 周期），
    /// 请求路径不触发；告警按 [`MAP_USAGE_ALERT_COOLDOWN_S`] 限频，避免持续
    /// 高水位刷屏。webhook 与通知通道（邮件）同时投递，与 DPS 告警互不抑制。
    pub async fn check_map_usage(&self, maps: &[MapWatermark]) {
        // 注意：entries 受采样上限（MAP_ENTRY_SAMPLE_CAP）截断。除了按比例判断，
        // 达到采样上限也要告警——大容量 map（max > 上限/阈值比例）否则永远达不到比例阈值。
        let over: Vec<&MapWatermark> = maps
            .iter()
            .filter(|m| {
                m.max_entries > 0
                    && (m.capped
                        || (m.entries as f64 / m.max_entries as f64)
                            >= MAP_USAGE_WARN_THRESHOLD)
            })
            .collect();
        if over.is_empty() {
            return;
        }

        let now = now_s();
        let mut last = self.last_map_alert_s.lock().await;
        if now.saturating_sub(*last) < MAP_USAGE_ALERT_COOLDOWN_S {
            return;
        }
        *last = now;
        drop(last);

        let summary = over
            .iter()
            .map(|m| {
                if m.capped {
                    format!("{}(≥{}条，已达采样上限)", m.name, m.entries)
                } else {
                    format!(
                        "{}({:.0}%: {}/{})",
                        m.name,
                        m.entries as f64 / m.max_entries as f64 * 100.0,
                        m.entries,
                        m.max_entries
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "XuanArmor [{}] 告警：eBPF map 水位超过 {:.0}%：{}",
            self.config.interface,
            MAP_USAGE_WARN_THRESHOLD * 100.0,
            summary
        );
        let payload = serde_json::json!({
            "event": "map_usage",
            "interface": self.config.interface,
            "warn_threshold": MAP_USAGE_WARN_THRESHOLD,
            "message": message,
            "maps": over
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "name": m.name,
                        "entries": m.entries,
                        "max": m.max_entries,
                        "ratio": m.entries as f64 / m.max_entries as f64,
                        "capped": m.capped,
                    })
                })
                .collect::<Vec<_>>(),
            "timestamp": now,
        });

        if let Some(url) = &self.config.webhook_url {
            // 通用 JSON 载荷，兼容自定义接收端（不套用攻击告警的 slack/dingtalk 模板）。
            if let Err(e) = self.client.post(url).json(&payload).send().await {
                tracing::warn!("failed to send map usage webhook: {}", e);
            }
        }

        let notifier = self.notifier.lock().ok().and_then(|g| g.clone());
        if let Some(notifier) = notifier {
            tokio::spawn(async move {
                notifier.send_system_alert("map_usage", payload).await;
            });
        }
    }

    fn format_payload(&self, event: &AlertEvent) -> serde_json::Value {
        match self.config.webhook_type.as_str() {
            "slack" => serde_json::json!({
                "text": event.message,
                "blocks": [
                    {
                        "type": "section",
                        "text": {
                            "type": "mrkdwn",
                            "text": format!(
                                "*XuanArmor {} 告警*\nDROP 速率：{} pps\n阈值：{} pps\n总丢弃：{}\nTOP 攻击源：{}",
                                event.interface,
                                event.dps,
                                event.threshold_dps,
                                event.total_dropped,
                                event.top_attackers.iter().map(|a| format!("{}({})", a.ip, a.count)).collect::<Vec<_>>().join(", ")
                            )
                        }
                    }
                ]
            }),
            "dingtalk" => serde_json::json!({
                "msgtype": "text",
                "text": {
                    "content": format!(
                        "{}\n规则分布：{}\nTOP：{}",
                        event.message,
                        serde_json::to_string(&event.rule_breakdown).unwrap_or_default(),
                        event.top_attackers.iter().map(|a| format!("{}({})", a.ip, a.count)).collect::<Vec<_>>().join(", ")
                    )
                }
            }),
            "wecom" | "wechat" => serde_json::json!({
                "msgtype": "text",
                "text": {
                    "content": format!(
                        "{}\n规则分布：{}\nTOP：{}",
                        event.message,
                        serde_json::to_string(&event.rule_breakdown).unwrap_or_default(),
                        event.top_attackers.iter().map(|a| format!("{}({})", a.ip, a.count)).collect::<Vec<_>>().join(", ")
                    )
                }
            }),
            _ => serde_json::to_value(event).unwrap_or_default(),
        }
    }
}

/// 告警使用 wall-clock 时间，便于与外部系统（Slack/钉钉/企业微信）对齐。
fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
