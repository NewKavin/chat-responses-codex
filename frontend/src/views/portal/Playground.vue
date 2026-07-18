<template>
  <div class="playground-workspace">
    <aside :class="['settings-panel', { 'settings-panel--collapsed': sidebarCollapsed }]">
      <div class="settings-panel__toggle">
        <el-tooltip :content="sidebarCollapsed ? '展开模型设置' : '收起模型设置'" placement="right">
          <el-button
            :aria-label="sidebarCollapsed ? '展开模型设置' : '收起模型设置'"
            circle
            @click="sidebarCollapsed = !sidebarCollapsed"
          >
            <el-icon><Expand v-if="sidebarCollapsed" /><Fold v-else /></el-icon>
          </el-button>
        </el-tooltip>
      </div>

      <div v-show="!sidebarCollapsed" class="settings-panel__body">
        <PlaygroundSettings
          :model-options="modelOptions"
          :selected-model="selectedModel"
          :busy="isBusy"
          :status-message="statusMessage"
          :status-type="statusType"
          :temperature="temperature"
          :temperature-enabled="temperatureEnabled"
          :max-tokens="maxTokens"
          :max-tokens-enabled="maxTokensEnabled"
          :inference-strength="inferenceStrength"
          :inference-strength-options="inferenceStrengthOptions"
          :inference-strength-enabled="inferenceStrengthEnabled"
          @clear="clearConversation"
          @update:selected-model="selectedModel = $event"
          @update:temperature="temperature = $event"
          @update:temperature-enabled="temperatureEnabled = $event"
          @update:max-tokens="maxTokens = $event"
          @update:max-tokens-enabled="maxTokensEnabled = $event"
          @update:inference-strength="inferenceStrength = $event as (typeof inferenceStrengthOptions)[number]"
          @update:inference-strength-enabled="inferenceStrengthEnabled = $event"
        />
      </div>
    </aside>

    <div class="chat-area">
      <div class="chat-toolbar">
        <div>
          <h1 class="chat-toolbar__title">模型操练场</h1>
          <p class="chat-toolbar__subtitle">选择模型、上传附件并观察流式响应。</p>
        </div>
        <el-button
          class="chat-toolbar__settings-trigger"
          aria-label="打开模型设置"
          circle
          @click="settingsDrawerOpen = true"
        >
          <el-icon><Setting /></el-icon>
        </el-button>
      </div>

      <div class="playground-message-stream" ref="messagesContainerRef">
        <div v-if="!messages.length" class="chat-empty">
          <div class="chat-empty-icon">
            <el-icon :size="42"><ChatDotRound /></el-icon>
          </div>
          <p>选择模型后开始对话</p>
        </div>

        <div
          v-for="(message, index) in messages"
          :key="`${message.role}-${index}`"
          :class="[
            'chat-message',
            `chat-message--${message.role}`,
            message.isError ? 'chat-message--error' : '',
            message.isEmptyResponse ? 'chat-message--empty-response' : ''
          ]"
        >
          <div class="chat-message-avatar">
            <el-icon v-if="message.role === 'user'" :size="20"><User /></el-icon>
            <el-icon v-else :size="20"><MagicStick /></el-icon>
          </div>
          <div class="chat-message-body">
            <details v-if="message.reasoning" class="message-reasoning" open>
              <summary class="message-reasoning__summary">
                <el-icon :size="14"><MagicStick /></el-icon>
                <span>思考过程</span>
              </summary>
              <div class="message-reasoning__content markdown-body" v-html="renderMarkdown(message.reasoning)"></div>
            </details>
            <div v-if="message.role === 'assistant' && !message.isError" class="chat-message-content markdown-body" v-html="renderMarkdown(message.content)"></div>
            <pre v-else class="chat-message-content chat-message-content--plain">{{ message.content }}</pre>
            <div class="chat-message-file" v-if="message.uploadedFiles?.length">
              <span v-for="file in message.uploadedFiles" :key="file.name" class="file-tag">
                {{ file.name }}
              </span>
            </div>
            <div class="chat-message-meta" v-if="message.usageText">{{ message.usageText }}</div>
          </div>
        </div>

        <div v-if="isSending" class="chat-message chat-message--assistant">
          <div class="chat-message-avatar">
            <el-icon :size="20"><MagicStick /></el-icon>
          </div>
          <div class="chat-message-body">
            <div v-if="streamStatusText" class="chat-stream-status">
              {{ streamStatusText }}
            </div>
            <details v-if="streamingReasoning" class="message-reasoning" open>
              <summary class="message-reasoning__summary">
                <el-icon :size="14"><MagicStick /></el-icon>
                <span>思考中…</span>
              </summary>
              <div class="message-reasoning__content markdown-body" v-html="renderMarkdown(streamingReasoning)"></div>
            </details>
            <div v-if="streamingContent" class="chat-message-content markdown-body" v-html="renderMarkdown(streamingContent)"></div>
            <span class="typing-cursor"></span>
          </div>
        </div>
      </div>

      <section class="playground-composer">
        <div class="composer-shell">
          <div v-if="uploadedFiles.length" class="upload-inline-list">
            <span v-for="file in uploadedFiles" :key="file.uid" class="upload-inline-tag">
              {{ file.name }}
              <el-icon :size="12" class="upload-inline-remove" @click="removeUploadedFile(file.uid)"><Close /></el-icon>
            </span>
          </div>

          <div class="composer-input-row">
            <el-input
              v-model="question"
              type="textarea"
              :autosize="{ minRows: 1, maxRows: 6 }"
              :maxlength="4000"
              placeholder="输入消息..."
              :disabled="isBusy"
              @keydown="handleInputKeydown"
            />
            <div class="composer-actions">
              <el-tooltip content="添加附件" placement="top">
                <el-button
                  aria-label="添加附件"
                  circle
                  :disabled="isBusy"
                  @click="openFileDialog"
                >
                  <el-icon :size="16"><Link /></el-icon>
                </el-button>
              </el-tooltip>
              <el-tooltip content="发送消息" placement="top">
                <el-button
                  aria-label="发送消息"
                  type="primary"
                  circle
                  :loading="isSending"
                  :disabled="sendDisabled"
                  @click="sendQuestion"
                  class="send-button"
                >
                  <el-icon v-if="!isSending" :size="18"><Promotion /></el-icon>
                </el-button>
              </el-tooltip>
              <input
                ref="fileInputRef"
                type="file"
                multiple
                class="hidden-file-input"
                @change="onFileInputChange"
              />
            </div>
          </div>
        </div>
      </section>
    </div>

    <el-drawer
      v-model="settingsDrawerOpen"
      append-to-body
      title="模型设置"
      size="min(360px, 100vw)"
      class="playground-settings-drawer"
    >
      <PlaygroundSettings
        :model-options="modelOptions"
        :selected-model="selectedModel"
        :busy="isBusy"
        :status-message="statusMessage"
        :status-type="statusType"
        :temperature="temperature"
        :temperature-enabled="temperatureEnabled"
        :max-tokens="maxTokens"
        :max-tokens-enabled="maxTokensEnabled"
        :inference-strength="inferenceStrength"
        :inference-strength-options="inferenceStrengthOptions"
        :inference-strength-enabled="inferenceStrengthEnabled"
        @clear="clearConversation(); settingsDrawerOpen = false"
        @update:selected-model="selectedModel = $event; settingsDrawerOpen = false"
        @update:temperature="temperature = $event"
        @update:temperature-enabled="temperatureEnabled = $event"
        @update:max-tokens="maxTokens = $event"
        @update:max-tokens-enabled="maxTokensEnabled = $event"
        @update:inference-strength="inferenceStrength = $event as (typeof inferenceStrengthOptions)[number]"
        @update:inference-strength-enabled="inferenceStrengthEnabled = $event"
      />
    </el-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import {
  ChatDotRound,
  Close,
  Expand,
  Fold,
  Link,
  MagicStick,
  Promotion,
  Setting,
  User
} from '@element-plus/icons-vue'
import { Marked } from 'marked'
import { portalApi } from '@/api/portal'
import PlaygroundSettings from '@/components/PlaygroundSettings.vue'
import { buildGatewayModelsEndpoint } from '@/utils/integration'
import { createHighlightedCodeRenderer } from '@/utils/highlight'
import { extractReadableErrorMessage } from '@/utils/errorDisplay'
import {
  buildPlaygroundAssistantResult,
  buildPlaygroundChatPayload,
  classifyPlaygroundAttachment,
  extractChatCompletionText,
  extractChatCompletionUsage,
  formatPlaygroundStreamStatus,
  inferenceStrengthOptions,
  parseSSELine,
  selectPlayableModels,
  type PlaygroundMessage,
  type PlaygroundStreamPhase,
  type UploadedFileContext
} from '@/utils/playground'

