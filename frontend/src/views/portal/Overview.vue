<template>
  <div class="crc-page portal-overview-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">配额与访问概览</h1>
        <p class="crc-page-description">查看请求与 Token 配额、模型范围以及当前下游的访问限制。</p>
      </div>
      <span class="overview-refresh-label">每 5 秒自动刷新</span>
    </header>

    <section class="quota-summary-grid" aria-label="配额总览">
      <template v-if="showSummarySkeleton">
        <article
          v-for="index in 3"
          :key="index"
          class="quota-summary-item crc-surface quota-summary-item--skeleton"
          aria-hidden="true"
        >
          <el-skeleton animated>
            <template #template>
              <el-skeleton-item variant="text" style="width: 44%" />
              <el-skeleton-item variant="h1" style="width: 64%; margin-top: 14px" />
              <el-skeleton-item variant="text" style="width: 100%; margin-top: 16px" />
            </template>
          </el-skeleton>
        </article>
      </template>

      <article v-if="data.quota_summary.request_quota" class="quota-summary-item crc-surface">
        <div class="quota-summary-head">
          <span class="quota-summary-label">请求配额</span>
          <span class="quota-summary-meta">{{ data.quota_summary.request_quota.window_hours }} 小时窗口</span>
        </div>
        <div class="quota-summary-value-row">
          <strong>{{ data.quota_summary.request_quota.used }}</strong>
          <span>/ {{ data.quota_summary.request_quota.limit }}</span>
        </div>
        <el-progress
          :percentage="formatPct(data.quota_summary.request_quota.percentage)"
          :color="getQuotaStatusColor(data.quota_summary.request_quota.percentage)"
          :show-text="false"
          :stroke-width="6"
        />
        <p class="quota-summary-foot">
          剩余 {{ data.quota_summary.request_quota.remaining }} · {{ formatPct(data.quota_summary.request_quota.percentage) }}%
        </p>
      </article>

      <article v-if="data.quota_summary.token_daily" class="quota-summary-item crc-surface">
        <div class="quota-summary-head">
          <span class="quota-summary-label">每日 Token</span>
          <span class="quota-summary-meta">今日累计</span>
        </div>
        <div class="quota-summary-value-row">
          <strong>{{ formatCompact(data.quota_summary.token_daily.used) }}</strong>
          <span>/ {{ formatCompact(data.quota_summary.token_daily.limit) }}</span>
        </div>
        <el-progress
          :percentage="formatPct(data.quota_summary.token_daily.percentage)"
          :color="getQuotaStatusColor(data.quota_summary.token_daily.percentage)"
          :show-text="false"
          :stroke-width="6"
        />
        <p class="quota-summary-foot">
          剩余 {{ formatCompact(data.quota_summary.token_daily.remaining) }} · {{ formatPct(data.quota_summary.token_daily.percentage) }}%
        </p>
      </article>

      <article v-if="data.quota_summary.token_monthly" class="quota-summary-item crc-surface">
        <div class="quota-summary-head">
          <span class="quota-summary-label">每月 Token</span>
          <span class="quota-summary-meta">本月累计</span>
        </div>
        <div class="quota-summary-value-row">
          <strong>{{ formatCompact(data.quota_summary.token_monthly.used) }}</strong>
          <span>/ {{ formatCompact(data.quota_summary.token_monthly.limit) }}</span>
        </div>
        <el-progress
          :percentage="formatPct(data.quota_summary.token_monthly.percentage)"
          :color="getQuotaStatusColor(data.quota_summary.token_monthly.percentage)"
          :show-text="false"
          :stroke-width="6"
        />
        <p class="quota-summary-foot">
          剩余 {{ formatCompact(data.quota_summary.token_monthly.remaining) }} · {{ formatPct(data.quota_summary.token_monthly.percentage) }}%
        </p>
      </article>
    </section>

    <section class="overview-meta-grid">
      <article class="overview-meta-item crc-surface">
        <span class="overview-meta-label">今日 Token 使用</span>
        <strong>{{ formatCompact(data.token_summary.today) }}</strong>
      </article>
      <article class="overview-meta-item crc-surface">
        <span class="overview-meta-label">本月 Token 使用</span>
        <strong>{{ formatCompact(data.token_summary.this_month) }}</strong>
      </article>
      <article class="overview-meta-item crc-surface">
        <span class="overview-meta-label">可用模型</span>
        <strong>{{ data.model_summary.total_models }}</strong>
      </article>
      <article class="overview-meta-item crc-surface">
        <span class="overview-meta-label">活跃模型</span>
        <strong>{{ data.model_summary.active_models }}</strong>
      </article>
    </section>

    <section class="quota-details-shell" v-loading="quotaLoading">
      <div class="quota-details-head">
        <h2>配额明细与白名单</h2>
        <p>根据当前限额配置展示请求窗口、Token 配额以及可访问范围。</p>
      </div>

      <el-collapse v-model="activeDetail" class="quota-detail-collapse">
        <el-collapse-item name="request" v-if="quotaData.request_quota">
          <template #title>
            <div class="quota-detail-title-row">
              <span>请求配额</span>
              <span class="quota-detail-title-meta">{{ quotaData.request_quota.window_hours }} 小时滑动窗口</span>
            </div>
          </template>
          <section class="quota-detail-section">
            <div class="quota-detail-metrics">
              <div class="quota-detail-metric">
                <span>配额限制</span>
                <strong>{{ quotaData.request_quota.limit }}</strong>
              </div>
              <div class="quota-detail-metric">
                <span>已使用</span>
                <strong>{{ quotaData.request_quota.used }}</strong>
              </div>
              <div class="quota-detail-metric">
                <span>剩余</span>
                <strong>{{ quotaData.request_quota.remaining }}</strong>
              </div>
            </div>
            <el-progress
              :percentage="formatPct(quotaData.request_quota.percentage)"
              :color="getQuotaStatusColor(quotaData.request_quota.percentage)"
            />
          </section>
        </el-collapse-item>

        <el-collapse-item name="daily" v-if="quotaData.token_quota?.daily">
          <template #title>
            <div class="quota-detail-title-row">
              <span>每日 Token 配额</span>
              <span class="quota-detail-title-meta">当日累计</span>
            </div>
          </template>
          <section class="quota-detail-section">
            <div class="quota-detail-metrics">
              <div class="quota-detail-metric">
                <span>配额限制</span>
                <strong>{{ quotaData.token_quota.daily.limit.toLocaleString() }}</strong>
              </div>
              <div class="quota-detail-metric">
                <span>已使用</span>
                <strong>{{ quotaData.token_quota.daily.used.toLocaleString() }}</strong>
              </div>
              <div class="quota-detail-metric">
                <span>剩余</span>
                <strong>{{ quotaData.token_quota.daily.remaining.toLocaleString() }}</strong>
              </div>
            </div>
            <el-progress
              :percentage="formatPct(quotaData.token_quota.daily.percentage)"
              :color="getQuotaStatusColor(quotaData.token_quota.daily.percentage)"
            />
          </section>
        </el-collapse-item>

        <el-collapse-item name="monthly" v-if="quotaData.token_quota?.monthly">
          <template #title>
            <div class="quota-detail-title-row">
              <span>每月 Token 配额</span>
              <span class="quota-detail-title-meta">本月累计</span>
            </div>
          </template>
          <section class="quota-detail-section">
            <div class="quota-detail-metrics">
              <div class="quota-detail-metric">
                <span>配额限制</span>
                <strong>{{ quotaData.token_quota.monthly.limit.toLocaleString() }}</strong>
              </div>
              <div class="quota-detail-metric">
                <span>已使用</span>
                <strong>{{ quotaData.token_quota.monthly.used.toLocaleString() }}</strong>
              </div>
              <div class="quota-detail-metric">
                <span>剩余</span>
                <strong>{{ quotaData.token_quota.monthly.remaining.toLocaleString() }}</strong>
              </div>
            </div>
            <el-progress
              :percentage="formatPct(quotaData.token_quota.monthly.percentage)"
              :color="getQuotaStatusColor(quotaData.token_quota.monthly.percentage)"
            />
          </section>
        </el-collapse-item>

        <el-collapse-item name="models">
          <template #title>
            <div class="quota-detail-title-row">
              <span>模型白名单</span>
              <span class="quota-detail-title-meta">{{ modelSectionHint }}</span>
            </div>
          </template>
          <section class="quota-detail-section">
            <div v-if="displayModelSlugs.length > 0" class="quota-tag-list">
              <el-tag v-for="model in displayModelSlugs" :key="model" class="quota-tag">{{ model }}</el-tag>
            </div>
            <el-empty v-else :description="modelEmptyDescription" :image-size="60" />
          </section>
        </el-collapse-item>

        <el-collapse-item name="ips">
          <template #title>
            <div class="quota-detail-title-row">
              <span>IP 白名单</span>
              <span class="quota-detail-title-meta">{{ quotaData.ip_allowlist.length > 0 ? '按来源地址限制' : '当前不限制来源地址' }}</span>
            </div>
          </template>
          <section class="quota-detail-section">
            <div v-if="quotaData.ip_allowlist.length > 0" class="quota-tag-list">
              <el-tag v-for="ip in quotaData.ip_allowlist" :key="ip" type="info" class="quota-tag">{{ ip }}</el-tag>
            </div>
            <el-empty v-else description="无限制" :image-size="60" />
          </section>
        </el-collapse-item>
      </el-collapse>
    </section>
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
const activeDetail = ref(['request', 'daily', 'monthly', 'models', 'ips'])
const overviewLoaded = ref(false)

