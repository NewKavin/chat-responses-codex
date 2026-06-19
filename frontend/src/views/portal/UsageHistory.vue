<template>
  <div class="usage-history-container">
    <el-card class="history-card">
      <template #header>
        <div class="header">
          <h2>使用历史</h2>
          <div class="header-actions">
            <el-radio-group v-model="timeRange" @change="handleTimeRangeChange">
              <el-radio-button label="1d">1 天</el-radio-button>
              <el-radio-button label="7d">7 天</el-radio-button>
              <el-radio-button label="30d">30 天</el-radio-button>
            </el-radio-group>
            <el-button @click="reloadCurrentPage" :loading="loading" circle>
              <el-icon><Refresh /></el-icon>
            </el-button>
          </div>
        </div>
      </template>

      <div v-loading="loading" class="history-content">
        <div class="charts-row">
          <div class="chart-panel">
            <h3>每日统计</h3>
            <div ref="dailyChartRef" class="chart"></div>
          </div>
          <div class="chart-panel">
            <h3>Token 使用趋势</h3>
            <div ref="tokenChartRef" class="chart"></div>
          </div>
        </div>

        <div class="table-panel">
          <h3>最近请求</h3>
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
        </div>
      </div>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'
import { ElMessage } from 'element-plus'
import { Refresh, Top, Bottom, PieChart } from '@element-plus/icons-vue'
import { portalApi } from '@/api/portal'
import type { PortalUsageHistory } from '@/types'
import { buildUsageHistoryBuckets } from '@/utils/usageHistoryChart'
import { loadEcharts } from '@/utils/echartsLoader'
import type { EChartsType } from 'echarts/core'
import { formatCompactNumber } from '@/utils/numberFormat'

type ChartRange = '1d' | '7d' | '30d'

const daysByRange: Record<ChartRange, number> = {
  '1d': 1,
  '7d': 7,
  '30d': 30
}

const loading = ref(false)
const timeRange = ref<ChartRange>('7d')
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
  const buckets = buildUsageHistoryBuckets(daysByRange[timeRange.value], data.value.daily_stats)
  const dates = buckets.map(item => item.label)
  const requests = buckets.map(item => item.requests)

  dailyChart.clear()
  dailyChart.resize()
  dailyChart.setOption({
    tooltip: { trigger: 'axis' },
    grid: { left: 40, right: 20, top: 24, bottom: 24 },
    xAxis: {
      type: 'category',
      data: dates,
      boundaryGap: false
    },
    yAxis: {
      type: 'value',
      name: '请求数'
    },
    series: [{
      name: '请求数',
      type: 'line',
      smooth: true,
      data: requests,
      areaStyle: { color: 'rgba(64, 158, 255, 0.12)' },
      itemStyle: { color: '#409EFF' }
    }]
  })
}

const updateTokenChart = () => {
  if (!tokenChart) return
  const buckets = buildUsageHistoryBuckets(daysByRange[timeRange.value], data.value.daily_stats)
  const dates = buckets.map(item => item.label)
  const tokens = buckets.map(item => item.tokens)

  tokenChart.clear()
  tokenChart.resize()
  tokenChart.setOption({
    tooltip: {
      trigger: 'axis',
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
    grid: { left: 40, right: 20, top: 24, bottom: 24 },
    xAxis: {
      type: 'category',
      data: dates
    },
    yAxis: {
      type: 'value',
      name: 'Token（K/M）',
      axisLabel: {
        formatter: (value: number) => formatCompactNumber(value)
      }
    },
    series: [{
      name: 'Token',
      type: 'bar',
      data: tokens,
      itemStyle: { color: '#67C23A' }
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
.usage-history-container {
  width: 100%;
  padding: 0;
}

:deep(.history-card .el-card__body) {
  padding: 16px 20px 20px;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.header h2 {
  margin: 0;
}

.header-actions {
  display: flex;
  gap: 10px;
  align-items: center;
}

.history-content {
  height: 100%;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.charts-row {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.chart-panel {
  border: 1px solid #ebeef5;
  border-radius: 8px;
  background: #fff;
  padding: 10px 12px 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 0;
}

.chart-panel h3 {
  margin: 0;
  font-size: 14px;
  color: #303133;
}

.chart {
  width: 100%;
  height: 240px;
  min-height: 240px;
  background-color: #f5f7fa;
  border-radius: 4px;
}

.table-panel {
  min-height: 0;
  border: 1px solid #ebeef5;
  border-radius: 8px;
  background: #fff;
  padding: 10px 12px 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.table-panel h3 {
  margin: 0;
  font-size: 14px;
  color: #303133;
}

.recent-requests-table {
  width: 100%;
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

.pagination-wrap {
  display: flex;
  justify-content: flex-end;
}

:deep(.recent-requests-table .el-table__cell) {
  white-space: nowrap;
}

@media (max-width: 1100px) {
  .charts-row {
    grid-template-columns: 1fr;
  }
}
</style>