const marked = new Marked({
  renderer: {
    code: createHighlightedCodeRenderer()
  }
})

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
  reasoning?: string
  isError?: boolean
  isEmptyResponse?: boolean
}

const isSending = ref(false)
const isLoading = ref(true)
const question = ref('')
const selectedModel = ref('')
const temperature = ref(0.7)
const maxTokens = ref(16384)
const inferenceStrength = ref<(typeof inferenceStrengthOptions)[number]>('high')
const temperatureEnabled = ref(false)
const maxTokensEnabled = ref(false)
const inferenceStrengthEnabled = ref(false)
const modelOptions = ref<string[]>([])
const downstreamKey = ref('')
const statusMessage = ref('')
const statusType = ref<'success' | 'info' | 'warning' | 'error'>('info')
const messages = ref<ConversationMessage[]>([])
const fileInputRef = ref<HTMLInputElement | null>(null)
const uploadedFiles = ref<UploadedFile[]>([])
const messagesContainerRef = ref<HTMLElement | null>(null)
const sidebarCollapsed = ref(false)
const settingsDrawerOpen = ref(false)
const streamingContent = ref('')
const streamingReasoning = ref('')
const firstOutputSeconds = ref<number | undefined>(undefined)
const streamPhase = ref<PlaygroundStreamPhase>('connecting')
const streamElapsedSeconds = ref(0)
const streamKeepaliveCount = ref(0)
let streamStartedAt = 0
let streamTimer: number | undefined

