<template>
  <div class="dashboard-page">
    <section class="hero-panel">
      <div class="hero-panel__body">
        <p class="eyebrow">ENTERPRISE DASHBOARD</p>
        <h1>控制台总览</h1>
        <p class="hero-copy">全部统计来自本地聚合数据，适合内网巡检、容量判断和异常回溯。</p>
      </div>
      <div class="hero-panel__meta">
        <div class="hero-panel__chips">
          <el-tag effect="light" type="success">自动聚合</el-tag>
          <el-tag effect="plain">{{ rangeLabel }}</el-tag>
          <el-tag effect="plain">Responses 上游 {{ dashboard.responses_upstreams }}</el-tag>
        </div>
        <div class="hero-panel__controls">
          <el-radio-group v-model="chartRange" size="small" @change="handleRangeChange">
            <el-radio-button label="1d">1 天</el-radio-button>
            <el-radio-button label="7d">7 天</el-radio-button>
            <el-radio-button label="30d">30 天</el-radio-button>
          </el-radio-group>
          <el-button :icon="Refresh" :loading="loading" size="small" circle @click="loadDashboard" />
        </div>
        <div class="refresh-label">最近刷新 {{ refreshedLabel }}</div>
      </div>
    </section>

    <div v-loading="loading" class="kpi-wrap">
      <el-row :gutter="20" class="kpi-grid">
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--blue">
            <div class="metric-card__value">{{ dashboard.upstreams_count }}</div>
            <div class="metric-card__label">上游密钥</div>
            <div class="metric-card__detail">启用 {{ dashboard.upstreams_active }} / 共 {{ dashboard.upstreams_count }}</div>
          </div>
        </el-col>
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--teal">
            <div class="metric-card__value">{{ dashboard.downstreams_count }}</div>
            <div class="metric-card__label">下游密钥</div>
            <div class="metric-card__detail">启用 {{ dashboard.downstreams_active }} / 共 {{ dashboard.downstreams_count }}</div>
          </div>
        </el-col>
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--amber">
            <div class="metric-card__value">{{ dashboard.logs_count }}</div>
            <div class="metric-card__label">运行日志</div>
            <div class="metric-card__detail">最近记录 {{ dashboard.logs_count }} 条</div>
          </div>
        </el-col>
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--violet">
            <div class="metric-card__value">{{ dashboard.active_models }}</div>
            <div class="metric-card__label">可见模型</div>
            <div class="metric-card__detail">{{ dashboard.responses_upstreams }} 个 Responses 上游在线</div>
          </div>
        </el-col>
      </el-row>
    </div>

    <div class="status-strip">
      <div class="status-pill">
        <span>当前范围</span>
        <strong>{{ rangeLabel }}</strong>
      </div>
      <div class="status-pill">
        <span>Responses 上游</span>
        <strong>{{ dashboard.responses_upstreams }}</strong>
      </div>
      <div class="status-pill">
        <span>最近刷新</span>
        <strong>{{ refreshedLabel }}</strong>
      </div>
    </div>

    <el-row :gutter="20" class="charts-grid model-health-grid">
      <el-col :xs="24">
        <el-card shadow="hover" class="chart-card model-health-card" v-loading="modelProbeLoading">
          <template #header>
            <div class="card-header card-header--trend">
              <div>
                <h2>模型探测健康</h2>
                <p>通道模型探测快照，不影响主控制台数据加载。</p>
              </div>
              <div class="model-health-actions">
                <el-tag effect="plain">轮询 {{ modelProbeRefreshIntervalLabel }}</el-tag>
                <el-button :icon="View" type="primary" plain size="small" @click="openModelProbe">
                  查看探测
                </el-button>
              </div>
            </div>
          </template>

          <div class="chart-summary model-health-summary">
            <div class="summary-chip summary-chip--success">
              <strong>{{ modelProbeSummary.healthy_channels }}</strong>
              <span>健康通道</span>
            </div>
            <div class="summary-chip summary-chip--warning">
              <strong>{{ modelProbeSummary.degraded_channels }}</strong>
              <span>降级通道</span>
            </div>
            <div class="summary-chip summary-chip--danger">
              <strong>{{ modelProbeSummary.offline_channels }}</strong>
              <span>离线通道</span>
            </div>
            <div class="summary-chip">
              <strong>{{ modelProbeSummary.average_latency_ms }}ms</strong>
              <span>平均探测耗时</span>
            </div>
            <div class="summary-chip summary-chip--wide">
              <strong>{{ modelProbeRefreshedLabel }}</strong>
              <span>最近探测</span>
            </div>
          </div>

          <el-alert
            v-if="modelProbeError"
            :title="modelProbeError"
            type="error"
            show-icon
            :closable="false"
            class="model-health-alert"
          />
        </el-card>
      </el-col>
    </el-row>

    <el-row :gutter="20" class="charts-grid">
      <el-col :xs="24">
        <el-card shadow="hover" class="chart-card chart-card--trend">
          <template #header>
            <div class="card-header card-header--trend">
              <div>
                <h2>请求趋势</h2>
                <p>请求量、Token 总量和平均耗时的组合视图。</p>
              </div>
              <el-tag effect="plain">{{ rangeLabel }} · {{ analytics.daily_series.length }} 天</el-tag>
            </div>
          </template>

          <div class="chart-summary">
            <div class="summary-chip">
              <strong>{{ formatCompactNumber(chartSummary.total_requests) }}</strong>
              <span>请求次数</span>
            </div>
            <div class="summary-chip">
              <strong>{{ formatPercentageLabel(chartSummary.success_rate) }}</strong>
              <span>成功率</span>
            </div>
            <div class="summary-chip">
              <strong>{{ chartSummary.average_latency_ms }}ms</strong>
              <span>平均耗时</span>
            </div>
            <div class="summary-chip">
              <strong>{{ formatCompactNumber(chartSummary.total_tokens) }}</strong>
              <span>Token 总量</span>
            </div>
          </div>

          <div ref="trendChartRef" v-loading="loading" class="chart chart--trend"></div>
        </el-card>
      </el-col>
    </el-row>

    <el-row :gutter="20" class="charts-grid">
      <el-col :xs="24" :lg="8">
        <el-card shadow="hover" class="chart-card">
          <template #header>
            <div class="card-header">
              <div>
                <h2>模型使用</h2>
                <p>按调用次数排序的 Top 模型分布。</p>
              </div>
              <el-tag effect="plain">{{ modelUsage.total }} 次</el-tag>
            </div>
          </template>

          <div class="chart-summary chart-summary--compact">
            <div class="summary-chip">
              <strong>{{ modelUsage.items[0]?.name ?? '-' }}</strong>
              <span>Top 模型 · {{ modelUsage.items[0]?.value ?? 0 }}</span>
            </div>
            <div class="summary-chip">
              <strong>{{ modelUsage.items.length }}</strong>
              <span>图例项</span>
            </div>
          </div>

          <div ref="modelUsageChartRef" v-loading="loading" class="chart chart--medium"></div>
        </el-card>
      </el-col>

      <el-col :xs="24" :lg="8">
        <el-card shadow="hover" class="chart-card">
          <template #header>
            <div class="card-header">
              <div>
                <h2>客户端分布</h2>
                <p>下游账号调用次数的横向排行。</p>
              </div>
              <el-tag effect="plain">{{ downstreamUsage.total }} 次</el-tag>
            </div>
          </template>

          <div class="chart-summary chart-summary--compact">
            <div class="summary-chip">
              <strong>{{ downstreamUsage.items[0]?.name ?? '-' }}</strong>
              <span>Top 客户端 · {{ downstreamUsage.items[0]?.value ?? 0 }}</span>
            </div>
            <div class="summary-chip">
              <strong>{{ downstreamUsage.items.length }}</strong>
              <span>图例项</span>
            </div>
          </div>

          <div ref="downstreamUsageChartRef" v-loading="loading" class="chart chart--medium"></div>
        </el-card>
      </el-col>

      <el-col :xs="24" :lg="8">
        <el-card shadow="hover" class="chart-card">
          <template #header>
            <div class="card-header">
              <div>
                <h2>失败分类</h2>
                <p>按错误类别拆分的失败请求结构。</p>
              </div>
              <el-tag effect="plain">{{ failureUsage.total }} 次</el-tag>
            </div>
          </template>

          <div class="chart-summary chart-summary--compact">
            <div class="summary-chip">
              <strong>{{ failureSummary.totalFailed }}</strong>
              <span>失败总数</span>
            </div>
            <div class="summary-chip">
              <strong>{{ failureSummary.contextErrors }}</strong>
              <span>400 上下文</span>
            </div>
            <div class="summary-chip">
              <strong>{{ failureSummary.quotaErrors }}</strong>
              <span>429 配额/限流</span>
            </div>
            <div class="summary-chip">
              <strong>{{ failureSummary.upstreamErrors }}</strong>
              <span>5xx 上游异常</span>
            </div>
          </div>

          <div ref="failureChartRef" v-loading="loading" class="chart chart--medium"></div>
        </el-card>
      </el-col>
    </el-row>

    <el-row :gutter="20" class="charts-grid">
      <el-col :xs="24">
        <el-card shadow="hover" class="chart-card">
          <template #header>
            <div class="card-header card-header--trend">
              <div>
                <h2>User-Agent 聚类</h2>
                <p>下游客户端的聚类结果与覆盖数。</p>
              </div>
              <el-tag effect="plain">{{ userAgentSummary.clusterCount }} 个聚类</el-tag>
            </div>
          </template>

          <div class="chart-summary">
            <div class="summary-chip">
              <strong>{{ userAgentSummary.totalDownstreams }}</strong>
              <span>累计下游数</span>
            </div>
            <div class="summary-chip">
              <strong>{{ userAgentSummary.topCluster }}</strong>
              <span>Top UA · {{ userAgentSummary.topClusterCount }} 个下游</span>
            </div>
            <div class="summary-chip">
              <strong>{{ userAgentUsage.total }}</strong>
              <span>覆盖总量</span>
            </div>
            <div class="summary-chip">
              <strong>{{ userAgentUsage.items.length }}</strong>
              <span>图例项</span>
            </div>
          </div>

          <div ref="userAgentChartRef" v-loading="loading" class="chart chart--wide"></div>
        </el-card>
      </el-col>
    </el-row>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { Refresh, View } from '@element-plus/icons-vue'
