import { describe, expect, it } from 'vitest'
import router from '../../src/router/index'

describe('router', () => {
  it('registers the admin and portal model probe routes', () => {
    expect(router.getRoutes().some(route => route.path === '/admin/model-probe')).toBe(true)
    expect(router.getRoutes().some(route => route.path === '/portal/model-probe')).toBe(true)
  })

  it('registers troubleshooting routes', () => {
    expect(router.getRoutes().some(route => route.path === '/portal/troubleshooting')).toBe(true)
    expect(router.getRoutes().some(route => route.path === '/admin/troubleshooting')).toBe(true)
  })
})
