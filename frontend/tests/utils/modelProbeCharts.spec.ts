import { describe, expect, it } from 'vitest'
import {
  buildProbeChartItems,
  filterProbeChannels,
  shouldShowProbeChannelEmpty,
  sortProbeChannels,
  sortProbeModels,
  type ProbeChannelFilter
} from '../../src/utils/modelProbeCharts'

describe('modelProbeCharts', () => {
  it('sorts probe channels with healthy channels first', () => {
    const channels = sortProbeChannels([
      {
        upstream_id: 'up-2',
        upstream_name: 'Beta',
        route_id: 'route_beta',
        status: 'offline',
        latency_ms: 200,
        model_count: 0,
        models: [],
        last_probe_at: 1,
        error: 'boom'
      },
      {
        upstream_id: 'up-1',
        upstream_name: 'Alpha',
        route_id: 'route_alpha',
        status: 'healthy',
        latency_ms: 100,
        model_count: 2,
        models: ['gpt-4o', 'gpt-4o-mini'],
        last_probe_at: 1,
        error: null
      },
      {
        upstream_id: 'up-3',
        upstream_name: 'Gamma',
        route_id: 'route_gamma',
        status: 'healthy',
        latency_ms: 90,
        model_count: 1,
        models: ['claude-3'],
        last_probe_at: 1,
        error: null
      }
    ])

    expect(channels.map(channel => channel.upstream_name)).toEqual(['Alpha', 'Gamma', 'Beta'])
  })

  it('sorts probe models by coverage then name', () => {
    const models = sortProbeModels([
      { model: 'gpt-4o', channel_count: 1 },
      { model: 'claude-3', channel_count: 2 },
      { model: 'deepseek-r1', channel_count: 2 }
    ])

    expect(models).toEqual([
      { model: 'claude-3', channel_count: 2 },
      { model: 'deepseek-r1', channel_count: 2 },
      { model: 'gpt-4o', channel_count: 1 }
    ])
  })
})

describe('model probe filtering', () => {
  const channels = [
    {
      upstream_id: 'up-1',
      upstream_name: 'Alpha',
      route_id: 'route_alpha',
      status: 'healthy',
      latency_ms: 100,
      model_count: 2,
      models: ['glm-5.1', 'deepseek-chat'],
      last_probe_at: 1,
      error: null
    },
    {
      upstream_id: 'up-2',
      upstream_name: 'Beta',
      route_id: 'route_beta',
      status: 'offline',
      latency_ms: 0,
      model_count: 0,
      models: [],
      last_probe_at: 1,
      error: 'timeout'
    }
  ]

  it('filters channels by text and status', () => {
    const filter: ProbeChannelFilter = { query: 'glm', status: 'healthy' }
    expect(filterProbeChannels(channels, filter).map(channel => channel.upstream_name)).toEqual(['Alpha'])
    expect(filterProbeChannels(channels, { query: 'beta', status: 'offline' }).map(channel => channel.upstream_name)).toEqual(['Beta'])
  })

  it('sorts anomalies first when requested', () => {
    expect(sortProbeChannels(channels, { anomalyFirst: true }).map(channel => channel.upstream_name)).toEqual(['Beta', 'Alpha'])
  })

  it('treats unknown statuses as anomalies before healthy channels', () => {
    const sorted = sortProbeChannels([
      {
        upstream_id: 'up-1',
        upstream_name: 'Alpha',
        route_id: 'route_alpha',
        status: 'healthy',
        latency_ms: 100,
        model_count: 1,
        models: ['glm-5.1'],
        last_probe_at: 1,
        error: null
      },
      {
        upstream_id: 'up-2',
        upstream_name: 'Beta',
        route_id: 'route_beta',
        status: 'unknown',
        latency_ms: 0,
        model_count: 0,
        models: [],
        last_probe_at: 1,
        error: 'unexpected status'
      }
    ], { anomalyFirst: true })

    expect(sorted.map(channel => channel.upstream_name)).toEqual(['Beta', 'Alpha'])
  })

  it('returns no chart items for empty status data', () => {
    expect(buildProbeChartItems({ healthy_channels: 0, degraded_channels: 0, offline_channels: 0 })).toEqual([])
  })
})

describe('probe channel empty visibility', () => {
  it('shows empty state only when not loading, not erroring, and no channels match', () => {
    expect(shouldShowProbeChannelEmpty({ loading: false, hasError: false, channelCount: 0 })).toBe(true)
    expect(shouldShowProbeChannelEmpty({ loading: true, hasError: false, channelCount: 0 })).toBe(false)
    expect(shouldShowProbeChannelEmpty({ loading: false, hasError: true, channelCount: 0 })).toBe(false)
    expect(shouldShowProbeChannelEmpty({ loading: false, hasError: false, channelCount: 1 })).toBe(false)
  })
})
