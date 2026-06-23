import { describe, expect, it } from 'vitest'
import { createHighlightedCodeRenderer, registeredHighlightLanguages } from './highlight'

describe('highlight helper', () => {
  it('registers a small explicit language set', () => {
    expect(registeredHighlightLanguages).toEqual([
      'bash',
      'json',
      'javascript',
      'python',
      'typescript'
    ])
  })

  it('renders code blocks with a registered language when available', () => {
    const renderCode = createHighlightedCodeRenderer()

    expect(
      renderCode({
        text: 'const value = 1',
        lang: 'ts'
      })
    ).toContain('language-typescript')
  })

  it('falls back to plaintext for unknown languages', () => {
    const renderCode = createHighlightedCodeRenderer()

    expect(
      renderCode({
        text: 'plain text',
        lang: 'not-a-language'
      })
    ).toContain('language-plaintext')
  })
})
