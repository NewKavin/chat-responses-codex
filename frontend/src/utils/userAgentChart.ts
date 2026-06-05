import type { DashboardAnalyticsRange } from '@/types'

export interface UserAgentChartSummary {
  clusterCount: number
  totalDownstreams: number
  topCluster: string
  topClusterCount: number
}

export const buildUserAgentChartSummary = (
  items: DashboardAnalyticsRange['user_agent_clusters']
): UserAgentChartSummary => {
  const sorted = [...items].sort((a, b) => b.value - a.value)

  return {
    clusterCount: items.length,
    totalDownstreams: items.reduce((sum, item) => sum + item.value, 0),
    topCluster: sorted[0]?.name ?? '-',
    topClusterCount: sorted[0]?.value ?? 0
  }
}
