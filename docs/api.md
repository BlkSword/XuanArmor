> # XuanArmor REST API 参考

> 版本：v0.5.0（2026-10-31），v0.5.4 增量见下

> **v0.5.4（未发布）变更摘要 —— 控制台大数量统计治理**：
> - 所有 JSON / 纯文本响应支持 `Accept-Encoding: gzip` 协商压缩（此前仅控制台 HTML 压缩）；
> - `GET /api/metrics/series` 默认按时长降采样（15m 原始 10s 粒度、1h 360 点、24h 720 点即 120s 粒度），
>   响应新增 `points` / `downsampled`，可用 `points` 参数覆盖；
> - `GET /api/blacklist` 改为服务端过滤 + 分页（`limit` 默认 200、上限 2000；`offset`/`q`/`reason`/`origin`），
>   响应新增 `total`；此前为全量返回（10 万条约 14 MB JSON）；
> - `GET /api/attack-events` 改为服务端分页（`limit` 默认 50、上限 200；`offset`/`q`/`protocol`），
>   响应新增 `total`/`window`/`truncated` 与稳定 `seq` 字段；**不再返回** `dst_ip`/`length` 等
>   数据面 `DropEvent` 中并不存在的字段；
> - `GET /api/packets` 新增 `offset` 与 `total`；`GET /api/audit` 的 `limit` 收紧到 1..1000、
>   新增 `actor` 过滤，`filter` 搜索范围包含来源 IP；
> - `GET /api/map-usage` 新增 `sampled_cap`（单表采样遍历上限 20000，`entries` 达到该值时真实值可能更大）；
> - eBPF map 水位 ≥ 80% 时通过 webhook 与通知通道（邮件）告警，冷却 30 分钟。

> **v0.5.0 变更摘要**：
> - 新增 `GET /api/map-usage`（map 水位）与 `GET /api/audit/verify`（审计哈希链校验）；
> - `GET /api/auth/check` 由返回 `OK` 文本改为返回 `{"role":"admin"|"readonly"}`；
> - 新增只读 Token（`readonly_token`）：只允许 `GET/HEAD/OPTIONS`，写操作返回 403；
> - `GET/PATCH /api/config` 新增 `fragment_policy`、`rate_limit_block_mode`；`GET` 另返回 `audit_path`/`audit_enabled`；
> - `/api/stats` 新增 `port_rate_blocked`、`fragment_blocked`；`/metrics` 新增
>   `xuanarmor_map_usage_ratio`、`xuanarmor_fragment_blocked_total`、`xuanarmor_port_rate_blocked_total`；
> - 新增规则/原因 ID：`FRAGMENT=14`（分片策略）、`PORT_RATE=15`（端口聚合限速）。

> **v0.4.5 破坏性变更**：`/api/attack-events` 与 `/api/packets` 返回的 `timestamp_ns` 字段语义由 **eBPF 单调时钟纳秒**（开机起算）改为 **wall-clock Unix 纳秒**（后端统一转换后返回）。此前依赖该字段自行对齐 `CLOCK_MONOTONIC` 的客户端需要改为按 Unix 时间戳处理。

## 认证

外部访问受保护端点时，若 `config.toml` 中设置了 `api_token`，需在请求头携带：

```
Authorization: Bearer <token>
```

认证支持两种角色（v0.5.0 RBAC-lite）：

| 凭据 | 角色 | 权限 |
|---|---|---|
| `api_token`（未配置时进程会生成随机 Token） | `admin` | 全部读写端点。 |
| `readonly_token`（顶层配置，可选） | `readonly` | 仅 `GET`/`HEAD`/`OPTIONS`；写方法返回 **403**。 |

- 本机 CLI 来源地址为 `127.0.0.1/::1`，自动跳过 token 校验（视为 `admin`）；
  但带 `X-Forwarded-For`/`X-Real-IP`/`Forwarded` 头的反向代理请求不享受该豁免。
- 浏览器登录后由后端下发 `xuanarmor-token` HttpOnly Cookie，Cookie 与 Bearer 等价；
  只读 Token 不能用于 `POST /api/auth/login` 登录控制台。
- 写方法使用只读 Token 时返回：

```json
403 {"error": "read-only token: write operations are not permitted"}
```

  唯一的写方法例外是 `POST /api/auth/logout`（用于清理浏览器会话 Cookie）。

## 端点概览

