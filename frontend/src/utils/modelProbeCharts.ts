import type { ModelProbeChannel, ModelProbeModel } from '@/types'

const statusRank = (status: ModelProbeChannel['status']) => {
  if (status === 'healthy') return 0
  if (status === 'degraded') return 1
  return 2
}

export const sortProbeChannels = (channels: ModelProbeChannel[]) =>
  [...channels].sort((left, right) => {
    const statusOrder = statusRank(left.status) - statusRank(right.status)
    if (statusOrder !== 0) return statusOrder
    return (
      left.upstream_name.localeCompare(right.upstream_name) ||
      left.key_prefix.localeCompare(right.key_prefix)
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
