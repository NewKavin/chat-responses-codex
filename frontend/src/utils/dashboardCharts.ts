import type { DashboardBreakdownItem } from '@/types'

const compareBreakdownItems = (left: DashboardBreakdownItem, right: DashboardBreakdownItem) =>
  right.value - left.value || left.name.localeCompare(right.name)

export const sortBreakdownItems = (items: DashboardBreakdownItem[]) =>
  [...items].sort(compareBreakdownItems)

export const groupTopBreakdownItems = (
  items: DashboardBreakdownItem[],
  limit = 6,
  otherLabel = '其他'
) => {
  const filtered = sortBreakdownItems(items).filter(item => item.value > 0)
  const total = filtered.reduce((sum, item) => sum + item.value, 0)
  const topItems = filtered.slice(0, limit)
  const overflow = filtered.slice(limit).reduce((sum, item) => sum + item.value, 0)

  return {
    items: overflow > 0 ? [...topItems, { name: otherLabel, value: overflow }] : topItems,
    total
  }
}