import { adminApi } from '@/api/admin'
import type { DashboardAnalyticsRange, DashboardBreakdownItem, DashboardData, ModelProbeResponse } from '@/types'
import { loadEcharts } from '@/utils/echartsLoader'
import { buildUserAgentChartSummary } from '@/utils/userAgentChart'
import { formatCompactNumber } from '@/utils/numberFormat'
import { formatPercentageLabel } from '@/utils/percentage'
import { groupTopBreakdownItems } from '@/utils/dashboardCharts'
import { DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS } from '@/utils/modelProbePolling'
import type { EChartsType } from 'echarts/core'

type ChartRange = '1d' | '7d' | '30d'

const router = useRouter()
const loading = ref(false)
const modelProbeLoading = ref(false)
const modelProbeError = ref('')
const chartRange = ref<ChartRange>('7d')
const lastRefreshedAt = ref(0)

const dashboard = ref<DashboardData>({
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

const analytics = ref<DashboardAnalyticsRange>({
  range: '7d',
  summary: {
    total_requests: 0,
    success_rate: 0,
    average_latency_ms: 0,
    total_tokens: 0
  },
  daily_series: [],
  failure_categories: [],
  user_agent_clusters: [],
  model_usage: [],
  downstream_usage: []
})

const createEmptyModelProbe = (): ModelProbeResponse => ({
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

const modelProbe = ref<ModelProbeResponse>(createEmptyModelProbe())

const trendChartRef = ref<HTMLElement>()
const modelUsageChartRef = ref<HTMLElement>()
const downstreamUsageChartRef = ref<HTMLElement>()
const failureChartRef = ref<HTMLElement>()
const userAgentChartRef = ref<HTMLElement>()

let trendChart: EChartsType | null = null
let modelUsageChart: EChartsType | null = null
let downstreamUsageChart: EChartsType | null = null
let failureChart: EChartsType | null = null
let userAgentChart: EChartsType | null = null

const rangeLabelMap: Record<ChartRange, string> = {
  '1d': '1 天',
  '7d': '7 天',
  '30d': '30 天'
}

const compactColorPalette = ['#2563eb', '#0f766e', '#f59e0b', '#8b5cf6', '#ec4899', '#14b8a6', '#ef4444', '#64748b']

const rangeLabel = computed(() => rangeLabelMap[chartRange.value])

const refreshedLabel = computed(() => {
  if (!lastRefreshedAt.value) return '等待刷新'
  return new Date(lastRefreshedAt.value).toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  })
})

const modelProbeRefreshedLabel = computed(() => {
  if (!modelProbe.value.refreshed_at) return '等待刷新'
  return new Date(modelProbe.value.refreshed_at * 1000).toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  })
})

