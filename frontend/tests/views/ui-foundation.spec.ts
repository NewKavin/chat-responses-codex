import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const readSource = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8')

describe('ui foundation composition', () => {
  it('initializes theme before mounting and imports global token layers', () => {
    const main = readSource('../../src/main.ts')

    expect(main).toContain("element-plus/theme-chalk/dark/css-vars.css")
    expect(main).toContain("./styles/tokens.css")
    expect(main).toContain("./styles/base.css")
    expect(main.indexOf('initializeTheme()')).toBeLessThan(main.indexOf('createApp(App)'))
  })

  it('provides responsive shared shell and compact theme control', () => {
    const shell = readSource('../../src/components/AppShell.vue')
    const switcher = readSource('../../src/components/ThemeSwitcher.vue')

    expect(shell).toContain('<el-drawer')
    expect(shell).toContain(':collapse="collapsed"')
    expect(shell).toContain("emit('navigate', path)")
    expect(shell).toContain("emit('update:mobileOpen', false)")
    expect(switcher).toContain("setThemeMode('light')")
    expect(switcher).toContain("setThemeMode('dark')")
    expect(switcher).toContain("setThemeMode('auto')")
    expect(switcher).toContain('跟随系统')
  })
})
