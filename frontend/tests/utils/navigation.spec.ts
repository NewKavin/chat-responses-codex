import { describe, expect, it } from 'vitest'
import { resolveActiveNavigationPath } from '../../src/utils/navigation'

describe('resolveActiveNavigationPath', () => {
  const paths = ['/portal', '/portal/model-probe', '/portal/history']

  it('prefers the most specific matching navigation path', () => {
    expect(resolveActiveNavigationPath('/portal/model-probe', paths, '/portal'))
      .toBe('/portal/model-probe')
    expect(resolveActiveNavigationPath('/portal/model-probe/result', paths, '/portal'))
      .toBe('/portal/model-probe')
  })

  it('falls back when no navigation path matches', () => {
    expect(resolveActiveNavigationPath('/unknown', paths, '/portal')).toBe('/portal')
  })
})
