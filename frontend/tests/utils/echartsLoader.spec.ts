import { GraphicComponent } from 'echarts/components'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { __resetEchartsLoaderForTests, loadEcharts } from '../../src/utils/echartsLoader'

const echartsUseMock = vi.hoisted(() => vi.fn())

vi.mock('echarts/core', async importOriginal => {
  const actual = await importOriginal<typeof import('echarts/core')>()
  return {
    ...actual,
    use: echartsUseMock
  }
})

describe('echartsLoader', () => {
  beforeEach(() => {
    __resetEchartsLoaderForTests()
    echartsUseMock.mockClear()
  })

  it('caches the same import promise and resolves echarts module', async () => {
    const first = loadEcharts()
    const second = loadEcharts()

    expect(first).toBe(second)

    const module = await first
    expect(typeof module.init).toBe('function')
  })

  it('registers the graphic component for chart empty states', async () => {
    await loadEcharts()

    const registeredComponents = echartsUseMock.mock.calls[0][0]
    expect(registeredComponents).toContain(GraphicComponent)
  })
})
