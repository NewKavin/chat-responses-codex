<template>
  <div class="upstreams-container">
    <el-card>
      <template #header>
        <div class="header">
          <h2>上游管理</h2>
          <el-button type="primary" @click="handleCreate">创建上游</el-button>
        </div>
      </template>
      
      <el-table :data="upstreams" v-loading="loading" stripe style="width: 100%">
        <el-table-column prop="id" label="ID" width="150" />
        <el-table-column prop="name" label="名称" width="200" />
        <el-table-column label="协议" width="220">
          <template #default="{ row }">
            <el-space wrap>
              <el-tag
                v-for="protocol in displayProtocols(row)"
                :key="`${row.id}-${protocol}`"
                size="small"
              >
                {{ protocol }}
              </el-tag>
            </el-space>
          </template>
        </el-table-column>
        <el-table-column label="模型数量" width="100">
          <template #default="{ row }">
            {{ row.supported_models.length }}
          </template>
        </el-table-column>
        
        <!-- 运行时状态显示 -->
        <el-table-column label="并发数" width="100">
          <template #default="{ row }">
            <span v-if="row.runtime_state">
              {{ row.runtime_state.in_flight }} / {{ row.max_concurrency }}
            </span>
            <span v-else>-</span>
          </template>
        </el-table-column>
        
        <el-table-column label="每分钟请求" width="180">
          <template #default="{ row }">
            <div v-if="row.runtime_state" class="quota-cell">
              <el-progress 
                :percentage="row.runtime_state.minute_percentage" 
                :color="getProgressColor(row.runtime_state.minute_percentage)"
                :show-text="false"
                style="width: 70px; margin-right: 8px;"
              />
              <span class="quota-text">
                {{ row.runtime_state.minute_cost }} / {{ row.runtime_state.minute_limit }}
              </span>
            </div>
            <span v-else class="quota-text-static">{{ row.requests_per_minute }}</span>
          </template>
        </el-table-column>
        
        <el-table-column label="窗口配额" width="220">
          <template #default="{ row }">
            <div v-if="row.runtime_state" class="quota-cell">
              <el-progress 
                :percentage="row.runtime_state.five_hour_percentage" 
                :color="getProgressColor(row.runtime_state.five_hour_percentage)"
                :show-text="false"
                style="width: 70px; margin-right: 8px;"
              />
              <span class="quota-text">
                {{ row.request_quota_window_hours }}小时 {{ row.runtime_state.five_hour_cost }} / {{ row.runtime_state.five_hour_limit }}
              </span>
            </div>
            <span v-else class="quota-text-static">{{ row.request_quota_window_hours }}小时 {{ row.request_quota_requests }}</span>
          </template>
        </el-table-column>
        
        <el-table-column label="高端模型保护" width="140">
          <template #default="{ row }">
            <el-tooltip v-if="row.protect_premium_quota && row.premium_models.length > 0" 
                        :content="'保护模型: ' + row.premium_models.join(', ')" 
                        placement="top">
              <el-tag type="warning" size="small">
                保护中 ({{ row.premium_models.length }}个)
              </el-tag>
            </el-tooltip>
            <span v-else>-</span>
          </template>
        </el-table-column>
        
        <el-table-column label="状态" width="100">
          <template #default="{ row }">
            <el-tag :type="row.active ? 'success' : 'danger'">
              {{ row.active ? '启用' : '禁用' }}
            </el-tag>
          </template>
        </el-table-column>
        
        <el-table-column label="操作" width="340" fixed="right">
          <template #default="{ row }">
            <el-button size="small" @click="handleEdit(row)">编辑</el-button>
            <el-button size="small" @click="handleCopy(row)">复制</el-button>
            <el-button size="small" @click="handleToggle(row)">
              {{ row.active ? '禁用' : '启用' }}
            </el-button>
            <el-button size="small" type="danger" @click="handleDelete(row)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
    </el-card>
    
    <!-- Create/Edit Dialog -->
    <el-dialog
      v-model="dialogVisible"
      :title="dialogMode === 'create' ? '创建上游' : '编辑上游'"
      width="700px"
    >
      <el-form :model="form" :rules="rules" ref="formRef" label-width="140px">
        <el-form-item v-if="dialogMode === 'edit'" label="ID">
          <el-input v-model="form.id" disabled />
        </el-form-item>
        <el-form-item label="名称" prop="name">
          <el-input v-model="form.name" placeholder="例如: OpenAI 主上游" />
        </el-form-item>
        <el-form-item label="Base URL" prop="base_url">
          <el-input v-model="form.base_url" placeholder="https://api.openai.com" />
        </el-form-item>
        <el-form-item label="API Key" prop="api_key">
          <el-input
            v-model="form.api_key"
            type="textarea"
            :rows="3"
            placeholder="每行一个 Key&#10;支持多 Key 快速创建多个同名上游"
          />
          <span class="form-hint">多行输入多个 Key，每行一个；单 Key 时不影响原有行为</span>
        </el-form-item>
        <el-form-item label="协议" prop="protocols">
          <el-select v-model="form.protocols" multiple>
            <el-option label="ChatCompletions" value="ChatCompletions" />
            <el-option label="Responses" value="Responses" />
          </el-select>
        </el-form-item>

        <!-- 限额配置 -->
        <el-divider>限额配置 (仅显示,不做实际校验)</el-divider>
        <el-form-item label="每分钟请求数">
          <el-input-number v-model="form.requests_per_minute" :min="1" :max="10000" />
          <span class="form-hint">用于显示和监控,不做强制限制</span>
        </el-form-item>
        <el-form-item label="配额窗口（小时）">
          <el-input-number v-model="form.request_quota_window_hours" :min="1" :max="168" />
          <span class="form-hint">用于显示和监控,不做强制限制</span>
        </el-form-item>
        <el-form-item label="窗口请求次数">
          <el-input-number v-model="form.request_quota_requests" :min="1" :max="1000000" />
          <span class="form-hint">用于显示和监控,不做强制限制</span>
        </el-form-item>
        <el-form-item label="最大并发数">
          <el-input-number v-model="form.max_concurrency" :min="1" :max="1000" />
          <span class="form-hint">用于显示和监控,不做强制限制</span>
        </el-form-item>

        <!-- 模型配置 -->
        <el-divider>模型配置</el-divider>
        <el-form-item label="支持的模型">
          <div class="model-input-group">
            <el-select v-model="form.supported_models" multiple filterable allow-create placeholder="手动输入或点击获取模型">
            </el-select>
            <el-button
              v-if="form.base_url && form.api_key"
              @click="fetchModels"
              :loading="fetchingModels"
              class="fetch-btn"
            >
              获取模型
            </el-button>
          </div>
        </el-form-item>

        <el-form-item label="模型成本">
          <el-table :data="form.model_request_costs" style="width: 100%; margin-bottom: 10px">
            <el-table-column label="模型" width="200">
              <template #default="{ row }">
                <el-select v-model="row.slug" placeholder="选择模型" filterable allow-create>
                  <el-option v-for="model in availableModelsForCost" :key="model" :label="model" :value="model" />
                </el-select>
              </template>
            </el-table-column>
            <el-table-column prop="cost" label="成本" width="120">
              <template #default="{ row }">
                <el-input-number v-model="row.cost" :min="0" :step="0.01" :precision="4" />
              </template>
            </el-table-column>
            <el-table-column label="操作" width="100">
              <template #default="{ row }">
                <el-button size="small" type="danger" @click="removeModelCost(row)">删除</el-button>
              </template>
            </el-table-column>
          </el-table>
          <el-button @click="addModelCost" size="small">添加模型成本</el-button>
        </el-form-item>

        <el-divider>模型上下文</el-divider>
        <el-tabs v-model="contextConfigTab">
          <el-tab-pane label="默认上下文" name="default">
            <el-form-item label="上下文上限">
              <el-input-number v-model="form.default_model_context!.context_limit" :min="0" :max="2000000" />
              <span class="form-hint">留空或 0 表示不启用默认值，后续仅按模型覆盖配置生效</span>
            </el-form-item>
            <el-form-item label="输出预留">
              <el-input-number v-model="form.default_model_context!.output_reserve" :min="0" :max="2000000" />
              <span class="form-hint">输入 0 时自动回退到网关默认预留值</span>
            </el-form-item>
            <el-form-item label="上下文分组">
              <el-input v-model="form.default_model_context!.context_group" placeholder="可选: 与模型分组一致时可自动切换更大上下文模型" />
            </el-form-item>
            <el-form-item>
              <el-button v-if="dialogMode === 'edit'" size="small" @click="clearDefaultContextConfig">清空默认上下文</el-button>
            </el-form-item>
          </el-tab-pane>
          <el-tab-pane label="模型覆盖" name="overrides">
            <el-table :data="form.model_contexts" style="width: 100%; margin-bottom: 10px">
              <el-table-column label="模型" width="220">
                <template #default="{ row }">
                  <el-select v-model="row.slug" placeholder="选择模型" filterable allow-create>
                    <el-option v-for="model in availableModelsForCost" :key="model" :label="model" :value="model" />
                  </el-select>
                </template>
              </el-table-column>
              <el-table-column label="上下文上限" width="160">
                <template #default="{ row }">
                  <el-input-number v-model="row.context_limit" :min="1" :max="2000000" />
                </template>
              </el-table-column>
              <el-table-column label="输出预留" width="160">
                <template #default="{ row }">
                  <el-input-number v-model="row.output_reserve" :min="0" :max="2000000" />
                </template>
              </el-table-column>
              <el-table-column label="上下文分组" min-width="180">
                <template #default="{ row }">
                  <el-input v-model="row.context_group" placeholder="可选: 同组可自动切换更大上下文模型" />
                </template>
              </el-table-column>
              <el-table-column label="操作" width="100">
                <template #default="{ row }">
                  <el-button size="small" type="danger" @click="removeModelContext(row)">删除</el-button>
                </template>
              </el-table-column>
            </el-table>
            <el-button @click="addModelContext" size="small">添加模型上下文</el-button>
          </el-tab-pane>
        </el-tabs>

        <!-- 路由权重配置 -->
        <el-divider>智能路由配置</el-divider>
        <el-form-item label="优先级权重">
          <el-input-number v-model="form.priority" :min="0" :max="1000" placeholder="数字越大优先级越高" />
          <el-alert
            title="说明"
            type="info"
            :closable="false"
            class="helper-text"
          >
            用于控制路由优先级。权重高的账号优先被选中。默认为0。
          </el-alert>
        </el-form-item>
        <el-form-item label="高端模型列表">
          <el-select v-model="form.premium_models" multiple filterable allow-create placeholder="选择此账号的高端模型">
            <el-option v-for="model in form.supported_models" :key="model" :label="model" :value="model" />
          </el-select>
          <el-alert
            title="说明"
            type="info"
            :closable="false"
            class="helper-text"
          >
            配置此账号独有的高端模型(如 glm-5.1)。这些模型只能通过此账号访问。
          </el-alert>
        </el-form-item>
        <el-form-item label="保护高端额度">
          <el-switch v-model="form.protect_premium_quota" />
          <el-alert
            title="说明"
            type="warning"
            :closable="false"
            class="helper-text"
          >
            <strong>重要:</strong> 开启后,请求非高端模型时会优先避开此账号,仅在其他账号不可用时才回退使用。
            这样可以保护高端模型的额度,避免被低权重模型占用。
          </el-alert>
        </el-form-item>

        <el-form-item label="启用">
          <el-switch v-model="form.active" />
        </el-form-item>
      </el-form>
      
      <template #footer>
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="primary" @click="handleSubmit" :loading="submitting">确定</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, computed, onUnmounted } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { adminApi, type BatchCreateUpstreamPayload } from '@/api/admin'
import type { UpstreamConfig } from '@/types'

