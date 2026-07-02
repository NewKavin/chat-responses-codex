import { describe, expect, it } from 'vitest'
import { buildUserAgentChartSummary } from '../../src/utils/userAgentChart'

describe('user agent chart summary', () => {
  it('keeps aggregate downstream summary without using request counts', () => {
    const summary = buildUserAgentChartSummary([
      { name: 'Claude-Code', value: 3 },
      { name: 'OpenAI/ChatGPT', value: 2 },
      { name: 'curl', value: 1 }
    ])

    expect(summary.clusterCount).toBe(3)
    expect(summary.totalDownstreams).toBe(6)
    expect(summary.topCluster).toBe('Claude-Code')
    expect(summary.topClusterCount).toBe(3)
  })
})
