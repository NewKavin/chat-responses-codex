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

    <el-dialog v-model="bindingsVisible" :title="`密钥与账户：${bindingsUser?.email ?? ''}`" width="940">
      <div class="binding-toolbar" style="display: flex; gap: 8px; align-items: center; margin-bottom: 12px">
        <span class="muted">已选 {{ batchSelection.length }} 个密钥</span>
        <el-select v-model="batchGroup" placeholder="批量改分组" clearable style="width: 150px" filterable>
          <el-option v-for="g in allModelGroups" :key="g.id" :label="g.name" :value="g.id" />
        </el-select>
        <el-button size="small" :disabled="!batchSelection.length || !batchGroup" @click="batchApplyGroup">
          应用分组
        </el-button>
        <el-button size="small" type="success" plain :disabled="!batchSelection.length" @click="batchToggleActive(true)">
          批量启用
        </el-button>
        <el-button size="small" type="warning" plain :disabled="!batchSelection.length" @click="batchToggleActive(false)">
          批量禁用
        </el-button>
        <el-button size="small" type="primary" plain :disabled="!batchSelection.length" @click="openBatchLimits">
          批量改限额
        </el-button>
        <el-button size="small" @click="refreshBindings">刷新</el-button>
      </div>
      <el-table
        :data="bindings"
        v-loading="bindingsLoading"
        stripe
        @selection-change="batchSelection = $event"
      >
        <el-table-column type="selection" width="46" />
        <el-table-column prop="downstream_id" label="密钥" min-width="180" show-overflow-tooltip />
        <el-table-column label="名称" min-width="120" show-overflow-tooltip>
          <template #default="{ row }">
            {{ rowConfig(row)?.name ?? '—' }}
          </template>
        </el-table-column>
        <el-table-column label="状态" width="80" align="center">
          <template #default="{ row }">
            <el-tag v-if="rowConfig(row)?.active !== undefined" :type="rowConfig(row)?.active ? 'success' : 'danger'" size="small">
              {{ rowConfig(row)?.active ? '启用' : '禁用' }}
            </el-tag>
            <span v-else class="muted">—</span>
          </template>
        </el-table-column>
        <el-table-column label="限额概要" min-width="150" show-overflow-tooltip>
          <template #default="{ row }">
            <span class="muted">
              {{ rowConfig(row)?.per_minute_limit ?? '—' }} req/min ·
              {{ rowConfig(row)?.billing_mode ?? '—' }}
            </span>
          </template>
        </el-table-column>
        <el-table-column label="模型分组" min-width="170">
          <template #default="{ row }">
            <el-select
              v-model="row.model_group_id"
              size="small"
              filterable
              style="width: 140px"
              @change="updateBindingGroup(row)"
            >
              <el-option v-for="g in allModelGroups" :key="g.id" :label="g.name" :value="g.id" />
            </el-select>
          </template>
        </el-table-column>
        <el-table-column label="默认" width="90" align="center">
          <template #default="{ row }">
            <el-tag v-if="row.is_default" type="primary" size="small">默认</el-tag>
            <span v-else class="muted">—</span>
          </template>
        </el-table-column>
        <el-table-column label="操作" width="140" align="center">
          <template #default="{ row }">
            <el-button
              size="small"
              :disabled="!rowConfig(row) || rowConfig(row)?.is_portal_key"
              @click="openEditConfig(row)"
            >
              编辑
            </el-button>
            <el-button size="small" type="danger" plain @click="removeBinding(row)">解绑</el-button>
          </template>
        </el-table-column>
      </el-table>

      <div class="binding-form">
        <el-select v-model="newBindingKey" placeholder="选择已存在的密钥" style="width: 220px" filterable>
          <el-option
            v-for="key in availableKeys"
            :key="key.id"
            :label="`${key.name} (${key.id})`"
            :value="key.id"
          />
        </el-select>
        <el-select v-model="newBindingGroup" placeholder="模型分组" clearable style="width: 160px" filterable>
          <el-option v-for="g in allModelGroups" :key="g.id" :label="g.name" :value="g.id" />
        </el-select>
        <el-checkbox v-model="newBindingDefault">默认</el-checkbox>
        <el-button type="primary" :loading="bindingSaving" @click="addBinding">添加</el-button>
      </div>
      <div class="field-hint" style="margin-top: 8px">
        不选模型分组时使用默认分组（basic）；也可对已有绑定重新指定分组（会更新该密钥的分组）。
        门户用户自建的密钥账户配置由系统管理，编辑按钮对其禁用；批量操作作用于当前选中的密钥。
      </div>
    </el-dialog>

    <el-dialog v-model="editConfigVisible" :title="`编辑密钥账户配置：${editConfigKeyId}`" width="480">
      <el-form label-width="130px">
        <el-form-item label="名称">
          <el-input v-model="editConfigForm.name" placeholder="账户名称" />
        </el-form-item>
        <el-form-item label="每分钟限额">
          <el-input-number v-model="editConfigForm.per_minute_limit" :min="1" :step="10" />
        </el-form-item>
        <el-form-item label="最大并发">
          <el-input-number v-model="editConfigForm.max_concurrency" :min="1" />
        </el-form-item>
        <el-form-item label="每日 Token 限额">
          <el-input-number v-model="editConfigForm.daily_token_limit" :min="0" :step="10000" />
        </el-form-item>
        <el-form-item label="计费模式">
          <el-select v-model="editConfigForm.billing_mode">
            <el-option label="按请求计费" value="request" />
            <el-option label="按 Token 计费" value="token" />
          </el-select>
        </el-form-item>
        <el-form-item label="过期时间">
          <el-date-picker v-model="editConfigForm.expires_at" type="datetime" value-format="x" style="width: 100%" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="editConfigVisible = false">取消</el-button>
        <el-button type="primary" :loading="editConfigSaving" @click="saveEditConfig">保存</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="batchLimitsVisible" title="批量修改密钥限额" width="480">
      <el-form label-width="130px">
        <el-form-item label="每分钟限额">
          <el-input-number v-model="batchLimitsForm.per_minute_limit" :min="1" :step="10" />
        </el-form-item>
        <el-form-item label="最大并发">
          <el-input-number v-model="batchLimitsForm.max_concurrency" :min="1" />
        </el-form-item>
        <el-form-item label="每日 Token 限额">
          <el-input-number v-model="batchLimitsForm.daily_token_limit" :min="0" :step="10000" />
        </el-form-item>
        <el-form-item label="计费模式">
          <el-select v-model="batchLimitsForm.billing_mode">
            <el-option label="按请求计费" value="request" />
            <el-option label="按 Token 计费" value="token" />
          </el-select>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="batchLimitsVisible = false">取消</el-button>
        <el-button type="primary" :loading="batchLimitsSaving" @click="saveBatchLimits">保存</el-button>
      </template>
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
  model_group_id?: string
}

