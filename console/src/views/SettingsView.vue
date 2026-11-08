<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import PanelCard from '../components/PanelCard.vue'
import ToggleSwitch from '../components/ToggleSwitch.vue'
import { api, state, setToken } from '../api/client'
import { store, toast } from '../stores/app'
import type { ConfigSnapshot, FragmentPolicy, PortRateLimitConfig, RateLimitBlockMode } from '../api/types'

const cfg = ref<ConfigSnapshot | null>(null)
const webhook = ref('')
const threshold = ref(1000)
const cooldown = ref(60)
const retention = ref(30)
const token = ref(state.token)
const saving = ref(false)
const packetLogEnabled = ref(true)
const packetLogRate = ref(16)

// ── API Token 管理 ──
interface TokenInfo { name: string; role: string; scopes: string[]; source: string }
const tokens = ref<TokenInfo[]>([])
const tokenName = ref('')
const tokenRole = ref('operator')
const tokenScopes = ref('read,write,policy,events')
const createdToken = ref('')
const tokenBusy = ref(false)

// ── 告警通知（SMTP 邮件） ──
interface NotifyEmail {
  enabled: boolean
  host: string
  port: number
  username: string
  password: string
  has_password: boolean
  from: string
  to: string
  security: string
}
const notify = ref<{ enabled: boolean; threshold_dps: number; cooldown_s: number; email: NotifyEmail }>({
  enabled: false, threshold_dps: 1000, cooldown_s: 60,
  email: { enabled: false, host: '', port: 587, username: '', password: '', has_password: false, from: '', to: '', security: 'starttls' },
})
const deliveries = ref<Array<Record<string, unknown>>>([])
const notifyBusy = ref(false)
const testing = ref(false)

// 「访问控制与分片」独立编辑态：避免直接 v-model 可选嵌套字段导致类型收窄问题。
const portRate = ref<PortRateLimitConfig>({ enabled: true, threshold: 2000, tick_ms: 100 })
const fragmentPolicy = ref<FragmentPolicy>('rate_limit')
const blockMode = ref<RateLimitBlockMode>('hard')

const fragmentHint = computed(() => {
  switch (fragmentPolicy.value) {
    case 'pass': return '放行非首片分片，仅保留源地址级判断（v0.4.x 兼容行为）'
    case 'rate_limit': return '计入 协议 + 目的端口 聚合限速（默认，推荐）'
    case 'drop': return '直接丢弃非首片分片，最严格，可能影响合法分片流量'
    default: return 'pass / rate_limit / drop'
  }
})
const blockModeHint = computed(() =>
  blockMode.value === 'soft'
    ? '只丢弃超限包、不拉黑源 IP，适合 NAT 出口'
    : '超限后把源 IP 加入动态黑名单（默认）',
)

const dangerLevel = computed(() => store.stats?.danger_level || 0)
const dangerLabel = computed(() => ['平稳', '关注', '警戒', '严重'][Math.min(dangerLevel.value, 3)])
const dangerTone = computed(() => (['ok', 'warn', 'warn', 'danger'] as const)[Math.min(dangerLevel.value, 3)])

/** 兼容旧节点：缺失 v0.5.0 字段时补默认值，保证面板可用。 */
function ensureDefaults() {
  if (!cfg.value) return
  if (!cfg.value.port_rate_limit) cfg.value.port_rate_limit = { enabled: true, threshold: 2000, tick_ms: 100 }
  if (!cfg.value.fragment_policy) cfg.value.fragment_policy = 'rate_limit'
  if (!cfg.value.rate_limit_block_mode) cfg.value.rate_limit_block_mode = 'hard'
  portRate.value = { ...cfg.value.port_rate_limit }
  fragmentPolicy.value = cfg.value.fragment_policy
  blockMode.value = cfg.value.rate_limit_block_mode
  packetLogEnabled.value = cfg.value.packet_log_enabled ?? true
  packetLogRate.value = Number(cfg.value.packet_log_sample_rate ?? 16) || 16
}
/** 保存后重新拉取运行时快照，让界面显示服务端实际生效值。 */
async function reloadConfig() {
  cfg.value = await api.config()
  ensureDefaults()
}
onMounted(async () => {
  cfg.value = await api.config()
  ensureDefaults()
  await loadTokens()
  await loadNotify()
})

