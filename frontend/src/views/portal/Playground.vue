<template>
  <div class="playground-page">
    <el-card class="playground-card">
      <template #header>
        <div class="playground-header">
          <h2>模型操练场</h2>
          <el-text type="info">使用当前门户凭证实时验证模型可用性与返回内容</el-text>
        </div>
      </template>

      <el-alert
        v-if="statusMessage"
        :type="statusType"
        :closable="false"
        show-icon
        class="status-alert"
      >
        {{ statusMessage }}
      </el-alert>

      <el-form label-position="top" class="playground-form">
        <el-row :gutter="16">
          <el-col :xs="24" :sm="12" :md="12" :lg="8">
            <el-form-item label="模型（必选）">
              <el-select
                v-model="selectedModel"
                placeholder="先读取可用模型"
                filterable
                clearable
                style="width: 100%"
                :disabled="isBusy || !modelOptions.length"
              >
                <el-option
                  v-for="model in modelOptions"
                  :key="model"
                  :label="model"
                  :value="model"
                />
              </el-select>
            </el-form-item>
          </el-col>

          <el-col :xs="12" :sm="6" :md="6" :lg="4">
            <el-form-item label="温度">
              <el-input-number
                v-model="temperature"
                :min="0"
                :max="2"
                :step="0.1"
                :disabled="isBusy"
                controls-position="right"
                :precision="1"
                style="width: 100%"
              />
            </el-form-item>
          </el-col>

          <el-col :xs="12" :sm="6" :md="6" :lg="4">
            <el-form-item label="max_tokens">
              <el-input-number
                v-model="maxTokens"
                :min="1"
                :max="8192"
                :step="128"
                :disabled="isBusy"
                controls-position="right"
                style="width: 100%"
              />
            </el-form-item>
          </el-col>
        </el-row>

        <el-form-item label="问题">
          <el-input
            v-model="question"
            type="textarea"
            :rows="4"
            :maxlength="4000"
            show-word-limit
            placeholder="在这里输入你要测试的问题"
            :disabled="isBusy"
          />
        </el-form-item>

        <el-form-item label="上传文件（可选）">
          <div class="upload-control">
            <el-button type="primary" plain :disabled="isBusy" @click="openFileDialog">选择文件</el-button>
            <el-text size="small" type="info" class="upload-tip">支持文本文件内联提问；当前支持最大 1MB 文件。</el-text>
          </div>

          <input
            ref="fileInputRef"
            type="file"
            multiple
            class="hidden-file-input"
            @change="onFileInputChange"
          />

          <div class="upload-list" v-if="uploadedFiles.length">
            <div v-for="file in uploadedFiles" :key="file.uid" class="upload-item">
              <div>
                <div class="upload-name">{{ file.name }}</div>
                <div class="upload-meta">{{ formatFileSize(file.size) }} · {{ file.type || 'application/octet-stream' }}</div>
                <div class="upload-status" v-if="file.isError">解析失败：{{ file.errorMessage }}</div>
              </div>
              <el-button text size="small" type="danger" @click="removeUploadedFile(file.uid)">移除</el-button>
            </div>
          </div>
        </el-form-item>

        <el-form-item>
          <el-button
            type="primary"
            :loading="isSending"
            :disabled="sendDisabled"
            @click="sendQuestion"
          >
            发送测试请求
          </el-button>

          <el-button :disabled="isBusy" @click="clearConversation">清空记录</el-button>
          <el-button v-if="rawResponse" :disabled="isBusy" @click="copyResponse">复制原始响应</el-button>
        </el-form-item>
      </el-form>
    </el-card>

    <el-card class="history-card" v-loading="isSending">
      <template #header>
        <div class="history-head">
          <h3>会话记录</h3>
          <el-text size="small" type="info">当前密钥仅用于当前门户会话，不会持久保存</el-text>
        </div>
      </template>

      <div class="conversation-list" v-if="messages.length">
          <div
            v-for="(message, index) in messages"
            :key="`${message.role}-${index}`"
            :class="['conversation-item', `conversation-item--${message.role}`, message.isError ? 'conversation-item--error' : '']"
          >
            <div class="conversation-role">{{ message.role === 'user' ? '你' : '模型' }}</div>
            <pre class="conversation-content">{{ message.content }}</pre>
            <div class="conversation-file" v-if="message.uploadedFiles?.length">
              <div>已附加文件：</div>
              <ul class="conversation-file-list">
                <li v-for="(file, index) in message.uploadedFiles" :key="`${file.name}-${index}`">
                  {{ file.name }}（{{ formatFileSize(file.size) }}）
                </li>
              </ul>
            </div>
            <div class="conversation-meta" v-if="message.usageText">{{ message.usageText }}</div>
          </div>
      </div>
      <el-empty v-else description="发送问题后这里会显示对话记录" />
    </el-card>

    <el-card v-if="rawResponse" class="raw-card">
      <template #header>
        <h3>原始响应</h3>
      </template>
      <pre class="raw-code">{{ rawResponse }}</pre>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'
