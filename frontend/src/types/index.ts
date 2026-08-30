// ============================================================================
// Authentication Types
// ============================================================================

export interface LoginRequest {
  username: string
  password: string
}

export interface LoginResponse {
  token: string
}

// ============================================================================
// Dashboard Types
// ============================================================================

export interface DashboardBreakdownItem {
  name: string
  value: number
}

export interface DashboardData {
  upstreams_count: number
  upstreams_active: number
  downstreams_count: number
  downstreams_active: number
  logs_count: number
  active_models: number
  responses_upstreams: number
  admin_username: string
  app_name: string
}

export interface DashboardSummaryResponse extends DashboardData {
  analytics: DashboardAnalyticsRange
}

export interface DashboardAnalyticsRange {
  range: string
  summary: {
    total_requests: number
    success_rate: number
    average_latency_ms: number
    total_tokens: number
  }
  daily_series: Array<{
    day: string
    date: number
    requests: number
    tokens: number
    avg_latency_ms: number
    success_rate: number
  }>
  failure_categories: DashboardBreakdownItem[]
  user_agent_clusters: DashboardBreakdownItem[]
  model_usage: DashboardBreakdownItem[]
  downstream_usage: DashboardBreakdownItem[]
}

// ============================================================================ 
// Upstream Types
// ============================================================================

export interface ApiKeyModelConfig {
  api_key: string
  supported_models: string[]
}

export interface UpstreamModelMapping {
  /** 该上游 supported_models / api_key_models 中的原拼写（发往上游用） */
  upstream_model: string
  /** 下游可见与请求用的名称 */
  downstream_model: string
}

export interface KeyModelDiscoveryResult {
  key_index: number
  models?: number
  model_list?: string[]
  error?: string
  error_code?: 'timeout' | 'connection' | 'request' | 'http_status' | 'invalid_json' | 'missing_data' | 'empty_models'
  http_status?: number
}

export type NonstandardFieldPolicy = 'auto' | 'always_strip' | 'forward'

export interface UpstreamConfig {
  id: string
  name: string
  remark: string
  continuation_provider_group?: string | null
  base_url: string
  api_key: string
  api_keys?: string[]
  api_key_models?: ApiKeyModelConfig[]
  protocol: 'ChatCompletions' | 'Responses'
  protocols?: Array<'ChatCompletions' | 'Responses'>
  supported_models: string[]
  model_mappings?: UpstreamModelMapping[]
  default_model_context?: DefaultModelContext
  model_contexts?: ModelContextConfig[]
  request_quota_window_hours: number
  request_quota_requests: number
  requests_per_minute: number
  max_concurrency: number
  priority: number
  premium_models: string[]
  protect_premium_quota: boolean
  active: boolean
  failure_count: number
  auto_managed?: boolean
  managed_source?: string | null
  last_synced_at?: number
  strip_nonstandard_chat_fields: NonstandardFieldPolicy
  dialect_preset?: string | null
  model_dialect_presets?: Record<string, string>
  runtime_state?: UpstreamRuntimeState
  route_health?: RouteHealthSnapshot
  _replace_api_keys?: boolean
}

export interface RouteHealthSnapshot {
  healthy_routes: number
  cooldown_routes: number
  half_open_routes: number
  legacy_local_admission_poisoned_routes: number
  earliest_retry_after_seconds?: number | null
  failure_classes?: Record<string, number>
}

export interface UpstreamRuntimeState {
  in_flight: number
  stale_lease_count: number
  oldest_lease_age_seconds: number
  queue_depth: number
  leaked_reclaimed_total: number
  stale_reclaimed_total: number
  /** E5.3: 租约持有时长统计与容量类计数。Redis 后端为 null/0。 */
  hold_p50_ms?: number | null
  hold_p95_ms?: number | null
  /** 本地并发闸门拒绝累计次数。 */
  capacity_reject_total?: number
  /** E1 生效次数：容量类失败本应冷却、实际跳过冷却的累计次数。 */
  route_cooldown_skipped_total?: number | null
  minute_cost: number
  minute_limit: number
  minute_percentage: number
  five_hour_cost: number
  five_hour_limit: number
  five_hour_percentage: number
  cooldown_until: number
  cooldown_remaining: number
}


