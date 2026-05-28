import type { DailyStats } from '@/types'

export interface UsageHistoryBucket {
  key: string
  label: string
  requests: number
  tokens: number
}

const toDayKey = (date: Date) => {
  const year = date.getFullYear()
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

const toDayLabel = (date: Date) => {
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${month}/${day}`
}

export const buildUsageHistoryBuckets = (rangeDays: number, stats: DailyStats[]) => {
  const today = new Date()
  today.setHours(0, 0, 0, 0)

  const buckets: UsageHistoryBucket[] = []
  const indexByKey = new Map<string, number>()

  for (let offset = rangeDays - 1; offset >= 0; offset -= 1) {
    const date = new Date(today)
    date.setDate(today.getDate() - offset)
    const key = toDayKey(date)
    buckets.push({
      key,
      label: toDayLabel(date),
      requests: 0,
      tokens: 0
    })
    indexByKey.set(key, buckets.length - 1)
  }

  for (const stat of stats) {
    const date = new Date(stat.date * 1000)
    date.setHours(0, 0, 0, 0)
    const index = indexByKey.get(toDayKey(date))
    if (index === undefined) continue
    buckets[index].requests = stat.total_requests
    buckets[index].tokens = stat.total_tokens
  }

  return buckets
}
