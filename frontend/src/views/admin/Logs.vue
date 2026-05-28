<template>
  <div class="logs-container">
    <el-card>
      <template #header>
        <div class="header">
          <h2>日志管理</h2>
          <el-button :icon="Refresh" circle :loading="loading" @click="loadData" />
        </div>
      </template>

      <el-form :inline="true" class="filter-form">
        <el-form-item label="状态码">
          <el-select
            v-model="filters.status_codes"
            @change="handleFilterChange"
            placeholder="全部"
            clearable
            multiple
            collapse-tags
            collapse-tags-tooltip
          >
            <el-option label="200 (成功)" :value="200" />
            <el-option label="400 (错误请求)" :value="400" />
            <el-option label="401 (未授权)" :value="401" />
            <el-option label="403 (拒绝访问)" :value="403" />
            <el-option label="404 (未找到)" :value="404" />
            <el-option label="429 (限流)" :value="429" />
            <el-option label="500 (服务器错误)" :value="500" />
          </el-select>
        </el-form-item>
        <el-form-item label="模型">
          <el-input
            v-model="filters.model"
            clearable
            placeholder="输入模型关键词"
            @keyup.enter="handleFilterChange"
            @clear="handleFilterChange"
          />
        </el-form-item>
        <el-form-item label="时间范围">
          <el-select v-model="filters.time_range" @change="handleFilterChange">
            <el-option label="最近 1 天" value="1d" />
            <el-option label="最近 7 天" value="7d" />
            <el-option label="最近 30 天" value="30d" />
            <el-option label="自定义范围" value="custom" />
          </el-select>
        </el-form-item>
        <el-form-item v-if="filters.time_range === 'custom'" label="自定义">
          <el-date-picker
            v-model="filters.custom_range"
            type="datetimerange"
            start-placeholder="开始时间"
            end-placeholder="结束时间"
            value-format="x"
            @change="handleFilterChange"
          />
        </el-form-item>
        <el-form-item>
          <el-button type="primary" @click="handleFilterChange">筛选</el-button>
        </el-form-item>
      </el-form>

      <el-alert
        title="提示"
        type="info"
        :closable="false"
        class="helper-text"
      >
        日志按时间倒序展示。推理强度按下游请求原值显示；下游调用/上游请求名称、计费模式与 User-Agent 均支持原始透传字段优先展示。
      </el-alert>

      <el-table :data="tableRows" v-loading="loading" stripe>
        <el-table-column label="时间" width="180">
          <template #default="{ row }">
            {{ formatTime(row.created_at) }}
          </template>
        </el-table-column>
        <el-table-column label="API 名称" width="190">
          <template #default="{ row }">
            <div class="api-cell">
              <el-icon class="api-icon">
                <component :is="row.apiIcon" />
              </el-icon>
              <span>{{ row.apiName }}</span>
            </div>
          </template>
        </el-table-column>
        <el-table-column prop="model" label="模型" min-width="140" />
        <el-table-column label="下游调用" min-width="140" show-overflow-tooltip>
          <template #default="{ row }">
            {{ row.downstreamName }}
          </template>
        </el-table-column>
        <el-table-column label="上游请求" min-width="140" show-overflow-tooltip>
          <template #default="{ row }">
            {{ row.upstreamName }}
          </template>
        </el-table-column>
        <el-table-column label="推理强度" width="100">
          <template #default="{ row }">
            <el-tag size="small" effect="plain">{{ row.inferenceStrength }}</el-tag>
          </template>
        </el-table-column>
        <el-table-column prop="endpoint" label="端点" min-width="220" show-overflow-tooltip />
        <el-table-column label="类型" width="100">
          <template #default="{ row }">
            <el-tag size="small">{{ row.logType }}</el-tag>
          </template>
        </el-table-column>
        <el-table-column label="计费模式" width="120">
          <template #default="{ row }">
            <el-tag size="small" type="success" effect="plain">{{ row.billingMode }}</el-tag>
          </template>
        </el-table-column>
        <el-table-column label="Token" width="180">
          <template #default="{ row }">
            <div class="token-cell">
              <div class="token-pair">
                <div class="token-line token-line--prompt">
                  <el-icon><Top /></el-icon>
                  <span>{{ row.prompt_tokens }}</span>
                </div>
                <div class="token-line token-line--completion">
                  <el-icon><Bottom /></el-icon>
                  <span>{{ row.completion_tokens }}</span>
                </div>
              </div>
              <div class="token-line token-line--total">
                <el-icon><PieChart /></el-icon>
                <strong>{{ row.total_tokens }}</strong>
              </div>
            </div>
          </template>
        </el-table-column>
        <el-table-column label="次数" width="80" align="center">
          <template #default="{ row }">
            {{ row.requestCount }}
          </template>
        </el-table-column>
        <el-table-column label="耗时" width="90">
          <template #default="{ row }">
            {{ row.latency_ms }}ms
          </template>
        </el-table-column>
        <el-table-column label="User-Agent" min-width="220" show-overflow-tooltip>
          <template #default="{ row }">
            {{ row.userAgent }}
          </template>
        </el-table-column>
        <el-table-column label="状态码" width="100">
          <template #default="{ row }">
            <el-tag :type="getStatusType(row.status_code)">
              {{ row.status_code }}
            </el-tag>
          </template>
        </el-table-column>
      </el-table>

      <el-pagination
        v-model:current-page="pagination.page"
        v-model:page-size="pagination.page_size"
        :total="pagination.total"
        :page-sizes="[10, 20, 50, 100]"
        layout="total, sizes, prev, pager, next, jumper"
        @current-change="loadData"
        @size-change="loadData"
        class="pagination"
      />
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, computed, type Component } from 'vue'
import { ElMessage } from 'element-plus'
import {
  Bottom,
  ChatDotRound,
  Connection,
  DataLine,
  Download,
  PieChart,
  QuestionFilled,
  Refresh,
  Top,
  UploadFilled
} from '@element-plus/icons-vue'
import { adminApi } from '@/api/admin'
import type { UsageLog } from '@/types'