| 端点 | 方法 | 说明 |
|---|---|---|
| `/healthz` | GET | 健康检查（仅表示进程存活） |
| `/ready` | GET | 就绪检查（v0.4.8 起反映真实挂载状态，见下方说明） |
| `/login` | GET | 控制台登录入口（Vue SPA，登录态由 SPA 处理） |
| `/blocked` | GET | 403 封禁示例页 |
| `/api/auth/login` | POST | 控制台登录验证（成功后下发 HttpOnly `xuanarmor-token` Cookie） |
| `/api/auth/check` | GET | 校验当前凭据并返回角色（v0.5.0：JSON） |
| `/api/auth/logout` | POST | 清除控制台会话 Cookie（v0.4.8） |
| `/api/auth/reset-token` | POST | 重置访问令牌 |
| `/` | GET | Web Dashboard（Vue 3 单文件 SPA，`console/` 构建后嵌入 `xuanarmor/web/app.html`） |
| `/api/stats` | GET | 运行统计 |
| `/api/attack-events` | GET | 攻击事件（DROP），**v0.5.4** 服务端分页（`limit`/`offset`/`q`/`protocol`），`timestamp_ns` 为 wall-clock Unix 纳秒 |
| `/api/packets` | GET | 采样包日志，**v0.5.4** 服务端分页（`limit`/`offset`/`ip`/`action`），`timestamp_ns` 为 wall-clock Unix 纳秒 |
| `/api/config` | GET, PATCH | 读取/修改运行时配置 |
| `/api/config/reload` | POST | 从文件重新加载配置 |
| `/api/protection-modules` | GET | 防护模块列表与状态 |
| `/api/blacklist` | GET, POST, DELETE | 查询实时黑名单（**v0.5.4** 分页/过滤）/ 封禁 / 解封 IP |
| `/api/whitelist` | GET, POST, DELETE | 查询 / 添加 / 移除 CIDR 白名单 |
| `/api/audit` | GET | 审计日志（**v0.5.4** `limit` ≤1000、`offset`、`filter`/`actor`） |
| `/api/audit/stream` | GET | 审计日志 SSE |
| `/api/audit/verify` | GET | **v0.5.0**：审计文件哈希链校验 |
| `/api/metrics/series` | GET | 时序指标（**v0.5.4** 默认降采样；参数 `duration_s`、`points`；附 wall-clock `timestamp_ns`） |
| `/api/metrics/attacker-series` | GET | 单 IP 时序（**v0.5.4** 起仅 Top 20 攻击源有非零曲线） |
| `/api/map-usage` | GET | **v0.5.0**：eBPF map 条目数/上限/使用率（**v0.5.4** 新增 `sampled_cap`） |
| `/api/ip-detail` | GET | 单 IP 详情（控制台 IP 抽屉） |
| `/api/ip-series` | GET | 单 IP 时序（控制台 IP 抽屉） |
| `/api/port-acl` | GET, POST | 端口 ACL |
| `/api/protection-projects` | GET, POST | 防护项目（v0.4.6 起 PASS/DROP 在数据面生效；target_ips 支持 CIDR，IPv4 下限 /24） |
| `/api/l7-patterns` | GET, POST | L7 指纹 |
| `/api/geoip/reload` | POST | 重新加载 GeoIP CSV |
| `/api/threat-intel/sync` | POST | 手动触发威胁情报同步 |
| `/api/hub/status` | GET | 节点到 Hub 的连接状态 |
| `/api/hub/proxy/stats` | GET | 通过节点代理访问 Hub 全局统计 |
| `/api/hub/proxy/policies` | GET | 通过节点代理访问 Hub 策略查询接口 |
| `/api/hub/proxy/nodes` | GET | 通过节点代理访问 Hub 节点列表 |
| `/metrics` | GET | Prometheus 指标 |

## 详细说明

### GET /api/stats

返回实时统计。

```json
{
  "total_dropped": 1234,
  "current_pps": 250000,
  "current_dps": 135000,
  "blacklist_blocked": 100,
  "rate_limited": 500,
  "syn_flood_blocked": 50,
  "l7_blocked": 30,
  "adaptive_blocked": 20,
  "udp_flood_blocked": 10,
  "icmp_flood_blocked": 5,
  "geoip_blocked": 0,
  "conn_track_blocked": 0,
  "tcp_rst_sent": 0,
  "tcp_rst_fail": 0,
  "tcp_rst_attempt": 0,
  "tcp_dropped": 900,
  "udp_dropped": 300,
  "icmp_dropped": 34,
  "other_dropped": 0,
  "top_attackers": [
    {"ip": "192.0.2.1", "count": 100},
    {"ip": "2001:db8::1", "count": 50}
  ],
  "top_ports": [{"port": 443, "count": 100}],
  "trust_trusted": 0,
  "trust_neutral": 0,
  "trust_suspicious": 0,
  "trust_malicious": 0,
  "danger_level": 0,
  "xdp_attached": true,
  "xdp_mode": "native",
  "port_rate_blocked": 0,
  "fragment_blocked": 0
}
```

