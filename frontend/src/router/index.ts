import { createMemoryHistory, createRouter, createWebHashHistory } from 'vue-router'

const history =
  typeof window === 'undefined' ? createMemoryHistory() : createWebHashHistory()

const router = createRouter({
  history,
  routes: [
    {
      path: '/',
      redirect: '/portal/login'
    },
    {
      path: '/portal/login',
      name: 'PortalLogin',
      component: () => import('@/views/portal/PortalLogin.vue'),
      meta: { title: '门户登录' }
    },
    {
      path: '/portal',
      component: () => import('@/views/portal/Portal.vue'),
      meta: { requiresPortalAuth: true, title: '自助门户' },
      children: [
        { path: '', name: 'PortalOverview', component: () => import('@/views/portal/Overview.vue'), meta: { title: '概览' } },
        { path: 'model-probe', name: 'PortalModelProbe', component: () => import('@/views/portal/ModelProbe.vue'), meta: { title: '模型探测' } },
        { path: 'history', name: 'PortalHistory', component: () => import('@/views/portal/UsageHistory.vue'), meta: { title: '使用历史' } },
        { path: 'integration', name: 'PortalIntegration', component: () => import('@/views/portal/Integration.vue'), meta: { title: '集成示例' } },
        { path: 'playground', name: 'PortalPlayground', component: () => import('@/views/portal/Playground.vue'), meta: { title: '模型操练场' } },
        { path: 'key', name: 'PortalKeyManagement', component: () => import('@/views/portal/KeyManagement.vue'), meta: { title: '密钥管理' } }
      ]
    },
    {
      path: '/admin/login',
      name: 'AdminLogin',
      component: () => import('@/views/admin/Login.vue'),
      meta: { title: '管理员登录' }
    },
    {
      path: '/admin',
      redirect: '/admin/dashboard'
    },
    {
      path: '/admin/dashboard',
      name: 'AdminDashboard',
      component: () => import('@/views/admin/Dashboard.vue'),
      meta: { requiresAuth: true, title: '控制台总览' }
    },
    {
      path: '/admin/model-probe',
      name: 'AdminModelProbe',
      component: () => import('@/views/admin/ModelProbe.vue'),
      meta: { requiresAuth: true, title: '模型探测' }
    },
    {
      path: '/admin/upstreams',
      name: 'AdminUpstreams',
      component: () => import('@/views/admin/Upstreams.vue'),
      meta: { requiresAuth: true, title: '上游管理' }
    },
    {
      path: '/admin/downstreams',
      name: 'AdminDownstreams',
      component: () => import('@/views/admin/Downstreams.vue'),
      meta: { requiresAuth: true, title: '下游管理' }
    },
    {
      path: '/admin/logs',
      name: 'AdminLogs',
      component: () => import('@/views/admin/Logs.vue'),
      meta: { requiresAuth: true, title: '运行日志' }
    },
    {
      path: '/admin/troubleshooting',
      name: 'AdminTroubleshooting',
      component: () => import('@/views/admin/Troubleshooting.vue'),
      meta: { requiresAuth: true, title: '排障中心' }
    },
    {
      path: '/admin/announcement',
      name: 'AdminAnnouncement',
      component: () => import('@/views/admin/Announcement.vue'),
      meta: { requiresAuth: true, title: '公告管理' }
    }
  ]
})

// Navigation guard for admin routes
router.beforeEach((to, _from, next) => {
  if (to.meta.requiresAuth) {
    const token = localStorage.getItem('admin_token')
    if (!token) {
      next('/admin/login')
    } else {
      next()
    }
  } else if (to.meta.requiresPortalAuth) {
    const token = localStorage.getItem('portal_token')
    if (!token) {
      next('/portal/login')
    } else {
      next()
    }
  } else {
    next()
  }
})

router.afterEach(to => {
  if (typeof document === 'undefined') return
  const title = typeof to.meta.title === 'string' ? to.meta.title : ''
  document.title = title ? `${title} - CRC Console` : 'CRC Console'
})

export default router