const MAX_FILE_SIZE_BYTES = 1024 * 1024

const gatewayBaseUrl = computed(() => window.location.origin.replace(/\/+$/, ''))
const isBusy = computed(() => isSending.value || isLoading.value)
const streamStatusText = computed(() => {
  if (!isSending.value) return ''
  return formatPlaygroundStreamStatus({
    phase: streamPhase.value,
    elapsedSeconds: streamElapsedSeconds.value,
    keepaliveCount: streamKeepaliveCount.value
  })
})

const sendDisabled = computed(() => {
  if (isBusy.value) return true
  if (!selectedModel.value) return true
  const hasText = Boolean(question.value.trim())
  const hasReadyFiles = uploadedFiles.value.some(file => !file.isError)
  if (!hasText && !hasReadyFiles) return true
  return false
})

const renderMarkdown = (text: string): string => {
  if (!text) return ''
  return marked.parse(text, { async: false }) as string
}

const scrollToBottom = () => {
  nextTick(() => {
    const container = messagesContainerRef.value
    if (container) {
      container.scrollTop = container.scrollHeight
    }
  })
}

watch(() => messages.value.length, scrollToBottom)
watch(streamingContent, scrollToBottom)
watch(streamingReasoning, scrollToBottom)

const startStreamTimer = () => {
  stopStreamTimer()
  streamStartedAt = Date.now()
  streamElapsedSeconds.value = 0
  streamKeepaliveCount.value = 0
  streamPhase.value = 'connecting'
  firstOutputSeconds.value = undefined
  streamTimer = window.setInterval(() => {
    streamElapsedSeconds.value = Math.floor((Date.now() - streamStartedAt) / 1000)
  }, 1000)
}

const stopStreamTimer = () => {
  if (streamTimer === undefined) return
  window.clearInterval(streamTimer)
  streamTimer = undefined
}

const markFirstOutput = () => {
  if (firstOutputSeconds.value !== undefined) return
  firstOutputSeconds.value = Math.max(0, Math.floor((Date.now() - streamStartedAt) / 1000))
}

const getFinalElapsedSeconds = () => Math.max(0, Math.floor((Date.now() - streamStartedAt) / 1000))

