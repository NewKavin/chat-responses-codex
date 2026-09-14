// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import ElementPlus from 'element-plus'
import UsageHistory from './UsageHistory.vue'
import { portalApi } from '@/api/portal'

vi.mock('@/api/portal', () => ({
  portalApi: {
    getUsageHistory: vi.fn(),
    getUsageSummary: vi.fn()
  },
  portalHttp: {}
}))

vi.mock('@/stores/portal', () => ({
  usePortalStore: () => ({ explicitSelection: true, selectedDownstreamId: 'key-2' })
}))

vi.mock('@/utils/echartsLoader', () => ({
  loadEcharts: vi.fn(() =>
    Promise.resolve({
      init: () => ({ setOption: vi.fn(), resize: vi.fn(), dispose: vi.fn() }),
      use: vi.fn()
    } as never)
  )
}))

const baseLog = {
  id: 'log-1',
  endpoint: '/v1/responses',
  model: 'gpt-5.1',
  status_code: 429,
  latency_ms: 120,
  created_at: 1_760_000_000,
  key_name: '我的工作机'
}

describe('UsageHistory client ip and key name', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
      matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(),
      addListener: vi.fn(), removeListener: vi.fn()
    }))
    vi.mocked(portalApi.getUsageSummary).mockResolvedValue({
      data: { time_range: '7d', timezone: 'UTC', start_time: 0, end_time: 0, daily_stats: [] }
    } as never)
    vi.mocked(portalApi.getUsageHistory).mockResolvedValue({
      data: {
        logs: [
          { ...baseLog, id: 'log-1', client_ip: '10.0.0.8' },
          { ...baseLog, id: 'log-2', client_ip: null }
        ],
        total: 2, page: 1, page_size: 20, total_pages: 1,
        mode: 'day', timezone: 'UTC', start_time: 0, end_time: 0
      }
    } as never)
  })

  it('renders key name, client ip and a placeholder when ip is missing', async () => {
    const wrapper = mount(UsageHistory, { global: { plugins: [ElementPlus] } })
    await flushPromises()
    const text = wrapper.text()
    expect(text).toContain('Key')
    expect(text).toContain('我的工作机')
    expect(text).toContain('客户端 IP')
    expect(text).toContain('10.0.0.8')
    expect(text).toContain('未采集')
    wrapper.unmount()
  })

  it('passes the selected key as downstream_id scope', async () => {
    const wrapper = mount(UsageHistory, { global: { plugins: [ElementPlus] } })
    await flushPromises()
    expect(vi.mocked(portalApi.getUsageHistory)).toHaveBeenCalledWith(
      expect.objectContaining({ downstream_id: 'key-2' })
    )
    wrapper.unmount()
  })
})
