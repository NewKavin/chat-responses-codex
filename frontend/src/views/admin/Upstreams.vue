<template>
  <div class="crc-page upstreams-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">上游管理</h1>
        <p class="crc-page-description">配置模型供应方、协议、密钥、上下文限制和智能路由策略。</p>
      </div>
      <el-button type="primary" @click="handleCreate">创建上游</el-button>
    </header>

    <div class="crc-table-shell">
      <el-table :data="upstreams" v-loading="loading" stripe style="width: 100%">
        <el-table-column prop="id" label="ID" width="150" />
        <el-table-column prop="name" label="名称" min-width="200" />
        <el-table-column label="协议" min-width="240">
          <template #default="{ row }">
            <div class="protocol-cell">
              <el-tag
                v-for="protocol in displayProtocols(row)"
                :key="`${row.id}-${protocol}`"
                size="small"
              >
                {{ protocol }}
              </el-tag>
            </div>
          </template>
        </el-table-column>
        <el-table-column label="模型数量" width="100">
          <template #default="{ row }">
            {{ row.supported_models.length }}
          </template>
        </el-table-column>
        <el-table-column label="Key 数量" width="100">
          <template #default="{ row }">
            {{ displayKeyCount(row) }} 个
          </template>
        </el-table-column>
        <el-table-column label="兼容清理" width="110">
          <template #default="{ row }">
            <el-tag v-if="row.strip_nonstandard_chat_fields" type="success" size="small">强制</el-tag>
            <el-tag v-else-if="isAutoChatCompatibility(row)" type="info" size="small">自动</el-tag>
            <span v-else>-</span>
          </template>
        </el-table-column>
        
        <el-table-column label="高端模型保护" min-width="160">
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
    </div>
    
    <!-- Create/Edit Drawer -->
    <el-drawer
      v-model="dialogVisible"
      :title="dialogMode === 'create' ? '创建上游' : '编辑上游'"
      direction="rtl"
      size="var(--account-drawer-width)"
      :destroy-on-close="false"
      class="form-drawer upstream-account-drawer"
    >
      <el-form ref="formRef" :model="form" :rules="rules" label-position="top" class="drawer-form">
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
        <el-form-item label="兼容清理">
          <el-switch v-model="form.strip_nonstandard_chat_fields" />
          <span class="form-hint">第三方 Chat 上游会自动保守清理；打开后对该上游强制清理 Codex/Responses 扩展字段</span>
        </el-form-item>

        <!-- 模型配置 -->
        <el-divider class="drawer-section">模型配置</el-divider>
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

        <el-divider class="drawer-section">模型上下文</el-divider>
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
            <el-form-item label="最大输出">
              <el-input-number v-model="form.default_model_context!.max_output_tokens" :min="0" :max="2000000" />
              <span class="form-hint">对 max_tokens 做上限裁剪，0 表示不限制。可避免请求超出上游额度或模型能力</span>
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
              <el-table-column label="最大输出" width="160">
                <template #default="{ row }">
                  <el-input-number v-model="row.max_output_tokens" :min="0" :max="2000000" />
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
        <el-divider class="drawer-section">智能路由配置</el-divider>
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
          <el-select v-model="form.premium_models" multiple filterable allow-create placeholder="选择此账号的高端模型（可手动输入）">
            <el-option v-for="model in premiumModelOptions" :key="model" :label="model" :value="model" />
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
        <div class="drawer-footer">
          <el-button @click="dialogVisible = false">取消</el-button>
          <el-button type="primary" @click="handleSubmit" :loading="submitting">确定</el-button>
        </div>
      </template>
    </el-drawer>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, computed, onUnmounted } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { adminApi, type BatchCreateUpstreamPayload } from '@/api/admin'
import type { ApiKeyModelConfig, UpstreamConfig } from '@/types'

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
  api_key_models: [],
  supported_models: [],
  default_model_context: {
    context_limit: 200000,
    output_reserve: 4096,
    max_output_tokens: 0,
    context_group: ''
  },
  active: true,
  model_request_costs: [],
  model_contexts: [],
  priority: 0,
  premium_models: [],
  protect_premium_quota: false,
  strip_nonstandard_chat_fields: false,
  failure_count: 0
})

