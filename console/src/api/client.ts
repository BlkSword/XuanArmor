import * as mock from './mock'
import type {
  AttackEvent, AttackEventPage, AuditEntry, AuditVerifyReport, BlockEntry, ConfigSnapshot, L7Pattern,
  MapUsage, ModuleState, PacketPage, PacketSample, PageResult, PortAclEntry, ProtectionProject, Role,
  SeriesPoint, Stats, WhitelistEntry,
} from './types'
import { reasonLabel } from '../utils/format'

const params = new URLSearchParams(location.search)
const initialMode = params.get('mock') === '1'
  ? 'mock'
  : params.get('live') === '1'
    ? 'live'
    : location.protocol.startsWith('http') ? 'auto' : 'mock'

export const state = {
  mode: initialMode as 'auto' | 'mock' | 'live',
  live: false,
  // 令牌只保留在内存中：浏览器侧认证依赖后端下发的 HttpOnly Cookie。
  // 旧实现把同一令牌写进 localStorage，会让 HttpOnly 形同虚设（XSS 可直接读取）。
  token: '',
}

export function getToken() { return state.token }
export const demoMode = () => state.mode === 'mock'
export function setToken(t: string) { state.token = t }

// ---------------------------------------------------------------- mock state
let mockSeries = mock.makeSeries()
let mockEvents = mock.makeAttackEvents()
let mockPackets = mock.makePacketSamples()
let mockAudit = mock.makeAudit()
let mockBlocks = mock.makeBlacklist()
let mockWhitelist = [...mock.whitelist]
let mockAcl = [...mock.portAcl]
let mockL7 = [...mock.l7Patterns]
let mockProjects = [...mock.projects]
let mockConfig = mock.makeConfig()
let mockModules = mock.makeModules(mock.makeStats())

// -------------------------------------------------------------- normalizers
const PROTO_NAMES: Record<number, string> = { 1: 'ICMP', 6: 'TCP', 17: 'UDP', 58: 'ICMP' }
// 与 xuanarmor-common::rules 的 rule_id 对齐；v0.5.0 新增 14/15。
const RULE_NAMES: Record<number, string> = {
  1: '黑名单', 2: '速率限制', 3: 'SYN Flood', 4: 'L7 指纹', 5: '自适应', 6: '手动封禁',
  7: '端口 ACL', 8: 'UDP Flood', 9: 'ICMP Flood', 10: 'GeoIP', 11: '威胁情报', 12: '防护项目',
  13: '连接跟踪', 14: '分片策略', 15: '端口聚合限速',
}

/** rule_name 缺失或为「未知」时按 rule_id 兜底，保证新规则在表格里可读。 */
function normalizeRuleName(raw: any): string {
  const id = Number(raw?.rule_id)
  const byId = Number.isFinite(id) ? RULE_NAMES[id] : undefined
  const name = typeof raw?.rule_name === 'string' ? raw.rule_name.trim() : ''
  if (name && name !== '未知' && name !== 'UNKNOWN') return name
  return byId || name || '未知'
}

function protoName(v: number | string | undefined): string {
  if (typeof v === 'string') return v.toUpperCase()
  return PROTO_NAMES[v ?? 0] ?? 'OTHER'
}
function actionName(v: number | string | undefined): string {
  if (typeof v === 'string') return v
  return v === 1 ? 'DROP' : v === 2 ? 'PASS' : v === 3 ? 'TX' : 'DROP'
}

function modulePatch(id: string, enabled: boolean): Record<string, unknown> {
  switch (id) {
    case 'syn_flood': return { syn_proxy_enabled: enabled }
    case 'udp_flood': return { udp_flood_enabled: enabled }
    case 'icmp_flood': return { icmp_flood_enabled: enabled }
    case 'l7_scan': return { l7_scan_enabled: enabled }
    case 'geoip': return { geoip_enabled: enabled }
    case 'conn_track': return { conn_track_enabled: enabled }
    case 'tcp_reset': return { tcp_reset_on_drop: enabled }
    case 'trust_score': return { trust_enabled: enabled }
    case 'rate_limit': return { rate_limit_enabled: enabled }
    case 'port_rate_limit':
      return { port_rate_limit: { enabled } }
    case 'adaptive':
      return { adaptive: { enabled } }
    default: return {}
  }
}

