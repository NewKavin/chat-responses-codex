import { readFileSync } from 'node:fs'
import { compileStyle, parse } from '@vue/compiler-sfc'
import { describe, expect, it } from 'vitest'

const readSource = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8')

describe('ui foundation composition', () => {
  it('uses a crisp local accent favicon without font or network dependencies', () => {
    const index = readSource('../../index.html')
    const favicon = readSource('../../public/favicon.svg')

    expect(index).toContain('<link rel="icon" type="image/svg+xml" href="/favicon.svg" />')
    expect(favicon).toContain('viewBox="0 0 64 64"')
    expect(favicon).toContain('fill="#0f8f76"')
    expect(favicon).toContain('<path')
    expect(favicon).not.toMatch(/linearGradient|<text|font-family|<script|(?:href|src)="https?:/)
  })

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

  it('emits drawer body styles that survive Element Plus teleport', () => {
    const filename = '../../src/components/AppShell.vue'
    const shell = readSource(filename)
    const style = parse(shell, { filename }).descriptor.styles[0]
    if (!style) throw new Error('AppShell style block is missing')

    const compiled = compileStyle({
      id: 'data-v-shell-test',
      filename,
      source: style.content,
      scoped: style.scoped
    }).code

    expect(compiled).toContain('.console-shell__drawer .el-drawer__body')
    expect(compiled).not.toContain('[data-v-shell-test] .console-shell__drawer')
  })

  it('composes admin navigation through the shared shell', () => {
    const app = readSource('../../src/App.vue')

    expect(app).toContain('<AppShell')
    expect(app).toContain('adminNavItems')
    expect(app).toContain('admin-sidebar-collapsed')
    expect(app).toContain('authStore.clearToken()')
    expect(app).toContain("router.push('/admin/login')")
    expect(app).not.toContain('linear-gradient')
  })

  it('uses shared chrome while retaining portal-owned behavior', () => {
    const portal = readSource('../../src/views/portal/Portal.vue')

    expect(portal).toContain('<AppShell')
    expect(portal).toContain('portalNavItems')
    expect(portal).toContain('portal-sidebar-collapsed')
    expect(portal).toContain('loadAnnouncement')
    expect(portal).toContain("provide('portalToken'")
    expect(portal).toContain("localStorage.removeItem('portal_token')")
    expect(portal).not.toContain('linear-gradient')
  })

  it('uses one neutral authentication surface for both account types', () => {
    const authShell = readSource('../../src/components/AuthShell.vue')
    const adminLogin = readSource('../../src/views/admin/Login.vue')
    const portalLogin = readSource('../../src/views/portal/PortalLogin.vue')

    expect(authShell).toContain('<ThemeSwitcher')
    expect(authShell).toContain('<slot />')
    expect(adminLogin).toContain('<AuthShell')
    expect(portalLogin).toContain('<AuthShell')
    expect(adminLogin).not.toContain('linear-gradient')
    expect(portalLogin).not.toContain('linear-gradient')
  })

  it('keeps typography letter spacing neutral across shared chrome', () => {
    const typographySources = [
      readSource('../../src/styles/base.css'),
      readSource('../../src/components/AppShell.vue'),
      readSource('../../src/components/AuthShell.vue')
    ].join('\n')
    const values = [...typographySources.matchAll(/letter-spacing:\s*([^;]+);/g)]
      .map(match => match[1].trim())

    expect(new Set(values)).toEqual(new Set(['0']))
  })

  it('keeps Element Plus primary controls on the dark theme accent', () => {
    const tokens = readSource('../../src/styles/tokens.css')
    const darkTokens = tokens.match(/html\.dark\s*\{([\s\S]*?)\n\}/)?.[1]

    expect(darkTokens).toContain('--el-color-primary: var(--crc-accent);')
  })

  it('lets business pages fill the shell while account drawers follow viewport proportions', () => {
    const base = readSource('../../src/styles/base.css')
    const pageRule = base.match(/\.crc-page\s*\{([\s\S]*?)\n\}/)?.[1]

    expect(pageRule).toContain('width: 100%;')
    expect(pageRule).toContain('min-width: 0;')
    expect(pageRule).not.toContain('max-width')
    expect(base).toContain('--account-drawer-width: 72vw;')
    expect(base).toContain('--account-drawer-width: 64vw;')
    expect(base).toContain('--account-drawer-width: 86vw;')
    expect(base).toContain('--account-drawer-width: 100vw;')
  })
})
