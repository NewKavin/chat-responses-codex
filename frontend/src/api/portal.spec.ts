import { describe, expect, it, vi } from 'vitest'
import { portalApi, portalHttp } from './portal'

describe('portal announcement api', () => {
  it('calls the announcement read endpoint', async () => {
    const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: { announcement: null } } as never)

    await portalApi.getAnnouncement()

    expect(spy).toHaveBeenCalledWith('/portal/announcement')
  })
})
