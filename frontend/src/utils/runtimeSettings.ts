import type { RuntimeSettingKey, RuntimeSettings } from '@/types'

export type RuntimeSettingGroupId =
  | 'general'
  | 'discovery'
  | 'routing'
  | 'concurrency'
  | 'http'
  | 'logs'

export type RuntimeSettingApplyMode = 'immediate' | 'restart'
export type RuntimeSettingControl = 'text' | 'switch' | 'number' | 'number-list'

export interface RuntimeSettingGroup {
  id: RuntimeSettingGroupId
  label: string
}

export interface RuntimeSettingField {
  key: RuntimeSettingKey
  group: RuntimeSettingGroupId
  label: string
  apply: RuntimeSettingApplyMode
  control: RuntimeSettingControl
  unit?: string
  min?: number
  max?: number
  step?: number
  integer?: boolean
  maxLength?: number
  description?: string
}

export type RuntimeSettingsValidationErrors = Partial<Record<RuntimeSettingKey, string>>

const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER
const MAX_U32 = 4_294_967_295
const MAX_PROBE_DELAY_MS = 60_000

export const runtimeSettingGroups: RuntimeSettingGroup[] = [
  { id: 'general', label: '通用' },
  { id: 'discovery', label: '发现与探测' },
  { id: 'routing', label: '路由策略' },
  { id: 'concurrency', label: '并发恢复' },
  { id: 'http', label: 'HTTP 与流式' },
  { id: 'logs', label: '日志' }
]

