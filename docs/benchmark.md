# XuanArmor 基准测试

## 环境要求

- Linux 内核 >= 5.10，启用 BTF
- root 权限
- `hping3`

## 运行

```bash
# 先构建 release 二进制
cargo build --package xuanarmor --target x86_64-unknown-linux-musl --release

# 运行基准测试
sudo bash scripts/benchmark.sh
```

可通过环境变量调整：

```bash
PACKETS=500000 INTERVAL=u1 sudo -E bash scripts/benchmark.sh
```

- `PACKETS`：每个场景发送的包数（默认 200000）
- `INTERVAL`：hping3 发包间隔（默认 `u1`，约 1us）

## 测试场景

1. **Baseline**：netns 已建立，但无 XuanArmor 运行，测量原生 veth 转发/丢弃开销。
2. **XDP PASS**：XuanArmor 已挂载，但无任何规则触发 DROP，测量 XDP 程序自身开销。
3. **XDP DROP**：XuanArmor 将源 IP 加入黑名单，测量 XDP 早期 DROP 路径。

## 指标

- **pps**：每秒处理包数
- **time**：发送全部包耗时

## 示例报告

```text
=== XuanArmor XDP Benchmark ===
packets: 200000, interval: u1

--- Baseline (no XuanArmor) ---
  packets: 200000, time: 0.823s, pps: 243013

--- XDP PASS (no drop rules) ---
  packets: 200000, time: 0.891s, pps: 224467

--- XDP DROP (blacklist source) ---
  packets: 200000, time: 0.812s, pps: 246305
```

> 注意：veth 为虚拟设备，pps 受限于单核 CPU 与用户态/内核切换，物理网卡环境通常更高。

## veth 大包吞吐基准（内核 7.0 / 8 vCPU）

使用 `iperf3 -c <server> -t 5 -P 4` 在 netns + veth 环境测量 TCP 吞吐；
该环境只用于衡量 XDP 程序的相对开销，不代表物理网卡线速。

| 场景 | 吞吐 | 说明 |
|---|---:|---|
| Baseline（无 XDP） | 274.44 Gbps | veth 本机内存拷贝上限 |
| 最小 XDP_PASS 程序（原生模式） | 12.83 Gbps | veth 原生 XDP 自身瓶颈 |
| XuanArmor PASS（原生模式，空规则） | 11.85 Gbps | 相对最小程序约 -7.6% |
| 最小 XDP_PASS 程序（SKB 通用模式） | 124.62 Gbps | 通用模式在 veth 上更快 |
| XuanArmor PASS（SKB 通用模式，空规则） | 112.30 Gbps | 相对最小程序约 -9.9% |
| XuanArmor PASS（原生 + 全部模块启用但不触发 DROP） | 11.81 Gbps | 相对空规则约 -0.3% |

结论：

- 本基准中 **veth 原生 XDP 本身是主要瓶颈**，XuanArmor 程序相对最小 XDP_PASS
  只增加约 7%–10% 开销；
- 大包 TCP 场景下，开启速率限制 / Trust / 连接跟踪等模块后额外开销很小；
- 小包 / 高 pps 场景需要真实物理网卡 + `xdp-bench`/`pktgen` 复测，不能直接
  用本表推算线速。

## 高频写路径深度优化（v0.4.7）

针对持续攻击期的 map 写放大，数据面加入采样与热路径裁剪：

- `TRUST_MAP` PASS 更新：1/64 采样，按 64 倍权重补偿；
- `TRUST_MAP` DROP 更新：信任分已归零的源 1/16 采样写入；
- `BLACKLIST` 命中 `hit_count`：1/16 采样写入，单次补 16；
- `TOP_ATTACKERS` 热榜：1/16 采样写入，单次补 16；
- 黑名单命中的 DROP 不再写 RingBuf 事件（计数已由全局统计与 hit_count 覆盖）。

优化后 netns 全部 12 项集成测试、57 项单元测试、eBPF/userspace clippy 均通过，
内核 7.0 verifier 仍可正常加载。

## 云内网极限测试（v0.4.6，2026-10）

本节汇总 v0.4.6 在云内网三节点环境的实测结果：attack1 / attack2 各 8 vCPU / 32GB，
defense 128 vCPU / 257GB、eth0 为 virtio_net 32 队列、native XDP（DRV 模式）。
完整方法、口径与限制见 [test-report-2026-10.md](test-report-2026-10.md)。