const loading = ref(false)
const logs = ref<UsageLog[]>([])

const filters = ref({
  status_codes: [] as number[],
  model: '',
  time_range: '7d',
  custom_range: [] as string[]
})

const pagination = ref({
  page: 1,
  page_size: 10,
  total: 0,
  total_pages: 0
})

interface ApiDescriptor {
  name: string
  logType: string
  icon: Component
}

interface DisplayLog extends UsageLog {
  apiName: string
  apiIcon: Component
  logType: string
  inferenceStrength: string
  billingMode: string
  requestCount: number
  userAgent: string
  downstreamName: string
  upstreamName: string
}

const tableRows = computed<DisplayLog[]>(() => logs.value.map(buildDisplayLog))

const getStatusType = (statusCode: number) => {
  if (statusCode >= 200 && statusCode < 300) return 'success'
  if (statusCode >= 400 && statusCode < 500) return 'warning'
  if (statusCode >= 500) return 'danger'
  return 'info'
}

const formatTime = (timestamp: number) => {
  const date = new Date(timestamp * 1000)
  return date.toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  })
}

const resolveApiDescriptor = (log: UsageLog): ApiDescriptor => {
  if (log.api_name && log.api_name.trim().length > 0) {
    return {
      name: log.api_name,
      logType: log.log_type?.trim() || '通用',
      icon: QuestionFilled
    }
  }

  const endpoint = log.endpoint.toLowerCase()
  if (endpoint.includes('/files') && (endpoint.includes('/content') || endpoint.includes('/download'))) {
    return { name: '文件下载', logType: '文件', icon: Download }
  }
  if (endpoint.includes('/files') || endpoint.includes('/upload')) {
    return { name: '文件上传', logType: '文件', icon: UploadFilled }
  }
  if (endpoint.includes('/responses')) {
    return { name: 'Responses API', logType: '推理', icon: ChatDotRound }
  }
  if (endpoint.includes('/chat/completions')) {
    return { name: 'ChatCompletions API', logType: '对话', icon: Connection }
  }
  if (endpoint.includes('/embeddings')) {
    return { name: 'Embeddings API', logType: '向量', icon: DataLine }
  }
  return { name: '通用 API', logType: '其它', icon: QuestionFilled }
}

const buildDisplayLog = (log: UsageLog): DisplayLog => {
  const api = resolveApiDescriptor(log)
  const inferenceStrength = log.inference_strength?.trim() || '标准'
  const billingMode = log.billing_mode?.trim() || (log.total_tokens > 0 ? 'Token 计费' : '请求计费')
  const userAgent = log.user_agent?.trim() || '未采集'
  const requestCount = log.request_count ?? 1
  const downstreamName = log.downstream_name?.trim() || log.downstream_key_id
  const upstreamName = log.upstream_name?.trim() || log.upstream_key_id

  return {
    ...log,
    apiName: api.name,
    apiIcon: api.icon,
    logType: log.log_type?.trim() || api.logType,
    inferenceStrength,
    billingMode,
    requestCount,
    userAgent,
    downstreamName,
    upstreamName
  }
}

const handleFilterChange = () => {
  pagination.value.page = 1
  loadData()
}

const loadData = async () => {
  try {
    loading.value = true
    const params: {
      page: number
      page_size: number
      time_range: string
      status_codes?: string
      model?: string
      start_time?: number
      end_time?: number
    } = {
      page: pagination.value.page,
      page_size: pagination.value.page_size,
      time_range: filters.value.time_range
    }

    if (filters.value.status_codes.length > 0) {
      params.status_codes = filters.value.status_codes.join(',')
    }
    if (filters.value.model.trim().length > 0) {
      params.model = filters.value.model.trim()
    }
    if (filters.value.time_range === 'custom') {
      const [start, end] = filters.value.custom_range
      if (start && end) {
        params.start_time = Math.floor(Number(start) / 1000)
        params.end_time = Math.floor(Number(end) / 1000)
      } else {
        params.time_range = '7d'
      }
    }

    const { data } = await adminApi.getLogs(params)
    logs.value = data.logs
    pagination.value.total = data.total
    pagination.value.total_pages = data.total_pages
  } catch (error) {
    ElMessage.error('加载日志失败')
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  loadData()
})
</script>

<style scoped>
.logs-container {
  padding: 20px;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.filter-form {
  margin-bottom: 20px;
}

.api-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.api-icon {
  color: #409eff;
}

.token-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.2;
}

.token-pair {
  display: flex;
  align-items: center;
  gap: 10px;
}

.token-line {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.token-line--prompt {
  color: #409eff;
}

.token-line--completion {
  color: #67c23a;
}

.token-line--total {
  color: #303133;
}

.token-line--total strong {
  color: #303133;
}

.pagination {
  margin-top: 20px;
  display: flex;
  justify-content: flex-end;
}

.helper-text {
  margin-bottom: 20px;
}
</style>
