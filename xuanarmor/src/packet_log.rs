use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::warn;
use xuanarmor_common::{IpFamily, IpKey, PacketSample};

use crate::ip::format_ip_key;

/// 用户态展示用的采样包条目，包含人类可读的字符串字段。
#[derive(Debug, Clone, Serialize)]
pub struct PacketSampleEntry {
    pub timestamp_ns: u64,
    pub src_ip: String,
    pub dst_ip: String,
    pub src_port: u16,
    pub dst_port: u16,
    pub protocol: u8,
    pub action: u8,
    pub rule_id: u16,
    pub packet_len: u16,
    pub payload_bytes: u8,
    pub payload_hex: String,
}

impl From<PacketSample> for PacketSampleEntry {
    fn from(sample: PacketSample) -> Self {
        let src_key = match IpFamily::from_u8(sample.family) {
            Some(IpFamily::Ipv4) => IpKey::from_ipv4([
                sample.src_ip[12],
                sample.src_ip[13],
                sample.src_ip[14],
                sample.src_ip[15],
            ]),
            Some(IpFamily::Ipv6) => IpKey::from_ipv6(sample.src_ip),
            None => {
                warn!(
                    family = sample.family,
                    "packet sample has unknown IP family"
                );
                IpKey::default()
            }
        };
        let dst_key = match IpFamily::from_u8(sample.family) {
            Some(IpFamily::Ipv4) => IpKey::from_ipv4([
                sample.dst_ip[12],
                sample.dst_ip[13],
                sample.dst_ip[14],
                sample.dst_ip[15],
            ]),
            Some(IpFamily::Ipv6) => IpKey::from_ipv6(sample.dst_ip),
            None => {
                warn!(
                    family = sample.family,
                    "packet sample has unknown IP family"
                );
                IpKey::default()
            }
        };
        let payload = &sample.payload_sample[..sample.payload_bytes as usize];
        Self {
            timestamp_ns: sample.timestamp_ns,
            src_ip: format_ip_key(&src_key),
            dst_ip: format_ip_key(&dst_key),
            src_port: sample.src_port,
            dst_port: sample.dst_port,
            protocol: sample.protocol,
            // eBPF 侧 0=DROP/1=PASS；API 统一为 1=DROP/2=PASS，与攻击事件/前端约定一致。
            action: if sample.action == 1 { 2 } else { 1 },
            rule_id: sample.rule_id,
            packet_len: sample.packet_len,
            payload_bytes: sample.payload_bytes,
            payload_hex: bytes_to_hex(payload),
        }
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0xf) as usize]);
    }
    String::from_utf8(out).unwrap_or_default()
}

/// 采样包日志查询条件。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PacketLogQuery {
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub protocol: Option<u8>,
    pub action: Option<u8>,
    pub rule: Option<u16>,
    pub from_ns: Option<u64>,
    pub to_ns: Option<u64>,
    /// 时间游标：只返回时间戳严格早于该值的样本（v0.5.4，游标翻页用）
    #[serde(default)]
    pub before_ns: Option<u64>,
    /// 分页偏移（v0.5.4，控制台服务端翻页）
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    100
}

/// 固定容量的采样包日志内存缓冲。
/// 内部保存原始 `PacketSample` 以减少数据面消费时的 CPU 开销，仅在查询时转换为展示条目。
pub struct PacketLog {
    max_entries: usize,
    /// 采样条目占用上限（字节），超过后自动清理最旧条目。
    max_bytes: usize,
    entries: Mutex<VecDeque<PacketSample>>,
    bytes: std::sync::atomic::AtomicUsize,
}

