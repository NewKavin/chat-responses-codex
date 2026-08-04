import { describe, expect, it, vi } from 'vitest'
import type { DailyStats } from '@/types'
import { buildUsageHistoryBuckets } from '../../src/utils/usageHistoryChart'

describe('usage history chart buckets', () => {
  it('keeps chronological buckets and fills missing days', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-05-26T09:00:00+08:00'))

    const stats: DailyStats[] = [
      { day: '2026-05-26', start_time: Math.floor(new Date('2026-05-26T00:00:00+08:00').getTime() / 1000), total_requests: 10, total_tokens: 1000, success_rate: 1 },
      { day: '2026-05-24', start_time: Math.floor(new Date('2026-05-24T00:00:00+08:00').getTime() / 1000), total_requests: 6, total_tokens: 600, success_rate: 1 }
    ]

    const buckets = buildUsageHistoryBuckets(3, stats)
    expect(buckets).toHaveLength(3)
    expect(buckets.map(item => item.requests)).toEqual([6, 0, 10])
    expect(buckets.map(item => item.tokens)).toEqual([600, 0, 1000])
    vi.useRealTimers()
  })

  it('ignores out-of-range stats', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-05-26T09:00:00+08:00'))

    const stats: DailyStats[] = [
      { day: '2026-05-20', start_time: Math.floor(new Date('2026-05-20T00:00:00+08:00').getTime() / 1000), total_requests: 99, total_tokens: 9999, success_rate: 1 }
    ]

    const buckets = buildUsageHistoryBuckets(3, stats)
    expect(buckets.map(item => item.requests)).toEqual([0, 0, 0])
    expect(buckets.map(item => item.tokens)).toEqual([0, 0, 0])
    vi.useRealTimers()
  })

  it('uses the server calendar day instead of the browser timezone', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-05-27T12:00:00Z'))

    const stats: DailyStats[] = [{
      day: '2026-05-26',
      // Asia/Shanghai midnight for 2026-05-26, which is still May 25 in UTC.
      start_time: Math.floor(new Date('2026-05-25T16:00:00Z').getTime() / 1000),
      total_requests: 8,
      total_tokens: 800,
      success_rate: 1
    }]

    const buckets = buildUsageHistoryBuckets(1, stats, '2026-05-26')

    expect(buckets).toEqual([{
      key: '2026-05-26',
      label: '05/26',
      requests: 8,
      tokens: 800
    }])
    vi.useRealTimers()
  })

})
