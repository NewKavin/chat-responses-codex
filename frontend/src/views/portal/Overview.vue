<template>
  <div class="overview-container">
    <!-- 配额概览卡片 -->
    <el-card class="summary-card">
      <template #header>
        <div class="card-header">
          <h2>配额使用概览</h2>
          <span class="card-subtitle">实时刷新</span>
        </div>
      </template>
      <el-row :gutter="20">
        <el-col :span="8" v-if="data.quota_summary.request_quota">
          <el-card shadow="hover" class="quota-tile">
            <el-statistic
              :title="`请求配额 (${data.quota_summary.request_quota.window_hours}h)`"
              :value="data.quota_summary.request_quota.used"
              :value-style="{ color: getQuotaColor(data.quota_summary.request_quota.percentage) }"
            >
              <template #suffix>/ {{ data.quota_summary.request_quota.limit }}</template>
            </el-statistic>
            <el-progress
              :percentage="data.quota_summary.request_quota.percentage"
              :color="getQuotaColor(data.quota_summary.request_quota.percentage)"
              :show-text="false"
              :stroke-width="6"
              class="progress"
            />
          </el-card>
        </el-col>
        <el-col :span="8" v-if="data.quota_summary.token_daily">
          <el-card shadow="hover" class="quota-tile">
            <el-statistic
              title="每日 Token"
              :value="data.quota_summary.token_daily.used"
              :value-style="{ color: getQuotaColor(data.quota_summary.token_daily.percentage) }"
            >
              <template #suffix>/ {{ formatCompact(data.quota_summary.token_daily.limit) }}</template>
            </el-statistic>
            <el-progress
              :percentage="data.quota_summary.token_daily.percentage"
              :color="getQuotaColor(data.quota_summary.token_daily.percentage)"
              :show-text="false"
              :stroke-width="6"
              class="progress"
            />
          </el-card>
        </el-col>
        <el-col :span="8" v-if="data.quota_summary.token_monthly">
          <el-card shadow="hover" class="quota-tile">
            <el-statistic
              title="每月 Token"
              :value="data.quota_summary.token_monthly.used"
              :value-style="{ color: getQuotaColor(data.quota_summary.token_monthly.percentage) }"
            >
              <template #suffix>/ {{ formatCompact(data.quota_summary.token_monthly.limit) }}</template>
            </el-statistic>
            <el-progress
              :percentage="data.quota_summary.token_monthly.percentage"
              :color="getQuotaColor(data.quota_summary.token_monthly.percentage)"
              :show-text="false"
              :stroke-width="6"
              class="progress"
            />
          </el-card>
        </el-col>
      </el-row>
    </el-card>

    <!-- 统计摘要 -->
    <el-row :gutter="20" class="stats-row">
      <el-col :span="12">
        <el-card>
          <template #header><div class="card-header"><h3>Token 使用</h3></div></template>
          <el-descriptions :column="1" border>
            <el-descriptions-item label="今日使用">{{ formatCompact(data.token_summary.today) }}</el-descriptions-item>
            <el-descriptions-item label="本月使用">{{ formatCompact(data.token_summary.this_month) }}</el-descriptions-item>
          </el-descriptions>
        </el-card>
      </el-col>
      <el-col :span="12">
        <el-card>
          <template #header><div class="card-header"><h3>模型概况</h3></div></template>
          <el-descriptions :column="1" border>
            <el-descriptions-item label="可用模型">{{ data.model_summary.total_models }}</el-descriptions-item>
            <el-descriptions-item label="活跃模型">{{ data.model_summary.active_models }}</el-descriptions-item>
          </el-descriptions>
        </el-card>
      </el-col>
    </el-row>

    <!-- 配额明细(可折叠,原限额详情页内容) -->
    <el-card class="detail-card" v-loading="quotaLoading">
      <template #header>
        <div class="card-header">
          <h3>配额明细与白名单</h3>
          <span class="card-subtitle">来自限额配置</span>
        </div>
      </template>
      <el-collapse v-model="activeDetail" class="detail-collapse">
        <el-collapse-item title="请求配额" name="request" v-if="quotaData.request_quota">
          <el-descriptions :column="2" border>
            <el-descriptions-item label="时间窗口">{{ quotaData.request_quota.window_hours }} 小时</el-descriptions-item>
            <el-descriptions-item label="配额限制">{{ quotaData.request_quota.limit }}</el-descriptions-item>
            <el-descriptions-item label="已使用">{{ quotaData.request_quota.used }}</el-descriptions-item>
            <el-descriptions-item label="剩余">{{ quotaData.request_quota.remaining }}</el-descriptions-item>
            <el-descriptions-item label="使用率" :span="2">
              <el-progress
                :percentage="formatPct(quotaData.request_quota.percentage)"
                :color="getQuotaColor(quotaData.request_quota.percentage)"
              />
            </el-descriptions-item>
          </el-descriptions>
        </el-collapse-item>
        <el-collapse-item title="每日 Token 配额" name="daily" v-if="quotaData.token_quota?.daily">
          <el-descriptions :column="2" border>
            <el-descriptions-item label="配额限制">{{ quotaData.token_quota.daily.limit.toLocaleString() }}</el-descriptions-item>
            <el-descriptions-item label="已使用">{{ quotaData.token_quota.daily.used.toLocaleString() }}</el-descriptions-item>
            <el-descriptions-item label="剩余">{{ quotaData.token_quota.daily.remaining.toLocaleString() }}</el-descriptions-item>
            <el-descriptions-item label="使用率">
              <el-progress
                :percentage="formatPct(quotaData.token_quota.daily.percentage)"
                :color="getQuotaColor(quotaData.token_quota.daily.percentage)"
              />
            </el-descriptions-item>
          </el-descriptions>
        </el-collapse-item>
        <el-collapse-item title="每月 Token 配额" name="monthly" v-if="quotaData.token_quota?.monthly">
          <el-descriptions :column="2" border>
            <el-descriptions-item label="配额限制">{{ quotaData.token_quota.monthly.limit.toLocaleString() }}</el-descriptions-item>
            <el-descriptions-item label="已使用">{{ quotaData.token_quota.monthly.used.toLocaleString() }}</el-descriptions-item>
            <el-descriptions-item label="剩余">{{ quotaData.token_quota.monthly.remaining.toLocaleString() }}</el-descriptions-item>
            <el-descriptions-item label="使用率">
              <el-progress
                :percentage="formatPct(quotaData.token_quota.monthly.percentage)"
                :color="getQuotaColor(quotaData.token_quota.monthly.percentage)"
              />
            </el-descriptions-item>
          </el-descriptions>
        </el-collapse-item>
        <el-collapse-item name="models">
          <template #title>
            <div class="collapse-title">
              <span>模型白名单</span>
              <span class="collapse-hint">{{ modelSectionHint }}</span>
            </div>
          </template>
          <template v-if="displayModelSlugs.length > 0">
            <el-tag v-for="model in displayModelSlugs" :key="model" class="tag-chip">{{ model }}</el-tag>
          </template>
          <el-empty v-else :description="modelEmptyDescription" :image-size="60" />
        </el-collapse-item>
        <el-collapse-item title="IP 白名单" name="ips">
          <template v-if="quotaData.ip_allowlist.length > 0">
            <el-tag v-for="ip in quotaData.ip_allowlist" :key="ip" type="info" class="tag-chip">{{ ip }}</el-tag>
          </template>
          <el-empty v-else description="无限制" :image-size="60" />
        </el-collapse-item>
      </el-collapse>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'
