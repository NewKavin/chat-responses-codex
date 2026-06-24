<template>
  <div class="portal-layout">
    <el-container>
      <el-header class="portal-header">
        <div class="header-content">
          <h1>自助门户</h1>
          <div class="header-info">
            <span class="employee-id">工号: {{ employeeId }}</span>
            <el-button type="danger" size="small" @click="handleLogout">退出登录</el-button>
          </div>
        </div>
      </el-header>

      <el-main class="portal-main">
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

        <el-tabs v-model="activeTab" @tab-change="handleTabChange">
          <el-tab-pane label="概览" name="overview">
            <Overview />
          </el-tab-pane>
          <el-tab-pane label="模型探测" name="model-probe" lazy>
            <ModelProbe />
          </el-tab-pane>
          <el-tab-pane label="限额详情" name="quota" lazy>
            <QuotaDetails />
          </el-tab-pane>
          <el-tab-pane label="使用历史" name="history" lazy>
            <UsageHistory />
          </el-tab-pane>
          <el-tab-pane label="集成示例" name="integration" lazy>
            <Integration />
          </el-tab-pane>
          <el-tab-pane label="模型操练场" name="playground" lazy>
            <Playground />
          </el-tab-pane>
          <el-tab-pane label="秘钥管理" name="key" lazy>
            <KeyManagement />
          </el-tab-pane>
        </el-tabs>
      </el-main>
    </el-container>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, provide, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import Overview from './Overview.vue'
import ModelProbe from './ModelProbe.vue'
import QuotaDetails from './QuotaDetails.vue'
import UsageHistory from './UsageHistory.vue'
import Integration from './Integration.vue'
import KeyManagement from './KeyManagement.vue'
import Playground from './Playground.vue'
import { portalApi } from '@/api/portal'
import { buildAnnouncementSeenKey, shouldShowAnnouncement } from '@/utils/announcement'
import type { Announcement } from '@/types'

const router = useRouter()
const route = useRoute()
const activeTab = ref('overview')
const employeeId = ref('')
const announcementDialogVisible = ref(false)
const currentAnnouncement = ref<Announcement | null>(null)

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
    // Local storage can be unavailable in privacy-restricted environments.
  }
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

  safeLocalStorageSet(
    buildAnnouncementSeenKey(employeeId.value),
    currentAnnouncement.value.id
  )
  announcementDialogVisible.value = false
  currentAnnouncement.value = null
}

const handleTabChange = (tabName: string) => {
  activeTab.value = tabName
  if (tabName === 'model-probe') {
    if (route.path !== '/portal/model-probe') {
      router.push('/portal/model-probe')
    }
    return
  }

  if (tabName === 'overview' && route.path === '/portal/model-probe') {
    router.push('/portal')
  }
}

const handleLogout = () => {
  try {
    localStorage.removeItem('portal_token')
    localStorage.removeItem('portal_employee_id')
  } catch {
    // Ignore local storage failures during logout.
  }
  announcementDialogVisible.value = false
  currentAnnouncement.value = null
  router.push('/portal/login')
}

onMounted(async () => {
  const id = extractEmployeeId()
  if (id) {
    await loadAnnouncement(id)
  }
})

watch(
  () => route.path,
  path => {
    activeTab.value = path === '/portal/model-probe' ? 'model-probe' : 'overview'
  },
  { immediate: true }
)

// 提供 token 给子组件
provide('portalToken', () => safeLocalStorageGet('portal_token'))
</script>

<style scoped>
.portal-layout {
  display: flex;
  flex-direction: column;
  height: 100vh;
  background: #f5f7fa;
  overflow: hidden;
}

.portal-header {
  background: white;
  border-bottom: 1px solid #e4e7ed;
  display: flex;
  align-items: center;
  padding: 0 20px;
  flex-shrink: 0;
}

.header-content {
  width: 100%;
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.header-content h1 {
  margin: 0;
  font-size: 20px;
  color: #303133;
}

.header-info {
  display: flex;
  gap: 15px;
  align-items: center;
}

.employee-id {
  color: #606266;
  font-size: 14px;
}

.portal-main {
  padding: 0;
  overflow: hidden;
  flex: 1;
  min-height: 0;
}

:deep(.el-container) {
  height: 100%;
  flex: 1;
  overflow: hidden;
}

:deep(.el-main) {
  height: 100%;
  overflow: hidden;
  min-height: 0;
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
  white-space: pre-wrap;
  line-height: 1.75;
  color: #303133;
  font-size: 15px;
}

:deep(.el-tabs) {
  background: white;
  padding: 0 20px;
  height: 100%;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

:deep(.el-tabs__header) {
  flex-shrink: 0;
}

:deep(.el-tabs__content) {
  padding: 0;
  flex: 1;
  overflow-y: auto;
  overflow-x: hidden;
  min-height: 0;
}

:deep(.el-tab-pane) {
  background: #f5f7fa;
  height: 100%;
  overflow-y: auto;
  overflow-x: hidden;
}
</style>