export interface ModelContextConfig {
  slug: string
  context_limit: number
  output_reserve: number
  max_output_tokens: number
  context_group: string
}

export interface DefaultModelContext {
  context_limit: number
  output_reserve: number
  max_output_tokens: number
  context_group: string
}

// ============================================================================
// Downstream Types
// ============================================================================

export interface DownstreamConfig {
  id: string
  name: string
  hash: string
  plaintext_key?: string
  plaintext_key_prefix?: string
  model_allowlist: string[]
  rate_limit_enabled: boolean
  per_minute_limit: number
  max_concurrency: number
  daily_token_limit?: number
  monthly_token_limit?: number
  /** 每百万输入 token 价格（分）。与每日金额上限同时配置时启用按金额计费。 */
  input_token_price_per_million_cents?: number
  /** 每百万输出 token 价格（分）。与每日金额上限同时配置时启用按金额计费。 */
  output_token_price_per_million_cents?: number
  /** 每日金额上限（分）。与输入/输出单价同时配置时启用按金额计费。 */
  daily_cost_limit_cents?: number
  request_quota_window_hours?: number
  request_quota_requests?: number
  ip_allowlist: string[]
  expires_at?: number
  active: boolean
  billing_mode?: 'request' | 'token'
  /** 可选：按模型拆分的并发组。先匹配先生效（顺序敏感），未命中任何组的模型走 max_concurrency 全局兜底。 */
  model_concurrency_groups?: DownstreamModelConcurrencyGroup[]
  /** 管理端列表附加的用量汇总（今日/本月 token 与金额，按分计）。 */
  usage?: DownstreamUsage | null
}

/** 一个下游并发组：把全局 max_concurrency 预算按模型拆分，避免 A 模型打满阻塞 B 模型。 */
export interface DownstreamModelConcurrencyGroup {
  /** 组名，同一下游下不可重复。 */
  name: string
  /** 模型匹配模式列表（精确名或含 * 通配），同组内任一命中即算匹配。 */
  match: string[]
  /** 该组并发上限。 */
  max_concurrency: number
}

export interface DownstreamUsage {
  downstream_id: string
  today_tokens: number
  month_tokens: number
  today_cost_cents: number
  month_cost_cents: number
  /** 滚动 24 小时金额（分），与准入限额同一窗口。 */
  cost_used_24h_cents: number
  total_models: number
  active_models: number
}

// ============================================================================
// Usage Log Types
// ============================================================================

export interface UsageLog {
  id: string
  downstream_key_id: string
  upstream_key_id: string
  downstream_name?: string
  upstream_name?: string
  endpoint: string
  model: string
  api_name?: string
  inference_strength?: string
  log_type?: string
  billing_mode?: string
  request_count?: number
  user_agent?: string
  request_id: string
  status_code: number
  error_message?: string
  error_category?: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  first_token_latency_ms?: number | null
  latency_ms: number
  created_at: number
}

export interface LogsResponse {
  logs: UsageLog[]
  total: number
  page: number
  page_size: number
  total_pages: number
  mode: ResolvedLogWindow['mode']
  day?: string
  timezone: string
  start_time: number
  end_time: number
}

// ============================================================================
// Portal Types
// ============================================================================

export type ChartTimeRange = '1d' | '7d' | '30d'

export interface ResolvedLogWindow {
  mode: 'calendar_day' | 'rolling_1h'
  day?: string
  timezone: string
  start_time: number
  end_time: number
}

export interface RequestQuotaUsage {
  used: number
  limit: number
  remaining: number
  window_hours: number
  percentage: number
}

export interface TokenQuota {
  used: number
  limit: number
  remaining: number
  percentage: number
}

export interface TokenUsage {
  daily?: TokenQuota
  monthly?: TokenQuota
}

export interface DailyStats {
  day: string
  start_time: number
  total_requests: number
  total_tokens: number
  success_rate: number
}

