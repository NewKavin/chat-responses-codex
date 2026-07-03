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

  it('runs portal troubleshooting diagnostics', async () => {
    const spy = vi.spyOn(portalHttp, 'post').mockResolvedValue({
      data: { run_id: 'diag_1', results: [] }
    } as never)

    await portalApi.runTroubleshooting({
      client_profile: 'cline',
      model: 'GLM-5.1',
      checks: ['models']
    })

    expect(spy).toHaveBeenCalledWith('/portal/troubleshooting/run', {
      client_profile: 'cline',
      model: 'GLM-5.1',
      checks: ['models']
    })
  })

  it('loads portal active troubleshooting requests', async () => {
    const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({
      data: { active_requests: [] }
    } as never)

    await portalApi.getActiveTroubleshootingRequests()

    expect(spy).toHaveBeenCalledWith('/portal/troubleshooting/active-requests')
  })
})
