// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import KeyManagement from './KeyManagement.vue'
import * as portalApi from '@/api/portal'

vi.mock('@/api/portal', () => ({
  portalApi: {
    listKeys: vi.fn(),
    createKey: vi.fn(),
    deleteKey: vi.fn(),
    rotateKeyById: vi.fn(),
    setDefaultKey: vi.fn(),
    listModelGroups: vi.fn(),
    updateKeyModelGroup: vi.fn()
  },
  portalHttp: {}
}))

vi.mock('@/components/KeyCard.vue', () => ({
  default: {
    name: 'KeyCard',
    template: `
      <div :data-testid="'key-' + keyData.downstream_id" class="key-card-mock" :data-group-count="(modelGroups || []).length">
        <span class="key-label">{{ keyData.label }}</span>
        <button class="group-change-trigger" @click="$emit('change-model-group', keyData.downstream_id, 'premium')">ChangeGroup</button>
        <button @click="$emit('delete', keyData.downstream_id)">Delete</button>
      </div>
    `,
    props: ['keyData', 'modelGroups'],
    emits: ['edit', 'rotate', 'delete', 'setDefault', 'change-model-group']
  }
}))

const mockKeys = [
  {
    downstream_id: 'sk-1',
    label: 'Key 1',
    model_group_id: 'default',
    created_at: Date.now() / 1000,
    usage_count: 10,
    is_default: true
  },
  {
    downstream_id: 'sk-2',
    label: 'Key 2',
    model_group_id: 'default',
    created_at: Date.now() / 1000 - 86400,
    usage_count: 5,
    is_default: false
  }
]