- `port_rate_blocked`（v0.5.0）：协议+目的端口聚合限速（`PORT_RATE`）拦截数；
- `fragment_blocked`（v0.5.0）：分片策略（`FRAGMENT`）拦截数；
- `xdp_attached` / `xdp_mode`（v0.4.8）：XDP 是否真的挂在网卡上、挂载模式（`native`/`generic`/`unknown`）；
- `danger_level`：全局危险等级 0/1/2；`trust_*`：Trust Score 分布。

### GET /api/config

返回当前运行时配置快照（`RuntimeConfigSnapshot`），主要字段：

```json
{
  "version": "0.5.0",
  "interface": "eth0",
  "web_port": 8720,
  "web_bind": "0.0.0.0:8720",
  "log_level": "info",
  "log_json": false,
  "store_path": "/var/lib/xuanarmor/rules.redb",
  "rate_limit_enabled": true,
  "syn_proxy_enabled": false,
  "l7_scan_enabled": false,
  "udp_flood_enabled": false,
  "icmp_flood_enabled": false,
  "geoip_enabled": false,
  "tcp_reset_on_drop": false,
  "trust_enabled": false,
  "conn_track_enabled": false,
  "conn_track_threshold": 20,
  "conn_track_window_ms": 10000,
  "trust_add_divisor": 100,
  "trust_sub_divisor": 3,
  "geoip_default_action": 0,
  "danger_level": 0,
  "rate_limit": {
    "enabled": true,
    "threshold": 2000,
    "tick_ms": 100,
    "decay_num": 7,
    "decay_den": 8,
    "block_duration_s": 120,
    "dport_grouping": false
  },
  "port_rate_limit": {
    "enabled": true,
    "threshold": 2000,
    "tick_ms": 100,
    "decay_num": 0,
    "decay_den": 0,
    "block_duration_s": 0,
    "dport_grouping": false
  },
  "fragment_policy": "rate_limit",
  "rate_limit_block_mode": "hard",
  "audit_path": "/var/log/xuanarmor/audit.log",
  "audit_enabled": false,
  "adaptive": { "...": "..." },
  "port_acl": [],
  "protection_projects": [],
  "l7_scan": { "...": "..." },
  "geoip": { "...": "..." },
  "threat_intel_feeds": [],
  "whitelist_entries": ["10.0.0.0/8"],
  "blacklist_entries": [],
  "hub_enabled": false,
  "hub_node_name": "",
  "hub_urls": [],
  "packet_log_enabled": false,
  "packet_log_sample_rate": 10000,
  "protection_projects_enabled": false
}
```

v0.5.0 新增字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `fragment_policy` | string | 非首片 IPv4 分片策略：`pass` / `rate_limit` / `drop`，默认 `rate_limit`。 |
| `rate_limit_block_mode` | string | 限速超限处置：`hard`（写黑名单）/ `soft`（只丢弃），默认 `hard`。 |
| `audit_enabled` | bool | 文件审计后端是否启用。 |
| `audit_path` | string | 审计文件路径（供 `/api/audit/verify` 使用）。 |

> 出于安全考虑，`api_token`、`readonly_token` 不会出现在任何响应中；
> `[limits]` 配置也不在快照中。

### PATCH /api/config

实时修改运行时开关与阈值，**立即生效**并写入数据面 `CONFIG`/`RATE_LIMIT_CFG` 等 map。
成功时返回纯文本 `配置已更新`；失败返回 `{"error":"..."}`（沿用现有错误约定）。

v0.5.0 可写字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `fragment_policy` | string | `pass` / `rate_limit` / `drop`；取值非法时报错。 |
| `rate_limit_block_mode` | string | `hard` / `soft`；取值非法时报错。 |
| `rate_limit` | object | `RateLimitParams`：`enabled`/`threshold`/`tick_ms`/`decay_num`/`decay_den`/`block_duration_s`/`dport_grouping`。 |
| `port_rate_limit` | object | 同上结构，但只读取 `enabled`/`threshold`/`tick_ms`（固定窗口，不做衰减）。 |
| `adaptive` | object | `AdaptiveConfig`。 |
| `rate_limit_enabled` / `syn_proxy_enabled` / `l7_scan_enabled` / `udp_flood_enabled` / `icmp_flood_enabled` / `geoip_enabled` / `tcp_reset_on_drop` / `trust_enabled` / `conn_track_enabled` | bool | 模块开关。 |
| `conn_track_threshold` / `conn_track_window_ms` | number | 连接跟踪参数（必须 > 0）。 |
| `trust_add_divisor` / `trust_sub_divisor` | number | Trust Score 除数（必须 > 0）。 |
| `geoip_default_action` | number | 0=pass（仅按 block 列表拦截）/ 1=drop（仅放行 allow 列表）。 |

