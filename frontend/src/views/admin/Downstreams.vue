<template>
  <div class="crc-page downstreams-page">
    <header class="crc-page-header">
      <div>
        <p class="crc-eyebrow">IDENTITY // DOWNSTREAMS</p>
        <h1 class="crc-page-title">下游管理</h1>
        <p class="crc-page-description">管理门户身份、可用模型、调用限额、生命周期和访问密钥。</p>
      </div>
      <div style="display: flex; gap: 8px; align-items: center;">
        <el-button :disabled="!selectedRows.length" @click="batchDialogVisible = true">
          <Settings2 :size="15" :stroke-width="2" style="margin-right: 5px" />批量设置计费模式
        </el-button>
        <el-button type="primary" @click="handleCreate">
          <Plus :size="15" :stroke-width="2" style="margin-right: 5px" />创建下游
        </el-button>
      </div>
    </header>

    <el-form :inline="true" class="crc-toolbar downstream-filters">
      <el-form-item>
        <template #label><span class="filter-label"><Activity :size="12" :stroke-width="2" />状态</span></template>
        <el-select v-model="filters.status" @change="loadData" placeholder="全部">
          <el-option label="全部" value="all" />
          <el-option label="启用" value="active" />
          <el-option label="禁用" value="inactive" />
        </el-select>
      </el-form-item>
      <el-form-item>
        <template #label><span class="filter-label"><Clock3 :size="12" :stroke-width="2" />生命周期</span></template>
        <el-select v-model="filters.lifecycle" @change="loadData" placeholder="全部">
          <el-option label="全部" value="all" />
          <el-option label="试用" value="trial" />
          <el-option label="永久" value="permanent" />
        </el-select>
      </el-form-item>
      <el-form-item>
        <template #label><span class="filter-label"><Search :size="12" :stroke-width="2" />搜索</span></template>
        <el-input v-model="filters.search" @input="loadData" placeholder="名称或ID" clearable />
      </el-form-item>
      <el-form-item class="table-column-settings-item">
        <TableColumnSettings
          v-model="visibleColumnKeys"
          :columns="tableColumns"
          :default-keys="defaultColumnKeys"
        />
      </el-form-item>
    </el-form>
      
    <div class="crc-table-shell downstreams-table-shell">
      <el-table class="compact-downstreams-table" :data="downstreams" v-loading="loading" stripe @selection-change="handleSelectionChange">
        <el-table-column type="selection" width="45" />
        <el-table-column v-if="isColumnVisible('id')" prop="id" label="ID" width="150" />
        <el-table-column v-if="isColumnVisible('name')" prop="name" label="名称" width="200" />
        <el-table-column v-if="isColumnVisible('key')" label="秘钥" width="220">
          <template #default="{ row }">
            <div class="key-cell">
              <code v-if="hasUsablePlaintextKey(row.plaintext_key)">
                {{ maskPlaintextKey(row.plaintext_key) }}
              </code>
              <span v-else class="legacy-key-hint">未存储真实秘钥，请先轮换</span>
              <el-tooltip content="复制秘钥" placement="top">
                <el-button
                  class="copy-key-button"
                  aria-label="复制秘钥"
                  circle
                  size="small"
                  @click="copyKey(row.plaintext_key)"
                  :disabled="!hasUsablePlaintextKey(row.plaintext_key)"
                >
                  <Copy :size="13" :stroke-width="1.8" />
                </el-button>
              </el-tooltip>
            </div>
          </template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('quota')" label="限额配置" min-width="320">
          <template #default="{ row }">
            <span v-if="!row.rate_limit_enabled">未启用限额</span>
            <span v-else-if="isCostRow(row)">
              <el-tag type="danger" size="small">金额计费</el-tag>
              余额 {{ formatMoney(Math.max(0, (row.daily_cost_limit_cents ?? 0) - (row.usage?.cost_used_24h_cents ?? 0))) }}
            </span>
            <span v-else>
              <el-tag size="small">按次数</el-tag>
              {{ row.per_minute_limit }}/分钟 · 并发 {{ row.max_concurrency }} · {{ row.request_quota_window_hours || 0 }} 小时 {{ row.request_quota_requests || 0 }} 次
            </span>
          </template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('runtime')" label="运行并发" width="400">
          <template #default="{ row }">
            <div class="runtime-cell" v-if="runtimeById[row.id]?.available">
              <span class="runtime-metric" v-if="runtimeById[row.id]?.running !== undefined">
                <Activity :size="12" :stroke-width="1.8" />运行中 {{ runtimeById[row.id]?.running }}
              </span>
              <span class="runtime-metric" v-if="runtimeById[row.id]?.waiting_upstream !== undefined">
                <Clock3 :size="12" :stroke-width="1.8" />等待上游 {{ runtimeById[row.id]?.waiting_upstream }}
              </span>
              <span class="runtime-metric" v-if="runtimeById[row.id]?.admitted !== undefined">
                <Gauge :size="12" :stroke-width="1.8" />已占用 {{ runtimeById[row.id]?.admitted }}
              </span>
              <span class="runtime-metric">
                <ShieldCheck :size="12" :stroke-width="1.8" />上限 {{ runtimeById[row.id]?.limit }}
              </span>
            </div>
            <el-tag v-else-if="runtimeById[row.id]?.available === false" type="info" size="small">Unavailable</el-tag>
            <el-tag v-else type="info" size="small">Unavailable</el-tag>
          </template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('lifecycle')" label="生命周期" width="120">
          <template #default="{ row }">
            <el-tag :type="row.expires_at ? 'warning' : 'success'">
              {{ row.expires_at ? '试用' : '永久' }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('status')" label="状态" width="100">
          <template #default="{ row }">
            <el-tag :type="row.active ? 'success' : 'danger'">
              {{ row.active ? '启用' : '禁用' }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column label="操作" width="300">
          <template #default="{ row }">
            <div class="row-actions">
              <el-button size="small" @click="handleEdit(row)">编辑</el-button>
              <el-button size="small" @click="handleToggle(row)">
                {{ row.active ? '禁用' : '启用' }}
              </el-button>
              <el-button size="small" type="warning" @click="handleRotate(row)">轮换密钥</el-button>
              <el-button size="small" type="danger" @click="handleDelete(row)">删除</el-button>
            </div>
          </template>
        </el-table-column>
      </el-table>
    </div>

    <el-alert
      title="重要提示"
      type="warning"
      :closable="false"
      class="helper-text"
    >
      仅可复制真实可用秘钥。若某行显示“未存储真实秘钥”，请先执行“轮换密钥”生成新秘钥后再复制。
    </el-alert>
    
    <!-- Create/Edit Drawer -->
    <el-drawer
      v-model="dialogVisible"
      :title="dialogMode === 'create' ? '创建下游' : '编辑下游'"
      direction="rtl"
      size="var(--account-drawer-width)"
      :destroy-on-close="false"
      class="form-drawer downstream-account-drawer"
    >
      <el-form ref="formRef" :model="form" :rules="rules" label-position="top" class="drawer-form">
        <el-form-item label="ID" prop="id">
          <el-input 
            v-model="form.id" 
            :disabled="dialogMode === 'edit'"
            :placeholder="dialogMode === 'create' ? '请输入下游ID（必填，用于门户登录）' : ''"
          />
          <el-alert
            v-if="dialogMode === 'create'"
            title="说明"
            type="info"
            :closable="false"
            class="helper-text"
          >
            下游ID必须手动填写，用于门户登录时的工号。建议使用有意义标识（如 team-a）。
          </el-alert>
        </el-form-item>
        <el-form-item label="名称" prop="name">
          <el-input v-model="form.name" placeholder="例如: 研发团队 A" />
        </el-form-item>
        <el-form-item label="限额开关">
          <el-switch v-model="form.rate_limit_enabled" />
          <el-alert
            title="说明"
            type="info"
            :closable="false"
            class="helper-text"
          >
            默认开启。关闭后，每分钟限制、并发限制、时间窗口次数限制都不生效。
          </el-alert>
        </el-form-item>

        <template v-if="form.rate_limit_enabled">
          <el-divider class="drawer-section">限额配置</el-divider>
          <el-form-item label="并发限制">
            <el-input-number v-model="form.max_concurrency" :min="1" :max="5000" />
            <el-alert
              title="说明"
              type="info"
              :closable="false"
              class="helper-text"
            >
              所有计费模式下生效：同一时刻允许并发处理中的请求数上限。
            </el-alert>
          </el-form-item>
          <el-form-item label="计费模式">
            <el-radio-group v-model="form.billing_mode">
              <el-radio-button value="request">按次数</el-radio-button>
              <el-radio-button value="cost">按金额（日限额）</el-radio-button>
            </el-radio-group>
            <el-alert
              title="说明"
              type="info"
              :closable="false"
              class="helper-text"
            >
              按次数：时间窗口内请求次数限额；按金额：输入/输出 token 按单价折算，从日金额上限中扣除。
            </el-alert>
          </el-form-item>
          <template v-if="form.billing_mode === 'cost'">
            <el-form-item>
              <template #label><span class="filter-label"><Coins :size="13" :stroke-width="2" />金额计费（元）</span></template>
              <div class="price-row">
                <div class="price-field">
                  <span class="price-label"><ArrowDownToLine :size="13" :stroke-width="2" />IN 单价/M</span>
                  <el-input-number v-model="inputTokenPricePerMillion" :min="0.01" :max="1000000" :step="0.1" :precision="2" style="width: 100%" />
                </div>
                <div class="price-field">
                  <span class="price-label"><ArrowUpFromLine :size="13" :stroke-width="2" />OUT 单价/M</span>
                  <el-input-number v-model="outputTokenPricePerMillion" :min="0.01" :max="1000000" :step="0.1" :precision="2" style="width: 100%" />
                </div>
                <div class="price-field">
                  <span class="price-label"><Wallet :size="13" :stroke-width="2" />日上限</span>
                  <el-input-number v-model="dailyCostLimit" :min="0.01" :max="100000000" :step="1" :precision="2" style="width: 100%" />
                </div>
              </div>
              <el-alert
                title="说明"
                type="info"
                :closable="false"
                class="helper-text"
              >
                消耗 = 输入 T × IN 单价 + 输出 T × OUT 单价，滚动 24h 从日上限扣除；只填一个单价时另一方向按 0 计。
              </el-alert>
            </el-form-item>
          </template>
          <template v-else>
            <el-form-item label="每分钟限制" prop="per_minute_limit">
              <el-input-number v-model="form.per_minute_limit" :min="1" :max="10000" />
            </el-form-item>
            <el-form-item label="时间窗口（小时）">
              <el-input-number v-model="requestQuotaHours" :min="1" :max="168" />
            </el-form-item>
            <el-form-item label="窗口请求次数">
              <el-input-number v-model="requestQuotaCount" :min="1" :max="1000000" />
            </el-form-item>
          </template>
        </template>

        <el-form-item label="模型白名单">
          <el-select v-model="form.model_allowlist" multiple filterable allow-create placeholder="留空表示允许所有模型">
            <el-option v-for="model in availableModels" :key="model" :label="model" :value="model" />
          </el-select>
          <el-alert
            title="说明"
            type="info"
            :closable="false"
            class="helper-text"
          >
            默认不限制模型，留空即可全部访问。
          </el-alert>
        </el-form-item>
        <el-form-item label="IP 白名单">
          <el-input v-model="ipAllowlistText" type="textarea" :rows="3" placeholder="每行一个 IP 或 CIDR&#10;例如: 10.0.0.1&#10;192.168.1.0/24" />
          <el-alert
            title="说明"
            type="info"
            :closable="false"
            class="helper-text"
          >
            留空表示允许所有 IP（默认不限制）。支持 CIDR 格式。
          </el-alert>
        </el-form-item>
        <el-form-item label="启用">
          <el-switch v-model="form.active" />
        </el-form-item>
      </el-form>
      
      <template #footer>
        <div class="drawer-footer">
          <el-button @click="dialogVisible = false">取消</el-button>
          <el-button type="primary" @click="handleSubmit" :loading="submitting">确定</el-button>
        </div>
      </template>
    </el-drawer>
    
    <!-- Batch Billing Mode Dialog -->
    <el-dialog
      v-model="batchDialogVisible"
      title="批量设置计费模式"
      width="min(520px, calc(100vw - 32px))"
    >
      <el-alert type="info" :closable="false" class="helper-text">
        已选 {{ selectedRows.length }} 个下游。填写数值的字段会统一设置，留空表示不修改。
      </el-alert>
      <el-form label-position="top" style="margin-top: 16px">
        <el-form-item label="计费模式">
          <el-radio-group v-model="batchForm.billing_mode">
            <el-radio-button value="request">按次数</el-radio-button>
            <el-radio-button value="cost">按金额（日限额）</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <template v-if="batchForm.billing_mode === 'cost'">
          <el-form-item>
            <template #label><span class="filter-label"><Coins :size="13" :stroke-width="2" />金额计费（元）</span></template>
            <div class="price-row">
              <div class="price-field">
                <span class="price-label"><ArrowDownToLine :size="13" :stroke-width="2" />IN 单价/M</span>
                <el-input-number v-model="batchForm.input_token_price_per_million_cents" :min="0.01" :max="1000000" :step="0.1" :precision="2" style="width: 100%" />
              </div>
              <div class="price-field">
                <span class="price-label"><ArrowUpFromLine :size="13" :stroke-width="2" />OUT 单价/M</span>
                <el-input-number v-model="batchForm.output_token_price_per_million_cents" :min="0.01" :max="1000000" :step="0.1" :precision="2" style="width: 100%" />
              </div>
              <div class="price-field">
                <span class="price-label"><Wallet :size="13" :stroke-width="2" />日上限</span>
                <el-input-number v-model="batchForm.daily_cost_limit_cents" :min="0.01" :max="100000000" :step="1" :precision="2" style="width: 100%" />
              </div>
            </div>
            <span class="form-hint">填写数值则统一设置；留空表示不修改。</span>
          </el-form-item>
        </template>
      </el-form>
      <template #footer>
        <el-button @click="batchDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="batchSubmitting" @click="submitBatchMode">确定</el-button>
      </template>
    </el-dialog>

    <!-- Rotate Key Dialog -->
    <el-dialog
      v-model="rotateDialogVisible"
      class="rotate-key-dialog"
      title="密钥轮换成功"
      width="min(500px, calc(100vw - 32px))"
    >
      <el-alert type="success" :closable="false" show-icon>
        <template #title>
          新密钥已生成，请妥善保存！此密钥只显示一次。
        </template>
      </el-alert>
      <div class="key-result-surface">
        <div class="key-result-heading">
          <span>新访问密钥</span>
          <el-tooltip content="复制新密钥" placement="top">
            <el-button
              aria-label="复制新密钥"
              circle
              type="primary"
              @click="copyKey(newPlaintextKey)"
            >
              <Copy :size="14" :stroke-width="1.8" />
            </el-button>
          </el-tooltip>
        </div>
        <code class="new-key-value">{{ newPlaintextKey }}</code>
      </div>
      <el-alert
        title="重要提示"
        type="warning"
        :closable="false"
        class="helper-text"
      >
        这是真正的秘钥，可用于门户登录。请立即复制并妥善保存，关闭后无法再次查看。
      </el-alert>
      <template #footer>
        <el-button type="primary" @click="rotateDialogVisible = false">我已保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import {
  Activity,
  ArrowDownToLine,
  ArrowUpFromLine,
  Clock3,
  Coins,
  Copy,
  Gauge,
  Plus,
  Search,
  Settings2,
  ShieldCheck,
  Wallet
} from '@lucide/vue'
import { adminApi } from '@/api/admin'
import type { DownstreamConfig, DownstreamConcurrencySnapshot } from '@/types'
import { useTableColumnPreferences, type TableColumnDefinition } from '@/composables/useTableColumns'
import { getCopyableKey, hasUsablePlaintextKey, maskPlaintextKey } from '@/utils/keyUtils'

const loading = ref(false)
const downstreams = ref<DownstreamConfig[]>([])
const dialogVisible = ref(false)
const rotateDialogVisible = ref(false)
const dialogMode = ref<'create' | 'edit'>('create')
const submitting = ref(false)
const formRef = ref()
const newPlaintextKey = ref('')
const runtimeById = ref<Record<string, DownstreamConcurrencySnapshot>>({})
let runtimeTimer: number | null = null
const tableColumns: TableColumnDefinition[] = [
  { key: 'id', label: 'ID' },
  { key: 'name', label: '名称' },
  { key: 'key', label: '秘钥' },
  { key: 'quota', label: '限额配置' },
  { key: 'runtime', label: '运行并发' },
  { key: 'lifecycle', label: '生命周期' },
  { key: 'status', label: '状态' }
]
const defaultColumnKeys = tableColumns.map(column => column.key)
const { visibleColumnKeys, isColumnVisible } = useTableColumnPreferences(
  tableColumns,
  'admin-downstreams-visible-columns',
  defaultColumnKeys
)

const requestQuotaHours = ref(5)
const requestQuotaCount = ref(600)
// 按金额计费：输入单位为元，提交时换算成分（¢）。
const inputTokenPricePerMillion = ref<number | undefined>(undefined)
const outputTokenPricePerMillion = ref<number | undefined>(undefined)
const dailyCostLimit = ref<number | undefined>(undefined)
const availableModels = ref<string[]>([])
const selectedRows = ref<DownstreamConfig[]>([])
const batchDialogVisible = ref(false)
const batchSubmitting = ref(false)
const batchForm = ref({
  billing_mode: 'cost' as 'request' | 'cost',
  input_token_price_per_million_cents: undefined as number | undefined,
  output_token_price_per_million_cents: undefined as number | undefined,
  daily_cost_limit_cents: undefined as number | undefined
})

const filters = ref({
  status: 'all',
  lifecycle: 'all',
  search: ''
})

// UI 层的「按金额」选项，提交时映射回后端的 token 模式。
type BillingModeUI = 'request' | 'cost'
const form = ref<Omit<Partial<DownstreamConfig>, 'billing_mode'> & { billing_mode?: BillingModeUI }>({
  id: '',
  name: '',
  hash: '',
  model_allowlist: [],
  rate_limit_enabled: true,
  per_minute_limit: 100,
  max_concurrency: 10,
  ip_allowlist: [],
  active: true,
  billing_mode: 'request'
})

const ipAllowlistText = computed({
  get: () => form.value.ip_allowlist?.join('\n') || '',
  set: (value: string) => {
    form.value.ip_allowlist = value.split('\n').filter(line => line.trim())
  }
})

const rules = {
  id: [
    { required: true, message: '请输入下游ID', trigger: 'blur' },
    { min: 1, message: 'ID不能为空', trigger: 'blur' }
  ],
  name: [{ required: true, message: '请输入名称', trigger: 'blur' }]
}

const copyKey = async (key: unknown) => {
  const copyableKey = getCopyableKey(key)
  if (!copyableKey) {
    ElMessage.warning('当前没有可复制的真实秘钥，请先轮换密钥')
    return
  }

  try {
    await navigator.clipboard.writeText(copyableKey)
    ElMessage.success('已复制到剪贴板')
  } catch {
    const textArea = document.createElement('textarea')
    textArea.value = copyableKey
    textArea.style.position = 'fixed'
    textArea.style.left = '-9999px'
    document.body.appendChild(textArea)
    textArea.focus()
    textArea.select()
    try {
      document.execCommand('copy')
      ElMessage.success('已复制到剪贴板')
    } catch {
      ElMessage.error('复制失败，请手动复制')
    }
    document.body.removeChild(textArea)
  }
}

const loadData = async () => {
  try {
    loading.value = true
    const params: any = {}
    if (filters.value.status !== 'all') params.status = filters.value.status
    if (filters.value.lifecycle !== 'all') params.lifecycle = filters.value.lifecycle
    if (filters.value.search) params.search = filters.value.search

    const { data } = await adminApi.getDownstreams(params)
    downstreams.value = data.map(item => ({
      ...item,
      rate_limit_enabled: item.rate_limit_enabled ?? true,
      max_concurrency: item.max_concurrency ?? 10
    }))
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const loadModels = async () => {
  try {
    const { data } = await adminApi.getModels()
    availableModels.value = data.models
  } catch (error) {
    ElMessage.error('加载模型列表失败')
  }
}

const loadRuntime = async () => {
  try {
    const { data } = await adminApi.getDownstreamRuntime()
    const next: Record<string, DownstreamConcurrencySnapshot> = {}
    for (const item of data.items) {
      next[item.downstream_id] = item.concurrency
    }
    for (const row of downstreams.value) {
      if (!next[row.id]) {
        next[row.id] = unavailableRuntime(row.max_concurrency ?? 10, data.updated_at)
      }
    }
    runtimeById.value = next
  } catch {
    markRuntimeUnavailable()
  }
}

const unavailableRuntime = (limit: number, updatedAt = 0): DownstreamConcurrencySnapshot => ({
  available: false,
  limit,
  updated_at: updatedAt
})

const markRuntimeUnavailable = () => {
  const next: Record<string, DownstreamConcurrencySnapshot> = {}
  for (const row of downstreams.value) {
    next[row.id] = unavailableRuntime(row.max_concurrency ?? 10)
  }
  runtimeById.value = next
}

const handleCreate = () => {
  dialogMode.value = 'create'
  form.value = {
    id: '',
    name: '',
    hash: '',
    model_allowlist: [],
    rate_limit_enabled: true,
    per_minute_limit: 100,
    max_concurrency: 10,
    ip_allowlist: [],
    active: true,
    billing_mode: 'request'
  }
  requestQuotaHours.value = 5
  requestQuotaCount.value = 600
  resetCostFields()
  dialogVisible.value = true
}

const handleEdit = (row: DownstreamConfig) => {
  dialogMode.value = 'edit'
  form.value = {
    ...row,
    rate_limit_enabled: row.rate_limit_enabled ?? true,
    max_concurrency: row.max_concurrency ?? 10,
    billing_mode: isCostRow(row) ? 'cost' : 'request'
  }
  requestQuotaHours.value = row.request_quota_window_hours || 5
  requestQuotaCount.value = row.request_quota_requests || 600
  if (isCostRow(row)) {
    inputTokenPricePerMillion.value = row.input_token_price_per_million_cents ? row.input_token_price_per_million_cents / 100 : undefined
    outputTokenPricePerMillion.value = row.output_token_price_per_million_cents ? row.output_token_price_per_million_cents / 100 : undefined
    dailyCostLimit.value = row.daily_cost_limit_cents ? row.daily_cost_limit_cents / 100 : undefined
  } else {
    resetCostFields()
  }
  dialogVisible.value = true
}

const handleSubmit = async () => {
  try {
    await formRef.value.validate()
    
    if (dialogMode.value === 'create' && !form.value.id?.trim()) {
      ElMessage.error('请输入下游ID')
      return
    }
    
    if (form.value.rate_limit_enabled) {
      if (form.value.billing_mode === 'cost') {
        if (
          (!inputTokenPricePerMillion.value || inputTokenPricePerMillion.value < 0.01) &&
          (!outputTokenPricePerMillion.value || outputTokenPricePerMillion.value < 0.01)
        ) {
          ElMessage.error('请至少填写输入或输出价格中的一项')
          return
        }
        if (!dailyCostLimit.value || dailyCostLimit.value < 0.01) {
          ElMessage.error('请填写有效的每日金额上限')
          return
        }
      } else {
        if (!form.value.per_minute_limit || form.value.per_minute_limit < 1) {
          ElMessage.error('请填写有效的每分钟限制')
          return
        }
        if (!form.value.max_concurrency || form.value.max_concurrency < 1) {
          ElMessage.error('请填写有效的并发限制')
          return
        }
        if (requestQuotaHours.value < 1 || requestQuotaCount.value < 1) {
          ElMessage.error('请填写有效的时间窗口和请求次数')
          return
        }
      }
    }
    submitting.value = true

    const isCost = form.value.billing_mode === 'cost'
    const submitData: Record<string, unknown> = {
      ...form.value,
      billing_mode: isCost ? 'token' : 'request',
      daily_token_limit: null,
      input_token_price_per_million_cents: isCost ? Math.round((inputTokenPricePerMillion.value ?? 0) * 100) : null,
      output_token_price_per_million_cents: isCost ? Math.round((outputTokenPricePerMillion.value ?? 0) * 100) : null,
      daily_cost_limit_cents: isCost ? Math.round((dailyCostLimit.value ?? 0) * 100) : null,
      request_quota_window_hours: form.value.rate_limit_enabled ? requestQuotaHours.value : null,
      request_quota_requests: form.value.rate_limit_enabled ? requestQuotaCount.value : null
    }

    if (dialogMode.value === 'create') {
      const { data } = await adminApi.createDownstream(submitData)
      if (data.plaintext_key) {
        newPlaintextKey.value = data.plaintext_key
        rotateDialogVisible.value = true
      }
      ElMessage.success('创建成功')
    } else {
      await adminApi.updateDownstream(form.value.id!, submitData)
      ElMessage.success('更新成功')
    }

    dialogVisible.value = false
    loadData()
  } catch (error: any) {
    if (error.response?.status === 409) {
      ElMessage.error('创建冲突，请重试')
    } else {
      ElMessage.error('操作失败')
    }
  } finally {
    submitting.value = false
  }
}

const handleToggle = async (row: DownstreamConfig) => {
  try {
    await adminApi.toggleDownstream(row.id)
    ElMessage.success('状态已更新')
    loadData()
  } catch (error) {
    ElMessage.error('操作失败')
  }
}

const handleRotate = async (row: DownstreamConfig) => {
  try {
    await ElMessageBox.confirm(`确定要轮换下游 "${row.name}" 的密钥吗？旧密钥将立即失效。`, '确认轮换', {
      type: 'warning'
    })
    
    const { data } = await adminApi.rotateDownstream(row.id)
    newPlaintextKey.value = data.plaintext_key
    rotateDialogVisible.value = true
    ElMessage.success('密钥已轮换')
    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('轮换失败')
    }
  }
}

const handleDelete = async (row: DownstreamConfig) => {
  try {
    await ElMessageBox.confirm(`确定要删除下游 "${row.name}" 吗？`, '确认删除', {
      type: 'warning'
    })

    await adminApi.deleteDownstream(row.id)
    ElMessage.success('删除成功')
    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('删除失败')
    }
  }
}

const formatMoney = (cents: number) => `¥${(cents / 100).toFixed(2)}`


// 按金额计费：token 模式 + 至少一个单价 + 每日金额上限同时配置才生效。
const isCostRow = (row: DownstreamConfig) =>
  row.billing_mode === 'token' &&
  (row.input_token_price_per_million_cents != null || row.output_token_price_per_million_cents != null) &&
  row.daily_cost_limit_cents != null

const resetCostFields = () => {
  inputTokenPricePerMillion.value = undefined
  outputTokenPricePerMillion.value = undefined
  dailyCostLimit.value = undefined
}

const handleSelectionChange = (rows: DownstreamConfig[]) => {
  selectedRows.value = rows
}

const submitBatchMode = async () => {
  try {
    batchSubmitting.value = true
    const ids = selectedRows.value.map(row => row.id)
    const payload: {
      ids: string[]
      billing_mode: 'request' | 'token'
      daily_token_limit?: number | null
      input_token_price_per_million_cents?: number | null
      output_token_price_per_million_cents?: number | null
      daily_cost_limit_cents?: number | null
    } = {
      ids,
      billing_mode: batchForm.value.billing_mode === 'cost' ? 'token' : 'request',
      daily_token_limit: null
    }
    if (batchForm.value.billing_mode === 'cost') {
      if (batchForm.value.input_token_price_per_million_cents) {
        payload.input_token_price_per_million_cents = Math.round(batchForm.value.input_token_price_per_million_cents * 100)
      }
      if (batchForm.value.output_token_price_per_million_cents) {
        payload.output_token_price_per_million_cents = Math.round(batchForm.value.output_token_price_per_million_cents * 100)
      }
      if (batchForm.value.daily_cost_limit_cents) {
        payload.daily_cost_limit_cents = Math.round(batchForm.value.daily_cost_limit_cents * 100)
      }
    }
    const { data } = await adminApi.batchSetDownstreamMode(payload)
    ElMessage.success(
      `已更新 ${data.updated} 个下游${data.failed.length ? `，${data.failed.length} 个失败` : ''}`
    )
    batchDialogVisible.value = false
    batchForm.value.input_token_price_per_million_cents = undefined
    batchForm.value.output_token_price_per_million_cents = undefined
    batchForm.value.daily_cost_limit_cents = undefined
    loadData()
  } catch (error) {
    ElMessage.error('批量设置失败')
  } finally {
    batchSubmitting.value = false
  }
}

onMounted(() => {
  loadData()
  loadModels()
  loadRuntime()
  runtimeTimer = window.setInterval(loadRuntime, 5000)
})

onUnmounted(() => {
  if (runtimeTimer !== null) {
    clearInterval(runtimeTimer)
    runtimeTimer = null
  }
})
</script>

<style scoped>

.table-column-settings-item {
  margin-right: 0;
  margin-bottom: 8px;
}
.downstreams-page {
  min-height: 100%;
}

.downstream-filters {
  align-items: flex-end;
}

.downstream-filters :deep(.el-form-item) {
  margin-right: 0;
  margin-bottom: 0;
}

.key-result-surface {
  margin: 20px 0;
  padding: 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface-muted);
}

.key-result-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
  color: var(--crc-text-strong);
  font-size: 13px;
  font-weight: 600;
}

