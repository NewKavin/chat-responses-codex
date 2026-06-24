<template>
  <div class="app-shell">
    <template v-if="isAdminShell">
      <el-container class="admin-shell">
        <el-aside class="sidebar" width="220px">
          <div class="brand">
            <div class="brand-title">CRC</div>
            <div class="brand-subtitle">Console</div>
          </div>
          <el-menu
            class="sidebar-menu"
            :default-active="activeMenu"
            @select="handleMenuSelect"
          >
            <el-menu-item index="/admin/dashboard">控制台总览</el-menu-item>
            <el-menu-item index="/admin/model-probe">模型探测</el-menu-item>
            <el-menu-item index="/admin/upstreams">上游管理</el-menu-item>
            <el-menu-item index="/admin/downstreams">下游管理</el-menu-item>
            <el-menu-item index="/admin/logs">运行日志</el-menu-item>
            <el-menu-item index="/admin/announcement">公告管理</el-menu-item>
          </el-menu>
        </el-aside>
        <el-container>
          <el-header class="topbar">
            <span>管理后台</span>
          </el-header>
          <el-main class="main-content">
            <router-view />
          </el-main>
        </el-container>
      </el-container>
    </template>
    <template v-else>
      <router-view />
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'

const route = useRoute()
const router = useRouter()

const isAdminShell = computed(() => {
  return route.path.startsWith('/admin') && route.path !== '/admin/login'
})

const activeMenu = computed(() => {
  return route.path
})

const handleMenuSelect = (path: string) => {
  if (route.path !== path) {
    router.push(path)
  }
}
</script>

<style>
#app,
.app-shell {
  width: 100%;
  height: 100vh;
}

.admin-shell {
  height: 100%;
}

.sidebar {
  border-right: 1px solid #e5e7eb;
  background: linear-gradient(180deg, #0f172a 0%, #1e293b 100%);
  color: #fff;
}

.brand {
  padding: 20px 16px 12px;
}

.brand-title {
  font-size: 18px;
  font-weight: 700;
  color: #f8fafc;
}

.brand-subtitle {
  margin-top: 4px;
  font-size: 12px;
  color: #cbd5e1;
}

.sidebar-menu {
  border-right: none !important;
  background: transparent !important;
}

.sidebar-menu .el-menu-item {
  color: #cbd5e1 !important;
}

.sidebar-menu .el-menu-item.is-active {
  color: #0f172a !important;
  background: #f59e0b !important;
  font-weight: 600;
}

.topbar {
  display: flex;
  align-items: center;
  font-weight: 600;
  color: #0f172a;
  border-bottom: 1px solid #e5e7eb;
}

.main-content {
  background: #f8fafc;
}

@media (max-width: 768px) {
  .sidebar {
    width: 180px !important;
  }
}
</style>