const hasQuotaSummary = computed(() =>
  Boolean(
    data.value.quota_summary.request_quota ||
      data.value.quota_summary.token_daily ||
      data.value.quota_summary.token_monthly
  )
)

const showSummarySkeleton = computed(() => !hasQuotaSummary.value && !overviewLoaded.value)

const displayModelSlugs = computed(() =>
  resolvePortalQuotaModelSlugs(quotaData.value.model_allowlist, availableModelSlugs.value)
)
const allowlistIsEmpty = computed(() => quotaData.value.model_allowlist.length === 0)
const modelSectionHint = computed(() => {
  if (!allowlistIsEmpty.value) return '仅展示配置的模型白名单'
  if (availableModelSlugs.value.length > 0) return '未配置白名单，当前展示全部可用模型'
  if (modelLoadError.value) return '未配置白名单，暂时无法读取全部模型'
  return '未配置白名单，当前展示全部可用模型'
})
const modelEmptyDescription = computed(() => {
  if (!allowlistIsEmpty.value) return '无限制'
  if (modelLoadError.value) return '未能读取全部可用模型'
  return '未发现可用模型'
})

const getQuotaStatusColor = (percentage: number) => {
  if (percentage >= 90) return 'var(--crc-danger)'
  if (percentage >= 70) return 'var(--crc-warning)'
  return 'var(--crc-success)'
}

