import { createRouter, createWebHashHistory } from 'vue-router'

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    {
      path: '/',
      redirect: '/admin/login'
    },
    {
      path: '/admin/login',
      name: 'AdminLogin',
      component: () => import('@/views/admin/Login.vue')
    },
    {
      path: '/admin',
      name: 'Admin',
      component: () => import('@/views/admin/Dashboard.vue'),
      meta: { requiresAuth: true }
    },
    {
      path: '/portal',
      name: 'Portal',
      component: () => import('@/views/portal/Portal.vue')
    }
  ]
})

export default router
