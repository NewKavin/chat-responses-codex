export interface ReadableErrorSource {
  status?: number | null
  statusText?: string | null
  body?: unknown
  bodyText?: string | null
}

interface ReadableErrorParts {
  message?: string
  code?: string
  category?: string
  type?: string
}

export const summarizeErrorText = (value?: string | null, maxLength = 180) => {
  const text = value?.replace(/\s+/g, ' ').trim()
  if (!text) {
    return '-'
  }

  return text.length > maxLength ? `${text.slice(0, maxLength)}...` : text
}

const readString = (value: unknown) => {
  if (typeof value !== 'string') {
    return undefined
  }

  const trimmed = value.trim()
  return trimmed || undefined
}

const parseJsonText = (value?: string | null): unknown => {
  if (!value) {
    return undefined
  }

  try {
    return JSON.parse(value)
  } catch {
    return undefined
  }
}

const extractOpenAiErrorParts = (body: unknown): ReadableErrorParts | undefined => {
  if (!body || typeof body !== 'object') {
    return undefined
  }

  const payload = body as {
    error?: unknown
    message?: unknown
    detail?: unknown
    code?: unknown
    category?: unknown
    type?: unknown
  }
  const error = payload.error
  if (typeof error === 'string') {
    return { message: readString(error) }
  }

  if (error && typeof error === 'object') {
    return {
      message: readString((error as { message?: unknown }).message),
      code: readString((error as { code?: unknown }).code),
      category: readString((error as { category?: unknown }).category),
      type: readString((error as { type?: unknown }).type)
    }
  }

  return {
    message: readString(payload.message) ?? readString(payload.detail),
    code: readString(payload.code),
    category: readString(payload.category),
    type: readString(payload.type)
  }
}

const formatStatusPrefix = ({ status, statusText }: ReadableErrorSource) => {
  const parts = [
    typeof status === 'number' ? String(status) : undefined,
    readString(statusText)
  ].filter(Boolean)

  return parts.join(' ')
}

const getFallbackBodyText = ({ body, bodyText }: ReadableErrorSource) => {
  if (readString(bodyText)) {
    return bodyText
  }
  if (typeof body === 'string') {
    return body
  }
  if (body && typeof body === 'object') {
    return JSON.stringify(body)
  }
  return undefined
}

export const extractReadableErrorMessage = (source: ReadableErrorSource) => {
  const parts =
    extractOpenAiErrorParts(source.body) ??
    extractOpenAiErrorParts(parseJsonText(source.bodyText))
  const statusPrefix = formatStatusPrefix(source)

  if (parts?.message) {
    const detail = [...new Set([parts?.category, parts?.code, parts?.type].filter(Boolean))].join(' / ')
    const message = detail ? `${parts.message}（${detail}）` : parts.message
    return statusPrefix ? `${statusPrefix}：${message}` : message
  }

  const message = summarizeErrorText(getFallbackBodyText(source))
  return statusPrefix ? `${statusPrefix}：${message}` : message
}
