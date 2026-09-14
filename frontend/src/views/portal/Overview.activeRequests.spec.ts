// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import ElementPlus from 'element-plus'
import Overview from './Overview.vue'
import { portalApi } from '@/api/portal'

vi.mock('@/api/portal', () => ({
  portalApi: {
    getOverview: vi.fn(),
    getQuota: vi.fn(),
    getModelAccess: vi.fn(),
    getActiveRequests: vi.fn()
  },
  portalHttp: {}
}))

vi.mock('@/stores/portal', () => ({
  usePortalStore: () => ({ explicitSelection: false, selectedDownstreamId: null })
}))

describe('Overview in-flight requests panel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
      matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(),
      addListener: vi.fn(), removeListener: vi.fn()
    }))
    Object.defineProperty(document, 'hidden', { configurable: true, value: false })
    vi.mocked(portalApi.getOverview).mockResolvedValue({
      data: {
        quota_summary: {},
        token_summary: { today: 0, this_month: 0 },
        cost_summary: { last_24h_cents: 0, this_month_cents: 0 },
        model_summary: { total_models: 0, active_models: 0 },
        concurrency: { available: true, running: 1, waiting_upstream: 0, admitted: 1, limit: 10, updated_at: 0 }
      }
    } as never)
    vi.mocked(portalApi.getQuota).mockResolvedValue({
      data: { model_allowlist: [], ip_allowlist: [], model_contexts: [] }
    } as never)
    vi.mocked(portalApi.getModelAccess).mockResolvedValue({
      data: { status: 'allowed', available_models: [] }
    } as never)
    vi.mocked(portalApi.getActiveRequests).mockResolvedValue({
      data: {
        active_requests: [{
          request_id: 'req-abcdef123456',
          endpoint: '/v1/responses',
          model: 'gpt-5.1',
          protocol: 'Responses',
          client_ip: '10.0.0.8',
          user_agent: 'codex/0.146.0',
          key_name: '我的工作机',
          started_at: 1_760_000_000,
          elapsed_seconds: 3,
          idle_seconds: 1,
          status: 'upstream',
          phase: 'streaming',
          queue_position: null
        }],
        refresh_interval_seconds: 2
      }
    } as never)
  })

  it('renders the in-flight panel with client ip', async () => {
    const wrapper = mount(Overview, { global: { plugins: [ElementPlus] } })
    await flushPromises()
    const text = wrapper.text()
    expect(text).toContain('在途请求')
    expect(text).toContain('10.0.0.8')
    expect(text).toContain('gpt-5.1')
    expect(vi.mocked(portalApi.getActiveRequests)).toHaveBeenCalled()
    wrapper.unmount()
  })
})
