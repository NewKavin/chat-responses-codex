<template>
  <div class="overview-container">
    <el-card class="summary-card">
      <template #header>
        <h2>配额使用概览</h2>
      </template>

      <el-row :gutter="20">
        <el-col :span="12" v-if="data.quota_summary.request_quota">
          <el-card shadow="hover">
            <el-statistic
              :title="`请求配额 (${data.quota_summary.request_quota.window_hours}小时)`"
              :value="data.quota_summary.request_quota.used"
              :value-style="{ color: getQuotaColor(data.quota_summary.request_quota.percentage) }"
            >
              <template #suffix>/ {{ data.quota_summary.request_quota.limit }}</template>
            </el-statistic>
            <el-progress
              :percentage="data.quota_summary.request_quota.percentage"
              :color="getQuotaColor(data.quota_summary.request_quota.percentage)"
              :show-text="false"
              class="progress"
            />
          </el-card>
        </el-col>

        <el-col :span="12" v-if="data.quota_summary.token_daily">
          <el-card shadow="hover">
            <el-statistic
              title="每日 Token 配额"
              :value="data.quota_summary.token_daily.used"
              :value-style="{ color: getQuotaColor(data.quota_summary.token_daily.percentage) }"
            >
              <template #suffix>/ {{ formatCompact(data.quota_summary.token_daily.limit) }}</template>
            </el-statistic>
            <el-progress
              :percentage="data.quota_summary.token_daily.percentage"
              :color="getQuotaColor(data.quota_summary.token_daily.percentage)"
              :show-text="false"
              class="progress"
            />
          </el-card>
        </el-col>

        <el-col :span="12" v-if="data.quota_summary.token_monthly">
          <el-card shadow="hover">
            <el-statistic
              title="每月 Token 配额"
              :value="data.quota_summary.token_monthly.used"
              :value-style="{ color: getQuotaColor(data.quota_summary.token_monthly.percentage) }"
            >
              <template #suffix>/ {{ formatCompact(data.quota_summary.token_monthly.limit) }}</template>
            </el-statistic>
            <el-progress
              :percentage="data.quota_summary.token_monthly.percentage"
              :color="getQuotaColor(data.quota_summary.token_monthly.percentage)"
              :show-text="false"
              class="progress"
            />
          </el-card>
        </el-col>
      </el-row>
    </el-card>

    <el-row :gutter="20" style="margin-top: 20px;">
      <el-col :span="12">
        <el-card>
          <template #header>
            <h3>Token 使用统计</h3>
          </template>
          <el-descriptions :column="1" border>
            <el-descriptions-item label="今日使用">
              {{ formatCompact(data.token_summary.today) }}
            </el-descriptions-item>
            <el-descriptions-item label="本月使用">
              {{ formatCompact(data.token_summary.this_month) }}
            </el-descriptions-item>
          </el-descriptions>
        </el-card>
      </el-col>

      <el-col :span="12">
        <el-card>
          <template #header>
            <h3>模型使用摘要</h3>
          </template>
          <el-descriptions :column="1" border>
            <el-descriptions-item label="可用模型">
              {{ data.model_summary.total_models }}
            </el-descriptions-item>
            <el-descriptions-item label="活跃模型">
              {{ data.model_summary.active_models }}
            </el-descriptions-item>
          </el-descriptions>
        </el-card>
      </el-col>
    </el-row>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'
import type { PortalOverview } from '@/types'
import { formatCompactNumber } from '@/utils/numberFormat'

const data = ref<PortalOverview>({
  quota_summary: {
    request_quota: undefined,
    token_daily: undefined,
    token_monthly: undefined
  },
  token_summary: {
    today: 0,
    this_month: 0
  },
  model_summary: {
    total_models: 0,
    active_models: 0
  }
})

const getQuotaColor = (percentage: number) => {
  if (percentage >= 90) return '#f56c6c'
  if (percentage >= 70) return '#e6a23c'
  return '#67c23a'
}

const formatCompact = (value: number) => formatCompactNumber(value)
let refreshTimer: number | null = null

const loadData = async () => {
  try {
    const response = await portalApi.getOverview()
    data.value = response.data
  } catch (error) {
    ElMessage.error('加载数据失败')
  }
}

onMounted(() => {
  loadData()
  refreshTimer = window.setInterval(() => {
    loadData()
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
.overview-container {
  padding: 20px;
}

.summary-card {
  margin-bottom: 20px;
}

.summary-card h2 {
  margin: 0;
  font-size: 18px;
}

.progress {
  margin-top: 10px;
}

.quota-normal {
  color: #67c23a;
}

.quota-warning {
  color: #e6a23c;
}

.quota-critical {
  color: #f56c6c;
}

:deep(.el-statistic__content) {
  font-size: 28px;
}

:deep(.el-card__header) {
  padding: 15px 20px;
}
</style>
