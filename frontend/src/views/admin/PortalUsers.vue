<template>
  <div class="container">
    <el-card shadow="never" class="header-card">
      <div class="toolbar">
        <el-input
          v-model="keyword"
          placeholder="按邮箱 / 姓名 / 用户名搜索"
          clearable
          style="width: 260px"
          @keyup.enter="load"
          @clear="load"
        >
          <template #prefix><el-icon><Search /></el-icon></template>
        </el-input>
        <el-button type="primary" @click="load">查询</el-button>
      </div>
    </el-card>

    <el-card shadow="never">
      <el-table :data="users" v-loading="loading" stripe>
        <el-table-column prop="email" label="邮箱" min-width="200" show-overflow-tooltip />
        <el-table-column prop="display_name" label="姓名" min-width="110" />
        <el-table-column prop="username" label="用户名" min-width="110" />
        <el-table-column label="身份" min-width="140">
          <template #default="{ row }">
            <span v-if="row.subject">{{ row.subject }}</span>
            <span v-else class="muted">—</span>
          </template>
        </el-table-column>
        <el-table-column prop="binding_count" label="密钥数" width="80" align="center" />
        <el-table-column label="状态" width="90" align="center">
          <template #default="{ row }">
            <el-tag :type="row.disabled ? 'danger' : 'success'" size="small">
              {{ row.disabled ? '已禁用' : '正常' }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column prop="last_login_at" label="最近登录" width="170">
          <template #default="{ row }">
            {{ row.last_login_at ? formatTime(row.last_login_at) : '—' }}
          </template>
        </el-table-column>
        <el-table-column label="操作" width="220" fixed="right">
          <template #default="{ row }">
            <el-button size="small" @click="openBindings(row)">绑定</el-button>
            <el-button
              size="small"
              :type="row.disabled ? 'success' : 'danger'"
              plain
              @click="toggleDisabled(row)"
            >
              {{ row.disabled ? '启用' : '禁用' }}
            </el-button>
          </template>
        </el-table-column>
      </el-table>

      <div class="pager">
        <el-pagination
          layout="total, prev, pager, next"
          :total="total"
          :page-size="pageSize"
          :current-page="page"
          @current-change="onPageChange"
        />
      </div>
    </el-card>

    <el-dialog v-model="bindingsVisible" :title="`密钥绑定：${bindingsUser?.email ?? ''}`" width="560">
      <el-table :data="bindings" v-loading="bindingsLoading" stripe>
        <el-table-column prop="downstream_id" label="密钥" />
        <el-table-column label="默认" width="90" align="center">
          <template #default="{ row }">
            <el-tag v-if="row.is_default" type="primary" size="small">默认</el-tag>
            <span v-else class="muted">—</span>
          </template>
        </el-table-column>
        <el-table-column label="操作" width="90" align="center">
          <template #default="{ row }">
            <el-button size="small" type="danger" plain @click="removeBinding(row)">解绑</el-button>
          </template>
        </el-table-column>
      </el-table>

      <div class="binding-form">
        <el-select v-model="newBindingKey" placeholder="选择已存在的密钥" style="width: 240px" filterable>
          <el-option
            v-for="key in availableKeys"
            :key="key.id"
            :label="`${key.name} (${key.id})`"
            :value="key.id"
          />
        </el-select>
        <el-checkbox v-model="newBindingDefault">设为默认</el-checkbox>
        <el-button type="primary" :loading="bindingSaving" @click="addBinding">添加</el-button>
      </div>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { Search } from '@lucide/vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { adminApi } from '@/api/admin'

interface PortalUserRow {
  id: string
  email: string
  display_name: string | null
  username: string | null
  disabled: boolean
  last_login_at: number | null
  subject: string | null
  binding_count: number
}

interface BindingRow {
  downstream_id: string
  is_default: boolean
}

const users = ref<PortalUserRow[]>([])
const total = ref(0)
const page = ref(1)
const pageSize = 20
const keyword = ref('')
const loading = ref(false)

const bindingsVisible = ref(false)
const bindingsLoading = ref(false)
const bindings = ref<BindingRow[]>([])
const bindingsUser = ref<PortalUserRow | null>(null)
const newBindingKey = ref('')
const newBindingDefault = ref(false)
const bindingSaving = ref(false)
const availableKeys = ref<Array<{ id: string; name: string }>>([])

const formatTime = (unix: number) => {
  return new Date(unix * 1000).toLocaleString()
}

const load = async () => {
  loading.value = true
  try {
    const response = await adminApi.getPortalUsers({
      keyword: keyword.value || undefined,
      page: page.value,
      page_size: pageSize
    })
    users.value = response.data.items
    total.value = response.data.total
  } finally {
    loading.value = false
  }
}

const onPageChange = (next: number) => {
  page.value = next
  load()
}

const toggleDisabled = async (row: PortalUserRow) => {
  const action = row.disabled ? '启用' : '禁用'
  await ElMessageBox.confirm(`确定${action}用户 ${row.email}？禁用会立即注销其全部会话。`, '确认')
  await adminApi.setPortalUserDisabled(row.id, !row.disabled)
  ElMessage.success(`已${action}`)
  load()
}

const openBindings = async (row: PortalUserRow) => {
  bindingsUser.value = row
  bindingsVisible.value = true
  await refreshBindings()
  const downstreams = await adminApi.getDownstreams()
  availableKeys.value = downstreams.data.map((d: { id: string; name: string }) => ({
    id: d.id,
    name: d.name
  }))
}

const refreshBindings = async () => {
  if (!bindingsUser.value) return
  bindingsLoading.value = true
  try {
    const response = await adminApi.getPortalUserBindings(bindingsUser.value.id)
    bindings.value = response.data.items
    newBindingKey.value = ''
    newBindingDefault.value = false
  } finally {
    bindingsLoading.value = false
  }
}

const addBinding = async () => {
  if (!bindingsUser.value || !newBindingKey.value) return
  bindingSaving.value = true
  try {
    await adminApi.addPortalUserBinding(bindingsUser.value.id, newBindingKey.value, newBindingDefault.value)
    ElMessage.success('已添加绑定')
    await refreshBindings()
  } finally {
    bindingSaving.value = false
  }
}

const removeBinding = async (row: BindingRow) => {
  if (!bindingsUser.value) return
  await ElMessageBox.confirm(`解绑密钥 ${row.downstream_id}？`, '确认')
  await adminApi.deletePortalUserBinding(bindingsUser.value.id, row.downstream_id)
  ElMessage.success('已解绑')
  await refreshBindings()
}

onMounted(load)
</script>

<style scoped>
.header-card {
  margin-bottom: 12px;
}
.toolbar {
  display: flex;
  gap: 8px;
}
.pager {
  display: flex;
  justify-content: flex-end;
  margin-top: 12px;
}
.binding-form {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 12px;
}
.muted {
  color: var(--el-text-color-placeholder);
}
</style>
