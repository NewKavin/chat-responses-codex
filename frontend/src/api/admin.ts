import axios, { type AxiosResponse } from 'axios'
import type {
  Announcement,
  AnnouncementLevel,
  ActiveGatewayRequestsResponse,
  DashboardAnalyticsRange,
  DashboardData,
  DashboardSummaryResponse,
  DownstreamConfig,
  LoginRequest,
  LoginResponse,
  LogsResponse,
  ModelProbeResponse,
  TroubleshootingRunRequest,
  TroubleshootingRunResponse,
  UpstreamConfig
} from '@/types'


export interface BatchCreateUpstreamPayload {
  name: string
  base_url: string
  keys: string[]
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
  results: Array<{
    id?: string
    name?: string
    key_prefix?: string
    models?: number
    model_list?: string[]
    error?: string
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
  results: Array<{
    key_prefix?: string
    models?: number
    model_list?: string[]
    error?: string
  }>
  message?: string
}
export interface DashboardViewResponse {
  dashboard: DashboardData
  analytics: DashboardAnalyticsRange
}

export interface AnnouncementResponse {
  announcement: Announcement | null
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
  getActiveTroubleshootingRequests: () =>
    adminHttp.get<ActiveGatewayRequestsResponse>('/admin/troubleshooting/active-requests'),

  // Announcements
  getAnnouncement: () => adminHttp.get<AnnouncementResponse>('/admin/announcement'),
  updateAnnouncement: (data: UpdateAnnouncementRequest) =>
    adminHttp.put<AnnouncementResponse>('/admin/announcement', data)
}
