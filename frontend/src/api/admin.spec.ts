import { describe, expect, it } from 'vitest'
import { createAdminApiClient, hasUsableAdminToken } from './admin'

describe('admin api auth behavior', () => {
  it('treats 401 as failed status', () => {
    const client = createAdminApiClient()
    expect(client.defaults.validateStatus?.(200)).toBe(true)
    expect(client.defaults.validateStatus?.(401)).toBe(false)
  })

  it('only accepts non-empty token strings', () => {
    expect(hasUsableAdminToken('abc')).toBe(true)
    expect(hasUsableAdminToken('')).toBe(false)
    expect(hasUsableAdminToken('   ')).toBe(false)
    expect(hasUsableAdminToken(undefined)).toBe(false)
    expect(hasUsableAdminToken(null)).toBe(false)
  })
})
