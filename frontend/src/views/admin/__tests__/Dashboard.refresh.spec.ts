// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { nextTick } from 'vue'
import ElementPlus from 'element-plus'
import { createRouter, createMemoryHistory } from 'vue-router'
import Dashboard from '../Dashboard.vue'
import { adminApi } from '@/api/admin'
import { __resetEchartsLoaderForTests } from '@/utils/echartsLoader'

vi.mock('@/api/admin', () => ({
  adminApi: {
    getDashboard: vi.fn(),
    getActiveTroubleshootingRequests: vi.fn(),
    getRetryAmplification: vi.fn(),
    getModelProbe: vi.fn()
  }
}))

// ECharts 在 happy-dom 无布局；替换为无操作 loader，保留调用路径。
vi.mock('@/utils/echartsLoader', async importOriginal => {
  const original = await importOriginal<typeof import('@/utils/echartsLoader')>()
  return {
    ...original,
    loadEcharts: vi.fn(() =>
      Promise.resolve({
        init: () => ({
          setOption: vi.fn(),
          getOption: () => ({}),
          resize: vi.fn(),
          dispose: vi.fn()
        }),
        use: vi.fn()
      } as never)
    )
  }
})

const emptyDashboard = () => ({
  dashboard: {
    upstreams_count: 0,
    upstreams_active: 0,
    downstreams_count: 0,
    downstreams_active: 0,
    logs_count: 0,
    active_models: 0,
    responses_upstreams: 0,
    admin_username: '',
    app_name: '',
    errors_last_24h: 0,
    today_requests: 0,
    month_requests: 0,
    today_api_errors: 0,
    diagnostics_status: 'ok'
  },
  analytics: {
    range: '7d',
    summary: {
      total_requests: 0,
      success_rate: 1,
      average_latency_ms: 0,
      total_tokens: 0
    },
    daily_series: [],
    failure_categories: [],
    user_agent_clusters: [],
    model_usage: [],
    downstream_usage: []
  }
})

const mounted: ReturnType<typeof mount>[] = []
const deferred = <T,>() => {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(done => { resolve = done })
  return { promise, resolve }
}
const mountDashboard = async () => {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', component: { template: '<div />' } }]
  })
  const wrapper = mount(Dashboard, {
    global: {
      plugins: [ElementPlus, router]
    }
  })
  mounted.push(wrapper)
  await flushPromises()
  await nextTick()
  return wrapper
}

