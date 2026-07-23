<template>
  <div class="crc-page dashboard-page">
    <section class="dashboard-deck">
      <SignalWave class="dashboard-deck__wave" :layers="3" :packets="12" :intensity="0.85" />
      <div class="dashboard-deck__left">
        <p class="dashboard-deck__eyebrow">CONSOLE // OVERVIEW</p>
        <h1 class="dashboard-deck__title">控制台总览</h1>
        <p class="dashboard-deck__desc">查看网关资源、请求趋势、模型健康和客户端使用情况。</p>
        <div class="dashboard-header__chips">
          <el-tag effect="light" type="success">自动聚合</el-tag>
          <el-tag effect="plain">{{ rangeLabel }}</el-tag>
          <el-tag effect="plain">Responses 上游 {{ dashboard.responses_upstreams }}</el-tag>
        </div>
      </div>
      <div class="dashboard-deck__right">
        <div class="dashboard-deck__headline-stat">
          <span class="dashboard-deck__stat-label">TOTAL REQUESTS // {{ rangeLabel }}</span>
          <strong class="dashboard-deck__stat-value">{{ formatCompactNumber(chartSummary.total_requests) }}</strong>
          <span class="dashboard-deck__stat-sub">
            成功率 {{ formatPercentageLabel(chartSummary.success_rate) }} · 平均耗时 {{ chartSummary.average_latency_ms }}ms
          </span>
        </div>
        <div class="dashboard-header__controls">
          <el-radio-group v-model="chartRange" size="small" @change="handleRangeChange">
            <el-radio-button label="1d" value="1d">1 天</el-radio-button>
            <el-radio-button label="7d" value="7d">7 天</el-radio-button>
            <el-radio-button label="30d" value="30d">30 天</el-radio-button>
          </el-radio-group>
          <el-button
            :icon="RefreshCw"
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
    </section>

    <div class="kpi-wrap">
      <el-row v-if="showKpiSkeleton" :gutter="20" class="kpi-grid" aria-hidden="true">
        <el-col v-for="index in 4" :key="index" :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--skeleton">
            <el-skeleton animated>
              <template #template>
                <el-skeleton-item variant="h1" style="width: 42%" />
                <el-skeleton-item variant="text" style="width: 56%; margin-top: 16px" />
                <el-skeleton-item variant="text" style="width: 72%; margin-top: 12px" />
              </template>
            </el-skeleton>
          </div>
        </el-col>
      </el-row>
      <el-row v-else v-loading="loading" :gutter="20" class="kpi-grid">
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--blue">
            <div class="metric-card__top">
              <span class="metric-card__icon"><SatelliteDish :size="15" :stroke-width="1.8" /></span>
              <span class="metric-card__tag">UPSTREAM</span>
            </div>
            <div class="metric-card__value"><CountUpValue :value="dashboard.upstreams_count" /></div>
            <div class="metric-card__label">上游密钥</div>
            <div class="metric-card__detail">启用 {{ dashboard.upstreams_active }} / 共 {{ dashboard.upstreams_count }}</div>
          </div>
        </el-col>
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--teal">
            <div class="metric-card__top">
              <span class="metric-card__icon"><KeyRound :size="15" :stroke-width="1.8" /></span>
              <span class="metric-card__tag">DOWNSTREAM</span>
            </div>
            <div class="metric-card__value"><CountUpValue :value="dashboard.downstreams_count" /></div>
            <div class="metric-card__label">下游密钥</div>
            <div class="metric-card__detail">启用 {{ dashboard.downstreams_active }} / 共 {{ dashboard.downstreams_count }}</div>
          </div>
        </el-col>
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--amber">
            <div class="metric-card__top">
              <span class="metric-card__icon"><ScrollText :size="15" :stroke-width="1.8" /></span>
              <span class="metric-card__tag">LOGS</span>
            </div>
            <div class="metric-card__value"><CountUpValue :value="dashboard.logs_count" /></div>
            <div class="metric-card__label">运行日志</div>
            <div class="metric-card__detail">最近记录 {{ dashboard.logs_count }} 条</div>
          </div>
        </el-col>
        <el-col :xs="24" :sm="12" :lg="6">
          <div class="metric-card metric-card--violet">
            <div class="metric-card__top">
              <span class="metric-card__icon"><Boxes :size="15" :stroke-width="1.8" /></span>
              <span class="metric-card__tag">MODELS</span>
            </div>
            <div class="metric-card__value"><CountUpValue :value="dashboard.active_models" /></div>
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
                <p class="card-eyebrow">PROBE // HEALTH</p>
              <h2>模型探测健康</h2>
                <p>通道模型探测快照，不影响主控制台数据加载。</p>
              </div>
              <div class="model-health-actions">
                <el-tag effect="plain">轮询 {{ modelProbeRefreshIntervalLabel }}</el-tag>
                <el-button :icon="Radar" type="primary" plain size="small" @click="openModelProbe">
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
                <p class="card-eyebrow">TREND // TRAFFIC</p>
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
                <p class="card-eyebrow">DIST // MODELS</p>
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
                <p class="card-eyebrow">RANK // CLIENTS</p>
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
                <p class="card-eyebrow">DIST // FAILURES</p>
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
                <p class="card-eyebrow">CLUSTER // UA</p>
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
import { Boxes, KeyRound, Radar, RefreshCw, SatelliteDish, ScrollText } from '@lucide/vue'
import CountUpValue from '@/components/CountUpValue.vue'
import SignalWave from '@/components/SignalWave.vue'
import { adminApi } from '@/api/admin'
import type { DashboardAnalyticsRange, DashboardBreakdownItem, DashboardData, ModelProbeResponse } from '@/types'
import { loadEcharts } from '@/utils/echartsLoader'
import { buildUserAgentChartSummary } from '@/utils/userAgentChart'
import { formatCompactNumber } from '@/utils/numberFormat'
import { formatPercentageLabel } from '@/utils/percentage'
import { groupTopBreakdownItems } from '@/utils/dashboardCharts'
import { DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS } from '@/utils/modelProbePolling'
import { useTheme } from '@/composables/useTheme'
import { buildChartTheme, chartEnterAnimation } from '@/utils/chartTheme'
import type { EChartsType } from 'echarts/core'

