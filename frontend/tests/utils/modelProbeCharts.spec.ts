import { describe, expect, it } from 'vitest'
import { sortProbeChannels, sortProbeModels } from '../../src/utils/modelProbeCharts'

describe('modelProbeCharts', () => {
  it('sorts probe channels with healthy channels first', () => {
    const channels = sortProbeChannels([
      {
        upstream_id: 'up-2',
        upstream_name: 'Beta',
        key_prefix: 'beta',
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
        key_prefix: 'alpha',
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
        key_prefix: 'gamma',
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
