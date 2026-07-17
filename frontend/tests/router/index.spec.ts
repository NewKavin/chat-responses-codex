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

  it('provides page titles for shell and document context', () => {
    const titles = new Map(router.getRoutes().map(route => [route.name, route.meta.title]))

    expect(titles.get('PortalLogin')).toBe('门户登录')
    expect(titles.get('PortalOverview')).toBe('概览')
    expect(titles.get('PortalPlayground')).toBe('模型操练场')
    expect(titles.get('AdminLogin')).toBe('管理员登录')
    expect(titles.get('AdminDashboard')).toBe('控制台总览')
    expect(titles.get('AdminTroubleshooting')).toBe('排障中心')
  })
})
