// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import TableColumnSettings from './TableColumnSettings.vue'

const columns = [
  { key: 'name', label: '名称' },
  { key: 'base_url', label: 'Base URL' },
  { key: 'remark', label: '备注' }
]

const render = () => mount(TableColumnSettings, {
  props: {
    columns,
    modelValue: ['name'],
    defaultKeys: ['name', 'remark']
  },
  global: {
    stubs: {
      ElPopover: { template: '<div><slot /><slot name="reference" /></div>' }
    }
  }
})

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn()
  }))
})

describe('TableColumnSettings', () => {
  it('renders one searchable checkbox row per column', async () => {
    const wrapper = render()
    const labels = () => wrapper.findAll('.table-column-option').map(row => row.text())

    expect(labels()).toEqual(['名称', 'Base URL', '备注'])

    await wrapper.find('.table-column-search input').setValue('base')
    expect(labels()).toEqual(['Base URL'])
  })

  it('keeps at least one column selected', async () => {
    const wrapper = render()
    const checkboxes = wrapper.findAll('input[type="checkbox"]')
    await checkboxes[0].setValue(false)

    expect(wrapper.emitted('update:modelValue')).toBeUndefined()
  })
})
