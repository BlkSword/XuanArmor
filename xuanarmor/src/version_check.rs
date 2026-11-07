//! 版本检测：周期性查询 GitHub 最新 Release，供控制台提示可升级版本。

use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

const REPO: &str = "BlkSword/XuanArmor";
const CHECK_INTERVAL_S: u64 = 6 * 3600;

#[derive(Clone, Debug)]
pub struct VersionSnapshot {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub checked_at: u64,
    pub release_url: Option<String>,
    pub error: Option<String>,
}

pub struct VersionCheck {
    snapshot: RwLock<VersionSnapshot>,
}

impl VersionCheck {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: RwLock::new(VersionSnapshot {
                current: env!("CARGO_PKG_VERSION").to_string(),
                latest: None,
                update_available: false,
                checked_at: 0,
                release_url: None,
                error: Some("尚未完成首次检测".to_string()),
            }),
        })
    }

    /// 启动后台检测任务（启动即检测一次，之后每 6 小时一次）。
    pub fn spawn(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.run_check().await;
                tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_S)).await;
            }
        });
    }

    pub async fn snapshot(&self) -> VersionSnapshot {
        self.snapshot.read().await.clone()
    }

    async fn run_check(&self) {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build();
        let result = match client {
            Ok(client) => {
                client
                    .get(format!(
                        "https://api.github.com/repos/{REPO}/releases/latest"
                    ))
                    .header("User-Agent", "XuanArmor-VersionCheck")
                    .header("Accept", "application/vnd.github+json")
                    .send()
                    .await
            }
            Err(e) => {
                self.set_error(format!("HTTP 客户端初始化失败: {e}")).await;
                return;
            }
        };
        match result {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(body) => {
                    let tag = body
                        .get("tag_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim_start_matches('v')
                        .to_string();
                    let url = body
                        .get("html_url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let current = env!("CARGO_PKG_VERSION").to_string();
                    let mut snap = self.snapshot.write().await;
                    snap.latest = if tag.is_empty() {
                        None
                    } else {
                        Some(tag.clone())
                    };
                    snap.update_available = !tag.is_empty() && version_gt(&tag, &current);
                    snap.checked_at = now_secs();
                    snap.release_url = url;
                    snap.error = None;
                }
                Err(e) => self.set_error(format!("解析 Release 响应失败: {e}")).await,
            },
            Ok(resp) => {
                self.set_error(format!("查询 Release 失败: HTTP {}", resp.status()))
                    .await;
            }
            Err(e) => self.set_error(format!("查询 Release 失败: {e}")).await,
        }
    }

    async fn set_error(&self, error: String) {
        let mut snap = self.snapshot.write().await;
        snap.checked_at = now_secs();
        snap.error = Some(error);
    }
}

/// 简化 semver 比较：按 `.` 分段比较数字，`a > b` 返回 true。
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let pa = parse(a);
    let pb = parse(b);
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::version_gt;

    #[test]
    fn compares_versions() {
        assert!(version_gt("0.5.3", "0.5.2"));
        assert!(version_gt("0.6", "0.5.9"));
        assert!(!version_gt("0.5.2", "0.5.2"));
        assert!(!version_gt("0.5.1", "0.5.2"));
    }
}
