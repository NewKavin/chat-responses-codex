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
        <el-table-column label="名称 / 密钥" min-width="240">
          <template #default="{ row }">
            <div>
              {{ rowConfig(row)?.name ?? '—' }}
              <el-tag v-if="isLegacyKey(row.downstream_id)" size="small" type="info" class="legacy-tag">旧版</el-tag>
            </div>
            <div class="key-id">{{ row.downstream_id }}</div>
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
        密钥可由管理员在此绑定（含 legacy key-xxx 登录密钥），也可由门户用户自行创建后自动出现；
        对已有绑定可重新指定分组（会更新该密钥的分组）。门户自建密钥（sk-）的账户配置由系统管理，
        编辑按钮对其禁用；批量操作作用于当前选中的密钥。
      </div>
    </el-dialog>

    <el-dialog v-model="editConfigVisible" :title="`编辑密钥账户配置：${editConfigKeyName}`" width="680">
      <el-form label-width="150px">
        <el-form-item label="账户名称">
          <el-input v-model="editConfigForm.name" placeholder="账户名称" />
        </el-form-item>
        <el-form-item label="启用">
          <el-switch v-model="editConfigForm.active" />
        </el-form-item>

        <el-divider content-position="left">限速与配额</el-divider>
        <el-form-item label="启用限速">
          <el-switch v-model="editConfigForm.rate_limit_enabled" />
        </el-form-item>
        <template v-if="editConfigForm.rate_limit_enabled">
          <el-form-item label="每分钟请求数">
            <el-input-number v-model="editConfigForm.per_minute_limit" :min="1" :max="10000" />
          </el-form-item>
          <el-form-item label="配额窗口（小时）">
            <el-input-number v-model="editConfigForm.request_quota_window_hours" :min="1" :max="168" />
          </el-form-item>
          <el-form-item label="窗口请求次数">
            <el-input-number v-model="editConfigForm.request_quota_requests" :min="1" :max="1000000" />
          </el-form-item>
          <el-form-item label="最大并发">
            <el-input-number v-model="editConfigForm.max_concurrency" :min="1" />
          </el-form-item>
        </template>

        <el-divider content-position="left">Token 限额</el-divider>
        <el-form-item label="每日 Token 限额">
          <el-input-number v-model="editConfigForm.daily_token_limit" :min="0" :step="10000" />
        </el-form-item>
        <el-form-item label="每月 Token 限额">
          <el-input-number v-model="editConfigForm.monthly_token_limit" :min="0" :step="100000" />
        </el-form-item>

        <el-divider content-position="left">计费</el-divider>
        <el-form-item label="计费模式">
          <el-radio-group v-model="editConfigForm.billing_mode">
            <el-radio-button value="request">按请求</el-radio-button>
            <el-radio-button value="token">按金额（日限额）</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <template v-if="editConfigForm.billing_mode === 'token'">
          <el-form-item label="输入单价（元/M）">
            <el-input-number v-model="editConfigForm.input_token_price_per_million" :min="0.01" :max="1000000" :step="0.1" :precision="2" />
          </el-form-item>
          <el-form-item label="输出单价（元/M）">
            <el-input-number v-model="editConfigForm.output_token_price_per_million" :min="0.01" :max="1000000" :step="0.1" :precision="2" />
          </el-form-item>
          <el-form-item label="每日金额上限（元）">
            <el-input-number v-model="editConfigForm.daily_cost_limit" :min="0.01" :max="100000000" :step="1" :precision="2" />
          </el-form-item>
          <el-form-item>
            <div class="field-hint">消耗 = 输入 T × 输入单价 + 输出 T × 输出单价，滚动 24h 从每日上限扣除。</div>
          </el-form-item>
        </template>

        <el-divider content-position="left">访问控制</el-divider>
        <el-form-item label="IP 白名单">
          <el-input v-model="editConfigForm.ip_allowlist_text" type="textarea" :rows="3" placeholder="每行一个 IP 或 CIDR，留空表示不限制" />
        </el-form-item>
        <el-form-item label="过期时间">
          <el-date-picker v-model="editConfigForm.expires_at" type="datetime" value-format="x" style="width: 100%" />
        </el-form-item>

        <el-divider content-position="left">模型并发组（可选）</el-divider>
        <el-form-item v-for="(group, index) in editConcurrencyGroups" :key="index" :label="`组 ${index + 1}`">
          <div class="cg-row">
            <el-input v-model="group.name" placeholder="组名" style="width: 110px" />
            <el-input v-model="group.matchText" placeholder="模型匹配（逗号/换行分隔，支持 *）" />
            <el-input-number v-model="group.max_concurrency" :min="1" style="width: 110px" />
            <el-button size="small" type="danger" plain @click="removeEditConcurrencyGroup(index)">移除</el-button>
          </div>
        </el-form-item>
        <el-form-item>
          <el-button size="small" @click="addEditConcurrencyGroup">+ 添加并发组</el-button>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="editConfigVisible = false">取消</el-button>
        <el-button type="primary" :loading="editConfigSaving" @click="saveEditConfig">保存</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="batchLimitsVisible" title="批量修改密钥限额" width="620">
      <el-alert type="info" :closable="false" class="helper-text">
        已选 {{ batchSelection.length }} 个密钥。本表字段以当前填写值为准统一写入所选密钥。
      </el-alert>
      <el-form label-width="150px" style="margin-top: 14px">
        <el-form-item label="每分钟请求数">
          <el-input-number v-model="batchLimitsForm.per_minute_limit" :min="1" :max="10000" />
        </el-form-item>
        <el-form-item label="配额窗口（小时）">
          <el-input-number v-model="batchLimitsForm.request_quota_window_hours" :min="1" :max="168" />
        </el-form-item>
        <el-form-item label="窗口请求次数">
          <el-input-number v-model="batchLimitsForm.request_quota_requests" :min="1" :max="1000000" />
        </el-form-item>
        <el-form-item label="最大并发">
          <el-input-number v-model="batchLimitsForm.max_concurrency" :min="1" />
        </el-form-item>
        <el-form-item label="每日 Token 限额">
          <el-input-number v-model="batchLimitsForm.daily_token_limit" :min="0" :step="10000" />
        </el-form-item>
        <el-form-item label="每月 Token 限额">
          <el-input-number v-model="batchLimitsForm.monthly_token_limit" :min="0" :step="100000" />
        </el-form-item>
        <el-form-item label="计费模式">
          <el-radio-group v-model="batchLimitsForm.billing_mode">
            <el-radio-button value="request">按请求</el-radio-button>
            <el-radio-button value="token">按金额（日限额）</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <template v-if="batchLimitsForm.billing_mode === 'token'">
          <el-form-item label="输入单价（元/M）">
            <el-input-number v-model="batchLimitsForm.input_token_price_per_million" :min="0.01" :max="1000000" :step="0.1" :precision="2" />
          </el-form-item>
          <el-form-item label="输出单价（元/M）">
            <el-input-number v-model="batchLimitsForm.output_token_price_per_million" :min="0.01" :max="1000000" :step="0.1" :precision="2" />
          </el-form-item>
          <el-form-item label="每日金额上限（元）">
            <el-input-number v-model="batchLimitsForm.daily_cost_limit" :min="0.01" :max="100000000" :step="1" :precision="2" />
          </el-form-item>
        </template>
      </el-form>
      <template #footer>
        <el-button @click="batchLimitsVisible = false">取消</el-button>
        <el-button type="primary" :loading="batchLimitsSaving" @click="saveBatchLimits">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
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
  rate_limit_enabled?: boolean
  per_minute_limit?: number
  max_concurrency?: number
  request_quota_window_hours?: number
  request_quota_requests?: number
  daily_token_limit?: number | null
  monthly_token_limit?: number | null
  billing_mode?: string
  input_token_price_per_million_cents?: number | null
  output_token_price_per_million_cents?: number | null
  daily_cost_limit_cents?: number | null
  ip_allowlist?: string[]
  expires_at?: number | null
  model_concurrency_groups?: Array<{ name: string; match: string[]; max_concurrency: number }>
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
      rate_limit_enabled: typeof d.rate_limit_enabled === 'boolean' ? d.rate_limit_enabled : undefined,
      per_minute_limit: typeof d.per_minute_limit === 'number' ? d.per_minute_limit : undefined,
      max_concurrency: typeof d.max_concurrency === 'number' ? d.max_concurrency : undefined,
      request_quota_window_hours: typeof d.request_quota_window_hours === 'number' ? d.request_quota_window_hours : undefined,
      request_quota_requests: typeof d.request_quota_requests === 'number' ? d.request_quota_requests : undefined,
      daily_token_limit: typeof d.daily_token_limit === 'number' ? d.daily_token_limit : undefined,
      monthly_token_limit: typeof d.monthly_token_limit === 'number' ? d.monthly_token_limit : undefined,
      billing_mode: typeof d.billing_mode === 'string' ? d.billing_mode : undefined,
      input_token_price_per_million_cents: typeof d.input_token_price_per_million_cents === 'number' ? d.input_token_price_per_million_cents : undefined,
      output_token_price_per_million_cents: typeof d.output_token_price_per_million_cents === 'number' ? d.output_token_price_per_million_cents : undefined,
      daily_cost_limit_cents: typeof d.daily_cost_limit_cents === 'number' ? d.daily_cost_limit_cents : undefined,
      ip_allowlist: Array.isArray(d.ip_allowlist) ? (d.ip_allowlist as string[]) : [],
      expires_at: typeof d.expires_at === 'number' ? d.expires_at : undefined,
      model_concurrency_groups: Array.isArray(d.model_concurrency_groups)
        ? (d.model_concurrency_groups as Array<{ name: string; match: string[]; max_concurrency: number }>)
        : []
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
  active: true,
  rate_limit_enabled: true,
  per_minute_limit: 60,
  max_concurrency: 10,
  request_quota_window_hours: 5,
  request_quota_requests: 600,
  daily_token_limit: undefined as number | undefined,
  monthly_token_limit: undefined as number | undefined,
  billing_mode: 'request' as 'request' | 'token',
  input_token_price_per_million: undefined as number | undefined,
  output_token_price_per_million: undefined as number | undefined,
  daily_cost_limit: undefined as number | undefined,
  ip_allowlist_text: '',
  expires_at: undefined as number | undefined
})
const editConcurrencyGroups = ref<Array<{ name: string; matchText: string; max_concurrency: number }>>([])
const batchLimitsVisible = ref(false)
const batchLimitsSaving = ref(false)
const batchLimitsForm = ref({
  per_minute_limit: 60,
  max_concurrency: 10,
  request_quota_window_hours: 5,
  request_quota_requests: 600,
  daily_token_limit: undefined as number | undefined,
  monthly_token_limit: undefined as number | undefined,
  billing_mode: 'request' as 'request' | 'token',
  input_token_price_per_million: undefined as number | undefined,
  output_token_price_per_million: undefined as number | undefined,
  daily_cost_limit: undefined as number | undefined
})

