<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import PanelCard from '../components/PanelCard.vue'
import { api } from '../api/client'
import { toast } from '../stores/app'
import { openDrawer } from '../stores/ui'
import type { AttackEvent } from '../api/types'
import { fmtInt, fmtTime, reasonLabel } from '../utils/format'

const rows = ref<AttackEvent[]>([])
/** 窗口内（最近 1000 条环形缓冲）匹配当前过滤条件的总条数 */
const total = ref(0)
/** 服务端环形缓冲容量 */
const windowSize = ref(1000)
const loading = ref(true)
const q = ref('')
const proto = ref('all')
const page = ref(1)
const pageSize = 50
const selected = ref<Set<string>>(new Set())

const PROTO_NUM: Record<string, number> = { tcp: 6, udp: 17, icmp: 1 }
const pages = computed(() => Math.max(1, Math.ceil(total.value / pageSize)))
const allChecked = computed(() => rows.value.length > 0 && rows.value.every((r) => selected.value.has(r.id)))

/** v0.5.4：改为服务端分页 + 服务端过滤，可翻到环形缓冲内的更早事件。 */
async function load() {
  loading.value = true
  try {
    const r = await api.attackEvents({
      limit: pageSize,
      offset: (page.value - 1) * pageSize,
      q: q.value.trim() || undefined,
      protocol: proto.value === 'all' ? undefined : PROTO_NUM[proto.value],
    })
    rows.value = r.items
    total.value = r.total
    windowSize.value = r.window
    // 翻页/筛选后清空跨页选中，避免批量封禁误伤不可见条目
    selected.value = new Set()
  } finally {
    loading.value = false
  }
}

let filterTimer: number | undefined
watch([q, proto], () => {
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
function toggle(id: string) {
  const next = new Set(selected.value)
  next.has(id) ? next.delete(id) : next.add(id)
  selected.value = next
}
function toggleAll() {
  const next = new Set(selected.value)
  if (allChecked.value) rows.value.forEach((r) => next.delete(r.id))
  else rows.value.forEach((r) => next.add(r.id))
  selected.value = next
}
async function bulkBlock() {
  const ips = [...new Set(rows.value.filter((r) => selected.value.has(r.id)).map((r) => r.src_ip))]
  for (const ip of ips) await api.block(ip, 3600)
  toast(`已封禁 ${ips.length} 个源地址`, '有效期 1 小时', 'ok')
  selected.value = new Set()
  load()
}
</script>

<template>
  <div class="page">
    <div class="page-head">
      <div class="page-title">攻击事件</div>
      <div class="page-meta">
        窗口内 <span class="mono">{{ fmtInt(total) }}</span> 条 · 服务端保留最近 <span class="mono">{{ fmtInt(windowSize) }}</span> 条
      </div>
      <div class="page-actions">
        <span class="state"><span class="dot live"></span>实时同步</span>
        <button class="btn btn-sm" :disabled="loading" @click="load">刷新</button>
      </div>
    </div>

    <PanelCard flush>
      <div class="toolbar">
        <div class="search-inline"><span>⌕</span><input v-model="q" placeholder="搜索源 IP / 命中规则" /></div>
        <select v-model="proto" class="select" style="width: 118px">
          <option value="all">全部协议</option><option value="tcp">TCP</option><option value="udp">UDP</option><option value="icmp">ICMP</option>
        </select>
        <div class="spacer"></div>
        <span class="muted">第 {{ page }} / {{ pages }} 页</span>
      </div>

      <div v-if="selected.size" class="bulkbar">
        <strong>已选 {{ selected.size }} 条</strong>
        <span class="muted">|</span>
        <button class="btn btn-sm btn-danger" @click="bulkBlock()">批量封禁源 IP</button>
        <button class="btn btn-sm btn-ghost" @click="selected = new Set()">取消选择</button>
      </div>

      <div class="table-wrap">
        <table class="dt">
          <thead>
            <tr>
              <th class="col-check"><input type="checkbox" :checked="allChecked" @change="toggleAll" /></th>
              <th>时间</th><th>源地址</th><th>目的端口</th><th>协议</th><th>命中规则</th><th>动作</th>
              <th class="col-actions">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.id" :class="{ selected: selected.has(r.id) }">
              <td class="col-check"><input type="checkbox" :checked="selected.has(r.id)" @change="toggle(r.id)" /></td>
              <td class="mono muted">{{ fmtTime(r.timestamp_ns) }}</td>
              <td>
                <span class="mono cell-link" @click="openDrawer('IP 详情', 'ip', { ip: r.src_ip, count: 0 })">{{ r.src_ip }}</span>
              </td>
              <td class="mono">{{ r.dst_port || '—' }}</td>
              <td class="muted">{{ r.protocol }}</td>
              <td>{{ reasonLabel(r.reason) }}</td>
              <td>
                <span class="state" :class="r.action === 'DROP' ? 'danger' : 'ok'">
                  <span class="dot" :class="r.action === 'DROP' ? 'danger' : 'ok'"></span>{{ r.action }}
                </span>
              </td>
              <td class="col-actions">
                <div class="row-actions"><button class="btn btn-sm" @click="openDrawer('事件详情', 'event', r)">详情</button></div>
              </td>
            </tr>
            <tr v-if="!rows.length"><td colspan="8" class="empty"><div class="title">没有匹配的事件</div>调整筛选条件或稍后刷新</td></tr>
          </tbody>
        </table>
      </div>

      <div class="tfoot">
        <span>每页 {{ pageSize }} 条 · 环形缓冲超过 {{ fmtInt(windowSize) }} 条后旧事件会被覆盖（数据面 DropEvent 不含目的 IP / 包长）</span>
        <div class="pager">
          <button class="btn btn-sm" :disabled="page <= 1 || loading" @click="prevPage">上一页</button>
          <button class="btn btn-sm" :disabled="page >= pages || loading" @click="nextPage">下一页</button>
        </div>
      </div>
    </PanelCard>
  </div>
</template>
