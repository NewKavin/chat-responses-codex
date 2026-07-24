import axios, { type AxiosResponse } from 'axios'
import type {
  Announcement,
  AnnouncementLevel,
  ActiveGatewayRequestsResponse,
  CompatibilityMatrixRunRequest,
  CompatibilityMatrixRunResponse,
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
  QualifyModelsRequest,
  QualifyModelsResponse,
  ResolvedCapabilitiesResponse,
  TroubleshootingRunRequest,
  TroubleshootingRunResponse,
  ApiKeyModelConfig,
  KeyModelDiscoveryResult,
  UpstreamConfig
} from '@/types'


export interface BatchCreateUpstreamPayload {
  name: string
  base_url: string
  keys: string[]
  supported_models: string[]
  api_key_models: ApiKeyModelConfig[]
  protocol?: string
  protocols?: string[]
  active?: boolean
  strip_nonstandard_chat_fields?: boolean
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
  route_id: string
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

  getModelProbe: () => adminHttp.get<ModelProbeResponse>('/admin/model-probe'),

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

  // Logs
  getLogs: (params?: {
    page?: number
    page_size?: number
    status_code?: number
    status_codes?: string
    error_category?: string
    error_categories?: string
    model?: string
    time_range?: string
    start_time?: number
    end_time?: number
  }) => adminHttp.get<LogsResponse>('/admin/logs', { params }),

  // Models
  getModels: () => adminHttp.get<{ models: string[] }>('/admin/models'),

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

  // Announcements
  getAnnouncement: () => adminHttp.get<AnnouncementResponse>('/admin/announcement'),
  updateAnnouncement: (data: UpdateAnnouncementRequest) =>
    adminHttp.put<AnnouncementResponse>('/admin/announcement', data)
}
