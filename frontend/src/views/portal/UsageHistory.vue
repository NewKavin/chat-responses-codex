<template>
  <div class="crc-page usage-history-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">使用历史</h1>
        <p class="crc-page-description">查看请求量与 Token 趋势，并追踪最近调用记录。</p>
      </div>
    </header>

    <div class="crc-toolbar history-toolbar">
      <el-radio-group v-model="timeRange" @change="handleTimeRangeChange">
        <el-radio-button label="1d" value="1d">1 天</el-radio-button>
        <el-radio-button label="7d" value="7d">7 天</el-radio-button>
        <el-radio-button label="30d" value="30d">30 天</el-radio-button>
      </el-radio-group>
      <el-tooltip content="刷新用量历史" placement="top">
        <el-button
          aria-label="刷新用量历史"
          :loading="loading"
          circle
          @click="reloadCurrentPage"
        >
          <el-icon><Refresh /></el-icon>
        </el-button>
      </el-tooltip>
    </div>

    <div v-loading="loading" class="history-content">
        <div class="history-chart-grid">
          <section class="crc-surface history-chart-surface">
            <h2>每日请求</h2>
            <div ref="dailyChartRef" class="chart"></div>
          </section>
          <section class="crc-surface history-chart-surface">
            <h2>Token 使用趋势</h2>
            <div ref="tokenChartRef" class="chart"></div>
          </section>
        </div>

        <section class="history-table-section">
          <h2>最近请求</h2>
          <div class="crc-table-shell">
          <el-table
            :data="data.recent_logs"
            stripe
            border
            class="recent-requests-table"
            table-layout="auto"
            :height="tableHeight"
          >
            <el-table-column label="时间" min-width="170">
              <template #default="{ row }">
                {{ formatTime(row.created_at) }}
              </template>
            </el-table-column>
            <el-table-column prop="model" label="模型" min-width="130" show-overflow-tooltip />
            <el-table-column prop="endpoint" label="端点" min-width="180" show-overflow-tooltip />
            <el-table-column label="推理强度" width="100" align="center">
              <template #default="{ row }">
                <el-tag size="small" effect="plain">
                  {{ formatInferenceStrength(row.inference_strength) }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column label="状态" width="88" align="center">
              <template #default="{ row }">
                <el-tag :type="getStatusType(row.status_code)">
                  {{ row.status_code }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column label="Token" min-width="180">
              <template #default="{ row }">
                <div class="token-cell">
                  <div class="token-pair">
                    <div class="token-line token-line--prompt">
                      <el-icon><Top /></el-icon>
                      <span>{{ formatToken(row.prompt_tokens) }}</span>
                    </div>
                    <div class="token-line token-line--completion">
                      <el-icon><Bottom /></el-icon>
                      <span>{{ formatToken(row.completion_tokens) }}</span>
                    </div>
                  </div>
                  <div class="token-line token-line--total">
                    <el-icon><PieChart /></el-icon>
                    <strong>{{ formatToken(row.total_tokens) }}</strong>
                  </div>
                </div>
              </template>
            </el-table-column>
            <el-table-column label="耗时" width="96" align="right">
              <template #default="{ row }">
                {{ row.latency_ms }}ms
              </template>
            </el-table-column>
            <el-table-column label="错误信息" min-width="220" show-overflow-tooltip>
              <template #default="{ row }">
                {{ row.error_message?.trim() || '-' }}
              </template>
            </el-table-column>
          </el-table>
          </div>

          <div class="pagination-wrap">
            <el-pagination
              v-model:current-page="pagination.page"
              v-model:page-size="pagination.pageSize"
              :total="pagination.total"
              :page-sizes="[10, 20, 50]"
              layout="total, sizes, prev, pager, next"
              @current-change="handlePageChange"
              @size-change="handlePageSizeChange"
            />
          </div>
        </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { Refresh, Top, Bottom, PieChart } from '@element-plus/icons-vue'
import { portalApi } from '@/api/portal'
import type { PortalUsageHistory } from '@/types'
import { buildUsageHistoryBuckets } from '@/utils/usageHistoryChart'
import { loadEcharts } from '@/utils/echartsLoader'
import type { EChartsType } from 'echarts/core'
import { formatCompactNumber } from '@/utils/numberFormat'
import { formatInferenceStrength } from '@/utils/logDisplay'
import { useTheme } from '@/composables/useTheme'
import { buildChartTheme } from '@/utils/chartTheme'

type ChartRange = '1d' | '7d' | '30d'

const daysByRange: Record<ChartRange, number> = {
  '1d': 1,
  '7d': 7,
  '30d': 30
}

const loading = ref(false)
const timeRange = ref<ChartRange>('7d')
const { resolvedTheme } = useTheme()
const dailyChartRef = ref<HTMLElement>()
const tokenChartRef = ref<HTMLElement>()
let dailyChart: EChartsType | null = null
let tokenChart: EChartsType | null = null
let dailyResizeObserver: ResizeObserver | null = null
let tokenResizeObserver: ResizeObserver | null = null

const data = ref<PortalUsageHistory>({
  daily_stats: [],
  recent_logs: [],
  recent_logs_total: 0,
  recent_logs_page: 1,
  recent_logs_page_size: 10,
  recent_logs_total_pages: 0
})

const pagination = ref({
  page: 1,
  pageSize: 10,
  total: 0,
  totalPages: 0
})

const tableHeight = computed(() => {
  const rows = Math.max(1, data.value.recent_logs.length)
  const estimatedRowHeight = 60
  const tableHeaderHeight = 56
  return Math.max(280, Math.min(640, tableHeaderHeight + rows * estimatedRowHeight))
})

const formatToken = (value: number) => value.toLocaleString('zh-CN')

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

const initCharts = async () => {
  const echarts = await loadEcharts()
  if (dailyChartRef.value) {
    dailyChart = echarts.init(dailyChartRef.value)
  }
  if (tokenChartRef.value) {
    tokenChart = echarts.init(tokenChartRef.value)
  }
}

const setupResizeObservers = () => {
  if (dailyChartRef.value && !dailyResizeObserver) {
    dailyResizeObserver = new ResizeObserver(() => {
      dailyChart?.resize()
    })
    dailyResizeObserver.observe(dailyChartRef.value)
  }
  if (tokenChartRef.value && !tokenResizeObserver) {
    tokenResizeObserver = new ResizeObserver(() => {
      tokenChart?.resize()
    })
    tokenResizeObserver.observe(tokenChartRef.value)
  }
}

const updateDailyChart = () => {
  if (!dailyChart) return
  const theme = buildChartTheme(resolvedTheme.value)
  const buckets = buildUsageHistoryBuckets(daysByRange[timeRange.value], data.value.daily_stats)
  const dates = buckets.map(item => item.label)
  const requests = buckets.map(item => item.requests)

  dailyChart.clear()
  dailyChart.resize()
  dailyChart.setOption({
    color: theme.series,
    textStyle: { color: theme.text },
    tooltip: {
      trigger: 'axis',
      backgroundColor: theme.tooltipBackground,
      borderColor: theme.tooltipBorder,
      textStyle: { color: theme.text }
    },
    grid: { left: 48, right: 20, top: 24, bottom: 28 },
    xAxis: {
      type: 'category',
      data: dates,
      boundaryGap: false,
      axisLine: { lineStyle: { color: theme.border } },
      axisLabel: { color: theme.muted }
    },
    yAxis: {
      type: 'value',
      name: '请求数',
      nameTextStyle: { color: theme.muted },
      axisLabel: { color: theme.muted },
      splitLine: { lineStyle: { color: theme.splitLine } }
    },
    series: [{
      name: '请求数',
      type: 'line',
      smooth: true,
      data: requests,
      areaStyle: { color: theme.series[0], opacity: 0.12 },
      lineStyle: { color: theme.series[0] },
      itemStyle: { color: theme.series[0] }
    }]
  })
}

const updateTokenChart = () => {
  if (!tokenChart) return
  const theme = buildChartTheme(resolvedTheme.value)
  const buckets = buildUsageHistoryBuckets(daysByRange[timeRange.value], data.value.daily_stats)
  const dates = buckets.map(item => item.label)
  const tokens = buckets.map(item => item.tokens)

  tokenChart.clear()
  tokenChart.resize()
  tokenChart.setOption({
    tooltip: {
      trigger: 'axis',
      backgroundColor: theme.tooltipBackground,
      borderColor: theme.tooltipBorder,
      textStyle: { color: theme.text },
      formatter: (params: any) => {
        if (!Array.isArray(params) || params.length === 0) {
          return ''
        }

        return `${params[0].axisValue}<br/>${params
          .map(item => {
            const value = Number(item.value ?? 0)
            return `${item.marker}${item.seriesName}: ${formatCompactNumber(value)}`
          })
          .join('<br/>')}`
      }
    },
    color: theme.series,
    textStyle: { color: theme.text },
    grid: { left: 48, right: 20, top: 24, bottom: 28 },
    xAxis: {
      type: 'category',
      data: dates,
      axisLine: { lineStyle: { color: theme.border } },
      axisLabel: { color: theme.muted }
    },
    yAxis: {
      type: 'value',
      name: 'Token（K/M）',
      nameTextStyle: { color: theme.muted },
      splitLine: { lineStyle: { color: theme.splitLine } },
      axisLabel: {
        color: theme.muted,
        formatter: (value: number) => formatCompactNumber(value)
      }
    },
    series: [{
      name: 'Token',
      type: 'bar',
      data: tokens,
      itemStyle: { color: theme.series[1] }
    }]
  })
}

const updateCharts = () => {
  updateDailyChart()
  updateTokenChart()
}

const syncPagination = (history: PortalUsageHistory) => {
  pagination.value.total = history.recent_logs_total
  pagination.value.page = history.recent_logs_page
  pagination.value.pageSize = history.recent_logs_page_size
  pagination.value.totalPages = history.recent_logs_total_pages
}

const loadData = async () => {
  try {
    loading.value = true
    const days = daysByRange[timeRange.value]
    const { data: history } = await portalApi.getUsageHistory({
      time_range: `${days}d`,
      page: pagination.value.page,
      page_size: pagination.value.pageSize
    })
    data.value = history
    syncPagination(history)

    await nextTick()
    updateCharts()
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const handleTimeRangeChange = () => {
  pagination.value.page = 1
  void loadData()
}

const handlePageChange = () => {
  void loadData()
}

const handlePageSizeChange = () => {
  pagination.value.page = 1
  void loadData()
}

const reloadCurrentPage = () => {
  void loadData()
}

const handleResize = () => {
  dailyChart?.resize()
  tokenChart?.resize()
}

watch(resolvedTheme, async () => {
  dailyChart?.dispose()
  tokenChart?.dispose()
  dailyChart = null
  tokenChart = null
  await nextTick()
  await initCharts()
  updateCharts()
})

onMounted(async () => {
  await nextTick()
  await initCharts()
  setupResizeObservers()
  dailyChart?.resize()
  tokenChart?.resize()
  await loadData()
  window.addEventListener('resize', handleResize)
})

onUnmounted(() => {
  dailyResizeObserver?.disconnect()
  tokenResizeObserver?.disconnect()
  dailyChart?.dispose()
  tokenChart?.dispose()
  window.removeEventListener('resize', handleResize)
})
</script>

<style scoped>
.usage-history-page {
  min-height: 100%;
}

.history-toolbar {
  justify-content: space-between;
}

.history-content {
  display: flex;
  flex-direction: column;
  gap: 20px;
  min-height: 100%;
}

.history-chart-grid {
  display: grid;
  gap: 16px;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.history-chart-surface {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 0;
  padding: 16px;
}

.history-chart-surface h2,
.history-table-section h2 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 14px;
}

.chart {
  width: 100%;
  height: 280px;
  min-height: 280px;
  border-radius: var(--crc-radius-sm);
  background: var(--crc-surface-muted);
}

.history-table-section {
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-height: 0;
  padding-top: 4px;
}

.recent-requests-table {
  width: 100%;
  min-width: 1160px;
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
  color: var(--crc-info);
}

.token-line--completion {
  color: var(--crc-success);
}

.token-line--total,
.token-line--total strong {
  color: var(--crc-text-strong);
}

.pagination-wrap {
  display: flex;
  justify-content: flex-end;
}

:deep(.recent-requests-table .el-table__cell) {
  white-space: nowrap;
}

@media (max-width: 1100px) {
  .history-chart-grid {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 767px) {
  .history-toolbar {
    align-items: center;
  }

  .history-toolbar :deep(.el-radio-group) {
    display: grid;
    flex: 1;
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }

  .history-toolbar :deep(.el-radio-button__inner) {
    width: 100%;
  }

  .chart {
    height: 240px;
    min-height: 240px;
  }

  .pagination-wrap {
    justify-content: flex-start;
    overflow-x: auto;
    padding-bottom: 4px;
  }
}
</style>