const loading = ref(false)
const upstreams = ref<UpstreamConfig[]>([])
const dialogVisible = ref(false)
const dialogMode = ref<'create' | 'edit'>('create')
const submitting = ref(false)
const fetchingModels = ref(false)
const formRef = ref()
const contextConfigTab = ref<'default' | 'overrides'>('overrides')
const clearDefaultContext = ref(false)

// Auto-refresh timer
let refreshTimer: number | null = null

const form = ref<Partial<UpstreamConfig>>({
  id: '',
  name: '',
  base_url: '',
  api_key: '',
  protocol: 'ChatCompletions',
  protocols: ['ChatCompletions'],
  supported_models: [],
  default_model_context: {
    context_limit: 0,
    output_reserve: 0,
    context_group: ''
  },
  active: true,
  request_quota_window_hours: 5,
  request_quota_requests: 600,
  requests_per_minute: 100,
  max_concurrency: 10,
  model_request_costs: [],
  model_contexts: [],
  priority: 0,
  premium_models: [],
  protect_premium_quota: false,
  failure_count: 0
})

const availableModelsForCost = computed(() => {
  const supported = form.value.supported_models || []
  return Array.from(new Set(supported)).sort()
})

const addModelCost = () => {
  if (!form.value.model_request_costs) {
    form.value.model_request_costs = []
  }
  form.value.model_request_costs.push({ slug: '', cost: 0 })
}

