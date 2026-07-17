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
})
