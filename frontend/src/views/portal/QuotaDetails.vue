<template>
  <div class="crc-page quota-details-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">限额详情</h1>
        <p class="crc-page-description">查看当前下游的请求窗口、Token 配额与访问白名单。</p>
      </div>
    </header>

    <div v-loading="loading" class="quota-details-content">
      <section class="quota-summary-grid" aria-label="限额摘要">
        <article v-if="data.request_quota" class="quota-summary-item crc-surface">
          <span>请求配额</span>
          <strong>{{ data.request_quota.used }} / {{ data.request_quota.limit }}</strong>
          <small>{{ data.request_quota.window_hours }} 小时窗口 · 剩余 {{ data.request_quota.remaining }}</small>
        </article>
        <article v-if="data.token_quota?.daily" class="quota-summary-item crc-surface">
          <span>每日 Token</span>
          <strong>{{ data.token_quota.daily.used.toLocaleString() }} / {{ data.token_quota.daily.limit.toLocaleString() }}</strong>
          <small>剩余 {{ data.token_quota.daily.remaining.toLocaleString() }}</small>
        </article>
        <article v-if="data.token_quota?.monthly" class="quota-summary-item crc-surface">
          <span>每月 Token</span>
          <strong>{{ data.token_quota.monthly.used.toLocaleString() }} / {{ data.token_quota.monthly.limit.toLocaleString() }}</strong>
          <small>剩余 {{ data.token_quota.monthly.remaining.toLocaleString() }}</small>
        </article>
      </section>

      <section v-if="data.request_quota" class="quota-detail-section">
        <div class="quota-detail-heading">
          <h2>请求配额</h2>
          <span>{{ data.request_quota.window_hours }} 小时滑动窗口</span>
        </div>
        <div class="quota-detail-metrics">
          <div><span>配额限制</span><strong>{{ data.request_quota.limit }}</strong></div>
          <div><span>已使用</span><strong>{{ data.request_quota.used }}</strong></div>
          <div><span>剩余</span><strong>{{ data.request_quota.remaining }}</strong></div>
        </div>
        <el-progress
          :percentage="formatPercentageTwoDecimals(data.request_quota.percentage)"
          :format="formatPercentageLabel"
          :color="getQuotaStatusColor(data.request_quota.percentage)"
        />
      </section>

      <section v-if="data.token_quota?.daily" class="quota-detail-section">
        <div class="quota-detail-heading">
          <h2>每日 Token 配额</h2>
          <span>当日累计</span>
        </div>
        <div class="quota-detail-metrics">
          <div><span>配额限制</span><strong>{{ data.token_quota.daily.limit.toLocaleString() }}</strong></div>
          <div><span>已使用</span><strong>{{ data.token_quota.daily.used.toLocaleString() }}</strong></div>
          <div><span>剩余</span><strong>{{ data.token_quota.daily.remaining.toLocaleString() }}</strong></div>
        </div>
        <el-progress
          :percentage="formatPercentageTwoDecimals(data.token_quota.daily.percentage)"
          :format="formatPercentageLabel"
          :color="getQuotaStatusColor(data.token_quota.daily.percentage)"
        />
      </section>

      <section v-if="data.token_quota?.monthly" class="quota-detail-section">
        <div class="quota-detail-heading">
          <h2>每月 Token 配额</h2>
          <span>本月累计</span>
        </div>
        <div class="quota-detail-metrics">
          <div><span>配额限制</span><strong>{{ data.token_quota.monthly.limit.toLocaleString() }}</strong></div>
          <div><span>已使用</span><strong>{{ data.token_quota.monthly.used.toLocaleString() }}</strong></div>
          <div><span>剩余</span><strong>{{ data.token_quota.monthly.remaining.toLocaleString() }}</strong></div>
        </div>
        <el-progress
          :percentage="formatPercentageTwoDecimals(data.token_quota.monthly.percentage)"
          :format="formatPercentageLabel"
          :color="getQuotaStatusColor(data.token_quota.monthly.percentage)"
        />
      </section>

      <section class="quota-detail-section">
        <div class="quota-detail-heading">
          <h2>模型白名单</h2>
          <span>{{ modelSectionHint }}</span>
        </div>
        <div v-if="displayModelSlugs.length > 0" class="quota-tag-list">
          <el-tag v-for="model in displayModelSlugs" :key="model">{{ model }}</el-tag>
        </div>
        <el-empty v-else :description="modelEmptyDescription" :image-size="60" />
      </section>

      <section class="quota-detail-section">
        <div class="quota-detail-heading">
          <h2>IP 白名单</h2>
          <span>{{ data.ip_allowlist.length > 0 ? '按来源地址限制' : '当前不限制来源地址' }}</span>
        </div>
        <div v-if="data.ip_allowlist.length > 0" class="quota-tag-list">
          <el-tag v-for="ip in data.ip_allowlist" :key="ip" type="info">{{ ip }}</el-tag>
        </div>
        <el-empty v-else description="无限制" :image-size="60" />
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'
import type { PortalQuota } from '@/types'
import { formatPercentageLabel, formatPercentageTwoDecimals } from '@/utils/percentage'
import {
  buildGatewayModelsEndpoint,
  extractGatewayModelSlugs
} from '@/utils/integration'
import { resolvePortalQuotaModelSlugs } from '@/utils/portalQuotaModels'