import type { PortalOverview, PortalQuota } from '@/types'
import { formatCompactNumber } from '@/utils/numberFormat'
import { formatPercentageTwoDecimals } from '@/utils/percentage'
import {
  buildGatewayModelsEndpoint,
  extractGatewayModelSlugs
} from '@/utils/integration'
import { resolvePortalQuotaModelSlugs } from '@/utils/portalQuotaModels'

const data = ref<PortalOverview>({
  quota_summary: { request_quota: undefined, token_daily: undefined, token_monthly: undefined },
  token_summary: { today: 0, this_month: 0 },
  model_summary: { total_models: 0, active_models: 0 }
})

const quotaData = ref<PortalQuota>({
  request_quota: undefined,
  token_quota: { daily: undefined, monthly: undefined },
  model_allowlist: [],
  ip_allowlist: []
})
const quotaLoading = ref(false)
const availableModelSlugs = ref<string[]>([])
const modelLoadError = ref('')
const activeDetail = ref<string[]>([])

const displayModelSlugs = computed(() =>
  resolvePortalQuotaModelSlugs(quotaData.value.model_allowlist, availableModelSlugs.value)
)
const allowlistIsEmpty = computed(() => quotaData.value.model_allowlist.length === 0)
const modelSectionHint = computed(() => {
  if (!allowlistIsEmpty.value) return '仅展示配置的模型白名单'
  if (availableModelSlugs.value.length > 0) return '未配置白名单,展示全部可用模型'
  if (modelLoadError.value) return '未配置白名单,暂无法读取全部模型'
  return '未配置白名单,展示全部可用模型'
})
const modelEmptyDescription = computed(() => {
  if (!allowlistIsEmpty.value) return '无限制'
  if (modelLoadError.value) return '未能读取全部可用模型'
  return '未发现可用模型'
})

