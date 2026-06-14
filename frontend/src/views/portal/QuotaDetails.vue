<template>
  <div class="quota-details-container">
    <el-card v-loading="loading">
      <template #header>
        <h2>限额详情</h2>
      </template>

      <div class="sections">
        <!-- 请求配额 -->
        <div class="section" v-if="data.request_quota">
          <h3>请求配额（滑动窗口）</h3>
          <el-descriptions :column="2" border>
            <el-descriptions-item label="时间窗口">
              {{ data.request_quota.window_hours }} 小时
            </el-descriptions-item>
            <el-descriptions-item label="配额限制">
              {{ data.request_quota.limit }}
            </el-descriptions-item>
            <el-descriptions-item label="已使用">
              {{ data.request_quota.used }}
            </el-descriptions-item>
            <el-descriptions-item label="剩余">
              {{ data.request_quota.remaining }}
            </el-descriptions-item>
            <el-descriptions-item label="使用率" :span="2">
              <el-progress
                :percentage="formatPercentageTwoDecimals(data.request_quota.percentage)"
                :format="formatPercentageLabel"
                :color="getQuotaColor(data.request_quota.percentage)"
              />
            </el-descriptions-item>
          </el-descriptions>
        </div>

        <!-- Token 配额 - 每日 -->
        <div class="section" v-if="data.token_quota?.daily">
          <h3>每日 Token 配额</h3>
          <el-descriptions :column="2" border>
            <el-descriptions-item label="配额限制">
              {{ data.token_quota.daily.limit.toLocaleString() }}
            </el-descriptions-item>
            <el-descriptions-item label="已使用">
              {{ data.token_quota.daily.used.toLocaleString() }}
            </el-descriptions-item>
            <el-descriptions-item label="剩余">
              {{ data.token_quota.daily.remaining.toLocaleString() }}
            </el-descriptions-item>
            <el-descriptions-item label="使用率">
              <el-progress
                :percentage="formatPercentageTwoDecimals(data.token_quota.daily.percentage)"
                :format="formatPercentageLabel"
                :color="getQuotaColor(data.token_quota.daily.percentage)"
              />
            </el-descriptions-item>
          </el-descriptions>
        </div>

        <!-- Token 配额 - 每月 -->
        <div class="section" v-if="data.token_quota?.monthly">
          <h3>每月 Token 配额</h3>
          <el-descriptions :column="2" border>
            <el-descriptions-item label="配额限制">
              {{ data.token_quota.monthly.limit.toLocaleString() }}
            </el-descriptions-item>
            <el-descriptions-item label="已使用">
              {{ data.token_quota.monthly.used.toLocaleString() }}
            </el-descriptions-item>
            <el-descriptions-item label="剩余">
              {{ data.token_quota.monthly.remaining.toLocaleString() }}
            </el-descriptions-item>
            <el-descriptions-item label="使用率">
              <el-progress
                :percentage="formatPercentageTwoDecimals(data.token_quota.monthly.percentage)"
                :format="formatPercentageLabel"
                :color="getQuotaColor(data.token_quota.monthly.percentage)"
              />
            </el-descriptions-item>
          </el-descriptions>
        </div>

        <!-- 模型白名单 -->
        <div class="section">
          <div class="section-head">
            <h3>模型白名单</h3>
            <p>{{ modelSectionHint }}</p>
          </div>
          <template v-if="displayModelSlugs.length > 0">
            <el-tag
              v-for="model in displayModelSlugs"
              :key="model"
              style="margin-right: 8px; margin-bottom: 8px;"
            >
              {{ model }}
            </el-tag>
          </template>
          <el-empty v-else :description="modelEmptyDescription" />
        </div>

        <!-- IP 白名单 -->
        <div class="section">
          <h3>IP 白名单</h3>
          <el-tag
            v-for="ip in data.ip_allowlist"
            :key="ip"
            type="info"
            style="margin-right: 8px; margin-bottom: 8px;"
          >
            {{ ip }}
          </el-tag>
          <el-empty v-if="data.ip_allowlist.length === 0" description="无限制" />
        </div>
      </div>
    </el-card>
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

const getQuotaColor = (percentage: number) => {
  if (percentage >= 90) return '#f56c6c'
  if (percentage >= 70) return '#e6a23c'
  return '#67c23a'
}

const loadData = async () => {
  try {
    loading.value = true
    modelLoadError.value = ''
    availableModelSlugs.value = []

    const response = await portalApi.getQuota()
    const responseData = response.data as any
    data.value = {
      request_quota: responseData.request_quota,
      token_quota: responseData.token_quota,
      model_allowlist: responseData.model_allowlist || [],
      ip_allowlist: responseData.ip_allowlist || []
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
            Authorization: `Bearer ${portalKey}`
          }
        }
      )

      if (!modelsResponse.ok) {
        modelLoadError.value = `网关模型接口返回 ${modelsResponse.status}`
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
  }
  finally {
    loading.value = false
  }
}

onMounted(() => {
  loadData()
})
</script>

<style scoped>
.quota-details-container {
  padding: 20px;
}

.sections {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.section-head {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: 15px;
}

.section h3 {
  margin: 0;
  font-size: 16px;
  color: #303133;
}

.section-head p {
  color: #909399;
  font-size: 13px;
  line-height: 1.6;
}

:deep(.el-descriptions__label) {
  font-weight: 500;
}
</style>