const isLegacyKey = (id: string) => id.startsWith('key-') || id.startsWith('portal-')

const rowConfigRef = (id: string) => accountConfigs.value[id]
const editConfigKeyName = computed(() => {
  return rowConfigRef(editConfigKeyId.value)?.name ?? editConfigKeyId.value
})

const addEditConcurrencyGroup = () => {
  editConcurrencyGroups.value.push({ name: '', matchText: '', max_concurrency: 4 })
}
const removeEditConcurrencyGroup = (index: number) => {
  editConcurrencyGroups.value.splice(index, 1)
}
const buildEditConcurrencyGroups = ():
  | Array<{ name: string; match: string[]; max_concurrency: number }>
  | null => {
  const groups: Array<{ name: string; match: string[]; max_concurrency: number }> = []
  const seen = new Set<string>()
  for (const group of editConcurrencyGroups.value) {
    const name = group.name.trim()
    if (!name) {
      ElMessage.error('并发组名不能为空')
      return null
    }
    if (seen.has(name)) {
      ElMessage.error(`并发组名重复：${name}`)
      return null
    }
    seen.add(name)
    const match = group.matchText
      .split(/[,，\n]/)
      .map(item => item.trim())
      .filter(Boolean)
    if (!match.length) {
      ElMessage.error(`并发组 ${name} 至少需要一个模型匹配`)
      return null
    }
    groups.push({ name, match, max_concurrency: group.max_concurrency })
  }
  return groups
}

