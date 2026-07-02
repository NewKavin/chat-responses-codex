import { describe, expect, it } from 'vitest'
import { getCopyableKey, hasUsablePlaintextKey, maskPlaintextKey } from '../../src/utils/keyUtils'

describe('key utils', () => {
  it('accepts only non-empty plaintext keys', () => {
    expect(hasUsablePlaintextKey('key-abc')).toBe(true)
    expect(hasUsablePlaintextKey('')).toBe(false)
    expect(hasUsablePlaintextKey('   ')).toBe(false)
    expect(hasUsablePlaintextKey(undefined)).toBe(false)
  })

  it('returns null for unusable keys', () => {
    expect(getCopyableKey('key-abc')).toBe('key-abc')
    expect(getCopyableKey('   key-abc   ')).toBe('key-abc')
    expect(getCopyableKey(null)).toBeNull()
    expect(getCopyableKey('')).toBeNull()
  })

  it('masks long keys and preserves short keys', () => {
    expect(maskPlaintextKey('key-1234567890abcdef')).toBe('key-12...cdef')
    expect(maskPlaintextKey('key-short')).toBe('key-short')
  })
})
