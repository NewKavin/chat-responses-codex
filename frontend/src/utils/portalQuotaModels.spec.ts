import { describe, expect, it } from 'vitest'
import { resolvePortalQuotaModelSlugs } from './portalQuotaModels'

describe('resolvePortalQuotaModelSlugs', () => {
  it('prefers the configured allowlist when it is present', () => {
    expect(
      resolvePortalQuotaModelSlugs(
        ['GLM-5', 'MiniMax/MiniMax-M2.7'],
        ['gpt-4', 'claude-3']
      )
    ).toEqual(['GLM-5', 'MiniMax/MiniMax-M2.7'])
  })

  it('falls back to available models when the allowlist is empty', () => {
    expect(resolvePortalQuotaModelSlugs([], ['gpt-4', 'claude-3'])).toEqual([
      'gpt-4',
      'claude-3'
    ])
  })
})
