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
export type ModelAccessMode = 'inherit' | 'group' | 'deny'

export interface ModelAccessSelection {
  mode: ModelAccessMode
  group_id?: string | null
}

export interface PortalKey {
  downstream_id: string
  /** 密钥明文：创建/轮换时返回，之后列表接口可随时回看（仅绑定 owner 可见） */
  plaintext_key?: string
  label: string
  model_group_id: string
  model_group_name?: string | null
  /** 新策略契约：密钥模型访问模式与限定组 */
  model_access?: ModelAccessSelection
  access_revision?: number
  expires_at?: number | null
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
  label?: string
  model_group_id?: string
  model_access?: ModelAccessSelection
}

export interface AnnouncementResponse {
  announcement: Announcement | null
}

export interface PortalSessionResponse {
  auth_method: 'cookie' | 'legacy'
  user: {
    id: string
    email: string
    display_name: string | null
    username: string | null
    provider: string | null
    subject: string | null
  }
  login_downstream_id: string | null
  default_downstream_id: string | null
  has_keys: boolean
}

export interface PortalModelAccessResponse {
  user_id: string
  scope: 'user' | 'key'
  downstream_id?: string | null
  available_models: string[]
  status: 'denied' | 'no_routes' | 'ready'
  reason?: string | null
  source: {
    user_group_ids: string[]
    mode?: string | null
    key_group_id?: string | null
  }
  model_access: ModelAccessSelection | null
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

  // Overview（可选 downstream_id：显式选定密钥，越权由服务端拒绝）
  getOverview: (params?: { downstream_id?: string }) =>
    portalHttp.get<PortalOverview>('/portal/overview', { params }),

  // Model Probe
  getModelProbe: (params?: { downstream_id?: string }) =>
    portalHttp.get<ModelProbeResponse>('/portal/model-probe', { params }),

  // Quota
  getQuota: (params?: { downstream_id?: string }) =>
    portalHttp.get<PortalQuota>('/portal/quota', { params }),

  // Usage History (detail-only, one calendar day)
  getUsageHistory: (params?: { day?: string; page?: number; page_size?: number }) =>
    portalHttp.get<PortalUsageHistory>('/portal/usage-history', { params }),

  // Usage Summary (independent seven-day chart aggregation)
  getUsageSummary: (params: { time_range?: ChartTimeRange }) =>
    portalHttp.get<PortalUsageSummary>('/portal/usage-summary', { params }),

  // Key Management (legacy single key)
  getKey: () => portalHttp.get<{ plaintext_key: string | null }>('/portal/key'),
  getModels: (params?: { downstream_id?: string }) =>
    portalHttp.get<PortalModelStat[]>('/portal/models', { params }),

  // 模型访问契约（用户范围或指定密钥范围）
  getModelAccess: (params?: { downstream_id?: string }) =>
    portalHttp.get<PortalModelAccessResponse>('/portal/model-access', { params }),
  rotateKey: () => portalHttp.post<{ plaintext_key: string }>('/portal/key/rotate'),

  // Multi-Key Management
  listKeys: () => portalHttp.get<PortalKey[]>('/portal/keys'),
  createKey: (data: CreateKeyRequest) =>
    portalHttp.post<{ success: boolean; downstream_id: string; plaintext_key: string }>(
      '/portal/keys',
      data
    ),
  getKeyDetails: (downstreamId: string) => portalHttp.get<PortalKey>(`/portal/keys/${downstreamId}`),
  rotateKeyById: (downstreamId: string) =>
    portalHttp.post<{ downstream_id: string; plaintext_key: string }>(
      `/portal/keys/${downstreamId}/rotate`,
      {}
    ),
  setDefaultKey: (downstreamId: string) =>
    portalHttp.put<{ success: boolean }>(`/portal/keys/${downstreamId}/default`),
  deleteKey: (downstreamId: string) =>
    portalHttp.delete<{ success: boolean }>(`/portal/keys/${downstreamId}`),

  // Model groups (portal users can read groups and set their keys' group)
  listModelGroups: () => portalHttp.get<{ groups: ModelGroup[] }>('/portal/model-groups'),
  updateKeyModelAccess: (
    downstreamId: string,
    modelAccess: ModelAccessSelection
  ) =>
    portalHttp.put<{ success: boolean }>(`/portal/keys/${downstreamId}/model-group`, {
      model_access: modelAccess
    }),
  updateKeyModelGroup: (downstreamId: string, modelGroupId: string) =>
    portalHttp.put<{ success: boolean }>(`/portal/keys/${downstreamId}/model-group`, {
      model_group_id: modelGroupId
    }),
  updateKeyLabel: (downstreamId: string, label: string) =>
    portalHttp.put<{ downstream_id: string; label: string | null; model_group_id: string }>(
      `/portal/keys/${downstreamId}/label`,
      { label }
    ),

  // Announcement
  getAnnouncement: () => portalHttp.get<AnnouncementResponse>('/portal/announcement'),
  getSession: () => portalHttp.get<PortalSessionResponse>('/portal/session'),
  logout: () => portalHttp.post<{ ok: boolean }>('/portal/logout')
}