function normalizeModule(m: any): ModuleState {
  return {
    id: String(m.id),
    name: String(m.name ?? m.id),
    group: String(m.category ?? m.group ?? '其他'),
    description: String(m.description ?? ''),
    enabled: !!m.enabled,
    statsKey: m.stats_key || m.statsKey || undefined,
    fields: (m.editable_fields || m.fields || [])
      .filter((f: any) => f && f.type !== 'readonly')
      .map((f: any) => ({
        key: String(f.id ?? f.key),
        label: String(f.label ?? f.id ?? f.key),
        value: f.value,
        type: f.type === 'switch' ? 'switch' : 'number',
      })),
    patch: modulePatch(String(m.id), !!m.enabled),
  }
}

function normalizeSeries(raw: any): SeriesPoint[] {
  const list = Array.isArray(raw) ? raw : (raw?.series || [])
  return list.map((p: any) => {
    const ns: number | undefined = p.timestamp_ns
    const ms = ns ? Math.round(ns / 1e6) : Number(p.timestamp ?? 0) * 1000
    const dropped = Number(p.dps ?? p.dropped ?? 0)
    const pps = Number(p.pps ?? 0)
    return { t: ms, pps, dps: dropped, passed: Math.max(0, pps - dropped), dropped }
  })
}

function normalizeEvents(raw: any): AttackEvent[] {
  const list = Array.isArray(raw) ? raw : (raw?.events || [])
  return list.map((e: any, i: number) => {
    const rule = normalizeRuleName(e)
    return {
      // v0.5.4：后端提供窗口内全局序号，作为跨页稳定的列表 key
      id: e.seq != null ? `evt-${e.seq}` : (e.id || `evt-${e.timestamp_ns ?? i}-${i}`),
      seq: typeof e.seq === 'number' ? e.seq : undefined,
      timestamp_ns: Number(e.timestamp_ns ?? Date.now() * 1e6),
      src_ip: String(e.src_ip ?? '—'),
      dst_ip: e.dst_ip != null ? String(e.dst_ip) : undefined,
      src_port: typeof e.src_port === 'number' ? e.src_port : undefined,
      dst_port: Number(e.dst_port ?? 0),
      protocol: protoName(e.protocol),
      action: actionName(e.action),
      rule,
      reason: e.reason ? reasonLabel(e.reason) : rule,
      length: typeof e.length === 'number' ? e.length : undefined,
      interface: e.interface ? String(e.interface) : undefined,
    }
  })
}

function normalizePackets(raw: any): PacketSample[] {
  const list = Array.isArray(raw) ? raw : (raw?.entries || raw?.packets || [])
  return list.map((e: any, i: number) => ({
    id: e.id || `pkt-${e.timestamp_ns ?? i}-${i}`,
    timestamp_ns: Number(e.timestamp_ns ?? Date.now() * 1e6),
    src_ip: String(e.src_ip ?? '—'),
    dst_ip: String(e.dst_ip ?? '—'),
    src_port: Number(e.src_port ?? 0),
    dst_port: Number(e.dst_port ?? 0),
    protocol: protoName(e.protocol),
    action: actionName(e.action),
    length: Number(e.packet_len ?? e.length ?? 0),
    tcp_flags: e.tcp_flags,
    payload_preview: e.payload_hex ? e.payload_hex.slice(0, 48) : undefined,
  }))
}

const AUDIT_LABELS: Record<string, string> = {
  BlockIp: '封禁 IP', UnblockIp: '解除封禁', AllowCidr: '添加白名单', DisallowCidr: '移除白名单',
  ReloadConfig: '重载配置', PatchConfig: '修改配置', Start: '启动', Stop: '停止',
  Login: '登录', ResetToken: '重置令牌',
}

