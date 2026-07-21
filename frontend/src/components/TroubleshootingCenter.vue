<template>
  <div class="troubleshooting-center">
    <div class="page-head">
      <div>
        <h2>诊断与运行证据</h2>
        <p>配置诊断项目，检查解析结果，并持续观察活跃长任务。</p>
      </div>
      <el-button :loading="loadingActive" @click="loadActiveRequests">刷新活跃请求</el-button>
    </div>

    <div class="diagnostic-workspace-container">
      <div class="diagnostic-workspace">
        <section class="evidence-section diagnostic-config-section">
          <header class="evidence-section__header">
            <h3>诊断配置</h3>
          </header>
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
        </section>

        <div class="diagnostic-results-stack">
          <section class="evidence-section diagnostic-results-section">
            <header class="evidence-section__header">
              <h3>诊断结果</h3>
            </header>
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
          </section>

          <section class="evidence-section active-panel">
            <header class="evidence-section__header">
              <h3>活跃长任务</h3>
            </header>
            <div class="crc-table-shell evidence-table-shell">
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
            </div>
          </section>
        </div>
      </div>
    </div>

    <section v-if="admin && exportCapabilities && importCapabilities" class="evidence-section capability-panel">
      <header class="evidence-section__header">
        <h3>Capability 策略</h3>
      </header>
      <div class="capability-actions">
        <el-button size="small" @click="handleExportCapabilities">导出 JSON</el-button>
        <el-button size="small" @click="openImportDialog">导入 JSON</el-button>
        <el-button size="small" :loading="loadingProfiles" @click="refreshDialectProfiles">刷新 Profiles</el-button>
      </div>

      <div class="crc-table-shell evidence-table-shell">
        <el-table :data="paginatedDialectProfiles" size="small" empty-text="暂无 profile">
          <el-table-column prop="upstream_id" label="Upstream" min-width="110" />
          <el-table-column prop="runtime_model_slug" label="Runtime Model" min-width="140" show-overflow-tooltip />
          <el-table-column prop="protocol" label="Protocol" width="120" />
          <el-table-column prop="state" label="State" width="110" />
          <el-table-column prop="currentness" label="Current" width="100" />
          <el-table-column label="Age" width="100">
            <template #default="{ row }">
              {{ row.profile_age_seconds ?? '-' }}
            </template>
          </el-table-column>
          <el-table-column label="Probe" width="90">
            <template #default="{ row }">
              {{ row.probe_version == null ? '-' : `v${row.probe_version}` }}
            </template>
          </el-table-column>
          <el-table-column label="Fingerprint" min-width="150" show-overflow-tooltip>
            <template #default="{ row }">
              {{ row.fingerprint || '-' }}
            </template>
          </el-table-column>
          <el-table-column label="Evidence" min-width="170" show-overflow-tooltip>
            <template #default="{ row }">
              {{ row.evidence.codes.join(', ') || '-' }}
            </template>
          </el-table-column>
          <el-table-column label="操作" width="180">
            <template #default="{ row }">
              <el-button size="small" text @click="loadResolved(row)">详情</el-button>
              <el-button
                size="small"
                text
                @click="runManualProbe(row.upstream_id, row.key.route_id, row.runtime_model_slug, row.protocol)"
              >
                手动探测
              </el-button>
            </template>
          </el-table-column>
        </el-table>
      </div>

      <div class="capability-pagination">
        <el-pagination
          v-model:current-page="profilePage"
          v-model:page-size="profilePageSize"
          :page-sizes="profilePageSizes"
          :total="dialectProfiles.length"
          layout="total, sizes, prev, pager, next"
          @size-change="handleProfilePageSizeChange"
        />
      </div>

      <el-alert
        v-if="resolvedError"
        class="resolved-alert"
        type="warning"
        :closable="false"
        show-icon
        :title="resolvedError"
      />
      <div v-if="selectedResolved" class="resolved-summary">
        <div class="resolved-meta">
          <el-tag size="small" effect="plain">{{ selectedResolved.profile.currentness }}</el-tag>
          <span>Fingerprint: {{ selectedResolved.profile.fingerprint || '-' }}</span>
          <span>Token: {{ selectedResolved.token.field }} ({{ selectedResolved.token.source }})</span>
          <span>Reasoning: {{ selectedResolved.reasoning.carrier }}</span>
        </div>
        <div class="crc-table-shell evidence-table-shell">
          <el-table :data="selectedResolvedCapabilityRows" size="small">
            <el-table-column prop="capability" label="Capability" min-width="170" />
            <el-table-column prop="state" label="State" width="120" />
            <el-table-column prop="source" label="Source" width="120" />
          </el-table>
        </div>
        <div class="crc-table-shell evidence-table-shell">
          <el-table :data="selectedResolved.conflicts" size="small" empty-text="No conflicts">
            <el-table-column prop="subject" label="Conflict" min-width="180" />
            <el-table-column label="Probe" min-width="160">
              <template #default="{ row }">
                {{ row.probe.code }} ({{ row.probe.state }})
              </template>
            </el-table-column>
            <el-table-column label="Policy" min-width="160">
              <template #default="{ row }">
                {{ row.policy.code }} ({{ row.policy.state }})
              </template>
            </el-table-column>
          </el-table>
        </div>
      </div>
    </section>
  </div>

  <el-dialog
    v-model="importDialogVisible"
    title="导入 Capability JSON"
    width="min(720px, calc(100vw - 32px))"
  >
    <el-input v-model="capabilityJson" type="textarea" :rows="18" spellcheck="false" />
    <el-alert
      v-if="importError"
      class="import-error"
      type="error"
      :closable="false"
      show-icon
      :title="importError"
    />
    <template #footer>
      <el-button @click="importDialogVisible = false">取消</el-button>
      <el-button type="primary" :loading="importingCapabilities" @click="handleImportCapabilities">
        导入
      </el-button>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import type {
  ActiveGatewayRequest,
  CapabilityConfigurationDocument,
  DialectProfileSummary,
  ResolvedCapabilitiesResponse,
  TroubleshootingCheck,
  TroubleshootingClientProfile,
  TroubleshootingLogFilter,
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
  exportCapabilities?: () => Promise<CapabilityConfigurationDocument>
  importCapabilities?: (payload: CapabilityConfigurationDocument) => Promise<void>
  loadDialectProfiles?: () => Promise<DialectProfileSummary[]>
  getResolvedCapabilities?: (payload: {
    upstream_id: string
    route_id: string
    model: string
    protocol: 'chat_completions' | 'responses'
  }) => Promise<ResolvedCapabilitiesResponse>
  queueDialectProbe?: (payload: {
    upstream_id: string
    route_id: string
    runtime_model_slug: string
    protocol: 'chat_completions' | 'responses'
  }) => Promise<{ queued?: boolean }>
}>()

