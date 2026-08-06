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
        content="对每个模型通道发起真实请求，探测其支持的思考档位（low/medium/high/xhigh/max）。会消耗模型 token。"
        placement="top"
      >
        <el-button
          type="primary"
          plain
          :loading="probingCapabilities"
          :disabled="loading || capabilityProbeCandidateCount === 0"
          @click="runCapabilityProbe"
        >
          <Radar :size="15" :stroke-width="1.8" style="margin-right: 6px" />
          {{ capabilityProbeCandidateCount > 0 ? `一键探测思考档位 (${capabilityProbeCandidateCount})` : '一键探测思考档位' }}
        </el-button>
      </el-tooltip>
    </div>

    <div v-if="probingCapabilities" class="capability-probe-progress">
      <el-progress
        :percentage="capabilityProbeProgress"
        :stroke-width="4"
        :show-text="false"
      />
      <span>能力探测进行中…</span>
    </div>

    <section v-if="probeResults.length > 0" class="capability-probe-results" aria-live="polite">
      <div class="capability-probe-results__header">
        <h3>探测结果 / Probe Results</h3>
        <el-tag type="success" effect="plain">
          {{ probeResults.filter(r => r.levels.length > 0).length }} 个探测出思考档位
        </el-tag>
        <el-tag v-if="probeResults.some(r => r.state === 'unknown' || r.operational_code)" type="danger" effect="plain">
          {{ probeResults.filter(r => r.state === 'unknown' || r.operational_code).length }} 个失败
        </el-tag>
      </div>
      <div class="crc-table-shell">
        <el-table :data="probeResults" size="small" empty-text="无探测结果">
          <el-table-column prop="runtime_model_slug" label="模型" min-width="200" show-overflow-tooltip />
          <el-table-column label="状态" width="110">
            <template #default="{ row }">
              <el-tag :type="probeStateMeta(row.state).type" effect="plain" size="small">
                {{ probeStateMeta(row.state).label }}
              </el-tag>
            </template>
          </el-table-column>
          <el-table-column label="HTTP" width="80" align="center">
            <template #default="{ row }">
              <span v-if="row.http_status">{{ row.http_status }}</span>
              <span v-else>-</span>
            </template>
          </el-table-column>
          <el-table-column label="思考档位" min-width="220">
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
          <el-table-column label="说明" min-width="160">
            <template #default="{ row }">
              <span v-if="row.operational_code" class="capability-probe-results__err">{{ row.operational_code }}</span>
              <span v-else-if="row.http_status === 200">探测成功</span>
              <span v-else>待确认</span>
            </template>
          </el-table-column>
        </el-table>
      </div>
    </section>

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
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { BadgeCheck, Radar } from '@lucide/vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { adminApi } from '@/api/admin'
import ModelProbeBoard from '@/components/ModelProbeBoard.vue'
import type {
  DialectProfileSummary,
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

const capabilityProbeCandidates = computed(() =>
  (probeData.value.channels ?? []).flatMap(channel =>
    (channel.models ?? []).map(model => ({
      upstream_id: channel.upstream_id,
      route_id: channel.route_id,
      runtime_model_slug: model,
      protocol: 'chat_completions' as const
    }))
  )
)
const capabilityProbeCandidateCount = computed(() => capabilityProbeCandidates.value.length)
const capabilityProbeProgress = ref(0)

const runWithConcurrency = async <T>(
  items: T[],
  limit: number,
  worker: (item: T) => Promise<void>,
  onProgress?: (done: number, total: number) => void
) => {
  let cursor = 0
  const results: Promise<void>[] = []
  const runNext = async () => {
    while (cursor < items.length) {
      const index = cursor++
      await worker(items[index])
      onProgress?.(index + 1, items.length)
    }
  }
  for (let i = 0; i < Math.min(limit, items.length); i++) {
    results.push(runNext())
  }
  await Promise.allSettled(results)
}

interface CapabilityProbeCandidate {
  upstream_id: string
  route_id: string
  runtime_model_slug: string
  protocol: 'chat_completions' | 'responses'
}

interface CapabilityProbeResult {
  upstream_id: string
  runtime_model_slug: string
  state: string
  http_status: number | null
  operational_code: string | null
  levels: string[]
  probed: boolean
}

const probeResults = ref<CapabilityProbeResult[]>([])

const waitForProbesToSettle = async (candidates: CapabilityProbeCandidate[]) => {
  // Poll the profiles endpoint until every candidate has a fresh probe result,
  // or a hard timeout is reached. Returns the latest profiles snapshot.
  const deadline = Date.now() + 90_000
  const keyed = new Map(
    candidates.map(candidate => [
      `${candidate.upstream_id}/${candidate.runtime_model_slug}/${candidate.protocol}`,
      candidate
    ])
  )
  const seen = new Map<string, number>()
  let latest: DialectProfileSummary[] = []
  while (Date.now() < deadline) {
    const { data } = await adminApi.getDialectProfiles()
    latest = data.profiles
    for (const profile of data.profiles) {
      const key = `${profile.upstream_id}/${profile.runtime_model_slug}/${profile.protocol}`
      if (keyed.has(key) && profile.age_seconds !== null && profile.age_seconds < 5) {
        seen.set(key, Math.max(seen.get(key) ?? 0, 1))
      }
    }
    // All candidates have a fresh profile (age < 5s means recently probed).
    if (Array.from(keyed.keys()).every(key => seen.has(key))) {
      return latest
    }
    await new Promise(resolve => setTimeout(resolve, 2500))
  }
  return latest
}

const buildProbeResults = (
  candidates: CapabilityProbeCandidate[],
  profiles: DialectProfileSummary[]
): CapabilityProbeResult[] => {
  const byKey = new Map(
    profiles.map(profile => [
      `${profile.upstream_id}/${profile.runtime_model_slug}/${profile.protocol}`,
      profile
    ])
  )
  return candidates.map(candidate => {
    const key = `${candidate.upstream_id}/${candidate.runtime_model_slug}/${candidate.protocol}`
    const profile = byKey.get(key)
    if (!profile) {
      return {
        upstream_id: candidate.upstream_id,
        runtime_model_slug: candidate.runtime_model_slug,
        state: 'unknown',
        http_status: null,
        operational_code: '未探测到结果',
        levels: [],
        probed: false
      }
    }
    const effortField = Object.keys(profile.reasoning?.controls ?? {}).find(field =>
      field.includes('effort')
    )
    const levels = effortField ? (profile.reasoning?.controls?.[effortField] ?? []) : []
    return {
      upstream_id: candidate.upstream_id,
      runtime_model_slug: candidate.runtime_model_slug,
      state: profile.state,
      http_status: profile.status_summary?.http_status ?? null,
      operational_code: profile.status_summary?.operational_code ?? null,
      levels,
      probed: true
    }
  })
}

const probeStateMeta = (state: string) => {
  if (state === 'verified') return { label: '已验证', type: 'success' as const }
  if (state === 'partial') return { label: '部分支持', type: 'warning' as const }
  if (state === 'unknown') return { label: '未确认', type: 'info' as const }
  if (state === 'unsupported') return { label: '不支持', type: 'danger' as const }
  return { label: state, type: 'info' as const }
}

const runCapabilityProbe = async () => {
  if (capabilityProbeCandidates.value.length === 0) {
    ElMessage.warning('没有可探测的模型通道')
    return
  }
  probingCapabilities.value = true
  capabilityProbeProgress.value = 0
  probeResults.value = []
  try {
    const candidates: CapabilityProbeCandidate[] = capabilityProbeCandidates.value
    let queued = 0
    let failed = 0
    await runWithConcurrency(
      candidates,
      4,
      async candidate => {
        try {
          await adminApi.queueDialectProbe(candidate)
          queued++
        } catch {
          failed++
        }
      },
      (done, total) => {
        capabilityProbeProgress.value = Math.round((done / total) * 100)
      }
    )
    if (queued === 0) {
      ElMessage.error('能力探测排队失败，请检查上游通道状态')
      return
    }
    ElMessage.success(`已排队 ${queued} 个能力探测请求${failed > 0 ? `，${failed} 个失败` : ''}，正在等待探测完成…`)
    const profiles = await waitForProbesToSettle(candidates)
    probeResults.value = buildProbeResults(candidates, profiles)
    capabilityProbeProgress.value = 100
    await loadData()
    const withLevels = probeResults.value.filter(result => result.levels.length > 0).length
    const failedProbes = probeResults.value.filter(
      result => result.state === 'unknown' || result.operational_code
    ).length
    ElMessage.success(
      `能力探测完成：${probeResults.value.length} 个通道，${withLevels} 个探测出思考档位${failedProbes > 0 ? `，${failedProbes} 个失败` : ''}，已刷新`
    )
  } catch (error: any) {
    const errorMsg = error?.response?.data?.error?.message || '能力探测排队失败'
    ElMessage.error(errorMsg)
  } finally {
    probingCapabilities.value = false
    capabilityProbeProgress.value = 0
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
  margin: 12px 0;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.capability-probe-progress .el-progress {
  flex: 1;
}

.capability-probe-results {
  margin: 12px 0 4px;
  padding: 12px 14px;
  border: 1px solid var(--crc-border-color, #e4e7ed);
  border-radius: 8px;
  background: var(--crc-bg-subtle, #fafafa);
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

.capability-probe-results__level {
  margin-right: 4px;
}

.capability-probe-results__none {
  color: var(--crc-text-muted, #909399);
  font-size: 12px;
}

.capability-probe-results__err {
  color: var(--crc-danger, #f56c6c);
  font-size: 12px;
}
</style>
