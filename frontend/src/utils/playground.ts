export interface UploadedFileContext {
  name: string
  type: string
  size: number
  text: string
}

export interface ChatContentTextPart {
  type: 'text'
  text: string
}

export type PlaygroundMessageContent = string | ChatContentTextPart[]

export interface PlaygroundMessage {
  role: 'system' | 'user' | 'assistant'
  content: PlaygroundMessageContent
}

export interface BuildPlaygroundChatRequestInput {
  model: string
  question: string
  history?: PlaygroundMessage[]
  uploadedFiles?: UploadedFileContext[]
  temperature?: number
  maxTokens?: number
  inferenceStrength?: string
}

export const inferenceStrengthOptions = ['xhigh', 'high', 'medium', 'low'] as const

const formatUploadedFileText = (file: UploadedFileContext) => {
  const mimeType = file.type || 'application/octet-stream'
  const suffix = `${file.name} (${mimeType}, ${file.size}B)`

  return `【文件】${suffix}\n${file.text}`
}

const normalizeMessageContent = (content: unknown): PlaygroundMessageContent | null => {
  if (typeof content === 'string') {
    return content.trim()
  }

  const maybeArray = Array.isArray(content) ? content : null
  if (!maybeArray) {
    return null
  }

  const contentBlocks: ChatContentTextPart[] = []
  for (const item of maybeArray) {
    if (!item || typeof item !== 'object') {
      return null
    }

    const block = item as { type?: unknown; text?: unknown }
    if (block.type !== 'text' || typeof block.text !== 'string') {
      return null
    }

    contentBlocks.push({ type: 'text', text: block.text })
  }

  return contentBlocks
}

const buildUserMessageContent = (question: string, uploadedFiles?: UploadedFileContext[]) => {
  const fileContext = (uploadedFiles ?? [])
    .filter(
      file =>
        typeof file?.name === 'string' &&
        file.name.trim() &&
        typeof file?.size === 'number' &&
        file.size >= 0 &&
        typeof file?.text === 'string'
    )
    .map(file => ({
      type: 'text' as const,
      text: formatUploadedFileText(file).trimEnd()
    }))

  const prompt = question.trim()
  if (!fileContext.length) {
    return prompt
  }

  if (prompt) {
    fileContext.push({ type: 'text', text: prompt })
  }

  return fileContext
}

export interface ChatCompletionMessage {
  role: string
  content?: unknown
}

interface ChatCompletionChoice {
  message?: ChatCompletionMessage
}

interface ChatCompletionResponse {
  choices?: ChatCompletionChoice[]
  usage?: unknown
}

export const parseGatewayModels = (response: unknown): string[] => {
  if (typeof response !== 'object' || response === null) {
    throw new Error('模型列表返回结构不正确')
  }

  const raw = (response as { data?: unknown[] }).data
  if (!Array.isArray(raw)) {
    return []
  }

  const models = new Set<string>()
  for (const item of raw) {
    if (typeof item !== 'object' || item === null) {
      continue
    }

    const id = (item as { id?: unknown }).id
    if (typeof id !== 'string') {
      continue
    }

    const model = id.trim()
    if (!model || models.has(model)) {
      continue
    }
    models.add(model)
  }

  return [...models]
}

export const buildPlaygroundChatPayload = ({
  model,
  question,
  history = [],
  uploadedFiles,
  temperature,
  maxTokens,
  inferenceStrength,
  stream = false
}: BuildPlaygroundChatRequestInput & { stream?: boolean }): {
  model: string
  messages: PlaygroundMessage[]
  stream: boolean
  temperature?: number
  max_tokens?: number
  inference_strength?: string
} => {
  const normalizedHistory = history
    .filter(
      message => message && (message.role === 'system' || message.role === 'user' || message.role === 'assistant')
    )
    .map(message => ({
      role: message.role,
      content: normalizeMessageContent(message.content) ?? ''
    }))

  const userContent = buildUserMessageContent(question, uploadedFiles)

  const payload: {
    model: string
    messages: PlaygroundMessage[]
    stream: boolean
    temperature?: number
    max_tokens?: number
    inference_strength?: string
  } = {
    model,
    messages: [...normalizedHistory, { role: 'user', content: userContent }],
    stream
  }

  if (typeof temperature === 'number' && temperature >= 0 && temperature <= 2) {
    payload.temperature = Number(temperature.toFixed(2))
  }

  if (typeof maxTokens === 'number' && maxTokens >= 1) {
    payload.max_tokens = Math.floor(maxTokens)
  }

  const normalizedInferenceStrength = inferenceStrength?.trim()
  if (normalizedInferenceStrength) {
    payload.inference_strength = normalizedInferenceStrength
  }

  return payload
}

