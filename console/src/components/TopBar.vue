<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { cycleTheme, isReadonly, refresh, store, openSearch, logout, updateInfo } from '../stores/app'
import { navGroups } from '../router'

const emit = defineEmits<{ (e: 'toggle-sidebar'): void }>()
const route = useRoute()

const groupLabel = computed(() => navGroups.find((g) => g.items.some((i) => i.path === route.path))?.label || '控制台')
const title = computed(() => (route.meta.title as string) || 'XuanArmor')

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
