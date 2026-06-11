import { describe, expect, it, vi } from 'vitest'
import { adminApi, adminHttp, createAdminApiClient, hasUsableAdminToken, splitDashboardResponse } from './admin'
import type { DashboardSummaryResponse } from '@/types'

describe('admin api auth behavior', () => {
  it('treats 401 as failed status', () => {
    const client = createAdminApiClient()
    expect(client.defaults.validateStatus?.(200)).toBe(true)
    expect(client.defaults.validateStatus?.(401)).toBe(false)
  })

  it('only accepts non-empty token strings', () => {
    expect(hasUsableAdminToken('abc')).toBe(true)
    expect(hasUsableAdminToken('')).toBe(false)
    expect(hasUsableAdminToken('   ')).toBe(false)
    expect(hasUsableAdminToken(undefined)).toBe(false)
    expect(hasUsableAdminToken(null)).toBe(false)
  })

  it('splits the dashboard payload into view data and analytics', () => {
    const payload: DashboardSummaryResponse = {
      upstreams_count: 3,
      upstreams_active: 2,
      downstreams_count: 4,
      downstreams_active: 3,
      logs_count: 9,
      active_models: 5,
      responses_upstreams: 1,
      admin_username: 'admin',
      app_name: 'chat-responses-codex',
      analytics: {
        range: '7d',
        summary: {
          total_requests: 9,
          success_rate: 88.8,
          average_latency_ms: 123,
          total_tokens: 456
        },
        daily_series: [],
        failure_categories: [],
        user_agent_clusters: []
      }
    }

    const view = splitDashboardResponse(payload)

    expect(view.dashboard).toEqual({
      upstreams_count: 3,
      upstreams_active: 2,
      downstreams_count: 4,
      downstreams_active: 3,
      logs_count: 9,
      active_models: 5,
      responses_upstreams: 1,
      admin_username: 'admin',
      app_name: 'chat-responses-codex'
    })
    expect(view.analytics.summary.total_requests).toBe(9)
  })
})

describe('admin announcement api', () => {
  it('calls the announcement read endpoint', async () => {
    const spy = vi.spyOn(adminHttp, 'get').mockResolvedValue({ data: { announcement: null } } as never)

    await adminApi.getAnnouncement()

    expect(spy).toHaveBeenCalledWith('/admin/announcement')
  })

  it('calls the announcement update endpoint', async () => {
    const spy = vi.spyOn(adminHttp, 'put').mockResolvedValue({ data: { announcement: null } } as never)

    await adminApi.updateAnnouncement({
      title: '系统公告',
      content: '正文',
      level: 'info',
      active: true
    })

    expect(spy).toHaveBeenCalledWith('/admin/announcement', {
      title: '系统公告',
      content: '正文',
      level: 'info',
      active: true
    })
  })
})
