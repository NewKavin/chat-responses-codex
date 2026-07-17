<template>
  <div class="portal-root">
    <AppShell
      :items="portalNavItems"
      :active-path="activeMenu"
      :page-title="currentTitle"
      context-label="自助门户"
      :account-label="accountLabel"
      :collapsed="collapsed"
      :mobile-open="mobileOpen"
      @navigate="handleNavigation"
      @logout="handleLogout"
      @toggle-collapse="toggleCollapsed"
      @update:mobile-open="mobileOpen = $event"
    >
      <router-view />
    </AppShell>

    <el-dialog
      v-model="announcementDialogVisible"
      class="announcement-modal"
      :title="currentAnnouncement?.title || '系统公告'"
      width="min(560px, calc(100vw - 32px))"
      :close-on-click-modal="false"
      :close-on-press-escape="false"
    >
      <div v-if="currentAnnouncement" class="announcement-dialog">
        <div class="announcement-badge">
          <el-tag :type="announcementTagType" effect="light">
            {{ announcementLevelLabel }}
          </el-tag>
        </div>
        <div class="announcement-content">{{ currentAnnouncement.content }}</div>
      </div>
      <template #footer>
        <el-button type="primary" @click="handleAcknowledge">我知道了</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, provide, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  ChatDotRound,
  Connection,
  Cpu,
  House,
  Key,
  TrendCharts
} from '@element-plus/icons-vue'
import AppShell from '@/components/AppShell.vue'
import { portalApi } from '@/api/portal'
import { buildAnnouncementSeenKey, shouldShowAnnouncement } from '@/utils/announcement'
import { resolveActiveNavigationPath } from '@/utils/navigation'
import type { Announcement } from '@/types'
import type { AppNavItem } from '@/types/navigation'

defineOptions({ name: 'PortalLayout' })

const PORTAL_COLLAPSED_KEY = 'portal-sidebar-collapsed'

const router = useRouter()
const route = useRoute()
const employeeId = ref('')
const announcementDialogVisible = ref(false)
const currentAnnouncement = ref<Announcement | null>(null)
const mobileOpen = ref(false)

const readCollapsedPreference = () => {
  if (typeof window === 'undefined') return false
  try {
    return window.localStorage.getItem(PORTAL_COLLAPSED_KEY) === 'true'
  } catch {
    return false
  }
}

const collapsed = ref(readCollapsedPreference())

const portalNavItems: AppNavItem[] = [
  { path: '/portal', label: '概览', icon: House, group: '工作台' },
  { path: '/portal/model-probe', label: '模型探测', icon: Cpu, group: '工作台' },
  { path: '/portal/history', label: '使用历史', icon: TrendCharts, group: '工作台' },
  { path: '/portal/integration', label: '集成示例', icon: Connection, group: '开发工具' },
  { path: '/portal/playground', label: '模型操练场', icon: ChatDotRound, group: '开发工具' },
  { path: '/portal/key', label: '密钥管理', icon: Key, group: '账户' }
]

const titleMap: Record<string, string> = {
  '/portal': '概览',
  '/portal/model-probe': '模型探测',
  '/portal/history': '使用历史',
  '/portal/integration': '集成示例',
  '/portal/playground': '模型操练场',
  '/portal/key': '密钥管理'
}

const activeMenu = computed(() =>
  resolveActiveNavigationPath(route.path, Object.keys(titleMap), '/portal')
)

const currentTitle = computed(() =>
  typeof route.meta.title === 'string'
    ? route.meta.title
    : titleMap[activeMenu.value] || '自助门户'
)

const accountLabel = computed(() =>
  employeeId.value ? `工号 ${employeeId.value}` : '门户用户'
)

const safeLocalStorageGet = (key: string) => {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

const safeLocalStorageSet = (key: string, value: string) => {
  try {
    localStorage.setItem(key, value)
  } catch {
    // The current session remains usable when storage is unavailable.
  }
}

const persistCollapsedPreference = () => {
  safeLocalStorageSet(PORTAL_COLLAPSED_KEY, String(collapsed.value))
}

const toggleCollapsed = () => {
  collapsed.value = !collapsed.value
  persistCollapsedPreference()
}

const handleNavigation = (path: string) => {
  if (route.path !== path) router.push(path)
}

const extractEmployeeId = () => {
  const id = safeLocalStorageGet('portal_employee_id')
  if (id) {
    employeeId.value = id
    return id
  }
  router.push('/portal/login')
  return null
}

const announcementLevelLabel = computed(() => {
  const level = currentAnnouncement.value?.level
  if (level === 'success') return '成功'
  if (level === 'warning') return '警告'
  if (level === 'error') return '错误'
  return '信息'
})

const announcementTagType = computed(() => {
  const level = currentAnnouncement.value?.level
  if (level === 'success') return 'success'
  if (level === 'warning') return 'warning'
  if (level === 'error') return 'danger'
  return 'info'
})

const loadAnnouncement = async (employee: string) => {
  try {
    const { data } = await portalApi.getAnnouncement()
    const announcement = data.announcement
    const seenVersion = safeLocalStorageGet(buildAnnouncementSeenKey(employee))
    if (!shouldShowAnnouncement(announcement, seenVersion)) {
      currentAnnouncement.value = null
      announcementDialogVisible.value = false
      return
    }
    currentAnnouncement.value = announcement
    announcementDialogVisible.value = true
  } catch (error) {
    console.error('加载公告失败', error)
  }
}

const handleAcknowledge = () => {
  if (!currentAnnouncement.value || !employeeId.value) {
    announcementDialogVisible.value = false
    currentAnnouncement.value = null
    return
  }
  safeLocalStorageSet(
    buildAnnouncementSeenKey(employeeId.value),
    currentAnnouncement.value.id
  )
  announcementDialogVisible.value = false
  currentAnnouncement.value = null
}

const handleLogout = () => {
  try {
    localStorage.removeItem('portal_token')
    localStorage.removeItem('portal_employee_id')
  } catch {
    // Continue to the login view even when storage is unavailable.
  }
  announcementDialogVisible.value = false
  currentAnnouncement.value = null
  router.push('/portal/login')
}

onMounted(async () => {
  const id = extractEmployeeId()
  if (id) await loadAnnouncement(id)
})

provide('portalToken', () => safeLocalStorageGet('portal_token'))
</script>

<style scoped>
.portal-root {
  width: 100%;
  min-height: 100vh;
}

.announcement-dialog {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.announcement-badge {
  display: flex;
  align-items: center;
}

.announcement-content {
  color: var(--crc-text);
  font-size: 14px;
  line-height: 1.75;
  overflow-wrap: anywhere;
  white-space: pre-wrap;
}
</style>
