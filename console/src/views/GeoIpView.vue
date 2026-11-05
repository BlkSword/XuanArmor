<script setup lang="ts">
import { onMounted, ref } from 'vue'
import PanelCard from '../components/PanelCard.vue'
import ToggleSwitch from '../components/ToggleSwitch.vue'
import { api } from '../api/client'
import { toast } from '../stores/app'

const enabled = ref(false)
const defaultAction = ref<'pass' | 'drop'>('pass')
const countries = ref('CN, SG')
const allowCountries = ref('')
const asns = ref('')
const csvPath = ref('/etc/xuanarmor/geoip.csv')
const loading = ref(false)

onMounted(async () => {
  const cfg = await api.config()
  enabled.value = !!cfg.geoip_enabled
  defaultAction.value = cfg.geoip_default_action === 1 ? 'drop' : 'pass'
  const g = (cfg as unknown as { geoip?: Record<string, unknown> }).geoip || {}
  countries.value = (g.block_countries as string[] || []).join(', ')
  allowCountries.value = (g.allow_countries as string[] || []).join(', ')
  asns.value = (g.block_asns as string[] || []).join(', ')
  if (g.country_blocks_csv) csvPath.value = String(g.country_blocks_csv)
})
async function save() {
  loading.value = true
  try {
    await api.patchConfig({
      geoip_enabled: enabled.value,
      geoip_default_action: defaultAction.value === 'drop' ? 1 : 0,
    })
    toast('GeoIP 策略已保存', enabled.value ? '运行时生效' : '已停用', 'ok')
  } finally {
    loading.value = false
  }
}
async function reload() {
  loading.value = true
  try {
    await api.reloadGeoIp()
    toast('GeoIP CSV 已重载', csvPath.value, 'ok')
  } catch (e) {
    toast('重载失败', String(e), 'danger')
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="page">
    <div class="page-head">
      <div class="page-title">GeoIP 与情报</div>
      <div class="page-meta">按国家/ASN 放行或阻断</div>
      <div class="page-actions">
        <span class="state" :class="enabled ? 'ok' : ''"><span class="dot" :class="enabled ? 'ok' : ''"></span>{{ enabled ? '已启用' : '已停用' }}</span>
        <button class="btn btn-sm" :disabled="loading" @click="reload">重载 CSV</button>
        <button class="btn btn-primary btn-sm" @click="save">保存</button>
      </div>
    </div>

    <div class="grid grid-main-side">
      <PanelCard title="GeoIP / ASN 策略" sub="按国家/地区或 ASN 决定放行或阻断">
        <div class="setting-row">
          <div class="sr-main">
            <div class="sr-title">启用 GeoIP 匹配</div>
            <div class="sr-desc">关闭时数据面跳过 GeoIP 表（零开销）</div>
          </div>
          <ToggleSwitch v-model="enabled" />
        </div>
        <div class="setting-row">
          <div class="sr-main">
            <div class="sr-title">默认动作</div>
            <div class="sr-desc">未命中 allow 列表时的行为；allow 为空且默认 drop 会拦截所有非白名单流量</div>
          </div>
          <div class="segmented">
            <button :class="{ active: defaultAction === 'pass' }" @click="defaultAction = 'pass'">PASS</button>
            <button :class="{ active: defaultAction === 'drop' }" @click="defaultAction = 'drop'">DROP</button>
          </div>
        </div>
        <div class="grid grid-2" style="margin-top: 12px">
          <div class="field"><label class="field-label">阻断国家/地区（ISO 代码，逗号分隔）</label><input v-model="countries" class="input mono" placeholder="RU, KP" /></div>
          <div class="field"><label class="field-label">放行国家/地区（默认 drop 时生效）</label><input v-model="allowCountries" class="input mono" placeholder="CN, SG" /></div>
          <div class="field"><label class="field-label">阻断 ASN</label><input v-model="asns" class="input mono" placeholder="4134, 4837" /></div>
          <div class="field"><label class="field-label">国家 CSV 路径</label><input v-model="csvPath" class="input mono" /></div>
        </div>
        <div class="panel-note" style="border-top: 0; padding: 10px 0 0">
          CSV 格式 <span class="mono">network,country_iso</span>，支持 IPv4/IPv6 CIDR；最多写入 4096 条 LPM 表项，超出后按文件顺序截断。
        </div>
      </PanelCard>

    </div>

  </div>
</template>