export interface DownstreamConcurrencySnapshot {
  available: boolean
  running?: number
  waiting_upstream?: number
  admitted?: number
  limit: number
  updated_at: number
}

export interface DownstreamRuntimeItem {
  downstream_id: string
  concurrency: DownstreamConcurrencySnapshot
}

export interface DownstreamRuntimeResponse {
  items: DownstreamRuntimeItem[]
  updated_at: number
}

export interface CostDailyQuota {
  used_cents: number
  limit_cents: number
  remaining_cents: number
  percentage: number
}

export interface PortalOverview {
  quota_summary: {
    request_quota?: RequestQuotaUsage
    cost_daily?: CostDailyQuota
  }
  token_summary: {
    today: number
    this_month: number
  }
  cost_summary: {
    last_24h_cents: number
    this_month_cents: number
  }
  model_summary: {
    total_models: number
    active_models: number
  }
  concurrency: DownstreamConcurrencySnapshot
}

export interface PortalModelStat {
  model: string
  today_count: number
  month_count: number
  today_tokens: number
  month_tokens: number
  avg_latency_ms: number
  success_rate: number
}

export interface ModelContextEntry {
  context_window: number
  output_reserve: number
}

export interface PortalQuota {
  request_quota?: RequestQuotaUsage
  cost_quota?: {
    daily?: CostDailyQuota
  }
  model_allowlist: string[]
  ip_allowlist: string[]
  /// Per-model context window resolved from the upstream admin configuration.
  /// Keyed by canonical model slug. Optional: older gateways omit this.
  model_contexts?: Record<string, ModelContextEntry>
}

export interface PortalUsageHistory {
  logs: PortalUsageLog[]
  total: number
  page: number
  page_size: number
  total_pages: number
  mode: ResolvedLogWindow['mode']
  day?: string
  timezone: string
  start_time: number
  end_time: number
}

export interface PortalUsageLog {
  id: string
  endpoint: string
  model: string
  api_name?: string
  inference_strength?: string
  log_type?: string
  status_code: number
  error_category?: string | null
  first_token_latency_ms?: number | null
  latency_ms: number
  created_at: number
}

export interface PortalUsageSummary {
  time_range: ChartTimeRange
  timezone: string
  start_time: number
  end_time: number
  daily_stats: DailyStats[]
}

export interface ModelProbeSummary {
  total_channels: number
  healthy_channels: number
  offline_channels: number
  degraded_channels: number
  total_models: number
  average_latency_ms: number
}

export interface ModelProbeChannel {
  upstream_id: string
  upstream_name: string
  route_id: string
  status: 'healthy' | 'offline' | 'degraded' | string
  latency_ms: number
  model_count: number
  models: string[]
  last_probe_at: number
  error: string | null
}

export interface ModelProbeModel {
  model: string
  channel_count: number
}

export interface ModelProbeResponse {
  refreshed_at: number
  refresh_interval_seconds: number
  summary: ModelProbeSummary
  channels: ModelProbeChannel[]
  models: ModelProbeModel[]
}

export type ModelQualificationLevel = 'full' | 'adapted' | 'unusable' | 'operational_failure'

export type ModelQualificationCategory =
  | 'passed'
  | 'authentication'
  | 'rate_limit'
  | 'upstream_unavailable'
  | 'request_rejected'
  | 'model_not_found'
  | 'malformed_response'
  | 'empty_response'
  | 'timeout'
  | 'network'

export interface ModelQualificationEvidence {
  upstream_id: string
  route_id: string
  model: string
  protocol: 'ChatCompletions' | 'Responses'
  level: ModelQualificationLevel
  category: ModelQualificationCategory
  latency_ms: number
  attempted_at: number
}

export interface QualifyModelsRequest {
  apply: boolean
  upstream_ids: string[]
  downstream_id: string
  excluded_models: string[]
}

export interface QualifyModelsSummary {
  retained_models: number
  full_models: number
  adapted_models: number
  removed_models: number
  operational_failures: number
  upstreams: number
}