interface AccountConfig {
  id: string
  name: string
  active?: boolean
  is_portal_key?: boolean
  per_minute_limit?: number
  max_concurrency?: number
  daily_token_limit?: number | null
  expires_at?: number | null
  billing_mode?: string
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
const newBindingGroup = ref('')
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
  accountConfigs.value = {}
  for (const d of downstreams.data as unknown as Array<Record<string, unknown>>) {
    const id = String(d.id ?? '')
    if (!id) continue
    accountConfigs.value[id] = {
      id,
      name: typeof d.name === 'string' && d.name.trim() ? d.name : id,
      active: typeof d.active === 'boolean' ? d.active : undefined,
      is_portal_key: Boolean(d.is_portal_key),
      per_minute_limit: typeof d.per_minute_limit === 'number' ? d.per_minute_limit : undefined,
      max_concurrency: typeof d.max_concurrency === 'number' ? d.max_concurrency : undefined,
      daily_token_limit: typeof d.daily_token_limit === 'number' ? d.daily_token_limit : undefined,
      expires_at: typeof d.expires_at === 'number' ? d.expires_at : undefined,
      billing_mode: typeof d.billing_mode === 'string' ? d.billing_mode : undefined
    }
  }
}

const refreshBindings = async () => {
  if (!bindingsUser.value) return
  bindingsLoading.value = true
  try {
    const response = await adminApi.getPortalUserBindings(bindingsUser.value.id)
    bindings.value = response.data.items
    newBindingKey.value = ''
    newBindingGroup.value = ''
    newBindingDefault.value = false
  } finally {
    bindingsLoading.value = false
  }
}

const addBinding = async () => {
  if (!bindingsUser.value || !newBindingKey.value) return
  bindingSaving.value = true
  try {
    await adminApi.addPortalUserBinding(
      bindingsUser.value.id,
      newBindingKey.value,
      newBindingDefault.value,
      newBindingGroup.value || undefined
    )
    ElMessage.success('已添加绑定')
    await refreshBindings()
  } finally {
    bindingSaving.value = false
  }
}

const updateBindingGroup = async (row: BindingRow) => {
  if (!bindingsUser.value) return
  try {
    await adminApi.updatePortalUserBinding(bindingsUser.value.id, row.downstream_id, {
      model_group_id: row.model_group_id
    })
    ElMessage.success('绑定分组已保存')
    await refreshBindings()
  } catch (error) {
    ElMessage.error((error as any)?.message || '保存失败')
    await refreshBindings()
  }
}