示例：

```json
{
  "rate_limit_enabled": true,
  "udp_flood_enabled": true,
  "fragment_policy": "rate_limit",
  "rate_limit_block_mode": "soft",
  "rate_limit": {
    "enabled": true,
    "threshold": 2000,
    "tick_ms": 100,
    "decay_num": 7,
    "decay_den": 8,
    "block_duration_s": 120,
    "dport_grouping": false
  },
  "port_rate_limit": {
    "enabled": true,
    "threshold": 2000,
    "tick_ms": 100
  }
}
```

> `PATCH` 只影响运行时，不会写回 `/etc/xuanarmor/config.toml`；
> 重启或 `POST /api/config/reload` 后会回到配置文件中的值。

### POST /api/config/reload

从磁盘重新加载配置文件。

### GET /api/auth/check

校验当前凭据并返回角色（v0.5.0）。

```json
{"role": "admin"}
```

```json
{"role": "readonly"}
```

### GET /api/attack-events（v0.5.4 服务端分页）

返回攻击事件环形缓冲（服务端保留最近 1000 条）中的 DROP 事件。

| 参数 | 默认 | 说明 |
|---|---|---|
| `limit` | 50 | 每页条数，上限 200 |
| `offset` | 0 | 分页偏移（至多到窗口容量） |
| `q` | — | 匹配源 IP / 命中规则名 |
| `protocol` | — | 协议号：`6`=TCP / `17`=UDP / `1`=ICMP / `58`=ICMPv6 |

```json
{
  "events": [
    {"seq": 184223, "timestamp_ns": 1761234567890000000, "src_ip": "203.0.113.9", "protocol": 6,
     "rule_id": 2, "rule_name": "速率限制", "dst_port": 443, "action": "DROP"}
  ],
  "count": 1,
  "total": 214,
  "window": 1000,
  "truncated": true,
  "limit": 50,
  "offset": 0
}
```

- `seq` 为全局单调事件序号，跨页稳定，可作列表 key；
- `total` 为窗口内应用 `q`/`protocol` 后的总条数，`window` 为环形缓冲容量，
  `truncated` 表示当前页之外窗口内仍有数据；
- 数据面 `DropEvent` 只包含源 IP / 协议 / 规则 ID / 目的端口，**不包含目的 IP、包长**；
  v0.5.4 起不再返回这两个字段。

### GET /api/packets（v0.5.4 服务端分页）

查询采样包日志（内存缓冲，默认 `[packet_log].memory_max_entries = 50000` 条 / 8 MB）。

| 参数 | 默认 | 说明 |
|---|---|---|
| `limit` | 100 | 每页条数，上限 1000 |
| `offset` | 0 | 分页偏移（v0.5.4） |
| `ip` / `port` / `protocol` / `action` / `rule` | — | 过滤条件（`ip` 匹配源或目的地址） |
| `before_ns` | — | 游标：只返回时间戳早于该 wall-clock 纳秒的样本（推荐翻页方式，避免环形缓冲写入导致 offset 重复） |

- `action` 取值：**1=DROP、2=PASS**（v0.5.4 统一；旧值 0 按 DROP 兼容处理）；
- 响应新增 `total`（过滤后总条数）、`limit`/`offset`、`before_ns`；`usage` 为缓冲占用（`entries`/`bytes`）。

### GET /api/audit（v0.5.4）

审计日志分页查询。

| 参数 | 默认 | 说明 |
|---|---|---|
| `limit` | 100 | 每页条数，**v0.5.4 收紧到 1..=1000** |
| `offset` | 0 | 分页偏移（至多 10000） |
| `filter` | — | 全文搜索（时间 / 操作者 / 动作 / 详情 / 来源 IP） |
| `actor` | — | 操作者精确匹配（v0.5.4） |
| `ip` / `action` / `from` / `to` | — | 其它过滤条件 |

响应：`{entries, count, total, limit, offset}`。

### GET /api/map-usage（v0.5.0）

返回每个 eBPF map 的条目数、容量与使用率。数据来自后台采样缓存
（`metrics_interval_s`，默认 30s），抓取路径不加锁、不遍历大表。