const router = useRouter()
const running = ref(false)
const loadingActive = ref(false)
const loadingProfiles = ref(false)
const importingCapabilities = ref(false)
const importDialogVisible = ref(false)
const capabilityJson = ref('')
const importError = ref('')
const resolvedError = ref('')
const lastRun = ref<TroubleshootingRunResponse | null>(null)
const activeRequests = ref<ActiveGatewayRequest[]>([])
const dialectProfiles = ref<DialectProfileSummary[]>([])
const profilePage = ref(1)
const profilePageSize = ref(10)
const profilePageSizes = [10, 20, 50]
const selectedResolved = ref<ResolvedCapabilitiesResponse | null>(null)

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
const paginatedDialectProfiles = computed(() => {
  const start = (profilePage.value - 1) * profilePageSize.value
  return dialectProfiles.value.slice(start, start + profilePageSize.value)
})
const selectedResolvedCapabilityRows = computed(() =>
  selectedResolved.value
    ? Object.entries(selectedResolved.value.capabilities).map(([capability, resolved]) => ({
        capability,
        state: resolved.state,
        source: resolved.source
      }))
    : []
)

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

const normalizeProfilePage = () => {
  const maxPage = Math.max(1, Math.ceil(dialectProfiles.value.length / profilePageSize.value))
  profilePage.value = Math.min(profilePage.value, maxPage)
}

const handleProfilePageSizeChange = () => {
  profilePage.value = 1
}

const refreshDialectProfiles = async () => {
  if (!props.loadDialectProfiles) return
  loadingProfiles.value = true
  try {
    dialectProfiles.value = await props.loadDialectProfiles()
    normalizeProfilePage()
  } finally {
    loadingProfiles.value = false
  }
}

const handleExportCapabilities = async () => {
  if (!props.exportCapabilities) return
  const payload = await props.exportCapabilities()
  capabilityJson.value = `${JSON.stringify(payload, null, 2)}\n`
  await navigator.clipboard.writeText(capabilityJson.value)
  ElMessage.success('Capability JSON 已复制')
}

const openImportDialog = () => {
  importError.value = ''
  importDialogVisible.value = true
}

const isCapabilityConfigurationDocument = (
  value: unknown
): value is CapabilityConfigurationDocument =>
  typeof value === 'object' &&
  value !== null &&
  !Array.isArray(value) &&
  'schema_version' in value &&
  typeof value.schema_version === 'number' &&
  'revision' in value &&
  typeof value.revision === 'number'