/** 后端审计条目 → 控制台展示结构；导出供审计 SSE 实时消息复用（v0.5.4）。 */
export function normalizeAudit(raw: any): AuditEntry[] {
  const list = Array.isArray(raw) ? raw : (raw?.entries || [])
  return list.map((e: any, i: number) => {
    const detail = e.detail ?? {}
    const target = detail.ip || detail.cidr || (detail.count ? `${detail.count} 条` : '—')
    return {
      id: e.id || `aud-${i}`,
      timestamp_ns: e.timestamp_ns ?? (Date.parse(e.timestamp) * 1e6 || Date.now() * 1e6),
      actor: String(e.actor ?? 'system'),
      action: AUDIT_LABELS[e.action] || String(e.action ?? ''),
      target: String(target),
      detail: typeof detail === 'string' ? detail : JSON.stringify(detail ?? {}),
      source_ip: String(e.source_ip ?? '—'),
    }
  })
}

function normalizeBlocks(raw: any): BlockEntry[] {
  const list = Array.isArray(raw) ? raw : (raw?.entries || raw?.blacklist || [])
  return list.map((b: any) => ({
    ip: String(b.ip ?? ''),
    reason: String(b.reason ?? ''),
    last_reason: String(b.last_reason ?? ''),
    origin: String(b.origin ?? ''),
    created_ns: Number(b.created_ns ?? Date.now() * 1e6),
    expires_ns: Number(b.expires_ns ?? 0),
    hits: Number(b.hits ?? b.hit_count ?? 0),
  }))
}

function normalizeWhitelist(raw: any): WhitelistEntry[] {
  const list = Array.isArray(raw) ? raw : (raw?.entries || raw?.whitelist || [])
  return list.map((w: any) => ({ cidr: String(w.cidr ?? w), note: String(w.note ?? '') }))
}

// ---------------------------------------------------------------- transport
/** 统一解析响应：JSON 正常解析；纯文本/空响应不再抛 SyntaxError，而是包装成 {ok,message}。 */
async function raw<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (state.token) headers.Authorization = `Bearer ${state.token}`
  const res = await fetch(path, {
    credentials: 'same-origin',
    ...init,
    headers: { ...headers, ...(init?.headers || {}) },
  })
  if (!res.ok) {
    // 401/403 单独标记，便于页面区分"未登录"与"服务异常"。
    if (res.status === 401) throw new Error('unauthorized')
    if (res.status === 403) throw new Error('forbidden')
    const text = await res.text().catch(() => '')
    throw new Error(`HTTP ${res.status}${text ? ` ${text.slice(0, 120)}` : ''}`)
  }
  const text = await res.text()
  if (!text) return null as T
  const contentType = res.headers.get('content-type') || ''
  if (!contentType.includes('json')) {
    return { ok: true, message: text } as unknown as T
  }
  try {
    return JSON.parse(text) as T
  } catch {
    return { ok: true, message: text } as unknown as T
  }
}

/** 读接口：live/auto 模式失败时返回 null（页面显示空状态），不再静默回退演示数据。 */
async function fetchOrNull<T>(path: string): Promise<T | null> {
  if (state.mode === 'mock') return null
  try {
    const value = await raw<T>(path)
    state.live = true
    return value
  } catch (err) {
    if (state.mode === 'live') throw err
    state.live = false
    return null
  }
}

/** 写接口：失败必须抛出，避免"看起来成功实际没生效"。 */
async function mutate(path: string, body: unknown, method = 'POST'): Promise<void> {
  if (state.mode === 'mock') return
  await raw(path, { method, body: JSON.stringify(body) })
  state.live = true
}