```json
{
  "maps": [
    {"name": "BLACKLIST", "type": "LruHashMap", "entries": 12000, "max": 100000, "ratio": 0.12},
    {"name": "RATE_MAP", "type": "LruHashMap", "entries": 18100, "max": 100000, "ratio": 0.181},
    {"name": "CONFIG", "type": "Array", "entries": 0, "max": 1, "ratio": 0.0}
  ],
  "warn_threshold": 0.8,
  "sampled_cap": 20000,
  "sampled_ns": 1761234567890000000
}
```

- `entries` 只对 LRU 表有真实采样值（`BLACKLIST`/`RATE_MAP`/`RATE_MAP_PORT`/`TRUST_MAP`/
  `CONN_TRACK`/`SYN_PROXY_CONN`/`TOP_ATTACKERS`/`PROJECT_POLICY`）；Array/RingBuf/LPM 表固定为 0；
- `sampled_cap`（v0.5.4）：单表采样遍历上限。`entries == sampled_cap` 时真实条目数可能更大，
  `ratio` 因此是「至少」水位；控制台显示为 `20000+`；
- `warn_threshold` 固定 0.8；**v0.5.4 起**后台采样任务在水位 ≥ 该值时通过
  顶层配置 `alert_webhook_url` 与通知通道（邮件，`notify.json`）告警，冷却 30 分钟；
- 容量估算与告警规则见 [operations.md](operations.md) 与 [capacity-planning.md](capacity-planning.md)。

### GET /api/audit/verify（v0.5.0）

校验文件审计后端的哈希链完整性（每条记录含 `seq`/`prev_hash`/`hash`，
`hash = sha256(seq|prev_hash|规范化内容)`）。

```json
{"ok": true, "checked": 1024, "first_bad_seq": null, "path": "/var/log/xuanarmor/audit.log"}
```

```json
{"ok": false, "checked": 900, "first_bad_seq": 513, "path": "/var/log/xuanarmor/audit.log"}
```

- 文件审计未启用时返回 `{"ok":true,"enabled":false,"message":"..."}`；
- 只能校验当前 `audit.path` 指向的单个文件；轮转后的 `audit.log.1~.3` 不在校验范围
  （详见 [operations.md](operations.md) 的审计链巡检建议）；
- 读取/解析审计文件失败时返回 500 `{"error":"..."}`。

### GET /api/blacklist

返回当前仍有效的黑名单条目（来自 eBPF `BLACKLIST` 实时表）。

**v0.5.4 起支持服务端过滤与分页**（此前全量返回，`BLACKLIST` 上限 10 万条时约 14 MB JSON）：

| 参数 | 默认 | 说明 |
|---|---|---|
| `limit` | 200 | 每页条数，上限 2000 |
| `offset` | 0 | 分页偏移 |
| `q` | — | IP 子串过滤 |
| `reason` | — | 封禁原因：中文标签（与响应 `reason` 一致，如 `速率限制`）或规则 ID（数字，推荐） |
| `origin` | — | 来源标签：`api` / `adaptive` / `config` / `policy` / `data` |

结果按封禁时间（`created_ns`）倒序，同时间按 IP 排序，翻页顺序稳定。

```json
{
  "entries": [
    {"ip": "203.0.113.9", "reason": "黑名单", "origin": "api", "created_ns": 0, "expires_ns": 0, "hits": 9812443}
  ],
  "count": 1,
  "total": 8421,
  "limit": 200,
  "offset": 0
}
```

- `entries`/`count` 为当前页数据与条数（`count` 字段保持旧版兼容）；
- `total`（v0.5.4）为应用 `q`/`reason`/`origin` 后的总条数。

### GET /api/whitelist

返回当前白名单 CIDR。

```json
{
  "entries": [{"cidr": "10.0.0.0/8", "note": "内网"}],
  "count": 1
}
```

### POST /api/blacklist

封禁 IP。

```json
{
  "ip": "192.0.2.1",
  "duration_s": 300
}
```

`duration_s` 为 0 表示永久。

### DELETE /api/blacklist

解封 IP。

```json
{
  "ip": "192.0.2.1"
}
```

### POST /api/whitelist

放行 CIDR。

```json
{
  "cidr": "10.0.0.0/8"
}
```

### DELETE /api/whitelist

移除 CIDR 放行。

```json
{
  "cidr": "10.0.0.0/8"
}
```

### GET /metrics

Prometheus 指标。v0.5.0 新增：

