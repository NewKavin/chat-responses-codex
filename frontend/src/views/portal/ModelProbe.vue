<template>
  <div class="model-probe-page">
    <ModelProbeBoard
      tone="portal"
      scope-label="门户视图"
      title="模型探测"
      subtitle="仅展示当前门户可见范围内的通道与模型快照。"
      :data="probeData"
      :loading="loading"
    />
  </div>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'
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
    const { data } = await portalApi.getModelProbe()
    probeData.value = data
  } catch (error) {
    ElMessage.error('加载模型探测失败')
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
}
</style>
