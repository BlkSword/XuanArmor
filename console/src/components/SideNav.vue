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
          <path d="M12 1.6 21.6 7.2v9.6L12 22.4 2.4 16.8V7.2Z" fill="currentColor" />
          <path d="M12 1.6 21.6 7.2v9.6L12 22.4 2.4 16.8V7.2Z" fill="none"
                stroke="rgba(255,255,255,0.35)" stroke-width="0.7" />
          <path d="M5.2 7.6 8.4 6.1l10.4 10.3-3.2 1.5Z" fill="rgba(255,255,255,0.95)" />
          <path d="M18.8 7.6 15.6 6.1 5.2 16.4l3.2 1.5Z" fill="rgba(255,255,255,0.55)" />
          <circle cx="12" cy="12" r="1.5" fill="#0A1E36" stroke="rgba(255,255,255,0.9)" stroke-width="0.6" />
        </svg>
      </div>
      <div class="brand-name">XuanArmor</div>
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
    <div class="side-foot">
      <div class="side-ver mono">v{{ appVersion }}</div>
    </div>
  </aside>
</template>