impl PacketLog {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            entries: Mutex::new(VecDeque::with_capacity(max_entries.min(65536))),
            bytes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 批量写入采样包，整个批次只加一次锁；按条目数与字节数双上限自动清理最旧数据。
    pub fn push_many(&self, samples: &[PacketSample]) {
        if samples.is_empty() {
            return;
        }
        let entry_size = std::mem::size_of::<PacketSample>();
        let mut entries = self.entries.lock().unwrap();
        let mut bytes = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
        for sample in samples {
            if entries.len() >= self.max_entries {
                entries.pop_front();
                bytes = bytes.saturating_sub(entry_size);
            }
            entries.push_back(*sample);
            bytes += entry_size;
        }
        while bytes > self.max_bytes && entries.len() > 1 {
            entries.pop_front();
            bytes = bytes.saturating_sub(entry_size);
        }
        self.bytes
            .store(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    /// 当前占用（条目数, 估算字节数），供 /api/packets 与运维排查使用。
    pub fn usage(&self) -> (usize, usize) {
        let entries = self.entries.lock().unwrap();
        (
            entries.len(),
            self.bytes.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// 按条件过滤后的快照（新→旧）。
    ///
    /// v0.5.4：过滤在锁内直接完成，只克隆命中项，避免每页请求先复制整表
    /// （5 万条 ≈ 7 MB）再过滤；只有带 `ip` 过滤时才做 IP 字符串格式化。
    fn filtered_snapshot(&self, opts: &PacketLogQuery) -> Vec<PacketSample> {
        let ip_filter = opts.ip.as_deref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .rev()
            .filter(|s| {
                if let Some(before) = opts.before_ns {
                    if s.timestamp_ns >= before {
                        return false;
                    }
                }
                if let Some(ip) = &ip_filter {
                    let src = format_ip_for_filter(&s.src_ip, s.family);
                    let dst = format_ip_for_filter(&s.dst_ip, s.family);
                    if !src.to_lowercase().contains(ip) && !dst.to_lowercase().contains(ip) {
                        return false;
                    }
                }
                if let Some(port) = opts.port {
                    if s.src_port != port && s.dst_port != port {
                        return false;
                    }
                }
                if let Some(protocol) = opts.protocol {
                    if s.protocol != protocol {
                        return false;
                    }
                }
                if let Some(action) = opts.action {
                    if s.action != action {
                        return false;
                    }
                }
                if let Some(rule) = opts.rule {
                    if s.rule_id != rule {
                        return false;
                    }
                }
                if let Some(from) = opts.from_ns {
                    if s.timestamp_ns < from {
                        return false;
                    }
                }
                if let Some(to) = opts.to_ns {
                    if s.timestamp_ns > to {
                        return false;
                    }
                }
                true
            })
            .copied()
            .collect()
    }

    pub fn query(&self, opts: &PacketLogQuery) -> Vec<PacketSampleEntry> {
        self.filtered_snapshot(opts)
            .into_iter()
            .skip(opts.offset)
            .take(opts.limit)
            .map(Into::into)
            .collect()
    }

    /// 分页查询：返回（当前页条目, 过滤后总条数），供控制台服务端翻页（v0.5.4）。
    pub fn query_page(&self, opts: &PacketLogQuery) -> (Vec<PacketSampleEntry>, usize) {
        let filtered = self.filtered_snapshot(opts);
        let total = filtered.len();
        let page = filtered
            .into_iter()
            .skip(opts.offset)
            .take(opts.limit)
            .map(Into::into)
            .collect();
        (page, total)
    }

    /// 返回最新的 n 条采样（保留给未来 CLI / 快速预览使用）。
    #[allow(dead_code)]
    pub fn latest(&self, n: usize) -> Vec<PacketSampleEntry> {
        let entries = self.entries.lock().unwrap();
        entries.iter().rev().take(n).map(|s| (*s).into()).collect()
    }
}

fn format_ip_for_filter(addr: &[u8; 16], family: u8) -> String {
    match IpFamily::from_u8(family) {
        Some(IpFamily::Ipv4) => {
            format_ip_key(&IpKey::from_ipv4([addr[12], addr[13], addr[14], addr[15]]))
        }
        Some(IpFamily::Ipv6) => format_ip_key(&IpKey::from_ipv6(*addr)),
        None => String::new(),
    }
}

/// 消费一批 `PACKET_SAMPLES` Ring Buffer 事件（默认最多 1024 条），然后返回。
///
/// `ring_buf` 由调用方全生命周期持有：aya RingBuf 会缓存 producer 位置，
/// 每批重建句柄会使 consumer 位置越过 producer，导致已消费事件被无限重读。
pub async fn run(
    packet_log: Arc<PacketLog>,
    ring_buf: &mut aya::maps::RingBuf<aya::maps::MapData>,
) -> anyhow::Result<usize> {
    const BATCH_SIZE: usize = 1024;
    let samples: Vec<PacketSample> = {
        let mut samples = Vec::with_capacity(BATCH_SIZE);
        while let Some(item) = ring_buf.next() {
            if item.len() >= std::mem::size_of::<PacketSample>() {
                let sample: &PacketSample = unsafe { &*(item.as_ptr() as *const PacketSample) };
                samples.push(*sample);
            }
            if samples.len() >= BATCH_SIZE {
                break;
            }
        }
        samples
    };

    packet_log.push_many(&samples);

    if !samples.is_empty() {
        tracing::debug!(count = samples.len(), "packet_log consumer batch");
    }

    // 无论是否有事件，都让出 CPU，避免在高采样率场景下霸占工作线程。
    // 有事件时短暂 yield 即可，无事件时多等一会儿。
    if samples.is_empty() {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    } else {
        tokio::task::yield_now().await;
    }

    Ok(samples.len())
}
