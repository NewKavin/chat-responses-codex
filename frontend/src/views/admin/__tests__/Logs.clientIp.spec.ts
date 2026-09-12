// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import ElementPlus from 'element-plus'
import { createRouter, createMemoryHistory } from 'vue-router'
import Logs from '../Logs.vue'
import { adminApi } from '@/api/admin'

vi.mock('@/api/admin', () => ({
  adminApi: {
    getLogs: vi.fn()
  }
}))

const baseLog = {
  id: 'log-1',
  downstream_key_id: 'down-1',
  upstream_key_id: 'up-1',
  downstream_name: 'team-a',
  upstream_name: 'primary',
  endpoint: '/v1/responses',
  model: 'gpt-5.1',
  request_id: 'req-1',
  status_code: 429,
  wire_status_code: 429,
  prompt_tokens: 10,
  completion_tokens: 0,
  total_tokens: 10,
  latency_ms: 120,
  created_at: 1_760_000_000,
  user_agent: 'codex/0.146.0'
}

describe('Logs client ip column', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(adminApi.getLogs).mockResolvedValue({
      data: {
        logs: [
          { ...baseLog, id: 'log-1', request_id: 'req-1', client_ip: '10.0.0.8' },
          { ...baseLog, id: 'log-2', request_id: 'req-2', client_ip: null }
        ],
        total: 2,
        page: 1,
        page_size: 20,
        total_pages: 1
      }
    } as never)
  })

  it('renders the client ip and a placeholder when missing', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/', component: { template: '<div />' } }]
    })
    const wrapper = mount(Logs, { global: { plugins: [ElementPlus, router] } })
    await flushPromises()

    const text = wrapper.text()
    expect(text).toContain('客户端 IP')
    expect(text).toContain('10.0.0.8')
    expect(text).toContain('未采集')
    wrapper.unmount()
  })
})
