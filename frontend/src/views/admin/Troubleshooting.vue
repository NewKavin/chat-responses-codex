<template>
  <div class="crc-page troubleshooting-page">
    <header class="crc-page-header">
      <div>
        <p class="crc-eyebrow">DIAGNOSE // EVIDENCE</p>
        <h1 class="crc-page-title">排障中心</h1>
        <p class="crc-page-description">验证客户端、模型、协议、工具调用与流式响应，并检查跨客户端兼容路径。</p>
      </div>
    </header>

    <TroubleshootingCenter
      admin
      :models="models"
      :downstreams="downstreamOptions"
      :run="runTroubleshooting"
      :load-active="loadActive"
      :export-capabilities="exportCapabilities"
      :import-capabilities="importCapabilities"
      :load-dialect-profiles="loadDialectProfiles"
      :get-resolved-capabilities="getResolvedCapabilities"
      :queue-dialect-probe="queueDialectProbe"
    />
    <CompatibilityMatrixPanel
      :downstreams="downstreamOptions"
      :run-matrix="runCompatibilityMatrix"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { adminApi } from '@/api/admin'
import CompatibilityMatrixPanel from '@/components/CompatibilityMatrixPanel.vue'
import TroubleshootingCenter from '@/components/TroubleshootingCenter.vue'
import type {
  ActiveGatewayRequest,
  CapabilityConfigurationDocument,
  CompatibilityMatrixRunRequest,
  DialectProfileSummary,
  DownstreamConfig,
  TroubleshootingRunRequest
} from '@/types'

const models = ref<string[]>([])
const downstreams = ref<DownstreamConfig[]>([])

const downstreamOptions = computed(() =>
  downstreams.value.map(item => ({
    id: item.id,
    name: item.name || item.id
  }))
)

const loadData = async () => {
  const [modelsResponse, downstreamsResponse] = await Promise.all([
    adminApi.getModels(),
    adminApi.getDownstreams()
  ])
  models.value = modelsResponse.data.models
  downstreams.value = downstreamsResponse.data
}

const runTroubleshooting = async (payload: TroubleshootingRunRequest) => {
  const { data } = await adminApi.runTroubleshooting(payload)
  return data
}

const runCompatibilityMatrix = async (payload: CompatibilityMatrixRunRequest) => {
  const { data } = await adminApi.runCompatibilityMatrix(payload)
  return data
}

const loadActive = async (): Promise<ActiveGatewayRequest[]> => {
  const { data } = await adminApi.getActiveTroubleshootingRequests()
  return data.active_requests
}

const exportCapabilities = async () => {
  const { data } = await adminApi.exportCapabilities()
  return data
}

const importCapabilities = async (payload: CapabilityConfigurationDocument) => {
  await adminApi.importCapabilities(payload)
}

const loadDialectProfiles = async (): Promise<DialectProfileSummary[]> => {
  const { data } = await adminApi.getDialectProfiles()
  return data.profiles
}

const getResolvedCapabilities = async (payload: {
  upstream_id: string
  route_id: string
  model: string
  protocol: 'chat_completions' | 'responses'
}) => {
  const { data } = await adminApi.getResolvedCapabilities(payload)
  return data
}

const queueDialectProbe = async (payload: {
  upstream_id: string
  route_id: string
  runtime_model_slug: string
  protocol: 'chat_completions' | 'responses'
}) => {
  const { data } = await adminApi.queueDialectProbe(payload)
  return data
}

onMounted(loadData)
</script>

<style scoped>
.troubleshooting-page {
  display: flex;
  flex-direction: column;
  gap: 24px;
}
</style>
