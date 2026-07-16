import { describe, expect, it } from 'vitest'
import router from '../../src/router/index'

describe('router', () => {
  it('registers the admin and portal model probe routes', () => {
    expect(router.getRoutes().some(route => route.path === '/admin/model-probe')).toBe(true)
    expect(router.getRoutes().some(route => route.path === '/portal/model-probe')).toBe(true)
  })

  it('registers only the admin troubleshooting route', () => {
    expect(router.getRoutes().some(route => route.name === 'PortalTroubleshooting')).toBe(false)
    expect(router.getRoutes().some(route => route.name === 'AdminTroubleshooting')).toBe(true)
  })
})
