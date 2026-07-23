<template>
  <section class="compatibility-matrix-panel crc-surface">
    <header class="panel-head">
      <div>
        <p class="crc-eyebrow">MATRIX // COMPATIBILITY</p>
        <h3>客户端兼容矩阵</h3>
        <p>批量检查下游对 Codex、opencode、Claude Code 和 Hermes 的兼容路径、语义证据与回退阶段。</p>
      </div>
      <div v-if="lastRun" class="summary-tags">
        <el-tag type="success" effect="light">{{ lastRun.summary.passed }} 通过</el-tag>
        <el-tag type="warning" effect="light">{{ lastRun.summary.warning }} 警告</el-tag>
        <el-tag type="danger" effect="light">{{ lastRun.summary.failed }} 失败</el-tag>
      </div>
    </header>

    <div class="matrix-toolbar">
      <el-select
        v-model="downstreamId"
        class="downstream-select"
        filterable
        placeholder="选择下游"
      >
        <el-option
          v-for="downstream in downstreams"
          :key="downstream.id"
          :label="`${downstream.name}（${downstream.id}）`"
          :value="downstream.id"
        />
      </el-select>
      <el-button type="primary" :icon="Grid3x3" :loading="running" @click="runMatrixJob">运行矩阵</el-button>
      <el-button :icon="Copy" :disabled="!lastRun" @click="copySummary">复制摘要</el-button>
    </div>

    <el-empty v-if="!lastRun" description="还没有运行兼容矩阵" />

    <template v-else>
      <div class="matrix-meta">
        <span>运行 ID：{{ lastRun.run_id }}</span>
        <span>客户端：{{ lastRun.client_profiles.map(formatClientProfile).join(' / ') }}</span>
        <span>耗时：{{ lastRun.duration_ms }} ms</span>
      </div>

      <div class="crc-table-shell matrix-table-shell">
        <el-table :data="lastRun.cells" size="small">
        <el-table-column type="expand">
          <template #default="{ row }">
            <div class="cell-details">
              <div class="detail-meta">
                <span>Profile: {{ row.profile_state || 'unknown' }}</span>
                <span>Currentness: {{ row.profile_currentness || 'missing' }}</span>
                <span>Profile age: {{ row.profile_age_seconds ?? '-' }} s</span>
                <span>Probe {{ row.probe_version == null ? '-' : `v${row.probe_version}` }}</span>
                <span>Runtime: {{ row.runtime_model_slug || row.model_slug }}</span>
                <span>Transition: {{ row.protocol_transition || 'native' }}</span>
                <span>Retry: {{ row.dialect_retry_count ?? 0 }}</span>
                <span v-if="row.first_meaningful_event_ms != null">
                  First event: {{ row.first_meaningful_event_ms }} ms
                </span>
              </div>
              <div v-if="row.adapter_set?.length" class="detail-meta">
                <span>Adapters: {{ row.adapter_set.join(', ') }}</span>
              </div>
              <div class="crc-table-shell check-table-shell">
                <el-table :data="row.check_results || []" size="small" class="check-table">
                  <el-table-column prop="id" label="检查项" min-width="180" />
                  <el-table-column label="结果" width="100">
                    <template #default="{ row: check }">
                      <el-tag :type="getMatrixCheckStatusMeta(check).type" effect="plain" size="small">
                        {{ getMatrixCheckStatusMeta(check).label }}
                      </el-tag>
                    </template>
                  </el-table-column>
                  <el-table-column label="观测值" width="110">
                    <template #default="{ row: check }">
                      {{ check.observed_value ?? '-' }}
                    </template>
                  </el-table-column>
                  <el-table-column label="证据代码" min-width="220">
                    <template #default="{ row: check }">
                      {{ (check.codes || []).join(', ') || '-' }}
                    </template>
                  </el-table-column>
                </el-table>
              </div>
            </div>
          </template>
        </el-table-column>
        <el-table-column prop="model_slug" label="模型" min-width="180" />
        <el-table-column label="客户端" min-width="120">
          <template #default="{ row }">
            {{ formatClientProfile(row.client_family) }}
          </template>
        </el-table-column>
        <el-table-column prop="endpoint" label="端点" min-width="150" />
        <el-table-column label="上游" min-width="160">
          <template #default="{ row }">
            {{ row.selected_upstream_name || '-' }}
          </template>
        </el-table-column>
        <el-table-column label="Fallback" min-width="140">
          <template #default="{ row }">
            {{ getFallbackStageLabel(row.fallback_stage) }}
          </template>
        </el-table-column>
        <el-table-column label="Profile" min-width="110">
          <template #default="{ row }">
            {{ row.profile_state || 'unknown' }}
          </template>
        </el-table-column>
        <el-table-column label="Transition" min-width="160" show-overflow-tooltip>
          <template #default="{ row }">
            {{ row.protocol_transition || 'native' }}
          </template>
        </el-table-column>
        <el-table-column label="Adapters" min-width="150" show-overflow-tooltip>
          <template #default="{ row }">
            {{ row.adapter_set?.join(', ') || '-' }}
          </template>
        </el-table-column>
        <el-table-column label="Retry" width="80">
          <template #default="{ row }">
            {{ row.dialect_retry_count ?? 0 }}
          </template>
        </el-table-column>
        <el-table-column label="状态" width="100">
          <template #default="{ row }">
            <el-tag :type="getTroubleshootingStatusMeta(row.status).type" size="small">
              {{ getTroubleshootingStatusMeta(row.status).label }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column prop="http_status" label="HTTP" width="90" />
        <el-table-column label="分类" min-width="180" show-overflow-tooltip>
          <template #default="{ row }">
            {{ row.error_category || '-' }}
          </template>
        </el-table-column>
        <el-table-column prop="summary" label="摘要" min-width="260" show-overflow-tooltip />
        </el-table>
      </div>
    </template>
  </section>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { Copy, Grid3x3 } from '@lucide/vue'
import type {
  CompatibilityMatrixRunRequest,
  CompatibilityMatrixRunResponse,
  TroubleshootingClientProfile
} from '@/types'
import {
  getClientProfileDefaults,
  getFallbackStageLabel,
  getMatrixCheckStatusMeta,
  getTroubleshootingStatusMeta,
  matrixClientProfiles
} from '@/utils/troubleshooting'

const props = defineProps<{
  downstreams: Array<{ id: string; name: string }>
  runMatrix: (payload: CompatibilityMatrixRunRequest) => Promise<CompatibilityMatrixRunResponse>
}>()

const downstreamId = ref('')
const running = ref(false)
const lastRun = ref<CompatibilityMatrixRunResponse | null>(null)

watch(
  () => props.downstreams,
  items => {
    if (items.length === 0) {
      downstreamId.value = ''
      return
    }

    const hasCurrent = items.some(item => item.id === downstreamId.value)
    if (!hasCurrent) {
      downstreamId.value = items[0].id
    }
  },
  { immediate: true }
)

const formatClientProfile = (profile: TroubleshootingClientProfile) =>
  getClientProfileDefaults(profile).label

const runMatrixJob = async () => {
  if (!downstreamId.value) {
    ElMessage.warning('请先选择下游')
    return
  }

  running.value = true
  try {
    lastRun.value = await props.runMatrix({
      downstream_id: downstreamId.value,
      client_profiles: matrixClientProfiles
    })
  } finally {
    running.value = false
  }
}

const copySummary = async () => {
  if (!lastRun.value) return

  try {
    await navigator.clipboard.writeText(lastRun.value.copy_summary)
    ElMessage.success('兼容矩阵摘要已复制')
  } catch {
    ElMessage.error('复制失败，请手动复制')
  }
}
</script>

<style scoped>
.compatibility-matrix-panel {
  container-name: compatibility-matrix;
  container-type: inline-size;
  min-width: 0;
  max-width: 100%;
  padding: 20px;
}

.panel-head {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.panel-head h3 {
  margin: 6px 0 6px;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 20px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.panel-head p {
  margin: 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.6;
}

.matrix-meta {
  margin: 0;
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  line-height: 1.6;
}

.matrix-toolbar,
.matrix-meta,
.summary-tags {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.matrix-toolbar {
  margin-bottom: 16px;
}

.matrix-meta {
  margin-bottom: 16px;
}

.summary-tags {
  flex: 0 0 auto;
}

.cell-details {
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-width: 0;
}

.detail-meta {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  overflow-wrap: anywhere;
}

.check-table {
  width: 100%;
  min-width: 680px;
}

.matrix-table-shell > :deep(.el-table) {
  min-width: 1840px;
}

.downstream-select {
  width: 320px;
  max-width: 100%;
}

@container compatibility-matrix (max-width: 860px) {
  .panel-head,
  .matrix-toolbar {
    align-items: stretch;
    flex-direction: column;
  }

  .matrix-toolbar .el-button {
    margin-left: 0;
  }

  .downstream-select {
    width: 100%;
  }
}

@media (max-width: 767px) {
  .compatibility-matrix-panel {
    padding: 16px;
  }
}
</style>