const rowConfig = (row: BindingRow) => accountConfigs.value[row.downstream_id]

const openEditConfig = (row: BindingRow) => {
  const config = rowConfig(row)
  if (!config || config.is_portal_key) return
  editConfigKeyId.value = row.downstream_id
  const isCost = config.billing_mode === 'token'
  editConfigForm.value = {
    name: config.name ?? row.downstream_id,
    active: config.active ?? true,
    rate_limit_enabled: config.rate_limit_enabled ?? true,
    per_minute_limit: config.per_minute_limit ?? 60,
    max_concurrency: config.max_concurrency ?? 10,
    request_quota_window_hours: config.request_quota_window_hours || 5,
    request_quota_requests: config.request_quota_requests || 600,
    daily_token_limit: config.daily_token_limit ?? undefined,
    monthly_token_limit: config.monthly_token_limit ?? undefined,
    billing_mode: (config.billing_mode as 'request' | 'token') ?? 'request',
    input_token_price_per_million:
      isCost && config.input_token_price_per_million_cents
        ? config.input_token_price_per_million_cents / 100
        : undefined,
    output_token_price_per_million:
      isCost && config.output_token_price_per_million_cents
        ? config.output_token_price_per_million_cents / 100
        : undefined,
    daily_cost_limit:
      isCost && config.daily_cost_limit_cents ? config.daily_cost_limit_cents / 100 : undefined,
    ip_allowlist_text: (config.ip_allowlist || []).join('\n'),
    expires_at: config.expires_at ?? undefined
  }
  editConcurrencyGroups.value = (config.model_concurrency_groups || []).map(group => ({
    name: group.name,
    matchText: (group.match || []).join(', '),
    max_concurrency: group.max_concurrency
  }))
  editConfigVisible.value = true
}