const formatFileSize = (size: number) => {
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`
  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}

const safeGetText = async (response: Response) => {
  const text = await response.text()
  return extractReadableErrorMessage({
    status: response.status,
    statusText: response.statusText,
    bodyText: text,
    fallback: '请求失败'
  })
}

const loadModels = async () => {
  const allowlist = await fetchPortalModelAllowlist()
  const response = await fetch(buildGatewayModelsEndpoint(gatewayBaseUrl.value), {
    headers: { Authorization: `Bearer ${downstreamKey.value}` }
  })
  if (!response.ok) throw new Error(await safeGetText(response))
  modelOptions.value = selectPlayableModels(allowlist, await response.json())
  if (modelOptions.value.length === 0) {
    throw new Error('当前下游没有可路由模型')
  }
  setStatus('实时模型列表已加载', 'success')
}

const fetchPortalModelAllowlist = async (): Promise<string[]> => {
  try {
    const { data } = await portalApi.getQuota()
    const allowlist = (data.model_allowlist ?? []).map(s => s.trim()).filter(Boolean)
    return [...new Set(allowlist)]
  } catch {
    return []
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
  if (!files.length) return trimmedQuestion
  if (!trimmedQuestion) return '（仅基于附件提问）'
  return trimmedQuestion
}

const toHistoryMessages = (): PlaygroundMessage[] => {
  return messages.value
    .filter(item => !item.isError)
    .map(item => {
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
      return { role: item.role, content: item.content }
    })
}

const openFileDialog = () => {
  if (isBusy.value) return
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
    errorMessage: '无法读取文件内容'
  }
}

const trimUploadedContent = (content: string) => {
  const trimmed = content.trim()
  const maxLength = 12000
  if (trimmed.length <= maxLength) return trimmed
  return `${trimmed.slice(0, maxLength)}\n\n[内容已截断，文件原始长度 ${trimmed.length} 字符]`
}

const onFileInputChange = async (event: Event) => {
  const target = event.target as HTMLInputElement
  const files = Array.from(target.files ?? [])
  if (!files.length) return

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
      const classification = classifyPlaygroundAttachment(file.name, file.type)
      if (!classification.accepted) {
        return {
          uid: `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
          name: file.name,
          size: file.size,
          type: file.type,
          content: '',
          isError: true,
          errorMessage: classification.message
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
  if (target.value) target.value = ''
}

const formatStreamError = (chunk: NonNullable<ReturnType<typeof parseSSELine>>) => {
  const details = [chunk.errorCategory, chunk.errorCode].filter(Boolean).join(' / ')
  if (!details) return chunk.errorMessage || '流式响应返回错误'
  return `${chunk.errorMessage || '流式响应返回错误'}（${details}）`
}

const handleInputKeydown = (event: KeyboardEvent) => {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault()
    sendQuestion()
  }
}