type ChartRange = '1d' | '7d' | '30d'

const router = useRouter()
const { resolvedTheme } = useTheme()
const loading = ref(false)
const modelProbeLoading = ref(false)
const modelProbeError = ref('')
const chartRange = ref<ChartRange>('7d')
const lastRefreshedAt = ref(0)

const showKpiSkeleton = computed(() => loading.value && lastRefreshedAt.value === 0)

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
    ...chartEnterAnimation,
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
    ...chartEnterAnimation,
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
    ...chartEnterAnimation,
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

/* -- Command deck hero -------------------------------------------------------- */

.dashboard-deck {
  position: relative;
  display: flex;
  margin-bottom: 20px;
  padding: clamp(22px, 3vw, 38px);
  align-items: center;
  justify-content: space-between;
  gap: 36px;
  overflow: hidden;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg);
  background:
    radial-gradient(ellipse 60% 90% at 8% -10%, var(--crc-accent-soft) 0%, transparent 56%),
    radial-gradient(ellipse 50% 75% at 100% 115%, var(--crc-info-soft) 0%, transparent 60%),
    linear-gradient(140deg, var(--crc-surface) 0%, var(--crc-canvas) 100%);
  box-shadow: var(--crc-shadow-sm);
}

.dashboard-deck__wave {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  opacity: 0.5;
}

.dashboard-deck__left,
.dashboard-deck__right {
  position: relative;
  z-index: 1;
}

.dashboard-deck__left {
  max-width: 520px;
}

.dashboard-deck__eyebrow {
  display: flex;
  margin: 0 0 14px;
  align-items: center;
  gap: 10px;
  color: var(--crc-accent);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.14em;
}

.dashboard-deck__eyebrow::before {
  content: '';
  width: 24px;
  height: 1px;
  background: var(--crc-accent);
}

.dashboard-deck__title {
  margin: 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: clamp(28px, 3.4vw, 46px);
  font-weight: 600;
  letter-spacing: -0.02em;
  line-height: 1.12;
}

.dashboard-deck__desc {
  margin: 12px 0 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.7;
}

.dashboard-deck .dashboard-header__chips {
  display: flex;
  margin-top: 18px;
  flex-wrap: wrap;
  gap: 6px;
}

.dashboard-deck__right {
  display: flex;
  flex: 0 0 auto;
  flex-direction: column;
  align-items: flex-end;
  gap: 12px;
}

