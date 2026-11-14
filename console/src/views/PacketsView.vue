<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import PanelCard from '../components/PanelCard.vue'
import { api } from '../api/client'
import { openDrawer } from '../stores/ui'
import type { PacketSample } from '../api/types'
import { fmtTime } from '../utils/format'

const rows = ref<PacketSample[]>([])
/** 服务端过滤后的采样总条数（v0.5.4 服务端分页） */
const total = ref(0)
/** 包日志内存缓冲占用（条数 / 字节） */
const usage = ref({ entries: 0, bytes: 0 })
const sampling = ref<{ enabled: boolean; rate: number } | null>(null)
const q = ref('')
const action = ref('all')
const pageSize = 25
const loading = ref(false)
/** 游标栈：cursors[0] = 第一页（无游标），翻页时压入上一页最后一条样本的时间戳 */
const cursors = ref<Array<number | undefined>>([undefined])
const pageNo = computed(() => cursors.value.length)
const hasMore = computed(() => rows.value.length >= pageSize)

/** v0.5.4：服务端过滤 + 游标翻页（before_ns），避免环形缓冲写入导致 offset 翻页重复。 */
async function load() {
  loading.value = true
  try {
    const r = await api.packets({
      limit: pageSize,
      before_ns: cursors.value[cursors.value.length - 1],
      ip: q.value.trim() || undefined,
      action: action.value === 'all' ? undefined : (action.value === 'DROP' ? 1 : 2),
    })
    rows.value = r.items
    total.value = r.total
    usage.value = r.usage
  } finally {
    loading.value = false
  }
}

let filterTimer: number | undefined
watch([q, action], () => {
  if (filterTimer) window.clearTimeout(filterTimer)
  filterTimer = window.setTimeout(() => {
    cursors.value = [undefined]
    load()
  }, 250)
})
onBeforeUnmount(() => {
  if (filterTimer) window.clearTimeout(filterTimer)
})

onMounted(async () => {
  load()
  try {
    const cfg = await api.config() as Record<string, unknown>
    sampling.value = {
      enabled: (cfg.packet_log_enabled as boolean) ?? true,
      rate: Number(cfg.packet_log_sample_rate ?? 16) || 16,
    }
  } catch { /* ignore */ }
})

function prevPage() {
  if (cursors.value.length <= 1) return
  cursors.value = cursors.value.slice(0, -1)
  load()
}
function nextPage() {
  const last = rows.value[rows.value.length - 1]
  if (!last || !hasMore.value) return
  cursors.value = [...cursors.value, last.timestamp_ns]
  load()
}
</script>

<template>
  <div class="page">
    <div class="page-head">
      <div class="page-title">包日志</div>
      <div class="page-meta">
        匹配 <span class="mono">{{ total }}</span> 条 · 缓冲占用 <span class="mono">{{ usage.entries }}</span> 条
        <span v-if="sampling">· 采样 {{ sampling.enabled ? `1/${sampling.rate}` : '已关闭' }}</span>
      </div>
      <div class="page-actions"><span class="state"><span class="dot warn"></span>采样</span></div>
    </div>

    <PanelCard flush>
      <div class="toolbar">
        <div class="search-inline"><span>⌕</span><input v-model="q" placeholder="搜索源 / 目的 IP" /></div>
        <select v-model="action" class="select" style="width: 118px">
          <option value="all">全部动作</option><option value="DROP">DROP</option><option value="PASS">PASS</option>
        </select>
        <div class="spacer"></div>
        <span class="muted">第 {{ pageNo }} 页 · 共 {{ total }} 条</span>
      </div>
      <div v-if="!rows.length && !q && action === 'all'" class="panel-note" style="border-top: 0">
        暂无采样记录：请在「系统设置 → 包日志」确认已启用，并将采样率调低（如 1/16）；仅 DROP 与防护项目 PASS 的流量会被采样。
      </div>
      <div class="table-wrap">
        <table class="dt">
          <thead>
            <tr><th>时间</th><th>源地址</th><th>目的地址</th><th>协议</th><th>TCP 标志</th><th>载荷预览</th><th>动作</th><th class="col-num">包长</th><th class="col-actions">操作</th></tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.id">
              <td class="mono muted">{{ fmtTime(r.timestamp_ns) }}</td>
              <td>
                <span class="mono cell-link" @click="openDrawer('IP 详情', 'ip', { ip: r.src_ip, count: 0 })">{{ r.src_ip }}</span>
              </td>
              <td class="mono">{{ r.dst_ip }}:{{ r.dst_port }}</td>
              <td class="muted">{{ r.protocol }}</td>
              <td class="mono muted">{{ r.tcp_flags || '—' }}</td>
              <td class="mono muted" style="max-width: 280px; overflow: hidden; text-overflow: ellipsis">{{ r.payload_preview || '—' }}</td>
              <td>
                <span class="state" :class="r.action === 'DROP' ? 'danger' : 'ok'">
                  <span class="dot" :class="r.action === 'DROP' ? 'danger' : 'ok'"></span>{{ r.action }}
                </span>
              </td>
              <td class="col-num mono">{{ r.length }}</td>
              <td class="col-actions">
                <div class="row-actions"><button class="btn btn-sm" @click="openDrawer('数据包详情', 'packet', r)">查看</button></div>
              </td>
            </tr>
            <tr v-if="!rows.length"><td colspan="9" class="empty"><div class="title">暂无采样包</div>没有匹配当前过滤条件的采样记录</td></tr>
          </tbody>
        </table>
      </div>
      <div class="tfoot">
        <span>每页 {{ pageSize }} 条 · 共 {{ total }} 条 · 缓冲已用 {{ usage.entries }} 条</span>
        <div class="pager">
          <button class="btn btn-sm" :disabled="cursors.length <= 1 || loading" @click="prevPage">上一页</button>
          <button class="btn btn-sm" :disabled="!hasMore || loading" @click="nextPage">下一页</button>
        </div>
      </div>
    </PanelCard>
  </div>
</template>
