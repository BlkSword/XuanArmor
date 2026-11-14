<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import PanelCard from '../components/PanelCard.vue'
import KpiCard from '../components/KpiCard.vue'
import { api } from '../api/client'
import { clearLiveAudit, liveAudit, toast } from '../stores/app'
import type { AuditEntry } from '../api/types'
import { fmtDateTime, fmtInt } from '../utils/format'

const rows = ref<AuditEntry[]>([])
/** 服务端过滤后的审计总条数（v0.5.4 服务端分页） */
const total = ref(0)
const q = ref('')
const actor = ref('all')
const page = ref(1)
const pageSize = 50
const verifying = ref(false)
const loading = ref(false)
const actorOptions = ref<string[]>([])

const pages = computed(() => Math.max(1, Math.ceil(total.value / pageSize)))
const apiCalls = computed(() => rows.value.filter((r) => r.actor === 'api').length)
const lastEntry = computed(() => rows.value[0])

/** v0.5.4：服务端分页 + 服务端过滤，可翻到更早的审计记录（后端最多 1 万条）。 */
async function load() {
  loading.value = true
  try {
    const r = await api.audit({
      limit: pageSize,
      offset: (page.value - 1) * pageSize,
      filter: q.value.trim() || undefined,
      actor: actor.value,
    })
    rows.value = r.items
    total.value = r.total
    // 翻页/刷新后 total 变小：收敛到最后一页，避免空白页
    const maxPage = Math.max(1, Math.ceil(r.total / pageSize))
    if (page.value > maxPage) {
      page.value = maxPage
      return load()
    }
    actorOptions.value = [...new Set([
      ...actorOptions.value,
      ...r.items.map((x) => x.actor).filter(Boolean),
    ])]
  } finally {
    loading.value = false
  }
}

let filterTimer: number | undefined
watch([q, actor], () => {
  if (filterTimer) window.clearTimeout(filterTimer)
  filterTimer = window.setTimeout(() => {
    page.value = 1
    load()
  }, 250)
})
onBeforeUnmount(() => {
  if (filterTimer) window.clearTimeout(filterTimer)
})
onMounted(load)

function prevPage() {
  if (page.value <= 1) return
  page.value--
  load()
}
function nextPage() {
  if (page.value >= pages.value) return
  page.value++
  load()
}

/** 收到 SSE 实时记录后由用户确认刷新，避免自动插入导致分页错位。 */
function applyLive() {
  clearLiveAudit()
  page.value = 1
  load()
}

/** v0.5.0：校验审计哈希链，任何改写/删除/插入都会在断点处被发现。 */
async function verifyChain() {
  verifying.value = true
  try {
    const r = await api.auditVerify()
    if (r.enabled === false) {
      toast('文件审计未启用', r.message || '开启 [audit].enabled 后可用哈希链校验', 'warn')
    } else if (!r.ok) {
      toast('审计链校验失败', `首个断点序号 ${r.first_bad_seq ?? '—'}`, 'danger')
    } else {
      toast('审计链完整', `已校验 ${r.checked ?? 0} 条记录${r.path ? ' · ' + r.path : ''}`, 'ok')
    }
  } catch (e) {
    toast('校验失败', String(e), 'danger')
  } finally {
    verifying.value = false
  }
}
</script>

<template>
  <div class="page">
    <div class="page-head">
      <div class="page-title">审计日志</div>
      <div class="page-meta">写操作审计</div>
      <div class="page-actions">
        <span class="state"><span class="dot live"></span>SSE 推送</span>
        <button class="btn btn-sm" :disabled="verifying" @click="verifyChain">
          {{ verifying ? '校验中…' : '校验审计链' }}
        </button>
      </div>
    </div>

    <div class="kpis">
      <KpiCard label="审计记录" :value="total" format="int" foot="服务端过滤后总数" />
      <KpiCard label="API 调用" :value="apiCalls" format="int" foot="本页来自受信管理网段" />
      <KpiCard label="操作者" :value="actorOptions.length" format="int" :foot="actorOptions.join(' / ') || '—'" />
      <KpiCard label="最近操作" :value="lastEntry ? fmtDateTime(lastEntry.timestamp_ns) : '—'" format="raw" :foot="lastEntry?.action || '—'" />
    </div>

    <div v-if="liveAudit.items.length" class="bulkbar">
      <strong>收到 {{ liveAudit.items.length }} 条新审计记录</strong>
      <span class="muted">|</span>
      <button class="btn btn-sm btn-primary" @click="applyLive">刷新到最新</button>
      <button class="btn btn-sm btn-ghost" @click="clearLiveAudit()">忽略</button>
    </div>

    <PanelCard flush>
      <div class="toolbar">
        <div class="search-inline"><span>⌕</span><input v-model="q" placeholder="搜索动作 / 目标 / 来源 IP / 详情" /></div>
        <select v-model="actor" class="select" style="width: 130px">
          <option value="all">全部来源</option>
          <option v-for="a in actorOptions" :key="a" :value="a">{{ a }}</option>
        </select>
        <div class="spacer"></div>
        <span class="muted">第 {{ page }} / {{ pages }} 页 · 共 {{ fmtInt(total) }} 条</span>
      </div>
      <div class="table-wrap">
        <table class="dt">
          <thead><tr><th>时间</th><th>操作者</th><th>动作</th><th>目标</th><th>来源 IP</th><th>详情</th></tr></thead>
          <tbody>
            <tr v-for="r in rows" :key="r.id">
              <td class="mono muted">{{ fmtDateTime(r.timestamp_ns) }}</td>
              <td class="muted">{{ r.actor }}</td>
              <td class="cell-main">{{ r.action }}</td>
              <td class="mono">{{ r.target }}</td>
              <td class="mono muted">{{ r.source_ip }}</td>
              <td class="muted" style="max-width: 380px; overflow: hidden; text-overflow: ellipsis">{{ r.detail }}</td>
            </tr>
            <tr v-if="!rows.length"><td colspan="6" class="empty"><div class="title">暂无审计记录</div>控制面写操作会自动记录</td></tr>
          </tbody>
        </table>
      </div>
      <div class="tfoot">
        <span>每页 {{ pageSize }} 条 · 审计记录可在服务端配置为 JSON Lines 文件持久化（后端最多回读 1 万条）</span>
        <div class="pager">
          <button class="btn btn-sm" :disabled="page <= 1 || loading" @click="prevPage">上一页</button>
          <button class="btn btn-sm" :disabled="page >= pages || loading" @click="nextPage">下一页</button>
        </div>
      </div>
    </PanelCard>
  </div>
</template>