const removeBinding = async (row: BindingRow) => {
  if (!bindingsUser.value) return
  await ElMessageBox.confirm(`解绑密钥 ${row.downstream_id}？`, '确认')
  await adminApi.deletePortalUserBinding(bindingsUser.value.id, row.downstream_id)
  ElMessage.success('已解绑')
  await refreshBindings()
}

// ---- 合并自下游管理：密钥账户配置与批量编辑 ----
const accountConfigs = ref<Record<string, AccountConfig>>({})
const batchSelection = ref<BindingRow[]>([])
const batchGroup = ref('')
const editConfigVisible = ref(false)
const editConfigKeyId = ref('')
const editConfigSaving = ref(false)
const editConfigForm = ref({
  name: '',
  per_minute_limit: 60,
  max_concurrency: 10,
  daily_token_limit: undefined as number | undefined,
  expires_at: undefined as number | undefined,
  billing_mode: 'request'
})
const batchLimitsVisible = ref(false)
const batchLimitsSaving = ref(false)
const batchLimitsForm = ref({
  per_minute_limit: 60,
  max_concurrency: 10,
  daily_token_limit: undefined as number | undefined,
  billing_mode: 'request'
})

const rowConfig = (row: BindingRow) => accountConfigs.value[row.downstream_id]

const openEditConfig = (row: BindingRow) => {
  const config = rowConfig(row)
  if (!config || config.is_portal_key) return
  editConfigKeyId.value = row.downstream_id
  editConfigForm.value = {
    name: config.name ?? row.downstream_id,
    per_minute_limit: config.per_minute_limit ?? 60,
    max_concurrency: config.max_concurrency ?? 10,
    daily_token_limit: config.daily_token_limit ?? undefined,
    expires_at: config.expires_at ?? undefined,
    billing_mode: config.billing_mode ?? 'request'
  }
  editConfigVisible.value = true
}

const saveEditConfig = async () => {
  editConfigSaving.value = true
  try {
    await adminApi.updateDownstream(editConfigKeyId.value, {
      name: editConfigForm.value.name.trim() || editConfigKeyId.value,
      per_minute_limit: editConfigForm.value.per_minute_limit,
      max_concurrency: editConfigForm.value.max_concurrency,
      daily_token_limit: editConfigForm.value.daily_token_limit,
      expires_at: editConfigForm.value.expires_at,
      billing_mode: editConfigForm.value.billing_mode as 'token' | 'request'
    })
    ElMessage.success('账户配置已保存')
    editConfigVisible.value = false
    await refreshBindings()
  } catch (error) {
    ElMessage.error((error as any)?.message || '保存失败')
  } finally {
    editConfigSaving.value = false
  }
}

const batchKeyIds = () => Array.from(new Set(batchSelection.value.map(r => r.downstream_id)))

const batchApplyGroup = async () => {
  if (!batchGroup.value) return
  const ids = batchKeyIds()
  if (!ids.length) return
  try {
    await adminApi.batchUpdateDownstreams(ids, { model_group_id: batchGroup.value })
    ElMessage.success(`已更新 ${ids.length} 个密钥的分组`)
    batchGroup.value = ''
    await refreshBindings()
  } catch (error) {
    ElMessage.error((error as any)?.message || '批量更新失败')
  }
}

const batchToggleActive = async (active: boolean) => {
  const ids = batchKeyIds()
  if (!ids.length) return
  try {
    await adminApi.batchUpdateDownstreams(ids, { active })
    ElMessage.success(active ? '已批量启用' : '已批量禁用')
    await refreshBindings()
  } catch (error) {
    ElMessage.error((error as any)?.message || '批量更新失败')
  }
}

const openBatchLimits = () => {
  batchLimitsForm.value = {
    per_minute_limit: 60,
    max_concurrency: 10,
    daily_token_limit: undefined,
    billing_mode: 'request'
  }
  batchLimitsVisible.value = true
}

const saveBatchLimits = async () => {
  const ids = batchKeyIds()
  if (!ids.length) return
  batchLimitsSaving.value = true
  try {
    await adminApi.batchUpdateDownstreams(ids, {
      per_minute_limit: batchLimitsForm.value.per_minute_limit,
      max_concurrency: batchLimitsForm.value.max_concurrency,
      daily_token_limit: batchLimitsForm.value.daily_token_limit,
      billing_mode: batchLimitsForm.value.billing_mode as 'token' | 'request'
    })
    ElMessage.success(`已更新 ${ids.length} 个密钥的限额`)
    batchLimitsVisible.value = false
    await refreshBindings()
  } catch (error) {
    ElMessage.error((error as any)?.message || '批量更新失败')
  } finally {
    batchLimitsSaving.value = false
  }
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
