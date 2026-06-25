import { describe, expect, it } from 'vitest'
import { formatInferenceStrength } from './logDisplay'

describe('formatInferenceStrength', () => {
  it('returns the trimmed raw value when present', () => {
    expect(formatInferenceStrength(' xhigh ')).toBe('xhigh')
    expect(formatInferenceStrength('high')).toBe('high')
  })

  it('falls back to a dash when the value is empty', () => {
    expect(formatInferenceStrength('')).toBe('-')
    expect(formatInferenceStrength('   ')).toBe('-')
    expect(formatInferenceStrength(undefined)).toBe('-')
  })
})
