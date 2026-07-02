import { describe, expect, it } from 'vitest'
import { __resetEchartsLoaderForTests, loadEcharts } from '../../src/utils/echartsLoader'

describe('echartsLoader', () => {
  it('caches the same import promise and resolves echarts module', async () => {
    __resetEchartsLoaderForTests()

    const first = loadEcharts()
    const second = loadEcharts()

    expect(first).toBe(second)

    const module = await first
    expect(typeof module.init).toBe('function')
  })
})