.dashboard-deck__headline-stat {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 4px;
}

.dashboard-deck__stat-label {
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.14em;
}

.dashboard-deck__stat-value {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: clamp(40px, 4.6vw, 64px);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.03em;
  line-height: 1;
}

.dashboard-deck__stat-sub {
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  letter-spacing: 0.04em;
}

.dashboard-header__controls {
  display: flex;
  align-items: center;
  gap: 8px;
}

.refresh-label {
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.06em;
}

/* -- KPI metric cards ----------------------------------------------------------- */

.kpi-wrap {
  margin-top: 4px;
}

.kpi-grid {
  margin: 0;
}

@keyframes kpi-card-in {
  from {
    opacity: 0;
    transform: translateY(12px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.kpi-grid .el-col {
  animation: kpi-card-in var(--crc-duration-slow) var(--crc-ease-expo) backwards;
}

.kpi-grid .el-col:nth-child(2) {
  animation-delay: 70ms;
}

.kpi-grid .el-col:nth-child(3) {
  animation-delay: 140ms;
}

.kpi-grid .el-col:nth-child(4) {
  animation-delay: 210ms;
}

.metric-card {
  position: relative;
  min-height: 138px;
  padding: 18px 20px;
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
  border-color: var(--metric-accent, var(--crc-border-strong));
  box-shadow: var(--crc-shadow-md);
  transform: translateY(-3px);
}

.metric-card--skeleton {
  pointer-events: none;
}

.metric-card--skeleton::before {
  content: none;
}

.metric-card::before {
  content: '';
  position: absolute;
  inset: 0 auto auto 0;
  width: 100%;
  height: 2px;
  background: linear-gradient(
    90deg,
    var(--metric-accent, var(--crc-accent)),
    transparent 160%
  );
}

.metric-card::after {
  content: '';
  position: absolute;
  right: -34px;
  bottom: -34px;
  width: 110px;
  height: 110px;
  border: 1px solid var(--metric-accent, var(--crc-accent));
  border-radius: 50%;
  opacity: 0.08;
  transition: opacity var(--crc-duration) var(--crc-ease),
    transform var(--crc-duration) var(--crc-ease-out);
}

.metric-card:hover::after {
  opacity: 0.2;
  transform: scale(1.12);
}

.metric-card--blue { --metric-accent: var(--crc-info); }
.metric-card--teal { --metric-accent: var(--crc-accent); }
.metric-card--amber { --metric-accent: var(--crc-warning); }
.metric-card--violet { --metric-accent: var(--crc-violet); }

.metric-card__top {
  display: flex;
  margin-bottom: 14px;
  align-items: center;
  justify-content: space-between;
}

.metric-card__icon {
  display: inline-flex;
  width: 30px;
  height: 30px;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--metric-accent, var(--crc-accent));
  background: var(--crc-canvas);
}

.metric-card__tag {
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 9px;
  font-weight: 500;
  letter-spacing: 0.14em;
}

.metric-card__value {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 38px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
  line-height: 1.05;
}

.metric-card__label {
  margin-top: 7px;
  color: var(--crc-text);
  font-size: 13px;
  font-weight: 550;
}

.metric-card__detail {
  margin-top: 5px;
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  letter-spacing: 0.02em;
}

/* -- Status strip ------------------------------------------------------------------ */

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
  padding: 12px 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.status-pill span {
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.status-pill strong {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 14px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

/* -- Chart cards --------------------------------------------------------------------- */

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
  font-size: 20px;
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

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.card-header--trend {
  align-items: center;
}

.card-eyebrow {
  margin: 0 0 6px;
  color: var(--crc-accent);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  font-weight: 500;
  letter-spacing: 0.14em;
}

.card-header h2 {
  margin: 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 17px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.card-header p:not(.card-eyebrow) {
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
  gap: 5px;
  padding: 13px 15px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-canvas);
}

.summary-chip strong {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 22px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.01em;
  line-height: 1.15;
}

.summary-chip span {
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.06em;
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
  .dashboard-deck {
    align-items: flex-start;
    flex-direction: column;
  }

  .dashboard-deck__right,
  .dashboard-deck__headline-stat {
    align-items: flex-start;
  }

  .status-strip {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 768px) {
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
