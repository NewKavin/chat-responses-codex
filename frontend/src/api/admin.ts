import axios, { type AxiosResponse } from 'axios'
import type {
  Announcement,
  AnnouncementLevel,
  ActiveGatewayRequestsResponse,
  CompatibilityMatrixRunRequest,
  CompatibilityMatrixRunResponse,
  CapabilityDiscoveryResponse,
  CapabilityConfigurationDocument,
  DashboardAnalyticsRange,
  DashboardData,
  DashboardSummaryResponse,
  DialectProfileSummary,
  DownstreamConfig,
  LoginRequest,
  LoginResponse,
  LogsResponse,
  ModelProbeResponse,
  ModelMappingStatusesResponse,
  CapabilityProbeBatchStatus,
  ProbeAllCapabilitiesRequest,
  ProbeAllCapabilitiesResponse,
  QualifyModelsRequest,
  QualifyModelsResponse,
  ResolvedCapabilitiesResponse,
  RuntimeSettingsResponse,
  RuntimeSettingsUpdateResponse,
  TroubleshootingRunRequest,
  TroubleshootingRunResponse,
  UpdateReasoningOverridesRequest,
  UpdateReasoningOverridesResponse,
  UpdateRuntimeSettingsRequest,
  ApiKeyModelConfig,
  KeyModelDiscoveryResult,
  UpstreamConfig,
  DownstreamRuntimeResponse,
  NonstandardFieldPolicy,
  ModelAliasRule
} from '@/types'


export interface BatchCreateUpstreamPayload {
  name: string
  remark?: string
  continuation_provider_group?: string | null
  base_url: string
  keys: string[]
  supported_models: string[]
  api_key_models: ApiKeyModelConfig[]
  protocol?: string
  protocols?: string[]
  max_concurrency?: number
  active?: boolean
  strip_nonstandard_chat_fields?: NonstandardFieldPolicy
  dialect_preset?: string | null
  model_dialect_presets?: Record<string, string>
}

export interface BatchCreateUpstreamResult {
  keys_count?: number
  created: number
  failed: number
  total: number
  results: Array<KeyModelDiscoveryResult & {
    id?: string
    name?: string
  }>
}

export interface DiscoverUpstreamModelsPayload {
  base_url: string
  keys: string[]
}

export interface DiscoverUpstreamModelsResult {
  models: string[]
  failed: number
  total: number
  results: KeyModelDiscoveryResult[]
  message?: string
}

export function formatModelDiscoveryFailure(result: DiscoverUpstreamModelsResult): string {
  const summary = result.message?.trim() || '所有 Key 获取模型均失败'
  const details = result.results
    .filter(item => item.error?.trim())
    .map(item => {
      let message = item.error!.trim()
      switch (item.error_code) {
        case 'http_status': {
          const status = Number(item.http_status)
          if (Number.isInteger(status) && status >= 100 && status <= 599) {
            message = `已收到上游 HTTP ${status}；可手动填写模型后继续保存`
          }
          break
        }
        case 'connection':
          message = '无法连接上游；请检查 DNS、TLS/自定义 CA、防火墙和源站可达性'
          break
        case 'timeout':
          message = '上游模型列表请求超时；请检查源站响应时间'
          break
        case 'invalid_json':
          message = '上游模型列表不是有效 JSON；可手动填写模型后继续保存'
          break
        case 'missing_data':
          message = '上游模型列表缺少 data；可手动填写模型后继续保存'
          break
        case 'empty_models':
          message = '上游未返回模型；可手动填写模型后继续保存'
          break
        case 'request':
          message = '上游模型列表请求失败；可手动填写模型后继续保存'
          break
      }
      return `Key #${item.key_index + 1}: ${message}`
    })
    .join('；')
  return details ? `${summary}：${details}` : summary
}