export const runtimeSettingFields: RuntimeSettingField[] = [
  {
    key: 'app_name',
    group: 'general',
    label: '应用名称',
    apply: 'immediate',
    control: 'text',
    maxLength: 120
  },
  {
    key: 'admin_upstream_timeout_seconds',
    group: 'general',
    label: '管理请求超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'troubleshooting_check_timeout_seconds',
    group: 'general',
    label: '故障排查单项超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'model_probe_refresh_interval_seconds',
    group: 'discovery',
    label: '模型探测刷新间隔',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_model_auto_discovery_enabled',
    group: 'discovery',
    label: '自动发现上游模型',
    apply: 'restart',
    control: 'switch'
  },
  {
    key: 'upstream_model_key_sync_interval_seconds',
    group: 'discovery',
    label: '模型 Key 同步间隔',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 0,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'capability_probe_queue_capacity',
    group: 'discovery',
    label: '能力探测队列容量',
    apply: 'restart',
    control: 'number',
    unit: '项',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'capability_probe_request_timeout_seconds',
    group: 'discovery',
    label: '能力探测请求超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'capability_probe_reasoning_timeout_seconds',
    group: 'discovery',
    label: '思考档位探测超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'automatic_capability_probes_enabled',
    group: 'discovery',
    label: '自动能力探测',
    apply: 'immediate',
    control: 'switch',
    description: '开启后会周期性对所有下游可见模型自动探测（消耗 token）'
  },
  {
    key: 'upstream_rate_limit_default_retry_seconds',
    group: 'routing',
    label: '限流默认重试等待',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'routing_affinity_enabled',
    group: 'routing',
    label: '路由亲和',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'routing_affinity_ttl_seconds',
    group: 'routing',
    label: '路由亲和有效期',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'routing_affinity_escape_pressure_ratio',
    group: 'routing',
    label: '亲和逃逸压力比',
    apply: 'immediate',
    control: 'number',
    unit: '倍',
    min: 1,
    max: 1_000_000,
    step: 0.1,
    integer: false
  },
  {
    key: 'upstream_hedge_enabled',
    group: 'routing',
    label: '上游对冲请求',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'upstream_hedge_delay_ms',
    group: 'routing',
    label: '首次对冲延迟',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_hedge_interval_ms',
    group: 'routing',
    label: '后续对冲间隔',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_hedge_max_extra_attempts',
    group: 'routing',
    label: '最大额外对冲次数',
    apply: 'immediate',
    control: 'number',
    unit: '次',
    min: 0,
    max: MAX_U32
  },
  {
    key: 'upstream_same_route_retry_enabled',
    group: 'routing',
    label: '同路由重试',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'upstream_transient_route_cooldown_base_seconds',
    group: 'routing',
    label: '瞬时错误基础冷却',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_transient_route_cooldown_max_seconds',
    group: 'routing',
    label: '瞬时错误最大冷却',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_route_health_half_open_ttl_seconds',
    group: 'routing',
    label: '半开探测有效期',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_route_exhaustion_retry_enabled',
    group: 'routing',
    label: '路由耗尽后重试',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'upstream_route_exhaustion_retry_max_wait_ms',
    group: 'routing',
    label: '路由耗尽最大等待',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_route_exhaustion_retry_max_rounds',
    group: 'routing',
    label: '路由耗尽最大轮次',
    apply: 'immediate',
    control: 'number',
    unit: '轮',
    min: 1,
    max: MAX_U32
  },
  {
    key: 'upstream_common_mode_breaker_threshold',
    group: 'routing',
    label: '共模失败熔断阈值',
    apply: 'immediate',
    control: 'number',
    unit: '条',
    min: 0,
    max: 64
  },
  {
    key: 'default_upstream_max_concurrency',
    group: 'concurrency',
    label: '新建上游每 Key 默认最大并发',
    apply: 'immediate',
    control: 'number',
    unit: '路',
    min: 1,
    max: MAX_U32
  },
  {
    key: 'downstream_lease_ttl_seconds',
    group: 'concurrency',
    label: '下游租约有效期',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 60,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_concurrency_recovery_max_wait_ms',
    group: 'concurrency',
    label: '并发恢复最大等待',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_concurrency_recovery_max_rounds',
    group: 'concurrency',
    label: '并发恢复最大轮次',
    apply: 'immediate',
    control: 'number',
    unit: '轮',
    min: 1,
    max: MAX_U32
  },
  {
    key: 'upstream_concurrency_probe_delays_ms',
    group: 'concurrency',
    label: '并发探测延迟序列',
    apply: 'immediate',
    control: 'number-list',
    unit: '毫秒'
  },
  {
    key: 'upstream_http_pool_max_idle_per_host',
    group: 'http',
    label: '每主机最大空闲连接',
    apply: 'restart',
    control: 'number',
    unit: '条',
    min: 8,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_user_agent',
    group: 'http',
    label: '上游 User-Agent',
    apply: 'restart',
    control: 'text',
    maxLength: 512
  },
  {
    key: 'upstream_connect_timeout_seconds',
    group: 'http',
    label: '上游连接超时',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_response_header_timeout_seconds',
    group: 'http',
    label: '响应头超时',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_stream_keepalive_interval_seconds',
    group: 'http',
    label: '流式保活间隔',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_stream_idle_timeout_seconds',
    group: 'http',
    label: '流式空闲超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_stream_max_duration_seconds',
    group: 'http',
    label: '流式最大时长',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_first_semantic_output_timeout_seconds',
    group: 'http',
    label: '首个语义输出超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'usage_log_archive_max_files',
    group: 'logs',
    label: '日志归档文件上限',
    apply: 'restart',
    control: 'number',
    unit: '个',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'usage_log_retention_days',
    group: 'logs',
    label: '日志保留天数',
    apply: 'restart',
    control: 'number',
    unit: '天',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'admin_logs_page_size_max',
    group: 'logs',
    label: '管理日志单页上限',
    apply: 'immediate',
    control: 'number',
    unit: '条',
    min: 200,
    max: MAX_SAFE_INTEGER
  }
]

const normalizeProbeDelays = (values: number[]): number[] => {
  if (
    values.length === 0 ||
    values.some(
      value =>
        !Number.isSafeInteger(value) || value < 1 || value > MAX_PROBE_DELAY_MS
    )
  ) {
    throw new Error('探测延迟必须是 1 到 60000 之间的整数')
  }
  return [...new Set(values)].sort((left, right) => left - right)
}

export const parseProbeDelays = (input: string): number[] => {
  const parts = input
    .split(',')
    .map(part => part.trim())
    .filter(Boolean)
  if (parts.length === 0 || parts.some(part => !/^\d+$/.test(part))) {
    throw new Error('请输入逗号分隔的毫秒整数')
  }
  return normalizeProbeDelays(parts.map(Number))
}

export const formatProbeDelays = (values: number[]): string => values.join(', ')

