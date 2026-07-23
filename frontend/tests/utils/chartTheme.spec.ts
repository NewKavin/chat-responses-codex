import { describe, expect, it } from 'vitest'
import { buildChartTheme } from '../../src/utils/chartTheme'

describe('chart theme', () => {
  it('uses readable semantic colors in light mode', () => {
    const theme = buildChartTheme('light')

    expect(theme.text).toBe('#2b3834')
    expect(theme.muted).toBe('#5c6a65')
    expect(theme.border).toBe('#dce3e0')
    expect(theme.series).toHaveLength(8)
  })

  it('uses neutral charcoal contrast in dark mode', () => {
    const theme = buildChartTheme('dark')

    expect(theme.text).toBe('#b3c4bd')
    expect(theme.tooltipBackground).toBe('#18221e')
    expect(theme.series[0]).toBe('#2fe0a8')
  })
})
