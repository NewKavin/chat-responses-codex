<template>
  <div class="dashboard-container">
    <el-card>
      <template #header>
        <div class="header">
          <h2>仪表盘</h2>
          <el-button @click="handleLogout">退出登录</el-button>
        </div>
      </template>
      
      <el-row :gutter="20" v-loading="loading">
        <el-col :span="8">
          <el-card shadow="hover">
            <el-statistic title="上游数量" :value="data.upstreams_count" />
          </el-card>
        </el-col>
        
        <el-col :span="8">
          <el-card shadow="hover">
            <el-statistic title="下游数量" :value="data.downstreams_count" />
          </el-card>
        </el-col>
        
        <el-col :span="8">
          <el-card shadow="hover">
            <el-statistic title="日志数量" :value="data.logs_count" />
          </el-card>
        </el-col>
      </el-row>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { adminApi } from '@/api/admin'
import { useAuthStore } from '@/stores/auth'
import type { DashboardData } from '@/types'

const router = useRouter()
const authStore = useAuthStore()
const loading = ref(false)
const data = ref<DashboardData>({
  upstreams_count: 0,
  downstreams_count: 0,
  logs_count: 0
})

const loadData = async () => {
  try {
    loading.value = true
    const response = await adminApi.getDashboard()
    data.value = response.data
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const handleLogout = () => {
  authStore.clearToken()
  router.push('/admin/login')
}

onMounted(() => {
  loadData()
})
</script>

<style scoped>
.dashboard-container {
  padding: 20px;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.header h2 {
  margin: 0;
}

.el-row {
  margin-top: 20px;
}
</style>