.new-key-value {
  display: block;
  width: 100%;
  overflow-wrap: anywhere;
  user-select: all;
}

.runtime-cell {
  display: flex;
  flex-wrap: nowrap;
  align-items: center;
  gap: 8px;
  white-space: nowrap;
}

.runtime-metric {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  color: var(--crc-text);
}

.key-cell {
  display: flex;
  align-items: center;
  flex-wrap: nowrap;
  gap: 6px;
  min-width: 0;
}

code {
  padding: 2px 6px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-strong);
  background: var(--crc-surface-muted);
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
}

.legacy-key-hint {
  color: var(--crc-text-muted);
}

.helper-text {
  margin-top: 8px;
}

.price-row {
  display: flex;
  gap: 12px;
  width: 100%;
}

.price-field {
  flex: 1;
  min-width: 0;
}

.price-label {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-bottom: 6px;
  font-size: 12px;
  color: var(--crc-text-muted);
}

:global(.form-drawer .el-drawer__header) {
  margin-bottom: 0;
  padding: 16px 24px;
  border-bottom: 1px solid var(--crc-border);
}

:global(.form-drawer .el-drawer__body) {
  padding: 24px 32px;
  overflow-y: auto;
}

:global(.form-drawer .el-drawer__footer) {
  border-top: 1px solid var(--crc-border);
  padding: 12px 24px;
  background: var(--crc-surface);
}

