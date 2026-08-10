import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import router from './index'

describe('admin runtime settings navigation', () => {
  it('registers an authenticated settings route and sidebar entry', () => {
    const route = router.getRoutes().find(candidate => candidate.path === '/admin/settings')
    const appSource = readFileSync(new URL('../App.vue', import.meta.url), 'utf8')

    expect(route?.name).toBe('AdminSettings')
    expect(route?.meta.requiresAuth).toBe(true)
    expect(appSource).toContain("path: '/admin/settings'")
  })
})
