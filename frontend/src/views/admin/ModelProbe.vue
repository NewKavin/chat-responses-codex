<template>
  <div class="model-probe-page">
    <ModelProbeBoard
      tone="admin"
      scope-label="管理员视图"
      title="模型探测"
      subtitle="自动轮询刷新通道健康、模型覆盖和最近探测结果。"
      :data="probeData"
      :loading="loading"
    />
  </div>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { adminApi } from '@/api/admin'
import ModelProbeBoard from '@/components/ModelProbeBoard.vue'
import type { ModelProbeResponse } from '@/types'
import {
  DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS,
  getModelProbeRefreshDelayMs
} from '@/utils/modelProbePolling'

const loading = ref(false)
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
    loading.value = true
    const { data } = await adminApi.getModelProbe()
    probeData.value = data
  } catch (error: any) {
    const errorMsg = error?.response?.data?.error?.message || '加载模型探测失败'
    ElMessage.error(errorMsg)
    // 保持原有数据，但标记为可能需要刷新
  } finally {
    loading.value = false
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
  background:
    radial-gradient(circle at top left, rgba(37, 99, 235, 0.08), transparent 30%),
    radial-gradient(circle at top right, rgba(14, 165, 233, 0.08), transparent 28%),
    linear-gradient(180deg, #f8fbff 0%, #f5f7fb 100%);
}
</style>