export function reconcileKeyModelMappings(
  keys: string[],
  previous: ApiKeyModelConfig[] = [],
  results: KeyModelDiscoveryResult[] = []
): ApiKeyModelConfig[] {
  const previousByKey = new Map<string, string[]>()
  for (const mapping of previous) {
    const key = String(mapping.api_key || '').trim()
    if (!key) continue
    const models = previousByKey.get(key) || []
    for (const model of mapping.supported_models || []) {
      const normalized = String(model || '').trim()
      if (normalized && !models.includes(normalized)) models.push(normalized)
    }
    previousByKey.set(key, models)
  }

  const discoveredByKey = new Map<string, string[]>()
  for (const result of results) {
    const key = keys[result.key_index]?.trim()
    if (!key || result.error || !Array.isArray(result.model_list) || result.model_list.length === 0) {
      continue
    }
    const models = discoveredByKey.get(key) || []
    for (const model of result.model_list) {
      const normalized = String(model || '').trim()
      if (normalized && !models.includes(normalized)) models.push(normalized)
    }
    discoveredByKey.set(key, models)
  }

  const seen = new Set<string>()
  const mappings: ApiKeyModelConfig[] = []
  for (const rawKey of keys) {
    const key = rawKey.trim()
    if (!key || seen.has(key)) continue
    seen.add(key)
    mappings.push({
      api_key: key,
      supported_models: discoveredByKey.get(key) || previousByKey.get(key) || []
    })
  }
  return mappings
}

const uniqueModels = (models: string[]): string[] => {
  const seen = new Set<string>()
  const normalized: string[] = []
  for (const rawModel of models) {
    const model = String(rawModel || '').trim()
    if (model && !seen.has(model)) {
      seen.add(model)
      normalized.push(model)
    }
  }
  return normalized
}

export function mergeDiscoveredModelCandidates(
  selected: string[],
  previousCandidates: string[],
  results: KeyModelDiscoveryResult[]
): string[] {
  return uniqueModels([
    ...selected,
    ...previousCandidates,
    ...results.flatMap(result => result.error ? [] : (result.model_list || []))
  ]).sort()
}

export function buildSelectedKeyModelMappings(
  keys: string[],
  selectedModels: string[],
  previous: ApiKeyModelConfig[] = [],
  results: KeyModelDiscoveryResult[] = []
): ApiKeyModelConfig[] {
  const selected = uniqueModels(selectedModels)
  const selectedSet = new Set(selected)
  const mappings = reconcileKeyModelMappings(keys, previous, results).map(mapping => ({
    api_key: mapping.api_key,
    supported_models: mapping.supported_models.filter(model => selectedSet.has(model))
  }))
  const assigned = new Set(mappings.flatMap(mapping => mapping.supported_models))
  const assertedModels = selected.filter(model => !assigned.has(model))
  for (const mapping of mappings) {
    mapping.supported_models = uniqueModels([
      ...mapping.supported_models,
      ...assertedModels
    ])
  }
  return mappings
}
export interface DashboardViewResponse {
  dashboard: DashboardData
  analytics: DashboardAnalyticsRange
}

export interface AnnouncementResponse {
  announcement: Announcement | null
}

export type CapabilityExportResponse = CapabilityConfigurationDocument

export interface DialectProfilesResponse {
  profiles: DialectProfileSummary[]
}

export interface ResolvedCapabilitiesParams {
  upstream_id: string
  route_id: string
  model: string
  protocol: 'chat_completions' | 'responses'
}

export interface QueueDialectProbeRequest {
  upstream_id: string
  route_id?: string
  runtime_model_slug: string
  protocol: 'chat_completions' | 'responses'
}

export interface QueueDialectProbeResponse {
  queued: true
}

export interface UpdateAnnouncementRequest {
  title: string
  content: string
  level: AnnouncementLevel
  active: boolean
}

export const createAdminApiClient = () =>
  axios.create({
    baseURL: '/api',
    timeout: 10000
  })

export const hasUsableAdminToken = (token: unknown): token is string =>
  typeof token === 'string' && token.trim().length > 0

export const splitDashboardResponse = (
  response: DashboardSummaryResponse
): DashboardViewResponse => {
  const { analytics, ...dashboard } = response
  return {
    dashboard,
    analytics
  }
}

export const adminHttp = createAdminApiClient()