const formatCompact = (value: number) => formatCompactNumber(value)
const formatPct = (value: number) => formatPercentageTwoDecimals(value)

let refreshTimer: number | null = null

const loadOverview = async () => {
  try {
    const response = await portalApi.getOverview()
    data.value = response.data
  } catch {
    ElMessage.error('加载概览失败')
  } finally {
    overviewLoaded.value = true
  }
}

const loadQuotaDetail = async () => {
  try {
    quotaLoading.value = true
    modelLoadError.value = ''
    availableModelSlugs.value = []
    const response = await portalApi.getQuota()
    const payload = response.data as PortalQuota
    quotaData.value = {
      request_quota: payload.request_quota,
      token_quota: payload.token_quota,
      model_allowlist: payload.model_allowlist || [],
      ip_allowlist: payload.ip_allowlist || [],
      model_contexts: payload.model_contexts
    }
    if (quotaData.value.model_allowlist.length === 0) {
      const keyResponse = await portalApi.getKey()
      const portalKey = keyResponse.data.plaintext_key?.trim() ?? ''
      if (!portalKey) {
        modelLoadError.value = '当前下游没有可用秘钥，无法读取全部模型。'
        return
      }

      const modelsResponse = await fetch(buildGatewayModelsEndpoint(window.location.origin), {
        headers: { Authorization: 'Bearer ' + portalKey }
      })

      if (!modelsResponse.ok) {
        modelLoadError.value = '网关模型接口返回 ' + modelsResponse.status
        return
      }

      const modelsPayload = await modelsResponse.json()
      availableModelSlugs.value = extractGatewayModelSlugs(modelsPayload)
      if (availableModelSlugs.value.length === 0) {
        modelLoadError.value = '未发现可用模型。'
      }
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
  refreshTimer = window.setInterval(() => {
    loadOverview()
  }, 5000)
})

onUnmounted(() => {
  if (refreshTimer !== null) {
    clearInterval(refreshTimer)
    refreshTimer = null
  }
})
</script>

<style scoped>
.portal-overview-page {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.overview-refresh-label {
  color: var(--crc-text-muted);
  font-size: 12px;
  line-height: 1.5;
}

.quota-summary-grid {
  display: grid;
  gap: 16px;
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.quota-summary-item {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 18px;
  transition: transform var(--crc-duration) var(--crc-ease-out),
    box-shadow var(--crc-duration) var(--crc-ease-out),
    border-color var(--crc-duration) var(--crc-ease-out);
}

.quota-summary-item:hover {
  border-color: var(--crc-border-strong);
  box-shadow: var(--crc-shadow-md);
  transform: translateY(-2px);
}

@keyframes quota-card-in {
  from {
    opacity: 0;
    transform: translateY(10px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.quota-summary-grid .quota-summary-item {
  animation: quota-card-in var(--crc-duration-slow) var(--crc-ease-out) backwards;
}

.quota-summary-grid .quota-summary-item:nth-child(2) {
  animation-delay: 80ms;
}

.quota-summary-grid .quota-summary-item:nth-child(3) {
  animation-delay: 160ms;
}

.quota-summary-item--skeleton {
  pointer-events: none;
  animation: none;
}

.quota-summary-item :deep(.el-progress-bar__inner) {
  position: relative;
  overflow: hidden;
}

.quota-summary-item :deep(.el-progress-bar__inner)::after {
  content: '';
  position: absolute;
  inset: 0;
  background: linear-gradient(
    105deg,
    transparent 30%,
    rgb(255 255 255 / 38%) 50%,
    transparent 70%
  );
  animation: quota-progress-sheen 2.8s var(--crc-ease) infinite;
  transform: translateX(-100%);
}

@keyframes quota-progress-sheen {
  55%,
  100% {
    transform: translateX(100%);
  }
}

.quota-summary-head,
.quota-summary-value-row,
.quota-detail-title-row,
.quota-details-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
}

.quota-summary-label,
.quota-summary-value-row strong,
.quota-details-head h2,
.quota-detail-metric strong {
  color: var(--crc-text-strong);
}

.quota-summary-label {
  font-size: 14px;
  font-weight: 600;
}

.quota-summary-meta,
.quota-summary-value-row span,
.quota-summary-foot,
.quota-details-head p,
.quota-detail-title-meta,
.quota-detail-metric span {
  color: var(--crc-text-muted);
}

.quota-summary-meta,
.quota-summary-foot,
.quota-details-head p,
.quota-detail-title-meta,
.quota-detail-metric span {
  font-size: 12px;
  line-height: 1.5;
}

.quota-summary-value-row strong {
  font-size: 28px;
  line-height: 1;
}

.overview-meta-grid {
  display: grid;
  gap: 12px;
  grid-template-columns: repeat(4, minmax(0, 1fr));
}

.overview-meta-item {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 16px 18px;
  transition: transform var(--crc-duration) var(--crc-ease-out),
    box-shadow var(--crc-duration) var(--crc-ease-out);
}

.overview-meta-item:hover {
  box-shadow: var(--crc-shadow-sm);
  transform: translateY(-1px);
}

.overview-meta-label {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.overview-meta-item strong {
  color: var(--crc-text-strong);
  font-size: 18px;
}

.quota-details-shell {
  padding-top: 4px;
}

.quota-details-head {
  margin-bottom: 16px;
}

.quota-details-head h2 {
  margin: 0;
  font-size: 16px;
}

.quota-detail-collapse {
  border-top: 1px solid var(--crc-border);
  border-bottom: 1px solid var(--crc-border);
}

.quota-detail-collapse :deep(.el-collapse-item__header) {
  min-height: 54px;
  padding: 0 4px;
  color: var(--crc-text-strong);
  background: transparent;
}

.quota-detail-collapse :deep(.el-collapse-item__wrap) {
  background: transparent;
}

.quota-detail-section {
  padding: 4px 0 20px;
}

.quota-detail-metrics {
  display: grid;
  gap: 12px;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  margin-bottom: 16px;
}

.quota-detail-metric {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 14px 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-surface);
}

.quota-tag-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.quota-tag {
  margin: 0;
}

@media (max-width: 1023px) {
  .quota-summary-grid {
    grid-template-columns: 1fr;
  }

  .overview-meta-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 767px) {
  .quota-summary-head,
  .quota-summary-value-row,
  .quota-detail-title-row,
  .quota-details-head {
    align-items: flex-start;
    flex-direction: column;
  }

  .overview-meta-grid,
  .quota-detail-metrics {
    grid-template-columns: 1fr;
  }

  .overview-refresh-label {
    display: none;
  }
}
</style>
