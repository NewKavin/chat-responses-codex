<template>
  <div class="crc-page model-probe-page">
    <div class="crc-toolbar qualification-command-bar">
      <div>
        <p class="crc-eyebrow">QUALIFY // REAL REQUESTS</p>
        <span class="qualification-command-title">模型探测 / Model Probe</span>
        <p>模型探测页会实时探测所有上游模型，测试结果会实时刷新。</p>
      </div>
      <el-tooltip
        content="向活动上游发起真实请求验证模型可用性，并按结果更新 test 下游模型列表。会消耗模型 token。"
        placement="top"
      >
        <el-button
          type="primary"
          :loading="qualifying"
          :disabled="loading"
          @click="runQualification"
        >
          <BadgeCheck :size="15" :stroke-width="1.8" style="margin-right: 6px" />
          真实验证并应用
        </el-button>
      </el-tooltip>
      <el-tooltip
        :content="capabilityProbeTooltip"
        placement="top"
      >
        <el-button
          type="primary"
          plain
          :loading="probingCapabilities"
          :disabled="loading || probingCapabilities || loadingProbeModels || probeModelScopeLoadFailed || selectedProbeModels.length === 0"
          @click="runCapabilityProbe"
        >
          <Radar :size="15" :stroke-width="1.8" style="margin-right: 6px" />
          一键探测思考档位
        </el-button>
      </el-tooltip>
      <el-select
        v-if="probeScopeModels.length > 0"
        v-model="selectedProbeModels"
        multiple
        collapse-tags
        collapse-tags-tooltip
        :max-collapse-tags="2"
        :loading="loadingProbeModels"
        placeholder="选择要探测的模型"
        class="probe-model-scope-select"
      >
        <el-option
          v-for="model in probeScopeModels"
          :key="model"
          :label="model"
          :value="model"
        />
      </el-select>
      <el-button
        v-if="probeModelScopeLoadFailed"
        text
        type="danger"
        :icon="RefreshCw"
        @click="loadProbeModelScope"
      >
        范围加载失败，重试
      </el-button>
    </div>

    <el-tabs v-model="activeProbeTab" class="model-probe-tabs">
      <el-tab-pane label="模型状态" name="status">
        <div class="model-probe-tab-panel">
          <ModelProbeBoard
            tone="admin"
            scope-label="管理员视图"
            title="模型探测"
            subtitle="自动轮询模型列表与通道状态；此刷新不发送推理请求。"
            :data="probeData"
            :loading="loading"
            :error-message="loadError"
            :on-retry="loadData"
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

            <div class="crc-table-shell">
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
            </div>
          </section>
        </div>
      </el-tab-pane>

      <el-tab-pane label="思考档位" name="reasoning">
        <div class="model-probe-tab-panel reasoning-probe-panel">
          <div v-if="probingCapabilities" class="capability-probe-progress">
            <el-progress
              :percentage="capabilityProbeProgress"
              :stroke-width="4"
              :show-text="false"
            />
            <span>能力探测进行中 {{ capabilityProbeCompleted }}/{{ capabilityProbeTotal }}</span>
          </div>

          <div
            v-if="probingCapabilities && capabilityProbeBatch"
            class="capability-probe-batch"
          >
            <div class="capability-probe-batch__header">
              <h4>本轮探测状态</h4>
              <el-tag
                v-if="capabilityProbeBatchReused > 0"
                type="info"
                effect="plain"
                size="small"
              >
                复用 {{ capabilityProbeBatchReused }} 条等价路由
              </el-tag>
              <span>{{ capabilityProbeBatchRows.length }} 个候选</span>
            </div>
            <div
              v-if="capabilityProbeBatchModels.length > 0"
              class="capability-probe-batch__scope"
            >
              <span class="capability-probe-batch__scope-label">本轮模型</span>
              <el-tag
                v-for="model in capabilityProbeBatchModels"
                :key="model"
                size="small"
                effect="plain"
                class="capability-probe-batch__scope-tag"
              >
                {{ model }}
              </el-tag>
            </div>
            <div v-if="capabilityProbeBatchEta" class="capability-probe-batch__eta">
              预计剩余 {{ capabilityProbeBatchEta }}
            </div>
            <div class="crc-table-shell">
              <el-table
                :data="capabilityProbeBatchRows"
                size="small"
                empty-text="无本轮探测候选"
              >
                <el-table-column prop="upstream_id" label="上游" min-width="140" show-overflow-tooltip />
                <el-table-column prop="exposed_model_slug" label="模型" min-width="170" show-overflow-tooltip />
                <el-table-column label="协议" width="130">
                  <template #default="{ row }">{{ capabilityProtocolLabel(row.protocol) }}</template>
                </el-table-column>
                <el-table-column label="本轮状态" width="120">
                  <template #default="{ row }">
                    <el-tag :type="probeBatchStateTagType(row.state)" effect="plain" size="small">
                      {{ probeBatchStateLabel(row.state) }}
                    </el-tag>
                  </template>
                </el-table-column>
                <el-table-column label="诊断" min-width="150" show-overflow-tooltip>
                  <template #default="{ row }">
                    <el-tooltip
                      v-if="row.diagnostic_code"
                      :content="capabilityDiagnosticTooltip(row.diagnostic_code)"
                      placement="top"
                    >
                      <span>{{ row.diagnostic_code }}</span>
                    </el-tooltip>
                    <span v-else>-</span>
                  </template>
                </el-table-column>
              </el-table>
            </div>
          </div>

          <section
            v-if="capabilityModelResults.length > 0 || capabilityRouteResults.length > 0"
            class="capability-probe-results"
            aria-live="polite"
          >
            <div class="capability-probe-results__header">
              <h3>模型汇总</h3>
              <el-switch
                v-model="showOnlyCurrentBatch"
                inline-prompt
                active-text="本批次"
                inactive-text="全局"
              />
              <span class="capability-probe-results__scope-label">
                {{ showCurrentBatchResults ? '仅显示本批次模型' : '全局 discovery' }}
              </span>
              <el-tag type="success" effect="plain">
                {{ capabilityModelResults.filter(row => row.levels.length > 0).length }} 个模型已验证
              </el-tag>
              <el-tag
                v-if="capabilityRouteResults.some(row => row.outcome === 'operational_failure' || row.outcome === 'deferred')"
                type="warning"
                effect="plain"
              >
                {{ capabilityRouteResults.filter(row => row.outcome === 'operational_failure' || row.outcome === 'deferred').length }} 条路由暂不可用
              </el-tag>
            </div>
            <div class="crc-table-shell">
              <el-table :data="capabilityModelResults" size="small" empty-text="无模型探测结果">
                <el-table-column prop="exposed_model_slug" label="模型" min-width="175" show-overflow-tooltip />
                <el-table-column label="思考档位" min-width="145">
                  <template #default="{ row }">
                    <template v-if="row.levels.length > 0">
                      <el-tag
                        v-for="level in row.levels"
                        :key="level"
                        size="small"
                        effect="plain"
                        class="capability-probe-results__level"
                      >
                        {{ level }}
                      </el-tag>
                    </template>
                    <span v-else class="capability-probe-results__none">无</span>
                  </template>
                </el-table-column>
              </el-table>
            </div>

            <el-collapse
              v-if="showCurrentBatchResults && globalCapabilityModelResults.length > capabilityModelResults.length"
              class="capability-probe-global-discovery"
            >
              <el-collapse-item name="global-discovery">
                <template #title>
                  <span>全局 discovery（{{ globalCapabilityModelResults.length }} 个模型）</span>
                </template>
                <div class="crc-table-shell">
                  <el-table
                    :data="globalCapabilityModelResults"
                    size="small"
                    empty-text="无全局模型探测结果"
                  >
                    <el-table-column
                      prop="exposed_model_slug"
                      label="模型"
                      min-width="175"
                      show-overflow-tooltip
                    />
                    <el-table-column label="思考档位" min-width="145">
                      <template #default="{ row }">
                        <span v-if="row.levels.length === 0" class="capability-probe-results__none">无</span>
                        <el-tag
                          v-for="level in row.levels"
                          :key="level"
                          size="small"
                          effect="plain"
                          class="capability-probe-results__level"
                        >
                          {{ level }}
                        </el-tag>
                      </template>
                    </el-table-column>
                  </el-table>
                </div>
              </el-collapse-item>
            </el-collapse>

            <div class="capability-probe-results__subheader">
              <h4>精确路由</h4>
              <span>{{ capabilityRouteResults.length }} 条</span>
            </div>
            <div class="crc-table-shell">
              <el-table :data="capabilityRouteResults" size="small" empty-text="无路由诊断">
                <el-table-column prop="exposed_model_slug" label="模型" min-width="190" show-overflow-tooltip />
                <el-table-column prop="upstream_id" label="上游" min-width="140" show-overflow-tooltip />
                <el-table-column label="路由" min-width="160" show-overflow-tooltip>
                  <template #default="{ row }">
                    <span class="capability-probe-results__route">{{ row.route_id }}</span>
                  </template>
                </el-table-column>
                <el-table-column label="协议" width="140">
                  <template #default="{ row }">{{ capabilityProtocolLabel(row.protocol) }}</template>
                </el-table-column>
                <el-table-column label="探测状态" width="130">
                  <template #default="{ row }">
                    <el-tag :type="routeStatusTagType(row)" effect="plain" size="small">
                      {{ routeStatusLabel(row) }}
                    </el-tag>
                  </template>
                </el-table-column>
                <el-table-column label="HTTP" width="80" align="center">
                  <template #default="{ row }">{{ row.http_status ?? '-' }}</template>
                </el-table-column>
                <el-table-column label="生效档位" min-width="180">
                  <template #default="{ row }">
                    <template v-if="row.accepted_reasoning_levels.length > 0">
                      <el-tag
                        v-for="level in row.accepted_reasoning_levels"
                        :key="level"
                        size="small"
                        effect="plain"
                        class="capability-probe-results__level"
                      >
                        {{ level }}
                      </el-tag>
                    </template>
                    <span v-else class="capability-probe-results__none">无</span>
                  </template>
                </el-table-column>
                <el-table-column label="来源" width="110" align="center">
                  <template #default="{ row }">
                    <div class="reasoning-source-cell">
                      <el-tag effect="plain" size="small">
                        {{ reasoningSourceLabel(row.reasoning_source) }}
                      </el-tag>
                      <span v-if="row.reasoning_source === 'override'">手工生效</span>
                    </div>
                  </template>
                </el-table-column>
                <el-table-column label="诊断" min-width="170" show-overflow-tooltip>
                  <template #default="{ row }">
                    <el-tooltip
                      v-if="row.operational_code"
                      :content="capabilityDiagnosticTooltip(row.operational_code)"
                      placement="top"
                    >
                      <span>{{ row.operational_code }}</span>
                    </el-tooltip>
                    <span v-else>-</span>
                  </template>
                </el-table-column>
                <el-table-column label="下次重试" width="110">
                  <template #default="{ row }">{{ formatProbeRetry(row.next_probe_at) }}</template>
                </el-table-column>
                <el-table-column label="操作" width="92" align="center">
                  <template #default="{ row }">
                    <div class="reasoning-route-actions">
                      <el-tooltip content="编辑生效档位" placement="top">
                        <el-button
                          text
                          circle
                          :icon="Pencil"
                          aria-label="编辑生效档位"
                          :disabled="savingReasoningOverride"
                          @click="openReasoningOverrideEditor(row)"
                        />
                      </el-tooltip>
                      <el-tooltip
                        v-if="row.managed_reasoning_override"
                        content="清除手工档位"
                        placement="top"
                      >
                        <el-button
                          text
                          circle
                          type="danger"
                          :icon="RotateCcw"
                          aria-label="清除手工档位"
                          :disabled="savingReasoningOverride"
                          @click="clearReasoningOverride(row, 'route')"
                        />
                      </el-tooltip>
                    </div>
                  </template>
                </el-table-column>
              </el-table>
            </div>
          </section>

          <el-empty
            v-else-if="!probingCapabilities"
            class="capability-probe-empty"
            :image-size="64"
            description="暂无思考档位探测结果"
          />
        </div>
      </el-tab-pane>
    </el-tabs>

    <el-dialog
      v-model="reasoningOverrideDialogVisible"
      title="编辑思考档位"
      width="520px"
      style="max-width: calc(100vw - 32px)"
      :close-on-click-modal="false"
    >
      <template v-if="editingReasoningRoute">
        <dl class="reasoning-override-identity">
          <div>
            <dt>模型</dt>
            <dd>{{ editingReasoningRoute.exposed_model_slug }}</dd>
          </div>
          <div>
            <dt>上游</dt>
            <dd>{{ editingReasoningRoute.upstream_id }}</dd>
          </div>
          <div>
            <dt>协议</dt>
            <dd>{{ capabilityProtocolLabel(editingReasoningRoute.protocol) }}</dd>
          </div>
          <div>
            <dt>当前来源</dt>
            <dd>{{ reasoningSourceLabel(editingReasoningRoute.reasoning_source) }}</dd>
          </div>
        </dl>

        <el-form label-position="top">
          <el-form-item label="应用范围">
            <el-radio-group v-model="reasoningOverrideScope" size="small">
              <el-radio-button value="route">当前精确路由</el-radio-button>
              <el-radio-button value="model_routes">模型全部当前路由</el-radio-button>
            </el-radio-group>
          </el-form-item>
          <el-form-item label="生效档位">
            <el-checkbox-group
              v-model="selectedReasoningLevels"
              class="reasoning-override-levels"
            >
              <el-checkbox
                v-for="level in REASONING_EFFORT_LEVELS"
                :key="level"
                :value="level"
              >
                {{ level }}
              </el-checkbox>
            </el-checkbox-group>
          </el-form-item>
        </el-form>
      </template>

      <template #footer>
        <div class="reasoning-override-footer">
          <el-button
            v-if="editingReasoningRoute?.managed_reasoning_override"
            type="danger"
            plain
            :icon="RotateCcw"
            :loading="savingReasoningOverride"
            @click="clearReasoningOverride(editingReasoningRoute, reasoningOverrideScope)"
          >
            清除手工设置
          </el-button>
          <span class="reasoning-override-footer__spacer" />
          <el-button
            :disabled="savingReasoningOverride"
            @click="reasoningOverrideDialogVisible = false"
          >
            取消
          </el-button>
          <el-button
            type="primary"
            :loading="savingReasoningOverride"
            :disabled="selectedReasoningLevels.length === 0"
            @click="saveReasoningOverride"
          >
            保存并生效
          </el-button>
        </div>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { BadgeCheck, Pencil, Radar, RefreshCw, RotateCcw } from '@lucide/vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { adminApi } from '@/api/admin'
