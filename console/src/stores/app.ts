import { computed, reactive, ref } from 'vue'
import { api, demoMode as clientDemoMode, getToken, setToken } from '../api/client'
import type { Role, SeriesPoint, Stats } from '../api/types'

export const demoMode = () => clientDemoMode()

export const polls = ref(0)

/** 全局时间范围：所有趋势图共用，切换后立即重新拉取对应窗口的数据。 */
export type RangeKey = '15m' | '1h' | '24h'
export const RANGE_SECONDS: Record<RangeKey, number> = { '15m': 900, '1h': 3600, '24h': 86400 }
export const RANGE_LABEL: Record<RangeKey, string> = { '15m': '15 分钟', '1h': '1 小时', '24h': '24 小时' }
export const range = ref<RangeKey>('1h')
/** 曲线数据加载中（切换时间范围时用于显示过渡动画） */
export const seriesLoading = ref(false)
/** 版本检测结果：有新版本时右上角显示红点 */
export const updateInfo = ref<{ available: boolean; latest: string; url: string }>({ available: false, latest: '', url: '' })
export async function checkUpdate() {
  try {
    const r = await api.versionCheck()
    if (!r) return
    updateInfo.value = {
      available: !!r.update_available,
      latest: String(r.latest || ''),
      url: String(r.release_url || ''),
    }
  } catch { /* 离线环境忽略 */ }
}
export const rangeSeconds = computed(() => RANGE_SECONDS[range.value])
export const rangeLabel = computed(() => RANGE_LABEL[range.value])
export function setRange(next: RangeKey) {
  range.value = next
  refresh(true)
}
export const live = ref(false)
export const danger = ref(0)

const ROLE_KEY = 'xuanarmor-role'
/** 角色只用于控制台展示（隐藏/提示写操作），服务端仍对只读令牌返回 403。 */
function storedRole(): Role {
  try {
    return localStorage.getItem(ROLE_KEY) === 'readonly' ? 'readonly' : 'admin'
  } catch {
    return 'admin'
  }
}

export const store = reactive({
  stats: null as Stats | null,
  series: [] as SeriesPoint[],
  loading: true,
  lastUpdated: 0,
  autoRefresh: true,
  theme: localStorage.getItem('xuanarmor-theme') || 'system',
  /** 访问令牌角色（v0.5.0 RBAC-lite）：admin 可写，readonly 只读。 */
  role: storedRole(),
})

export const isReadonly = computed(() => store.role === 'readonly')
export function setRole(role: Role) {
  store.role = role
  try {
    localStorage.setItem(ROLE_KEY, role)
  } catch {
    /* 隐私模式下忽略持久化失败 */
  }
}

export const toasts = reactive<{ id: number; kind: string; title: string; desc?: string }[]>([])
let toastSeq = 0
export function toast(title: string, desc = '', kind: 'ok' | 'warn' | 'danger' | 'info' = 'ok') {
  const id = ++toastSeq
  toasts.push({ id, kind, title, desc })
  setTimeout(() => {
    const i = toasts.findIndex((t) => t.id === id)
    if (i >= 0) toasts.splice(i, 1)
  }, 3600)
}

export async function refresh(force = false) {
  if (!store.autoRefresh && !force) return
  if (force) seriesLoading.value = true
  try {
    const [stats, series] = await Promise.all([api.stats(), api.series(rangeSeconds.value)])
    store.stats = stats
    store.series = series
    store.lastUpdated = Date.now()
    live.value = api.isLive()
    danger.value = stats.danger_level
    polls.value++
  } catch (e) {
    // keep last known values; surface only when the user forced a refresh
    if (force) toast('刷新失败', String(e), 'danger')
  } finally {
    store.loading = false
    seriesLoading.value = false
  }
}

const themeMedia = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null

/** 把 system 解析成真实主题后写入 data-theme，CSS 只需 light/dark 两套。 */
export function initTheme() {
  const override = new URLSearchParams(location.search).get('theme')
  const wanted = override === 'dark' || override === 'light' ? override : store.theme
  const resolved = wanted === 'system' ? (themeMedia?.matches ? 'dark' : 'light') : wanted
  document.documentElement.dataset.theme = resolved
  window.dispatchEvent(new Event('xuanarmor:theme'))
}

if (themeMedia) {
  const onChange = () => { if (store.theme === 'system') initTheme() }
  themeMedia.addEventListener?.('change', onChange)
}
export function cycleTheme() {
  store.theme = store.theme === 'system' ? 'light' : store.theme === 'light' ? 'dark' : 'system'
  localStorage.setItem('xuanarmor-theme', store.theme)
  initTheme()
}

export const commandOpen = ref(false)
export const searchSeed = ref('')
export function openSearch(seed = '') {
  searchSeed.value = seed
  commandOpen.value = true
}

export async function logout() {
  setToken('')
  setRole('admin')
  // 清掉后端下发的 HttpOnly 会话 Cookie，否则刷新页面仍会被自动认证。
  try {
    await fetch('/api/auth/logout', { method: 'POST', credentials: 'same-origin' })
  } catch { /* 离线/演示模式忽略 */ }
  location.hash = '#/login'
}
export const tokenPresent = () => !!getToken()
