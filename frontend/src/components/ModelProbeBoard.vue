<template>
  <div class="probe-board" :class="`probe-board--${tone}`">
    <section class="probe-hero">
      <div class="probe-hero__copy">
        <p class="eyebrow">{{ scopeLabel }}</p>
        <h2>{{ title }}</h2>
        <p class="hero-copy">{{ subtitle }}</p>
      </div>
      <div class="probe-hero__meta">
        <el-tag :type="loading ? 'warning' : 'success'" effect="light">
          {{ loading ? '刷新中' : '自动刷新' }}
        </el-tag>
        <el-tag effect="plain">轮询 {{ refreshIntervalLabel }}</el-tag>
        <span class="refresh-label">最近刷新 {{ refreshedLabel }}</span>
        <div class="probe-hero__status">
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
        <div class="summary-card">
          <div class="summary-value">{{ summary.total_channels }}</div>
          <div class="summary-label">通道总数</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="summary-card">
          <div class="summary-value">{{ summary.healthy_channels }}</div>
          <div class="summary-label">健康通道</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="summary-card">
          <div class="summary-value">{{ summary.degraded_channels }}</div>
          <div class="summary-label">降级通道</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="summary-card">
          <div class="summary-value">{{ summary.offline_channels }}</div>
          <div class="summary-label">离线通道</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="summary-card">
          <div class="summary-value">{{ summary.total_models }}</div>
          <div class="summary-label">可见模型</div>
        </div>
      </el-col>
      <el-col :xs="24" :sm="12" :md="8" :lg="4">
        <div class="summary-card">
          <div class="summary-value">{{ summary.average_latency_ms }}ms</div>
          <div class="summary-label">平均探测耗时</div>
        </div>
      </el-col>
    </el-row>

    <el-row :gutter="16" class="chart-grid">
      <el-col :xs="24" :lg="9">
        <el-card shadow="hover" class="probe-card">
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
        <el-card shadow="hover" class="probe-card">
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

    <el-card shadow="hover" class="probe-card">
      <template #header>
        <div class="card-header">
          <div>
            <h3>通道状态明细</h3>
            <p>自动刷新后的最新探测结果。</p>
          </div>
          <el-tag effect="plain">{{ sortedChannels.length }} 个通道</el-tag>
        </div>
      </template>

      <div class="channel-grid" v-loading="loading">
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
import { groupTopProbeModels, formatProbeStatusLabel, sortProbeChannels } from '@/utils/modelProbeCharts'
import { normalizeModelProbeRefreshIntervalSeconds } from '@/utils/modelProbePolling'

const props = defineProps<{
  tone: 'admin' | 'portal'
  scopeLabel: string
  title: string
  subtitle: string
  data: ModelProbeResponse
  loading?: boolean
}>()

const statusChartRef = ref<HTMLElement>()
const coverageChartRef = ref<HTMLElement>()
let statusChart: EChartsType | null = null
let coverageChart: EChartsType | null = null

const loading = computed(() => props.loading ?? false)
const hasError = computed(() => props.data.summary?.total_channels === 0 && !props.loading)
const isEmpty = computed(() => props.data.summary?.total_channels === 0 && !props.loading && !hasError.value)
const errorMessage = computed(() => '模型探测失败，请检查上游配置或稍后重试')
const summary = computed(() => props.data.summary)
const sortedChannels = computed(() => sortProbeChannels(props.data.channels))
const modelCoverage = computed(() => groupTopProbeModels(props.data.models, 8))

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

const buildStatusSeries = () => {
  const items = [
    { name: '健康', value: summary.value.healthy_channels },
    { name: '降级', value: summary.value.degraded_channels },
    { name: '离线', value: summary.value.offline_channels }
  ].filter(item => item.value > 0)
  return items.length > 0 ? items : [{ name: '暂无数据', value: 1 }]
}

const buildCoverageSeries = () => {
  const items = modelCoverage.value.items.map(item => ({
    name: item.model,
    value: item.channel_count
  }))
  return items.length > 0 ? items : [{ name: '暂无模型', value: 1 }]
}

const renderCharts = async () => {
  const echarts = await loadEcharts()

  if (statusChartRef.value) {
    if (!statusChart) {
      statusChart = echarts.init(statusChartRef.value)
    }
    statusChart.setOption({
      color: ['#18c29c', '#f59e0b', '#ef4444'],
      tooltip: { trigger: 'item' },
      legend: {
        bottom: 0,
        left: 'center',
        icon: 'circle'
      },
      series: [
        {
          type: 'pie',
          radius: ['48%', '72%'],
          center: ['50%', '45%'],
          avoidLabelOverlap: true,
          label: {
            formatter: '{b}\n{c}',
            color: '#334155'
          },
          data: buildStatusSeries()
        }
      ]
    })
  }

  if (coverageChartRef.value) {
    if (!coverageChart) {
      coverageChart = echarts.init(coverageChartRef.value)
    }
    const series = buildCoverageSeries()
    coverageChart.setOption({
      grid: { left: 24, right: 20, top: 12, bottom: 24, containLabel: true },
      tooltip: { trigger: 'axis', axisPointer: { type: 'shadow' } },
      xAxis: {
        type: 'value',
        name: '通道数',
        axisLabel: { color: '#64748b' },
        splitLine: { lineStyle: { color: 'rgba(148, 163, 184, 0.18)' } }
      },
      yAxis: {
        type: 'category',
        inverse: true,
        data: series.map(item => item.name),
        axisLabel: { color: '#334155' }
      },
      series: [
        {
          type: 'bar',
          data: series.map(item => item.value),
          barWidth: 16,
          itemStyle: {
            borderRadius: [0, 10, 10, 0],
            color: '#2563eb'
          }
        }
      ]
    })
  }
}