.drawer-form {
  width: 100%;
}

.drawer-section {
  margin: 26px 0 20px;
}

.drawer-section :deep(.el-divider__text) {
  color: var(--crc-text-strong);
  font-size: 13px;
  font-weight: 600;
}

.drawer-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

@media (max-width: 767px) {
  .downstream-filters {
    display: grid;
    grid-template-columns: 1fr;
  }

  :global(.form-drawer .el-drawer__body) {
    padding: 18px 16px;
  }
}

.key-cell code {
  min-width: 0;
  overflow: hidden;
  padding: 3px 8px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text);
  background: var(--crc-canvas);
  font-family: var(--crc-font-mono);
  font-size: 11.5px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.legacy-key-hint {
  color: var(--crc-text-subtle);
  font-size: 12px;
}

.key-result-surface code.new-key-value {
  font-family: var(--crc-font-mono);
  letter-spacing: 0.02em;
}

.downstream-filters :deep(.el-form-item__label) {
  font-family: var(--crc-font-mono);
  font-size: 11px;
  letter-spacing: 0.06em;
}

.downstream-filters :deep(.el-select) {
  min-width: 150px;
}

.downstream-filters :deep(.el-input) {
  min-width: 220px;
}

.filter-label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.filter-label :deep(svg) {
  color: var(--crc-accent);
}

@media (max-width: 767px) {
  .downstream-filters :deep(.el-select),
  .downstream-filters :deep(.el-input) {
    min-width: 0;
    width: 100%;
  }
}

.downstreams-table-shell {
  overflow: hidden;
}

.downstreams-table-shell > .compact-downstreams-table {
  min-width: 0;
}

.copy-key-button {
  width: 26px;
  min-width: 26px;
  height: 26px;
  padding: 0;
}

.row-actions {
  display: flex;
  flex-wrap: nowrap;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
}

.row-actions :deep(.el-button + .el-button) {
  margin-left: 0;
}

</style>
