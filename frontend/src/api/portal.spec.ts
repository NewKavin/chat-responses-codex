import { describe, expect, it, vi } from 'vitest'
import { portalApi, portalHttp } from './portal'

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
})
