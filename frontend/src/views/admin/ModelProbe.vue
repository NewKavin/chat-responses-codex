<template>
  <div class="model-probe-page">
    <div class="qualification-command-bar">
      <span class="qualification-command-title">模型资格验证</span>
      <el-button
        type="primary"
        :loading="qualifying"
        :disabled="loading"
        @click="runQualification"
      >
        <el-icon><CircleCheck /></el-icon>
        真实验证并应用
      </el-button>
    </div>

    <ModelProbeBoard
      tone="admin"
      scope-label="管理员视图"
      title="模型探测"
      subtitle="自动轮询刷新通道健康、模型覆盖和最近探测结果。"
      :data="probeData"
      :loading="loading"
      :error-message="loadError"
    />

    <section v-if="qualificationResult" class="qualification-result" aria-live="polite">
      <div class="qualification-result-header">
        <h2>资格结果</h2>
        <el-tag :type="qualificationResult.applied ? 'success' : 'info'" effect="plain">
          {{ qualificationResult.applied ? '已应用' : '仅预览' }}
        </el-tag>
      </div>

      <div class="qualification-metrics">
        <div class="qualification-metric">
          <strong>{{ qualificationResult.summary.retained_models }}</strong>
          <span>保留</span>
        </div>
        <div class="qualification-metric">
          <strong>{{ qualificationResult.summary.full_models }}</strong>
          <span>完整</span>
        </div>
        <div class="qualification-metric">
          <strong>{{ qualificationResult.summary.adapted_models }}</strong>
          <span>适配</span>
        </div>
        <div class="qualification-metric">
          <strong>{{ qualificationResult.summary.removed_models }}</strong>
          <span>移除</span>
        </div>
        <div class="qualification-metric">
          <strong>{{ qualificationResult.summary.operational_failures }}</strong>
          <span>运行故障</span>
        </div>
      </div>

      <el-table :data="qualificationRows" size="small" empty-text="无资格证据">
        <el-table-column prop="upstreamId" label="上游" min-width="150" />
        <el-table-column prop="model" label="模型" min-width="190" show-overflow-tooltip />
        <el-table-column label="协议" width="130">
          <template #default="{ row }">{{ protocolLabel(row.protocol) }}</template>
        </el-table-column>
        <el-table-column label="级别" width="110">
          <template #default="{ row }">
            <el-tag :type="levelTagType(row.level)" effect="plain" size="small">
              {{ levelLabel(row.level) }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column label="类别" min-width="150">
          <template #default="{ row }">{{ categoryLabel(row.category) }}</template>
        </el-table-column>
        <el-table-column prop="latencyMs" label="耗时 (ms)" width="110" align="right" />
      </el-table>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { CircleCheck } from '@element-plus/icons-vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { adminApi } from '@/api/admin'
import ModelProbeBoard from '@/components/ModelProbeBoard.vue'
import type {
  ModelProbeResponse,
  ModelQualificationCategory,
  ModelQualificationLevel,
  QualifyModelsResponse
} from '@/types'
import {
  DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS,
  getModelProbeRefreshDelayMs
} from '@/utils/modelProbePolling'

const loading = ref(false)
const qualifying = ref(false)
const loadError = ref('')
const qualificationResult = ref<QualifyModelsResponse | null>(null)
const probeData = ref<ModelProbeResponse>({
  refreshed_at: 0,
  refresh_interval_seconds: DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS,
  summary: {
    total_channels: 0,
    healthy_channels: 0,
    offline_channels: 0,
    degraded_channels: 0,
    total_models: 0,
    average_latency_ms: 0
  },
  channels: [],
  models: []
})

let refreshTimer: number | null = null
let isUnmounted = false

const qualificationRows = computed(() =>
  (qualificationResult.value?.upstreams ?? []).flatMap(upstream =>
    upstream.evidence.map(evidence => ({
      upstreamId: upstream.upstream_id,
      model: evidence.model,
      protocol: evidence.protocol,
      level: evidence.level,
      category: evidence.category,
      latencyMs: evidence.latency_ms
    }))
  )
)

const levelLabel = (level: ModelQualificationLevel) => ({
  full: '完整',
  adapted: '适配',
  unusable: '不可用',
  operational_failure: '运行故障'
})[level]

const levelTagType = (level: ModelQualificationLevel) => {
  if (level === 'full') return 'success'
  if (level === 'adapted') return 'warning'
  if (level === 'unusable') return 'danger'
  return 'info'
}

const categoryLabel = (category: ModelQualificationCategory) => ({
  passed: '通过',
  authentication: '认证失败',
  rate_limit: '限流',
  upstream_unavailable: '上游不可用',
  request_rejected: '请求被拒绝',
  model_not_found: '模型不存在',
  malformed_response: '响应格式错误',
  empty_response: '空响应',
  timeout: '超时',
  network: '网络失败'
})[category]

const protocolLabel = (protocol: 'ChatCompletions' | 'Responses') =>
  protocol === 'Responses' ? 'Responses' : 'Chat Completions'

const clearRefreshTimer = () => {
  if (refreshTimer !== null) {
    window.clearTimeout(refreshTimer)
    refreshTimer = null
  }
}

const scheduleRefresh = () => {
  if (isUnmounted) return
  clearRefreshTimer()
  refreshTimer = window.setTimeout(() => {
    void loadData()
  }, getModelProbeRefreshDelayMs(probeData.value))
}

const loadData = async () => {
  if (loading.value || isUnmounted) return
  try {
    loadError.value = ''
    loading.value = true
    const { data } = await adminApi.getModelProbe()
    probeData.value = data
  } catch (error: any) {
    const errorMsg = error?.response?.data?.error?.message || '加载模型探测失败'
    loadError.value = errorMsg
    ElMessage.error(errorMsg)
    // 保持原有数据，但标记为可能需要刷新
  } finally {
    loading.value = false
    scheduleRefresh()
  }
}

const runQualification = async () => {
  try {
    await ElMessageBox.confirm(
      '将向所有活动上游发送真实推理请求，并原子更新 test 下游模型列表。',
      '确认真实验证并应用',
      {
        type: 'warning',
        confirmButtonText: '验证并应用',
        cancelButtonText: '取消'
      }
    )
  } catch {
    return
  }

  clearRefreshTimer()
  qualifying.value = true
  try {
    const { data } = await adminApi.qualifyUpstreamModels({
      apply: true,
      upstream_ids: [],
      downstream_id: 'test',
      excluded_models: []
    })
    if (isUnmounted) return
    qualificationResult.value = data
    ElMessage.success('模型资格结果已应用')
    await loadData()
  } catch (error: any) {
    const errorMsg = error?.response?.data?.error?.message || '模型资格验证失败'
    ElMessage.error(errorMsg)
  } finally {
    qualifying.value = false
    scheduleRefresh()
  }
}

onMounted(() => {
  void loadData()
})

onUnmounted(() => {
  isUnmounted = true
  clearRefreshTimer()
})
</script>

<style scoped>
.model-probe-page {
  min-height: 100%;
  padding: 20px;
  background: #f5f7fa;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.qualification-command-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid #dcdfe6;
}

.qualification-command-title {
  color: #303133;
  font-size: 15px;
  font-weight: 600;
}

.qualification-result {
  background: #fff;
  border-top: 1px solid #dcdfe6;
  border-bottom: 1px solid #dcdfe6;
  padding: 16px 0;
}

.qualification-result-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 0 16px 12px;
}

.qualification-result-header h2 {
  margin: 0;
  color: #303133;
  font-size: 15px;
}

.qualification-metrics {
  display: grid;
  grid-template-columns: repeat(5, minmax(96px, 1fr));
  border-top: 1px solid #ebeef5;
  border-bottom: 1px solid #ebeef5;
  margin-bottom: 12px;
}

.qualification-metric {
  min-width: 0;
  padding: 12px 16px;
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.qualification-metric + .qualification-metric {
  border-left: 1px solid #ebeef5;
}

.qualification-metric strong {
  color: #303133;
  font-size: 20px;
  line-height: 1;
}

.qualification-metric span {
  color: #606266;
  font-size: 12px;
}

@media (max-width: 768px) {
  .model-probe-page {
    padding: 12px;
  }

  .qualification-command-bar {
    align-items: flex-start;
    flex-direction: column;
  }

  .qualification-metrics {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .qualification-metric + .qualification-metric {
    border-left: 0;
  }
}
</style>