| 指标 | 类型 | 说明 |
|---|---|---|
| `xuanarmor_map_usage_ratio{interface,name}` | gauge | map 使用率（entries/max_entries），仅采样表有值。 |
| `xuanarmor_map_entries{interface,name}` | gauge | 采样表当前条目数。 |
| `xuanarmor_map_max_entries{interface,name,map_type}` | gauge | map 容量（启动时采集，不变）。 |
| `xuanarmor_fragment_blocked_total{interface,policy}` | counter | 分片策略拦截数，`policy` 为当前策略。 |
| `xuanarmor_port_rate_blocked_total{interface}` | counter | 端口聚合限速拦截数。 |

已有指标：`xuanarmor_dropped_total`、`xuanarmor_passed_total`、`xuanarmor_blacklist_blocked_total`、
`xuanarmor_rate_limited_total`、`xuanarmor_syn_flood_blocked_total`、`xuanarmor_l7_blocked_total`、
`xuanarmor_adaptive_blocked_total`、`xuanarmor_udp_flood_blocked_total`、`xuanarmor_icmp_flood_blocked_total`、
`xuanarmor_geoip_blocked_total`、`xuanarmor_conn_track_blocked_total`、
`xuanarmor_dropped_by_protocol_total{protocol}`、`xuanarmor_dropped_by_port_total{port}`、
`xuanarmor_source_dropped_total{ip}`、`xuanarmor_event_consumer_duration_us`（直方图）。

> 指标抓取需要 `api_token` 或 `readonly_token`（GET 即可）。

### GET /healthz

进程存活检查。

### GET /ready

服务就绪检查。v0.4.8 起不再只检查"程序对象是否存在"，而是检查 **XDP 是否真的挂在网卡上**
（由后台任务每 15s 校验）：

- 已挂载：`200 {"status":"ready","xdp_attached":true,"xdp_mode":"native|generic"}`
- 未挂载/被摘掉/重挂失败：`503 {"status":"not ready","xdp_attached":false,"xdp_mode":"...","error":"XDP program is not attached to the interface"}`

`/healthz` 只表示进程存活，不反映防护是否生效。

## 错误码约定

所有 JSON 错误响应统一为：

```json
{"error": "错误说明"}
```

| 状态码 | 含义 |
|---|---|
| 400 | 参数解析/校验错误（如非法 IP/CIDR、Port ACL 与防护项目条目非法）。 |
| 401 | 未携带/携带错误的 Token（`/login` 页面会重定向）。 |
| 403 | 只读 Token 执行写操作；或命中封禁页。 |
| 413 | 请求体超过上限（`PAYLOAD_TOO_LARGE`）。 |
| 429 | 登录失败次数过多（`TOO_MANY_REQUESTS`）。 |
| 500 | 内部错误；`PATCH /api/config` 的参数校验失败也走 500（沿用现有实现，便于前端统一处理）。 |

---

## Hub API（v0.4.2）

Hub 独立运行在 `:9930`，所有接口都需要 `Authorization: Bearer <hub_token>`。

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/v1/policies` | GET | 拉取增量策略（`since`、`limit`） |
| `/api/v1/policies` | POST | 节点上报本地策略 |
| `/api/v1/policies` | DELETE | 撤回共享策略（生成 tombstone） |
| `/api/v1/policies/deleted` | GET | 拉取已删除策略（tombstone） |
| `/api/v1/rules` | GET | 获取当前规则包 |
| `/api/v1/rules` | POST | 统一下发规则包（ACL/L7/防护项目） |
| `/api/v1/nodes` | GET | 在线节点列表 |
| `/api/v1/nodes/heartbeat` | POST | 节点心跳 |
| `/api/v1/stats` | GET | 全局聚合统计 |

### POST /api/v1/policies

节点上报：

```json
{
  "node_name": "web-tier-01",
  "policies": [
    {
      "ip": { "family": 4, "addr": [0,0,0,0,0,0,0,0,0,0,0,0,10,0,0,2], "padding": [0,...] },
      "reason": 2,
      "hit_count": 150,
      "trust_score": 0,
      "blocked_until_ns": 1750819200000000000,
      "ttl_s": 300
    }
  ]
}
```

### GET /api/v1/policies?since=...

Hub 返回：

```json
{
  "policies": [
    {
      "ip": { "family": 4, "addr": [...], "padding": [...] },
      "reason": 2,
      "hit_count": 150,
      "trust_score": 0,
      "first_seen_ns": 1750819100000000000,
      "last_seen_ns": 1750819200000000000,
      "source_nodes": ["web-tier-01"],
      "ttl_s": 300
    }
  ],
  "cursor": "1750819200000000000",
  "deleted": [],
  "deleted_cursor": "0"
}
```

### POST /api/v1/rules

统一下发规则包：

```json
{
  "port_acl": [{"protocol": "tcp", "dport": "9999", "action": "drop"}],
  "l7_patterns": [{"pattern": "EVIL"}],
  "protection_projects": [
    {"name": "hub-test", "protocol": "tcp", "dport": "9999", "target_ips": ["10.0.0.1/32"], "enabled_modules": ["syn_flood"], "action": "defend"}
  ],
  "updated_at_ns": 1750819200000000000
}
```

## 上游编排 API（v0.5.2）

面向编排系统 / 上游应用的远程控制接口，补充在原有管理 API 之上。所有接口都需要
`Authorization: Bearer <token>`（本机回环请求免认证），返回 JSON。

### 认证模型

| 角色 | 说明 | 写操作 | Token 管理 |
|---|---|---|---|
| `admin` | 全部权限 | 是 | 是 |
| `operator` | 读写策略/封禁，适合编排器 | 是 | 否 |
| `readonly` | 只读，适合监控/大屏 | 否（403） | 否 |

配置示例（`/etc/xuanarmor/config.toml`）：

```toml
api_token = "admin-token-0123456789"        # 兼容旧配置：等价 admin 角色
readonly_token = "readonly-token-012345"    # 兼容旧配置：等价 readonly 角色

