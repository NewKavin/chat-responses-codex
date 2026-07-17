import { describe, expect, it } from 'vitest'
import {
  applyResolvedTheme,
  normalizeThemeMode,
  readStoredThemeMode,
  resolveThemeMode
} from '../../src/composables/useTheme'

describe('theme state', () => {
  it('accepts supported persisted modes and rejects other values', () => {
    expect(normalizeThemeMode('light')).toBe('light')
    expect(normalizeThemeMode('dark')).toBe('dark')
    expect(normalizeThemeMode('auto')).toBe('auto')
    expect(normalizeThemeMode('midnight')).toBe('auto')
    expect(readStoredThemeMode({ getItem: () => 'dark' })).toBe('dark')
    expect(readStoredThemeMode({ getItem: () => { throw new Error('blocked') } })).toBe('auto')
  })

  it('resolves auto from the current system preference', () => {
    expect(resolveThemeMode('light', true)).toBe('light')
    expect(resolveThemeMode('dark', false)).toBe('dark')
    expect(resolveThemeMode('auto', true)).toBe('dark')
    expect(resolveThemeMode('auto', false)).toBe('light')
  })

  it('updates root class, data attribute, and color scheme together', () => {
    const classes = new Set<string>()
    const root = {
      classList: {
        toggle: (name: string, enabled: boolean) => enabled
          ? classes.add(name)
          : classes.delete(name)
      },
      dataset: {} as Record<string, string>,
      style: {} as Record<string, string>
    }

    applyResolvedTheme(root, 'dark')

    expect(classes.has('dark')).toBe(true)
    expect(root.dataset.theme).toBe('dark')
    expect(root.style.colorScheme).toBe('dark')
  })
})
