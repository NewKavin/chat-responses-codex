<template>
  <el-card shadow="never" class="panel">
    <template #header>
      <div class="panel-head">
        <div>
          <h3>客户端兼容矩阵</h3>
          <p>批量检查下游对 Codex、opencode 和 Hermes 的兼容路径与回退阶段。</p>
        </div>
        <div v-if="lastRun" class="summary-tags">
          <el-tag type="success" effect="light">{{ lastRun.summary.passed }} 通过</el-tag>
          <el-tag type="warning" effect="light">{{ lastRun.summary.warning }} 警告</el-tag>
          <el-tag type="danger" effect="light">{{ lastRun.summary.failed }} 失败</el-tag>
        </div>
      </div>
    </template>

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
      <el-button type="primary" :loading="running" @click="runMatrixJob">运行矩阵</el-button>
      <el-button :disabled="!lastRun" @click="copySummary">复制摘要</el-button>
    </div>

    <el-empty v-if="!lastRun" description="还没有运行兼容矩阵" />

    <template v-else>
      <div class="matrix-meta">
        <span>运行 ID：{{ lastRun.run_id }}</span>
        <span>客户端：{{ lastRun.client_profiles.map(formatClientProfile).join(' / ') }}</span>
        <span>耗时：{{ lastRun.duration_ms }} ms</span>
      </div>

      <el-table :data="lastRun.cells" size="small">
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
    </template>
  </el-card>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import type {
  CompatibilityMatrixRunRequest,
  CompatibilityMatrixRunResponse,
  TroubleshootingClientProfile
} from '@/types'
import {
  getClientProfileDefaults,
  getFallbackStageLabel,
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
.panel {
  border-radius: 8px;
}

.panel-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.panel-head h3 {
  margin: 0 0 6px;
  font-size: 18px;
}

.panel-head p,
.matrix-meta {
  margin: 0;
  color: #64748b;
  font-size: 13px;
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

.downstream-select {
  width: 320px;
  max-width: 100%;
}

@media (max-width: 768px) {
  .panel-head {
    flex-direction: column;
  }

  .downstream-select {
    width: 100%;
  }
}
</style>