const availableModelsForCost = computed(() => {
  const supported = form.value.supported_models || []
  return Array.from(new Set(supported)).sort()
})

const premiumModelOptions = computed(() => {
  const supported = form.value.supported_models || []
  const premium = form.value.premium_models || []
  const combined = [...supported, ...premium]
  return Array.from(new Set(combined)).sort()
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
    max_output_tokens: 0,
    context_group: ''
  })
}

const clearDefaultContextConfig = () => {
  if (!form.value.default_model_context) {
    form.value.default_model_context = {
      context_limit: 0,
      output_reserve: 0,
      max_output_tokens: 0,
      context_group: ''
    }
  } else {
    form.value.default_model_context.context_limit = 0
    form.value.default_model_context.output_reserve = 0
    form.value.default_model_context.max_output_tokens = 0
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

const isOfficialOpenAIBaseUrl = (baseUrl?: string) => {
  const value = String(baseUrl || '').trim().toLowerCase()
  return value.includes('://api.openai.com') || value.includes('.openai.azure.com')
}

const isAutoChatCompatibility = (value: UpstreamConfig) => {
  return displayProtocols(value).includes('ChatCompletions') && !isOfficialOpenAIBaseUrl(value.base_url)
}

const displayKeyCount = (value: UpstreamConfig) => {
  const keys = [
    value.api_key,
    ...(value.api_keys || []),
    ...(value.api_key_models || []).map(item => item.api_key)
  ]
    .map(key => String(key || '').trim())
    .filter(Boolean)

  return new Set(keys).size
}

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
    api_key_models: [],
    supported_models: [],
    default_model_context: {
      context_limit: 200000,
      output_reserve: 4096,
      max_output_tokens: 0,
      context_group: ''
    },
    active: true,
    model_request_costs: [],
    model_contexts: [],
    priority: 0,
    premium_models: [],
    protect_premium_quota: false,
    strip_nonstandard_chat_fields: false,
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
    api_key_models: [],
    supported_models: [...(row.supported_models || [])],
    default_model_context: row.default_model_context
      ? { ...row.default_model_context }
      : { context_limit: 200000, output_reserve: 4096, max_output_tokens: 0, context_group: '' },
    active: row.active,
    model_request_costs: row.model_request_costs ? [...row.model_request_costs] : [],
    model_contexts: row.model_contexts ? [...row.model_contexts] : [],
    priority: row.priority,
    premium_models: [...(row.premium_models || [])],
    protect_premium_quota: row.protect_premium_quota,
    strip_nonstandard_chat_fields: Boolean(row.strip_nonstandard_chat_fields),
    failure_count: 0
  }
  dialogVisible.value = true
}

