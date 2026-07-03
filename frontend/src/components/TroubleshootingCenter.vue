<template>
  <div class="troubleshooting-center">
    <div class="page-head">
      <div>
        <h2>排障中心</h2>
        <p>验证客户端、模型、协议、工具调用和流式响应。</p>
      </div>
      <el-button :loading="loadingActive" @click="loadActiveRequests">刷新活跃请求</el-button>
    </div>

    <el-row :gutter="16">
      <el-col :xs="24" :lg="8">
        <el-card shadow="never" class="panel">
          <template #header>诊断配置</template>
          <el-form label-position="top">
            <el-form-item v-if="admin" label="下游">
              <el-select v-model="form.downstream_id" class="full-width" filterable>
                <el-option
                  v-for="downstream in downstreams"
                  :key="downstream.id"
                  :label="`${downstream.name}（${downstream.id}）`"
                  :value="downstream.id"
                />
              </el-select>
            </el-form-item>

            <el-form-item label="客户端">
              <el-select v-model="form.client_profile" class="full-width" @change="applyProfileDefaults">
                <el-option
                  v-for="profile in profileOptions"
                  :key="profile.value"
                  :label="profile.label"
                  :value="profile.value"
                />
              </el-select>
              <p class="hint">{{ currentProfile.description }}</p>
            </el-form-item>

            <el-form-item label="模型">
              <el-select v-model="form.model" class="full-width" filterable allow-create default-first-option>
                <el-option v-for="model in modelOptions" :key="model" :label="model" :value="model" />
              </el-select>
            </el-form-item>

            <el-form-item label="诊断项目">
              <el-checkbox-group v-model="form.checks" class="check-group">
                <el-checkbox-button v-for="check in checkOptions" :key="check.value" :label="check.value">
                  {{ check.label }}
                </el-checkbox-button>
              </el-checkbox-group>
            </el-form-item>

            <el-button type="primary" :loading="running" @click="runDiagnostics">开始诊断</el-button>
          </el-form>
        </el-card>
      </el-col>

      <el-col :xs="24" :lg="16">
        <el-card shadow="never" class="panel">
          <template #header>诊断结果</template>
          <el-empty v-if="!lastRun" description="还没有运行诊断" />
          <div v-else>
            <div class="result-toolbar">
              <span>诊断 ID：{{ lastRun.run_id }}</span>
              <el-button size="small" @click="copySummary">复制摘要</el-button>
            </div>
            <el-timeline>
              <el-timeline-item
                v-for="result in lastRun.results"
                :key="result.id"
                :type="getTroubleshootingStatusMeta(result.status).type"
                :timestamp="`${result.duration_ms} ms`"
              >
                <div class="result-item">
                  <div class="result-title">
                    <strong>{{ result.label }}</strong>
                    <el-tag :type="getTroubleshootingStatusMeta(result.status).type" size="small">
                      {{ getTroubleshootingStatusMeta(result.status).label }}
                    </el-tag>
                  </div>
                  <p>{{ result.summary }}</p>
                  <p v-if="result.error_category" class="category">分类：{{ result.error_category }}</p>
                  <p v-if="result.details" class="details">{{ result.details }}</p>
                  <p class="hint">
                    {{ result.suggestion || getTroubleshootingSuggestion(result.error_category) }}
                  </p>
                  <el-button
                    v-if="admin && result.log_filter"
                    size="small"
                    text
                    @click="openAdminLogs(result.log_filter)"
                  >
                    查看相关日志
                  </el-button>
                </div>
              </el-timeline-item>
            </el-timeline>
          </div>
        </el-card>

        <el-card shadow="never" class="panel active-panel">
          <template #header>活跃长任务</template>
          <el-table :data="activeRequests" size="small">
            <el-table-column prop="model" label="模型" min-width="140" />
            <el-table-column prop="endpoint" label="接口" min-width="160" />
            <el-table-column prop="upstream_name" label="上游" min-width="120" />
            <el-table-column prop="user_agent" label="客户端" min-width="160" show-overflow-tooltip />
            <el-table-column label="状态" width="100">
              <template #default="{ row }">
                <el-tag :type="getActiveRequestHealth(row).type" size="small">
                  {{ getActiveRequestHealth(row).label }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column prop="elapsed_seconds" label="运行秒数" width="100" />
            <el-table-column prop="idle_seconds" label="无增量秒数" width="110" />
          </el-table>
        </el-card>
      </el-col>
    </el-row>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import type {
  ActiveGatewayRequest,
  TroubleshootingCheck,
  TroubleshootingClientProfile,
  TroubleshootingRunRequest,
  TroubleshootingRunResponse
} from '@/types'
import {
  buildTroubleshootingCopySummary,
  clientProfileDefaults,
  getActiveRequestHealth,
  getClientProfileDefaults,
  getTroubleshootingStatusMeta,
  getTroubleshootingSuggestion
} from '@/utils/troubleshooting'

const props = defineProps<{
  admin?: boolean
  models: string[]
  downstreams?: Array<{ id: string; name: string }>
  run: (payload: TroubleshootingRunRequest) => Promise<TroubleshootingRunResponse>
  loadActive: () => Promise<ActiveGatewayRequest[]>
}>()

const router = useRouter()
const running = ref(false)
const loadingActive = ref(false)
const lastRun = ref<TroubleshootingRunResponse | null>(null)
const activeRequests = ref<ActiveGatewayRequest[]>([])

const profileOptions = Object.entries(clientProfileDefaults).map(([value, profile]) => ({
  value: value as TroubleshootingClientProfile,
  label: profile.label
}))

const checkOptions: Array<{ value: TroubleshootingCheck; label: string }> = [
  { value: 'models', label: '模型列表' },
  { value: 'chat', label: 'Chat' },
  { value: 'chat_stream', label: 'Chat 流式' },
  { value: 'responses', label: 'Responses' },
  { value: 'responses_stream', label: 'Responses 流式' },
  { value: 'messages', label: 'Messages' },
  { value: 'messages_stream', label: 'Messages 流式' },
  { value: 'count_tokens', label: 'Count Tokens' },
  { value: 'tools', label: '工具调用' }
]

const form = reactive<{
  downstream_id: string
  client_profile: TroubleshootingClientProfile
  model: string
  checks: TroubleshootingCheck[]
}>({
  downstream_id: '',
  client_profile: 'cline',
  model: '',
  checks: getClientProfileDefaults('cline').checks.slice()
})

const modelOptions = computed(() => props.models)
const downstreams = computed(() => props.downstreams || [])
const currentProfile = computed(() => getClientProfileDefaults(form.client_profile))

watch(
  () => props.models,
  models => {
    if (!form.model && models.length > 0) form.model = models[0]
  },
  { immediate: true }
)

watch(
  () => props.downstreams,
  downstreamOptions => {
    if (props.admin && !form.downstream_id && downstreamOptions && downstreamOptions.length > 0) {
      form.downstream_id = downstreamOptions[0].id
    }
  },
  { immediate: true }
)

const applyProfileDefaults = () => {
  form.checks = getClientProfileDefaults(form.client_profile).checks.slice()
}

const runDiagnostics = async () => {
  if (!form.model) {
    ElMessage.warning('请先选择模型')
    return
  }
  const payload: TroubleshootingRunRequest = {
    client_profile: form.client_profile,
    model: form.model,
    checks: form.checks
  }
  if (props.admin) {
    if (!form.downstream_id) {
      ElMessage.warning('请先选择下游')
      return
    }
    payload.downstream_id = form.downstream_id
  }

  running.value = true
  try {
    lastRun.value = await props.run(payload)
  } finally {
    running.value = false
  }
}

const loadActiveRequests = async () => {
  loadingActive.value = true
  try {
    activeRequests.value = await props.loadActive()
  } finally {
    loadingActive.value = false
  }
}

const copySummary = async () => {
  if (!lastRun.value) return
  await navigator.clipboard.writeText(buildTroubleshootingCopySummary(lastRun.value))
  ElMessage.success('诊断摘要已复制')
}

const openAdminLogs = (filter: Record<string, unknown> | null | undefined) => {
  if (!filter) return
  router.push({
    path: '/admin/logs',
    query: Object.fromEntries(Object.entries(filter).map(([key, value]) => [key, String(value)]))
  })
}

onMounted(() => {
  void loadActiveRequests()
})
</script>

<style scoped>
.troubleshooting-center {
  min-height: 100%;
  padding: 20px;
}

.page-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  margin-bottom: 16px;
}

.page-head h2 {
  margin: 0 0 6px;
  font-size: 22px;
}

.page-head p,
.hint {
  margin: 0;
  color: #64748b;
  font-size: 13px;
  line-height: 1.6;
}

.panel {
  border-radius: 8px;
}

.active-panel {
  margin-top: 16px;
}

.full-width {
  width: 100%;
}

.check-group {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.result-toolbar,
.result-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.result-toolbar {
  margin-bottom: 16px;
  color: #475569;
  font-size: 13px;
}

.result-item p {
  margin: 6px 0 0;
}

.category {
  color: #b45309;
  font-size: 13px;
}

.details {
  color: #334155;
  font-size: 13px;
  line-height: 1.6;
  overflow-wrap: anywhere;
}
</style>