import ModelProbeBoard from '@/components/ModelProbeBoard.vue'
import type {
  CapabilityDiscoveryResponse,
  CapabilityProbeBatchStatus,
  CapabilityRouteDiscoverySummary,
  CapabilityWireProtocol,
  ModelProbeResponse,
  ModelQualificationCategory,
  ModelQualificationLevel,
  ProbeAllCapabilitiesResponse,
  QualifyModelsResponse,
  ReasoningEffortLevel,
  ReasoningOverrideScope
} from '@/types'
import {
  CAPABILITY_PROBE_WAIT_TIMEOUT_MS,
  capabilityDiagnosticTooltip,
  filterCapabilityDiscoveryByModels,
  formatProbeEta,
  pollCapabilityDiscovery,
  pollCapabilityProbeBatch,
  probeBatchStateLabel,
  probeBatchStateTagType,
  routeStatusLabel,
  routeStatusTagType
} from '@/utils/capabilityDiscovery'
import {
  REASONING_EFFORT_LEVELS,
  normalizeReasoningLevels,
  reasoningSourceLabel
} from '@/utils/reasoningOverrides'
import {
  DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS,
  getModelProbeRefreshDelayMs
} from '@/utils/modelProbePolling'

type ProbeTab = 'status' | 'reasoning'
type EditableReasoningRoute = CapabilityRouteDiscoverySummary & {
  exposed_model_slug: string
}

