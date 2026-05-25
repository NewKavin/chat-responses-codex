import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useAuthStore = defineStore('auth', () => {
  const token = ref<string | null>(localStorage.getItem('admin_token'))

  const setToken = (newToken: string) => {
    token.value = newToken
    localStorage.setItem('admin_token', newToken)
  }

  const clearToken = () => {
    token.value = null
    localStorage.removeItem('admin_token')
  }

  const isAuthenticated = () => !!token.value

  return {
    token,
    setToken,
    clearToken,
    isAuthenticated
  }
})
