import axios from 'axios'
import type {
  PortalOverview,
  PortalQuota,
  PortalUsageHistory
} from '@/types'

const api = axios.create({
  baseURL: '/api',
  timeout: 10000
})

// 请求拦截器：添加 Bearer token
api.interceptors.request.use(config => {
  const token = localStorage.getItem('portal_token')
  if (token) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

// 响应拦截器：处理 401 错误
api.interceptors.response.use(
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
    api.post<{ token: string }>('/portal/login', data),

  // Overview
  getOverview: () => api.get<PortalOverview>('/portal/overview'),

  // Quota
  getQuota: () => api.get<PortalQuota>('/portal/quota'),

  // Usage History
  getUsageHistory: (params?: { time_range?: string }) =>
    api.get<PortalUsageHistory>('/portal/usage-history', { params }),

  // Key Management
  getKey: () => api.get<{ plaintext_key: string | null }>('/portal/key'),
  rotateKey: () => api.post<{ plaintext_key: string }>('/portal/key/rotate')
}