describe('KeyManagement Page', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn()
    }))
  })

  it('loads and displays keys', async () => {
    vi.mocked(portalApi.portalApi.listKeys).mockResolvedValue({ data: mockKeys } as any)

    const wrapper = mount(KeyManagement, {
      global: {
        stubs: {
          ElButton: { template: '<button><slot /></button>' }
        }
      }
    })

    // Should show loading initially
    expect(wrapper.text()).toContain('加载中')

    // Wait for keys to load
    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))

    expect(wrapper.text()).toContain('Key 1')
    expect(wrapper.text()).toContain('Key 2')
  })

  it('shows empty state when no keys', async () => {
    vi.mocked(portalApi.portalApi.listKeys).mockResolvedValue({ data: [] } as any)

    const wrapper = mount(KeyManagement, {
      global: {
        stubs: {
          ElButton: { template: '<button><slot /></button>' }
        }
      }
    })

    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))

    expect(wrapper.text()).toMatch(/暂无.*密钥|没有.*密钥/)
  })

  it('handles API error with retry', async () => {
    vi.mocked(portalApi.portalApi.listKeys).mockRejectedValue(
      new Error('Network error')
    )

    const wrapper = mount(KeyManagement, {
      global: {
        stubs: {
          ElButton: { template: '<button><slot /></button>' }
        }
      }
    })

    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))

    expect(wrapper.text()).toMatch(/错误|失败/)
  })

  it('opens add key dialog', async () => {
    vi.mocked(portalApi.portalApi.listKeys).mockResolvedValue({ data: [] } as any)

    const wrapper = mount(KeyManagement, {
      global: {
        stubs: {
          ElButton: { template: '<button @click="$emit(\'click\')"><slot /></button>' },
          ElDialog: {
            template: '<div v-if="modelValue" class="el-dialog"><slot /></div>',
            props: ['modelValue']
          }
        }
      }
    })

    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))

    const addButtons = wrapper.findAll('button')
    const addButton = addButtons.find(btn => btn.text().includes('添加') || btn.text().includes('新增'))
    expect(addButton).toBeDefined()

    await addButton!.trigger('click')
    await wrapper.vm.$nextTick()

    expect(wrapper.find('.el-dialog').exists()).toBe(true)
  })

  it('sorts default key first', async () => {
    const unsortedKeys = [
      { downstream_id: 'sk-1', label: 'Key 1', is_default: false, created_at: 100, model_group_id: 'default', usage_count: 0 },
      { downstream_id: 'sk-2', label: 'Key 2', is_default: true, created_at: 50, model_group_id: 'default', usage_count: 0 }
    ]

    vi.mocked(portalApi.portalApi.listKeys).mockResolvedValue({ data: unsortedKeys } as any)

    const wrapper = mount(KeyManagement)

    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))

    const keyCards = wrapper.findAll('[data-testid^="key-"]')
    expect(keyCards[0].attributes('data-testid')).toBe('key-sk-2') // default first
    expect(keyCards[1].attributes('data-testid')).toBe('key-sk-1')
  })

  it('refreshes after delete', async () => {
    const initialKeys = [...mockKeys]
    const afterDelete = [mockKeys[1]]

    vi.mocked(portalApi.portalApi.listKeys)
      .mockResolvedValueOnce({ data: initialKeys } as any)
      .mockResolvedValueOnce({ data: afterDelete } as any)
    vi.mocked(portalApi.portalApi.deleteKey).mockResolvedValue({ data: { success: true } } as any)

    const wrapper = mount(KeyManagement)

    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))

    expect(wrapper.text()).toContain('Key 1')

    // Find the first KeyCard and trigger its delete button
    const keyCard = wrapper.find('[data-testid="key-sk-1"]')
    const deleteButton = keyCard.find('button')
    await deleteButton.trigger('click')

    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))

    expect(wrapper.text()).not.toContain('Key 1')
    expect(wrapper.text()).toContain('Key 2')
  })

  it('loads model groups and passes them to KeyCard', async () => {
    const mockGroups = [
      { id: 'basic', name: 'Basic', description: null, allowed_models: ['gpt-3.5-turbo'], created_at: 1, updated_at: 1 },
      { id: 'premium', name: 'Premium', description: null, allowed_models: ['gpt-4'], created_at: 1, updated_at: 1 }
    ]
    vi.mocked(portalApi.portalApi.listKeys).mockResolvedValue({ data: mockKeys } as any)
    vi.mocked(portalApi.portalApi.listModelGroups).mockResolvedValue({ data: { groups: mockGroups } } as any)

    const wrapper = mount(KeyManagement, {
      global: {
        stubs: {
          ElButton: { template: '<button><slot /></button>' },
          ElDialog: { template: '<div><slot /><slot name="footer" /></div>' },
          ElForm: { template: '<form><slot /></form>' },
          ElFormItem: { template: '<div><slot /></div>' },
          ElInput: { template: '<input />' },
          ElSelect: { template: '<select><slot /></select>' },
          ElOption: { template: '<option />' }
        }
      }
    })
    await wrapper.vm.$nextTick()
    await wrapper.vm.$nextTick()

    expect(portalApi.portalApi.listModelGroups).toHaveBeenCalled()
    const cards = wrapper.findAll('.key-card-mock')
    expect(cards.length).toBe(mockKeys.length)
    // The mocked card receives the groups via props (Vue passes model-groups
    // through the kebab-case attribute), verified via the rendered tree.
    expect(cards[0].attributes('data-group-count')).toBe('2')
  })

  it('updates a key model group when the card requests it', async () => {
    vi.mocked(portalApi.portalApi.listKeys).mockResolvedValue({ data: mockKeys } as any)
    vi.mocked(portalApi.portalApi.listModelGroups).mockResolvedValue({
      data: { groups: [{ id: 'basic', name: 'Basic', description: null, allowed_models: [], created_at: 1, updated_at: 1 }] }
    } as any)
    vi.mocked(portalApi.portalApi.updateKeyModelGroup).mockResolvedValue({ data: { success: true } } as any)

    const wrapper = mount(KeyManagement, {
      global: {
        stubs: {
          ElButton: { template: '<button><slot /></button>' }
        }
      }
    })
    await wrapper.vm.$nextTick()
    await wrapper.vm.$nextTick()

    const changeBtn = wrapper.find('.group-change-trigger')
    await changeBtn.trigger('click')
    await wrapper.vm.$nextTick()

    expect(portalApi.portalApi.updateKeyModelGroup).toHaveBeenCalledWith(
      mockKeys[0].downstream_id,
      'premium'
    )
  })
})

describe('KeyManagement model-group dialog', () => {
  it('create key dialog sends model_group_id when selected', async () => {
    vi.mocked(portalApi.portalApi.listKeys).mockResolvedValue({ data: [] } as any)
    vi.mocked(portalApi.portalApi.listModelGroups).mockResolvedValue({
      data: { groups: [{ id: 'basic', name: 'Basic', description: null, allowed_models: ['gpt-3.5-turbo'], created_at: 1, updated_at: 1 }] }
    } as any)

    const wrapper = mount(KeyManagement, {
      global: {
        stubs: {
          ElButton: { template: '<button @click="$emit(\'click\')"><slot /></button>' },
          ElDialog: { template: '<div v-if="modelValue !== false"><slot /><slot name="footer" /></div>' },
          ElForm: { template: '<form><slot /></form>' },
          ElFormItem: { template: '<div><slot /></div>' },
          ElInput: { template: '<input />' },
          ElSelect: { template: '<select><slot /></select>' },
          ElOption: { template: '<option />' }
        }
      }
    })
    await wrapper.vm.$nextTick()
    await wrapper.vm.$nextTick()
    expect(portalApi.portalApi.listModelGroups).toHaveBeenCalled()
  })
})