// 请求拦截器：添加 JWT token
adminHttp.interceptors.request.use(config => {
  const token = localStorage.getItem('admin_token')
  if (hasUsableAdminToken(token)) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

// 响应拦截器：只处理 401 错误
adminHttp.interceptors.response.use(
  response => response,
  error => {
    if (error.response?.status === 401) {
      localStorage.removeItem('admin_token')
      window.location.hash = '#/admin/login'
    }
    return Promise.reject(error)
  }
)

export const adminApi = {
  // Authentication
  login: (data: LoginRequest) => adminHttp.post<LoginResponse>('/admin/login', data),

  // Dashboard
  getDashboard: (range?: string): Promise<AxiosResponse<DashboardViewResponse>> =>
    adminHttp
      .get<DashboardSummaryResponse>('/admin/dashboard', {
        params: range ? { range } : undefined
      })
      .then(response => ({
        ...response,
        data: splitDashboardResponse(response.data)
      })),

  getModelProbe: () =>
    adminHttp.get<ModelProbeResponse>('/admin/model-probe', { timeout: 60000 }),

  // Upstreams
  getUpstreams: () => adminHttp.get<UpstreamConfig[]>('/admin/upstreams'),
  createUpstream: (data: Partial<UpstreamConfig>) =>
    adminHttp.post<UpstreamConfig>('/admin/upstreams', data),
  createUpstreamsBatch: (data: BatchCreateUpstreamPayload) =>
    adminHttp.post<BatchCreateUpstreamResult>('/admin/upstreams/batch', data),
  discoverUpstreamModels: (data: DiscoverUpstreamModelsPayload) =>
    adminHttp.post<DiscoverUpstreamModelsResult>('/admin/upstreams/discover-models', data),
  qualifyUpstreamModels: (data: QualifyModelsRequest) =>
    adminHttp.post<QualifyModelsResponse>('/admin/upstreams/qualify-models', data, {
      timeout: 10 * 60 * 1000
    }),
  getUpstream: (id: string) => adminHttp.get<UpstreamConfig>(`/admin/upstreams/${id}`),
  updateUpstream: (id: string, data: Partial<UpstreamConfig>) =>
    adminHttp.put<UpstreamConfig>(`/admin/upstreams/${id}`, data),
  deleteUpstream: (id: string) => adminHttp.delete(`/admin/upstreams/${id}`),
  toggleUpstream: (id: string) => adminHttp.post<{ active: boolean }>(`/admin/upstreams/${id}/toggle`),
  resetUpstreamRouteHealth: (id: string) =>
    adminHttp.post<{ upstream_id: string; cleared_routes: number }>(
      `/admin/upstreams/${id}/route-health/reset`
    ),
  batchToggleUpstreams: (ids: string[], active: boolean) =>
    adminHttp.post<{ updated: number; failed: Array<{ id: string; error: string }> }>(
      '/admin/upstreams/batch-toggle',
      { ids, active }
    ),
  batchDeleteUpstreams: (ids: string[]) =>
    adminHttp.post<{ deleted: number; failed: Array<{ id: string; error: string }> }>(
      '/admin/upstreams/batch-delete',
      { ids }
    ),

  // Downstreams
  getDownstreams: (params?: { status?: string; lifecycle?: string; search?: string }) =>
    adminHttp.get<DownstreamConfig[]>('/admin/downstreams', { params }),
  createDownstream: (data: Partial<DownstreamConfig>) =>
    adminHttp.post<DownstreamConfig>('/admin/downstreams', data),
  getDownstream: (id: string) => adminHttp.get<DownstreamConfig>(`/admin/downstreams/${id}`),
  updateDownstream: (id: string, data: Partial<DownstreamConfig>) =>
    adminHttp.put<DownstreamConfig>(`/admin/downstreams/${id}`, data),
  deleteDownstream: (id: string) => adminHttp.delete(`/admin/downstreams/${id}`),
  toggleDownstream: (id: string) => adminHttp.post<{ active: boolean }>(`/admin/downstreams/${id}/toggle`),
  rotateDownstream: (id: string) => adminHttp.post<{ plaintext_key: string }>(`/admin/downstreams/${id}/rotate`),
  batchSetDownstreamMode: (data: {
    ids: string[]
    billing_mode?: 'request' | 'token'
    daily_token_limit?: number | null
    daily_cost_limit_cents?: number | null
    input_token_price_per_million_cents?: number | null
    output_token_price_per_million_cents?: number | null
    request_quota_window_hours?: number | null
    request_quota_requests?: number | null
  }) => adminHttp.post<{ updated: number; failed: Array<{ id: string; error: string }> }>(
    '/admin/downstreams/batch-mode',
    data
  ),

  getDownstreamRuntime: () =>
    adminHttp.get<DownstreamRuntimeResponse>('/admin/downstreams/runtime'),

  // Logs
  getLogs: (params?: {
    page?: number
    page_size?: number
    status_code?: number
    status_codes?: string
    error_category?: string
    error_categories?: string
    model?: string
    downstream_id?: string
    upstream_id?: string
    day?: string
    time_range?: '1h'
  }) => adminHttp.get<LogsResponse>('/admin/logs', { params }),

  // Models
  getModels: (params?: { scope?: 'visible' }) =>
    adminHttp.get<{ models: string[] }>('/admin/models', { params }),

  // Troubleshooting
  runTroubleshooting: (data: TroubleshootingRunRequest) =>
    adminHttp.post<TroubleshootingRunResponse>('/admin/troubleshooting/run', data),
  runCompatibilityMatrix: (data: CompatibilityMatrixRunRequest) =>
    adminHttp.post<CompatibilityMatrixRunResponse>('/admin/troubleshooting/matrix/run', data),
  getActiveTroubleshootingRequests: () =>
    adminHttp.get<ActiveGatewayRequestsResponse>('/admin/troubleshooting/active-requests'),
  exportCapabilities: () =>
    adminHttp.get<CapabilityExportResponse>('/admin/capabilities/export'),
  importCapabilities: (data: CapabilityConfigurationDocument) =>
    adminHttp.post<{ ok: true }>('/admin/capabilities/import', data),
  getDialectProfiles: () =>
    adminHttp.get<DialectProfilesResponse>('/admin/capabilities/profiles'),
  getResolvedCapabilities: (params: ResolvedCapabilitiesParams) =>
    adminHttp.get<ResolvedCapabilitiesResponse>('/admin/capabilities/resolved', { params }),
  queueDialectProbe: (data: QueueDialectProbeRequest) =>
    adminHttp.post<QueueDialectProbeResponse>('/admin/capabilities/probe', data),
  probeAllCapabilities: (data: ProbeAllCapabilitiesRequest = {}) =>
    adminHttp.post<ProbeAllCapabilitiesResponse>('/admin/capabilities/probe-all', data),
  getCapabilityProbeBatch: (batchId: string, timeoutMs?: number) =>
    timeoutMs === undefined
      ? adminHttp.get<CapabilityProbeBatchStatus>(`/admin/capabilities/probe-batches/${batchId}`)
      : adminHttp.get<CapabilityProbeBatchStatus>(`/admin/capabilities/probe-batches/${batchId}`, {
        timeout: timeoutMs
      }),
  getCapabilityDiscovery: (timeoutMs?: number) =>
    timeoutMs === undefined
      ? adminHttp.get<CapabilityDiscoveryResponse>('/admin/capabilities/discovery')
      : adminHttp.get<CapabilityDiscoveryResponse>('/admin/capabilities/discovery', {
        timeout: timeoutMs
      }),
  updateReasoningOverrides: (data: UpdateReasoningOverridesRequest) =>
    adminHttp.put<UpdateReasoningOverridesResponse>(
      '/admin/capabilities/reasoning-overrides',
      data
    ),

  // Runtime settings
  getRuntimeSettings: () =>
    adminHttp.get<RuntimeSettingsResponse>('/admin/runtime-settings'),
  updateRuntimeSettings: (data: UpdateRuntimeSettingsRequest) =>
    adminHttp.put<RuntimeSettingsUpdateResponse>('/admin/runtime-settings', data),

  // Announcements
  getAnnouncement: () => adminHttp.get<AnnouncementResponse>('/admin/announcement'),
  updateAnnouncement: (data: UpdateAnnouncementRequest) =>
    adminHttp.put<AnnouncementResponse>('/admin/announcement', data),

  // Model Aliases
  getModelAliases: () =>
    adminHttp.get<{ model_aliases: ModelAliasRule[] }>('/admin/model-aliases'),
  updateModelAliases: (data: { model_aliases: ModelAliasRule[] }) =>
    adminHttp.put<{ success: boolean }>('/admin/model-aliases', data),
  getModelMappingStatuses: () =>
    adminHttp.get<ModelMappingStatusesResponse>('/admin/model-mappings/status')
}