import { buildGatewayModelsEndpoint } from '@/utils/integration'
import {
  buildPlaygroundChatPayload,
  extractChatCompletionText,
  extractChatCompletionUsage,
  parseGatewayModels,
  type PlaygroundMessage,
  type UploadedFileContext
} from '@/utils/playground'

interface UploadedFile {
  uid: string
  name: string
  size: number
  type: string
  content: string
  isError: boolean
  errorMessage?: string
}

interface ConversationMessage {
  role: 'user' | 'assistant'
  content: string
  uploadedFiles?: UploadedFileContext[]
  usageText?: string
  isError?: boolean
}

const isSending = ref(false)
const isLoading = ref(true)
const question = ref('')
const selectedModel = ref('')
const temperature = ref(0.7)
const maxTokens = ref(1024)
const modelOptions = ref<string[]>([])
const downstreamKey = ref('')
const rawResponse = ref('')
const statusMessage = ref('')
const statusType = ref<'success' | 'info' | 'warning' | 'error'>('info')
const messages = ref<ConversationMessage[]>([])
const fileInputRef = ref<HTMLInputElement | null>(null)
const uploadedFiles = ref<UploadedFile[]>([])

const MAX_FILE_SIZE_BYTES = 1024 * 1024

const gatewayBaseUrl = computed(() => window.location.origin.replace(/\/+$/, ''))
const isBusy = computed(() => isSending.value || isLoading.value)

const sendDisabled = computed(() => {
  if (isBusy.value) return true
  if (!selectedModel.value) return true
  const hasText = Boolean(question.value.trim())
  const hasReadyFiles = uploadedFiles.value.some(file => !file.isError)
  if (!hasText && !hasReadyFiles) return true
  return false
})

const formatFileSize = (size: number) => {
  if (size < 1024) {
    return `${size} B`
  }

  if (size < 1024 * 1024) {
    return `${(size / 1024).toFixed(1)} KB`
  }

  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}

const safeGetText = async (response: Response) => {
  const text = await response.text()
  if (!text) {
    return `${response.status} ${response.statusText}`
  }

  try {
    const payload = JSON.parse(text) as { error?: { message?: string } }
    if (typeof payload?.error?.message === 'string' && payload.error.message.trim()) {
      return payload.error.message
    }
  } catch {
    // keep plain text
  }

  return text
}

const loadModels = async (authorizationKey: string) => {
  try {
    const response = await fetch(buildGatewayModelsEndpoint(gatewayBaseUrl.value), {
      headers: {
        Authorization: `Bearer ${authorizationKey}`
      }
    })

    if (response.ok) {
      const payload = await response.json()
      const models = parseGatewayModels(payload)
      if (models.length > 0) {
        modelOptions.value = models
        return
      }
    } else {
      const message = await safeGetText(response)
      throw new Error(`模型列表请求失败：${message}`)
    }
  } catch (error) {
    console.warn('通过 /v1/models 读取失败，准备回退到门户统计模型', error)
  }

  await fallbackToPortalModelStats()
}

const fallbackToPortalModelStats = async () => {
  try {
    const { data } = await portalApi.getModels()
    const fallback = [...new Set((data ?? []).map(item => item.model.trim()).filter(Boolean))]

    if (fallback.length) {
      modelOptions.value = fallback
      statusMessage.value = '网关 /v1/models 暂不可用，已使用 portal/model 统计模型兜底。'
      statusType.value = 'warning'
      return
    }

    throw new Error('无法读取任何模型信息')
  } catch {
    statusMessage.value = '当前未能读取任何模型，暂时无法发起测试请求。请先在门户管理员为当前 key 配置可用上游模型。'
    statusType.value = 'error'
    modelOptions.value = []
  }
}

const setStatus = (message: string, type: 'success' | 'info' | 'warning' | 'error' = 'info') => {
  statusMessage.value = message
  statusType.value = type
}

const buildUploadedPayload = (files: UploadedFile[]): UploadedFileContext[] => {
  return files
    .filter(file => !file.isError)
    .map(file => ({
      name: file.name,
      size: file.size,
      type: file.type || 'application/octet-stream',
      text: file.content
    }))
}

const toDisplayMessageContent = (questionText: string, files: UploadedFileContext[]) => {
  const trimmedQuestion = questionText.trim()
  if (!files.length) {
    return trimmedQuestion
  }

  if (!trimmedQuestion) {
    return '（仅基于附件提问）'
  }

  return trimmedQuestion
}