// ───────────── API Token 管理 ─────────────
async function loadTokens() {
  try {
    const r = await api.tokens()
    tokens.value = Array.isArray(r.tokens) ? r.tokens : []
  } catch { /* 只读或旧版本忽略 */ }
}
async function createToken() {
  if (!tokenName.value.trim()) {
    toast('请填写 Token 名称', '', 'warn')
    return
  }
  tokenBusy.value = true
  createdToken.value = ''
  try {
    const scopes = tokenScopes.value.split(',').map((s) => s.trim()).filter(Boolean)
    const r = await api.createToken(tokenName.value.trim(), tokenRole.value, scopes)
    createdToken.value = String(r.token || '')
    toast('Token 已创建', '密钥仅显示一次，请立即保存', 'ok')
    await loadTokens()
  } catch (e) {
    toast('创建 Token 失败', String(e), 'danger')
  } finally {
    tokenBusy.value = false
  }
}
async function revokeToken(name: string) {
  tokenBusy.value = true
  try {
    await api.revokeToken(name)
    toast('Token 已吊销', name, 'warn')
    await loadTokens()
  } catch (e) {
    toast('吊销 Token 失败', String(e), 'danger')
  } finally {
    tokenBusy.value = false
  }
}

// ───────────── 告警通知 ─────────────
async function loadNotify() {
  try {
    const r = await api.notifyConfig()
    const email = r.email || {}
    notify.value = {
      enabled: !!r.enabled,
      threshold_dps: Number(r.threshold_dps ?? 1000),
      cooldown_s: Number(r.cooldown_s ?? 60),
      email: {
        enabled: !!email.enabled,
        host: String(email.host ?? ''),
        port: Number(email.port ?? 587),
        username: String(email.username ?? ''),
        password: '',
        has_password: !!email.has_password,
        from: String(email.from ?? ''),
        to: Array.isArray(email.to) ? email.to.join(', ') : String(email.to ?? ''),
        security: String(email.security ?? 'starttls'),
      },
    }
    deliveries.value = await api.notifyDeliveries(10)
  } catch { /* ignore */ }
}
async function saveNotify() {
  notifyBusy.value = true
  try {
    await api.saveNotify({
      enabled: notify.value.enabled,
      threshold_dps: Number(notify.value.threshold_dps) || 0,
      cooldown_s: Number(notify.value.cooldown_s) || 0,
      email: {
        enabled: notify.value.email.enabled,
        host: notify.value.email.host,
        port: Number(notify.value.email.port) || 587,
        username: notify.value.email.username,
        password: notify.value.email.password,
        from: notify.value.email.from,
        to: notify.value.email.to.split(',').map((x) => x.trim()).filter(Boolean),
        security: notify.value.email.security,
      },
    })
    toast('通知配置已保存', '', 'ok')
    await loadNotify()
  } catch (e) {
    toast('保存通知配置失败', String(e), 'danger')
  } finally {
    notifyBusy.value = false
  }
}
async function testNotify() {
  testing.value = true
  try {
    const r = await api.testNotify()
    const sent = Number(r.sent ?? 0)
    toast('测试通知已发送', `${sent} 个通道`, sent > 0 ? 'ok' : 'warn')
    deliveries.value = await api.notifyDeliveries(10)
  } catch (e) {
    toast('测试通知失败', String(e), 'danger')
  } finally {
    testing.value = false
  }
}
async function save() {
  if (!cfg.value) return
  saving.value = true
  try {
    await api.patchConfig({
      // v0.5.0：分片策略 / 限速处置模式 / 协议+目的端口聚合限速
      fragment_policy: fragmentPolicy.value,
      rate_limit_block_mode: blockMode.value,
      // 端口聚合限速不使用指数衰减，decay/block 字段按数据面语义取 0/1/0。
      packet_log_enabled: packetLogEnabled.value,
      packet_log_sample_rate: Math.max(1, Math.round(Number(packetLogRate.value) || 16)),
      port_rate_limit: {
        enabled: portRate.value.enabled,
        // 输入框被清空时 v-model.number 会得到空串，这里兜底成合法值，避免 PATCH 被拒。
        threshold: Math.max(1, Math.round(Number(portRate.value.threshold) || 2000)),
        tick_ms: Math.max(1, Math.round(Number(portRate.value.tick_ms) || 100)),
        decay_num: 0,
        decay_den: 1,
        block_duration_s: 0,
      },
    })
    toast('系统设置已保存', '运行时参数已下发数据面，部分参数需重载配置后生效', 'ok')
    await reloadConfig()
  } catch (e) {
    const desc = store.role === 'readonly' ? '只读令牌不允许写操作（403）' : String(e)
    toast('保存失败', desc, 'danger')
  } finally {
    saving.value = false
  }
}
function resetToken() {
  const t = Array.from(crypto.getRandomValues(new Uint8Array(16))).map((b) => b.toString(16).padStart(2, '0')).join('')
  token.value = t
  setToken(t)
  toast('访问令牌已重置', '请同步更新所有调用方', 'warn')
}
</script>