const saveEditConfig = async () => {
  if (!editConfigForm.value.name.trim()) {
    ElMessage.warning('请填写账户名称')
    return
  }
  const concurrencyGroups = buildEditConcurrencyGroups()
  if (concurrencyGroups === null) return
  const isCost = editConfigForm.value.billing_mode === 'token'
  editConfigSaving.value = true
  try {
    const payload: Record<string, unknown> = {
      name: editConfigForm.value.name.trim() || editConfigKeyId.value,
      active: editConfigForm.value.active,
      rate_limit_enabled: editConfigForm.value.rate_limit_enabled,
      per_minute_limit: editConfigForm.value.per_minute_limit,
      max_concurrency: editConfigForm.value.max_concurrency,
      request_quota_window_hours: editConfigForm.value.rate_limit_enabled
        ? editConfigForm.value.request_quota_window_hours
        : null,
      request_quota_requests: editConfigForm.value.rate_limit_enabled
        ? editConfigForm.value.request_quota_requests
        : null,
      daily_token_limit: editConfigForm.value.daily_token_limit ?? null,
      monthly_token_limit: editConfigForm.value.monthly_token_limit ?? null,
      billing_mode: isCost ? 'token' : 'request',
      input_token_price_per_million_cents: isCost
        ? Math.round((editConfigForm.value.input_token_price_per_million ?? 0) * 100)
        : null,
      output_token_price_per_million_cents: isCost
        ? Math.round((editConfigForm.value.output_token_price_per_million ?? 0) * 100)
        : null,
      daily_cost_limit_cents: isCost
        ? Math.round((editConfigForm.value.daily_cost_limit ?? 0) * 100)
        : null,
      ip_allowlist: editConfigForm.value.ip_allowlist_text
        .split('\n')
        .map(item => item.trim())
        .filter(Boolean),
      expires_at: editConfigForm.value.expires_at,
      model_concurrency_groups: concurrencyGroups
    }
    await adminApi.updateDownstream(editConfigKeyId.value, payload)
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
    request_quota_window_hours: 5,
    request_quota_requests: 600,
    daily_token_limit: undefined,
    monthly_token_limit: undefined,
    billing_mode: 'request',
    input_token_price_per_million: undefined,
    output_token_price_per_million: undefined,
    daily_cost_limit: undefined
  }
  batchLimitsVisible.value = true
}

const saveBatchLimits = async () => {
  const ids = batchKeyIds()
  if (!ids.length) return
  const isCost = batchLimitsForm.value.billing_mode === 'token'
  batchLimitsSaving.value = true
  try {
    await adminApi.batchUpdateDownstreams(ids, {
      per_minute_limit: batchLimitsForm.value.per_minute_limit,
      max_concurrency: batchLimitsForm.value.max_concurrency,
      request_quota_window_hours: batchLimitsForm.value.request_quota_window_hours,
      request_quota_requests: batchLimitsForm.value.request_quota_requests,
      daily_token_limit: batchLimitsForm.value.daily_token_limit ?? null,
      monthly_token_limit: batchLimitsForm.value.monthly_token_limit ?? null,
      billing_mode: isCost ? 'token' : 'request',
      input_token_price_per_million_cents: isCost
        ? Math.round((batchLimitsForm.value.input_token_price_per_million ?? 0) * 100)
        : null,
      output_token_price_per_million_cents: isCost
        ? Math.round((batchLimitsForm.value.output_token_price_per_million ?? 0) * 100)
        : null,
      daily_cost_limit_cents: isCost
        ? Math.round((batchLimitsForm.value.daily_cost_limit ?? 0) * 100)
        : null
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
.legacy-tag {
  margin-left: 6px;
}
.key-id {
  font-size: 12px;
  color: var(--el-text-color-placeholder);
  word-break: break-all;
}
.cg-row {
  display: flex;
  gap: 8px;
  align-items: center;
  width: 100%;
}
.helper-text {
  margin-top: 8px;
}
.field-hint {
  font-size: 12px;
  color: var(--el-text-color-placeholder);
  line-height: 1.5;
}
</style>
