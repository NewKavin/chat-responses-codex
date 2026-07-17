<template>
  <div class="probe-board" :class="`probe-board--${tone}`">
    <section class="probe-page-header">
      <div class="probe-page-header__copy">
        <p class="probe-context">{{ scopeLabel }}</p>
        <h2>{{ title }}</h2>
        <p class="probe-description">{{ subtitle }}</p>
      </div>
      <div class="probe-page-header__meta">
        <el-tag :type="loading ? 'warning' : 'success'" effect="light">
          {{ loading ? '刷新中' : '自动刷新' }}
        </el-tag>
        <el-tag effect="plain">轮询 {{ refreshIntervalLabel }}</el-tag>
        <span class="refresh-label">最近刷新 {{ refreshedLabel }}</span>
        <div class="probe-page-header__status">
          <el-tag type="success" effect="light">{{ summary.healthy_channels }} 健康</el-tag>
          <el-tag type="warning" effect="light">{{ summary.degraded_channels }} 降级</el-tag>
          <el-tag type="danger" effect="light">{{ summary.offline_channels }} 离线</el-tag>
        </div>
      </div>
    </section>

    <!-- 错误提示 -->
    <el-alert
      v-if="hasError"
      :title="errorMessage"
      type="error"
      :closable="false"
      show-icon
      class="probe-error-alert"
    />

    <!-- 空数据提示 -->
    <el-alert
      v-else-if="isEmpty"
      title="暂无通道数据"
      type="info"
      :closable="false"
      show-icon
      class="probe-empty-alert"
    />

    <el-row :gutter="16" class="summary-grid">
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="probe-metric">
          <div class="summary-value">{{ summary.total_channels }}</div>
          <div class="summary-label">通道总数</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="probe-metric">
          <div class="summary-value">{{ summary.healthy_channels }}</div>
          <div class="summary-label">健康通道</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="probe-metric">
          <div class="summary-value">{{ summary.degraded_channels }}</div>
          <div class="summary-label">降级通道</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="probe-metric">
          <div class="summary-value">{{ summary.offline_channels }}</div>
          <div class="summary-label">离线通道</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="probe-metric">
          <div class="summary-value">{{ summary.total_models }}</div>
          <div class="summary-label">可见模型</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="probe-metric">
          <div class="summary-value">{{ summary.average_latency_ms }}ms</div>
          <div class="summary-label">平均探测耗时</div>
        </div>
      </el-col>
    </el-row>

    <el-row :gutter="16" class="chart-grid">
      <el-col :xs="24" :lg="9">
        <el-card shadow="never" class="probe-card">
          <template #header>
            <div class="card-header">
              <div>
                <h3>通道健康</h3>
                <p>按健康、离线和降级状态分布。</p>
              </div>
              <el-tag effect="plain">{{ summary.offline_channels }} 个离线</el-tag>
            </div>
          </template>
          <div ref="statusChartRef" class="probe-chart" v-loading="loading" />
        </el-card>
      </el-col>
      <el-col :xs="24" :lg="15">
        <el-card shadow="never" class="probe-card">
          <template #header>
            <div class="card-header">
              <div>
                <h3>模型覆盖</h3>
                <p>按通道覆盖数排序的模型 Top 列表。</p>
              </div>
              <el-tag effect="plain">{{ modelCoverage.items.length }} 项</el-tag>
            </div>
          </template>
          <div ref="coverageChartRef" class="probe-chart probe-chart--wide" v-loading="loading" />
        </el-card>
      </el-col>
    </el-row>

    <el-card shadow="never" class="probe-card">
      <template #header>
        <div class="card-header">
          <div>
            <h3>通道状态明细</h3>
            <p>自动刷新后的最新探测结果。</p>
          </div>
          <el-tag effect="plain">{{ sortedChannels.length }} 个通道</el-tag>
        </div>
      </template>

      <div class="channel-toolbar">
        <el-input
          v-model="searchQuery"
          class="channel-toolbar__search"
          placeholder="搜索上游、Key 或模型"
          clearable
          size="small"
        />
        <el-radio-group v-model="statusFilter" size="small" class="channel-toolbar__status">
          <el-radio-button
            v-for="option in statusFilterOptions"
            :key="option.value"
            :label="option.value"
            :value="option.value"
          >
            {{ option.label }}
          </el-radio-button>
        </el-radio-group>
        <div class="channel-toolbar__switch">
          <span>异常优先</span>
          <el-switch v-model="anomalyFirst" size="small" />
        </div>
      </div>

      <div class="channel-grid" v-loading="loading">
        <div v-if="showChannelEmpty" class="channel-empty">
          当前条件下暂无通道
        </div>
        <article
          v-for="channel in sortedChannels"
          :key="`${channel.upstream_id}-${channel.key_prefix}`"
          class="channel-card"
        >
          <div class="channel-card__top">
            <div>
              <h4>{{ channel.upstream_name }}</h4>
            </div>
            <el-tag :type="statusTagType(channel.status)" effect="light">
              {{ formatProbeStatusLabel(channel.status) }}
            </el-tag>
          </div>

          <div class="channel-card__metrics">
            <div>
              <span>模型</span>
              <strong>{{ channel.model_count }}</strong>
            </div>
            <div>
              <span>耗时</span>
              <strong>{{ channel.latency_ms }}ms</strong>
            </div>
            <div>
              <span>刷新</span>
              <strong>{{ formatTime(channel.last_probe_at) }}</strong>
            </div>
          </div>

          <div class="channel-card__models">
            <span class="channel-card__models-label">可用模型</span>
            <div class="channel-card__models-list">
              <el-tag
                v-for="model in channel.models"
                :key="model"
                size="small"
                effect="plain"
                class="channel-card__model-tag"
              >
                {{ model }}
              </el-tag>
              <span v-if="channel.models.length === 0" class="channel-card__models-empty">
                暂无可用模型
              </span>
            </div>
          </div>

          <div v-if="channel.error" class="channel-card__error">
            {{ channel.error }}
          </div>
        </article>
      </div>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { loadEcharts } from '@/utils/echartsLoader'