<template>
  <div class="page">
    <div class="page-head">
      <div class="page-title">系统设置</div>
      <div class="page-meta">开关类参数热生效</div>
      <div class="page-actions">
        <button class="btn btn-primary btn-sm" :disabled="saving" @click="save">
          {{ saving ? '保存中…' : '保存运行参数' }}
        </button>
      </div>
    </div>

    <div class="grid grid-main-side">
      <PanelCard title="运行参数" sub="限速处置与分片策略">
        <div class="panel-note" style="border-top: 0; padding: 0">
          各防护模块的启用/停用开关已统一放在「防护模块」页面；这里保留分片策略、限速处置模式等运行参数。
        </div>
      </PanelCard>

      <div class="grid" style="grid-template-columns: minmax(0, 1fr); gap: 14px">
        <PanelCard title="实例信息" sub="只读">
          <dl class="kv">
            <dt>版本</dt><dd class="mono">v{{ cfg?.version || '—' }}</dd>
            <dt>网卡</dt><dd class="mono">{{ cfg?.interface || '—' }}</dd>
            <dt>监听地址</dt><dd class="mono">{{ cfg?.web_bind || '—' }}</dd>
            <dt>存储路径</dt><dd class="mono" :title="cfg?.store_path">{{ cfg?.store_path || '—' }}</dd>
            <dt>审计后端</dt>
            <dd class="mono" :title="cfg?.audit_path">
              {{ cfg?.audit_enabled ? '文件审计已启用' : '文件审计未启用' }}
            </dd>
            <dt>XDP 模式</dt><dd class="mono">{{ (store.stats as never as Record<string, unknown>)?.xdp_mode || '—' }}</dd>
          </dl>
        </PanelCard>
        <PanelCard title="访问令牌" sub="控制台与 API 共用">
          <div class="field">
            <label class="field-label">当前令牌（仅本次会话内存）</label>
            <input class="input mono" :value="token || '（由 HttpOnly Cookie 管理）'" readonly />
          </div>
          <div class="row" style="margin-top: 10px">
            <button class="btn" @click="resetToken">重置令牌</button>
            <span class="muted" style="font-size: 11px">重置后需重新登录</span>
          </div>
        </PanelCard>
      </div>
    </div>

    <PanelCard title="访问控制与分片" sub="v0.5.0 · 运行时热生效">
      <div class="setting-row">
        <div class="sr-main">
          <div class="sr-title">端口聚合限速</div>
          <div class="sr-desc">按 协议 + 目的端口 聚合统计，防伪造源 IP 绕过单源限速（默认启用）</div>
        </div>
        <ToggleSwitch v-model="portRate.enabled" />
      </div>
      <div class="grid grid-3" style="margin-top: 12px">
        <div class="field">
          <label class="field-label">聚合限速阈值（包/窗口）</label>
          <input v-model.number="portRate.threshold" class="input mono" type="number" min="1" />
          <div class="field-hint">窗口 {{ portRate.tick_ms || 100 }}ms，默认 2000 包/窗口</div>
        </div>
        <div class="field">
          <label class="field-label">非首片分片策略</label>
          <select v-model="fragmentPolicy" class="select">
            <option value="pass">pass · 放行</option>
            <option value="rate_limit">rate_limit · 计入聚合限速</option>
            <option value="drop">drop · 直接丢弃</option>
          </select>
          <div class="field-hint">{{ fragmentHint }}</div>
        </div>
        <div class="field">
          <label class="field-label">限速超限处置模式</label>
          <select v-model="blockMode" class="select">
            <option value="hard">hard · 拉黑源 IP</option>
            <option value="soft">soft · 只丢弃不拉黑</option>
          </select>
          <div class="field-hint">{{ blockModeHint }}</div>
        </div>
      </div>
      <div class="muted" style="margin-top: 10px; font-size: 11px; line-height: 1.55">
        soft 适合 NAT 出口：超限只丢弃超量包，不把出口 IP 拉黑；pass 为 v0.4.x 兼容行为（跳过 L4 模块，不推荐）。
      </div>
    </PanelCard>

    <PanelCard title="API Token" sub="供上游应用远程调用；运行时 Token 显示一次密钥，可随时吊销" flush>
      <div class="table-wrap">
        <table class="dt">
          <thead><tr><th>名称</th><th>角色</th><th>作用域</th><th>来源</th><th class="col-actions">操作</th></tr></thead>
          <tbody>
            <tr v-for="t in tokens" :key="t.name">
              <td class="cell-main mono">{{ t.name }}</td>
              <td>{{ t.role }}</td>
              <td class="mono muted">{{ (t.scopes || []).join(', ') || '—' }}</td>
              <td>{{ t.source === 'runtime' ? '运行时' : '配置文件' }}</td>
              <td class="col-actions">
                <div class="row-actions">
                  <button v-if="t.source === 'runtime'" class="btn btn-sm" :disabled="tokenBusy" @click="revokeToken(t.name)">吊销</button>
                  <span v-else class="muted" style="font-size: 11px">配置项</span>
                </div>
              </td>
            </tr>
            <tr v-if="!tokens.length"><td colspan="5" class="empty"><div class="title">暂无 Token</div>下方创建运行时 Token，或在配置文件中添加 [[api_tokens]]</td></tr>
          </tbody>
        </table>
      </div>
      <div class="grid grid-3" style="padding: 10px 12px 4px">
        <div class="field"><label class="field-label">名称</label><input v-model="tokenName" class="input mono" placeholder="orchestrator-prod" /></div>
        <div class="field"><label class="field-label">角色</label>
          <select v-model="tokenRole" class="input">
            <option value="operator">operator（读写策略）</option>
            <option value="readonly">readonly（只读）</option>
            <option value="admin">admin（全部权限）</option>
          </select>
        </div>
        <div class="field"><label class="field-label">作用域（逗号分隔）</label><input v-model="tokenScopes" class="input mono" /></div>
      </div>
      <div class="spread" style="padding: 4px 12px 12px">
        <button class="btn btn-primary btn-sm" :disabled="tokenBusy" @click="createToken">{{ tokenBusy ? '创建中…' : '创建 Token' }}</button>
        <span class="muted" style="font-size: 11px">密钥仅在创建时返回一次；运行时 Token 保存在数据目录，重启后仍有效。</span>
      </div>
      <div v-if="createdToken" class="panel-note" style="padding: 8px 12px; border-top: 0">
        新 Token 密钥（仅显示一次）：<span class="mono">{{ createdToken }}</span>
      </div>
    </PanelCard>

    <PanelCard title="包日志" sub="采样包日志用于排障；采样率 1/N 表示平均每 N 个包记录 1 个">
      <div class="grid grid-3">
        <div class="field" style="min-width: 160px">
          <label class="field-label">启用采样</label>
          <ToggleSwitch :model-value="packetLogEnabled" @update:model-value="(v: boolean) => (packetLogEnabled = v)" />
        </div>
        <div class="field"><label class="field-label">采样率 1/N</label><input v-model.number="packetLogRate" class="input mono" type="number" min="1" max="65535" /></div>
        <div class="field"><label class="field-label">当前值</label><input class="input mono" :value="packetLogEnabled ? `1/${packetLogRate}` : '已关闭'" readonly /></div>
      </div>
      <div class="panel-note" style="border-top: 0; padding: 10px 0 0">
        修改后点右上角「保存运行参数」生效；内存上限（条目数/字节数）在配置文件中调整。
      </div>
    </PanelCard>

    <div class="grid grid-2">
      <PanelCard title="告警通知" sub="DROP 速率超过阈值时发送邮件">
        <div class="spread" style="margin-bottom: 8px">
          <div class="field" style="min-width: 160px">
            <label class="field-label">启用阈值告警</label>
            <ToggleSwitch :model-value="notify.enabled" @update:model-value="(v: boolean) => (notify.enabled = v)" />
          </div>
          <div class="field" style="width: 160px"><label class="field-label">阈值（DROP/s）</label><input v-model.number="notify.threshold_dps" class="input mono" type="number" /></div>
          <div class="field" style="width: 140px"><label class="field-label">冷却（s）</label><input v-model.number="notify.cooldown_s" class="input mono" type="number" /></div>
        </div>
        <div class="spread" style="margin-bottom: 8px">
          <div class="field" style="min-width: 160px">
            <label class="field-label">启用邮件通知</label>
            <ToggleSwitch :model-value="notify.email.enabled" @update:model-value="(v: boolean) => (notify.email.enabled = v)" />
          </div>
          <div class="field" style="width: 140px"><label class="field-label">传输安全</label>
            <select v-model="notify.email.security" class="input">
              <option value="starttls">STARTTLS（587）</option>
              <option value="tls">SSL/TLS（465）</option>
              <option value="none">明文（仅内网测试）</option>
            </select>
          </div>
        </div>
        <div class="grid grid-3">
          <div class="field"><label class="field-label">SMTP 服务器</label><input v-model="notify.email.host" class="input mono" placeholder="smtp.example.com" /></div>
          <div class="field"><label class="field-label">端口</label><input v-model.number="notify.email.port" class="input mono" type="number" /></div>
          <div class="field"><label class="field-label">用户名</label><input v-model="notify.email.username" class="input mono" /></div>
          <div class="field"><label class="field-label">密码/授权码</label><input v-model="notify.email.password" class="input mono" type="password" :placeholder="notify.email.has_password ? '已保存（留空表示不修改）' : ''" /></div>
          <div class="field"><label class="field-label">发件人</label><input v-model="notify.email.from" class="input mono" placeholder="XuanArmor &lt;alert@example.com&gt;" /></div>
          <div class="field"><label class="field-label">收件人（逗号分隔）</label><input v-model="notify.email.to" class="input mono" placeholder="ops@example.com, oncall@example.com" /></div>
        </div>
        <div class="spread" style="margin-top: 8px">
          <span class="muted" style="font-size: 11px">阈值告警按冷却时间限频发送到上述收件人。</span>
          <div class="row-actions">
            <button class="btn btn-sm" :disabled="testing" @click="testNotify">{{ testing ? '发送中…' : '发送测试邮件' }}</button>
            <button class="btn btn-primary btn-sm" :disabled="notifyBusy" @click="saveNotify">{{ notifyBusy ? '保存中…' : '保存通知配置' }}</button>
          </div>
        </div>
        <div v-if="deliveries.length" class="mt-8">
          <div class="muted" style="font-size: 11px; margin-bottom: 4px">最近投递</div>
          <div class="table-wrap">
            <table class="dt">
              <thead><tr><th>时间</th><th>事件</th><th>结果</th><th class="col-num">耗时</th></tr></thead>
              <tbody>
                <tr v-for="(d, i) in deliveries" :key="i">
                  <td class="mono muted">{{ d.timestamp }}</td>
                  <td>{{ d.event }}</td>
                  <td><span class="state" :class="d.ok ? 'ok' : 'danger'"><span class="dot" :class="d.ok ? 'ok' : 'danger'"></span>{{ d.ok ? '成功' : (d.error || '失败') }}</span></td>
                  <td class="col-num mono">{{ d.latency_ms }}ms</td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </PanelCard>

      <PanelCard title="危险信号" sub="综合 PPS、DROP 率与信任分布">
        <div class="spread" style="align-items: baseline">
          <div>
            <div class="muted" style="font-size: 11px">当前危险等级</div>
            <div class="mono" style="font-size: 26px; font-weight: 600; line-height: 1.2">L{{ dangerLevel }}</div>
          </div>
          <span class="state" :class="dangerTone === 'ok' ? 'ok' : dangerTone === 'danger' ? 'danger' : 'warn'">
            <span class="dot" :class="dangerTone"></span>{{ dangerLabel }}
          </span>
        </div>
        <div class="progress" :class="dangerTone === 'danger' ? 'danger' : dangerTone === 'warn' ? 'warn' : 'ok'" style="margin-top: 10px">
          <i :style="{ width: ((dangerLevel / 3) * 100) + '%' }"></i>
        </div>
        <div class="dl" style="margin-top: 10px">
          <div class="dl-row"><span class="dl-key">可信来源</span><span class="dl-val">{{ store.stats?.trust_trusted || 0 }}</span></div>
          <div class="dl-row"><span class="dl-key">中性来源</span><span class="dl-val">{{ store.stats?.trust_neutral || 0 }}</span></div>
          <div class="dl-row"><span class="dl-key">可疑来源</span><span class="dl-val">{{ store.stats?.trust_suspicious || 0 }}</span></div>
          <div class="dl-row"><span class="dl-key">恶意来源</span><span class="dl-val">{{ store.stats?.trust_malicious || 0 }}</span></div>
        </div>
      </PanelCard>
    </div>
  </div>
</template>
