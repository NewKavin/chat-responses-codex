import axios from 'axios'
import type {
  Announcement,
  PortalOverview,
  PortalModelStat,
  ModelProbeResponse,
  PortalQuota,
  PortalUsageHistory,
  PortalUsageSummary,
  ChartTimeRange
} from '@/types'

// Multi-key management types
export interface PortalKey {
  downstream_id: string
  label: string
  model_group_id: string
  model_group_name?: string | null
  created_at: number
  usage_count: number
  is_default: boolean
}

export interface ModelGroup {
  id: string
  name: string
  description: string | null
  allowed_models: string[]
  created_at: number
  updated_at: number
}

export interface CreateKeyRequest {
  downstream_id: string
  label?: string
  model_group_id?: string
}

export interface RotateKeyRequest {
  new_downstream_id: string
}

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

  // Usage History (detail-only, one calendar day)
  getUsageHistory: (params?: { day?: string; page?: number; page_size?: number }) =>
    portalHttp.get<PortalUsageHistory>('/portal/usage-history', { params }),

  // Usage Summary (independent seven-day chart aggregation)
  getUsageSummary: (params: { time_range?: ChartTimeRange }) =>
    portalHttp.get<PortalUsageSummary>('/portal/usage-summary', { params }),

  // Key Management (legacy single key)
  getKey: () => portalHttp.get<{ plaintext_key: string | null }>('/portal/key'),
  getModels: () => portalHttp.get<PortalModelStat[]>('/portal/models'),
  rotateKey: () => portalHttp.post<{ plaintext_key: string }>('/portal/key/rotate'),

  // Multi-Key Management
  listKeys: () => portalHttp.get<PortalKey[]>('/portal/keys'),
  createKey: (data: CreateKeyRequest) => portalHttp.post<{ success: boolean }>('/portal/keys', data),
  getKeyDetails: (downstreamId: string) => portalHttp.get<PortalKey>(`/portal/keys/${downstreamId}`),
  rotateKeyById: (downstreamId: string, newDownstreamId: string) =>
    portalHttp.post<{ success: boolean }>(`/portal/keys/${downstreamId}/rotate`, {
      new_downstream_id: newDownstreamId
    }),
  setDefaultKey: (downstreamId: string) =>
    portalHttp.put<{ success: boolean }>(`/portal/keys/${downstreamId}/default`),
  deleteKey: (downstreamId: string) =>
    portalHttp.delete<{ success: boolean }>(`/portal/keys/${downstreamId}`),

  // Model groups (portal users can read groups and set their keys' group)
  listModelGroups: () => portalHttp.get<{ groups: ModelGroup[] }>('/portal/model-groups'),
  updateKeyModelGroup: (downstreamId: string, modelGroupId: string) =>
    portalHttp.put<{ success: boolean }>(`/portal/keys/${downstreamId}/model-group`, {
      model_group_id: modelGroupId
    }),

  // Announcement
  getAnnouncement: () => portalHttp.get<AnnouncementResponse>('/portal/announcement')
}