import type { ModelProbeResponse } from '@/types'
import type { EChartsType } from 'echarts/core'
import {
  buildProbeChartItems,
  filterProbeChannels,
  formatProbeStatusLabel,
  groupTopProbeModels,
  shouldShowProbeChannelEmpty,
  sortProbeChannels,
  type ProbeStatusFilter
} from '@/utils/modelProbeCharts'
import { normalizeModelProbeRefreshIntervalSeconds } from '@/utils/modelProbePolling'
import { useTheme } from '@/composables/useTheme'
import { buildChartTheme } from '@/utils/chartTheme'

const props = defineProps<{
  tone: 'admin' | 'portal'
  scopeLabel: string
  title: string
  subtitle: string
  data: ModelProbeResponse
  loading?: boolean
  errorMessage?: string
}>()

const { resolvedTheme } = useTheme()
const chartTheme = computed(() => buildChartTheme(resolvedTheme.value))

const statusChartRef = ref<HTMLElement>()
const coverageChartRef = ref<HTMLElement>()
let statusChart: EChartsType | null = null
let coverageChart: EChartsType | null = null

const searchQuery = ref('')
const statusFilter = ref<ProbeStatusFilter>('all')
const anomalyFirst = ref(true)

const loading = computed(() => props.loading ?? false)
const hasError = computed(() => Boolean(props.errorMessage && !loading.value))
const isEmpty = computed(() => !hasError.value && props.data.summary?.total_channels === 0 && !loading.value)
const errorMessage = computed(() => props.errorMessage || '模型探测失败，请检查上游配置或稍后重试')
const summary = computed(() => props.data.summary)
const sortedChannels = computed(() =>
  sortProbeChannels(
    filterProbeChannels(props.data.channels, {
      query: searchQuery.value,
      status: statusFilter.value
    }),
    { anomalyFirst: anomalyFirst.value }
  )
)
const showChannelEmpty = computed(() =>
  shouldShowProbeChannelEmpty({
    loading: loading.value,
    hasError: hasError.value,
    channelCount: sortedChannels.value.length
  })
)
const modelCoverage = computed(() => groupTopProbeModels(props.data.models, 8))

