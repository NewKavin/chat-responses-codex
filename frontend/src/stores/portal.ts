import { defineStore } from 'pinia'
import { ref } from 'vue'
import { portalApi, type PortalKey, type PortalSessionResponse } from '@/api/portal'

/**
 * 门户共享状态（设计 5.4）：
 * - principal：统一 cookie/legacy 身份，跨页面一致；
 * - selectedDownstreamId：当前密钥选择，自动初选优先 inherit 密钥、
 *   其次服务端默认 key；用户显式选择后不被默认设置更新覆盖；
 * - 不持久化任何 secret。
 */
export const usePortalStore = defineStore('portal', () => {
  const session = ref<PortalSessionResponse | null>(null)
  const selectedDownstreamId = ref<string | null>(null)
  const explicitSelection = ref(false)
  const sessionLoading = ref(false)
  const sessionError = ref<string | null>(null)

  const fetchSession = async () => {
    if (sessionLoading.value) return
    sessionLoading.value = true
    sessionError.value = null
    try {
      const { data } = await portalApi.getSession()
      session.value = data
    } catch (err: any) {
      sessionError.value = err?.message || 'session 加载失败'
    } finally {
      sessionLoading.value = false
    }
  }

  const clearSession = () => {
    session.value = null
    selectedDownstreamId.value = null
    explicitSelection.value = false
  }

  /**
   * 自动初选：用户显式选择后不覆盖；否则按 inherit 密钥优先、
   * 服务端默认 key 其次的顺序选择。
   */
  const primeSelection = (keys: PortalKey[]) => {
    if (explicitSelection.value) {
      // 显式选择失效（密钥被删/轮换）时清理并要求重新选择。
      if (
        selectedDownstreamId.value &&
        !keys.some(key => key.downstream_id === selectedDownstreamId.value)
      ) {
        selectedDownstreamId.value = null
        explicitSelection.value = false
      }
      return
    }
    const inheritKey = keys.find(key => key.model_access?.mode === 'inherit')
    const defaultKey = keys.find(key => key.is_default)
    const firstKey = keys[0]
    selectedDownstreamId.value =
      inheritKey?.downstream_id ?? defaultKey?.downstream_id ?? firstKey?.downstream_id ?? null
    explicitSelection.value = false
  }

  const selectKey = (downstreamId: string) => {
    selectedDownstreamId.value = downstreamId
    explicitSelection.value = true
  }

  const clearSelection = () => {
    selectedDownstreamId.value = null
    explicitSelection.value = false
  }

  /**
   * 请求作用域：仅当用户显式选择了密钥时带上 downstream_id；
   * 否则交由服务端默认选择（兼容旧客户端）。
   */
  const scopeParams = () =>
    explicitSelection.value && selectedDownstreamId.value
      ? { downstream_id: selectedDownstreamId.value }
      : undefined

  return {
    session,
    sessionLoading,
    sessionError,
    selectedDownstreamId,
    explicitSelection,
    fetchSession,
    clearSession,
    primeSelection,
    selectKey,
    clearSelection,
    scopeParams
  }
})