| 场景 | 实测结果 |
|---|---|
| 单机全 PASS（20M UDP） | total_packets 20,000,037，全部 PASS，无 DROP |
| 单机全 DROP（20M UDP + 静态黑名单） | total_dropped 20,000,000，blacklist_blocked 20,000,000 |
| 双机同时 PASS（各 20M） | 处理 39,271,100 / 40,000,000，聚合约 3.14M pps |
| 双机混合 DROP + PASS（各 20M） | 聚合约 3.08M pps；DROP 侧 19,996,176 全部命中 blacklist_blocked |
| SYN Flood（trafgen 20M SYN） | syn_flood_blocked 15,767,577，total_passed 217 |
| UDP Flood（pktgen 20M） | total_dropped 19,998,743，首窗口后由动态黑名单承担 DROP |
| ICMP Flood（8 路 hping3，8s） | icmp_dropped 12,794,723，total_passed 1,645 |
| conn_track（8 路 hping3 SYN，8s） | conn_track_blocked 11,779,346，total_passed 198 |
| L7 扫描（pattern="GET /"） | 负向 POST 不误杀；正向 GET 全部命中 DROP |
| L7 吞吐（hping3 交替 payload，8s） | l7_blocked 6,399,046，约处理数一半；未命中由测试工具 payload 导致 |

结论：

- v0.4.6 在 virtio_net + native XDP（DRV 模式）下，单机可稳定处理约 1.5–2M pps，
  双机聚合约 3.1M pps；全量 DROP/PASS、SYN/UDP/ICMP Flood、conn_track 与 L7
  扫描均按预期生效。
- defense 为云主机 virtio_net，吞吐受 vNIC 与内网链路限制，不能据此推算物理网卡
  线速；发送侧约 0.01% 的云内网丢包使绝对 pps 为下限值。
- 测试中复现的 4 个问题（热加载瞬断、L7/端口 ACL 的 API 上限与 map 容量不一致、
  Flood 模块计数被黑名单短路掩盖、DELETE 黑名单非幂等）记录在完整报告，建议在
  v0.4.7 前评估。

## generic XDP 组合攻击与调优实测（v0.5.3，2026-11）

本节记录 generic（SKB 模式）下的多机多向量组合攻击实测，以及 `xuanarmor tune`
的调优前后对比。与上一节 native 数据不可直接比较：**generic 在 `sk_buff` 分配之后
执行，单核软中断是瓶颈**，绝对 pps 显著低于 native。

### 环境

| 角色 | 公网 IP | 内网 IP | 规格 | 网络 |
|---|---|---|---|---|
| defense | 47.239.18.203 | 172.23.83.40 | 2 vCPU / 3.6 GB | virtio_net，**1 RX 队列**，`rx-gro-hw: on [fixed]` → **generic** |
| attack1 | 47.86.40.236 | 172.23.83.41 | 2 vCPU | 内网直连 |
| attack2 | 47.86.41.231 | 172.23.83.42 | 2 vCPU | 内网直连 |
| attack3 | 47.238.237.28 | 172.23.83.43 | 2 vCPU | 内网直连 |
| attack4 | 47.76.58.124 | 172.23.83.44 | 2 vCPU | 内网直连 |

- 版本：XuanArmor v0.5.3，`[xdp].mode = "auto"`（回退 generic）
- 攻击工具：hping3（SYN / UDP / ICMP / 自定义 payload）、内置 HTTP 服务
- 统计口径：`/api/stats` 前后增量；每轮前清空动态黑名单，避免上一轮封禁掩盖本轮模块计数
- 攻击目标为**内网 IP**（公网 IP 在云内不可达）

### 组合攻击结果

| 场景 | total_packets | dropped | passed | 主要模块计数 |
|---|---:|---:|---:|---|
| SYN Flood（4 机 × 10s） | 546,090 | 174,681 | 12,990 | `syn_flood_blocked=358,419` |
| UDP Flood（4 机 × 10s） | 849,640 | 838,154 | 11,486 | `udp_flood_blocked=4`、`port_rate_blocked=169` |
| ICMP Flood（4 机 × 10s） | 2,542,342 | 2,533,976 | 8,366 | `icmp_flood_blocked=4` |
| 混合 SYN+UDP+ICMP（4 机分工 × 12s） | 954,756 | 786,751 | 10,173 | `syn=157,832`、`tcp_dropped=178,958`、`udp_dropped=329,759`、`icmp_dropped=278,034` |
| 混合 + 正常 HTTP 并发 | 919,478 | 692,239 | 11,122 | `syn=216,117`、`tcp=211,211`、`udp=231,500`、`icmp=249,528` |
| conn_track 半连接（4 机 × 10s） | 535,817 | 535,618 | 199 | `conn_track_blocked=71`，首窗口后转 `blacklist_blocked=535,547` |
| 大包 UDP 1400B（4 机 × 10s） | 778,500 | 767,735 | 10,765 | `udp_flood_blocked=4`、`blacklist_blocked=767,730` |
| 小包 SYN 64B（4 机 × 10s） | 362,843 | 362,694 | 149 | `conn_track_blocked=362,692`、`l7_blocked=2` |
| 全协议混合（TCP+UDP+ICMP+大包，4 机 × 15s） | 1,244,410 | 1,240,257 | 4,153 | `conn_track_blocked=397,552`、`blacklist_blocked=842,648` |