describe('Dashboard refresh state machine', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    Object.defineProperty(document, 'hidden', { configurable: true, value: false })
    vi.clearAllMocks()
    __resetEchartsLoaderForTests()
    vi.mocked(adminApi.getDashboard).mockResolvedValue({ data: emptyDashboard() } as never)
    vi.mocked(adminApi.getActiveTroubleshootingRequests).mockReset().mockResolvedValue({
      data: { active_requests: [] }
    } as never)
    vi.mocked(adminApi.getRetryAmplification).mockResolvedValue({
      data: { window_seconds: 300, categories: [], summary: { total: 0 } }
    } as never)
    vi.mocked(adminApi.getModelProbe).mockResolvedValue({
      data: { channels: [], models: [], summary: {} }
    } as never)
  })

  afterEach(() => {
    mounted.splice(0).forEach(wrapper => wrapper.unmount())
    vi.clearAllTimers()
    vi.useRealTimers()
  })

  it('never stacks duplicate loads while a request is in flight', async () => {
    let resolveDashboard: (value: never) => void = () => {}
    vi.mocked(adminApi.getDashboard).mockReturnValue(
      new Promise(resolve => {
        resolveDashboard = resolve
      }) as never
    )
    const wrapper = await mountDashboard()
    const header = wrapper.find('.dashboard-deck')
    void (header.exists() ? (wrapper.vm as unknown as { loadDashboard: () => void }).loadDashboard() : Promise.resolve())
    void (wrapper.vm as unknown as { loadDashboard: () => void }).loadDashboard()
    void (wrapper.vm as unknown as { loadDashboard: () => void }).loadDashboard()
    expect(vi.mocked(adminApi.getDashboard).mock.calls.length).toBe(1)
    resolveDashboard({ data: emptyDashboard() } as never)
    await flushPromises()
  })

  it('stale responses never overwrite fresher data (sequencing)', async () => {
    const wrapper = await mountDashboard()
    const old = deferred<any>()
    const latest = deferred<any>()
    vi.mocked(adminApi.getDashboard).mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise)
    const vm = wrapper.vm as any
    vm.handleRangeChange('1d')
    vm.handleRangeChange('30d')
    latest.resolve({ data: { ...emptyDashboard(), dashboard: { ...emptyDashboard().dashboard, upstreams_count: 7 } } })
    await flushPromises()
    old.resolve({ data: { ...emptyDashboard(), dashboard: { ...emptyDashboard().dashboard, upstreams_count: 99 } } })
    await flushPromises()
    expect(vm.dashboard.upstreams_count).toBe(7)
  })

  it('refreshes active requests every two seconds by default without changing retry polling', async () => {
    await mountDashboard()
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1999)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(2)
    expect(adminApi.getRetryAmplification).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(2000)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(3)
    await vi.advanceTimersByTimeAsync(1000)
    expect(adminApi.getRetryAmplification).toHaveBeenCalledTimes(2)
  })

  it('uses the latest response interval for the next active request refresh', async () => {
    vi.mocked(adminApi.getActiveTroubleshootingRequests)
      .mockResolvedValueOnce({ data: { active_requests: [], refresh_interval_seconds: 3 } } as never)
      .mockResolvedValue({ data: { active_requests: [], refresh_interval_seconds: 7 } } as never)
    await mountDashboard()
    await vi.advanceTimersByTimeAsync(2999)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(6999)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(1)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(3)
  })

  it('does not stack active request refreshes while a background request is pending', async () => {
    const wrapper = await mountDashboard()
    const active = deferred<any>()
    vi.mocked(adminApi.getActiveTroubleshootingRequests).mockReturnValueOnce(active.promise)
    await vi.advanceTimersByTimeAsync(2000)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(2)
    void (wrapper.vm as any).loadActiveRequests()
    await vi.advanceTimersByTimeAsync(10000)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(2)
    active.resolve({ data: { active_requests: [], refresh_interval_seconds: 2 } })
    await flushPromises()
    await vi.advanceTimersByTimeAsync(2000)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(3)
  })

  it('pauses active request polling while hidden and refreshes on return', async () => {
    await mountDashboard()
    Object.defineProperty(document, 'hidden', { configurable: true, value: true })
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(10000)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(1)
    Object.defineProperty(document, 'hidden', { configurable: true, value: false })
    document.dispatchEvent(new Event('visibilitychange'))
    await flushPromises()
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(2000)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(3)
  })

  it('keeps empty states and loading masks unchanged during a background refresh', async () => {
    const wrapper = await mountDashboard()
    const active = deferred<any>()
    const retry = deferred<any>()
    vi.mocked(adminApi.getActiveTroubleshootingRequests).mockReturnValueOnce(active.promise)
    vi.mocked(adminApi.getRetryAmplification).mockReturnValueOnce(retry.promise)
    const empty = wrapper.findAll('.el-empty').map(node => node.element)
    await vi.advanceTimersByTimeAsync(5000)
    await nextTick()
    const vm = wrapper.vm as any
    expect(vm.activeRequestsLoading).toBe(false)
    expect(vm.retryLoading).toBe(false)
    expect(wrapper.findAll('.el-empty').map(node => node.element)).toEqual(empty)
    active.resolve({ data: { active_requests: [] } })
    retry.resolve({ data: { total: 0, points: [] } })
    await flushPromises()
  })

  it('does not restart polling after unmount while a refresh is pending', async () => {
    const wrapper = await mountDashboard()
    const active = deferred<any>()
    vi.mocked(adminApi.getActiveTroubleshootingRequests).mockReturnValueOnce(active.promise)
    await vi.advanceTimersByTimeAsync(5000)
    wrapper.unmount()
    const count = vi.mocked(adminApi.getActiveTroubleshootingRequests).mock.calls.length
    active.resolve({ data: { active_requests: [] } })
    await flushPromises()
    await vi.advanceTimersByTimeAsync(30000)
    expect(adminApi.getActiveTroubleshootingRequests).toHaveBeenCalledTimes(count)
  })

  it('keeps last successful data and marks the error state on failure', async () => {
    vi.mocked(adminApi.getDashboard).mockResolvedValue({ data: emptyDashboard() } as never)
    const wrapper = await mountDashboard()
    const vm = wrapper.vm as unknown as {
      dashboard: { upstreams_count: number }
      refreshState: string
      refreshError: string
      loadDashboard: () => Promise<void>
    }
    expect(vm.refreshState).toBe('ready')
    vi.mocked(adminApi.getDashboard).mockRejectedValueOnce(new Error('network down'))
    await vm.loadDashboard()
    expect(vm.refreshState).toBe('error')
    expect(vm.refreshError).toBe('network down')
    // 上次成功数据仍在（后台保留 DOM/数据）。
    expect(wrapper.text()).toContain('TOTAL REQUESTS')
  })

  it('shows the client ip of active requests', async () => {
    vi.mocked(adminApi.getActiveTroubleshootingRequests).mockResolvedValue({
      data: {
        active_requests: [
          {
            request_id: 'req-1',
            downstream_id: 'down-1',
            downstream_name: 'team-a',
            endpoint: '/v1/responses',
            model: 'gpt-4',
            protocol: 'Responses',
            user_agent: 'codex/0.146.0',
            client_ip: '10.0.0.8',
            upstream_id: null,
            upstream_name: null,
            started_at: 1_760_000_000,
            last_event_at: 1_760_000_000,
            elapsed_seconds: 3,
            idle_seconds: 1,
            status: 'routing',
            error_category: null,
            phase: 'selecting',
            queue_position: null
          }
        ],
        refresh_interval_seconds: 2
      }
    } as never)
    const wrapper = await mountDashboard()
    expect(wrapper.text()).toContain('客户端 IP')
    expect(wrapper.text()).toContain('10.0.0.8')
  })
})
