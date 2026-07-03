<template>
  <TroubleshootingCenter :models="models" :run="runTroubleshooting" :load-active="loadActive" />
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { portalApi } from '@/api/portal'
import TroubleshootingCenter from '@/components/TroubleshootingCenter.vue'
import type { ActiveGatewayRequest, TroubleshootingRunRequest } from '@/types'

const models = ref<string[]>([])

const loadModels = async () => {
  const { data } = await portalApi.getModels()
  models.value = data.map(item => item.model)
}

const runTroubleshooting = async (payload: TroubleshootingRunRequest) => {
  const { data } = await portalApi.runTroubleshooting(payload)
  return data
}

const loadActive = async (): Promise<ActiveGatewayRequest[]> => {
  const { data } = await portalApi.getActiveTroubleshootingRequests()
  return data.active_requests
}

onMounted(loadModels)
</script>
