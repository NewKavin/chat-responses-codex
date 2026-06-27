<template>
  <div class="portal-shell">
    <el-container class="portal-container">
      <el-aside class="portal-aside" width="220px">
        <div class="portal-brand">
          <div class="portal-brand-title">CRC</div>
          <div class="portal-brand-subtitle">自助门户</div>
        </div>
        <div class="portal-user">
          <span class="portal-employee">工号: {{ employeeId }}</span>
          <el-button type="danger" size="small" plain @click="handleLogout">退出</el-button>
        </div>
        <el-menu
          class="portal-menu"
          :default-active="activeMenu"
          :router="true"
        >
          <el-menu-item index="/portal">概览</el-menu-item>
          <el-menu-item index="/portal/model-probe">模型探测</el-menu-item>
          <el-menu-item index="/portal/history">使用历史</el-menu-item>
          <el-menu-item index="/portal/integration">集成示例</el-menu-item>
          <el-menu-item index="/portal/playground">模型操练场</el-menu-item>
          <el-menu-item index="/portal/key">秘钥管理</el-menu-item>
        </el-menu>
      </el-aside>
      <el-container>
        <el-header class="portal-topbar">
          <span>{{ currentTitle }}</span>
        </el-header>
        <el-main class="portal-main">
          <router-view />
        </el-main>
      </el-container>
    </el-container>

    <el-dialog
      v-model="announcementDialogVisible"
      :title="currentAnnouncement?.title || '系统公告'"
      width="560px"
      :close-on-click-modal="false"
      :close-on-press-escape="false"
    >
      <div v-if="currentAnnouncement" class="announcement-dialog">
        <div class="announcement-badge">
          <el-tag :type="announcementTagType" effect="light">{{ announcementLevelLabel }}</el-tag>
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
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { portalApi } from '@/api/portal'
import { buildAnnouncementSeenKey, shouldShowAnnouncement } from '@/utils/announcement'
import type { Announcement } from '@/types'

const router = useRouter()
const route = useRoute()
const employeeId = ref('')
const announcementDialogVisible = ref(false)
const currentAnnouncement = ref<Announcement | null>(null)

const titleMap: Record<string, string> = {
  '/portal': '概览',
  '/portal/model-probe': '模型探测',
  '/portal/history': '使用历史',
  '/portal/integration': '集成示例',
  '/portal/playground': '模型操练场',
  '/portal/key': '秘钥管理'
}

const activeMenu = computed(() => {
  const path = route.path
  for (const key of Object.keys(titleMap)) {
    if (path === key || path.startsWith(key + '/')) return key
  }
  return '/portal'
})

const currentTitle = computed(() => titleMap[activeMenu.value] || '自助门户')

const safeLocalStorageGet = (key: string) => {
  try { return localStorage.getItem(key) } catch { return null }
}
const safeLocalStorageSet = (key: string, value: string) => {
  try { localStorage.setItem(key, value) } catch { /* ignore */ }
}

const extractEmployeeId = () => {
  const id = safeLocalStorageGet('portal_employee_id')
  if (id) { employeeId.value = id; return id }
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
    if (!shouldShowAnnouncement(announcement, safeLocalStorageGet(buildAnnouncementSeenKey(employee)))) {
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
  safeLocalStorageSet(buildAnnouncementSeenKey(employeeId.value), currentAnnouncement.value.id)
  announcementDialogVisible.value = false
  currentAnnouncement.value = null
}

const handleLogout = () => {
  try {
    localStorage.removeItem('portal_token')
    localStorage.removeItem('portal_employee_id')
  } catch { /* ignore */ }
  announcementDialogVisible.value = false
  currentAnnouncement.value = null
  router.push('/portal/login')
}

onMounted(async () => {
  const id = extractEmployeeId()
  if (id) await loadAnnouncement(id)
})

// 提供 token 给子组件
provide('portalToken', () => safeLocalStorageGet('portal_token'))
</script>

<script lang="ts">
import { provide } from 'vue'
export default { name: 'PortalLayout' }
</script>

<style scoped>
.portal-shell { width: 100%; height: 100vh; }
.portal-container { height: 100%; }
.portal-aside {
  border-right: 1px solid #e5e7eb;
  background: linear-gradient(180deg, #0f172a 0%, #1e293b 100%);
  color: #fff;
  display: flex;
  flex-direction: column;
}
.portal-brand { padding: 20px 16px 8px; }
.portal-brand-title { font-size: 18px; font-weight: 700; color: #f8fafc; }
.portal-brand-subtitle { margin-top: 4px; font-size: 12px; color: #cbd5e1; }
.portal-user {
  display: flex; flex-direction: column; gap: 8px; align-items: flex-start;
  padding: 8px 16px 16px; border-bottom: 1px solid rgba(255,255,255,0.08);
}
.portal-employee { color: #cbd5e1; font-size: 13px; }
.portal-menu {
  border-right: none !important;
  background: transparent !important;
  flex: 1;
  overflow-y: auto;
}
.portal-menu :deep(.el-menu-item) { color: #cbd5e1 !important; }
.portal-menu :deep(.el-menu-item.is-active) {
  color: #0f172a !important; background: #f59e0b !important; font-weight: 600;
}
.portal-topbar {
  display: flex; align-items: center; font-weight: 600; color: #0f172a;
  border-bottom: 1px solid #e5e7eb;
}
.portal-main { background: #f8fafc; overflow-y: auto; }
.announcement-dialog { display: flex; flex-direction: column; gap: 16px; }
.announcement-badge { display: flex; align-items: center; }
.announcement-content { white-space: pre-wrap; line-height: 1.75; color: #303133; font-size: 15px; }
</style>
