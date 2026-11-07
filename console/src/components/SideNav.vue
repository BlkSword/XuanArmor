<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { navGroups } from '../router'
import { store } from '../stores/app'
import { fmtCompact } from '../utils/format'

const appVersion = __APP_VERSION__

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ (e: 'navigate'): void }>()
const route = useRoute()
const active = computed(() => route.path)

const attackBadge = computed(() => {
  const dps = store.stats?.current_dps || 0
  if (!dps) return ''
  const c = fmtCompact(dps)
  return c.value + c.unit
})
</script>

<template>
  <aside class="sidebar" :class="{ open: props.open }">
    <div class="brand">
      <div class="brand-mark" aria-hidden="true">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 2.1 20.4 5.3v6.4c0 4-2.9 7.3-8.4 9.9-5.5-2.6-8.4-5.9-8.4-9.9V5.3Z"
                fill="currentColor" />
          <path d="M8.5 8.1 15.5 15.9M15.5 8.1 8.5 15.9" stroke="rgba(255,255,255,0.92)"
                stroke-width="1.9" stroke-linecap="round" fill="none" />
          <circle cx="12" cy="5.9" r="0.9" fill="rgba(255,255,255,0.95)" />
        </svg>
      </div>
      <div class="brand-name">XuanArmor</div>
      <div class="brand-ver">{{ appVersion }}</div>
    </div>
    <div class="side-scroll">
      <div v-for="g in navGroups" :key="g.label" class="nav-group">
        <div class="nav-group-label">{{ g.label }}</div>
        <router-link
          v-for="item in g.items"
          :key="item.path"
          :to="item.path"
          class="nav-item"
          :class="{ active: active === item.path }"
          @click="emit('navigate')"
        >
          <span>{{ item.label }}</span>
          <span v-if="item.path === '/attacks' && attackBadge" class="nav-badge">{{ attackBadge }}</span>
        </router-link>
      </div>
    </div>
  </aside>
</template>
