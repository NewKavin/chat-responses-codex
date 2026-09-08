import { onMounted, onUnmounted, ref } from 'vue'

export function useQuietRefresh<T>(
  fetchValue: (signal: AbortSignal) => Promise<T>,
  apply: (value: T) => void | Promise<void>,
  intervalMs: () => number
) {
  const ready = ref(false)
  const inFlight = ref(false)
  const manualLoading = ref(false)
  const error = ref('')
  const updatedAt = ref(0)
  let timer: ReturnType<typeof setTimeout> | undefined
  let controller: AbortController | undefined
  let pending: Promise<void> | undefined
  let generation = 0
  let disposed = false

  const cancel = () => {
    generation++
    clearTimeout(timer)
    controller?.abort()
    pending = undefined
    inFlight.value = false
    manualLoading.value = false
  }

  const refresh = (manual = false): Promise<void> => {
    if (disposed || document.hidden) return Promise.resolve()
    if (pending) return pending
    clearTimeout(timer)
    const sequence = ++generation
    controller = new AbortController()
    inFlight.value = true
    manualLoading.value = manual
    const signal = controller.signal
    pending = Promise.resolve().then(() => fetchValue(signal)).then(async value => {
      if (disposed || sequence !== generation) return
      await apply(value)
      if (disposed || sequence !== generation) return
      ready.value = true
      error.value = ''
      updatedAt.value = Date.now()
    }).catch(reason => {
      if (!disposed && sequence === generation && !signal.aborted) {
        error.value = reason instanceof Error ? reason.message : '刷新失败'
      }
    }).finally(() => {
      if (disposed || sequence !== generation) return
      pending = undefined
      inFlight.value = false
      manualLoading.value = false
      if (!document.hidden) timer = setTimeout(() => { void refresh() }, intervalMs())
    })
    return pending
  }

  const reload = () => { cancel(); return refresh(true) }
  const onVisibility = () => {
    if (document.hidden) cancel()
    else void refresh()
  }
  onMounted(() => {
    document.addEventListener('visibilitychange', onVisibility)
    void refresh()
  })
  onUnmounted(() => {
    disposed = true
    cancel()
    document.removeEventListener('visibilitychange', onVisibility)
  })
  return { ready, inFlight, manualLoading, error, updatedAt, refresh, reload }
}