const activeProbeTab = ref<ProbeTab>('status')
const loading = ref(false)
const qualifying = ref(false)
const probingCapabilities = ref(false)
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
const capabilityDiscovery = ref<CapabilityDiscoveryResponse>({ models: [] })
const capabilityProbeProgress = ref(0)
const capabilityProbeCompleted = ref(0)
const capabilityProbeTotal = ref(0)
const capabilityProbeBatch = ref<CapabilityProbeBatchStatus | null>(null)
const probeScopeModels = ref<string[]>([])
const selectedProbeModels = ref<string[]>([])
const loadingProbeModels = ref(false)
const probeModelScopeLoadFailed = ref(false)
const showOnlyCurrentBatch = ref(true)
const reasoningOverrideDialogVisible = ref(false)
const savingReasoningOverride = ref(false)
const editingReasoningRoute = ref<EditableReasoningRoute | null>(null)
const reasoningOverrideScope = ref<ReasoningOverrideScope>('route')
const selectedReasoningLevels = ref<ReasoningEffortLevel[]>([])
const capabilityProbeTooltip = computed(() => {
  if (probeModelScopeLoadFailed.value) return '模型范围加载失败，请重试'
  if (selectedProbeModels.value.length === 0) return '请先选择要探测的模型'
  return '对所选模型发起真实请求，探测其支持的思考档位。会消耗模型 token。'
})
const capabilityProbeBatchRows = computed(() =>
  capabilityProbeBatch.value?.candidates ?? []
)
const capabilityProbeBatchReused = computed(() =>
  capabilityProbeBatch.value?.reused_routes ?? 0
)
const capabilityProbeBatchModels = computed(() =>
  capabilityProbeBatch.value?.models ?? []
)
const showCurrentBatchResults = computed(() =>
  showOnlyCurrentBatch.value && capabilityProbeBatchModels.value.length > 0
)
const capabilityProbeBatchEta = computed(() => {
  const status = capabilityProbeBatch.value
  if (!status || status.terminal_at !== null) return null
  return formatProbeEta(status.estimated_remaining_seconds)
})
const capabilityModelResults = computed(() => {
  const discovery = showCurrentBatchResults.value
    ? filterCapabilityDiscoveryByModels(capabilityDiscovery.value, capabilityProbeBatchModels.value)
    : capabilityDiscovery.value
  return discovery.models.map(model => ({
    exposed_model_slug: model.exposed_model_slug,
    levels: model.verified_reasoning_levels
  }))
})
const capabilityRouteResults = computed<EditableReasoningRoute[]>(() => {
  const discovery = showCurrentBatchResults.value
    ? filterCapabilityDiscoveryByModels(capabilityDiscovery.value, capabilityProbeBatchModels.value)
    : capabilityDiscovery.value
  return discovery.models.flatMap(model =>
    model.routes.map(route => ({
      ...route,
      exposed_model_slug: model.exposed_model_slug
    }))
  )
})
const globalCapabilityModelResults = computed(() => capabilityDiscovery.value.models.map(model => ({
  exposed_model_slug: model.exposed_model_slug,
  levels: model.verified_reasoning_levels
})))

