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
      component: () => import('@/views/portal/PortalLogin.vue')
    },
    {
      path: '/portal',
      component: () => import('@/views/portal/Portal.vue'),
      meta: { requiresPortalAuth: true },
      children: [
        { path: '', name: 'PortalOverview', component: () => import('@/views/portal/Overview.vue') },
        { path: 'model-probe', name: 'PortalModelProbe', component: () => import('@/views/portal/ModelProbe.vue') },
        { path: 'history', name: 'PortalHistory', component: () => import('@/views/portal/UsageHistory.vue') },
        { path: 'integration', name: 'PortalIntegration', component: () => import('@/views/portal/Integration.vue') },
        { path: 'playground', name: 'PortalPlayground', component: () => import('@/views/portal/Playground.vue') },
        { path: 'key', name: 'PortalKeyManagement', component: () => import('@/views/portal/KeyManagement.vue') }
      ]
    },
    {
      path: '/admin/login',
      name: 'AdminLogin',
      component: () => import('@/views/admin/Login.vue')
    },
    {
      path: '/admin',
      redirect: '/admin/dashboard'
    },
    {
      path: '/admin/dashboard',
      name: 'AdminDashboard',
      component: () => import('@/views/admin/Dashboard.vue'),
      meta: { requiresAuth: true }
    },
    {
      path: '/admin/model-probe',
      name: 'AdminModelProbe',
      component: () => import('@/views/admin/ModelProbe.vue'),
      meta: { requiresAuth: true }
    },
    {
      path: '/admin/upstreams',
      name: 'AdminUpstreams',
      component: () => import('@/views/admin/Upstreams.vue'),
      meta: { requiresAuth: true }
    },
    {
      path: '/admin/downstreams',
      name: 'AdminDownstreams',
      component: () => import('@/views/admin/Downstreams.vue'),
      meta: { requiresAuth: true }
    },
    {
      path: '/admin/logs',
      name: 'AdminLogs',
      component: () => import('@/views/admin/Logs.vue'),
      meta: { requiresAuth: true }
    },
    {
      path: '/admin/troubleshooting',
      name: 'AdminTroubleshooting',
      component: () => import('@/views/admin/Troubleshooting.vue'),
      meta: { requiresAuth: true }
    },
    {
      path: '/admin/announcement',
      name: 'AdminAnnouncement',
      component: () => import('@/views/admin/Announcement.vue'),
      meta: { requiresAuth: true }
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

export default router