const sendQuestion = async () => {
  if (sendDisabled.value) return

  const prompt = question.value.trim()
  const uploadedPayload = buildUploadedPayload(uploadedFiles.value)
  const requestPrompt = prompt || '请基于上述附件内容作答。'
  const requestKey = downstreamKey.value
  if (!requestKey) {
    setStatus('未找到门户 key', 'error')
    return
  }

  isSending.value = true
  startStreamTimer()
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
  uploadedFiles.value = []
  streamingContent.value = ''
  streamingReasoning.value = ''

  try {
    const payload = buildPlaygroundChatPayload({
      model: selectedModel.value,
      question: requestPrompt,
      history,
      temperature: temperatureEnabled.value ? temperature.value : undefined,
      maxTokens: maxTokensEnabled.value ? maxTokens.value : undefined,
      inferenceStrength: inferenceStrengthEnabled.value ? inferenceStrength.value : undefined,
      uploadedFiles: uploadedPayload,
      stream: true
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

    const contentType = response.headers.get('content-type') || ''
    let finalContent = ''
    let finalUsage: ReturnType<typeof extractChatCompletionUsage> = null

    if (contentType.includes('text/event-stream') || contentType.includes('application/octet-stream')) {
      const reader = response.body?.getReader()
      if (!reader) throw new Error('无法读取流式响应')

      const decoder = new TextDecoder()
      let buffer = ''
      streamPhase.value = 'waiting'

      while (true) {
        const { done, value } = await reader.read()
        if (done) break

        buffer += decoder.decode(value, { stream: true })
        const lines = buffer.split('\n')
        buffer = lines.pop() || ''

        for (const line of lines) {
          const chunk = parseSSELine(line)
          if (!chunk) continue
          if (chunk.errorMessage) {
            throw new Error(formatStreamError(chunk))
          }
          if (chunk.done) continue
          if (chunk.keepalive) {
            streamKeepaliveCount.value += 1
            streamPhase.value = 'waiting'
            continue
          }
          if (chunk.reasoningContent) {
            markFirstOutput()
            streamPhase.value = 'thinking'
            streamingReasoning.value += chunk.reasoningContent
          }
          if (chunk.content) {
            markFirstOutput()
            streamPhase.value = 'generating'
            streamingContent.value += chunk.content
            finalContent = streamingContent.value
          }
          if (chunk.usage) {
            finalUsage = chunk.usage
          }
        }
      }

      for (const line of buffer.split('\n')) {
        const chunk = parseSSELine(line)
        if (!chunk) continue
        if (chunk.errorMessage) {
          throw new Error(formatStreamError(chunk))
        }
        if (chunk.keepalive || chunk.done) {
          continue
        }
        if (chunk.reasoningContent) {
          markFirstOutput()
          streamPhase.value = 'thinking'
          streamingReasoning.value += chunk.reasoningContent
        }
        if (chunk.content) {
          markFirstOutput()
          streamPhase.value = 'generating'
          streamingContent.value += chunk.content
          finalContent = streamingContent.value
        }
        if (chunk.usage) {
          finalUsage = chunk.usage
        }
      }
    } else {
      const json = await response.json()
      markFirstOutput()
      finalContent = extractChatCompletionText(json)
      finalUsage = extractChatCompletionUsage(json)
    }

    const finalReasoning = streamingReasoning.value
    if (!finalContent.trim() && !finalReasoning.trim()) {
      throw new Error('模型返回空响应，请更换模型或检查上游兼容性')
    }
    const assistantResult = buildPlaygroundAssistantResult({
      model: selectedModel.value,
      content: finalContent,
      reasoning: finalReasoning,
      usage: finalUsage,
      elapsedSeconds: getFinalElapsedSeconds(),
      firstOutputSeconds: firstOutputSeconds.value
    })

    streamingContent.value = ''
    streamingReasoning.value = ''
    messages.value.push({
      role: 'assistant',
      ...assistantResult
    })

    setStatus('请求已完成', 'success')
  } catch (error) {
    uploadedFiles.value = pendingUploads
    const message = error instanceof Error ? error.message : '未知错误'
    streamingContent.value = ''
    streamingReasoning.value = ''
    messages.value.push({
      role: 'assistant',
      content: message,
      isError: true
    })
    setStatus(message, 'error')
  } finally {
    stopStreamTimer()
    isSending.value = false
  }
}

const clearConversation = () => {
  messages.value = []
  uploadedFiles.value = []
  streamingContent.value = ''
  streamingReasoning.value = ''
  streamPhase.value = 'connecting'
  streamElapsedSeconds.value = 0
  streamKeepaliveCount.value = 0
  firstOutputSeconds.value = undefined
  statusMessage.value = ''
  statusType.value = 'info'
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
    setStatus('当前门户没有可用 key', 'error')
    isLoading.value = false
    return
  }

  downstreamKey.value = portalDownstreamKey
  try {
    await loadModels()
  } catch (error) {
    selectedModel.value = ''
    modelOptions.value = []
    const message = error instanceof Error ? error.message : '读取实时模型列表失败'
    setStatus(message, 'error')
    isLoading.value = false
    return
  }

  if (modelOptions.value.length > 0) {
    selectedModel.value = modelOptions.value[0]
    if (!statusMessage.value) {
      setStatus('已就绪', 'success')
    }
  }

  isLoading.value = false
}

onMounted(() => {
  void loadInitialData()
})

onBeforeUnmount(() => {
  stopStreamTimer()
})
</script>

<style scoped>
.playground-workspace {
  display: flex;
  width: 100%;
  height: calc(100dvh - var(--crc-topbar-height) - 48px);
  min-height: 560px;
  overflow: hidden;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.settings-panel {
  position: relative;
  display: flex;
  flex: 0 0 280px;
  width: 280px;
  min-width: 280px;
  flex-direction: column;
  border-right: 1px solid var(--crc-border);
  background: var(--crc-surface-muted);
  transition: width 160ms ease, min-width 160ms ease, flex-basis 160ms ease;
}

.settings-panel--collapsed {
  flex-basis: 48px;
  width: 48px;
  min-width: 48px;
}

.settings-panel__toggle {
  display: flex;
  justify-content: flex-end;
  padding: 10px;
}

.settings-panel__toggle .el-button {
  width: 36px;
  height: 36px;
}

.settings-panel__body {
  flex: 1;
  min-height: 0;
  padding: 8px 16px 16px;
  overflow-y: auto;
}

.chat-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  min-height: 64px;
  padding: 10px 20px;
  border-bottom: 1px solid var(--crc-border);
  background: var(--crc-surface);
}

.chat-toolbar__title {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 15px;
  line-height: 1.4;
}

.chat-toolbar__subtitle {
  margin: 3px 0 0;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.chat-toolbar__settings-trigger {
  display: none;
  width: 36px;
  height: 36px;
  flex: 0 0 36px;
}

:global(.playground-settings-drawer .el-drawer__body) {
  padding: 16px;
  background: var(--crc-surface-muted);
}

.hidden-file-input {
  position: absolute;
  opacity: 0;
  width: 0;
  height: 0;
  pointer-events: none;
}

.chat-area {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
  overflow: hidden;
}

.playground-message-stream {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-height: 0;
  padding: 20px 24px;
  overflow-y: auto;
  background: var(--crc-canvas);
}

.chat-empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  color: var(--crc-text-muted);
  gap: 12px;
}

.chat-empty-icon {
  display: grid;
  width: 76px;
  height: 76px;
  place-items: center;
  border-radius: 50%;
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
  animation: chat-empty-float 3.2s ease-in-out infinite;
}

@keyframes chat-empty-float {
  0%,
  100% {
    transform: translateY(0);
  }
  50% {
    transform: translateY(-6px);
  }
}

.chat-empty p {
  margin: 0;
  font-size: 14px;
}

.chat-message {
  display: flex;
  gap: 12px;
  max-width: min(85%, 920px);
  min-width: 0;
  animation: chat-message-in var(--crc-duration) var(--crc-ease-out) both;
}

@keyframes chat-message-in {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.chat-message--user {
  align-self: flex-end;
  flex-direction: row-reverse;
}

.chat-message--assistant {
  align-self: flex-start;
}

.chat-message--error {
  align-self: flex-start;
}

.chat-message-avatar {
  width: 32px;
  height: 32px;
  min-width: 32px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--crc-text-muted);
  background: var(--crc-surface-muted);
  margin-top: 2px;
}

.chat-message--user .chat-message-avatar {
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

.chat-message--assistant .chat-message-avatar {
  color: var(--crc-success);
  background: var(--crc-success-soft);
}

.chat-message-body {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.chat-stream-status {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  color: var(--crc-text-muted);
  font-size: 12px;
  line-height: 1.5;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-surface-muted);
  padding: 4px 8px;
  width: fit-content;
  max-width: 100%;
}


.message-reasoning {
  margin: 0 0 10px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-surface-muted);
  overflow: hidden;
}

.message-reasoning__summary {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 12px;
  cursor: pointer;
  color: var(--crc-text);
  font-size: 13px;
  user-select: none;
}

.message-reasoning__content {
  padding: 4px 12px 12px;
  border-top: 1px dashed var(--crc-border);
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.7;
}

.chat-message-content {
  min-width: 0;
  margin: 0;
  color: var(--crc-text);
  overflow-wrap: anywhere;
  line-height: 1.7;
  font-size: 14px;
}

.chat-message-content--plain {
  white-space: pre-wrap;
  font-family: inherit;
}

.chat-message--user .chat-message-content {
  padding: 10px 14px;
  border: 1px solid transparent;
  border-radius: var(--crc-radius) var(--crc-radius) 2px var(--crc-radius);
  color: #ffffff;
  background: var(--crc-accent);
  white-space: pre-wrap;
}

html.dark .chat-message--user .chat-message-content {
  color: #07211b;
}

.chat-message--assistant .chat-message-content {
  padding: 10px 14px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius) var(--crc-radius) var(--crc-radius) 2px;
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.chat-message--error .chat-message-content {
  padding: 10px 14px;
  border: 1px solid var(--crc-danger);
  border-radius: var(--crc-radius) var(--crc-radius) var(--crc-radius) 2px;
  color: var(--crc-danger);
  background: var(--crc-danger-soft);
  white-space: pre-wrap;
}

.chat-message--empty-response .chat-message-content {
  border-color: var(--crc-warning);
  color: var(--crc-warning);
  background: var(--crc-warning-soft);
}

.chat-message-file {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
}

.file-tag {
  padding: 2px 6px;
  border-radius: var(--crc-radius-sm);
  color: var(--crc-info);
  background: var(--crc-info-soft);
  font-size: 11px;
}

.chat-message-meta {
  color: var(--crc-text-muted);
  font-size: 11px;
}

.typing-cursor {
  display: inline-block;
  width: 6px;
  height: 16px;
  border-radius: 2px;
  background: var(--crc-accent);
  margin-left: 2px;
  animation: blink 0.8s infinite;
  vertical-align: text-bottom;
}

@keyframes blink {
  0%, 50% { opacity: 1; }
  51%, 100% { opacity: 0; }
}

.playground-composer {
  padding: 12px 24px 16px;
  border-top: 1px solid var(--crc-border);
  background: var(--crc-surface);
  z-index: 5;
}

.composer-shell {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
  max-width: 800px;
  margin: 0 auto;
}

.composer-input-row {
  display: flex;
  align-items: flex-end;
  gap: 8px;
  min-width: 0;
}

.composer-input-row :deep(.el-textarea__inner) {
  border-radius: var(--crc-radius);
  padding: 10px 14px;
  resize: none;
  font-size: 14px;
  line-height: 1.5;
  transition: border-color var(--crc-duration-fast) var(--crc-ease),
    box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.composer-input-row :deep(.el-textarea__inner:focus) {
  box-shadow: 0 0 0 3px var(--crc-accent-soft);
}

.composer-input-row :deep(.el-textarea) {
  flex: 1;
  min-width: 0;
}

.send-button {
  min-width: 36px;
  min-height: 36px;
  width: 36px;
  height: 36px;
  flex-shrink: 0;
}

.composer-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 0 0 80px;
  justify-content: flex-end;
}

.composer-actions .el-button {
  width: 36px;
  height: 36px;
}

.upload-inline-list {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.upload-inline-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border-radius: var(--crc-radius-sm);
  color: var(--crc-info);
  background: var(--crc-info-soft);
  font-size: 12px;
  max-width: 200px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.upload-inline-remove {
  cursor: pointer;
  color: var(--crc-text-muted);
  flex-shrink: 0;
}

.upload-inline-remove:hover {
  color: var(--crc-danger);
}

.markdown-body {
  min-width: 0;
  overflow-wrap: anywhere;
}

.markdown-body :deep(pre) {
  max-width: 100%;
  margin: 8px 0;
  padding: 12px;
  overflow-x: auto;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-surface-muted);
}

.markdown-body :deep(code) {
  font-family: 'SFMono-Regular', Consolas, 'Liberation Mono', Menlo, monospace;
  font-size: 13px;
}

.markdown-body :deep(pre code) {
  background: none;
  padding: 0;
  color: var(--crc-text);
}

.markdown-body :deep(:not(pre) > code) {
  padding: 2px 6px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-accent);
  background: var(--crc-surface-muted);
  font-size: 0.9em;
}

