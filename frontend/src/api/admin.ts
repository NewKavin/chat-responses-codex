import axios from 'axios'
import type { LoginRequest, LoginResponse, DashboardData } from '@/types'

const api = axios.create({
  baseURL: '/api',
  timeout: 10000
})

// 请求拦截器：添加 JWT token
api.interceptors.request.use(config => {
  const token = localStorage.getItem('admin_token')
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
      localStorage.removeItem('admin_token')
      window.location.hash = '#/admin/login'
    }
    return Promise.reject(error)
  }
)

export const adminApi = {
  login: (data: LoginRequest) => api.post<LoginResponse>('/admin/login', data),
  getDashboard: () => api.get<DashboardData>('/admin/dashboard')
}
