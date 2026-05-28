import { describe, expect, it } from 'vitest'
import { formatPercentageTwoDecimals } from './percentage'

describe('formatPercentageTwoDecimals', () => {
  it('keeps two decimals with standard rounding', () => {
    expect(formatPercentageTwoDecimals(33.335)).toBe(33.34)
    expect(formatPercentageTwoDecimals(99.994)).toBe(99.99)
  })

  it('handles invalid values safely', () => {
    expect(formatPercentageTwoDecimals(Number.NaN)).toBe(0)
    expect(formatPercentageTwoDecimals(Number.POSITIVE_INFINITY)).toBe(0)
  })
})
