<template>
  <div class="playground-workspace">
    <aside :class="['settings-panel', { 'settings-panel--collapsed': sidebarCollapsed }]">
      <div class="settings-panel__toggle">
        <el-tooltip :content="sidebarCollapsed ? '展开参数设置' : '收起参数设置'" placement="right">
          <el-button
            :aria-label="sidebarCollapsed ? '展开参数设置' : '收起参数设置'"
            circle
            @click="sidebarCollapsed = !sidebarCollapsed"
          >
            <PanelLeftOpen v-if="sidebarCollapsed" :size="15" :stroke-width="1.8" /><PanelLeftClose v-else :size="15" :stroke-width="1.8" />
          </el-button>
        </el-tooltip>
      </div>

      <div v-show="!sidebarCollapsed" class="settings-panel__body">
        <PlaygroundSettings
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
        <h1 class="chat-toolbar__title sr-only">模型操练场</h1>
        <div class="chat-toolbar__model">
          <el-select
            v-model="selectedModel"
            class="model-picker"
            filterable
            :disabled="isBusy || !modelOptions.length"
            :placeholder="isLoading ? '正在加载模型…' : '选择模型'"
            :loading="isLoading"
            popper-class="playground-model-popper"
          >
            <template #prefix>
              <Sparkles :size="15" :stroke-width="1.8" class="model-picker__spark" />
            </template>
            <el-option
              v-for="model in modelOptions"
              :key="model"
              :label="model"
              :value="model"
            />
          </el-select>
        </div>
        <div class="chat-toolbar__actions">
          <el-tooltip content="新建对话" placement="bottom">
            <el-button
              class="new-chat-button"
              :disabled="isBusy || !messages.length"
              plain
              @click="clearConversation"
            >
              <Plus :size="15" :stroke-width="1.8" />
              <span class="new-chat-button__label">新建对话</span>
            </el-button>
          </el-tooltip>
          <el-tooltip content="参数设置" placement="bottom">
            <el-button
              class="chat-toolbar__settings-trigger"
              aria-label="参数设置"
              circle
              @click="openSettings"
            >
              <Settings2 :size="15" :stroke-width="1.8" />
            </el-button>
          </el-tooltip>
        </div>
      </div>

      <div class="playground-message-stream" ref="messagesContainerRef">
        <div class="playground-message-stream__inner">
          <div v-if="!messages.length" class="chat-empty">
            <div class="chat-empty-icon">
              <MessageSquareText :size="38" :stroke-width="1.2" />
            </div>
            <h2 class="chat-empty__title">{{ modelOptions.length ? '开始对话' : '暂无可用的模型' }}</h2>
            <p class="chat-empty__sub">
              {{
                modelOptions.length
                  ? '选择下方模型，或在顶部挑选模型后开始对话'
                  : statusMessage || '当前下游没有可路由模型，请检查模型映射配置'
              }}
            </p>
            <div v-if="modelOptions.length" class="chat-empty__models">
              <button
                v-for="model in modelOptions.slice(0, 6)"
                :key="model"
                type="button"
                class="model-chip"
                :class="{ 'model-chip--active': model === selectedModel }"
                :disabled="isBusy"
                @click="selectedModel = model"
              >
                <Sparkles :size="13" :stroke-width="1.8" />
                <span>{{ model }}</span>
              </button>
            </div>
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
              <UserRound v-if="message.role === 'user'" :size="15" :stroke-width="1.8" />
              <Sparkles v-else :size="15" :stroke-width="1.8" />
            </div>
            <div class="chat-message-body">
              <div v-if="message.role === 'assistant' && message.model" class="chat-message-model">
                {{ message.model }}
              </div>
              <details v-if="message.reasoning" class="message-reasoning" open>
                <summary class="message-reasoning__summary">
                  <Sparkles :size="13" :stroke-width="1.8" />
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
              <Sparkles :size="15" :stroke-width="1.8" />
            </div>
            <div class="chat-message-body">
              <div v-if="selectedModel" class="chat-message-model">{{ selectedModel }}</div>
              <div v-if="streamStatusText" class="chat-stream-status">
                {{ streamStatusText }}
              </div>
              <details v-if="streamingReasoning" class="message-reasoning" open>
                <summary class="message-reasoning__summary">
                  <Sparkles :size="13" :stroke-width="1.8" />
                  <span>思考中…</span>
                </summary>
                <div class="message-reasoning__content markdown-body" v-html="renderMarkdown(streamingReasoning)"></div>
              </details>
              <div v-if="streamingContent" class="chat-message-content markdown-body" v-html="renderMarkdown(streamingContent)"></div>
              <span class="typing-cursor"></span>
            </div>
          </div>
        </div>
      </div>

      <section class="playground-composer">
        <div class="composer-shell">
          <div v-if="uploadedFiles.length" class="upload-inline-list">
            <span v-for="file in uploadedFiles" :key="file.uid" class="upload-inline-tag">
              {{ file.name }}
              <X :size="12" class="upload-inline-remove" @click="removeUploadedFile(file.uid)" />
            </span>
          </div>

          <div class="composer-input-row">
            <el-tooltip content="添加附件" placement="top">
              <el-button
                aria-label="添加附件"
                circle
                :disabled="isBusy"
                @click="openFileDialog"
                class="composer-attach"
              >
                <Paperclip :size="15" :stroke-width="1.8" />
              </el-button>
            </el-tooltip>
            <el-input
              v-model="question"
              type="textarea"
              :autosize="{ minRows: 1, maxRows: 6 }"
              :maxlength="4000"
              placeholder="输入消息..."
              :disabled="isBusy"
              @keydown="handleInputKeydown"
            />
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
                <Send v-if="!isSending" :size="16" :stroke-width="1.8" />
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
        <p class="composer-hint">内容由 AI 生成，请仔细甄别 · Enter 发送，Shift + Enter 换行</p>
      </section>
    </div>

    <el-drawer
      v-model="settingsDrawerOpen"
      append-to-body
      title="参数设置"
      size="min(360px, 100vw)"
      class="playground-settings-drawer"
    >
      <PlaygroundSettings
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
  MessageSquareText,
  PanelLeftClose,
  PanelLeftOpen,
  Paperclip,
  Plus,
  Send,
  Settings2,
  Sparkles,
  UserRound,
  X
} from '@lucide/vue'
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
  model?: string
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
      model: selectedModel.value,
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