const removeModelCost = (row: any) => {
  const index = form.value.model_request_costs?.indexOf(row)
  if (index !== undefined && index > -1) {
    form.value.model_request_costs?.splice(index, 1)
  }
}

const addModelContext = () => {
  if (!form.value.model_contexts) {
    form.value.model_contexts = []
  }
  form.value.model_contexts.push({
    slug: '',
    context_limit: 200000,
    output_reserve: 4096,
    context_group: ''
  })
}

const clearDefaultContextConfig = () => {
  if (!form.value.default_model_context) {
    form.value.default_model_context = {
      context_limit: 0,
      output_reserve: 0,
      context_group: ''
    }
  } else {
    form.value.default_model_context.context_limit = 0
    form.value.default_model_context.output_reserve = 0
    form.value.default_model_context.context_group = ''
  }
  clearDefaultContext.value = true
}

const removeModelContext = (row: any) => {
  const index = form.value.model_contexts?.indexOf(row)
  if (index !== undefined && index > -1) {
    form.value.model_contexts?.splice(index, 1)
  }
}

const rules = {
  name: [{ required: true, message: '请输入名称', trigger: 'blur' }],
  base_url: [{ required: true, message: '请输入Base URL', trigger: 'blur' }],
  api_key: [{ required: true, message: '请输入API Key', trigger: 'blur' }],
  protocols: [{ required: true, message: '请选择协议', trigger: 'change' }]
}

