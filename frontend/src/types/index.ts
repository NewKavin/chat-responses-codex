export interface LoginRequest {
  username: string
  password: string
}

export interface LoginResponse {
  token: string
}

export interface DashboardData {
  upstreams_count: number
  downstreams_count: number
  logs_count: number
}
