import { afterEach, describe, expect, it, vi } from 'vitest'
import type { RuntimeSettings, UpdateRuntimeSettingsRequest } from '@/types'
import { adminApi, adminHttp } from './admin'

afterEach(() => {
  vi.restoreAllMocks()
})

describe('admin runtime settings API', () => {
  it('uses the authenticated singleton GET endpoint', async () => {
    const get = vi.spyOn(adminHttp, 'get').mockResolvedValue({ data: {} })

    await adminApi.getRuntimeSettings()

    expect(get).toHaveBeenCalledWith('/admin/runtime-settings')
  })

  it('forwards the expected revision and complete settings document', async () => {
    const put = vi.spyOn(adminHttp, 'put').mockResolvedValue({ data: {} })
    const request: UpdateRuntimeSettingsRequest = {
      expected_revision: 4,
      settings: {
        routing_affinity_enabled: false,
        upstream_route_exhaustion_retry_max_rounds: 7
      } as RuntimeSettings
    }

    await adminApi.updateRuntimeSettings(request)

    expect(put).toHaveBeenCalledWith('/admin/runtime-settings', request)
  })
})