export interface QualifyModelsUpstreamResult {
  upstream_id: string
  retained_models: string[]
  full_models: string[]
  adapted_models: string[]
  removed_models: string[]
  operational_models: string[]
  evidence: ModelQualificationEvidence[]
}

export interface QualifyModelsResponse {
  applied: boolean
  downstream_id: string
  summary: QualifyModelsSummary
  upstreams: QualifyModelsUpstreamResult[]
  apply_summary?: {
    upstreams_updated: number
    retained_models: number
  }
}

// ============================================================================
// Troubleshooting Types
// ============================================================================

export type TroubleshootingClientProfile =
  | 'cline'
  | 'codex'
  | 'opencode'
  | 'claude_code'
  | 'hermes'
  | 'open_ai_compatible'
  | 'anthropic_compatible'

export type TroubleshootingCheck =
  | 'models'
  | 'chat'
  | 'chat_stream'
  | 'responses'
  | 'responses_stream'
  | 'messages'
  | 'messages_stream'
  | 'count_tokens'
  | 'tools'

export type TroubleshootingStepStatus = 'passed' | 'warning' | 'failed' | 'timeout'
export interface TroubleshootingLogFilter {
  [key: string]: string | number | boolean
}

export interface TroubleshootingRunRequest {
  client_profile: TroubleshootingClientProfile
  model: string
  checks: TroubleshootingCheck[]
  downstream_id?: string
}

export interface TroubleshootingStepResult {
  id: string
  label: string
  status: TroubleshootingStepStatus
  protocol: string
  http_status: number
  observed_value?: number | null
  duration_ms: number
  summary: string
  details: string
  error_category?: string | null
  suggestion: string
  copy_summary: string
  log_filter?: TroubleshootingLogFilter | null
}

export interface TroubleshootingRunResponse {
  run_id: string
  status: string
  client_profile: TroubleshootingClientProfile
  model: string
  summary?: {
    passed: number
    warning: number
    failed: number
    timeout: number
  }
  results: TroubleshootingStepResult[]
  duration_ms?: number
  copy_summary?: string
  log_filter?: string
}

export interface ActiveGatewayRequest {
  request_id: string
  downstream_id: string
  downstream_name: string
  endpoint: string
  model: string
  protocol: string
  user_agent?: string | null
  upstream_id?: string | null
  upstream_name?: string | null
  started_at: number
  last_event_at: number
  elapsed_seconds: number
  idle_seconds: number
  status: string
  phase?: string | null
  queue_position?: number | null
  error_category?: string | null
}

export interface RetryAmplificationPoint {
  downstream_id: string
  model: string
  category: string
  count: number
}

export interface RetryAmplificationResponse {
  window_seconds: number
  total: number
  category_totals: Record<string, number>
  points: RetryAmplificationPoint[]
}

export interface ActiveGatewayRequestsResponse {
  active_requests: ActiveGatewayRequest[]
}

export interface CompatibilityMatrixRunRequest {
  downstream_id: string
  client_profiles?: TroubleshootingClientProfile[]
  models?: string[]
}

export interface CompatibilityMatrixCell {
  client_family: TroubleshootingClientProfile
  model_slug: string
  endpoint: string
  selected_upstream_id?: string | null
  selected_upstream_name?: string | null
  selected_upstream_protocol?: string | null
  protocol_transition?: string | null
  fallback_stage?: string | null
  profile_state?: string
  profile_currentness?: ProfileCurrentness
  profile_age_seconds?: number | null
  probe_version?: number | null
  runtime_model_slug?: string
  adapter_set?: string[]
  dialect_retry_count?: number
  optional_downgrades?: string[]
  check_results?: Array<{
    id: string
    passed: boolean
    codes: string[]
    observed_value?: number | null
  }>
  first_meaningful_event_ms?: number | null
  status: TroubleshootingStepStatus
  http_status: number
  error_category?: string | null
  summary: string
  details: string
  duration_ms: number
}

