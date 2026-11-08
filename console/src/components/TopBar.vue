<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { cycleTheme, isReadonly, refresh, store, openSearch, logout, updateInfo } from '../stores/app'
import { navGroups } from '../router'

const emit = defineEmits<{ (e: 'toggle-sidebar'): void }>()
const route = useRoute()

const groupLabel = computed(() => navGroups.find((g) => g.items.some((i) => i.path === route.path))?.label || '控制台')
const title = computed(() => (route.meta.title as string) || 'XuanArmor')

const XDP_SOLUTION = [
  '检查节点配置中的 interface 是否为实际网卡名（ip link 查看）',
  '若驱动不支持 native XDP，请在配置中设置 [xdp] mode = "generic" 后重启',
  '确认进程以 root/CAP_BPF 运行，且内核已启用 BTF（/sys/kernel/btf/vmlinux）',
  '查看日志排查挂载失败原因：journalctl -u xuanarmor -n 50',
].join('\n')
const xdp = computed(() => {
  const s = store.stats as Record<string, unknown> | null
  if (!s) return { text: '连接中', dot: 'warn', title: '正在连接数据面' }
  if (s.xdp_attached === false) {
    return { text: 'XDP 未挂载', dot: 'danger', title: `防护未生效，排查建议：\n${XDP_SOLUTION}` }
  }
  const mode = String(s.xdp_mode || '')
  if (mode !== 'native' && mode !== 'generic') {
    return { text: 'XDP 状态未知', dot: 'warn', title: `无法确认 XDP 挂载状态，建议排查：\n${XDP_SOLUTION}` }
  }
  return {
    text: mode === 'native' ? 'XDP native' : 'XDP generic',
    dot: mode === 'native' ? 'ok' : 'warn',
    title: mode === 'native' ? 'XDP 运行在原生驱动模式' : 'XDP 运行在 generic/SKB 模式（性能低于 native）',
  }
})
const themeLabel = computed(() => (store.theme === 'system' ? '跟随系统' : store.theme === 'light' ? '浅色' : '深色'))
const online = computed(() => !!store.stats)

async function onLogout() {
  await logout()
}
function openRelease() {
  const url = updateInfo.value?.url
  if (url) window.open(url, '_blank')
}
</script>

<template>
  <header class="topbar">
    <button class="icon-btn" style="display: none" @click="emit('toggle-sidebar')">菜单</button>
    <div class="crumb">
      <span class="crumb-group">{{ groupLabel }}</span>
      <span class="crumb-sep">/</span>
      <span class="crumb-title">{{ title }}</span>
    </div>
    <div class="spacer"></div>
    <div class="search" role="search" @click="openSearch()">
      <span>⌕</span>
      <span>搜索 IP、规则、事件…</span>
      <kbd>Ctrl K</kbd>
    </div>
    <span class="state" :title="xdp.title"><span class="dot" :class="xdp.dot"></span>{{ xdp.text }}</span>
    <span v-if="isReadonly" class="state warn" title="只读令牌：可查看全部数据，写操作会被服务端拒绝（403）">
      <span class="dot warn"></span>只读
    </span>
    <span v-if="updateInfo.available" class="state danger" :title="`发现新版本 v${updateInfo.latest}，点击查看发布说明`" style="cursor: pointer" @click="openRelease">
      <span class="dot danger"></span>可升级
    </span>
    <span class="topbar-divider"></span>
    <button class="icon-btn" toggles="refresh" title="刷新（R）" @click="refresh(true)">刷新</button>
    <button class="icon-btn" title="切换主题" @click="cycleTheme()">{{ themeLabel }}</button>
    <button v-if="online" class="icon-btn" title="退出登录" @click="onLogout()">退出</button>
  </header>
</template>
