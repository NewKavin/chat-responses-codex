import axios, { type AxiosResponse } from 'axios'
import type {
  LoginRequest,
  LoginResponse,
  DashboardData,
  DashboardAnalyticsRange,
  DashboardSummaryResponse,
  UpstreamConfig,
  DownstreamConfig,
  LogsResponse
} from '@/types'

export const createAdminApiClient = () =>
  axios.create({
    baseURL: '/api',
    timeout: 10000
  })

export const hasUsableAdminToken = (token: unknown): token is string =>
  typeof token === 'string' && token.trim().length > 0

export interface DashboardViewResponse {
  dashboard: DashboardData
  analytics: DashboardAnalyticsRange
}

export const splitDashboardResponse = (
  response: DashboardSummaryResponse
): DashboardViewResponse => {
  const { analytics, ...dashboard } = response
  return {
    dashboard,
    analytics
  }
}

const api = createAdminApiClient()

// 请求拦截器：添加 JWT token
api.interceptors.request.use(config => {
  const token = localStorage.getItem('admin_token')
  if (hasUsableAdminToken(token)) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

// 响应拦截器：只处理 401 错误
api.interceptors.response.use(
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
  login: (data: LoginRequest) => api.post<LoginResponse>('/admin/login', data),
  
  // Dashboard
  getDashboard: (range?: string): Promise<AxiosResponse<DashboardViewResponse>> =>
    api
      .get<DashboardSummaryResponse>('/admin/dashboard', {
        params: range ? { range } : undefined
      })
      .then(response => ({
        ...response,
        data: splitDashboardResponse(response.data)
      })),
  
  // Upstreams
  getUpstreams: () => api.get<UpstreamConfig[]>('/admin/upstreams'),
  createUpstream: (data: Partial<UpstreamConfig>) => api.post<UpstreamConfig>('/admin/upstreams', data),
  getUpstream: (id: string) => api.get<UpstreamConfig>(`/admin/upstreams/${id}`),
  updateUpstream: (id: string, data: Partial<UpstreamConfig>) => api.put<UpstreamConfig>(`/admin/upstreams/${id}`, data),
  deleteUpstream: (id: string) => api.delete(`/admin/upstreams/${id}`),
  toggleUpstream: (id: string) => api.post<{ active: boolean }>(`/admin/upstreams/${id}/toggle`),
  
  // Downstreams
  getDownstreams: (params?: { status?: string; lifecycle?: string; search?: string }) =>
    api.get<DownstreamConfig[]>('/admin/downstreams', { params }),
  createDownstream: (data: Partial<DownstreamConfig>) => api.post<DownstreamConfig>('/admin/downstreams', data),
  getDownstream: (id: string) => api.get<DownstreamConfig>(`/admin/downstreams/${id}`),
  updateDownstream: (id: string, data: Partial<DownstreamConfig>) => api.put<DownstreamConfig>(`/admin/downstreams/${id}`, data),
  deleteDownstream: (id: string) => api.delete(`/admin/downstreams/${id}`),
  toggleDownstream: (id: string) => api.post<{ active: boolean }>(`/admin/downstreams/${id}/toggle`),
  rotateDownstream: (id: string) => api.post<{ plaintext_key: string }>(`/admin/downstreams/${id}/rotate`),
  
  // Logs
  getLogs: (params?: {
    page?: number
    page_size?: number
    status_code?: number
    status_codes?: string
    model?: string
    time_range?: string
    start_time?: number
    end_time?: number
  }) => api.get<LogsResponse>('/admin/logs', { params }),

  // Models
  getModels: () => api.get<{ models: string[] }>('/admin/models')
}
