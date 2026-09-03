// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import KeyCard from './KeyCard.vue'
import type { PortalKey } from '@/api/portal'

const mockKey: PortalKey = {
  downstream_id: 'sk-test123abc',
  label: 'Test Key',
  model_group_id: 'default',
  created_at: Date.now() / 1000 - 86400 * 2, // 2 days ago
  usage_count: 1234,
  is_default: false,
}

const mockHandlers = {
  onEdit: vi.fn().mockResolvedValue(undefined),
  onRotate: vi.fn().mockResolvedValue(undefined),
  onDelete: vi.fn().mockResolvedValue(undefined),
  onSetDefault: vi.fn().mockResolvedValue(undefined),
}

const render = (keyData: PortalKey = mockKey) => mount(KeyCard, {
  props: {
    keyData,
    ...mockHandlers
  },
  global: {
    stubs: {
      ElButton: { template: '<button><slot /></button>' },
      ElDialog: { template: '<div v-if="modelValue"><slot /><slot name="footer" /></div>', props: ['modelValue'] },
      ElInput: { template: '<input :value="modelValue" @input="$emit(\'update:modelValue\', $event.target.value)" />', props: ['modelValue'] },
      ElTooltip: { template: '<div><slot /></div>' },
    }
  }
})

beforeEach(() => {
  vi.clearAllMocks()
  vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn()
  }))
})

describe('KeyCard', () => {
  it('renders key information correctly', () => {
    const wrapper = render()

    expect(wrapper.text()).toContain('sk-***23abc')
    expect(wrapper.text()).toContain('Test Key')
    expect(wrapper.text()).toContain('1,234')
    expect(wrapper.text()).toContain('2 days ago')
  })

  it('shows default badge when is_default is true', () => {
    const defaultKey = { ...mockKey, is_default: true }
    const wrapper = render(defaultKey)

    expect(wrapper.text()).toContain('DEFAULT')
  })

  it('masks key ID correctly', () => {
    const wrapper = render()

    expect(wrapper.text()).toContain('sk-***23abc')
    expect(wrapper.text()).not.toContain('sk-test123abc')
  })

  it('enables label editing', async () => {
    const wrapper = render()

    const editButton = wrapper.find('[aria-label="Edit label"]')
    await editButton.trigger('click')

    const input = wrapper.find('input[type="text"]')
    await input.setValue('New Label')

    const saveButton = wrapper.find('[aria-label="Save label"]')
    await saveButton.trigger('click')

    expect(mockHandlers.onEdit).toHaveBeenCalledWith('sk-test123abc', 'New Label')
  })

  it('copies key ID to clipboard', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', {
      clipboard: { writeText },
    })

    const wrapper = render()

    const copyButton = wrapper.find('[aria-label="Copy key ID"]')
    await copyButton.trigger('click')

    expect(writeText).toHaveBeenCalledWith('sk-test123abc')
  })

  it('calls onSetDefault when set default button clicked', async () => {
    const wrapper = render()

    const defaultButton = wrapper.find('[aria-label="Set as default"]')
    await defaultButton.trigger('click')

    expect(mockHandlers.onSetDefault).toHaveBeenCalledWith('sk-test123abc')
  })

  it('shows confirmation dialog before delete', async () => {
    const wrapper = render()

    const deleteButton = wrapper.find('[aria-label="Delete key"]')
    await deleteButton.trigger('click')

    expect(wrapper.text()).toContain('确认删除')

    const confirmButton = wrapper.findAll('button').find(btn => btn.text().includes('确认'))
    await confirmButton?.trigger('click')

    expect(mockHandlers.onDelete).toHaveBeenCalledWith('sk-test123abc')
  })

  it('shows rotate dialog with input', async () => {
    const wrapper = render()

    const rotateButton = wrapper.find('[aria-label="Rotate key"]')
    await rotateButton.trigger('click')

    expect(wrapper.text()).toMatch(/输入新的密钥/)

    const input = wrapper.find('input[placeholder*="新密钥"]')
    await input.setValue('sk-newkey456')

    const confirmButton = wrapper.findAll('button').find(btn => btn.text().includes('确认'))
    await confirmButton?.trigger('click')

    expect(mockHandlers.onRotate).toHaveBeenCalledWith('sk-test123abc', 'sk-newkey456')
  })

  it('shows loading state during operation', async () => {
    mockHandlers.onEdit.mockImplementation(() => new Promise(resolve => setTimeout(resolve, 100)))

    const wrapper = render()

    const editButton = wrapper.find('[aria-label="Edit label"]')
    await editButton.trigger('click')

    const input = wrapper.find('input[type="text"]')
    await input.setValue('New')

    const saveButton = wrapper.find('[aria-label="Save label"]')
    await saveButton.trigger('click')

    expect(wrapper.find('[aria-label="Save label"]').attributes('disabled')).toBeDefined()
  })

  it('shows error state on operation failure', async () => {
    mockHandlers.onDelete.mockRejectedValue(new Error('Cannot delete default key'))

    const wrapper = render()

    const deleteButton = wrapper.find('[aria-label="Delete key"]')
    await deleteButton.trigger('click')

    const confirmButton = wrapper.findAll('button').find(btn => btn.text().includes('确认'))
    await confirmButton?.trigger('click')

    await vi.waitFor(() => {
      expect(wrapper.text()).toContain('Cannot delete default key')
    })
  })

  it('formats relative time correctly', () => {
    const now = Date.now() / 1000
    const cases = [
      { created_at: now - 30, expected: 'just now' },
      { created_at: now - 90, expected: '1 minute ago' },
      { created_at: now - 3600, expected: '1 hour ago' },
      { created_at: now - 7200, expected: '2 hours ago' },
      { created_at: now - 86400, expected: '1 day ago' },
      { created_at: now - 172800, expected: '2 days ago' },
    ]

    cases.forEach(({ created_at, expected }) => {
      const wrapper = render({ ...mockKey, created_at })
      expect(wrapper.text()).toContain(expected)
    })
  })

  it('formats usage count with commas', () => {
    const wrapper = render({ ...mockKey, usage_count: 1234567 })
    expect(wrapper.text()).toContain('1,234,567')
  })
})