const fetchCapabilityDiscovery = async (
  timeoutMs?: number
): Promise<CapabilityDiscoveryResponse> => {
  const { data } = await adminApi.getCapabilityDiscovery(timeoutMs)
  return data
}

const loadProbeModelScope = async () => {
  if (isUnmounted) return
  loadingProbeModels.value = true
  probeModelScopeLoadFailed.value = false
  try {
    // 管理员视图：探测范围覆盖网关全部下游模型（不做下游 key 可见性过滤）
    const { data: modelsData } = await adminApi.getModels()
    const models = modelsData.models
    if (isUnmounted) return
    probeScopeModels.value = models
    selectedProbeModels.value = []
  } catch {
    probeModelScopeLoadFailed.value = true
    probeScopeModels.value = []
    selectedProbeModels.value = []
  } finally {
    loadingProbeModels.value = false
  }
}

const refreshCapabilityDiscovery = async (): Promise<CapabilityDiscoveryResponse> => {
  const data = await fetchCapabilityDiscovery()
  if (!isUnmounted) {
    capabilityDiscovery.value = data
  }
  return data
}

const openReasoningOverrideEditor = (route: EditableReasoningRoute) => {
  editingReasoningRoute.value = route
  reasoningOverrideScope.value = 'route'
  selectedReasoningLevels.value = normalizeReasoningLevels(
    route.accepted_reasoning_levels
  )
  reasoningOverrideDialogVisible.value = true
}

