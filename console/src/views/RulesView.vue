<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import PanelCard from '../components/PanelCard.vue'
import KpiCard from '../components/KpiCard.vue'
import { api } from '../api/client'
import { toast } from '../stores/app'
import type { ProtectionProject } from '../api/types'

interface HubNode { name: string; ip: string; version: string; status: string; policies: number; last_seen: string }

const projects = ref<ProtectionProject[]>([])
const nodes = ref<HubNode[]>([])
const hub = ref({ connected: false, url: '—' })
const syncing = ref(false)

const onlineNodes = computed(() => nodes.value.filter((n) => n.status === 'online').length)
const ruleCount = computed(() => projects.value.length)

onMounted(async () => {
  projects.value = await api.projects()
  const status = await api.hubStatus().catch(() => null)
  let configured = false
  if (status && typeof status === 'object') {
    const s = status as Record<string, unknown>
    configured = !!(s.configured ?? s.enabled)
    hub.value = {
      connected: !!(s.connected ?? s.hub_connected),
      url: String(s.active_url || s.url || s.hub_url || '—'),
    }
  }
  const remote = configured ? await api.hubNodes().catch(() => null) : null
  if (Array.isArray(remote) && remote.length) {
    nodes.value = remote.map((n: Record<string, unknown>) => ({
      name: String(n.node_name ?? n.name ?? '—'),
      ip: String(n.ip ?? n.address ?? '—'),
      version: String(n.version ?? '—'),
      status: String(n.status ?? 'online'),
      policies: Number(n.policies ?? n.policy_count ?? 0),
      last_seen: n.last_seen ? String(n.last_seen) : n.last_seen_s ? `${n.last_seen_s}s` : '—',
    }))
  }
})
async function sync() {
  syncing.value = true
  try {
    await api.hubStatus()
    toast('规则包同步已触发', '已向 Hub 查询最新策略', 'ok')
  } catch (e) {
    toast('同步失败', String(e), 'danger')
  } finally {
    syncing.value = false
  }
}
</script>

<template>
  <div class="page">
    <div class="page-head">
      <div class="page-title">规则与 Hub</div>
      <div class="page-meta">Hub 节点与策略同步状态</div>
      <div class="page-actions">
        <button class="btn btn-primary btn-sm" :disabled="syncing" @click="sync">{{ syncing ? '同步中…' : '同步规则包' }}</button>
      </div>
    </div>

    <div class="kpis">
      <KpiCard
        label="Hub 连接"
        :value="hub.connected ? '已连接' : '未连接'"
        format="raw"
        :tone="hub.connected ? 'ok' : 'warn'"
        :foot="hub.url"
      />
      <KpiCard label="在线节点" :value="`${onlineNodes} / ${nodes.length}`" format="raw" foot="心跳间隔 10s" />
      <KpiCard label="防护项目" :value="ruleCount" format="int" foot="本地项目数" />
    </div>

    <PanelCard title="节点列表" sub="Hub 侧的 XuanArmor 节点与策略版本" flush>
      <div class="table-wrap">
        <table class="dt">
          <thead><tr><th>节点</th><th>地址</th><th>版本</th><th class="col-num">策略数</th><th>最近心跳</th><th>状态</th><th class="col-actions">操作</th></tr></thead>
          <tbody>
            <tr v-for="n in nodes" :key="n.name">
              <td class="cell-main">{{ n.name }}</td>
              <td class="mono">{{ n.ip }}</td>
              <td class="mono muted">v{{ n.version }}</td>
              <td class="col-num mono">{{ n.policies }}</td>
              <td class="muted mono">{{ n.last_seen }}</td>
              <td>
                <span class="state" :class="n.status === 'online' ? 'ok' : 'warn'">
                  <span class="dot" :class="n.status === 'online' ? 'ok' : 'warn'"></span>
                  {{ n.status === 'online' ? '在线' : '延迟' }}
                </span>
              </td>
              <td class="col-actions"><div class="row-actions"><button class="btn btn-sm">详情</button></div></td>
            </tr>
            <tr v-if="!nodes.length"><td colspan="7" class="empty"><div class="title">未配置 Hub</div>在节点配置中启用 [hub] 后此处显示集群节点</td></tr>
          </tbody>
        </table>
      </div>
    </PanelCard>

  </div>
</template>