.markdown-body :deep(p) {
  margin: 0 0 8px 0;
}

.markdown-body :deep(p:last-child) {
  margin-bottom: 0;
}

.markdown-body :deep(ul),
.markdown-body :deep(ol) {
  margin: 4px 0;
  padding-left: 20px;
}

.markdown-body :deep(blockquote) {
  margin: 8px 0;
  padding: 4px 12px;
  border-left: 3px solid var(--crc-border-strong);
  color: var(--crc-text-muted);
}

.markdown-body :deep(table) {
  display: block;
  max-width: 100%;
  overflow-x: auto;
  border-collapse: collapse;
  margin: 8px 0;
  width: 100%;
}

.markdown-body :deep(th),
.markdown-body :deep(td) {
  border: 1px solid var(--crc-border);
  padding: 6px 10px;
  text-align: left;
}

.markdown-body :deep(th) {
  background: var(--crc-surface-muted);
}

@media (max-width: 767px) {
  .playground-workspace {
    height: calc(100dvh - var(--crc-topbar-height) - 32px);
    min-height: 520px;
  }

  .settings-panel {
    display: none;
  }

  .chat-toolbar {
    min-height: 58px;
    padding: 8px 12px;
  }

  .chat-toolbar__subtitle {
    display: none;
  }

  .chat-toolbar__settings-trigger {
    display: inline-flex;
  }

  .playground-message-stream {
    padding: 12px;
  }

  .playground-composer {
    padding: 8px 12px 12px;
  }

  .chat-message {
    max-width: 95%;
  }

  .chat-message-avatar {
    width: 28px;
    height: 28px;
    min-width: 28px;
  }

  .composer-input-row {
    gap: 6px;
  }

  .composer-actions {
    flex-basis: 76px;
    gap: 4px;
  }
}
</style>
