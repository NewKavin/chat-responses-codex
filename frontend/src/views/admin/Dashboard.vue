<template>
  <div class="dashboard-container">
    <section class="hero-section">
      <div class="hero-content">
        <h1>控制台总览</h1>
        <p>从这里查看上游、下游和请求日志的整体状态。这个控制台强调协议转换如何保留工具面、模型语义和调用上下文，必要时才做可追踪的降级。</p>
      </div>
      <div class="hero-actions">
        <el-button type="primary" @click="$router.push('/admin/upstreams')">管理上游</el-button>
        <el-button @click="$router.push('/admin/downstreams')">管理下游</el-button>
        <el-button @click="$router.push('/admin/logs')">查看运行日志</el-button>
      </div>
    </section>

    <el-row :gutter="20" class="summary-grid" v-loading="loading">
      <el-col :xs="24" :sm="12" :md="6">
        <div class="summary-card">
          <div class="card-number">{{ data.upstreams_count }}</div>
          <div class="card-label">上游密钥</div>
          <div class="card-detail">启用 {{ data.upstreams_active }} / 共 {{ data.upstreams_count }}</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="6">
        <div class="summary-card">
          <div class="card-number">{{ data.downstreams_count }}</div>
          <div class="card-label">下游密钥</div>
          <div class="card-detail">启用 {{ data.downstreams_active }} / 共 {{ data.downstreams_count }}</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="6">
        <div class="summary-card">
          <div class="card-number">{{ data.logs_count }}</div>
          <div class="card-label">运行日志</div>
          <div class="card-detail">最近记录 {{ data.logs_count }} 条</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="6">
        <div class="summary-card">
          <div class="card-number">{{ data.active_models }}</div>
          <div class="card-label">可见模型</div>
          <div class="card-detail">{{ data.responses_upstreams }} 个 Responses 上游在线</div>
        </div>
      </el-col>
    </el-row>

    <el-card shadow="hover" class="stats-card">
      <template #header>
        <div class="card-header">
          <h2>统计图</h2>
          <div class="header-actions">
            <el-radio-group v-model="chartRange" size="small" @change="loadOverviewChart">
              <el-radio-button label="1d">1 天</el-radio-button>
              <el-radio-button label="7d">7 天</el-radio-button>
              <el-radio-button label="30d">30 天</el-radio-button>
            </el-radio-group>
            <el-button :icon="Refresh" :loading="chartLoading" size="small" circle @click="loadOverviewChart" />
          </div>
        </div>
      </template>

      <div class="chart-summary">
        <div class="summary-chip">
          <strong>{{ chartSummary.totalRequests }}</strong>
          <span>请求次数</span>
        </div>
        <div class="summary-chip">
          <strong>{{ chartSummary.successRate }}%</strong>
          <span>成功率</span>
        </div>
        <div class="summary-chip">
          <strong>{{ chartSummary.averageLatency }}ms</strong>
          <span>平均耗时</span>
        </div>
        <div class="summary-chip">
          <strong>{{ formatCompactToken(chartSummary.totalTokens) }}</strong>
          <span>Token 总量</span>
        </div>
      </div>
      <div ref="overviewChartRef" class="overview-chart" v-loading="chartLoading"></div>
    </el-card>

    <el-row :gutter="20" class="capability-grid">
      <el-col :xs="24" :sm="12" :md="8">
        <el-card shadow="hover" class="capability-card">
          <h3>能力保留</h3>
          <strong>优先保留 Responses 工具面</strong>
          <p>支持 web_search、file_search、computer_use 等能力时，不做无声裁剪。</p>
        </el-card>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8">
        <el-card shadow="hover" class="capability-card">
          <h3>降级可追踪</h3>
          <strong>不支持时再降级到 ChatCompletions</strong>
          <p>无法原样透传的工具会被记录到日志，方便排查能力损耗。</p>
        </el-card>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8">
        <el-card shadow="hover" class="capability-card">
          <h3>模型映射</h3>
          <strong>自动归一大小写与别名</strong>
          <p>减少手工输入模型名，让上游模型和下游暴露名称保持一致。</p>
        </el-card>
      </el-col>
    </el-row>

    <el-row :gutter="20" class="info-grid">
      <el-col :xs="24" :md="16">
        <el-card shadow="hover">
          <template #header>
            <div class="card-header">
              <h2>概览</h2>
            </div>
          </template>
          <p class="section-desc">这个网关会把 chat 和 responses 请求转换后转发给可用的上游密钥，并记录所有请求用于审计。Responses 优先保留完整工具面，必要时才做可追踪的降级。</p>
          <div class="context-list">
            <div class="context-item">
              <strong>管理员账号</strong>
              <span>{{ data.admin_username }}</span>
            </div>
            <div class="context-item">
              <strong>应用名称</strong>
              <span>{{ data.app_name }}</span>
            </div>
            <div class="context-item">
              <strong>能力保留</strong>
              <span>Responses 上游优先保留完整工具面；无法原样透传时会降级并记录原因。</span>
            </div>
            <div class="context-item">
              <strong>路由说明</strong>
              <span>常规 chat-completions 请求仍可复用同一套管理页配置，模型映射和大小写归一会自动处理。</span>
            </div>
          </div>
        </el-card>
      </el-col>
      <el-col :xs="24" :md="8">
        <el-card shadow="hover">
          <template #header>
            <h2>运维提示</h2>
          </template>
          <p class="section-desc">这里保留最常用的快捷入口和状态摘要，适合日常巡检和能力回溯。</p>
          <div class="context-list">
            <div class="context-item">
              <strong>管理入口</strong>
              <span>上游、下游和日志都在左侧导航中可直接切换。</span>
            </div>
            <div class="context-item">
              <strong>能力路线</strong>
              <span>Responses 优先保留工具面，ChatCompletions 只承接 function 工具。</span>
            </div>
            <div class="context-item">
              <strong>模型容量</strong>
              <span>当前可见模型数为 {{ data.active_models }}，来自可用上游的合并路由结果。</span>
            </div>
            <div class="context-item">
              <strong>请求节奏</strong>
              <span>当前累计记录 {{ data.logs_count }} 条请求日志，用于排障、审计和降级追踪。</span>
            </div>
          </div>
        </el-card>
      </el-col>
    </el-row>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick } from 'vue'