const getQuotaColor = (p: number) => (p >= 90 ? '#f56c6c' : p >= 70 ? '#e6a23c' : '#67c23a')
const formatCompact = (v: number) => formatCompactNumber(v)
const formatPct = (v: number) => formatPercentageTwoDecimals(v)

let refreshTimer: number | null = null

const loadOverview = async () => {
  try {
    const response = await portalApi.getOverview()
    data.value = response.data
  } catch {
    ElMessage.error('加载概览失败')
  }
}

const loadQuotaDetail = async () => {
  try {
    quotaLoading.value = true
    modelLoadError.value = ''
    availableModelSlugs.value = []
    const response = await portalApi.getQuota()
    const r = response.data as any
    quotaData.value = {
      request_quota: r.request_quota,
      token_quota: r.token_quota,
      model_allowlist: r.model_allowlist || [],
      ip_allowlist: r.ip_allowlist || []
    }
    if (quotaData.value.model_allowlist.length === 0) {
      const keyResponse = await portalApi.getKey()
      const portalKey = keyResponse.data.plaintext_key?.trim() ?? ''
      if (!portalKey) { modelLoadError.value = '当前下游没有可用秘钥,无法读取全部模型。'; return }
      const modelsResponse = await fetch(buildGatewayModelsEndpoint(window.location.origin), {
        headers: { Authorization: `Bearer ${portalKey}` }
      })
      if (!modelsResponse.ok) { modelLoadError.value = `网关模型接口返回 ${modelsResponse.status}`; return }
      const payload = await modelsResponse.json()
      availableModelSlugs.value = extractGatewayModelSlugs(payload)
      if (availableModelSlugs.value.length === 0) modelLoadError.value = '未发现可用模型。'
    }
  } catch (error) {
    ElMessage.error('加载限额详情失败')
    modelLoadError.value = error instanceof Error ? error.message : '加载失败'
  } finally {
    quotaLoading.value = false
  }
}

onMounted(() => {
  loadOverview()
  loadQuotaDetail()
  refreshTimer = window.setInterval(() => { loadOverview() }, 5000)
})
onUnmounted(() => {
  if (refreshTimer !== null) { clearInterval(refreshTimer); refreshTimer = null }
})
</script>

<style scoped>
.overview-container { padding: 20px; display: flex; flex-direction: column; gap: 20px; }
.card-header { display: flex; align-items: baseline; justify-content: space-between; }
.card-header h2, .card-header h3 { margin: 0; font-size: 16px; color: #303133; }
.card-subtitle { font-size: 12px; color: #909399; }
.quota-tile { height: 100%; }
.progress { margin-top: 10px; }
.stats-row { margin: 0; }
.detail-card { margin: 0; }
.detail-collapse { border: none; }
.detail-collapse :deep(.el-collapse-item__header) { font-weight: 600; color: #303133; }
.collapse-title { display: flex; align-items: baseline; gap: 10px; }
.collapse-hint { font-size: 12px; color: #909399; font-weight: 400; }
.tag-chip { margin: 0 8px 8px 0; }
:deep(.el-statistic__content) { font-size: 26px; }
:deep(.el-card__header) { padding: 14px 20px; }
</style>