export interface CompatibilityMatrixRunResponse {
  run_id: string
  downstream_id: string
  models: string[]
  client_profiles: TroubleshootingClientProfile[]
  summary: {
    passed: number
    warning: number
    failed: number
  }
  cells: CompatibilityMatrixCell[]
  duration_ms: number
  copy_summary: string
}

export type EvidenceState = 'supported' | 'rejected' | 'unobserved'
export type CapabilitySource = 'override' | 'probe' | 'policy' | 'baseline'
export type ProfileCurrentness = 'current' | 'stale' | 'missing'
export type CapabilityWireProtocol = 'chat_completions' | 'responses'
export type JsonPrimitive = string | number | boolean | null
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue }

export interface ProbeAllCapabilitiesRequest {
  upstream_ids?: string[]
  models?: string[]
  mode?: 'reasoning' | 'full'
}

export type CapabilityProbeCandidateState =
  | 'queued'
  | 'reused'
  | 'running'
  | 'completed'
  | 'failed'
  | 'cooldown_skipped'
  | 'superseded'

export interface CapabilityProbeCandidateSummary {
  upstream_id: string
  route_id: string
  exposed_model_slug: string
  runtime_model_slug: string
  protocol: CapabilityWireProtocol
  state: CapabilityProbeCandidateState
  diagnostic_code?: string
}

export interface ProbeAllCapabilitiesResponse {
  batch_id: string
  configuration_revision: number
  started_at: number
  queued_routes: number
  reused_routes: number
  models: string[]
  candidates: CapabilityProbeCandidateSummary[]
}

export interface CapabilityProbeBatchStatus extends ProbeAllCapabilitiesResponse {
  estimated_remaining_seconds: number | null
  terminal_at: number | null
}

export type CapabilityRouteProbeOutcome =
  | 'accepted'
  | 'rejected'
  | 'operational_failure'
  | 'deferred'
  | 'pending'

export interface CapabilityRouteDiscoverySummary {
  upstream_id: string
  route_id: string
  runtime_model_slug: string
  protocol: CapabilityWireProtocol
  outcome: CapabilityRouteProbeOutcome
  accepted_reasoning_levels: string[]
  reasoning_source: CapabilitySource
  managed_reasoning_override: boolean
  http_status: number | null
  operational_code: string | null
  last_attempt_at: number | null
  next_probe_at: number | null
}

export interface CapabilityModelDiscoverySummary {
  exposed_model_slug: string
  verified_reasoning_levels: string[]
  routes: CapabilityRouteDiscoverySummary[]
}

export interface CapabilityDiscoveryResponse {
  models: CapabilityModelDiscoverySummary[]
}

export type ReasoningEffortLevel =
  | 'none'
  | 'low'
  | 'medium'
  | 'high'
  | 'xhigh'
  | 'max'
export type ReasoningOverrideScope = 'route' | 'model_routes'

export interface UpdateReasoningOverridesRequest {
  upstream_id: string
  route_id: string
  exposed_model_slug: string
  runtime_model_slug: string
  protocol: CapabilityWireProtocol
  levels: ReasoningEffortLevel[]
  scope: ReasoningOverrideScope
}

export interface UpdateReasoningOverridesResponse {
  ok: true
  configuration_revision: number
  affected_route_count: number
  affected_route_ids: string[]
}

export type ModelMappingStatus = 'effective' | 'partial' | 'inactive'
export type ModelMappingStatusReason =
  | 'eligible_routes_available'
  | 'some_routes_ineligible'
  | 'upstream_inactive'
  | 'upstream_model_unavailable'
  | 'no_key_for_upstream_model'
  | 'no_eligible_routes'

export interface ModelMappingStatusSummary {
  upstream_id: string
  upstream_model: string
  downstream_model: string
  status: ModelMappingStatus
  reason: ModelMappingStatusReason
  eligible_routes: number
  configured_routes: number
  unverified_routes: number
}

export interface ModelMappingStatusesResponse {
  mappings: ModelMappingStatusSummary[]
}

