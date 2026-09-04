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
        <el-table-column label="模型分组" min-width="180">
          <template #default="{ row }">
            <template v-if="(row.model_group_ids || []).length === 0">
              <el-tag size="small" type="info">basic</el-tag>
            </template>
            <el-tag
              v-for="gid in row.model_group_ids || []"
              :key="gid"
              size="small"
              class="group-tag"
            >
              {{ groupName(gid) }}
            </el-tag>
          </template>
        </el-table-column>
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
            <el-button size="small" @click="openEdit(row)">编辑</el-button>
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

    <el-dialog v-model="editVisible" title="编辑门户用户" width="480">
      <el-form :model="editForm" label-width="90px">
        <el-form-item label="邮箱">
          <el-input v-model="editForm.email" placeholder="用户邮箱" />
        </el-form-item>
        <el-form-item label="姓名">
          <el-input v-model="editForm.display_name" placeholder="可留空" />
        </el-form-item>
        <el-form-item label="用户名">
          <el-input v-model="editForm.username" placeholder="可留空" />
        </el-form-item>
        <el-form-item label="身份（UUID）">
          <el-input :model-value="editSubject" disabled placeholder="—" />
          <div class="field-hint">身份标识不可编辑</div>
        </el-form-item>
        <el-form-item label="模型分组">
          <el-select
            v-model="editForm.model_group_ids"
            multiple
            filterable
            style="width: 100%"
            placeholder="选择该用户可用的模型分组"
          >
            <el-option v-for="g in allModelGroups" :key="g.id" :label="groupLabel(g)" :value="g.id" :disabled="g.id === 'basic'" />
          </el-select>
          <div class="field-hint">basic 分组恒可用，不可撤销；用户可在此范围内为自己的密钥切换分组。</div>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="editVisible = false">取消</el-button>
        <el-button type="primary" :loading="editSaving" @click="saveEdit">保存</el-button>
      </template>
    </el-dialog>

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
import type { ModelGroup } from '@/api/portal'

interface PortalUserRow {
  id: string
  email: string
  display_name: string | null
  username: string | null
  disabled: boolean
  last_login_at: number | null
  subject: string | null
  binding_count: number
  model_group_ids?: string[]
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
const allModelGroups = ref<ModelGroup[]>([])

const groupName = (gid: string) => {
  const group = allModelGroups.value.find(g => g.id === gid)
  return group ? `${group.name} (${group.id})` : gid
}

const groupLabel = (group: ModelGroup) => {
  return group.id === 'basic' ? `${group.name}（${group.id}，恒可用）` : `${group.name} (${group.id})`
}

const loadModelGroups = async () => {
  try {
    const response = await adminApi.listModelGroups()
    allModelGroups.value = response.data.groups ?? []
  } catch {
    allModelGroups.value = []
  }
}

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

const editVisible = ref(false)
const editSaving = ref(false)
const editSubject = ref('')
const editForm = ref<{ email: string; display_name: string; username: string; model_group_ids: string[] }>({
  email: '',
  display_name: '',
  username: '',
  model_group_ids: ['basic']
})

const openEdit = async (row: PortalUserRow) => {
  currentlyEditingId.value = row.id
  editSubject.value = row.subject ?? '—'
  editForm.value = {
    email: row.email,
    display_name: row.display_name ?? '',
    username: row.username ?? '',
    model_group_ids: ['basic']
  }
  if (row.model_group_ids && row.model_group_ids.length > 0) {
    editForm.value.model_group_ids = Array.from(new Set(['basic', ...row.model_group_ids]))
  }
  editVisible.value = true
  try {
    const response = await adminApi.getPortalUserModelGroups(row.id)
    const ids = response.data.model_group_ids ?? []
    editForm.value.model_group_ids = Array.from(new Set(ids.length ? ids : ['basic']))
  } catch {
    // 保留列表里的分组信息，加载失败不阻塞编辑
  }
}

const saveEdit = async () => {
  if (!editForm.value.email.trim()) {
    ElMessage.warning('邮箱不能为空')
    return
  }
  editSaving.value = true
  try {
    await adminApi.updatePortalUser(currentlyEditingId.value, {
      email: editForm.value.email.trim(),
      display_name: editForm.value.display_name.trim(),
      username: editForm.value.username.trim()
    })
    if (currentlyEditingId.value) {
      await adminApi.setPortalUserModelGroups(
        currentlyEditingId.value,
        Array.from(new Set(editForm.value.model_group_ids))
      )
    }
    ElMessage.success('已保存')
    editVisible.value = false
    load()
  } finally {
    editSaving.value = false
  }
}

const currentlyEditingId = ref('')

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

onMounted(() => {
  load()
  loadModelGroups()
})
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
.group-tag {
  margin-right: 4px;
}
.field-hint {
  font-size: 12px;
  color: var(--el-text-color-placeholder);
  line-height: 1.5;
}
</style>