export const extractChatCompletionText = (body: unknown): string => {
  if (typeof body !== 'object' || body === null) {
    throw new Error('响应体不是合法 JSON')
  }

  const payload = body as ChatCompletionResponse
  const choices = payload.choices
  if (!Array.isArray(choices) || choices.length === 0) {
    throw new Error('响应缺少 choices')
  }

  const firstChoice = choices[0]
  const message = firstChoice?.message
  if (typeof message !== 'object' || message === null) {
    throw new Error('响应消息结构不合法')
  }

  const content = message.content
  if (typeof content !== 'string') {
    return ''
  }

  return content
}

export const extractChatCompletionUsage = (body: unknown) => {
  if (typeof body !== 'object' || body === null) {
    return null
  }

  const usage = (body as ChatCompletionResponse).usage
  if (!usage || typeof usage !== 'object') {
    return null
  }

  const promptTokens = Number((usage as { prompt_tokens?: unknown }).prompt_tokens)
  const completionTokens = Number((usage as { completion_tokens?: unknown }).completion_tokens)
  const totalTokens = Number((usage as { total_tokens?: unknown }).total_tokens)

  if ([promptTokens, completionTokens, totalTokens].some(Number.isNaN)) {
    return null
  }

  return {
    prompt_tokens: promptTokens,
    completion_tokens: completionTokens,
    total_tokens: totalTokens
  }
}

export type PlaygroundStreamPhase = 'connecting' | 'waiting' | 'thinking' | 'generating'

export const formatPlaygroundStreamStatus = ({
  phase,
  elapsedSeconds,
  keepaliveCount
}: {
  phase: PlaygroundStreamPhase
  elapsedSeconds: number
  keepaliveCount: number
}): string => {
  const seconds = Math.max(0, Math.floor(elapsedSeconds))
  if (phase === 'thinking') {
    return `思考中 ${seconds}s`
  }
  if (phase === 'generating') {
    return `生成中 ${seconds}s`
  }
  if (phase === 'waiting' || keepaliveCount > 0) {
    return `已连接，等待模型首个输出 ${seconds}s`
  }
  return `正在连接模型 ${seconds}s`
}

export interface StreamChunk {
  content: string
  reasoningContent?: string
  usage?: {
    prompt_tokens: number
    completion_tokens: number
    total_tokens: number
  }
  keepalive?: boolean
  errorMessage?: string
  errorType?: string
  errorCode?: string
  errorCategory?: string
  done: boolean
}

export const parseSSELine = (line: string): StreamChunk | null => {
  const trimmed = line.trim()
  if (!trimmed) {
    return null
  }

  if (trimmed.startsWith(':')) {
    return { content: '', keepalive: true, done: false }
  }

  if (!trimmed.startsWith('data:')) {
    return null
  }

  const data = trimmed.slice(5).trim()
  if (data === '[DONE]') {
    return { content: '', done: true }
  }

  try {
    const parsed = JSON.parse(data)
    if (
      parsed &&
      typeof parsed === 'object' &&
      !Array.isArray(parsed) &&
      Object.keys(parsed).length === 0
    ) {
      return { content: '', keepalive: true, done: false }
    }

    const error = parsed?.error
    if (typeof error === 'string' && error.trim()) {
      return { content: '', errorMessage: error.trim(), done: false }
    }
    if (error && typeof error === 'object') {
      const message = typeof error.message === 'string' ? error.message.trim() : ''
      const type = typeof error.type === 'string' ? error.type.trim() : undefined
      const code = typeof error.code === 'string' ? error.code.trim() : undefined
      const category =
        typeof error.category === 'string'
          ? error.category.trim()
          : typeof parsed.category === 'string'
            ? parsed.category.trim()
            : undefined
      if (message) {
        return {
          content: '',
          errorMessage: message,
          errorType: type || undefined,
          errorCode: code || undefined,
          errorCategory: category || undefined,
          done: false
        }
      }
    }

    const delta = parsed.choices?.[0]?.delta
    const content = typeof delta?.content === 'string' ? delta.content : ''
    const reasoningContent =
      typeof delta?.reasoning_content === 'string' ? delta.reasoning_content : undefined
    const usage = parsed.usage
    let usageResult: StreamChunk['usage'] | undefined

    if (usage && typeof usage === 'object') {
      const pt = Number(usage.prompt_tokens)
      const ct = Number(usage.completion_tokens)
      const tt = Number(usage.total_tokens)
      if (![pt, ct, tt].some(Number.isNaN)) {
        usageResult = { prompt_tokens: pt, completion_tokens: ct, total_tokens: tt }
      }
    }

    return { content, reasoningContent, usage: usageResult, done: false }
  } catch {
    return null
  }
}