const toHistoryMessages = () => {
  return messages.value
    .filter(item => !item.isError)
    .map<PlaygroundMessage>(item => {
      if (item.role === 'user' && item.uploadedFiles?.length) {
        return {
          role: item.role,
          content: [
            ...item.uploadedFiles.map(file => ({
              type: 'text' as const,
              text: `【文件】${file.name} (${file.type || 'application/octet-stream'}, ${formatFileSize(file.size)})\n${file.text}`
            })),
            ...(item.content.trim() ? [{ type: 'text' as const, text: item.content.trim() }] : [])
          ]
        }
      }

      return {
        role: item.role,
        content: item.content
      }
    })
}

const openFileDialog = () => {
  if (isBusy.value) {
    return
  }

  fileInputRef.value?.click()
}

const removeUploadedFile = (uid: string) => {
  uploadedFiles.value = uploadedFiles.value.filter(file => file.uid !== uid)
}

const handleUploadedFileReadError = (file: File): UploadedFile => {
  return {
    uid: `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
    name: file.name,
    size: file.size,
    type: file.type,
    content: '',
    isError: true,
    errorMessage: '无法读取文件内容，请上传可读文本文件'
  }
}

const trimUploadedContent = (content: string) => {
  const trimmed = content.trim()
  const maxLength = 12000
  if (trimmed.length <= maxLength) {
    return trimmed
  }

  return `${trimmed.slice(0, maxLength)}\n\n[内容已截断，文件原始长度 ${trimmed.length} 字符]`
}

const onFileInputChange = async (event: Event) => {
  const target = event.target as HTMLInputElement
  const files = Array.from(target.files ?? [])
  if (!files.length) {
    return
  }

  const newUploads = await Promise.all(
    files.map(async file => {
      if (file.size > MAX_FILE_SIZE_BYTES) {
        return {
          uid: `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
          name: file.name,
          size: file.size,
          type: file.type,
          content: '',
          isError: true,
          errorMessage: `文件超出限制，最大支持 ${formatFileSize(MAX_FILE_SIZE_BYTES)}。`
        }
      }

      try {
        const text = trimUploadedContent(await file.text())
        return {
          uid: `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
          name: file.name,
          size: file.size,
          type: file.type,
          content: text,
          isError: false
        }
      } catch {
        return handleUploadedFileReadError(file)
      }
    })
  )

  uploadedFiles.value = [...uploadedFiles.value, ...newUploads]
  if (target.value) {
    target.value = ''
  }
}

const formatUsage = (usage: ReturnType<typeof extractChatCompletionUsage>) => {
  if (!usage) return undefined
  return `tokens: in=${usage.prompt_tokens} out=${usage.completion_tokens} total=${usage.total_tokens}`
}

const sendQuestion = async () => {
  if (sendDisabled.value) {
    return
  }

  const prompt = question.value.trim()
  const uploadedPayload = buildUploadedPayload(uploadedFiles.value)
  const requestPrompt = prompt || '请基于上述附件内容作答。'
  const requestKey = downstreamKey.value
  if (!requestKey) {
    setStatus('未找到门户 key，可在“秘钥管理”查看后重试。', 'error')
    return
  }

  isSending.value = true
  statusMessage.value = ''
  const userMessage = toDisplayMessageContent(prompt, uploadedPayload)
  const history = toHistoryMessages()
  const pendingUploads = [...uploadedFiles.value]

  messages.value.push({
    role: 'user',
    content: userMessage,
    uploadedFiles: uploadedPayload
  })
  question.value = ''

  try {
    const payload = buildPlaygroundChatPayload({
      model: selectedModel.value,
      question: requestPrompt,
      history,
      temperature: temperature.value,
      maxTokens: maxTokens.value,
      uploadedFiles: uploadedPayload
    })

    const response = await fetch(`${gatewayBaseUrl.value}/v1/chat/completions`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: `Bearer ${requestKey}`
      },
      body: JSON.stringify(payload)
    })

    if (!response.ok) {
      const message = await safeGetText(response)
      throw new Error(`请求失败：${message}`)
    }

    const json = await response.json()
    rawResponse.value = JSON.stringify(json, null, 2)
    const content = extractChatCompletionText(json)
    const usage = extractChatCompletionUsage(json)

    messages.value.push({
      role: 'assistant',
      content: content || '（模型返回空内容）',
      usageText: formatUsage(usage)
    })

    setStatus('请求已完成', 'success')
  } catch (error) {
    uploadedFiles.value = pendingUploads
    const message = error instanceof Error ? error.message : '未知错误'
    messages.value.push({
      role: 'assistant',
      content: message,
      isError: true
    })
    setStatus(message, 'error')
    rawResponse.value = ''
  } finally {
    isSending.value = false
  }
}

const clearConversation = () => {
  messages.value = []
  uploadedFiles.value = []
  rawResponse.value = ''
  statusMessage.value = ''
  statusType.value = 'info'
}

const copyResponse = async () => {
  if (!rawResponse.value) {
    ElMessage.warning('当前没有可复制的原始响应')
    return
  }

  try {
    await navigator.clipboard.writeText(rawResponse.value)
    ElMessage.success('已复制原始响应')
  } catch {
    const fallbackInput = document.createElement('textarea')
    fallbackInput.value = rawResponse.value
    fallbackInput.style.position = 'fixed'
    fallbackInput.style.opacity = '0'
    document.body.appendChild(fallbackInput)
    fallbackInput.select()

    try {
      document.execCommand('copy')
      ElMessage.success('已复制原始响应')
    } catch {
      ElMessage.error('复制失败，请手动复制')
    } finally {
      document.body.removeChild(fallbackInput)
    }
  }
}

const loadInitialData = async () => {
  isLoading.value = true

  let portalDownstreamKey = ''
  try {
    const { data } = await portalApi.getKey()
    portalDownstreamKey = (data.plaintext_key ?? '').trim()
  } catch {
    setStatus('读取门户 key 失败，请重新登录门户', 'error')
    isLoading.value = false
    return
  }

  if (!portalDownstreamKey) {
    setStatus('当前门户没有可用 key，不能进行模型测试', 'error')
    isLoading.value = false
    return
  }

  downstreamKey.value = portalDownstreamKey
  await loadModels(portalDownstreamKey)

  if (modelOptions.value.length > 0) {
    selectedModel.value = modelOptions.value[0]
    if (!statusMessage.value) {
      setStatus('已就绪，可开始提问测试。', 'success')
    }
  }

  isLoading.value = false
}

onMounted(() => {
  void loadInitialData()
})
</script>

<style scoped>
.playground-page {
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  background: linear-gradient(180deg, #f8fbff 0%, #f4f7fb 100%);
  min-height: 100%;
}

.playground-card,
.history-card,
.raw-card {
  border-radius: 14px;
}

.playground-header {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.playground-header h2 {
  margin: 0;
  font-size: 20px;
  color: #1f2d3d;
}

.status-alert {
  margin-bottom: 16px;
}

.playground-form {
  display: flex;
  flex-direction: column;
  gap: 0;
}

.history-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}

.history-head h3 {
  margin: 0;
  font-size: 16px;
}

.conversation-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.conversation-item {
  border: 1px solid #e4e7ed;
  border-radius: 12px;
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  background: #fff;
}

.conversation-item--user {
  border-color: #d9ecff;
  background: #f0f8ff;
}

.conversation-item--assistant {
  border-color: #ecf5e8;
  background: #f8fff3;
}

.conversation-item--error {
  border-color: #fde2e2;
  background: #fff5f5;
}

.conversation-role {
  font-weight: 600;
  color: #1f2d3d;
}

.conversation-content {
  margin: 0;
  white-space: pre-wrap;
  line-height: 1.6;
  color: #303133;
}

.upload-control {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.upload-tip {
  margin-top: 4px;
}

.hidden-file-input {
  position: absolute;
  opacity: 0;
  width: 0;
  height: 0;
  pointer-events: none;
}

.upload-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-top: 8px;
}

.upload-item {
  border: 1px dashed #dcdfe6;
  border-radius: 8px;
  padding: 8px 10px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.upload-name {
  font-size: 13px;
  color: #1f2d3d;
  font-weight: 600;
}

.upload-meta,
.upload-status {
  font-size: 12px;
  color: #909399;
  margin-top: 2px;
}

.upload-status {
  color: #e6a23c;
}

.conversation-file {
  border-left: 3px solid #d9ecff;
  padding-left: 8px;
  color: #606266;
  font-size: 12px;
  line-height: 1.6;
}

.conversation-file-list {
  margin: 0;
  padding-left: 18px;
}

.conversation-meta {
  color: #909399;
  font-size: 12px;
}

.raw-code {
  margin: 0;
  border-radius: 10px;
  background: #0b1220;
  color: #e5e7eb;
  padding: 14px;
  max-height: 360px;
  overflow: auto;
  line-height: 1.6;
  font-family:
    'SFMono-Regular',
    Consolas,
    'Liberation Mono',
    Menlo,
    monospace;
  font-size: 12px;
}

@media (max-width: 768px) {
  .playground-page {
    padding: 12px;
    gap: 12px;
  }

  .playground-form :deep(.el-col) {
    width: 100% !important;
  }
}
</style>