const getProgressColor = (percentage: number) => {
  if (percentage < 60) return '#67c23a'
  if (percentage < 80) return '#e6a23c'
  return '#f56c6c'
}

const loadData = async () => {
  try {
    loading.value = true
    const { data } = await adminApi.getUpstreams()
    upstreams.value = data
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const startAutoRefresh = () => {
  // Refresh every 5 seconds
  refreshTimer = window.setInterval(() => {
    loadData()
  }, 5000)
}

const stopAutoRefresh = () => {
  if (refreshTimer) {
    clearInterval(refreshTimer)
    refreshTimer = null
  }
}

const resolveProtocols = (value: Partial<UpstreamConfig>): UpstreamConfig['protocol'][] => {
  const fromProtocols = Array.isArray(value.protocols)
    ? value.protocols.filter(Boolean) as UpstreamConfig['protocol'][]
    : []
  const fallback: UpstreamConfig['protocol'][] = value.protocol
    ? [value.protocol]
    : ['ChatCompletions']
  return Array.from(new Set((fromProtocols.length > 0 ? fromProtocols : fallback)))
}

const displayProtocols = (value: UpstreamConfig) => resolveProtocols(value)

const handleCreate = () => {
  dialogMode.value = 'create'
  contextConfigTab.value = 'overrides'
  clearDefaultContext.value = false
  form.value = {
    id: '',
    name: '',
    base_url: '',
    api_key: '',
    protocol: 'ChatCompletions',
    protocols: ['ChatCompletions'],
    supported_models: [],
    default_model_context: {
      context_limit: 0,
      output_reserve: 0,
      context_group: ''
    },
    active: true,
    request_quota_window_hours: 5,
    request_quota_requests: 600,
    requests_per_minute: 100,
    max_concurrency: 10,
    model_request_costs: [],
    model_contexts: [],
    priority: 0,
    premium_models: [],
    protect_premium_quota: false,
    failure_count: 0
  }
  dialogVisible.value = true
}

const handleCopy = (row: UpstreamConfig) => {
  dialogMode.value = 'create'
  contextConfigTab.value = 'overrides'
  clearDefaultContext.value = false
  const protocols = resolveProtocols(row)
  form.value = {
    id: '',
    name: row.name + ' (副本)',
    base_url: row.base_url,
    api_key: '',
    protocol: protocols[0] as UpstreamConfig['protocol'],
    protocols,
    supported_models: [...(row.supported_models || [])],
    default_model_context: row.default_model_context
      ? { ...row.default_model_context }
      : { context_limit: 0, output_reserve: 0, context_group: '' },
    active: row.active,
    request_quota_window_hours: row.request_quota_window_hours,
    request_quota_requests: row.request_quota_requests,
    requests_per_minute: row.requests_per_minute,
    max_concurrency: row.max_concurrency,
    model_request_costs: row.model_request_costs ? [...row.model_request_costs] : [],
    model_contexts: row.model_contexts ? [...row.model_contexts] : [],
    priority: row.priority,
    premium_models: [...(row.premium_models || [])],
    protect_premium_quota: row.protect_premium_quota,
    failure_count: 0
  }
  dialogVisible.value = true
}

const handleEdit = (row: UpstreamConfig) => {
  dialogMode.value = 'edit'
  contextConfigTab.value = 'default'
  clearDefaultContext.value = false
  const protocols = resolveProtocols(row)
  form.value = {
    ...row,
    protocol: protocols[0] as UpstreamConfig['protocol'],
    protocols,
    default_model_context: row.default_model_context
      ? {
          ...row.default_model_context
        }
      : {
          context_limit: 0,
          output_reserve: 0,
          context_group: ''
        },
    model_request_costs: row.model_request_costs ? [...row.model_request_costs] : [],
    model_contexts: row.model_contexts ? [...row.model_contexts] : []
  }
  dialogVisible.value = true
}

const handleSubmit = async () => {
  try {
    await formRef.value.validate()
    submitting.value = true

    const submitData: Partial<UpstreamConfig> = {
      ...form.value
    }
    submitData.model_contexts = (submitData.model_contexts || [])
      .map((item: any) => ({
        slug: String(item.slug || '').trim(),
        context_limit: Number(item.context_limit || 0),
        output_reserve: Number(item.output_reserve || 0),
        context_group: String(item.context_group || '').trim()
      }))
      .filter(item => item.slug.length > 0 && item.context_limit > 0)
    if (submitData.default_model_context) {
      const context = submitData.default_model_context
      const context_limit = Number(context.context_limit || 0)
      const output_reserve = Number(context.output_reserve || 0)
      const context_group = String(context.context_group || '').trim()
      if (context_limit > 0) {
        submitData.default_model_context = {
          context_limit,
          output_reserve,
          context_group
        }
      } else {
        submitData.default_model_context = {
          context_limit: 0,
          output_reserve: 0,
          context_group: ''
        }
        if (!clearDefaultContext.value) {
          delete submitData.default_model_context
        }
      }
    }
    const protocols = resolveProtocols(submitData)
    submitData.protocols = protocols
    submitData.protocol = protocols[0] as UpstreamConfig['protocol']
    
    if (dialogMode.value === 'create') {
      submitData.id = ''
      // 将 API Key 按换行分割，每行一个 key 创建一个上游
      const apiKeys = (form.value.api_key || '')
        .split('\n')
        .map(k => k.trim())
        .filter(k => k.length > 0)
      
      if (apiKeys.length === 0) {
        ElMessage.error('请输入至少一个 API Key')
        submitting.value = false
        return
      }
      
      if (apiKeys.length === 1) {
        // 单 key：保持原有行为
        submitData.api_key = apiKeys[0]
        await adminApi.createUpstream(submitData)
        ElMessage.success('创建成功')
      } else {
        // 多 key：改用 batch 接口自动获取模型
        const batchPayload: BatchCreateUpstreamPayload = {
          name: form.value.name!,
          base_url: form.value.base_url!,
          keys: apiKeys,
          protocol: protocols[0] ? String(protocols[0]) : 'ChatCompletions',
          protocols: protocols.map(p => String(p)),
          requests_per_minute: submitData.requests_per_minute,
          request_quota_window_hours: submitData.request_quota_window_hours,
          request_quota_requests: submitData.request_quota_requests,
          max_concurrency: submitData.max_concurrency,
          active: submitData.active
        }

        const response = await adminApi.createUpstreamsBatch(batchPayload)
        const result = response.data

        if (result.failed > 0 && result.created > 0) {
          ElMessage.success(`创建成功 ${result.created} 个，${result.failed} 个失败`)
        } else if (result.created > 0) {
          ElMessage.success(`创建成功 ${result.created} 个`)
        } else {
          const errors = result.results.filter(r => r.error).map(r => r.error).join('；')
          ElMessage.error(`创建失败：${errors || '全部失败'}`)
        }
      }
    } else {
      await adminApi.updateUpstream(form.value.id!, submitData)
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
    clearDefaultContext.value = false
    submitting.value = false
  }
}

const handleToggle = async (row: UpstreamConfig) => {
  try {
    await adminApi.toggleUpstream(row.id)
    ElMessage.success('状态已更新')
    loadData()
  } catch (error) {
    ElMessage.error('操作失败')
  }
}

const handleDelete = async (row: UpstreamConfig) => {
  try {
    await ElMessageBox.confirm(`确定要删除上游 "${row.name}" 吗？`, '确认删除', {
      type: 'warning'
    })

    await adminApi.deleteUpstream(row.id)
    ElMessage.success('删除成功')
    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('删除失败')
    }
  }
}

const fetchModels = async () => {
  if (!form.value.base_url || !form.value.api_key) {
    ElMessage.warning('请先填写 Base URL 和 API Key')
    return
  }

  // 取所有有效 Key
  const apiKeys = (form.value.api_key || '')
    .split('\n')
    .map(k => k.trim())
    .filter(k => k.length > 0)

  if (apiKeys.length === 0) {
    ElMessage.warning('请输入至少一个有效的 API Key')
    return
  }

  // 取 base_url 第一行（多行粘贴时取第一行有效 URL）
  const baseUrl = (form.value.base_url || '')
    .split('\n')
    .map(u => u.trim())
    .filter(u => u.length > 0)[0] || form.value.base_url

  try {
    fetchingModels.value = true

    const allModels = new Set<string>()
    let successCount = 0
    let failCount = 0

    for (const key of apiKeys) {
      try {
        const response = await fetch(baseUrl + '/v1/models', {
          headers: {
            'Authorization': 'Bearer ' + key
          }
        })

        if (!response.ok) {
          failCount++
          continue
        }

        const data = await response.json()
        const models: string[] = (data.data || [])
          .map((m: any) => (typeof m?.id === 'string' ? m.id : ''))
          .filter((id: string) => id.length > 0)

        for (const model of models) {
          const trimmed = typeof model === 'string' ? model.trim() : ''
          if (trimmed) {
            allModels.add(trimmed)
          }
        }
        successCount++
      } catch {
        failCount++
      }
    }

    if (allModels.size === 0) {
      ElMessage.error('所有 Key 获取模型均失败')
      return
    }

    form.value.supported_models = Array.from(allModels).sort()

    const parts: string[] = ['成功获取 ' + allModels.size + ' 个模型']
    if (successCount > 1) {
      parts.push('用了 ' + successCount + ' 个 Key')
    }
    if (failCount > 0) {
      parts.push(failCount + ' 个 Key 获取失败')
    }
    ElMessage.success(parts.join('，'))
  } catch (error: any) {
    ElMessage.error('获取模型失败: ' + error.message)
  } finally {
    fetchingModels.value = false
  }
}


onMounted(() => {
  loadData()
  startAutoRefresh()
})

onUnmounted(() => {
  stopAutoRefresh()
})
</script>

<style scoped>
.upstreams-container {
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

.quota-cell {
  display: flex;
  align-items: center;
}

.quota-text {
  font-size: 12px;
  color: #606266;
  white-space: nowrap;
}

.quota-text-static {
  font-size: 13px;
  color: #909399;
}

.model-input-group {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  width: 100%;
}

.model-input-group :deep(.el-select) {
  flex: 1;
}

.fetch-btn {
  white-space: nowrap;
}

.helper-text {
  margin-top: 8px;
}

.form-hint {
  margin-left: 10px;
  font-size: 12px;
  color: #909399;
}
</style>