const modelProbeRefreshIntervalLabel = computed(
  () => `${modelProbe.value.refresh_interval_seconds || DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS}s`
)

const chartSummary = computed(() => analytics.value.summary)
const modelProbeSummary = computed(() => modelProbe.value.summary)

const modelUsage = computed(() => groupTopBreakdownItems(analytics.value.model_usage, 7))
const downstreamUsage = computed(() => groupTopBreakdownItems(analytics.value.downstream_usage, 8))
const failureUsage = computed(() => groupTopBreakdownItems(analytics.value.failure_categories, 6))
const userAgentUsage = computed(() => groupTopBreakdownItems(analytics.value.user_agent_clusters, 8))

const failureSummary = computed(() => {
  const items = analytics.value.failure_categories
  const totalFailed = items.reduce((sum, item) => sum + item.value, 0)
  const findValue = (name: string) => items.find(item => item.name === name)?.value ?? 0

  return {
    totalFailed,
    contextErrors: findValue('400-上下文超限'),
    quotaErrors: findValue('429-配额/限流'),
    upstreamErrors: findValue('5xx-上游异常')
  }
})

const userAgentSummary = computed(() => buildUserAgentChartSummary(analytics.value.user_agent_clusters))

const toShortDate = (timestamp: number) =>
  new Date(timestamp * 1000).toLocaleDateString('zh-CN', {
    month: '2-digit',
    day: '2-digit'
  })

