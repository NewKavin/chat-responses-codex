<template>
  <div class="crc-page dashboard-page">
    <header class="crc-page-header dashboard-header">
      <div class="dashboard-header__body">
        <h1 class="crc-page-title">控制台总览</h1>
        <p class="crc-page-description">查看网关资源、请求趋势、模型健康和客户端使用情况。</p>
      </div>
      <div class="dashboard-header__meta">
        <div class="dashboard-header__chips">
          <el-tag effect="light" type="success">自动聚合</el-tag>
          <el-tag effect="plain">{{ rangeLabel }}</el-tag>
          <el-tag effect="plain">Responses 上游 {{ dashboard.responses_upstreams }}</el-tag>
        </div>
        <div class="dashboard-header__controls">
          <el-radio-group v-model="chartRange" size="small" @change="handleRangeChange">
            <el-radio-button label="1d" value="1d">1 天</el-radio-button>
            <el-radio-button label="7d" value="7d">7 天</el-radio-button>
            <el-radio-button label="30d" value="30d">30 天</el-radio-button>
          </el-radio-group>
          <el-button
            :icon="Refresh"
            :loading="loading"
            size="small"
            circle
            aria-label="刷新控制台数据"
            title="刷新控制台数据"
            @click="loadDashboard"
          />
        </div>
        <div class="refresh-label">最近刷新 {{ refreshedLabel }}</div>
      </div>
    </header>

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
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
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
import { useTheme } from '@/composables/useTheme'
import { buildChartTheme } from '@/utils/chartTheme'
import type { EChartsType } from 'echarts/core'

type ChartRange = '1d' | '7d' | '30d'

const router = useRouter()
const { resolvedTheme } = useTheme()
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