function emptyStats(): Stats {
  return {
    total_packets: 0, total_passed: 0, total_dropped: 0,
    current_pps: 0, current_dps: 0, danger_level: 0,
    blacklist_blocked: 0, rate_limited: 0, syn_flood_blocked: 0,
    udp_flood_blocked: 0, icmp_flood_blocked: 0, l7_blocked: 0,
    geoip_blocked: 0, conn_track_blocked: 0, adaptive_blocked: 0,
    trust_trusted: 0, trust_neutral: 0, trust_suspicious: 0, trust_malicious: 0,
    xdp_attached: false, xdp_mode: 'unknown', uptime_s: 0,
    port_rate_blocked: 0, fragment_blocked: 0,
  } as unknown as Stats
}

/** 服务端分页查询选项（v0.5.4）。 */
export interface AttackEventsOptions {
  limit?: number
  offset?: number
  /** 匹配源 IP / 命中规则 */
  q?: string
  /** 协议号：6=TCP / 17=UDP / 1=ICMP */
  protocol?: number
}

export interface PacketsOptions {
  limit?: number
  offset?: number
  /** 时间游标（wall-clock 纳秒）：只返回早于该值的样本，避免环形缓冲写入导致 offset 翻页重复 */
  before_ns?: number
  /** 匹配源/目的 IP */
  ip?: string
  /** 1=DROP / 2=PASS */
  action?: number
}

export interface AuditOptions {
  limit?: number
  offset?: number
  /** 全文搜索（动作/操作者/来源 IP/详情） */
  filter?: string
  /** 操作者精确匹配（all 表示不过滤） */
  actor?: string
}

export interface BlacklistOptions {
  limit?: number
  offset?: number
  /** IP 子串 */
  q?: string
  /** 封禁原因中文标签 */
  reason?: string
  /** 来源标签 */
  origin?: string
}

/** mock 模式下模拟服务端分页（与后端返回结构保持一致）。 */
function mockPage<T>(list: T[], offset: number, limit: number): PageResult<T> {
  return { items: list.slice(offset, offset + limit), total: list.length, limit, offset }
}