const handleEdit = (row: UpstreamConfig) => {
  dialogMode.value = 'edit'
  contextConfigTab.value = 'default'
  clearDefaultContext.value = false
  const protocols = resolveProtocols(row)
  const allKeys = [
    row.api_key,
    ...(row.api_keys || []),
    ...(row.api_key_models || []).map(item => item.api_key)
  ]
    .map(key => String(key || '').trim())
    .filter((v, i, a) => a.indexOf(v) === i)
  form.value = {
    ...row,
    api_key: allKeys.join('\n'),
    api_keys: [...(row.api_keys || [])],
    api_key_models: (row.api_key_models || []).map((item: ApiKeyModelConfig) => ({
      api_key: item.api_key,
      supported_models: [...item.supported_models]
    })),
    protocol: protocols[0] as UpstreamConfig['protocol'],
    protocols,
    strip_nonstandard_chat_fields: Boolean(row.strip_nonstandard_chat_fields),
    default_model_context: row.default_model_context
      ? {
          ...row.default_model_context
        }
      : {
          context_limit: 200000,
          output_reserve: 4096,
          max_output_tokens: 0,
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
    delete submitData.requests_per_minute
    delete submitData.request_quota_window_hours
    delete submitData.request_quota_requests
    delete submitData.max_concurrency
    submitData.model_contexts = (submitData.model_contexts || [])
      .map((item: any) => ({
        slug: String(item.slug || '').trim(),
        context_limit: Number(item.context_limit || 0),
        output_reserve: Number(item.output_reserve || 0),
        max_output_tokens: Number(item.max_output_tokens || 0),
        context_group: String(item.context_group || '').trim()
      }))
      .filter(item => item.slug.length > 0 && item.context_limit > 0)
    if (submitData.default_model_context) {
      const context = submitData.default_model_context
      const context_limit = Number(context.context_limit || 0)
      const output_reserve = Number(context.output_reserve || 0)
      const max_output_tokens = Number(context.max_output_tokens || 0)
      const context_group = String(context.context_group || '').trim()
      if (context_limit > 0) {
        submitData.default_model_context = {
          context_limit,
          output_reserve,
          max_output_tokens,
          context_group
        }
      } else {
        submitData.default_model_context = {
          context_limit: 0,
          output_reserve: 0,
          max_output_tokens: 0,
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
    submitData.strip_nonstandard_chat_fields = Boolean(submitData.strip_nonstandard_chat_fields)
    
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
          active: submitData.active,
          strip_nonstandard_chat_fields: Boolean(submitData.strip_nonstandard_chat_fields)
        }

        const response = await adminApi.createUpstreamsBatch(batchPayload)
        const result = response.data

        const keysCount = result.keys_count || 0
        const failedKeys = result.failed || 0

        if (failedKeys > 0 && keysCount > 0) {
          ElMessage.success(`保存了 ${keysCount} 个有效 Key，${failedKeys} 个无效 Key 已剔除`)
        } else if (keysCount > 0) {
          ElMessage.success(`保存了 ${keysCount} 个有效 Key`)
        } else {
          const errors = result.results.filter(r => r.error).map(r => r.error).join("；")
          ElMessage.error(`所有 Key 均无效：${errors || "无法验证"}`)
        }
      }
    } else {
      const editKeys = (submitData.api_key || '')
        .split('\n')
        .map(k => k.trim())
        .filter(k => k.length > 0)
      if (editKeys.length > 0) {
        submitData.api_key = editKeys[0]
        submitData.api_keys = editKeys.slice(1)
        // 添加替换标志，让后端替换所有 key 而不是合并
        submitData._replace_api_keys = true
      } else {
        // 用户删除了所有 key，需要清空
        submitData.api_key = ''
        submitData.api_keys = []
        submitData.api_key_models = []
        submitData._replace_api_keys = true
      }
      await adminApi.updateUpstream(form.value.id!, submitData)
      const editTotalKeys = [submitData.api_key, ...(submitData.api_keys || [])].filter(Boolean).length
      if (editTotalKeys > 1) {
        ElMessage.success('更新成功，保存了 ' + editTotalKeys + ' 个 Key')
      } else {
        ElMessage.success('更新成功')
      }
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
    const response = await adminApi.discoverUpstreamModels({
      base_url: baseUrl,
      keys: apiKeys
    })
    const result = response.data

    if (!result.models || result.models.length === 0) {
      const errorDetails = (result.results || [])
        .filter(item => item.error)
        .map(item => item.error)
        .join('；')
      ElMessage.error(result.message || `所有 Key 获取模型均失败${errorDetails ? `：${errorDetails}` : ''}`)
      return
    }

    form.value.supported_models = [...result.models].sort()

    const successCount = (result.total || 0) - (result.failed || 0)
    const parts: string[] = ['成功获取 ' + result.models.length + ' 个模型']
    if (successCount > 1) {
      parts.push('用了 ' + successCount + ' 个 Key')
    }
    if (result.failed > 0) {
      parts.push(result.failed + ' 个 Key 获取失败')
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
.upstreams-page {
  min-height: 100%;
}

.protocol-cell {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  width: 100%;
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
  display: block;
  width: 100%;
  margin-top: 6px;
  color: var(--crc-text-muted);
  font-size: 12px;
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
  margin: 28px 0 20px;
}

.drawer-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

@media (max-width: 767px) {
  :global(.form-drawer .el-drawer__body) {
    padding: 18px 16px;
  }

  .model-input-group {
    flex-direction: column;
  }

  .fetch-btn {
    width: 100%;
  }
}
</style>