const chartTheme = computed(() => buildChartTheme(resolvedTheme.value))

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

  const theme = chartTheme.value
  const series = analytics.value.daily_series
  const hasData = series.length > 0
  const labels = series.map(item => toShortDate(item.date))
  const requestSeries = series.map(item => item.requests)
  const tokenSeries = series.map(item => item.tokens)
  const latencySeries = series.map(item => item.avg_latency_ms)

  trendChart.setOption({
    color: theme.series.slice(0, 3),
    tooltip: {
      trigger: 'axis',
      backgroundColor: theme.tooltipBackground,
      borderColor: theme.tooltipBorder,
      textStyle: { color: theme.text },
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
        color: theme.muted
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
          color: theme.border
        }
      },
      axisLabel: {
        color: theme.muted
      }
    },
    yAxis: [
      {
        type: 'value',
        name: '请求次数',
        axisLabel: {
          color: theme.muted,
          formatter: (value: number) => value.toLocaleString('zh-CN')
        },
        splitLine: {
          lineStyle: {
            color: theme.splitLine
          }
        }
      },
      {
        type: 'value',
        name: '耗时(ms)',
        position: 'right',
        axisLabel: {
          color: theme.muted,
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
          color: theme.muted,
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
          color: theme.series[0]
        },
        areaStyle: {
          color: `${theme.series[0]}20`
        }
      },
      {
        name: 'Token 总量',
        type: 'bar',
        yAxisIndex: 2,
        data: tokenSeries,
        barWidth: 12,
        itemStyle: {
          color: theme.series[1],
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
          color: theme.series[2]
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
              fill: theme.muted,
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

  const theme = chartTheme.value
  const hasData = items.length > 0

  chart.setOption({
    color: colors,
    tooltip: {
      trigger: 'item',
      backgroundColor: theme.tooltipBackground,
      borderColor: theme.tooltipBorder,
      textStyle: { color: theme.text }
    },
    legend: {
      type: 'scroll',
      bottom: 0,
      left: 'center',
      icon: 'circle',
      itemWidth: 10,
      itemHeight: 10,
      textStyle: {
        color: theme.muted
      }
    },
    series: [
      {
        type: 'pie',
        radius: ['48%', '72%'],
        center: ['50%', '42%'],
        avoidLabelOverlap: true,
        itemStyle: {
          borderColor: theme.tooltipBackground,
          borderWidth: 2
        },
        label: {
          color: theme.text,
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
              fill: theme.muted,
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

  const theme = chartTheme.value
  const hasData = items.length > 0
  const names = items.map(item => item.name)
  const values = items.map(item => item.value)

  chart.setOption({
    tooltip: {
      trigger: 'axis',
      backgroundColor: theme.tooltipBackground,
      borderColor: theme.tooltipBorder,
      textStyle: { color: theme.text },
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
        color: theme.muted
      },
      splitLine: {
        lineStyle: {
          color: theme.splitLine
        }
      }
    },
    yAxis: {
      type: 'category',
      inverse: true,
      data: names,
      axisLabel: {
        color: theme.text,
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
          color: theme.text
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
              fill: theme.muted,
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
    chartTheme.value.series,
    '暂无模型使用数据'
  )
  renderRankChart(
    downstreamUsageChart,
    safeBreakdownSeries(downstreamUsage.value.items),
    chartTheme.value.series[0],
    '暂无客户端数据'
  )
  renderDonutChart(
    failureChart,
    safeBreakdownSeries(failureUsage.value.items),
    chartTheme.value.series.slice(2, 8),
    '暂无失败数据'
  )
  renderRankChart(
    userAgentChart,
    safeBreakdownSeries(userAgentUsage.value.items),
    chartTheme.value.series[5],
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

const disposeCharts = () => {
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
}

watch(resolvedTheme, async () => {
  disposeCharts()
  await nextTick()
  await initCharts()
  renderCharts()
})

onMounted(async () => {
  await nextTick()
  await initCharts()
  await loadDashboard()
  window.addEventListener('resize', handleResize)
})

onUnmounted(() => {
  window.removeEventListener('resize', handleResize)
  disposeCharts()
})
</script>

<style scoped>
.dashboard-page {
  min-height: 100%;
}

.dashboard-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
}

.dashboard-header__meta {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 8px;
  white-space: nowrap;
}

.dashboard-header__chips {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 6px;
}

.dashboard-header__controls {
  display: flex;
  align-items: center;
  gap: 8px;
}

.refresh-label {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.kpi-wrap {
  margin-top: 4px;
}

.kpi-grid {
  margin: 0;
}

.metric-card {
  position: relative;
  min-height: 118px;
  padding: 18px;
  overflow: hidden;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
  transition: transform var(--crc-duration) var(--crc-ease-out),
    box-shadow var(--crc-duration) var(--crc-ease-out),
    border-color var(--crc-duration) var(--crc-ease-out);
}

.metric-card:hover {
  border-color: var(--crc-border-strong);
  box-shadow: var(--crc-shadow-md);
  transform: translateY(-2px);
}

.metric-card::before {
  content: '';
  position: absolute;
  inset: 0 auto auto 0;
  width: 100%;
  height: 3px;
  background: linear-gradient(
    90deg,
    var(--metric-accent, var(--crc-accent)),
    transparent 160%
  );
}

.metric-card--blue {
  --metric-accent: var(--crc-info);
}

.metric-card--teal {
  --metric-accent: var(--crc-accent);
}

.metric-card--amber {
  --metric-accent: var(--crc-warning);
}

.metric-card--violet {
  --metric-accent: var(--crc-violet);
}

.metric-card__value {
  color: var(--crc-text-strong);
  font-size: 28px;
  font-weight: 680;
  line-height: 1.15;
  letter-spacing: -0.01em;
}

.metric-card__label {
  margin-top: 8px;
  font-size: 14px;
  color: var(--crc-text);
}

.metric-card__detail {
  margin-top: 10px;
  font-size: 12px;
  color: var(--crc-text-muted);
}

.status-strip {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
  margin-top: 14px;
}

.status-pill {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 12px 14px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.status-pill span {
  color: var(--crc-text-muted);
  font-size: 13px;
}

.status-pill strong {
  color: var(--crc-text-strong);
  font-size: 14px;
  font-weight: 600;
}

.charts-grid {
  margin-top: 16px;
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
  border-color: var(--crc-success);
}

.summary-chip--warning {
  border-color: var(--crc-warning);
}

.summary-chip--danger {
  border-color: var(--crc-danger);
}

.summary-chip--wide {
  min-width: 210px;
}

.model-health-alert {
  margin-top: 14px;
  border-radius: var(--crc-radius);
}

.chart-card {
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  overflow: hidden;
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.chart-card--trend {
  background: var(--crc-surface);
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
  color: var(--crc-text-strong);
  font-size: 16px;
}

.card-header p {
  margin: 6px 0 0;
  color: var(--crc-text-muted);
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
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface-muted);
}

.summary-chip strong {
  color: var(--crc-text-strong);
  font-size: 18px;
  line-height: 1.2;
}

.summary-chip span {
  color: var(--crc-text-muted);
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
  border-bottom: 1px solid var(--crc-border);
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
  .dashboard-header {
    flex-direction: column;
    align-items: flex-start;
  }

  .dashboard-header__meta {
    width: 100%;
    align-items: flex-start;
    white-space: normal;
  }

  .dashboard-header__chips,
  .dashboard-header__controls {
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
