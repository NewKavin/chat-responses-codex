import { describe, expect, it } from 'vitest'
import { buildChartTheme } from '../../src/utils/chartTheme'

describe('chart theme', () => {
  it('uses readable semantic colors in light mode', () => {
    const theme = buildChartTheme('light')

    expect(theme.text).toBe('#34413d')
    expect(theme.muted).toBe('#66716d')
    expect(theme.border).toBe('#dfe5e2')
    expect(theme.series).toHaveLength(8)
  })

  it('uses neutral charcoal contrast in dark mode', () => {
    const theme = buildChartTheme('dark')

    expect(theme.text).toBe('#d2dad6')
    expect(theme.tooltipBackground).toBe('#202624')
    expect(theme.series[0]).toBe('#39b99c')
  })
})
