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

export interface UpstreamConfig {
  id: string
  name: string
  base_url: string
  api_key: string
  api_keys?: string[]
  api_key_models?: ApiKeyModelConfig[]
  protocol: 'ChatCompletions' | 'Responses'
  protocols?: Array<'ChatCompletions' | 'Responses'>
  supported_models: string[]
  default_model_context?: DefaultModelContext
  model_contexts?: ModelContextConfig[]
  request_quota_window_hours: number
  request_quota_requests: number
  requests_per_minute: number
  max_concurrency: number
  model_request_costs: ModelRequestCost[]
  priority: number
  premium_models: string[]
  protect_premium_quota: boolean
  active: boolean
  failure_count: number
  auto_managed?: boolean
  managed_source?: string | null
  last_synced_at?: number
  strip_nonstandard_chat_fields: boolean
  runtime_state?: UpstreamRuntimeState
  _replace_api_keys?: boolean
}

export interface UpstreamRuntimeState {
  in_flight: number
  minute_cost: number
  minute_limit: number
  minute_percentage: number
  five_hour_cost: number
  five_hour_limit: number
  five_hour_percentage: number
  cooldown_until: number
  cooldown_remaining: number
}

export interface ModelRequestCost {
  slug: string
  cost: number
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
  request_quota_window_hours?: number
  request_quota_requests?: number
  ip_allowlist: string[]
  expires_at?: number
  active: boolean
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
  latency_ms: number
  created_at: number
}

export interface LogsResponse {
  logs: UsageLog[]
  total: number
  page: number
  page_size: number
  total_pages: number
}

// ============================================================================
// Portal Types
// ============================================================================

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
  date: number
  total_requests: number
  total_tokens: number
  success_rate: number
}

export interface PortalOverview {
  quota_summary: {
    request_quota?: RequestQuotaUsage
    token_daily?: TokenQuota
    token_monthly?: TokenQuota
  }
  token_summary: {
    today: number
    this_month: number
  }
  model_summary: {
    total_models: number
    active_models: number
  }
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
  token_quota?: {
    daily?: TokenQuota
    monthly?: TokenQuota
  }
  model_allowlist: string[]
  ip_allowlist: string[]
  /// Per-model context window resolved from the upstream admin configuration.
  /// Keyed by canonical model slug. Optional: older gateways omit this.
  model_contexts?: Record<string, ModelContextEntry>
}

export interface PortalUsageHistory {
  daily_stats: DailyStats[]
  recent_logs: UsageLog[]
  recent_logs_total: number
  recent_logs_page: number
  recent_logs_page_size: number
  recent_logs_total_pages: number
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
  key_prefix: string
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
  duration_ms: number
  summary: string
  details: string
  error_category?: string | null
  suggestion: string
  copy_summary: string
  log_filter?: Record<string, unknown> | null
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
  error_category?: string | null
}

export interface ActiveGatewayRequestsResponse {
  active_requests: ActiveGatewayRequest[]
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