const applyReasoningOverride = async (
  route: EditableReasoningRoute,
  levels: ReasoningEffortLevel[],
  scope: ReasoningOverrideScope
) => {
  savingReasoningOverride.value = true
  try {
    const { data } = await adminApi.updateReasoningOverrides({
      upstream_id: route.upstream_id,
      route_id: route.route_id,
      exposed_model_slug: route.exposed_model_slug,
      runtime_model_slug: route.runtime_model_slug,
      protocol: route.protocol,
      levels: normalizeReasoningLevels(levels),
      scope
    })
    reasoningOverrideDialogVisible.value = false
    try {
      await refreshCapabilityDiscovery()
      ElMessage.success(`已更新 ${data.affected_route_count} 条精确路由`)
    } catch {
      ElMessage.warning(`已更新 ${data.affected_route_count} 条精确路由，状态刷新失败`)
    }
  } catch (error: any) {
    const errorMsg = error?.response?.data?.error?.message || '思考档位保存失败'
    ElMessage.error(errorMsg)
  } finally {
    savingReasoningOverride.value = false
  }
}

const saveReasoningOverride = async () => {
  if (!editingReasoningRoute.value || selectedReasoningLevels.value.length === 0) return
  await applyReasoningOverride(
    editingReasoningRoute.value,
    selectedReasoningLevels.value,
    reasoningOverrideScope.value
  )
}