[[api_tokens]]
name = "orchestrator-prod"                  # 审计与日志中标识调用方
token = "orchestrator-token-0123456789abcdef"
role = "operator"
scopes = ["read", "write", "policy", "events"]

[[api_tokens]]
name = "prometheus"
token = "prometheus-token-01234567890123"
role = "readonly"
```

作用域可选：`read`、`write`、`policy`、`events`、`system`、`*`；留空时按角色取默认值。
运行时 Token 可通过 API 创建/吊销（重启后失效），配置文件中的 Token 只能改配置后重启。

### GET /api/version

返回版本、API schema、能力清单与运行时信息，供上游做能力探测：

```json
{
  "name": "XuanArmor",
  "version": "0.5.2",
  "api_schema": 1,
  "capabilities": ["policy.read", "policy.apply", "policy.dry_run", "config.validate",
                   "events.poll", "events.stream", "tokens.manage", "idempotency_key", "openapi"],
  "started_unix": 1762700000,
  "uptime_s": 3600,
  "runtime": {"interface": "eth0", "xdp_mode": "native", "xdp_attached": true}
}
```

### GET /api/openapi.json

OpenAPI 3 描述（含认证方式与已实现路径），可直接用于生成客户端 SDK。

### POST /api/config/validate

下发前校验配置，避免把错误配置推到线上：

```json
{"toml": "interface = \"eth0\"\napi_token = \"...\"\n"}
```

返回 `{"valid": true, "errors": []}`；也可传 `{"config": {...}}`（JSON 形式，字段同 TOML）。

### GET /api/policy

读取当前可编排策略与版本指纹 `etag`：

```json
{
  "policy": {
    "whitelist": ["10.0.0.0/8"],
    "blacklist": ["203.0.113.0/24"],
    "port_acl": [],
    "protection_projects": [],
    "l7_scan": {"patterns": []},
    "geoip": {...},
    "rate_limit": {...},
    "port_rate_limit": {...}
  },
  "etag": "sha256:...",
  "source": "config_file"
}
```

### POST /api/policy/apply

原子下发策略，支持部分字段覆盖、`dry_run` 预演与 `Idempotency-Key` 幂等重试：

```bash
curl -sS -X POST http://127.0.0.1:8720/api/policy/apply \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: deploy-2026-11-03-001' \
  -d '{
        "dry_run": false,
        "blacklist": ["203.0.113.9/32"],
        "whitelist": ["10.0.0.0/8"],
        "port_acl": [{"protocol": "tcp", "dport": 9999, "action": "drop"}]
      }'