export interface CapabilityConfigurationDocument {
  schema_version: number
  revision: number
  policies?: JsonValue[]
  route_overrides?: JsonValue[]
  route_tags?: JsonValue[]
  bundles?: JsonValue[]
  compatibility_expectations?: JsonValue[]
  probe?: JsonValue
}

export interface DialectProfileKey {
  upstream_id: string
  route_id: string
  runtime_model_slug: string
  protocol: 'chat_completions' | 'responses'
}

export interface DialectProfileEvidence {
  capabilities: { [capability: string]: EvidenceState }
  extensions: { [extension: string]: EvidenceState }
  codes: string[]
}

export interface DialectProfileSources {
  capabilities: { [capability: string]: 'probe' | 'baseline' }
  extensions: { [extension: string]: 'probe' }
}

export interface DialectProfileEventSummary {
  types: string[]
}

export interface DialectProfileStatusSummary {
  http_status: number | null
  operational_code: string | null
}

export interface ResolvedCapabilityValue {
  state: EvidenceState
  source: CapabilitySource
}

export interface DialectProfileReasoningSummary {
  controls: { [field: string]: string[] }
  carrier: string | null
}

export interface DialectProfileSummary {
  key: DialectProfileKey
  upstream_id: string
  runtime_model_slug: string
  protocol: 'chat_completions' | 'responses'
  state: 'verified' | 'partial' | 'unsupported' | 'unknown'
  currentness: ProfileCurrentness
  age_seconds: number | null
  profile_age_seconds: number | null
  probe_version: number | null
  fingerprint: string | null
  reasoning?: DialectProfileReasoningSummary
  sources: DialectProfileSources
  evidence: DialectProfileEvidence
  event_summary: DialectProfileEventSummary
  status_summary: DialectProfileStatusSummary
}

export interface ResolvedCapabilityConflictSide {
  code: string
  state: EvidenceState
}

export interface ResolvedCapabilityConflict {
  subject: string
  probe: ResolvedCapabilityConflictSide
  policy: ResolvedCapabilityConflictSide
  winner: CapabilitySource
}

export interface ResolvedCapabilitiesResponse {
  configuration_revision: number
  configuration_fingerprint: string | null
  capabilities: { [capability: string]: ResolvedCapabilityValue }
  profile_age_seconds: number | null
  profile_currentness: ProfileCurrentness
  profile_state: 'verified' | 'partial' | 'unsupported' | 'unknown'
  profile: {
    currentness: ProfileCurrentness
    state: 'verified' | 'partial' | 'unsupported' | 'unknown'
    age_seconds: number | null
    fingerprint: string | null
  }
  field_sources: { [field: string]: CapabilitySource }
  token: {
    field: 'max_tokens' | 'max_completion_tokens' | 'max_output_tokens' | 'omit'
    source: CapabilitySource
  }
  reasoning: {
    mode: 'off' | 'optional' | 'fixed_on'
    carrier: 'none' | 'reasoning_content' | 'responses_reasoning_item' | 'messages_thinking'
    control_field: string | null
    source: CapabilitySource
  }
  extensions: {
    ids: string[]
    source: CapabilitySource
  }
  conflicts: ResolvedCapabilityConflict[]
}

// ============================================================================
// Runtime Settings Types
// ============================================================================

