<template>
  <div class="app-root">
    <AppShell
      v-if="isAdminShell"
      :items="adminNavItems"
      :active-path="activeMenu"
      :page-title="pageTitle"
      context-label="管理后台"
      account-label="管理员"
      :collapsed="collapsed"
      :mobile-open="mobileOpen"
      @navigate="handleMenuSelect"
      @logout="handleLogout"
      @toggle-collapse="toggleCollapsed"
      @update:mobile-open="mobileOpen = $event"
    >
      <router-view v-slot="{ Component }">
        <transition name="page" mode="out-in">
          <component :is="Component" />
        </transition>
      </router-view>
    </AppShell>
    <router-view v-else v-slot="{ Component }">
      <transition name="page" mode="out-in">
        <component :is="Component" />
      </transition>
    </router-view>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  Bell,
  Connection,
  Cpu,
  Document,
  Key,
  Odometer,
  Tools
} from '@element-plus/icons-vue'
import AppShell from '@/components/AppShell.vue'
import { useAuthStore } from '@/stores/auth'
import type { AppNavItem } from '@/types/navigation'

const ADMIN_COLLAPSED_KEY = 'admin-sidebar-collapsed'

const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()

const readCollapsedPreference = () => {
  if (typeof window === 'undefined') return false
  try {
    return window.localStorage.getItem(ADMIN_COLLAPSED_KEY) === 'true'
  } catch {
    return false
  }
}

const collapsed = ref(readCollapsedPreference())
const mobileOpen = ref(false)

const adminNavItems: AppNavItem[] = [
  { path: '/admin/dashboard', label: '控制台总览', icon: Odometer, group: '概览' },
  { path: '/admin/model-probe', label: '模型探测', icon: Cpu, group: '概览' },
  { path: '/admin/upstreams', label: '上游管理', icon: Connection, group: '资源管理' },
  { path: '/admin/downstreams', label: '下游管理', icon: Key, group: '资源管理' },
  { path: '/admin/logs', label: '运行日志', icon: Document, group: '运维' },
  { path: '/admin/troubleshooting', label: '排障中心', icon: Tools, group: '运维' },
  { path: '/admin/announcement', label: '公告管理', icon: Bell, group: '运维' }
]

const isAdminShell = computed(() =>
  route.path.startsWith('/admin') && route.path !== '/admin/login'
)

const activeMenu = computed(() => route.path)

const pageTitle = computed(() =>
  typeof route.meta.title === 'string' ? route.meta.title : '管理后台'
)

const persistCollapsedPreference = () => {
  if (typeof window === 'undefined') return
  try {
    window.localStorage.setItem(ADMIN_COLLAPSED_KEY, String(collapsed.value))
  } catch {
    // The shell remains usable when storage is unavailable.
  }
}

const toggleCollapsed = () => {
  collapsed.value = !collapsed.value
  persistCollapsedPreference()
}

const handleMenuSelect = (path: string) => {
  if (route.path !== path) router.push(path)
}

const handleLogout = () => {
  authStore.clearToken()
  router.push('/admin/login')
}
</script>

<style>
.app-root {
  width: 100%;
  min-height: 100vh;
}
</style>