const safeBreakdownSeries = (items: DashboardBreakdownItem[]) => (items.length > 0 ? items : [])

const renderTrendChart = () => {
  if (!trendChart) return

  const series = analytics.value.daily_series
  const hasData = series.length > 0
  const labels = series.map(item => toShortDate(item.date))
  const requestSeries = series.map(item => item.requests)
  const tokenSeries = series.map(item => item.tokens)
  const latencySeries = series.map(item => item.avg_latency_ms)

  trendChart.setOption({
    color: ['#2563eb', '#14b8a6', '#f59e0b'],
    tooltip: {
      trigger: 'axis',
      axisPointer: {
        type: 'line'
      },
      formatter: (params: any) => {
        if (!Array.isArray(params) || params.length === 0) {
          return ''
        }

        return `${params[0].axisValue}<br/>${params
          .map(item => {
            const value = Number(item.value ?? 0)
            const formattedValue =
              item.seriesName === 'Token 总量'
                ? formatCompactNumber(value)
                : value.toLocaleString('zh-CN')

            return `${item.marker}${item.seriesName}: ${formattedValue}`
          })
          .join('<br/>')}`
      }
    },
    legend: {
      data: ['请求次数', 'Token 总量', '平均耗时'],
      top: 0,
      left: 0,
      icon: 'circle',
      itemWidth: 10,
      itemHeight: 10,
      textStyle: {
        color: '#475569'
      }
    },
    grid: {
      left: 50,
      right: 56,
      top: 48,
      bottom: 28,
      containLabel: true
    },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: labels,
      axisLine: {
        lineStyle: {
          color: 'rgba(148, 163, 184, 0.38)'
        }
      },
      axisLabel: {
        color: '#64748b'
      }
    },
    yAxis: [
      {
        type: 'value',
        name: '请求次数',
        axisLabel: {
          color: '#64748b',
          formatter: (value: number) => value.toLocaleString('zh-CN')
        },
        splitLine: {
          lineStyle: {
            color: 'rgba(148, 163, 184, 0.16)'
          }
        }
      },
      {
        type: 'value',
        name: '耗时(ms)',
        position: 'right',
        axisLabel: {
          color: '#64748b',
          formatter: (value: number) => value.toLocaleString('zh-CN')
        },
        splitLine: {
          show: false
        }
      },
      {
        type: 'value',
        name: 'Token',
        position: 'right',
        offset: 56,
        axisLabel: {
          color: '#64748b',
          formatter: (value: number) => formatCompactNumber(value)
        },
        splitLine: {
          show: false
        }
      }
    ],
    series: [
      {
        name: '请求次数',
        type: 'line',
        smooth: true,
        showSymbol: false,
        data: requestSeries,
        itemStyle: {
          color: '#2563eb'
        },
        areaStyle: {
          color: 'rgba(37, 99, 235, 0.12)'
        }
      },
      {
        name: 'Token 总量',
        type: 'bar',
        yAxisIndex: 2,
        data: tokenSeries,
        barWidth: 12,
        itemStyle: {
          color: '#14b8a6',
          borderRadius: [8, 8, 0, 0]
        }
      },
      {
        name: '平均耗时',
        type: 'line',
        smooth: true,
        showSymbol: false,
        yAxisIndex: 1,
        data: latencySeries,
        itemStyle: {
          color: '#f59e0b'
        }
      }
    ],
    graphic: hasData
      ? []
      : [
          {
            type: 'text',
            left: 'center',
            top: 'middle',
            style: {
              text: '暂无趋势数据',
              fill: '#94a3b8',
              fontSize: 14
            }
          }
        ]
  })
}