```

响应包含 `applied`、`dry_run`、`etag_before`、`etag_after` 与 `changes`（各段的 added/removed）。
`dry_run: true` 只返回变更预览，不落数据面。同一 `Idempotency-Key`（10 分钟内）重放返回首次结果，
并带 `x-idempotent-replay: true`。

> 说明：该接口把策略应用到运行时；如需重启后保留，请同步写入配置文件并 `POST /api/config/reload`
> 或 `systemctl reload xuanarmor`。返回的 `note` 字段会提示这一点。

### GET /api/events?after_seq=N&limit=M

按单调序号升序返回最近的攻击/丢弃事件，`after_seq` 传上次响应的 `latest_seq` 可实现增量拉取；
`limit` 最大 500。响应含 `latest_seq`、`oldest_available_seq`（环形缓冲仅保留最近 1000 条）。

```json
{
  "events": [
    {"seq": 42, "timestamp_ns": 1762700123000000000, "src_ip": "203.0.113.9",
     "protocol": 6, "rule_id": 2, "rule_name": "RATE_LIMIT", "dst_port": 443}
  ],
  "count": 1,
  "after_seq": 41,
  "latest_seq": 42,
  "oldest_available_seq": 1,
  "has_more": false
}
```

### GET /api/events/stream?after_seq=N

SSE 实时推送，事件类型 `attack`，`data` 为 `{events:[...], latest_seq}`。
适合上游做实时告警/自动处置，无需轮询。

### Token 管理

| 方法 | 路径 | 角色 | 说明 |
|---|---|---|---|
| GET | `/api/tokens` | admin | 列出 Token 元数据（不含密钥） |
| POST | `/api/tokens` | admin | 创建运行时 Token，密钥只在响应中返回一次 |
| DELETE | `/api/tokens/{name}` | admin | 吊销运行时 Token（配置文件中的 Token 返回 404） |

```bash
curl -sS -X POST http://127.0.0.1:8720/api/tokens \
  -H 'Authorization: Bearer <admin-token>' -H 'Content-Type: application/json' \
  -d '{"name": "orchestrator-01", "role": "operator", "scopes": ["read", "write", "policy", "events"]}'
# → {"name":"orchestrator-01","role":"operator","token":"<一次性密钥>", ...}
```

### 错误约定

新增接口沿用统一错误结构 `{"error": "...", "code": "..."}`：

| HTTP | code | 场景 |
|---|---|---|
| 400 | `empty_policy` / `invalid_config` | 空策略或校验失败 |
| 401 | `unauthorized` | 缺少/错误 Token |
| 403 | `forbidden` | 只读角色写操作、非 admin 管理 Token |
| 404 | `token_not_found` | 吊销不存在的运行时 Token |
| 409 | `token_create_failed` | Token 名称重复 |
| 500 | `apply_failed` | 数据面应用失败 |

**兼容性**：`api_token` / `readonly_token` 旧配置继续有效；未配置任何 Token 时保持旧行为
（本机与外部均视为 admin，仅建议在完全可信的内网使用）。

## 告警通知 API

通知配置独立保存在数据目录的 `notify.json`（权限 0600），支持多种通道格式：

| 方法 | 路径 | 角色 | 说明 |
|---|---|---|---|
| GET | `/api/notify` | 任意（只读） | 读取通知配置（密钥以 `has_secret` 表示，不回传明文） |
| PUT | `/api/notify` | admin/operator | 保存通知配置；`secret` 传空表示保留原值 |
| POST | `/api/notify/test` | admin/operator | 向所有启用通道发送测试通知 |
| GET | `/api/notify/deliveries?limit=N` | 任意（只读） | 最近投递记录（最多 200 条） |

配置示例：

```json
{
  "enabled": true,
  "threshold_dps": 1000,
  "cooldown_s": 60,
  "channels": [
    {"name": "ops-webhook", "kind": "generic", "url": "https://example.com/hook", "secret": "", "enabled": true},
    {"name": "dingtalk", "kind": "dingtalk", "url": "https://oapi.dingtalk.com/robot/send?access_token=...", "enabled": false},
    {"name": "wecom", "kind": "wecom", "url": "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=...", "enabled": false},
    {"name": "feishu", "kind": "feishu", "url": "https://open.feishu.cn/open-apis/bot/v2/hook/...", "enabled": false},
    {"name": "slack", "kind": "slack", "url": "https://hooks.slack.com/services/...", "secret": "xoxb-...", "enabled": false}
  ]
}
```

- `kind` 支持 `generic` / `dingtalk` / `wecom` / `feishu` / `slack`，服务端按平台格式封装消息；
- DROP 速率超过 `threshold_dps` 时按 `cooldown_s` 冷却推送到所有启用通道；
- 控制台「系统设置 → 告警通知」提供同等配置、测试发送与投递记录查看。

### 运行时 Token 持久化

通过 `POST /api/tokens` 创建的运行时 Token 写入数据目录 `api_tokens.json`（0600），
服务重启后仍然有效；`DELETE /api/tokens/{name}` 吊销并同步更新文件。
配置文件中的 `api_token` / `readonly_token` / `[[api_tokens]]` 仍以配置文件为准。