累计：20,461,657 包，丢弃 19,108,524（93.4%），通过 163,398。

### 攻击后可用性

| 检查项 | 结果 |
|---|---|
| HTTP :80 | 200 |
| `/ready` | 200 |
| 服务状态 | active |
| XDP 挂载 | `xdpgeneric`（未掉挂） |

### L7 隔离验证

组合场景中 `l7_blocked` 为 0，原因是**黑名单检查在 L7 之前**：攻击机在首窗口即被
Flood 模块拉黑，后续包走黑名单快路径，不再进入 L7 扫描。单独关闭其它模块、只保留
L7 后重测：

| 阶段 | `l7_blocked` |
|---|---:|
| 隔离前 | 25 |
| 发 8s `GET /` payload 后 | 966（+941） |

结论：L7 指纹匹配正常工作，组合场景的 0 计数是模块顺序导致的测量现象，不是漏判。

### `xuanarmor tune` 调优前后对比

同机同负载（4 机 UDP Flood，15s × 2 轮取平均）：

| 阶段 | dropped pps |
|---|---:|
| 调优前（默认内核参数） | 63,560 / 89,323 → **平均 76,442** |
| 调优后（RPS + RFS + NAPI 预算） | 88,499 / 86,424 → **平均 87,462** |
| 变化 | **+14.4%** |

调优项（`xuanarmor tune` 自动按 generic 策略选择）：

| 项 | 值 |
|---|---|
| `rps_cpus`（rx-0） | `3`（2 核全参与） |
| `rps_flow_cnt` | 32768 |
| `net.core.rps_sock_flow_entries` | 32768 |
| `net.core.netdev_budget` | 300 → 600 |
| `net.core.netdev_budget_usecs` | 2000 → 4000 |
| `net.core.netdev_max_backlog` | 1000 → 10000 |

测试后已 `xuanarmor tune --revert` 还原（`tune-state.json` 删除、sysctl 复位）。

### 评价

**符合预期的部分**

- 多机多向量组合攻击下，SYN / UDP / ICMP Flood、conn_track、L7、端口聚合限速、
  动态黑名单按设计联动，没有出现模块互相干扰或统计错乱；
- 攻击全程服务可用（HTTP 200、`/ready` 200）、XDP 未掉挂，说明 generic 路径的
  稳定性没有问题；
- 单队列 generic 在 2 vCPU 上可稳定 DROP 约 **7.6–8.7 万 pps**，与
  [capacity-planning.md](capacity-planning.md) 的 generic 单包成本（75–92 ns/包）自洽。

**需要注意的部分**

- **绝对 pps 远低于 native**：native 基准（128 vCPU / 32 队列）为 1.5–2M pps，
  本节 generic 仅约 8 万 pps。差距来自三方面：单队列、2 vCPU、generic 路径本身，
  不能把本节数据当作 XuanArmor 的性能上限；
- **黑名单短路会掩盖模块计数**：极限流量下 `udp_flood_blocked` / `icmp_flood_blocked`
  只有个位数，DROP 量体现在 `blacklist_blocked`。评估模块是否生效必须看隔离场景，
  或结合 `blacklist_blocked` 一起看；
- **L7 在组合场景中不参与**：黑名单前置是性能优化（快路径），但也意味着被拉黑的源
  不会再触发 L7 判定。这是设计取舍，不是缺陷；
- **调优收益有限（+14.4%）**：单队列 + 2 vCPU 的机器上 RPS 只能分摊到 2 个核，
  收益上限本来就低。多队列机器（4/8 队列）上 RPS 的收益会明显更大，但本轮未测。

**建议**

- 性能承诺只引用 native 数据；generic 数据仅用于"功能可用 + 稳定性"结论；
- 需要 generic 高 PPS 时，优先选**多队列**实例，再叠加 `xuanarmor tune`；
- 评估防护效果时同时看 `total_dropped`、`blacklist_blocked` 与各模块计数，
  避免被黑名单短路误导。
