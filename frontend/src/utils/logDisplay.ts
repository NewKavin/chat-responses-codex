export const formatInferenceStrength = (value?: string | null) => {
  const trimmed = value?.trim()
  return trimmed && trimmed.length > 0 ? trimmed : '-'
}

export interface ErrorCategoryOption {
  value: string
  label: string
}

export type ErrorCategoryGroupKey =
  | 'gateway_access'
  | 'gateway_quota'
  | 'upstream_feedback'
  | 'upstream_response'
  | 'streaming'

export interface ErrorCategoryGroup {
  key: ErrorCategoryGroupKey
  label: string
  options: ErrorCategoryOption[]
}

export const errorCategoryGroups: ErrorCategoryGroup[] = [
  {
    key: 'gateway_access',
    label: '网关访问',
    options: [
      { value: 'gateway_auth_invalid', label: '认证无效' },
      { value: 'gateway_key_expired', label: 'Key 已过期' },
      { value: 'gateway_ip_not_allowed', label: 'IP 不允许' },
      { value: 'gateway_model_not_allowed', label: '模型不允许' },
      { value: 'gateway_no_routable_upstream', label: '无可路由上游' },
      { value: 'gateway_invalid_request', label: '请求无效' },
      { value: 'gateway_response_history_invalid', label: '响应历史无效' }
    ]
  },
  {
    key: 'gateway_quota',
    label: '网关配额',
    options: [
      { value: 'gateway_per_minute_limit_exceeded', label: '分钟请求限额' },
      { value: 'gateway_request_quota_exceeded', label: '窗口请求限额' },
      { value: 'gateway_daily_token_quota_exceeded', label: '日 Token 限额' },
      { value: 'gateway_monthly_token_quota_exceeded', label: '月 Token 限额' },
      { value: 'gateway_concurrency_full', label: '下游并发已满' }
    ]
  },
  {
    key: 'upstream_feedback',
    label: '上游反馈',
    options: [
      { value: 'upstream_auth_error', label: '上游认证错误' },
      { value: 'upstream_rate_limited', label: '上游限流' },
      { value: 'upstream_concurrency_full', label: '上游并发已满' },
      { value: 'upstream_protocol_unsupported', label: '上游协议不支持' },
      { value: 'upstream_context_limit', label: '上下文超限' },
      { value: 'upstream_request_rejected', label: '上游拒绝请求' },
      { value: 'upstream_temporary_unavailable', label: '上游临时不可用' }
    ]
  },
  {
    key: 'upstream_response',
    label: '上游响应',
    options: [
      { value: 'upstream_timeout', label: '上游超时' },
      { value: 'upstream_network_error', label: '上游网络错误' },
      { value: 'upstream_invalid_response', label: '上游响应无效' },
      { value: 'upstream_empty_response', label: '上游空响应' }
    ]
  },
  {
    key: 'streaming',
    label: '流式中断',
    options: [
      { value: 'stream_client_cancelled', label: '客户端取消，无输出' },
      { value: 'stream_incomplete_close', label: '下游断连，已有部分输出' },
      { value: 'stream_interrupted', label: '下游断连，未分类' },
      { value: 'stream_upstream_body_decode_error', label: '上游响应解码失败' },
      { value: 'stream_upstream_read_error', label: '上游流读取失败' },
      { value: 'stream_upstream_timeout', label: '上游流超时' },
      { value: 'stream_idle_timeout', label: '空闲超时' },
      { value: 'stream_max_duration', label: '最大时长' }
    ]
  }
]

const categoryEntries = errorCategoryGroups.flatMap(group =>
  group.options.map(option => ({ ...option, groupKey: group.key }))
)
const categoryLabelByValue = new Map(categoryEntries.map(option => [option.value, option.label]))
const categoryGroupByValue = new Map(categoryEntries.map(option => [option.value, option.groupKey]))

export const formatErrorCategory = (value?: string | null) => {
  const trimmed = value?.trim()
  if (!trimmed) {
    return '-'
  }

  return categoryLabelByValue.get(trimmed) ?? trimmed
}

export const buildVisibleLogSummary = (
  logs: Array<{ status_code: number; error_category?: string | null }>
) => {
  const summary = {
    total: logs.length,
    failed: 0,
    gatewayAccess: 0,
    gatewayQuota: 0,
    upstreamFeedback: 0,
    upstreamResponse: 0,
    streaming: 0,
    uncategorized: 0
  }

  for (const log of logs) {
    const category = log.error_category?.trim()
    if (log.status_code < 400 && !category) {
      continue
    }

    summary.failed += 1
    const groupKey = category ? categoryGroupByValue.get(category) : undefined

    if (groupKey === 'gateway_access') {
      summary.gatewayAccess += 1
    } else if (groupKey === 'gateway_quota') {
      summary.gatewayQuota += 1
    } else if (groupKey === 'upstream_feedback') {
      summary.upstreamFeedback += 1
    } else if (groupKey === 'upstream_response') {
      summary.upstreamResponse += 1
    } else if (groupKey === 'streaming') {
      summary.streaming += 1
    } else {
      summary.uncategorized += 1
    }
  }

  return summary
}