const renderDonutChart = (
  chart: EChartsType | null,
  items: DashboardBreakdownItem[],
  colors: string[],
  emptyLabel: string
) => {
  if (!chart) return

  const hasData = items.length > 0

  chart.setOption({
    color: colors,
    tooltip: {
      trigger: 'item'
    },
    legend: {
      type: 'scroll',
      bottom: 0,
      left: 'center',
      icon: 'circle',
      itemWidth: 10,
      itemHeight: 10,
      textStyle: {
        color: '#475569'
      }
    },
    series: [
      {
        type: 'pie',
        radius: ['48%', '72%'],
        center: ['50%', '42%'],
        avoidLabelOverlap: true,
        itemStyle: {
          borderColor: '#fff',
          borderWidth: 2
        },
        label: {
          color: '#334155',
          formatter: '{b}\n{c}'
        },
        labelLine: {
          length: 14,
          length2: 10
        },
        data: items
      }
    ],
    graphic: hasData
      ? []
      : [
          {
            type: 'text',
            left: 'center',
            top: 'middle',
            style: {
              text: emptyLabel,
              fill: '#94a3b8',
              fontSize: 14
            }
          }
        ]
  })
}

const renderRankChart = (
  chart: EChartsType | null,
  items: DashboardBreakdownItem[],
  color: string,
  emptyLabel: string
) => {
  if (!chart) return

  const hasData = items.length > 0
  const names = items.map(item => item.name)
  const values = items.map(item => item.value)

  chart.setOption({
    tooltip: {
      trigger: 'axis',
      axisPointer: {
        type: 'shadow'
      }
    },
    grid: {
      left: 132,
      right: 28,
      top: 12,
      bottom: 20,
      containLabel: true
    },
    xAxis: {
      type: 'value',
      axisLabel: {
        color: '#64748b'
      },
      splitLine: {
        lineStyle: {
          color: 'rgba(148, 163, 184, 0.16)'
        }
      }
    },
    yAxis: {
      type: 'category',
      inverse: true,
      data: names,
      axisLabel: {
        color: '#334155',
        width: 110,
        overflow: 'truncate'
      }
    },
    series: [
      {
        type: 'bar',
        data: values,
        barWidth: 14,
        itemStyle: {
          color,
          borderRadius: [0, 10, 10, 0]
        },
        label: {
          show: true,
          position: 'right',
          color: '#0f172a'
        }
      }
    ],
    graphic: hasData
      ? []
      : [
          {
            type: 'text',
            left: 'center',
            top: 'middle',
            style: {
              text: emptyLabel,
              fill: '#94a3b8',
              fontSize: 14
            }
          }
        ]
  })
}

const renderCharts = () => {
  renderTrendChart()
  renderDonutChart(
    modelUsageChart,
    safeBreakdownSeries(modelUsage.value.items),
    compactColorPalette,
    '暂无模型使用数据'
  )
  renderRankChart(
    downstreamUsageChart,
    safeBreakdownSeries(downstreamUsage.value.items),
    '#2563eb',
    '暂无客户端数据'
  )
  renderDonutChart(
    failureChart,
    safeBreakdownSeries(failureUsage.value.items),
    ['#ef4444', '#f59e0b', '#8b5cf6', '#14b8a6', '#2563eb', '#64748b'],
    '暂无失败数据'
  )
  renderRankChart(
    userAgentChart,
    safeBreakdownSeries(userAgentUsage.value.items),
    '#7c3aed',
    '暂无 User-Agent 数据'
  )
}