export interface RuntimeSettings {
  app_name: string
  usage_log_archive_max_files: number
  usage_log_retention_days: number
  admin_logs_page_size_max: number
  admin_upstream_timeout_seconds: number
  troubleshooting_check_timeout_seconds: number
  model_probe_refresh_interval_seconds: number
  upstream_model_auto_discovery_enabled: boolean
  upstream_model_key_sync_interval_seconds: number
  capability_probe_queue_capacity: number
  capability_probe_request_timeout_seconds: number
  capability_probe_reasoning_timeout_seconds: number
  capability_probe_concurrency: number
  automatic_capability_probes_enabled: boolean
  model_case_insensitive_matching: boolean
  tool_call_merge_strict: boolean
  tool_arguments_strict: boolean
  upstream_rate_limit_default_retry_seconds: number
  routing_affinity_enabled: boolean
  routing_affinity_ttl_seconds: number
  routing_affinity_escape_pressure_ratio: number
  upstream_hedge_enabled: boolean
  upstream_hedge_delay_ms: number
  upstream_hedge_interval_ms: number
  upstream_hedge_max_extra_attempts: number
  upstream_same_route_retry_enabled: boolean
  upstream_transient_same_route_retry_enabled: boolean
  upstream_shared_host_failure_domain_enabled: boolean
  upstream_common_mode_same_host_transient_enabled: boolean
  upstream_capacity_failure_cooldown_enabled: boolean
  upstream_transient_route_cooldown_base_seconds: number
  upstream_transient_route_cooldown_max_seconds: number
  upstream_transient_route_cooldown_max_step: number
  upstream_route_health_half_open_ttl_seconds: number
  upstream_route_half_open_exclusive_window_ms: number
  upstream_route_half_open_busy_max_rounds: number
  upstream_retry_after_cap_seconds: number
  upstream_retry_after_cooldown_cap_seconds: number
  upstream_credentials_first_strike_seconds: number
  upstream_error_body_excerpt_enabled: boolean
  upstream_error_body_excerpt_max_chars: number
  upstream_route_exhaustion_retry_enabled: boolean
  upstream_route_exhaustion_retry_max_wait_ms: number
  upstream_route_exhaustion_retry_max_rounds: number
  upstream_route_exhaustion_budget_alignment_enabled: boolean
  upstream_route_exhaustion_alignment_truncated_enabled: boolean
  upstream_transient_last_resort_probe_enabled: boolean
  upstream_common_mode_breaker_threshold: number
  upstream_common_mode_transient_threshold: number
  upstream_continuation_pin_escape_enabled: boolean
  upstream_local_lease_ttl_seconds: number
  upstream_lease_stale_after_ms: number
  upstream_account_queue_enabled: boolean
  upstream_account_queue_max_depth: number
  upstream_account_queue_max_wait_ms: number
  upstream_account_queue_adaptive_budget_enabled: boolean
  upstream_local_gate_max_wait_ms: number
  upstream_local_gate_fast_fail_enabled: boolean
  upstream_local_gate_distinct_error_code_enabled: boolean
  stream_decode_error_code_split_enabled: boolean
  stream_max_skipped_bad_frames: number
  default_upstream_max_concurrency: number
  downstream_lease_ttl_seconds: number
  upstream_concurrency_recovery_max_wait_ms: number
  upstream_concurrency_recovery_max_rounds: number
  upstream_concurrency_probe_delays_ms: number[]
  upstream_http_pool_max_idle_per_host: number
  upstream_user_agent: string
  upstream_connect_timeout_seconds: number
  upstream_response_header_timeout_seconds: number
  upstream_stream_keepalive_interval_seconds: number
  upstream_stream_idle_timeout_seconds: number
  upstream_stream_max_duration_seconds: number
  upstream_first_semantic_output_timeout_seconds: number
  upstream_first_output_warn_after_seconds: number
  gateway_request_body_limit_mb: number
}

export type RuntimeSettingKey = keyof RuntimeSettings
export type RuntimeSettingsSource = 'startup' | 'persisted'

export interface RuntimeSettingsResponse {
  schema_version: number
  revision: number
  source: RuntimeSettingsSource
  settings: RuntimeSettings
  restart_required: boolean
  restart_required_fields: RuntimeSettingKey[]
}

export interface RuntimeSettingsUpdateResponse extends RuntimeSettingsResponse {
  applied_immediately: RuntimeSettingKey[]
}

export interface UpdateRuntimeSettingsRequest {
  expected_revision: number
  settings: RuntimeSettings
}

// ============================================================================
// Announcement Types
// ============================================================================

export type AnnouncementLevel = 'info' | 'success' | 'warning' | 'error'

export interface Announcement {
  id: string
  title: string
  content: string
  level: AnnouncementLevel
  active: boolean
  updated_at: number
}

// ============================================================================
// Model Alias Types
// ============================================================================

export interface ModelAliasRule {
  canonical: string
  aliases: string[]
}
