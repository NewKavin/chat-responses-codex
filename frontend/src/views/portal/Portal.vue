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
        <el-tabs v-model="activeTab" @tab-change="handleTabChange">
          <el-tab-pane label="概览" name="overview">
            <Overview />
          </el-tab-pane>
          <el-tab-pane label="限额详情" name="quota">
            <QuotaDetails />
          </el-tab-pane>
          <el-tab-pane label="使用历史" name="history">
            <UsageHistory />
          </el-tab-pane>
          <el-tab-pane label="集成示例" name="integration">
            <Integration />
          </el-tab-pane>
          <el-tab-pane label="秘钥管理" name="key">
            <KeyManagement />
          </el-tab-pane>
        </el-tabs>
      </el-main>
    </el-container>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, provide } from 'vue'
import { useRouter } from 'vue-router'
import Overview from './Overview.vue'
import QuotaDetails from './QuotaDetails.vue'
import UsageHistory from './UsageHistory.vue'
import Integration from './Integration.vue'
import KeyManagement from './KeyManagement.vue'

const router = useRouter()
const activeTab = ref('overview')
const employeeId = ref('')

const extractEmployeeId = () => {
  const id = localStorage.getItem('portal_employee_id')
  if (id) {
    employeeId.value = id
  } else {
    router.push('/portal/login')
  }
}

const handleTabChange = (tabName: string) => {
  activeTab.value = tabName
}

const handleLogout = () => {
  localStorage.removeItem('portal_token')
  localStorage.removeItem('portal_employee_id')
  router.push('/portal/login')
}

onMounted(() => {
  extractEmployeeId()
})

// 提供 token 给子组件
provide('portalToken', () => localStorage.getItem('portal_token'))
</script>

<style scoped>
.portal-layout {
  height: 100vh;
  background: #f5f7fa;
}

.portal-header {
  background: white;
  border-bottom: 1px solid #e4e7ed;
  display: flex;
  align-items: center;
  padding: 0 20px;
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
}

:deep(.el-tabs) {
  background: white;
  padding: 0 20px;
}

:deep(.el-tabs__content) {
  padding: 0;
}

:deep(.el-tab-pane) {
  background: #f5f7fa;
}
</style>
