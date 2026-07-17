import { readonly, ref } from 'vue'

export type ThemeMode = 'light' | 'dark' | 'auto'
export type ResolvedTheme = Exclude<ThemeMode, 'auto'>

export const THEME_STORAGE_KEY = 'crc-theme-mode'

type ThemeRoot = {
  classList: { toggle: (name: string, enabled: boolean) => unknown }
  dataset: { theme?: string }
  style: { colorScheme?: string }
}

const mode = ref<ThemeMode>('auto')
const resolvedTheme = ref<ResolvedTheme>('light')
let mediaQuery: MediaQueryList | null = null
let initialized = false

export const normalizeThemeMode = (value: unknown): ThemeMode =>
  value === 'light' || value === 'dark' || value === 'auto' ? value : 'auto'

export const readStoredThemeMode = (storage: Pick<Storage, 'getItem'>): ThemeMode => {
  try {
    return normalizeThemeMode(storage.getItem(THEME_STORAGE_KEY))
  } catch {
    return 'auto'
  }
}

export const resolveThemeMode = (
  value: ThemeMode,
  prefersDark: boolean
): ResolvedTheme => value === 'auto' ? (prefersDark ? 'dark' : 'light') : value

export const applyResolvedTheme = (root: ThemeRoot, value: ResolvedTheme) => {
  root.classList.toggle('dark', value === 'dark')
  root.dataset.theme = value
  root.style.colorScheme = value
}

const syncTheme = () => {
  const resolved = resolveThemeMode(mode.value, mediaQuery?.matches ?? false)
  resolvedTheme.value = resolved
  if (typeof document !== 'undefined') {
    applyResolvedTheme(document.documentElement, resolved)
  }
}

const handleSystemThemeChange = () => {
  if (mode.value === 'auto') syncTheme()
}

export const initializeTheme = () => {
  if (typeof window === 'undefined' || initialized) return

  initialized = true
  mode.value = readStoredThemeMode(window.localStorage)
  mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
  mediaQuery.addEventListener('change', handleSystemThemeChange)
  syncTheme()
}

export const setThemeMode = (value: ThemeMode) => {
  mode.value = value
  if (typeof window !== 'undefined') {
    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, value)
    } catch {
      // Theme changes still apply when storage is unavailable.
    }
  }
  syncTheme()
}

export const useTheme = () => ({
  mode: readonly(mode),
  resolvedTheme: readonly(resolvedTheme),
  setThemeMode
})
