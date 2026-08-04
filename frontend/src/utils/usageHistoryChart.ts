import type { DailyStats } from '@/types'

export interface UsageHistoryBucket {
  key: string
  label: string
  requests: number
  tokens: number
}

const toDayKey = (date: Date) => {
  const year = date.getUTCFullYear()
  const month = String(date.getUTCMonth() + 1).padStart(2, '0')
  const day = String(date.getUTCDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

const shiftDay = (dayKey: string, offset: number) => {
  const [year, month, day] = dayKey.split('-').map(Number)
  const date = new Date(Date.UTC(year, month - 1, day))
  date.setUTCDate(date.getUTCDate() + offset)
  return toDayKey(date)
}

const toDayLabel = (dayKey: string) => {
  const [, month, day] = dayKey.split('-')
  return `${month}/${day}`
}

export const buildUsageHistoryBuckets = (
  rangeDays: number,
  stats: DailyStats[],
  endDay?: string
) => {
  const normalizedRange = Math.max(0, Math.floor(rangeDays))
  const statsByDay = new Map(stats.map(stat => [stat.day, stat]))
  const anchorDay = endDay || toDayKey(new Date())
  const firstDay = shiftDay(anchorDay, -(normalizedRange - 1))

  const buckets: UsageHistoryBucket[] = []

  for (let offset = 0; offset < normalizedRange; offset += 1) {
    const key = shiftDay(firstDay, offset)
    const stat = statsByDay.get(key)
    buckets.push({
      key,
      label: toDayLabel(key),
      requests: stat?.total_requests ?? 0,
      tokens: stat?.total_tokens ?? 0
    })
  }

  return buckets
}