export const cloneRuntimeSettings = (settings: RuntimeSettings): RuntimeSettings => ({
  ...settings,
  upstream_concurrency_probe_delays_ms: [
    ...settings.upstream_concurrency_probe_delays_ms
  ]
})

const settingValuesEqual = (
  left: RuntimeSettings[RuntimeSettingKey],
  right: RuntimeSettings[RuntimeSettingKey]
): boolean => {
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((value, index) => value === right[index])
  }
  return left === right
}

export const isRuntimeSettingsDirty = (
  loaded: RuntimeSettings,
  edited: RuntimeSettings
): boolean =>
  runtimeSettingFields.some(
    field => !settingValuesEqual(loaded[field.key], edited[field.key])
  )

export const changedRestartFields = (
  loaded: RuntimeSettings,
  edited: RuntimeSettings
): RuntimeSettingKey[] =>
  runtimeSettingFields
    .filter(
      field =>
        field.apply === 'restart' &&
        !settingValuesEqual(loaded[field.key], edited[field.key])
    )
    .map(field => field.key)

export const validateRuntimeSettings = (
  settings: RuntimeSettings
): RuntimeSettingsValidationErrors => {
  const errors: RuntimeSettingsValidationErrors = {}

  for (const field of runtimeSettingFields) {
    const value = settings[field.key]
    if (field.control === 'text') {
      if (typeof value !== 'string' || value.trim().length === 0) {
        errors[field.key] = '不能为空'
      } else if (field.maxLength !== undefined && [...value.trim()].length > field.maxLength) {
        errors[field.key] = `最多 ${field.maxLength} 个字符`
      }
      continue
    }
    if (field.control !== 'number') continue
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      errors[field.key] = '请输入有效数字'
      continue
    }
    if (field.integer !== false && !Number.isSafeInteger(value)) {
      errors[field.key] = '请输入整数'
      continue
    }
    if (field.min !== undefined && value < field.min) {
      errors[field.key] = `不能小于 ${field.min}`
    } else if (field.max !== undefined && value > field.max) {
      errors[field.key] = `不能大于 ${field.max}`
    }
  }

  let probeDelays: number[] | undefined
  try {
    probeDelays = normalizeProbeDelays(settings.upstream_concurrency_probe_delays_ms)
  } catch (error) {
    errors.upstream_concurrency_probe_delays_ms =
      error instanceof Error ? error.message : '探测延迟无效'
  }

  if (
    settings.upstream_transient_route_cooldown_base_seconds >
    settings.upstream_transient_route_cooldown_max_seconds
  ) {
    errors.upstream_transient_route_cooldown_base_seconds = '不能超过最大冷却时间'
  }
  if (
    settings.upstream_stream_keepalive_interval_seconds >=
    settings.upstream_stream_idle_timeout_seconds
  ) {
    errors.upstream_stream_keepalive_interval_seconds = '必须短于流式空闲超时'
  }
  if (
    settings.upstream_stream_idle_timeout_seconds >
    settings.upstream_stream_max_duration_seconds
  ) {
    errors.upstream_stream_idle_timeout_seconds = '不能超过流式最大时长'
  }

  const minimumFirstSemantic =
    Math.ceil(settings.upstream_concurrency_recovery_max_wait_ms / 1_000) +
    settings.upstream_response_header_timeout_seconds +
    settings.upstream_stream_idle_timeout_seconds
  if (
    Number.isSafeInteger(minimumFirstSemantic) &&
    settings.upstream_first_semantic_output_timeout_seconds < minimumFirstSemantic
  ) {
    errors.upstream_first_semantic_output_timeout_seconds =
      `不能小于并发、响应头和空闲预算之和 ${minimumFirstSemantic} 秒`
  }

  if (
    probeDelays !== undefined &&
    Number.isSafeInteger(settings.upstream_concurrency_recovery_max_rounds) &&
    settings.upstream_concurrency_recovery_max_rounds > 0
  ) {
    let covered = 0
    for (let round = 1; round < settings.upstream_concurrency_recovery_max_rounds; round += 1) {
      covered += probeDelays[Math.min(round - 1, probeDelays.length - 1)]
    }
    if (covered < settings.upstream_concurrency_recovery_max_wait_ms) {
      errors.upstream_concurrency_recovery_max_rounds = '当前轮次无法覆盖并发恢复等待预算'
    }
  }

  return errors
}