import { ElMessage } from 'element-plus'
import { Refresh } from '@element-plus/icons-vue'
import { adminApi } from '@/api/admin'
import { loadEcharts } from '@/utils/echartsLoader'
import type { DashboardData, UsageLog } from '@/types'
import type { EChartsType } from 'echarts/core'
import { formatCompactNumber } from '@/utils/numberFormat'

type ChartRange = '1d' | '7d' | '30d'

const loading = ref(false)
const chartLoading = ref(false)
const overviewChartRef = ref<HTMLElement>()
let overviewChart: EChartsType | null = null
const chartRange = ref<ChartRange>('7d')

const chartSummary = ref({
  totalRequests: 0,
  successRate: 0,
  averageLatency: 0,
  totalTokens: 0
})

const data = ref<DashboardData>({
  upstreams_count: 0,
  upstreams_active: 0,
  downstreams_count: 0,
  downstreams_active: 0,
  logs_count: 0,
  active_models: 0,
  responses_upstreams: 0,
  admin_username: 'admin',
  app_name: 'chat-responses-codex'
})

const formatCompactToken = (value: number) => formatCompactNumber(value)

const daysByRange: Record<ChartRange, number> = {
  '1d': 1,
  '7d': 7,
  '30d': 30
}

const toDayKey = (date: Date) => {
  const year = date.getFullYear()
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

const toDayLabel = (date: Date) => {
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${month}/${day}`
}

interface DayBucket {
  key: string
  label: string
  requests: number
  tokens: number
  latencyTotal: number
}

const buildBuckets = (range: ChartRange) => {
  const days = daysByRange[range]
  const today = new Date()
  today.setHours(0, 0, 0, 0)

  const buckets: DayBucket[] = []
  const bucketIndex = new Map<string, number>()

  for (let offset = days - 1; offset >= 0; offset -= 1) {
    const date = new Date(today)
    date.setDate(today.getDate() - offset)
    const key = toDayKey(date)
    buckets.push({
      key,
      label: toDayLabel(date),
      requests: 0,
      tokens: 0,
      latencyTotal: 0
    })
    bucketIndex.set(key, buckets.length - 1)
  }

  return { buckets, bucketIndex }
}

const updateChart = (logs: UsageLog[]) => {
  if (!overviewChart) return

  const { buckets, bucketIndex } = buildBuckets(chartRange.value)
  let successCount = 0
  let totalLatency = 0
  let totalTokens = 0

  logs.forEach(log => {
    if (log.status_code >= 200 && log.status_code < 300) successCount += 1
    totalLatency += log.latency_ms
    totalTokens += log.total_tokens

    const date = new Date(log.created_at * 1000)
    date.setHours(0, 0, 0, 0)
    const index = bucketIndex.get(toDayKey(date))
    if (index === undefined) return

    buckets[index].requests += 1
    buckets[index].tokens += log.total_tokens
    buckets[index].latencyTotal += log.latency_ms
  })

  const requestSeries = buckets.map(item => item.requests)
  const tokenSeries = buckets.map(item => item.tokens)
  const latencySeries = buckets.map(item => (item.requests > 0 ? Math.round(item.latencyTotal / item.requests) : 0))
  const labels = buckets.map(item => item.label)

  chartSummary.value = {
    totalRequests: logs.length,
    successRate: logs.length > 0 ? Number(((successCount / logs.length) * 100).toFixed(1)) : 0,
    averageLatency: logs.length > 0 ? Math.round(totalLatency / logs.length) : 0,
    totalTokens
  }

  overviewChart.setOption({
    tooltip: { trigger: 'axis' },
    legend: { data: ['请求次数', 'Token 总量', '平均耗时'] },
    grid: { left: 40, right: 40, top: 40, bottom: 30 },
    xAxis: {
      type: 'category',
      data: labels
    },
    yAxis: [
      {
        type: 'value',
        name: '请求/Token'
      },
      {
        type: 'value',
        name: '耗时(ms)'
      }
    ],
    series: [
      {
        name: '请求次数',
        type: 'line',
        smooth: true,
        data: requestSeries,
        itemStyle: { color: '#409EFF' },
        areaStyle: { color: 'rgba(64, 158, 255, 0.12)' }
      },
      {
        name: 'Token 总量',
        type: 'bar',
        data: tokenSeries,
        itemStyle: { color: '#67C23A' }
      },
      {
        name: '平均耗时',
        type: 'line',
        smooth: true,
        yAxisIndex: 1,
        data: latencySeries,
        itemStyle: { color: '#E6A23C' }
      }
    ]
  })
}

const initChart = async () => {
  if (!overviewChartRef.value) return
  const echarts = await loadEcharts()
  overviewChart = echarts.init(overviewChartRef.value)
}

const loadOverviewChart = async () => {
  try {
    chartLoading.value = true
    const { data: firstPage } = await adminApi.getLogs({
      page: 1,
      page_size: 200,
      time_range: chartRange.value
    })

    const logs: UsageLog[] = [...firstPage.logs]
    if (firstPage.total_pages > 1) {
      const remainingPages = Array.from({ length: firstPage.total_pages - 1 }, (_, index) => index + 2)
      const pageResponses = await Promise.all(
        remainingPages.map(page =>
          adminApi.getLogs({
            page,
            page_size: 200,
            time_range: chartRange.value
          })
        )
      )
      pageResponses.forEach(response => {
        logs.push(...response.data.logs)
      })
    }

    updateChart(logs)
  } catch (error) {
    ElMessage.error('加载统计图失败')
  } finally {
    chartLoading.value = false
  }
}

const loadData = async () => {
  try {
    loading.value = true
    const response = await adminApi.getDashboard()
    data.value = response.data
    await nextTick()
    await loadOverviewChart()
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const handleResize = () => {
  overviewChart?.resize()
}

onMounted(async () => {
  await nextTick()
  await initChart()
  await loadData()
  window.addEventListener('resize', handleResize)
})

onUnmounted(() => {
  overviewChart?.dispose()
  window.removeEventListener('resize', handleResize)
})
</script>

<style scoped>
.dashboard-container {
  padding: 20px;
}

.hero-section {
  background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
  color: white;
  padding: 40px;
  border-radius: 8px;
  margin-bottom: 30px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 40px;
}

.hero-content {
  flex: 1;
}

.hero-content h1 {
  margin: 0 0 10px 0;
  font-size: 28px;
}

.hero-content p {
  margin: 0;
  opacity: 0.9;
  line-height: 1.6;
}

.hero-actions {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
}

.summary-grid {
  margin-bottom: 30px;
}

.summary-card {
  background: white;
  border: 1px solid #e0e0e0;
  border-radius: 8px;
  padding: 20px;
  text-align: center;
  transition: all 0.3s ease;
}

.summary-card:hover {
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.1);
  border-color: #667eea;
}

.card-number {
  font-size: 32px;
  font-weight: bold;
  color: #667eea;
  margin-bottom: 8px;
}

.card-label {
  font-size: 14px;
  color: #666;
  margin-bottom: 8px;
}

.card-detail {
  font-size: 12px;
  color: #999;
}

.stats-card {
  margin-bottom: 30px;
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  width: 100%;
}

.card-header h2 {
  margin: 0;
  font-size: 18px;
}

.header-actions {
  display: flex;
  gap: 8px;
  align-items: center;
}

.chart-summary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: 12px;
  margin-bottom: 16px;
}

.summary-chip {
  border: 1px solid #ebeef5;
  border-radius: 8px;
  padding: 10px 12px;
  background: #f8fafc;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.summary-chip strong {
  color: #1f2937;
  font-size: 20px;
}

.summary-chip span {
  color: #64748b;
  font-size: 12px;
}

.overview-chart {
  width: 100%;
  height: 320px;
}

.capability-grid {
  margin-bottom: 30px;
}

.capability-card {
  height: 100%;
}

.capability-card h3 {
  margin: 0 0 10px 0;
  color: #667eea;
}

.capability-card strong {
  display: block;
  margin-bottom: 8px;
  color: #333;
}

.capability-card p {
  margin: 0;
  font-size: 14px;
  color: #666;
  line-height: 1.6;
}

.info-grid {
  margin-bottom: 20px;
}

.section-desc {
  margin: 0 0 20px 0;
  color: #666;
  line-height: 1.6;
}

.context-list {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.context-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.context-item strong {
  color: #333;
  font-size: 14px;
}

.context-item span {
  color: #666;
  font-size: 13px;
  line-height: 1.5;
}

@media (max-width: 768px) {
  .hero-section {
    flex-direction: column;
    align-items: flex-start;
  }

  .header-actions {
    width: 100%;
    justify-content: flex-end;
  }
}
</style>