const statusFilterOptions: Array<{ label: string; value: ProbeStatusFilter }> = [
  { label: '全部', value: 'all' },
  { label: '健康', value: 'healthy' },
  { label: '降级', value: 'degraded' },
  { label: '离线', value: 'offline' }
]

const refreshedLabel = computed(() => {
  if (!props.data.refreshed_at) return '等待刷新'
  return new Date(props.data.refreshed_at * 1000).toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  })
})

const refreshIntervalLabel = computed(
  () => `${normalizeModelProbeRefreshIntervalSeconds(props.data.refresh_interval_seconds)}s`
)

const formatTime = (timestamp: number) => {
  if (!timestamp) return '-'
  return new Date(timestamp * 1000).toLocaleTimeString('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  })
}

const statusTagType = (status: string) => {
  if (status === 'healthy') return 'success'
  if (status === 'degraded') return 'warning'
  return 'danger'
}

const buildStatusSeries = () => buildProbeChartItems(summary.value)

const buildCoverageSeries = () => {
  return modelCoverage.value.items.map(item => ({
    name: item.model,
    value: item.channel_count
  }))
}

const renderCharts = async () => {
  const echarts = await loadEcharts()
  const theme = chartTheme.value

  if (statusChartRef.value) {
    if (!statusChart) {
      statusChart = echarts.init(statusChartRef.value)
    }
    const series = buildStatusSeries()
    const hasData = series.length > 0
    statusChart.setOption({
      color: [theme.series[0], theme.series[3], theme.series[4]],
      tooltip: {
        trigger: 'item',
        backgroundColor: theme.tooltipBackground,
        borderColor: theme.tooltipBorder,
        textStyle: { color: theme.text }
      },
      legend: {
        bottom: 0,
        left: 'center',
        icon: 'circle',
        textStyle: { color: theme.muted }
      },
      series: [
        {
          type: 'pie',
          radius: ['48%', '72%'],
          center: ['50%', '45%'],
          avoidLabelOverlap: true,
          label: {
            formatter: '{b}\n{c}',
            color: theme.text
          },
          data: series
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
                text: '暂无通道健康数据',
                fill: theme.muted,
                fontSize: 14
              }
            }
          ]
    }, { replaceMerge: ['series', 'graphic'] })
  }

  if (coverageChartRef.value) {
    if (!coverageChart) {
      coverageChart = echarts.init(coverageChartRef.value)
    }
    const series = buildCoverageSeries()
    const hasData = series.length > 0
    coverageChart.setOption({
      grid: { left: 24, right: 20, top: 12, bottom: 24, containLabel: true },
      tooltip: {
        trigger: 'axis',
        backgroundColor: theme.tooltipBackground,
        borderColor: theme.tooltipBorder,
        textStyle: { color: theme.text },
        axisPointer: { type: 'shadow' }
      },
      xAxis: {
        type: 'value',
        name: '通道数',
        axisLabel: { color: theme.muted },
        splitLine: { lineStyle: { color: theme.splitLine } }
      },
      yAxis: {
        type: 'category',
        inverse: true,
        data: series.map(item => item.name),
        axisLabel: { color: theme.text }
      },
      series: [
        {
          type: 'bar',
          data: series.map(item => item.value),
          barWidth: 16,
          itemStyle: {
            borderRadius: [0, 10, 10, 0],
            color: theme.series[0]
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
                text: '暂无模型覆盖数据',
                fill: theme.muted,
                fontSize: 14
              }
            }
          ]
    }, { replaceMerge: ['series', 'graphic'] })
  }
}

const resizeCharts = () => {
  statusChart?.resize()
  coverageChart?.resize()
}

const disposeCharts = () => {
  statusChart?.dispose()
  coverageChart?.dispose()
  statusChart = null
  coverageChart = null
}

onMounted(async () => {
  await nextTick()
  await renderCharts()
  window.addEventListener('resize', resizeCharts)
})

watch(
  () => props.data,
  async () => {
    await nextTick()
    await renderCharts()
  },
  { deep: true }
)

watch(resolvedTheme, async () => {
  disposeCharts()
  await nextTick()
  await renderCharts()
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', resizeCharts)
  disposeCharts()
})
</script>

