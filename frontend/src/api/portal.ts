import axios from 'axios'
import type {
  Announcement,
  PortalOverview,
  PortalModelStat,
  ModelProbeResponse,
  PortalQuota,
  PortalUsageHistory,
  ActiveGatewayRequestsResponse,
  TroubleshootingRunRequest,
  TroubleshootingRunResponse
} from '@/types'

export interface AnnouncementResponse {
  announcement: Announcement | null
}

export const portalHttp = axios.create({
  baseURL: '/api',
  timeout: 10000
})

// 请求拦截器：添加 Bearer token
portalHttp.interceptors.request.use(config => {
  const token = localStorage.getItem('portal_token')
  if (token) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

// 响应拦截器：处理 401 错误
portalHttp.interceptors.response.use(
  response => response,
  error => {
    if (error.response?.status === 401) {
      localStorage.removeItem('portal_token')
      localStorage.removeItem('portal_employee_id')
      window.location.hash = '#/portal/login'
    }
    return Promise.reject(error)
  }
)

export const portalApi = {
  // Authentication
  login: (data: { employee_id: string; key: string }) =>
    portalHttp.post<{ token: string }>('/portal/login', data),

  // Overview
  getOverview: () => portalHttp.get<PortalOverview>('/portal/overview'),

  // Model Probe
  getModelProbe: () => portalHttp.get<ModelProbeResponse>('/portal/model-probe'),

  // Quota
  getQuota: () => portalHttp.get<PortalQuota>('/portal/quota'),

  // Usage History
  getUsageHistory: (params?: { time_range?: string; page?: number; page_size?: number }) =>
    portalHttp.get<PortalUsageHistory>('/portal/usage-history', { params }),

  // Key Management
  getKey: () => portalHttp.get<{ plaintext_key: string | null }>('/portal/key'),
  getModels: () => portalHttp.get<PortalModelStat[]>('/portal/models'),
  rotateKey: () => portalHttp.post<{ plaintext_key: string }>('/portal/key/rotate'),

  // Troubleshooting
  runTroubleshooting: (data: TroubleshootingRunRequest) =>
    portalHttp.post<TroubleshootingRunResponse>('/portal/troubleshooting/run', data),
  getActiveTroubleshootingRequests: () =>
    portalHttp.get<ActiveGatewayRequestsResponse>('/portal/troubleshooting/active-requests'),

  // Announcement
  getAnnouncement: () => portalHttp.get<AnnouncementResponse>('/portal/announcement')
}
