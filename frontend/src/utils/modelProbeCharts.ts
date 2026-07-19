import type { ModelProbeChannel, ModelProbeModel } from '@/types'

export type ProbeStatusFilter = 'all' | 'healthy' | 'degraded' | 'offline'

export interface ProbeChannelFilter {
  query?: string
  status?: ProbeStatusFilter
}

const statusRank = (status: ModelProbeChannel['status']) => {
  if (status === 'healthy') return 0
  if (status === 'degraded') return 1
  return 2
}

const anomalyStatusRank = (status: ModelProbeChannel['status']) => {
  if (status === 'offline') return 0
  if (status === 'degraded') return 1
  if (status === 'healthy') return 2
  return 1.5
}

export const sortProbeChannels = (
  channels: ModelProbeChannel[],
  options: { anomalyFirst?: boolean } = {}
) =>
  [...channels].sort((left, right) => {
    const rankStatus = options.anomalyFirst ? anomalyStatusRank : statusRank
    const statusOrder = rankStatus(left.status) - rankStatus(right.status)
    if (statusOrder !== 0) return statusOrder
    return (
      left.upstream_name.localeCompare(right.upstream_name) ||
      left.route_id.localeCompare(right.route_id)
    )
  })

export const sortProbeModels = (models: ModelProbeModel[]) =>
  [...models].sort(
    (left, right) =>
      right.channel_count - left.channel_count || left.model.localeCompare(right.model)
  )

export const groupTopProbeModels = (
  models: ModelProbeModel[],
  limit = 8,
  otherLabel = '其他'
): {
  items: Array<{
    model: string
    channel_count: number
  }>
  total: number
} => {
  const sorted = sortProbeModels(models).filter(model => model.channel_count > 0)
  const total = sorted.reduce((sum, model) => sum + model.channel_count, 0)
  const topModels = sorted.slice(0, limit)
  const overflow = sorted.slice(limit).reduce((sum, model) => sum + model.channel_count, 0)

  return {
    items: overflow > 0
      ? [...topModels, { model: otherLabel, channel_count: overflow }]
      : topModels,
    total
  }
}

export const formatProbeStatusLabel = (status: ModelProbeChannel['status']) => {
  if (status === 'healthy') return '健康'
  if (status === 'degraded') return '降级'
  return '离线'
}

export const filterProbeChannels = (
  channels: ModelProbeChannel[],
  { query = '', status = 'all' }: ProbeChannelFilter
) => {
  const normalizedQuery = query.trim().toLowerCase()

  return channels.filter(channel => {
    if (status !== 'all' && channel.status !== status) {
      return false
    }

    if (!normalizedQuery) {
      return true
    }

    const haystack = [
      channel.upstream_id,
      channel.upstream_name,
      channel.route_id,
      ...channel.models
    ].join(' ').toLowerCase()
    return haystack.includes(normalizedQuery)
  })
}

export const buildProbeChartItems = (summary: {
  healthy_channels: number
  degraded_channels: number
  offline_channels: number
}) =>
  [
    { name: '健康', value: summary.healthy_channels },
    { name: '降级', value: summary.degraded_channels },
    { name: '离线', value: summary.offline_channels }
  ].filter(item => item.value > 0)

export const shouldShowProbeChannelEmpty = ({
  loading,
  hasError,
  channelCount
}: {
  loading: boolean
  hasError: boolean
  channelCount: number
}) => !loading && !hasError && channelCount === 0
