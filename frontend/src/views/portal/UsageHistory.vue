<template>
  <div class="usage-history-container">
    <el-card>
      <template #header>
        <div class="header">
          <h2>使用历史</h2>
          <div class="header-actions">
            <el-radio-group v-model="timeRange" @change="loadData">
              <el-radio-button label="1d">1 天</el-radio-button>
              <el-radio-button label="7d">7 天</el-radio-button>
              <el-radio-button label="30d">30 天</el-radio-button>
            </el-radio-group>
            <el-button @click="loadData" :loading="loading" circle>
              <el-icon><Refresh /></el-icon>
            </el-button>
          </div>
        </div>
      </template>

      <div v-loading="loading">
        <!-- 每日统计图表 -->
        <div class="section">
          <h3>每日统计</h3>
          <div ref="dailyChartRef" class="chart"></div>
        </div>

        <!-- Token 使用趋势 -->
        <div class="section">
          <h3>Token 使用趋势</h3>
          <div ref="tokenChartRef" class="chart"></div>
        </div>

        <!-- 最近请求日志 -->
        <div class="section">
          <h3>最近请求</h3>
          <el-table :data="data.recent_logs" stripe>
            <el-table-column label="时间" width="180">
              <template #default="{ row }">
                {{ formatTime(row.created_at) }}
              </template>
            </el-table-column>
            <el-table-column prop="model" label="模型" width="150" />
            <el-table-column label="状态" width="100">
              <template #default="{ row }">
                <el-tag :type="getStatusType(row.status_code)">
                  {{ row.status_code }}
                </el-tag>
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
            <el-table-column label="耗时" width="100">
              <template #default="{ row }">
                {{ row.latency_ms }}ms
              </template>
            </el-table-column>
          </el-table>
        </div>
      </div>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick } from 'vue'
import { ElMessage } from 'element-plus'
import { Refresh, Top, Bottom, PieChart } from '@element-plus/icons-vue'
import * as echarts from 'echarts'
import { portalApi } from '@/api/portal'
import type { PortalUsageHistory } from '@/types'

const loading = ref(false)
const timeRange = ref('7d')
const dailyChartRef = ref<HTMLElement>()
const tokenChartRef = ref<HTMLElement>()
let dailyChart: echarts.ECharts | null = null
let tokenChart: echarts.ECharts | null = null
let dailyResizeObserver: ResizeObserver | null = null
let tokenResizeObserver: ResizeObserver | null = null

const data = ref<PortalUsageHistory>({
  daily_stats: [],
  recent_logs: []
})

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

const formatDate = (timestamp: number) => {
  const date = new Date(timestamp * 1000)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${month}/${day}`
}

const initCharts = () => {
  if (dailyChartRef.value) {
    dailyChart = echarts.init(dailyChartRef.value)
  }
  if (tokenChartRef.value) {
    tokenChart = echarts.init(tokenChartRef.value)
  }
}

/**
 * 使用 ResizeObserver 监听容器尺寸变化（tab 隐藏→显示时生效）
 */
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
  const dates = data.value.daily_stats.map(s => formatDate(s.date))
  const requests = data.value.daily_stats.map(s => s.total_requests)

  dailyChart.clear()
  dailyChart.resize()
  dailyChart.setOption({
    tooltip: { trigger: 'axis' },
    grid: { left: 40, right: 20, top: 30, bottom: 30 },
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
  const dates = data.value.daily_stats.map(s => formatDate(s.date))
  const tokens = data.value.daily_stats.map(s => s.total_tokens)

  tokenChart.clear()
  tokenChart.resize()
  tokenChart.setOption({
    tooltip: { trigger: 'axis' },
    grid: { left: 40, right: 20, top: 30, bottom: 30 },
    xAxis: {
      type: 'category',
      data: dates
    },
    yAxis: {
      type: 'value',
      name: 'Token 数'
    },
    series: [{
      name: 'Token 数',
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

const loadData = async () => {
  try {
    loading.value = true
    const days = timeRange.value === '1d' ? 1 : timeRange.value === '7d' ? 7 : 30
    const { data: history } = await portalApi.getUsageHistory({ time_range: `${days}d` })
    data.value = history

    await nextTick()
    updateCharts()
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const handleResize = () => {
  dailyChart?.resize()
  tokenChart?.resize()
}

onMounted(async () => {
  await nextTick()
  initCharts()
  setupResizeObservers()
  dailyChart?.resize()
  tokenChart?.resize()
  loadData()
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
  padding: 20px;
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

.section {
  margin-bottom: 30px;
}

.section:last-child {
  margin-bottom: 0;
}

.section h3 {
  margin: 0 0 15px 0;
  font-size: 16px;
  color: #303133;
}

.chart {
  width: 100%;
  height: 320px;
  min-height: 320px;
  background-color: #f5f7fa;
  border-radius: 4px;
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
</style>