const clearReasoningOverride = async (
  route: EditableReasoningRoute,
  scope: ReasoningOverrideScope
) => {
  try {
    await ElMessageBox.confirm(
      scope === 'model_routes'
        ? `清除模型 ${route.exposed_model_slug} 当前全部路由的手工档位？`
        : `清除路由 ${route.route_id} 的手工档位？`,
      '确认清除手工档位',
      {
        type: 'warning',
        confirmButtonText: '清除',
        cancelButtonText: '取消'
      }
    )
  } catch {
    return
  }
  await applyReasoningOverride(route, [], scope)
}

const waitForProbesToSettle = async (
  receipt: ProbeAllCapabilitiesResponse
) => {
  const result = await pollCapabilityDiscovery({
    receipt,
    initial: capabilityDiscovery.value,
    fetchDiscovery: fetchCapabilityDiscovery,
    cancelled: () => isUnmounted,
    timeoutMs: CAPABILITY_PROBE_WAIT_TIMEOUT_MS,
    onProgress: progress => {
      if (isUnmounted) return
      capabilityProbeCompleted.value = progress.completed
      capabilityProbeTotal.value = progress.total
      capabilityProbeProgress.value = progress.total === 0
        ? 100
        : Math.round((progress.completed / progress.total) * 100)
    }
  })
  if (!result.cancelled) {
    capabilityDiscovery.value = result.discovery
  }
  return result
}