const resizeCharts = () => {
  statusChart?.resize()
  coverageChart?.resize()
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

onBeforeUnmount(() => {
  window.removeEventListener('resize', resizeCharts)
  statusChart?.dispose()
  coverageChart?.dispose()
  statusChart = null
  coverageChart = null
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
  border-radius: 12px;
}

.probe-error-alert {
  background: rgba(239, 68, 68, 0.08);
  border: 1px solid rgba(239, 68, 68, 0.2);
}

.probe-empty-alert {
  background: rgba(148, 163, 184, 0.08);
  border: 1px solid rgba(148, 163, 184, 0.2);
}

.probe-hero {
  display: flex;
  justify-content: space-between;
  gap: 24px;
  padding: 24px 28px;
  border-radius: 24px;
  color: #f8fafc;
  background:
    radial-gradient(circle at top left, rgba(56, 189, 248, 0.22), transparent 32%),
    radial-gradient(circle at top right, rgba(168, 85, 247, 0.18), transparent 28%),
    linear-gradient(135deg, #0f172a 0%, #111827 48%, #1f2937 100%);
  box-shadow: 0 24px 48px rgba(15, 23, 42, 0.18);
}

.probe-board--portal .probe-hero {
  background:
    radial-gradient(circle at top left, rgba(16, 185, 129, 0.18), transparent 32%),
    radial-gradient(circle at top right, rgba(14, 165, 233, 0.18), transparent 28%),
    linear-gradient(135deg, #052e2b 0%, #083344 48%, #0f172a 100%);
}

.probe-hero__copy h2 {
  margin: 0;
  font-size: 28px;
  line-height: 1.1;
}

.probe-hero__copy .hero-copy {
  margin: 10px 0 0;
  color: rgba(226, 232, 240, 0.84);
  max-width: 64ch;
}

.eyebrow {
  margin: 0 0 10px;
  font-size: 12px;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: rgba(191, 219, 254, 0.88);
}

.probe-hero__meta {
  display: flex;
  flex-direction: column;
  justify-content: center;
  align-items: flex-end;
  gap: 10px;
  white-space: nowrap;
}

.probe-hero__status {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 8px;
}

.refresh-label {
  font-size: 13px;
  color: rgba(226, 232, 240, 0.72);
}

.summary-grid,
.chart-grid {
  margin-top: 0;
}

.summary-card {
  min-height: 122px;
  padding: 18px 20px;
  border-radius: 20px;
  background: linear-gradient(180deg, #ffffff 0%, #f8fafc 100%);
  border: 1px solid rgba(148, 163, 184, 0.12);
  box-shadow: 0 16px 32px rgba(15, 23, 42, 0.06);
  display: flex;
  flex-direction: column;
  justify-content: space-between;
}

.summary-value {
  font-size: 30px;
  font-weight: 700;
  color: #0f172a;
}

.summary-label {
  margin-top: 8px;
  color: #64748b;
  font-size: 13px;
}

.probe-card {
  border: none;
  border-radius: 20px;
  overflow: hidden;
  box-shadow: 0 16px 32px rgba(15, 23, 42, 0.06);
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.card-header h3 {
  margin: 0;
  font-size: 17px;
}

.card-header p {
  margin: 6px 0 0;
  color: #64748b;
  font-size: 13px;
}

.probe-chart {
  height: 320px;
}

.probe-chart--wide {
  height: 360px;
}

.channel-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.channel-card {
  border: 1px solid rgba(148, 163, 184, 0.12);
  border-radius: 18px;
  padding: 18px;
  background: linear-gradient(180deg, #ffffff 0%, #f8fafc 100%);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.72);
}

.channel-card__top {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}

.channel-card__top h4 {
  margin: 0;
  font-size: 16px;
  color: #0f172a;
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
  font-size: 12px;
  color: #94a3b8;
  margin-bottom: 8px;
}

.channel-card__models-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.channel-card__model-tag {
  border-radius: 999px;
}

.channel-card__models-empty {
  font-size: 13px;
  color: #64748b;
}

.channel-card__metrics span {
  display: block;
  font-size: 12px;
  color: #94a3b8;
}

.channel-card__metrics strong {
  display: block;
  margin-top: 4px;
  font-size: 15px;
  color: #0f172a;
}

.channel-card__error {
  margin-top: 14px;
  padding: 10px 12px;
  border-radius: 12px;
  background: rgba(239, 68, 68, 0.08);
  color: #b91c1c;
  font-size: 12px;
  line-height: 1.5;
}

@media (max-width: 1200px) {
  .channel-grid {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 768px) {
  .probe-hero {
    flex-direction: column;
  }

  .probe-hero__meta {
    align-items: flex-start;
  }

  .probe-hero__status {
    justify-content: flex-start;
  }

  .channel-card__metrics {
    grid-template-columns: 1fr;
  }

  .probe-chart,
  .probe-chart--wide {
    height: 280px;
  }
}
</style>
