import type {
  ActiveGatewayRequest,
  TroubleshootingCheck,
  TroubleshootingClientProfile,
  TroubleshootingRunResponse,
  TroubleshootingStepStatus
} from '@/types'

export interface ClientProfileDefaults {
  label: string
  description: string
  checks: TroubleshootingCheck[]
}

export const clientProfileDefaults: Record<TroubleshootingClientProfile, ClientProfileDefaults> = {
  cline: {
    label: 'Cline',
    description:
      'OpenAI Compatible，重点验证 stream、tools 和模型能力提示。Cline 的 complex prompts warning 是模型能力提示，不是网关错误。',
    checks: ['models', 'chat_stream', 'tools']
  },
  codex: {
    label: 'Codex',
    description: 'Responses 优先，验证模型列表、Responses stream 和 Chat stream。',
    checks: ['models', 'responses_stream', 'chat_stream']
  },
  opencode: {
    label: 'opencode',
    description: 'OpenAI Compatible，重点验证 stream 和 tools。',
    checks: ['models', 'chat_stream', 'tools']
  },
  claude_code: {
    label: 'Claude Code',
    description: 'Anthropic Messages，验证 messages stream 和 count_tokens。',
    checks: ['models', 'messages_stream', 'count_tokens']
  },
  hermes: {
    label: 'Hermes',
    description: 'OpenAI Compatible，验证 Chat Completions stream。',
    checks: ['models', 'chat_stream']
  },
  open_ai_compatible: {
    label: '通用 OpenAI Compatible',
    description: '验证模型列表和 Chat Completions。',
    checks: ['models', 'chat_stream']
  },
  anthropic_compatible: {
    label: '通用 Anthropic Compatible',
    description: '验证 Messages stream 和 count_tokens。',
    checks: ['models', 'messages_stream', 'count_tokens']
  }
}

export const getClientProfileDefaults = (profile: TroubleshootingClientProfile) =>
  clientProfileDefaults[profile]

export const getTroubleshootingStatusMeta = (status: TroubleshootingStepStatus) => {
  if (status === 'passed') return { label: '通过', type: 'success' as const }
  if (status === 'warning') return { label: '警告', type: 'warning' as const }
  if (status === 'timeout') return { label: '超时', type: 'warning' as const }
  return { label: '失败', type: 'danger' as const }
}

export const getTroubleshootingSuggestion = (category?: string | null) => {
  if (!category) return '继续查看该诊断项的 HTTP 状态、耗时和详细说明。'
  if (category === 'gateway_daily_token_quota_exceeded') {
    return '日 Token 限额已达到；等待额度恢复或联系管理员调整下游限额。'
  }
  if (category === 'gateway_monthly_token_quota_exceeded') {
    return '月 Token 限额已达到；等待额度恢复或联系管理员调整下游限额。'
  }
  if (category === 'gateway_per_minute_limit_exceeded') {
    return '下游分钟请求限额已触发；稍后重试或降低并发。'
  }
  if (category === 'gateway_request_quota_exceeded') {
    return '下游窗口请求限额已触发；等待窗口恢复或联系管理员调整限制。'
  }
  if (category === 'gateway_model_not_allowed') {
    return '模型未对当前下游暴露；检查模型名、下游白名单和上游支持模型。'
  }
  if (category === 'upstream_rate_limited') {
    return '上游限流；稍后重试、降低并发或切换上游通道。'
  }
  if (category === 'upstream_context_limit') {
    return '上下文超限；缩短输入、调低历史长度或调整模型上下文配置。'
  }
  if (category === 'upstream_temporary_unavailable') {
    return '上游临时不可用；稍后重试或在管理端检查上游健康。'
  }
  if (category.startsWith('stream_')) {
    return '流式响应异常；查看最后增量时间、上游耗时和客户端是否断开。'
  }
  return '查看管理端日志中的错误分类和上游响应，必要时复制诊断摘要给管理员。'
}

const redactSecrets = (value: string) =>
  value
    .replace(/sk-[A-Za-z0-9_-]{6,}/g, 'sk-***')
    .replace(/key-[A-Za-z0-9_-]{6,}/g, 'key-***')
    .replace(/Bearer\s+[A-Za-z0-9._-]+/gi, 'Bearer ***')

export const buildTroubleshootingCopySummary = (run: TroubleshootingRunResponse) => {
  const lines = [
    `诊断 ID: ${run.run_id}`,
    `客户端: ${clientProfileDefaults[run.client_profile].label}`,
    `模型: ${run.model}`,
    `状态: ${run.status}`,
    ...run.results.map(result =>
      [
        `- ${result.label}: ${getTroubleshootingStatusMeta(result.status).label}`,
        result.http_status ? `HTTP ${result.http_status}` : '',
        result.error_category ? `分类 ${result.error_category}` : '',
        result.summary,
        result.details
      ]
        .filter(Boolean)
        .join(' | ')
    )
  ]
  return redactSecrets(lines.join('\n'))
}

export const getActiveRequestHealth = (
  request: Pick<ActiveGatewayRequest, 'idle_seconds' | 'status'>
) => {
  if (request.status === 'error') return { label: '异常', type: 'danger' as const }
  if (request.idle_seconds >= 120) return { label: '无增量', type: 'warning' as const }
  return { label: '运行中', type: 'success' as const }
}