export const api = {
  isLive: () => state.live,

  async authCheck(): Promise<Role | null> {
    if (state.mode === 'mock') return 'admin'
    try {
      const headers: Record<string, string> = {}
      if (state.token) headers.Authorization = `Bearer ${state.token}`
      const res = await fetch('/api/auth/check', { credentials: 'same-origin', headers })
      if (!res.ok) return null
      const body = await res.json().catch(() => null)
      return body?.role === 'readonly' ? 'readonly' : 'admin'
    } catch (err) {
      if (state.mode === 'live') throw err
      state.live = false
      return null
    }
  },

  async stats(): Promise<Stats> {
    if (state.mode === 'mock') return mock.makeStats()
    const r = await fetchOrNull<Stats>('/api/stats')
    return r ?? emptyStats()
  },
  async mapUsage(): Promise<MapUsage> {
    if (state.mode === 'mock') return mock.makeMapUsage()
    const r = await fetchOrNull<any>('/api/map-usage')
    if (!r) return { maps: [], warn_threshold: 0.8, sampled_ns: 0, sampled_cap: 0 }
    const maps: any[] = Array.isArray(r.maps) ? r.maps : []
    return {
      maps: maps.map((m: any) => {
        const max = Number(m.max ?? m.max_entries ?? 0)
        const entries = Number(m.entries ?? m.used ?? 0)
        const ratio = Number(m.ratio ?? (max > 0 ? entries / max : 0))
        return {
          name: String(m.name ?? ''),
          type: String(m.type ?? m.map_type ?? ''),
          entries: Number.isFinite(entries) ? entries : 0,
          max: Number.isFinite(max) ? max : 0,
          ratio: Number.isFinite(ratio) ? ratio : 0,
        }
      }),
      warn_threshold: Number(r.warn_threshold ?? 0.8),
      sampled_cap: Number(r.sampled_cap ?? 0),
      sampled_ns: Number(r.sampled_ns ?? 0),
    }
  },
  async auditVerify(): Promise<AuditVerifyReport> {
    if (state.mode === 'mock') return mock.makeAuditVerify()
    const r = await fetchOrNull<any>('/api/audit/verify')
    if (!r) return { ok: false, enabled: false, checked: 0, first_bad_seq: null }
    return {
      ok: !!r.ok,
      enabled: r.enabled !== false,
      message: r.message ? String(r.message) : undefined,
      checked: Number(r.checked ?? 0),
      first_bad_seq: r.first_bad_seq === null || r.first_bad_seq === undefined ? null : Number(r.first_bad_seq),
      path: r.path ? String(r.path) : undefined,
    }
  },
  /** 流量曲线：duration_s 由时间范围选择器决定，图表随范围切换而变化。 */
  async series(durationSeconds = 3600): Promise<SeriesPoint[]> {
    if (state.mode === 'mock') {
      mockSeries = mock.makeSeries(Math.max(24, Math.min(120, Math.round(durationSeconds / 60))))
      return mockSeries
    }
    const r = await fetchOrNull<any>(`/api/metrics/series?duration_s=${Math.max(60, Math.floor(durationSeconds))}`)
    return r ? normalizeSeries(r) : []
  },
  /**
   * 攻击事件（v0.5.4 服务端分页）。
   *
   * 服务端只保留最近 1000 条环形缓冲；`total` 是窗口内过滤后的条数，
   * `truncated` 表示除当前页外窗口内仍有数据。
   */
  async attackEvents(opts: AttackEventsOptions = {}): Promise<AttackEventPage> {
    const limit = Math.min(Math.max(opts.limit ?? 200, 1), 200)
    const offset = Math.max(opts.offset ?? 0, 0)
    const q = (opts.q || '').trim()
    if (state.mode === 'mock') {
      const filtered = mock.makeAttackEvents().filter((r) => {
        if (opts.protocol != null && r.protocol.toUpperCase() !== (PROTO_NAMES[opts.protocol] || '')) return false
        if (q && !(r.src_ip + r.rule + r.reason).toLowerCase().includes(q.toLowerCase())) return false
        return true
      })
      const page = mockPage(filtered, offset, limit)
      return { items: page.items, total: page.total, window: 1000, truncated: page.total > offset + page.items.length }
    }
    const params = new URLSearchParams()
    params.set('limit', String(limit))
    if (offset) params.set('offset', String(offset))
    if (q) params.set('q', q)
    if (opts.protocol != null) params.set('protocol', String(opts.protocol))
    const r = await fetchOrNull<any>(`/api/attack-events?${params.toString()}`)
    if (!r) return { items: [], total: 0, window: 1000, truncated: false }
    return {
      items: normalizeEvents(r),
      total: Number(r.total ?? r.count ?? 0),
      window: Number(r.window ?? 1000),
      truncated: !!r.truncated,
    }
  },
  async packets(opts: PacketsOptions = {}): Promise<PacketPage> {
    const limit = Math.min(Math.max(opts.limit ?? 100, 1), 1000)
    const offset = Math.max(opts.offset ?? 0, 0)
    if (state.mode === 'mock') {
      const ip = (opts.ip || '').trim().toLowerCase()
      const filtered = mock.makePacketSamples().filter((r) => {
        if (opts.action != null && (r.action === 'DROP' ? 1 : 2) !== opts.action) return false
        if (ip && !(r.src_ip + r.dst_ip).toLowerCase().includes(ip)) return false
        return true
      })
      const page = mockPage(filtered, offset, limit)
      return { items: page.items, total: page.total, usage: { entries: page.total, bytes: 0 } }
    }
    const params = new URLSearchParams()
    params.set('limit', String(limit))
    if (offset) params.set('offset', String(offset))
    if (opts.before_ns) params.set('before_ns', String(opts.before_ns))
    if (opts.ip) params.set('ip', opts.ip)
    if (opts.action != null) params.set('action', String(opts.action))
    const r = await fetchOrNull<any>(`/api/packets?${params.toString()}`)
    if (!r) return { items: [], total: 0, usage: { entries: 0, bytes: 0 } }
    return {
      items: normalizePackets(r),
      total: Number(r.total ?? r.count ?? 0),
      usage: {
        entries: Number(r.usage?.entries ?? 0),
        bytes: Number(r.usage?.bytes ?? 0),
      },
    }
  },
  async audit(opts: AuditOptions = {}): Promise<PageResult<AuditEntry>> {
    const limit = Math.min(Math.max(opts.limit ?? 100, 1), 1000)
    const offset = Math.max(opts.offset ?? 0, 0)
    const filter = (opts.filter || '').trim().toLowerCase()
    const actor = opts.actor && opts.actor !== 'all' ? opts.actor : ''
    if (state.mode === 'mock') {
      const filtered = mock.makeAudit().filter((r) => {
        if (actor && r.actor !== actor) return false
        if (filter && !(r.action + r.target + r.detail + r.source_ip).toLowerCase().includes(filter)) return false
        return true
      })
      return mockPage(filtered, offset, limit)
    }
    const params = new URLSearchParams()
    params.set('limit', String(limit))
    if (offset) params.set('offset', String(offset))
    if (filter) params.set('filter', filter)
    if (actor) params.set('actor', actor)
    const r = await fetchOrNull<any>(`/api/audit?${params.toString()}`)
    if (!r) return { items: [], total: 0, limit, offset }
    return {
      items: normalizeAudit(r),
      total: Number(r.total ?? r.count ?? 0),
      limit,
      offset,
    }
  },
  async blacklist(opts: BlacklistOptions = {}): Promise<PageResult<BlockEntry>> {
    const limit = Math.min(Math.max(opts.limit ?? 200, 1), 2000)
    const offset = Math.max(opts.offset ?? 0, 0)
    const q = (opts.q || '').trim().toLowerCase()
    if (state.mode === 'mock') {
      const filtered = mock.makeBlacklist().filter((b) => {
        if (opts.reason && opts.reason !== 'all' && b.reason !== opts.reason) return false
        if (opts.origin && opts.origin !== 'all' && b.origin !== opts.origin) return false
        if (q && !(b.ip + b.reason + (b.last_reason || '') + b.origin).toLowerCase().includes(q)) return false
        return true
      })
      return mockPage(filtered, offset, limit)
    }
    const params = new URLSearchParams()
    params.set('limit', String(limit))
    if (offset) params.set('offset', String(offset))
    if (q) params.set('q', q)
    if (opts.reason && opts.reason !== 'all') params.set('reason', opts.reason)
    if (opts.origin && opts.origin !== 'all') params.set('origin', opts.origin)
    const r = await fetchOrNull<any>(`/api/blacklist?${params.toString()}`)
    if (!r) return { items: [], total: 0, limit, offset }
    return {
      items: normalizeBlocks(r),
      total: Number(r.total ?? r.count ?? 0),
      limit,
      offset,
    }
  },
  async whitelist(): Promise<WhitelistEntry[]> {
    if (state.mode === 'mock') return mock.whitelist.map((w) => ({ ...w }))
    const r = await fetchOrNull<any>('/api/whitelist')
    return r ? normalizeWhitelist(r) : []
  },
  async portAcl(): Promise<PortAclEntry[]> {
    if (state.mode === 'mock') return mock.portAcl.map((a) => ({ ...a }))
    const r = await fetchOrNull<any>('/api/port-acl')
    const list = r ? (r.items || r) : []
    return list.map((a: any) => ({ protocol: String(a.protocol), dport: String(a.dport), action: String(a.action) }))
  },
  async l7Patterns(): Promise<L7Pattern[]> {
    if (state.mode === 'mock') return mock.l7Patterns.map((p) => ({ ...p }))
    const r = await fetchOrNull<any>('/api/l7-patterns')
    const list = r ? (r.patterns || r) : []
    return list.map((p: any) => ({ pattern: String(p.pattern ?? ''), mask: p.mask || undefined }))
  },
  async projects(): Promise<ProtectionProject[]> {
    if (state.mode === 'mock') return mock.projects.map((p) => ({ ...p }))
    const r = await fetchOrNull<any>('/api/protection-projects')
    const list = r ? (r.projects || r) : []
    return list.map((p: any) => ({
      name: String(p.name ?? ''), description: String(p.description ?? ''),
      protocol: String(p.protocol ?? 'tcp'), dport: String(p.dport ?? ''),
      target_ips: (p.target_ips || []).map(String),
      enabled_modules: (p.enabled_modules || []).map(String),
      action: (p.action || 'defend') as ProtectionProject['action'],
      enabled: p.enabled ?? true,
    }))
  },
  async config(): Promise<ConfigSnapshot> {
    if (state.mode === 'mock') return mock.makeConfig()
    const r = await fetchOrNull<any>('/api/config')
    return (r ?? {}) as ConfigSnapshot
  },
  async modules(stats: Stats): Promise<ModuleState[]> {
    if (state.mode === 'mock') return mock.makeModules(stats)
    const r = await fetchOrNull<any>('/api/protection-modules')
    if (!r) return []
    const list = r.modules || r
    return list.map(normalizeModule)
  },

  async block(ip: string, duration_s = 0) {
    if (state.mode === 'mock') {
      mockBlocks = [{ ip, reason: '手动封禁', origin: 'api', created_ns: Date.now() * 1e6, expires_ns: duration_s ? (Date.now() + duration_s * 1000) * 1e6 : 0, hits: 0 }, ...mockBlocks]
      return { ok: true }
    }
    await raw('/api/blacklist', { method: 'POST', body: JSON.stringify({ ip, duration_s }) })
    return { ok: true }
  },
  async unblock(ip: string) {
    if (state.mode === 'mock') {
      mockBlocks = mockBlocks.filter((b) => b.ip !== ip)
      return { ok: true }
    }
    await raw('/api/blacklist', { method: 'DELETE', body: JSON.stringify({ ip }) })
    return { ok: true }
  },
  async addWhitelist(cidr: string) {
    if (state.mode === 'mock') {
      mockWhitelist = [...mockWhitelist, { cidr, note: '手动添加' }]
      return { ok: true }
    }
    await raw('/api/whitelist', { method: 'POST', body: JSON.stringify({ cidr }) })
    return { ok: true }
  },
  async delWhitelist(cidr: string) {
    if (state.mode === 'mock') {
      mockWhitelist = mockWhitelist.filter((w) => w.cidr !== cidr)
      return { ok: true }
    }
    await raw('/api/whitelist', { method: 'DELETE', body: JSON.stringify({ cidr }) })
    return { ok: true }
  },
  async savePortAcl(items: PortAclEntry[]) {
    if (state.mode === 'mock') { mockAcl = items; return { ok: true } }
    await raw('/api/port-acl', { method: 'POST', body: JSON.stringify({ items }) })
    return { ok: true }
  },
  async saveL7(patterns: L7Pattern[]) {
    if (state.mode === 'mock') { mockL7 = patterns; return { ok: true } }
    await raw('/api/l7-patterns', { method: 'POST', body: JSON.stringify({ patterns }) })
    return { ok: true }
  },
  async saveProjects(projects: ProtectionProject[]) {
    if (state.mode === 'mock') { mockProjects = projects; return { ok: true } }
    await raw('/api/protection-projects', { method: 'POST', body: JSON.stringify({ projects }) })
    return { ok: true }
  },
  async patchConfig(patch: Record<string, unknown>) {
    if (state.mode === 'mock') { mockConfig = { ...mockConfig, ...(patch as Partial<ConfigSnapshot>) }; return { ok: true } }
    await mutate('/api/config', patch, 'PATCH')
    return { ok: true }
  },
  async patchModule(id: string, enabled: boolean) {
    const patch = modulePatch(id, enabled)
    if (state.mode === 'mock') {
      const m = mockModules.find((x) => x.id === id)
      if (m) m.enabled = enabled
      return { ok: true }
    }
    await mutate('/api/config', patch, 'PATCH')
    return { ok: true }
  },
  async reloadConfig() {
    if (state.mode === 'mock') return { ok: true }
    await mutate('/api/config/reload', {})
    return { ok: true }
  },
  async reloadGeoIp() {
    if (state.mode === 'mock') return { ok: true }
    await mutate('/api/geoip/reload', {})
    return { ok: true }
  },
  async syncThreatIntel() {
    if (state.mode === 'mock') return { ok: true }
    await mutate('/api/threat-intel/sync', {})
    return { ok: true }
  },
  async hubStatus(): Promise<any | null> {
    if (state.mode === 'mock') return null
    return fetchOrNull<any>('/api/hub/status')
  },
  async hubNodes(): Promise<any[] | null> {
    if (state.mode === 'mock') return null
    const r = await fetchOrNull<any>('/api/hub/proxy/nodes')
    if (!r) return null
    return r.nodes || r
  },
  async versionCheck(): Promise<any | null> {
    if (state.mode === 'mock') return null
    return fetchOrNull<any>('/api/version/check')
  },
  async ipDetail(ip: string): Promise<any | null> {
    return fetchOrNull<any>(`/api/ip-detail?ip=${encodeURIComponent(ip)}`)
  },

  // ── API Token 管理（仅 admin 可用；运行时 Token 持久化到数据目录） ──
  async tokens(): Promise<any> {
    if (state.mode === 'mock') {
      return { tokens: [
        { name: 'admin', role: 'admin', scopes: ['*'], source: 'config' },
        { name: 'monitor', role: 'readonly', scopes: ['read', 'events'], source: 'runtime' },
      ], count: 2 }
    }
    const r = await fetchOrNull<any>('/api/tokens')
    return r ?? { tokens: [], count: 0 }
  },
  async createToken(name: string, role: string, scopes: string[]): Promise<any> {
    return raw<any>('/api/tokens', { method: 'POST', body: JSON.stringify({ name, role, scopes }) })
  },
  async revokeToken(name: string): Promise<void> {
    await raw(`/api/tokens/${encodeURIComponent(name)}`, { method: 'DELETE' })
  },

  // ── 告警通知（多通道） ──
  async notifyConfig(): Promise<any> {
    if (state.mode === 'mock') {
      return {
        enabled: true, threshold_dps: 1000, cooldown_s: 60,
        email: {
          enabled: true, host: 'smtp.example.com', port: 587, username: 'alert@example.com',
          has_password: true, from: 'XuanArmor <alert@example.com>',
          to: ['oncall@example.com'], security: 'starttls',
        },
      }
    }
    const r = await fetchOrNull<any>('/api/notify')
    return r ?? { enabled: false, threshold_dps: 1000, cooldown_s: 60, email: { enabled: false, host: '', port: 587, username: '', has_password: false, from: '', to: [], security: 'starttls' } }
  },
  async saveNotify(config: Record<string, unknown>): Promise<any> {
    return raw<any>('/api/notify', { method: 'PUT', body: JSON.stringify(config) })
  },
  async testNotify(): Promise<any> {
    return raw<any>('/api/notify/test', { method: 'POST' })
  },
  async notifyDeliveries(limit = 20): Promise<any[]> {
    if (state.mode === 'mock') return []
    const r = await fetchOrNull<any>(`/api/notify/deliveries?limit=${limit}`)
    return r?.deliveries ?? []
  },

  async ipSeries(ip: string, durationSeconds = 3600): Promise<SeriesPoint[]> {
    if (state.mode === 'mock') return []
    const r = await fetchOrNull<any>(`/api/ip-series?ip=${encodeURIComponent(ip)}&duration_s=${Math.max(60, Math.floor(durationSeconds))}`)
    if (!r) return []
    return (r.series || []).map((p: any) => {
      const drops = Number(p.drop_count ?? 0)
      const passes = Number(p.pass_count ?? 0)
      return { t: Number(p.timestamp ?? 0) * 1000, pps: passes, dps: drops, passed: passes, dropped: drops }
    })
  },
}