const loading = ref(false)
const data = ref<PortalQuota>({
  request_quota: undefined,
  token_quota: {
    daily: undefined,
    monthly: undefined
  },
  model_allowlist: [],
  ip_allowlist: []
})
const availableModelSlugs = ref<string[]>([])
const modelLoadError = ref('')

const displayModelSlugs = computed(() =>
  resolvePortalQuotaModelSlugs(data.value.model_allowlist, availableModelSlugs.value)
)

const allowlistIsEmpty = computed(() => data.value.model_allowlist.length === 0)

const modelSectionHint = computed(() => {
  if (!allowlistIsEmpty.value) {
    return '仅展示当前下游配置的模型白名单。'
  }

  if (availableModelSlugs.value.length > 0) {
    return '当前未配置白名单，下面展示全部可用模型。'
  }

  if (modelLoadError.value) {
    return '当前未配置白名单，但暂时无法读取全部可用模型。'
  }

  return '当前未配置白名单，下面展示全部可用模型。'
})

const modelEmptyDescription = computed(() => {
  if (!allowlistIsEmpty.value) {
    return '无限制'
  }

  if (modelLoadError.value) {
    return '未能读取全部可用模型'
  }

  return '未发现可用模型'
})

const getQuotaStatusColor = (percentage: number) => {
  if (percentage >= 90) return 'var(--crc-danger)'
  if (percentage >= 70) return 'var(--crc-warning)'
  return 'var(--crc-success)'
}

const loadData = async () => {
  try {
    loading.value = true
    modelLoadError.value = ''
    availableModelSlugs.value = []

    const response = await portalApi.getQuota()
    const responseData = response.data as PortalQuota
    data.value = {
      request_quota: responseData.request_quota,
      token_quota: responseData.token_quota,
      model_allowlist: responseData.model_allowlist || [],
      ip_allowlist: responseData.ip_allowlist || [],
      model_contexts: responseData.model_contexts
    }

    if (data.value.model_allowlist.length === 0) {
      const keyResponse = await portalApi.getKey()
      const portalKey = keyResponse.data.plaintext_key?.trim() ?? ''
      if (!portalKey) {
        modelLoadError.value = '当前下游没有可用秘钥，无法读取全部模型。'
        return
      }

      const modelsResponse = await fetch(
        buildGatewayModelsEndpoint(window.location.origin),
        {
          headers: {
            Authorization: 'Bearer ' + portalKey
          }
        }
      )

      if (!modelsResponse.ok) {
        modelLoadError.value = '网关模型接口返回 ' + modelsResponse.status
        return
      }

      const payload = await modelsResponse.json()
      availableModelSlugs.value = extractGatewayModelSlugs(payload)
      if (availableModelSlugs.value.length === 0) {
        modelLoadError.value = '未发现可用模型。'
      }
    }
  } catch (error) {
    ElMessage.error('加载数据失败')
    modelLoadError.value = error instanceof Error ? error.message : '加载模型失败'
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  loadData()
})
</script>

<style scoped>
.quota-details-page,
.quota-details-content {
  display: flex;
  flex-direction: column;
}

.quota-details-content {
  gap: 20px;
}

.quota-summary-grid {
  display: grid;
  gap: 16px;
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.quota-summary-item {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 18px;
}

.quota-summary-item span,
.quota-summary-item small,
.quota-detail-heading span,
.quota-detail-metrics span {
  color: var(--crc-text-muted);
  font-size: 12px;
  line-height: 1.5;
}

.quota-summary-item strong,
.quota-detail-heading h2,
.quota-detail-metrics strong {
  color: var(--crc-text-strong);
}

.quota-summary-item strong {
  font-size: 20px;
  overflow-wrap: anywhere;
}

.quota-detail-section {
  padding: 20px 0;
  border-top: 1px solid var(--crc-border);
}

.quota-detail-heading {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 16px;
}

.quota-detail-heading h2 {
  margin: 0;
  font-size: 16px;
}

.quota-detail-metrics {
  display: grid;
  gap: 12px;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  margin-bottom: 16px;
}

.quota-detail-metrics > div {
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

@media (max-width: 1023px) {
  .quota-summary-grid {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 767px) {
  .quota-detail-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .quota-detail-metrics {
    grid-template-columns: 1fr;
  }
}
</style>
