// @vitest-environment happy-dom
import { beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { usePortalStore } from './portal'
import type { PortalKey } from '@/api/portal'

const key = (overrides: Partial<PortalKey>): PortalKey => ({
  downstream_id: 'sk-x',
  plaintext_key: 'sk-secret',
  label: 'Key',
  model_group_id: 'basic',
  created_at: 1700000000,
  usage_count: 0,
  is_default: false,
  ...overrides
})

describe('portal store selection', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('auto-primes to the inherit key before the server default key', () => {
    const store = usePortalStore()
    const keys = [
      key({ downstream_id: 'sk-default', is_default: true }),
      key({ downstream_id: 'sk-inherit', model_access: { mode: 'inherit' } })
    ]
    store.primeSelection(keys)
    expect(store.selectedDownstreamId).toBe('sk-inherit')
    expect(store.explicitSelection).toBe(false)
  })

  it('falls back to the server default key when no inherit key exists', () => {
    const store = usePortalStore()
    store.primeSelection([
      key({ downstream_id: 'sk-a' }),
      key({ downstream_id: 'sk-default', is_default: true })
    ])
    expect(store.selectedDownstreamId).toBe('sk-default')
  })

  it('keeps the explicit selection and never overrides it with defaults', () => {
    const store = usePortalStore()
    store.selectKey('sk-a')
    store.primeSelection([
      key({ downstream_id: 'sk-a' }),
      key({ downstream_id: 'sk-default', is_default: true }),
      key({ downstream_id: 'sk-inherit', model_access: { mode: 'inherit' } })
    ])
    expect(store.selectedDownstreamId).toBe('sk-a')
    expect(store.explicitSelection).toBe(true)
  })

  it('clears a stale explicit selection when the key disappears', () => {
    const store = usePortalStore()
    store.selectKey('sk-gone')
    store.primeSelection([key({ downstream_id: 'sk-other' })])
    expect(store.selectedDownstreamId).toBeNull()
    expect(store.explicitSelection).toBe(false)
  })

  it('scopes requests only when the selection is explicit', () => {
    const store = usePortalStore()
    expect(store.scopeParams()).toBeUndefined()
    store.selectKey('sk-a')
    expect(store.scopeParams()).toEqual({ downstream_id: 'sk-a' })
  })
})