const capabilityProtocolLabel = (protocol: CapabilityWireProtocol) =>
  protocol === 'responses' ? 'Responses' : 'Chat Completions'

const formatProbeRetry = (nextProbeAt: number | null) => {
  if (nextProbeAt === null) return '-'
  return new Date(nextProbeAt * 1000).toLocaleTimeString('zh-CN', { hour12: false })
}

const runCapabilityProbe = async () => {
  activeProbeTab.value = 'reasoning'
  probingCapabilities.value = true
  capabilityProbeProgress.value = 0
  capabilityProbeCompleted.value = 0
  capabilityProbeTotal.value = 0
  capabilityProbeBatch.value = null
  try {
    const { data: receipt } = await adminApi.probeAllCapabilities({
      mode: 'reasoning',
      models: selectedProbeModels.value
    })
    if (isUnmounted) return
    capabilityProbeTotal.value = receipt.queued_routes
    capabilityProbeBatch.value = { ...receipt, terminal_at: null, estimated_remaining_seconds: null }
    ElMessage.success(`已排队 ${receipt.queued_routes} 条精确路由，正在等待探测完成`)
    let batchPollCancelled = false
    void pollCapabilityProbeBatch({
      initial: { ...receipt, terminal_at: null, estimated_remaining_seconds: null },
      fetchBatch: async (timeoutMs) => {
        const { data } = await adminApi.getCapabilityProbeBatch(
          receipt.batch_id,
          timeoutMs
        )
        if (!isUnmounted) capabilityProbeBatch.value = data
        return data
      },
      cancelled: () => isUnmounted || batchPollCancelled,
      timeoutMs: CAPABILITY_PROBE_WAIT_TIMEOUT_MS
    })
    const polling = await waitForProbesToSettle(receipt)
    batchPollCancelled = true
    if (polling.cancelled || isUnmounted) return
    const latest = polling.discovery
    const progress = polling.progress
    const routes = latest.models.flatMap(model => model.routes)
    const unavailable = routes.filter(route =>
      route.outcome === 'operational_failure' || route.outcome === 'deferred'
    ).length
    const modelsWithLevels = latest.models.filter(
      model => model.verified_reasoning_levels.length > 0
    ).length
    const pending = progress.total - progress.completed
    await loadData()
    if (isUnmounted) return
    const summary = `能力探测结束：${modelsWithLevels} 个模型已验证思考档位`
    if (unavailable > 0 || pending > 0) {
      ElMessage.warning(`${summary}，${unavailable} 条路由暂不可用，${pending} 条仍在等待`)
    } else {
      ElMessage.success(summary)
    }
  } catch (error: any) {
    if (isUnmounted) return
    const errorMsg = error?.response?.data?.error?.message || '能力探测排队失败'
    ElMessage.error(errorMsg)
  } finally {
    if (!isUnmounted) {
      probingCapabilities.value = false
      capabilityProbeProgress.value = 0
    }
  }
}

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
      '将向所有活动上游发送真实推理请求，会消耗模型 token，并原子更新 test 下游模型列表。',
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
  void loadProbeModelScope()
  void refreshCapabilityDiscovery().catch(() => undefined)
})

onUnmounted(() => {
  isUnmounted = true
  clearRefreshTimer()
})
</script>

<style scoped>
.model-probe-page {
  display: flex;
  min-height: 100%;
  flex-direction: column;
  gap: 16px;
}

