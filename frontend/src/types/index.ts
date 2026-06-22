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
  failure_categories: Array<{
    name: string
    value: number
  }>
  user_agent_clusters: Array<{
    name: string
    value: number
  }>
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
  runtime_state?: UpstreamRuntimeState
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
  context_group: string
}

export interface DefaultModelContext {
  context_limit: number
  output_reserve: number
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

export interface PortalQuota {
  request_quota?: RequestQuotaUsage
  token_quota?: {
    daily?: TokenQuota
    monthly?: TokenQuota
  }
  model_allowlist: string[]
  ip_allowlist: string[]
}

export interface PortalUsageHistory {
  daily_stats: DailyStats[]
  recent_logs: UsageLog[]
  recent_logs_total: number
  recent_logs_page: number
  recent_logs_page_size: number
  recent_logs_total_pages: number
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
