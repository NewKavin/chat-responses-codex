<template>
  <div class="quota-details-container">
    <el-card>
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
                :percentage="data.request_quota.percentage"
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
                :percentage="data.token_quota.daily.percentage"
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
                :percentage="data.token_quota.monthly.percentage"
                :color="getQuotaColor(data.token_quota.monthly.percentage)"
              />
            </el-descriptions-item>
          </el-descriptions>
        </div>

        <!-- 模型白名单 -->
        <div class="section">
          <h3>模型白名单</h3>
          <el-tag
            v-for="model in data.model_allowlist"
            :key="model"
            style="margin-right: 8px; margin-bottom: 8px;"
          >
            {{ model }}
          </el-tag>
          <el-empty v-if="data.model_allowlist.length === 0" description="无限制" />
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
import { ref, onMounted } from 'vue'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'
import type { PortalQuota } from '@/types'

const data = ref<PortalQuota>({
  request_quota: undefined,
  token_quota: {
    daily: undefined,
    monthly: undefined
  },
  model_allowlist: [],
  ip_allowlist: []
})

const getQuotaColor = (percentage: number) => {
  if (percentage >= 90) return '#f56c6c'
  if (percentage >= 70) return '#e6a23c'
  return '#67c23a'
}

const loadData = async () => {
  try {
    const response = await portalApi.getQuota()
    const responseData = response.data as any
    data.value = {
      request_quota: responseData.request_quota,
      token_quota: responseData.token_quota,
      model_allowlist: responseData.model_allowlist || [],
      ip_allowlist: responseData.ip_allowlist || []
    }
  } catch (error) {
    ElMessage.error('加载数据失败')
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

.section h3 {
  margin: 0 0 15px 0;
  font-size: 16px;
  color: #303133;
}

:deep(.el-descriptions__label) {
  font-weight: 500;
}
</style>