const openSettings = () => {
  const narrow = window.matchMedia('(max-width: 767px)').matches
  if (narrow) {
    settingsDrawerOpen.value = true
    return
  }
  sidebarCollapsed.value = !sidebarCollapsed.value
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
.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

.playground-workspace {
  display: flex;
  width: 100%;
  height: 100%;
  min-height: 540px;
  overflow: hidden;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.settings-panel {
  position: relative;
  display: flex;
  flex: 0 0 240px;
  width: 240px;
  min-width: 240px;
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
  padding: 10px 10px 0;
}

.settings-panel__toggle .el-button {
  width: 32px;
  height: 32px;
  border: none;
  color: var(--crc-text-muted);
  background: transparent;
}

.settings-panel__toggle .el-button:hover {
  color: var(--crc-text-strong);
  background: var(--crc-surface-hover);
}

.settings-panel__body {
  flex: 1;
  min-height: 0;
  padding: 10px 16px 16px;
  overflow-y: auto;
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

/* -- Toolbar -------------------------------------------------------------- */

.chat-toolbar {
  position: relative;
  display: flex;
  min-height: 56px;
  align-items: center;
  justify-content: center;
  gap: 16px;
  padding: 8px 16px;
  border-bottom: 1px solid var(--crc-border);
  background: var(--crc-surface);
}

.chat-toolbar__model {
  display: flex;
  align-items: center;
  min-width: 0;
}

.model-picker {
  width: min(340px, 40vw);
}

.model-picker :deep(.el-select__wrapper) {
  min-height: 36px;
  padding: 4px 12px;
  border-radius: 999px;
  background: var(--crc-surface-muted);
  box-shadow: 0 0 0 1px var(--crc-border) inset;
  transition: box-shadow var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease);
}

.model-picker :deep(.el-select__wrapper:hover) {
  background: var(--crc-surface-hover);
  box-shadow: 0 0 0 1px var(--crc-border-strong) inset;
}

.model-picker :deep(.el-select__wrapper.is-focused) {
  background: var(--crc-surface);
  box-shadow: 0 0 0 1px var(--crc-accent) inset, 0 0 0 3px var(--crc-accent-soft);
}

.model-picker__spark {
  flex-shrink: 0;
  color: var(--crc-accent);
}

.model-picker :deep(.el-select__selected-item) {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 13px;
  font-weight: 550;
}

.model-picker :deep(.el-select__placeholder) {
  font-size: 13px;
}

:global(.playground-model-popper .el-select-dropdown__item) {
  border-radius: var(--crc-radius-sm);
  margin: 0 6px;
}

.chat-toolbar__actions {
  position: absolute;
  right: 16px;
  display: flex;
  align-items: center;
  gap: 8px;
}

.new-chat-button {
  border-radius: 999px;
}

.new-chat-button:not(.is-disabled):hover {
  border-color: var(--crc-accent);
  color: var(--crc-accent);
}

.chat-toolbar__actions .el-button {
  height: 36px;
}

.chat-toolbar__settings-trigger {
  width: 36px;
}

/* -- Message stream -------------------------------------------------------- */

.playground-message-stream {
  flex: 1;
  display: flex;
  min-height: 0;
  overflow-y: auto;
  background: var(--crc-canvas);
}

.playground-message-stream__inner {
  display: flex;
  width: 100%;
  max-width: 880px;
  min-height: 100%;
  margin: 0 auto;
  flex-direction: column;
  gap: 20px;
  padding: 24px 24px 32px;
}

.chat-empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  color: var(--crc-text-muted);
  gap: 12px;
  text-align: center;
}

.chat-empty-icon {
  display: grid;
  width: 72px;
  height: 72px;
  place-items: center;
  border: 1px solid var(--crc-border);
  border-radius: 20px;
  color: var(--crc-accent);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-sm), 0 0 40px var(--crc-accent-soft);
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

.chat-empty__title {
  margin: 4px 0 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 20px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.chat-empty__sub {
  max-width: 420px;
  margin: 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.6;
}

.chat-empty__models {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 8px;
  margin-top: 8px;
}

.model-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 7px 14px;
  border: 1px solid var(--crc-border);
  border-radius: 999px;
  color: var(--crc-text);
  background: var(--crc-surface);
  font-size: 13px;
  font-family: var(--crc-font-display);
  font-weight: 500;
  cursor: pointer;
  transition: color var(--crc-duration-fast) var(--crc-ease),
    border-color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease),
    box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.model-chip:hover:not(:disabled) {
  border-color: var(--crc-accent);
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

.model-chip--active {
  border-color: var(--crc-accent);
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
  box-shadow: 0 0 0 3px var(--crc-accent-soft), var(--crc-shadow-xs);
}

.model-chip:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}

/* -- Messages -------------------------------------------------------------- */

.chat-message {
  display: flex;
  gap: 12px;
  max-width: min(92%, 860px);
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

.chat-message--assistant,
.chat-message--error {
  align-self: flex-start;
}

.chat-message-avatar {
  display: flex;
  width: 32px;
  height: 32px;
  min-width: 32px;
  align-items: center;
  justify-content: center;
  margin-top: 2px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  background: var(--crc-surface-muted);
}

.chat-message--user .chat-message-avatar {
  border-color: transparent;
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

.chat-message--assistant .chat-message-avatar,
.chat-message--error .chat-message-avatar {
  color: var(--crc-accent);
  background: var(--crc-surface);
}

.chat-message-body {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}

.chat-message-model {
  color: var(--crc-text-muted);
  font-family: var(--crc-font-display);
  font-size: 12px;
  font-weight: 550;
  letter-spacing: 0;
}

.chat-stream-status {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  width: fit-content;
  max-width: 100%;
  padding: 4px 8px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  background: var(--crc-surface-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  line-height: 1.5;
}

.message-reasoning {
  margin: 0 0 2px;
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
  font-family: var(--crc-font-mono);
  font-size: 11px;
  letter-spacing: 0.06em;
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
  font-size: 14px;
  line-height: 1.7;
}

.chat-message-content--plain {
  white-space: pre-wrap;
  font-family: inherit;
}

.chat-message--user .chat-message-content {
  padding: 10px 14px;
  border: 1px solid transparent;
  border-radius: 18px 18px 4px 18px;
  color: #ffffff;
  background: linear-gradient(135deg, var(--crc-accent) 0%, var(--crc-accent-active) 100%);
  box-shadow: var(--crc-shadow-sm);
  white-space: pre-wrap;
}

html.dark .chat-message--user .chat-message-content {
  color: #07211b;
}

.chat-message--assistant .chat-message-content {
  padding: 2px 0;
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
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.04em;
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

/* -- Composer -------------------------------------------------------------- */

.playground-composer {
  padding: 10px 24px 14px;
  border-top: 1px solid var(--crc-border);
  background: var(--crc-surface);
  z-index: 5;
}

.composer-shell {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
  max-width: 880px;
  margin: 0 auto;
  padding: 10px 12px;
  border: 1px solid var(--crc-border);
  border-radius: 16px;
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
  transition: border-color var(--crc-duration-fast) var(--crc-ease),
    box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.composer-shell:focus-within {
  border-color: var(--crc-accent);
  box-shadow: 0 0 0 3px var(--crc-accent-soft), var(--crc-shadow-sm);
}

.composer-input-row {
  display: flex;
  align-items: flex-end;
  gap: 6px;
  min-width: 0;
}

.composer-input-row :deep(.el-textarea) {
  flex: 1;
  min-width: 0;
}

.composer-input-row :deep(.el-textarea__inner) {
  padding: 8px 6px;
  border: none;
  background: transparent;
  resize: none;
  font-size: 14px;
  line-height: 1.5;
  box-shadow: none;
  transition: box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.composer-input-row :deep(.el-textarea__inner:focus) {
  box-shadow: none;
}

.composer-input-row :deep(.el-input__count) {
  background: transparent;
}

.composer-input-row .el-button {
  flex-shrink: 0;
}

.composer-input-row .el-button.composer-attach {
  width: 36px;
  height: 36px;
}

.send-button {
  width: 36px;
  height: 36px;
  min-width: 36px;
  min-height: 36px;
}

.send-button:not(.is-disabled) {
  box-shadow: var(--crc-accent-glow);
}

.composer-hint {
  margin: 8px 0 0;
  text-align: center;
  color: var(--crc-text-subtle);
  font-size: 12px;
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

/* -- Markdown -------------------------------------------------------------- */

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

/* -- Mobile ---------------------------------------------------------------- */

@media (max-width: 767px) {
  .playground-workspace {
    min-height: 480px;
  }

  .settings-panel {
    display: none;
  }

  .chat-toolbar {
    min-height: 52px;
    justify-content: flex-start;
    gap: 8px;
    padding: 8px 12px;
  }

  .chat-toolbar__model {
    flex: 1;
  }

  .model-picker {
    width: 100%;
  }

  .chat-toolbar__actions {
    position: static;
    gap: 6px;
  }

  .new-chat-button {
    width: 36px;
    padding: 8px;
  }

  .new-chat-button__label {
    display: none;
  }

  .playground-message-stream__inner {
    padding: 16px 14px 20px;
    gap: 16px;
  }

  .chat-message {
    max-width: 95%;
  }

  .chat-message-avatar {
    width: 28px;
    height: 28px;
    min-width: 28px;
  }

  .playground-composer {
    padding: 8px 12px 12px;
  }

  .composer-shell {
    padding: 8px 10px;
    border-radius: 14px;
  }

  .composer-hint {
    font-size: 11px;
  }
}

<style scoped>
.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

.playground-workspace {
  display: flex;
  width: 100%;
  height: 100%;
  min-height: 540px;
  overflow: hidden;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.settings-panel {
  position: relative;
  display: flex;
  flex: 0 0 240px;
  width: 240px;
  min-width: 240px;
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
  padding: 10px 10px 0;
}

.settings-panel__toggle .el-button {
  width: 32px;
  height: 32px;
  border: none;
  color: var(--crc-text-muted);
  background: transparent;
}

.settings-panel__toggle .el-button:hover {
  color: var(--crc-text-strong);
  background: var(--crc-surface-hover);
}

.settings-panel__body {
  flex: 1;
  min-height: 0;
  padding: 10px 16px 16px;
  overflow-y: auto;
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

/* -- Toolbar -------------------------------------------------------------- */

.chat-toolbar {
  position: relative;
  display: flex;
  min-height: 56px;
  align-items: center;
  justify-content: center;
  gap: 16px;
  padding: 8px 16px;
  border-bottom: 1px solid var(--crc-border);
  background: var(--crc-surface);
}

.chat-toolbar__model {
  display: flex;
  align-items: center;
  min-width: 0;
}

.model-picker {
  width: min(340px, 40vw);
}

.model-picker :deep(.el-select__wrapper) {
  min-height: 36px;
  padding: 4px 12px;
  border-radius: 999px;
  background: var(--crc-surface-muted);
  box-shadow: 0 0 0 1px var(--crc-border) inset;
  transition: box-shadow var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease);
}

.model-picker :deep(.el-select__wrapper:hover) {
  background: var(--crc-surface-hover);
  box-shadow: 0 0 0 1px var(--crc-border-strong) inset;
}

.model-picker :deep(.el-select__wrapper.is-focused) {
  background: var(--crc-surface);
  box-shadow: 0 0 0 1px var(--crc-accent) inset, 0 0 0 3px var(--crc-accent-soft);
}

.model-picker__spark {
  flex-shrink: 0;
  color: var(--crc-accent);
}

.model-picker :deep(.el-select__selected-item) {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 13px;
  font-weight: 550;
}

.model-picker :deep(.el-select__placeholder) {
  font-size: 13px;
}

:global(.playground-model-popper .el-select-dropdown__item) {
  border-radius: var(--crc-radius-sm);
  margin: 0 6px;
}

.chat-toolbar__actions {
  position: absolute;
  right: 16px;
  display: flex;
  align-items: center;
  gap: 8px;
}

.new-chat-button {
  border-radius: 999px;
}

.new-chat-button:not(.is-disabled):hover {
  border-color: var(--crc-accent);
  color: var(--crc-accent);
}

.chat-toolbar__actions .el-button {
  height: 36px;
}

.chat-toolbar__settings-trigger {
  width: 36px;
}

/* -- Message stream -------------------------------------------------------- */

.playground-message-stream {
  flex: 1;
  display: flex;
  min-height: 0;
  overflow-y: auto;
  background: var(--crc-canvas);
}

.playground-message-stream__inner {
  display: flex;
  width: 100%;
  max-width: 880px;
  min-height: 100%;
  margin: 0 auto;
  flex-direction: column;
  gap: 20px;
  padding: 24px 24px 32px;
}

.chat-empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  color: var(--crc-text-muted);
  gap: 12px;
  text-align: center;
}

.chat-empty-icon {
  display: grid;
  width: 72px;
  height: 72px;
  place-items: center;
  border: 1px solid var(--crc-border);
  border-radius: 20px;
  color: var(--crc-accent);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-sm), 0 0 40px var(--crc-accent-soft);
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

.chat-empty__title {
  margin: 4px 0 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 20px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.chat-empty__sub {
  max-width: 420px;
  margin: 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.6;
}

.chat-empty__models {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 8px;
  margin-top: 8px;
}

.model-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 7px 14px;
  border: 1px solid var(--crc-border);
  border-radius: 999px;
  color: var(--crc-text);
  background: var(--crc-surface);
  font-size: 13px;
  font-family: var(--crc-font-display);
  font-weight: 500;
  cursor: pointer;
  transition: color var(--crc-duration-fast) var(--crc-ease),
    border-color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease),
    box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.model-chip:hover:not(:disabled) {
  border-color: var(--crc-accent);
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

.model-chip--active {
  border-color: var(--crc-accent);
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
  box-shadow: 0 0 0 3px var(--crc-accent-soft), var(--crc-shadow-xs);
}

.model-chip:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}

/* -- Messages -------------------------------------------------------------- */

.chat-message {
  display: flex;
  gap: 12px;
  max-width: min(92%, 860px);
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

.chat-message--assistant,
.chat-message--error {
  align-self: flex-start;
}

.chat-message-avatar {
  display: flex;
  width: 32px;
  height: 32px;
  min-width: 32px;
  align-items: center;
  justify-content: center;
  margin-top: 2px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  background: var(--crc-surface-muted);
}

.chat-message--user .chat-message-avatar {
  border-color: transparent;
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

.chat-message--assistant .chat-message-avatar,
.chat-message--error .chat-message-avatar {
  color: var(--crc-accent);
  background: var(--crc-surface);
}

.chat-message-body {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}

.chat-message-model {
  color: var(--crc-text-muted);
  font-family: var(--crc-font-display);
  font-size: 12px;
  font-weight: 550;
  letter-spacing: 0;
}

.chat-stream-status {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  width: fit-content;
  max-width: 100%;
  padding: 4px 8px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  background: var(--crc-surface-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  line-height: 1.5;
}

.message-reasoning {
  margin: 0 0 2px;
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
  font-family: var(--crc-font-mono);
  font-size: 11px;
  letter-spacing: 0.06em;
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
  font-size: 14px;
  line-height: 1.7;
}

.chat-message-content--plain {
  white-space: pre-wrap;
  font-family: inherit;
}

.chat-message--user .chat-message-content {
  padding: 10px 14px;
  border: 1px solid transparent;
  border-radius: 18px 18px 4px 18px;
  color: #ffffff;
  background: linear-gradient(135deg, var(--crc-accent) 0%, var(--crc-accent-active) 100%);
  box-shadow: var(--crc-shadow-sm);
  white-space: pre-wrap;
}

html.dark .chat-message--user .chat-message-content {
  color: #07211b;
}

.chat-message--assistant .chat-message-content {
  padding: 2px 0;
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
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.04em;
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

/* -- Composer -------------------------------------------------------------- */

.playground-composer {
  padding: 10px 24px 14px;
  border-top: 1px solid var(--crc-border);
  background: var(--crc-surface);
  z-index: 5;
}

.composer-shell {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
  max-width: 880px;
  margin: 0 auto;
  padding: 10px 12px;
  border: 1px solid var(--crc-border);
  border-radius: 16px;
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
  transition: border-color var(--crc-duration-fast) var(--crc-ease),
    box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.composer-shell:focus-within {
  border-color: var(--crc-accent);
  box-shadow: 0 0 0 3px var(--crc-accent-soft), var(--crc-shadow-sm);
}

.composer-input-row {
  display: flex;
  align-items: flex-end;
  gap: 6px;
  min-width: 0;
}

.composer-input-row :deep(.el-textarea) {
  flex: 1;
  min-width: 0;
}

.composer-input-row :deep(.el-textarea__inner) {
  padding: 8px 6px;
  border: none;
  background: transparent;
  resize: none;
  font-size: 14px;
  line-height: 1.5;
  box-shadow: none;
  transition: box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.composer-input-row :deep(.el-textarea__inner:focus) {
  box-shadow: none;
}

.composer-input-row :deep(.el-input__count) {
  background: transparent;
}

.composer-input-row .el-button {
  flex-shrink: 0;
}

.composer-input-row .el-button.composer-attach {
  width: 36px;
  height: 36px;
}

.send-button {
  width: 36px;
  height: 36px;
  min-width: 36px;
  min-height: 36px;
}

.send-button:not(.is-disabled) {
  box-shadow: var(--crc-accent-glow);
}

.composer-hint {
  margin: 8px 0 0;
  text-align: center;
  color: var(--crc-text-subtle);
  font-size: 12px;
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

/* -- Markdown -------------------------------------------------------------- */

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

/* -- Mobile ---------------------------------------------------------------- */

@media (max-width: 767px) {
  .playground-workspace {
    min-height: 480px;
  }

  .settings-panel {
    display: none;
  }

  .chat-toolbar {
    min-height: 52px;
    justify-content: flex-start;
    gap: 8px;
    padding: 8px 12px;
  }

  .chat-toolbar__model {
    flex: 1;
  }

  .model-picker {
    width: 100%;
  }

  .chat-toolbar__actions {
    position: static;
    gap: 6px;
  }

  .new-chat-button {
    width: 36px;
    padding: 8px;
  }

  .new-chat-button__label {
    display: none;
  }

  .playground-message-stream__inner {
    padding: 16px 14px 20px;
    gap: 16px;
  }

  .chat-message {
    max-width: 95%;
  }

  .chat-message-avatar {
    width: 28px;
    height: 28px;
    min-width: 28px;
  }

  .playground-composer {
    padding: 8px 12px 12px;
  }

  .composer-shell {
    padding: 8px 10px;
    border-radius: 14px;
  }

  .composer-hint {
    font-size: 11px;
  }
}
</style>