<style scoped>
.probe-board {
  display: flex;
  flex-direction: column;
  gap: 18px;
}

.probe-error-alert,
.probe-empty-alert {
  margin: 0;
  border-radius: var(--crc-radius);
}

.probe-error-alert {
  border: 1px solid var(--crc-danger);
  background: var(--crc-danger-soft);
}

.probe-empty-alert {
  border: 1px solid var(--crc-border);
  background: var(--crc-surface-muted);
}

.probe-page-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 20px;
  padding-bottom: 16px;
  border-bottom: 1px solid var(--crc-border);
}

.probe-page-header__copy h2 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 20px;
  line-height: 1.3;
}

.probe-description {
  max-width: 64ch;
  margin: 6px 0 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.6;
}

.probe-context {
  margin: 0 0 5px;
  color: var(--crc-accent);
  font-size: 11px;
  font-weight: 650;
}

.probe-page-header__meta {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 8px;
  white-space: nowrap;
}

.probe-page-header__status {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 6px;
}

.refresh-label {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.summary-grid,
.chart-grid {
  margin-top: 0;
}

.probe-metric {
  display: flex;
  min-height: 104px;
  padding: 16px;
  flex-direction: column;
  justify-content: space-between;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
}

.summary-value {
  color: var(--crc-text-strong);
  font-size: 26px;
  font-weight: 680;
}

.summary-label {
  margin-top: 8px;
  color: var(--crc-text-muted);
  font-size: 13px;
}

.probe-card {
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  overflow: hidden;
  background: var(--crc-surface);
  box-shadow: none;
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.card-header h3 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 16px;
}

.card-header p {
  margin: 6px 0 0;
  color: var(--crc-text-muted);
  font-size: 13px;
}

.probe-chart {
  height: 320px;
}

.probe-chart--wide {
  height: 360px;
}

.channel-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
  flex-wrap: wrap;
}

.channel-toolbar__search {
  width: 280px;
  max-width: 100%;
}

.channel-toolbar__status {
  flex-shrink: 0;
}

.channel-toolbar__switch {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-height: 32px;
  padding: 0 10px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text);
  background: var(--crc-surface-muted);
  font-size: 13px;
}

.channel-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.channel-empty {
  grid-column: 1 / -1;
  padding: 28px;
  border: 1px dashed var(--crc-border-strong);
  border-radius: var(--crc-radius);
  color: var(--crc-text-muted);
  background: var(--crc-surface-muted);
  text-align: center;
  font-size: 14px;
}

.channel-card {
  padding: 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
}

.channel-card__top {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}

.channel-card__top h4 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 15px;
}

.channel-card__metrics {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
  margin-top: 14px;
}

.channel-card__models {
  margin-top: 14px;
}

.channel-card__models-label {
  display: block;
  margin-bottom: 8px;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.channel-card__models-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  max-height: 108px;
  overflow-y: auto;
  padding-right: 2px;
}

.channel-card__model-tag {
  border-radius: var(--crc-radius-sm);
}

.channel-card__models-empty {
  color: var(--crc-text-muted);
  font-size: 13px;
}

.channel-card__metrics span {
  display: block;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.channel-card__metrics strong {
  display: block;
  margin-top: 4px;
  color: var(--crc-text-strong);
  font-size: 15px;
}

.channel-card__error {
  margin-top: 14px;
  padding: 10px 12px;
  border: 1px solid var(--crc-danger);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-danger);
  background: var(--crc-danger-soft);
  font-size: 12px;
  line-height: 1.5;
}

@media (max-width: 1200px) {
  .channel-grid {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 768px) {
  .probe-page-header {
    flex-direction: column;
  }

  .probe-page-header__meta {
    align-items: flex-start;
    white-space: normal;
  }

  .probe-page-header__status {
    justify-content: flex-start;
  }

  .channel-card__metrics {
    grid-template-columns: 1fr;
  }

  .channel-toolbar {
    align-items: stretch;
  }

  .channel-toolbar__search,
  .channel-toolbar__status,
  .channel-toolbar__switch {
    width: 100%;
  }

  .probe-chart,
  .probe-chart--wide {
    height: 280px;
  }
}
</style>
