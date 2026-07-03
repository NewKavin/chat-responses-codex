<template>
  <TroubleshootingCenter
    admin
    :models="models"
    :downstreams="downstreamOptions"
    :run="runTroubleshooting"
    :load-active="loadActive"
  />
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { adminApi } from '@/api/admin'
import TroubleshootingCenter from '@/components/TroubleshootingCenter.vue'
import type { ActiveGatewayRequest, DownstreamConfig, TroubleshootingRunRequest } from '@/types'

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

const loadActive = async (): Promise<ActiveGatewayRequest[]> => {
  const { data } = await adminApi.getActiveTroubleshootingRequests()
  return data.active_requests
}

onMounted(loadData)
</script>