.qualification-command-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 0;
  padding: 16px 18px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background:
    radial-gradient(ellipse 90% 160% at 100% 50%, var(--crc-accent-soft) 0%, transparent 55%),
    var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.qualification-command-title {
  display: inline-block;
  margin-top: 6px;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 17px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.qualification-command-bar p:not(.crc-eyebrow) {
  margin: 5px 0 0;
  color: var(--crc-text-muted);
  font-size: 12px;
  line-height: 1.5;
}

.model-probe-tabs,
.model-probe-tab-panel {
  min-width: 0;
}

.probe-model-scope-select {
  width: 320px;
}

.capability-probe-batch__scope {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin-top: 8px;
}

.capability-probe-batch__scope-label {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.capability-probe-batch__scope-tag {
  margin-right: 0;
}

.capability-probe-batch__eta {
  margin-top: 6px;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.model-probe-tabs :deep(.el-tabs__header) {
  margin: 0;
}

.model-probe-tabs :deep(.el-tabs__nav-wrap::after) {
  height: 1px;
  background-color: var(--crc-border);
}

.model-probe-tabs :deep(.el-tabs__item) {
  height: 42px;
  color: var(--crc-text-muted);
  font-weight: 600;
}

.model-probe-tabs :deep(.el-tabs__item.is-active) {
  color: var(--crc-accent);
}

.model-probe-tabs :deep(.el-tabs__content) {
  padding-top: 16px;
  overflow: visible;
}

.model-probe-tab-panel {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.qualification-result {
  padding: 18px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
}

.qualification-result-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 14px;
}

.qualification-result-header h2 {
  margin: 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 17px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.qualification-metrics {
  display: grid;
  grid-template-columns: repeat(5, minmax(96px, 1fr));
  margin-bottom: 14px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-canvas);
  overflow: hidden;
}

.qualification-metric {
  min-width: 0;
  padding: 14px 16px;
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.qualification-metric + .qualification-metric {
  border-left: 1px solid var(--crc-border);
}

.qualification-metric strong {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 24px;
  font-weight: 600;
  letter-spacing: -0.02em;
  line-height: 1;
}

.qualification-metric span {
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
}

@media (max-width: 768px) {
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

.capability-probe-progress {
  display: flex;
  align-items: center;
  gap: 12px;
  margin: 0;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.capability-probe-progress .el-progress {
  flex: 1;
}

.capability-probe-batch {
  margin-top: 12px;
}

.capability-probe-batch__header {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 8px;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.capability-probe-batch__header h4 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 13px;
  font-weight: 600;
}

.capability-probe-results {
  min-width: 0;
}

.capability-probe-empty {
  min-height: 240px;
}

.capability-probe-results__header {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
}

.capability-probe-results__header h3 {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
}

.capability-probe-results__subheader {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 16px 0 8px;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.capability-probe-results__subheader h4 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 13px;
  font-weight: 600;
}

.capability-probe-results__level {
  margin-right: 4px;
}

.capability-probe-results__none {
  color: var(--crc-text-muted, #909399);
  font-size: 12px;
}

.capability-probe-results__route {
  font-family: var(--crc-font-mono);
  font-size: 12px;
}

.reasoning-source-cell {
  display: flex;
  min-height: 44px;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 3px;
  color: var(--crc-text-muted);
  font-size: 11px;
}

.reasoning-route-actions {
  display: flex;
  min-width: 68px;
  align-items: center;
  justify-content: center;
  gap: 2px;
}

.reasoning-override-identity {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 20px;
  margin: 0 0 18px;
  padding-bottom: 16px;
  border-bottom: 1px solid var(--crc-border);
}

.reasoning-override-identity div {
  min-width: 0;
}

.reasoning-override-identity dt {
  margin-bottom: 4px;
  color: var(--crc-text-muted);
  font-size: 11px;
}

.reasoning-override-identity dd {
  margin: 0;
  overflow-wrap: anywhere;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-mono);
  font-size: 12px;
}

.reasoning-override-levels {
  display: grid;
  width: 100%;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 8px;
}

.reasoning-override-levels :deep(.el-checkbox) {
  margin-right: 0;
}

.reasoning-override-footer {
  display: flex;
  width: 100%;
  align-items: center;
  gap: 8px;
}

.reasoning-override-footer__spacer {
  flex: 1;
}

@media (max-width: 560px) {
  .reasoning-override-identity {
    grid-template-columns: 1fr;
  }

  .reasoning-override-levels {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .reasoning-override-footer {
    flex-wrap: wrap;
  }
}
</style>
