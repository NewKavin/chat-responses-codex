// @vitest-environment happy-dom
import { describe, it, expect, beforeEach, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import Upstreams from '../Upstreams.vue'

// Mock API
vi.mock('@/api/admin', () => ({
  adminApi: {
    listUpstreams: vi.fn().mockResolvedValue({ data: [] }),
    discoverUpstreamModels: vi.fn()
  },
  buildSelectedKeyModelMappings: vi.fn(),
  formatModelDiscoveryFailure: vi.fn(),
  mergeDiscoveredModelCandidates: vi.fn()
}))

describe('全局获取模型 - 批量选择功能', () => {
  let wrapper: ReturnType<typeof mount>

  beforeEach(() => {
    const pinia = createPinia()
    wrapper = mount(Upstreams, {
      global: {
        plugins: [pinia]
      }
    })
  })

  describe('全选功能', () => {
    it('点击全选按钮，应选中所有筛选结果中的模型', async () => {
      const vm = wrapper.vm as any

      // 1. 手动设置全局获取模型的状态（模拟 openGlobalFetch 后的状态）
      vm.globalFetchVisible = true
      vm.globalFetching = false
      vm.globalUpstreamModels = {
        'upstream-1': {
          name: 'Upstream 1',
          models: ['gpt-4', 'gpt-3.5-turbo', 'claude-3-opus', 'claude-3-sonnet']
        },
        'upstream-2': {
          name: 'Upstream 2',
          models: ['gpt-4', 'claude-3-opus', 'llama-3-70b']
        }
      }
      vm.globalSelectedModels = []

      await wrapper.vm.$nextTick()
      await wrapper.vm.$nextTick() // 需要等待计算属性更新

      // 验证 globalModelPool 计算属性工作正常（去重）
      expect(vm.globalModelPool.length).toBe(5)

      // 2. 查找全选按钮
      const selectAllButton = wrapper.find('[data-test="select-all-button"]')
      expect(selectAllButton.exists()).toBe(true)

      // 3. 点击全选按钮
      await selectAllButton.trigger('click')
      await wrapper.vm.$nextTick()

      // 4. 断言：所有模型都被选中
      expect(vm.globalSelectedModels.length).toBe(5)
      expect(vm.globalSelectedModels).toContain('gpt-4')
      expect(vm.globalSelectedModels).toContain('gpt-3.5-turbo')
      expect(vm.globalSelectedModels).toContain('claude-3-opus')
      expect(vm.globalSelectedModels).toContain('claude-3-sonnet')
      expect(vm.globalSelectedModels).toContain('llama-3-70b')
    })

    it('点击反选按钮，应反转当前选择状态', async () => {
      const vm = wrapper.vm as any

      // 1. 设置初始状态
      vm.globalFetchVisible = true
      vm.globalFetching = false
      vm.globalUpstreamModels = {
        'upstream-1': {
          name: 'Upstream 1',
          models: ['gpt-4', 'gpt-3.5-turbo', 'claude-3-opus']
        }
      }
      // 初始已选中 2 个模型
      vm.globalSelectedModels = ['gpt-4', 'claude-3-opus']

      await wrapper.vm.$nextTick()
      await wrapper.vm.$nextTick()

      // 验证初始状态
      expect(vm.globalModelPool.length).toBe(3)
      expect(vm.globalSelectedModels.length).toBe(2)

      // 2. 查找反选按钮（这会失败，因为按钮还不存在）
      const invertButton = wrapper.find('[data-test="invert-selection-button"]')
      expect(invertButton.exists()).toBe(true)

      // 3. 点击反选
      await invertButton.trigger('click')
      await wrapper.vm.$nextTick()

      // 4. 断言：原来选中的变未选中，原来未选中的变选中
      expect(vm.globalSelectedModels.length).toBe(1)
      expect(vm.globalSelectedModels).toContain('gpt-3.5-turbo') // 原来未选中
      expect(vm.globalSelectedModels).not.toContain('gpt-4') // 原来选中
      expect(vm.globalSelectedModels).not.toContain('claude-3-opus') // 原来选中
    })

    it('点击清空按钮，应清除所有选择', async () => {
      const vm = wrapper.vm as any

      // 1. 设置初始状态 - 已选中所有模型
      vm.globalFetchVisible = true
      vm.globalFetching = false
      vm.globalUpstreamModels = {
        'upstream-1': {
          name: 'Upstream 1',
          models: ['gpt-4', 'claude-3-opus', 'llama-3-70b']
        }
      }
      vm.globalSelectedModels = ['gpt-4', 'claude-3-opus', 'llama-3-70b']

      await wrapper.vm.$nextTick()
      await wrapper.vm.$nextTick()

      // 验证初始状态
      expect(vm.globalSelectedModels.length).toBe(3)

      // 2. 查找清空按钮（这会失败，因为按钮还不存在）
      const clearButton = wrapper.find('[data-test="clear-selection-button"]')
      expect(clearButton.exists()).toBe(true)

      // 3. 点击清空
      await clearButton.trigger('click')
      await wrapper.vm.$nextTick()

      // 4. 断言：所有选择都被清除
      expect(vm.globalSelectedModels.length).toBe(0)
    })
  })

  describe('预设模型组', () => {
    it('点击"GPT系列"按钮，应选中所有 GPT 模型', async () => {
      const vm = wrapper.vm as any

      // 1. 设置初始状态
      vm.globalFetchVisible = true
      vm.globalFetching = false
      vm.globalUpstreamModels = {
        'upstream-1': {
          name: 'Upstream 1',
          models: ['gpt-4', 'gpt-4-turbo', 'gpt-3.5-turbo', 'claude-3-opus', 'llama-3-70b']
        }
      }
      vm.globalSelectedModels = []

      await wrapper.vm.$nextTick()
      await wrapper.vm.$nextTick()

      // 2. 查找 GPT 系列按钮（这会失败，因为按钮还不存在）
      const gptButton = wrapper.find('[data-test="model-group-gpt"]')
      expect(gptButton.exists()).toBe(true)

      // 3. 点击 GPT 系列按钮
      await gptButton.trigger('click')
      await wrapper.vm.$nextTick()

      // 4. 断言：只选中了 GPT 系列的模型
      expect(vm.globalSelectedModels.length).toBe(3)
      expect(vm.globalSelectedModels).toContain('gpt-4')
      expect(vm.globalSelectedModels).toContain('gpt-4-turbo')
      expect(vm.globalSelectedModels).toContain('gpt-3.5-turbo')
      expect(vm.globalSelectedModels).not.toContain('claude-3-opus')
      expect(vm.globalSelectedModels).not.toContain('llama-3-70b')
    })
  })

  describe('筛选与排序', () => {
    it('输入筛选文本后，只显示匹配的模型', async () => {
      const vm = wrapper.vm as any

      // 1. 设置初始状态
      vm.globalFetchVisible = true
      vm.globalFetching = false
      vm.globalUpstreamModels = {
        'upstream-1': {
          name: 'Upstream 1',
          models: ['gpt-4', 'gpt-3.5-turbo', 'claude-3-opus', 'claude-3-sonnet', 'llama-3-70b']
        }
      }
      vm.globalFilterText = ''
      vm.globalSelectedModels = []

      await wrapper.vm.$nextTick()
      await wrapper.vm.$nextTick()

      // 验证初始状态：所有 5 个模型都可见
      expect(vm.globalModelPool.length).toBe(5)

      // 2. 输入筛选文本 "claude"
      vm.globalFilterText = 'claude'
      await wrapper.vm.$nextTick()

      // 3. 断言：只显示包含 "claude" 的模型（通过计算属性 filteredModels）
      // 这会失败，因为 filteredModels 计算属性还不存在
      expect(vm.filteredModels).toBeDefined()
      expect(vm.filteredModels.length).toBe(2)
      expect(vm.filteredModels).toContain('claude-3-opus')
      expect(vm.filteredModels).toContain('claude-3-sonnet')
      expect(vm.filteredModels).not.toContain('gpt-4')
      expect(vm.filteredModels).not.toContain('llama-3-70b')
    })

    it('筛选支持正则表达式', async () => {
      const vm = wrapper.vm as any

      vm.globalFetchVisible = true
      vm.globalFetching = false
      vm.globalUpstreamModels = {
        'upstream-1': {
          name: 'Upstream 1',
          models: ['gpt-4', 'gpt-4-turbo', 'claude-3-opus', 'llama-3-70b', 'mistral-7b']
        }
      }
      vm.globalFilterText = ''

      await wrapper.vm.$nextTick()
      await wrapper.vm.$nextTick()

      // 使用正则表达式：匹配以 "gpt-4" 开头的模型
      vm.globalFilterText = '^gpt-4'
      await wrapper.vm.$nextTick()

      expect(vm.filteredModels.length).toBe(2)
      expect(vm.filteredModels).toContain('gpt-4')
      expect(vm.filteredModels).toContain('gpt-4-turbo')
      expect(vm.filteredModels).not.toContain('claude-3-opus')
    })
  })
})
