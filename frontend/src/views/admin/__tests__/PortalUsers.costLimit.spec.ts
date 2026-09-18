// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import PortalUsers from '../PortalUsers.vue'
import { adminApi } from '@/api/admin'

vi.mock('@/api/admin', () => ({
  adminApi: {
    getPortalUsers: vi.fn(),
    listModelGroups: vi.fn(),
    getAccessMigration: vi.fn(),
    getPortalUserBindings: vi.fn(),
    getDownstreams: vi.fn(),
    setPortalUserCostLimit: vi.fn(),
    batchUpdateDownstreams: vi.fn(),
    getPortalUserModelGroups: vi.fn(),
    updatePortalUser: vi.fn(),
    setPortalUserModelGroups: vi.fn(),
    setPortalUserDisabled: vi.fn(),
    addPortalUserBinding: vi.fn(),
    deletePortalUserBinding: vi.fn(),
    updateDownstream: vi.fn(),
    batchUserModelGroups: vi.fn(),
    applyAccessMigration: vi.fn()
  }
}))

vi.mock('element-plus', async importOriginal => {
  const actual = await importOriginal<typeof import('element-plus')>()
  return {
    ...actual,
    ElMessage: { success: vi.fn(), error: vi.fn(), warning: vi.fn() },
    ElMessageBox: { confirm: vi.fn().mockResolvedValue(true) }
  }
})

const user = {
  id: 'user-1',
  email: 'u1@example.com',
  display_name: null,
  username: null,
  disabled: false,
  last_login_at: null,
  subject: null,
  binding_count: 1,
  model_group_ids: ['basic'],
  cost_limit_cents: null
}

const binding = {
  downstream_id: 'key-a',
  is_default: true,
  model_group_id: null,
  model_access: { mode: 'inherit' }
}

const downstream = {
  id: 'key-a',
  name: 'Key A',
  active: true,
  is_portal_key: true,
  rate_limit_enabled: true,
  per_minute_limit: 60,
  max_concurrency: 10,
  request_quota_window_hours: 5,
  request_quota_requests: 600,
  billing_mode: 'request',
  input_token_price_per_million_cents: null,
  output_token_price_per_million_cents: null,
  ip_allowlist: []
}

const makeStubs = (
  rowUser: Record<string, unknown>,
  selectionRows: Array<Record<string, unknown>> = []
) => ({
  ElButton: {
    template: '<button type="button" @click="$emit(\'click\')"><slot /></button>',
    emits: ['click']
  },
  ElDialog: {
    template: '<div v-if="modelValue" class="el-dialog"><slot /><slot name="footer" /></div>',
    props: ['modelValue']
  },
  ElTable: {
    template: '<div class="el-table"><slot /></div>',
    mounted() {
      ;(this as unknown as { $emit: (event: string, rows: unknown[]) => void }).$emit(
        'selection-change',
        selectionRows
      )
    }
  },
  ElTableColumn: {
    template: '<div class="el-table-column"><slot :row="row" /></div>',
    data: () => ({ row: rowUser })
  },
  ElInput: { template: '<input />' },
  ElInputNumber: {
    template:
      '<input type="number" :value="modelValue" @input="$emit(\'update:modelValue\', Number($event.target.value))" />',
    props: ['modelValue']
  },
  ElSelect: { template: '<select><slot /></select>' },
  ElOption: { template: '<option />' },
  ElPagination: { template: '<div />' },
  ElTag: { template: '<span><slot /></span>' },
  ElForm: { template: '<form><slot /></form>' },
  ElFormItem: { template: '<div><slot /></div>' },
  ElSwitch: {
    template: '<button type="button"><slot /></button>',
    props: ['modelValue']
  },
  ElDivider: { template: '<div><slot /></div>' },
  ElRadioGroup: {
    template: '<div class="radio-group"><slot /></div>',
    props: ['modelValue']
  },
  ElRadioButton: {
    template:
      '<label><input type="radio" :checked="modelValue === value" @change="$emit(\'update:modelValue\', value)" /><slot /></label>',
    props: ['modelValue', 'value']
  },
  ElAlert: { template: '<div class="el-alert"><slot /></div>' },
  ElCheckbox: {
    template:
      '<label><input type="checkbox" :checked="modelValue" @change="$emit(\'update:modelValue\', $event.target.checked)" /><slot /></label>',
    props: ['modelValue']
  },
  ElDatePicker: { template: '<input />' },
  ElTooltip: { template: '<span><slot /></span>' },
  ElCard: { template: '<div><slot /></div>' }
})

const stubApi = () => {
  vi.mocked(adminApi.getPortalUsers).mockResolvedValue({
    data: { total: 1, page: 1, page_size: 20, items: [user] }
  } as any)
  vi.mocked(adminApi.listModelGroups).mockResolvedValue({
    data: {
      groups: [
        { id: 'basic', name: 'Basic', description: null, allowed_models: [], created_at: 1, updated_at: 1 }
      ]
    }
  } as any)
  vi.mocked(adminApi.getAccessMigration).mockResolvedValue({
    data: { summary: { pending: 0, total: 0, resolved: 0 } }
  } as any)
  vi.mocked(adminApi.getPortalUserBindings).mockResolvedValue({
    data: { items: [binding] }
  } as any)
  vi.mocked(adminApi.getDownstreams).mockResolvedValue({ data: [downstream] } as any)
}

const mountPage = async () => {
  stubApi()
  const wrapper = mount(PortalUsers, {
    global: {
      stubs: makeStubs(user)
    }
  })
  await flushPromises()
  return wrapper
}

const clickButton = async (wrapper: ReturnType<typeof mount>, text: string) => {
  const button = wrapper.findAll('button').find(b => b.text().includes(text))
  expect(button).toBeDefined()
  // stubs 不建模 :disabled 语义；去掉 disabled 属性避免 happy-dom 吞掉 click。
  button!.element.removeAttribute('disabled')
  await button!.trigger('click')
  await flushPromises()
}

describe('PortalUsers account cost limit', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn()
    }))
  })

  it('writes the account cost limit instead of per-key limits', async () => {
    vi.mocked(adminApi.setPortalUserCostLimit).mockResolvedValue({
      data: { user_id: 'user-1', daily_limit_cents: 5000 }
    } as any)
    const wrapper = await mountPage()

    await clickButton(wrapper, '绑定')
    await clickButton(wrapper, '设账号日上限')

    const input = wrapper.find('input[type="number"]')
    await input.setValue(50)
    await flushPromises()
    await clickButton(wrapper, '保存')

    expect(adminApi.setPortalUserCostLimit).toHaveBeenCalledWith('user-1', 5000)
    expect(adminApi.batchUpdateDownstreams).not.toHaveBeenCalled()
  })

  it('batch limit save does not send a per-key daily cost limit', async () => {
    vi.mocked(adminApi.batchUpdateDownstreams).mockResolvedValue({
      data: { updated: ['key-a'], failed: [] }
    } as any)
    vi.mocked(adminApi.getPortalUserBindings).mockResolvedValue({ data: { items: [binding] } } as any)
    const wrapper = mount(PortalUsers, {
      global: { stubs: makeStubs(user, [binding]) }
    })
    await flushPromises()

    await clickButton(wrapper, '绑定')
    await clickButton(wrapper, '批量改限额')
    await clickButton(wrapper, '保存')

    expect(adminApi.batchUpdateDownstreams).toHaveBeenCalledTimes(1)
    const calls = vi.mocked(adminApi.batchUpdateDownstreams).mock.calls[0]
    const payload = calls[1] as Record<string, unknown>
    expect(payload.daily_cost_limit_cents).toBeUndefined()
  })
})
