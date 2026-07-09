import { describe, expect, it, vi } from 'vitest'
import { adminApi, adminHttp, createAdminApiClient, hasUsableAdminToken, splitDashboardResponse } from '../../src/api/admin'
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
        user_agent_clusters: [],
        model_usage: [
          { name: 'gpt-4o', value: 4 }
        ],
        downstream_usage: [
          { name: 'Team Alpha', value: 3 }
        ]
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

  it('calls the model probe endpoint', async () => {
    const spy = vi.spyOn(adminHttp, 'get').mockResolvedValue({
      data: {
        channels: [],
        models: [],
        summary: {
          total_channels: 0,
          healthy_channels: 0,
          offline_channels: 0,
          degraded_channels: 0,
          total_models: 0,
          average_latency_ms: 0
        },
        refreshed_at: 0,
        refresh_interval_seconds: 15
      }
    } as never)

    await adminApi.getModelProbe()

    expect(spy).toHaveBeenCalledWith('/admin/model-probe')
  })

  it('calls the logs endpoint with error category filters', async () => {
    const spy = vi.spyOn(adminHttp, 'get').mockResolvedValue({
      data: {
        logs: [],
        total: 0,
        page: 1,
        page_size: 10,
        total_pages: 0
      }
    } as never)

    await adminApi.getLogs({
      error_categories: 'stream_interrupted,stream_upstream_body_decode_error'
    } as never)

    expect(spy).toHaveBeenCalledWith('/admin/logs', {
      params: {
        error_categories: 'stream_interrupted,stream_upstream_body_decode_error'
      }
    })
  })

  it('runs admin troubleshooting diagnostics', async () => {
    const spy = vi.spyOn(adminHttp, 'post').mockResolvedValue({
      data: { run_id: 'diag_1', results: [] }
    } as never)

    await adminApi.runTroubleshooting({
      downstream_id: 'test',
      client_profile: 'cline',
      model: 'GLM-5.1',
      checks: ['models']
    })

    expect(spy).toHaveBeenCalledWith('/admin/troubleshooting/run', {
      downstream_id: 'test',
      client_profile: 'cline',
      model: 'GLM-5.1',
      checks: ['models']
    })
  })

  it('posts compatibility matrix runs through the admin api', async () => {
    const spy = vi.spyOn(adminHttp, 'post').mockResolvedValue({
      data: {
        run_id: 'matrix_1',
        downstream_id: 'test',
        models: ['GLM-5.1'],
        client_profiles: ['codex', 'opencode', 'hermes'],
        summary: {
          passed: 1,
          warning: 1,
          failed: 1
        },
        cells: [],
        duration_ms: 1000,
        copy_summary: 'compatibility matrix completed'
      }
    } as never)

    await adminApi.runCompatibilityMatrix({
      downstream_id: 'test',
      client_profiles: ['codex', 'opencode', 'hermes']
    })

    expect(spy).toHaveBeenCalledWith('/admin/troubleshooting/matrix/run', {
      downstream_id: 'test',
      client_profiles: ['codex', 'opencode', 'hermes']
    })
  })

  it('loads admin active troubleshooting requests', async () => {
    const spy = vi.spyOn(adminHttp, 'get').mockResolvedValue({
      data: { active_requests: [] }
    } as never)

    await adminApi.getActiveTroubleshootingRequests()

    expect(spy).toHaveBeenCalledWith('/admin/troubleshooting/active-requests')
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
