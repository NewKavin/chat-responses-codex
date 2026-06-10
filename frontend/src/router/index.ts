import { createRouter, createWebHashHistory } from 'vue-router'

const router = createRouter({
  history: createWebHashHistory(),
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
      name: 'Portal',
      component: () => import('@/views/portal/Portal.vue'),
      meta: { requiresPortalAuth: true }
    },
    {
      path: '/portal/integration',
      name: 'PortalIntegration',
      component: () => import('@/views/portal/Integration.vue'),
      meta: { requiresPortalAuth: true }
    },
    {
      path: '/portal/key',
      name: 'PortalKeyManagement',
      component: () => import('@/views/portal/KeyManagement.vue'),
      meta: { requiresPortalAuth: true }
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