const initCharts = async () => {
  const echarts = await loadEcharts()

  if (trendChartRef.value) {
    trendChart = echarts.init(trendChartRef.value)
  }
  if (modelUsageChartRef.value) {
    modelUsageChart = echarts.init(modelUsageChartRef.value)
  }
  if (downstreamUsageChartRef.value) {
    downstreamUsageChart = echarts.init(downstreamUsageChartRef.value)
  }
  if (failureChartRef.value) {
    failureChart = echarts.init(failureChartRef.value)
  }
  if (userAgentChartRef.value) {
    userAgentChart = echarts.init(userAgentChartRef.value)
  }
}

const loadModelProbe = async () => {
  if (modelProbeLoading.value) return

  try {
    modelProbeLoading.value = true
    modelProbeError.value = ''
    const response = await adminApi.getModelProbe()
    modelProbe.value = response.data
  } catch (error: any) {
    modelProbeError.value = error?.response?.data?.error?.message || '加载模型探测失败'
  } finally {
    modelProbeLoading.value = false
  }
}

const loadDashboard = async () => {
  try {
    loading.value = true
    const response = await adminApi.getDashboard(chartRange.value)
    dashboard.value = response.data.dashboard
    analytics.value = response.data.analytics
    lastRefreshedAt.value = Date.now()
    await nextTick()
    renderCharts()
    void loadModelProbe()
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const handleRangeChange = (value: ChartRange) => {
  chartRange.value = value
  loadDashboard()
}

const openModelProbe = () => {
  void router.push('/admin/model-probe')
}

const handleResize = () => {
  trendChart?.resize()
  modelUsageChart?.resize()
  downstreamUsageChart?.resize()
  failureChart?.resize()
  userAgentChart?.resize()
}

onMounted(async () => {
  await nextTick()
  await initCharts()
  await loadDashboard()
  window.addEventListener('resize', handleResize)
})

onUnmounted(() => {
  window.removeEventListener('resize', handleResize)
  trendChart?.dispose()
  modelUsageChart?.dispose()
  downstreamUsageChart?.dispose()
  failureChart?.dispose()
  userAgentChart?.dispose()
  trendChart = null
  modelUsageChart = null
  downstreamUsageChart = null
  failureChart = null
  userAgentChart = null
})
</script>

<style scoped>
.dashboard-page {
  min-height: 100%;
  padding: 20px;
  background:
    radial-gradient(circle at top left, rgba(37, 99, 235, 0.08), transparent 26%),
    radial-gradient(circle at top right, rgba(14, 165, 233, 0.08), transparent 24%),
    linear-gradient(180deg, #f8fbff 0%, #f4f7fb 100%);
}

.hero-panel {
  display: flex;
  justify-content: space-between;
  gap: 24px;
  padding: 26px 28px;
  border-radius: 24px;
  color: #f8fafc;
  background:
    radial-gradient(circle at top left, rgba(56, 189, 248, 0.24), transparent 34%),
    radial-gradient(circle at top right, rgba(129, 140, 248, 0.22), transparent 28%),
    linear-gradient(135deg, #0f172a 0%, #111827 44%, #1f2937 100%);
  box-shadow: 0 24px 48px rgba(15, 23, 42, 0.18);
}

.hero-panel__body h1 {
  margin: 0;
  font-size: 32px;
  line-height: 1.1;
  letter-spacing: -0.02em;
}

.hero-copy {
  margin: 12px 0 0;
  max-width: 64ch;
  color: rgba(226, 232, 240, 0.84);
  line-height: 1.7;
}

.eyebrow {
  margin: 0 0 10px;
  color: rgba(191, 219, 254, 0.88);
  font-size: 12px;
  letter-spacing: 0.18em;
  text-transform: uppercase;
}

.hero-panel__meta {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  justify-content: center;
  gap: 10px;
  white-space: nowrap;
}

.hero-panel__chips {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.hero-panel__controls {
  display: flex;
  align-items: center;
  gap: 10px;
}

.refresh-label {
  font-size: 13px;
  color: rgba(226, 232, 240, 0.72);
}

.kpi-wrap {
  margin-top: 18px;
}

.kpi-grid {
  margin: 0;
}

.metric-card {
  position: relative;
  min-height: 128px;
  padding: 18px 20px;
  border-radius: 20px;
  background: linear-gradient(180deg, #ffffff 0%, #f8fafc 100%);
  border: 1px solid rgba(148, 163, 184, 0.14);
  box-shadow: 0 16px 32px rgba(15, 23, 42, 0.06);
  overflow: hidden;
}

.metric-card::before {
  content: '';
  position: absolute;
  inset: 0 auto auto 0;
  width: 100%;
  height: 4px;
  background: var(--metric-accent, #2563eb);
}

.metric-card--blue {
  --metric-accent: #2563eb;
}

.metric-card--teal {
  --metric-accent: #14b8a6;
}

.metric-card--amber {
  --metric-accent: #f59e0b;
}

.metric-card--violet {
  --metric-accent: #8b5cf6;
}

.metric-card__value {
  font-size: 30px;
  font-weight: 700;
  color: #0f172a;
}

.metric-card__label {
  margin-top: 8px;
  font-size: 14px;
  color: #475569;
}

.metric-card__detail {
  margin-top: 10px;
  font-size: 12px;
  color: #94a3b8;
}

.status-strip {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 16px;
  margin-top: 18px;
}

.status-pill {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 14px 18px;
  border-radius: 18px;
  border: 1px solid rgba(148, 163, 184, 0.12);
  background: rgba(255, 255, 255, 0.72);
  backdrop-filter: blur(12px);
  box-shadow: 0 12px 24px rgba(15, 23, 42, 0.05);
}

.status-pill span {
  color: #64748b;
  font-size: 13px;
}

.status-pill strong {
  color: #0f172a;
  font-size: 14px;
  font-weight: 600;
}

.charts-grid {
  margin-top: 18px;
}

.model-health-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 10px;
  flex-wrap: wrap;
}

.model-health-summary {
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  margin-bottom: 0;
}

.model-health-card .summary-chip strong {
  font-size: 17px;
  overflow-wrap: anywhere;
}

.summary-chip--success {
  border-color: rgba(20, 184, 166, 0.24);
}

.summary-chip--warning {
  border-color: rgba(245, 158, 11, 0.26);
}

.summary-chip--danger {
  border-color: rgba(239, 68, 68, 0.24);
}

.summary-chip--wide {
  min-width: 210px;
}

.model-health-alert {
  margin-top: 14px;
  border-radius: 12px;
}

.chart-card {
  border: none;
  border-radius: 22px;
  overflow: hidden;
  box-shadow: 0 16px 32px rgba(15, 23, 42, 0.06);
}

.chart-card--trend {
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.98) 0%, rgba(248, 250, 252, 0.96) 100%);
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.card-header--trend {
  align-items: center;
}

.card-header h2 {
  margin: 0;
  font-size: 18px;
  color: #0f172a;
}

.card-header p {
  margin: 6px 0 0;
  color: #64748b;
  font-size: 13px;
}

.chart-summary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: 12px;
  margin-bottom: 16px;
}

.chart-summary--compact {
  margin-bottom: 12px;
}

.summary-chip {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px 14px;
  border-radius: 14px;
  border: 1px solid rgba(148, 163, 184, 0.12);
  background: linear-gradient(180deg, #ffffff 0%, #f8fafc 100%);
}

.summary-chip strong {
  color: #0f172a;
  font-size: 18px;
  line-height: 1.2;
}

.summary-chip span {
  color: #64748b;
  font-size: 12px;
}

.chart {
  width: 100%;
}

.chart--trend {
  height: 360px;
}

.chart--medium {
  height: 320px;
}

.chart--wide {
  height: 340px;
}

:deep(.el-card__header) {
  padding: 18px 20px 14px;
  border-bottom: 1px solid rgba(148, 163, 184, 0.12);
}

:deep(.el-card__body) {
  padding: 18px 20px 20px;
}

@media (max-width: 1200px) {
  .status-strip {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 768px) {
  .dashboard-page {
    padding: 14px;
  }

  .hero-panel {
    flex-direction: column;
    align-items: flex-start;
    padding: 22px 20px;
  }

  .hero-panel__meta {
    align-items: flex-start;
  }

  .hero-panel__chips,
  .hero-panel__controls {
    justify-content: flex-start;
  }

  .model-health-actions {
    justify-content: flex-start;
    width: 100%;
  }

  .chart--trend,
  .chart--medium,
  .chart--wide {
    height: 280px;
  }
}
</style>
