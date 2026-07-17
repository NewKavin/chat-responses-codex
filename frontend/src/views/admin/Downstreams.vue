<template>
  <div class="crc-page downstreams-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">下游管理</h1>
        <p class="crc-page-description">管理门户身份、可用模型、调用限额、生命周期和访问密钥。</p>
      </div>
      <el-button type="primary" @click="handleCreate">创建下游</el-button>
    </header>

    <el-form :inline="true" class="crc-toolbar downstream-filters">
      <el-form-item label="状态">
        <el-select v-model="filters.status" @change="loadData" placeholder="全部">
          <el-option label="全部" value="all" />
          <el-option label="启用" value="active" />
          <el-option label="禁用" value="inactive" />
        </el-select>
      </el-form-item>
      <el-form-item label="生命周期">
        <el-select v-model="filters.lifecycle" @change="loadData" placeholder="全部">
          <el-option label="全部" value="all" />
          <el-option label="试用" value="trial" />
          <el-option label="永久" value="permanent" />
        </el-select>
      </el-form-item>
      <el-form-item label="搜索">
        <el-input v-model="filters.search" @input="loadData" placeholder="名称或ID" clearable />
      </el-form-item>
    </el-form>
      
    <div class="crc-table-shell">
      <el-table :data="downstreams" v-loading="loading" stripe>
        <el-table-column prop="id" label="ID" width="150" />
        <el-table-column prop="name" label="名称" width="200" />
        <el-table-column label="秘钥" width="220">
          <template #default="{ row }">
            <div class="key-cell">
              <code v-if="hasUsablePlaintextKey(row.plaintext_key) && !expandedKeys.includes(row.id)">
                {{ maskPlaintextKey(row.plaintext_key) }}
              </code>
              <code v-else-if="hasUsablePlaintextKey(row.plaintext_key)" class="full-key">
                {{ row.plaintext_key }}
              </code>
              <span v-else class="legacy-key-hint">未存储真实秘钥，请先轮换</span>
              <el-button-group>
                <el-button size="small" @click="toggleKeyView(row.id)" :disabled="!hasUsablePlaintextKey(row.plaintext_key)">
                  {{ expandedKeys.includes(row.id) ? '隐藏' : '查看' }}
                </el-button>
                <el-button size="small" @click="copyKey(row.plaintext_key)" :disabled="!hasUsablePlaintextKey(row.plaintext_key)">
                  复制秘钥
                </el-button>
              </el-button-group>
            </div>
          </template>
        </el-table-column>
        <el-table-column label="限额配置" min-width="320">
          <template #default="{ row }">
            <span v-if="!row.rate_limit_enabled">未启用限额</span>
            <span v-else>
              {{ row.per_minute_limit }}/分钟 · 并发 {{ row.max_concurrency }} · {{ row.request_quota_window_hours || 0 }} 小时 {{ row.request_quota_requests || 0 }} 次
            </span>
          </template>
        </el-table-column>
        <el-table-column label="生命周期" width="120">
          <template #default="{ row }">
            <el-tag :type="row.expires_at ? 'warning' : 'success'">
              {{ row.expires_at ? '试用' : '永久' }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column label="状态" width="100">
          <template #default="{ row }">
            <el-tag :type="row.active ? 'success' : 'danger'">
              {{ row.active ? '启用' : '禁用' }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column label="操作" width="300" fixed="right">
          <template #default="{ row }">
            <el-button size="small" @click="handleEdit(row)">编辑</el-button>
            <el-button size="small" @click="handleToggle(row)">
              {{ row.active ? '禁用' : '启用' }}
            </el-button>
            <el-button size="small" type="warning" @click="handleRotate(row)">轮换密钥</el-button>
            <el-button size="small" type="danger" @click="handleDelete(row)">删除</el-button>
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
      size="min(680px, 100vw)"
      :destroy-on-close="false"
      class="form-drawer"
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
          <el-form-item label="每分钟限制" prop="per_minute_limit">
            <el-input-number v-model="form.per_minute_limit" :min="1" :max="10000" />
          </el-form-item>
          <el-form-item label="并发限制">
            <el-input-number v-model="form.max_concurrency" :min="1" :max="5000" />
          </el-form-item>
          <el-form-item label="时间窗口（小时）">
            <el-input-number v-model="requestQuotaHours" :min="1" :max="168" />
          </el-form-item>
          <el-form-item label="窗口请求次数">
            <el-input-number v-model="requestQuotaCount" :min="1" :max="1000000" />
          </el-form-item>
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
              <el-icon><CopyDocument /></el-icon>
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
import { ref, computed, onMounted } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { CopyDocument } from '@element-plus/icons-vue'
import { adminApi } from '@/api/admin'
import type { DownstreamConfig } from '@/types'
import { getCopyableKey, hasUsablePlaintextKey, maskPlaintextKey } from '@/utils/keyUtils'

const loading = ref(false)
const downstreams = ref<DownstreamConfig[]>([])
const dialogVisible = ref(false)
const rotateDialogVisible = ref(false)
const dialogMode = ref<'create' | 'edit'>('create')
const submitting = ref(false)
const formRef = ref()
const newPlaintextKey = ref('')
const expandedKeys = ref<string[]>([])
const requestQuotaHours = ref(5)
const requestQuotaCount = ref(600)
const availableModels = ref<string[]>([])

const filters = ref({
  status: 'all',
  lifecycle: 'all',
  search: ''
})

const form = ref<Partial<DownstreamConfig>>({
  id: '',
  name: '',
  hash: '',
  model_allowlist: [],
  rate_limit_enabled: true,
  per_minute_limit: 100,
  max_concurrency: 10,
  ip_allowlist: [],
  active: true
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

const toggleKeyView = (id: string) => {
  const index = expandedKeys.value.indexOf(id)
  if (index > -1) {
    expandedKeys.value.splice(index, 1)
  } else {
    expandedKeys.value.push(id)
  }
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
    active: true
  }
  requestQuotaHours.value = 5
  requestQuotaCount.value = 600
  dialogVisible.value = true
}

const handleEdit = (row: DownstreamConfig) => {
  dialogMode.value = 'edit'
  form.value = {
    ...row,
    rate_limit_enabled: row.rate_limit_enabled ?? true,
    max_concurrency: row.max_concurrency ?? 10
  }
  requestQuotaHours.value = row.request_quota_window_hours || 5
  requestQuotaCount.value = row.request_quota_requests || 600
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
    submitting.value = true

    const submitData: Record<string, unknown> = {
      ...form.value,
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

onMounted(() => {
  loadData()
  loadModels()
})
</script>

<style scoped>
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

.key-cell {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.full-key {
  word-break: break-all;
  flex: 1;
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
</style>
