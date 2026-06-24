import { describe, expect, it } from 'vitest'
import { groupTopBreakdownItems, sortBreakdownItems } from './dashboardCharts'

describe('dashboardCharts', () => {
  it('sorts breakdown items by value descending and name ascending', () => {
    const items = sortBreakdownItems([
      { name: 'beta', value: 2 },
      { name: 'alpha', value: 2 },
      { name: 'gamma', value: 5 }
    ])

    expect(items).toEqual([
      { name: 'gamma', value: 5 },
      { name: 'alpha', value: 2 },
      { name: 'beta', value: 2 }
    ])
  })

  it('groups overflow items into other', () => {
    const grouped = groupTopBreakdownItems([
      { name: 'A', value: 6 },
      { name: 'B', value: 4 },
      { name: 'C', value: 3 },
      { name: 'D', value: 2 }
    ], 2)

    expect(grouped.items).toEqual([
      { name: 'A', value: 6 },
      { name: 'B', value: 4 },
      { name: '其他', value: 5 }
    ])
    expect(grouped.total).toBe(15)
  })
})
