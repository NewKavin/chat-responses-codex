import { describe, expect, it, vi } from 'vitest'
import { portalApi, portalHttp } from '../../src/api/portal'

describe('portal api', () => {
  it('calls the key read endpoint', async () => {
    const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({
      data: { plaintext_key: 'sk-downstream-123' }
    } as never)

    await portalApi.getKey()

    expect(spy).toHaveBeenCalledWith('/portal/key')
  })

  it('calls the models stats endpoint', async () => {
    const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: [] } as never)

    await portalApi.getModels()

    expect(spy).toHaveBeenCalledWith('/portal/models')
  })

  it('calls the announcement read endpoint', async () => {
    const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: { announcement: null } } as never)

    await portalApi.getAnnouncement()

    expect(spy).toHaveBeenCalledWith('/portal/announcement')
  })

  it('calls the model probe endpoint', async () => {
    const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({
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

    await portalApi.getModelProbe()

    expect(spy).toHaveBeenCalledWith('/portal/model-probe')
  })

})

describe('portal usage api', () => {
  it('keeps portal chart range separate from selected-day logs', async () => {
    const summarySpy = vi.spyOn(portalHttp, 'get').mockResolvedValue({
      data: { time_range: '7d', daily_stats: [] }
    } as never)

    await portalApi.getUsageSummary({ time_range: '7d' })

    expect(summarySpy).toHaveBeenCalledWith('/portal/usage-summary', {
      params: { time_range: '7d' }
    })
  })

  it('requests selected-day logs with day, page, and page_size', async () => {
    const historySpy = vi.spyOn(portalHttp, 'get').mockResolvedValue({
      data: {
        recent_logs: [],
        recent_logs_total: 0,
        recent_logs_page: 1,
        recent_logs_page_size: 10,
        recent_logs_total_pages: 0,
        window: { mode: 'calendar_day', day: '2026-08-01', timezone: 'Asia/Shanghai', start_time: 1, end_time: 2 }
      }
    } as never)

    await portalApi.getUsageHistory({ day: '2026-08-01', page: 1, page_size: 10 })

    expect(historySpy).toHaveBeenCalledWith('/portal/usage-history', {
      params: { day: '2026-08-01', page: 1, page_size: 10 }
    })
  })
})