const handleImportCapabilities = async () => {
  if (!props.importCapabilities) return
  importingCapabilities.value = true
  importError.value = ''
  try {
    const payload = JSON.parse(capabilityJson.value)
    if (!isCapabilityConfigurationDocument(payload)) {
      throw new Error('Capability JSON 必须包含 numeric schema_version 和 revision')
    }
    await props.importCapabilities(payload)
    importDialogVisible.value = false
    ElMessage.success('Capability JSON 已导入')
    await refreshDialectProfiles()
  } catch (error) {
    importError.value = error instanceof Error ? error.message : '导入失败'
    ElMessage.error(importError.value)
  } finally {
    importingCapabilities.value = false
  }
}

const loadResolved = async (profile: DialectProfileSummary) => {
  if (!props.getResolvedCapabilities) return
  resolvedError.value = ''
  try {
    selectedResolved.value = await props.getResolvedCapabilities({
      upstream_id: profile.key.upstream_id,
      route_id: profile.key.route_id,
      model: profile.key.runtime_model_slug,
      protocol: profile.key.protocol
    })
  } catch (error) {
    selectedResolved.value = null
    resolvedError.value = error instanceof Error ? error.message : '无法读取 capability resolved 状态'
  }
}

const runManualProbe = async (
  upstream_id: string,
  route_id: string,
  runtime_model_slug: string,
  protocol: 'chat_completions' | 'responses'
) => {
  if (!props.queueDialectProbe) return
  const result = await props.queueDialectProbe({ upstream_id, route_id, runtime_model_slug, protocol })
  ElMessage.success(result.queued ? '探测任务已入队' : '当前探测队列不可用')
}

const copySummary = async () => {
  if (!lastRun.value) return
  await navigator.clipboard.writeText(buildTroubleshootingCopySummary(lastRun.value))
  ElMessage.success('诊断摘要已复制')
}

const openAdminLogs = (filter: TroubleshootingLogFilter | null | undefined) => {
  if (!filter) return
  router.push({
    path: '/admin/logs',
    query: Object.fromEntries(Object.entries(filter).map(([key, value]) => [key, String(value)]))
  })
}

onMounted(() => {
  void loadActiveRequests()
  void refreshDialectProfiles()
})
</script>

<style scoped>
.troubleshooting-center {
  min-height: 100%;
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
  color: var(--crc-text-strong);
  font-size: 18px;
}

.page-head p,
.hint {
  margin: 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.6;
}

.diagnostic-workspace-container {
  container-name: diagnostic-workspace;
  container-type: inline-size;
}

.diagnostic-workspace {
  display: grid;
  grid-template-columns: minmax(320px, 0.75fr) minmax(560px, 1.25fr);
  gap: 24px;
  align-items: start;
}

.diagnostic-results-stack {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 16px;
}

.diagnostic-config-section,
.diagnostic-results-section,
.active-panel,
.capability-panel {
  min-width: 0;
}

.evidence-section {
  padding: 18px 0;
  border-top: 1px solid var(--crc-border);
}

.evidence-section__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 16px;
}

.evidence-section__header h3 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 15px;
  line-height: 1.4;
}

.full-width {
  width: 100%;
}

.check-group {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.capability-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.resolved-alert,
.import-error {
  margin-top: 12px;
}

.resolved-summary {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-top: 12px;
}

.resolved-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  color: var(--crc-text-muted);
  font-size: 12px;
  overflow-wrap: anywhere;
}

.evidence-table-shell + .evidence-table-shell {
  margin-top: 10px;
}

.evidence-table-shell :deep(.el-table) {
  min-width: 680px;
}

.result-toolbar,
.result-title {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.result-toolbar .el-button {
  flex: 0 0 auto;
}

.result-toolbar {
  margin-bottom: 16px;
  color: var(--crc-text-muted);
  font-size: 13px;
}

.result-item p {
  margin: 6px 0 0;
}

.category {
  color: var(--crc-warning);
  font-size: 13px;
}

.details {
  color: var(--crc-text);
  font-size: 13px;
  line-height: 1.6;
  overflow-wrap: anywhere;
}

.capability-pagination {
  display: flex;
  max-width: 100%;
  overflow-x: auto;
  justify-content: flex-end;
  margin-top: 12px;
  padding-bottom: 4px;
}

@container diagnostic-workspace (max-width: 960px) {
  .diagnostic-workspace {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 767px) {
  .page-head,
  .result-toolbar,
  .result-title {
    align-items: flex-start;
    flex-direction: column;
  }

  .check-group {
    width: 100%;
  }

  .capability-actions {
    align-items: stretch;
    flex-direction: column;
  }

  .capability-actions .el-button {
    margin-left: 0;
  }

  .capability-pagination {
    justify-content: flex-start;
  }
}
</style>
