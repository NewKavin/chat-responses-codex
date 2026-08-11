import type {
  CapabilityDiscoveryResponse,
  CapabilityModelDiscoverySummary,
  CapabilityRouteDiscoverySummary,
  ProbeAllCapabilitiesResponse
} from '@/types'

export interface IndexedCapabilityModel {
  levels: string[]
  summary: CapabilityModelDiscoverySummary
}

export interface IndexedCapabilityRoute extends CapabilityRouteDiscoverySummary {
  exposed_model_slug: string
}

export interface CapabilityDiscoveryIndex {
  models: Map<string, IndexedCapabilityModel>
  routes: Map<string, IndexedCapabilityRoute>
}

export interface DiscoveryBatchProgress {
  completed: number
  total: number
  settled: boolean
}

export interface PollCapabilityDiscoveryOptions {
  receipt: ProbeAllCapabilitiesResponse
  initial: CapabilityDiscoveryResponse
  fetchDiscovery: (requestTimeoutMs: number) => Promise<CapabilityDiscoveryResponse>
  now?: () => number
  sleep?: (delayMs: number) => Promise<void>
  cancelled?: () => boolean
  onProgress?: (progress: DiscoveryBatchProgress) => void
  intervalMs?: number
  timeoutMs?: number
}

export interface PollCapabilityDiscoveryResult {
  discovery: CapabilityDiscoveryResponse
  progress: DiscoveryBatchProgress
  timedOut: boolean
  cancelled: boolean
}

// One full probe round can take 30-60+ minutes with 130 routes at
// concurrency 2 and up to request-timeout seconds per route. The poll must
// not give up mid-round: keep waiting until the batch settles, with a 2h
// hard cap as a safety net (the backend worker keeps running regardless).
export const CAPABILITY_PROBE_WAIT_TIMEOUT_MS = 2 * 60 * 60 * 1000

export const indexDiscovery = (
  response: CapabilityDiscoveryResponse
): CapabilityDiscoveryIndex => {
  const models = new Map<string, IndexedCapabilityModel>()
  const routes = new Map<string, IndexedCapabilityRoute>()

  for (const summary of response.models) {
    models.set(summary.exposed_model_slug, {
      levels: [...summary.verified_reasoning_levels],
      summary
    })
    for (const route of summary.routes) {
      routes.set(route.route_id, {
        ...route,
        exposed_model_slug: summary.exposed_model_slug
      })
    }
  }

  return { models, routes }
}

export const routeStatusLabel = (
  route: CapabilityRouteDiscoverySummary
): string => {
  switch (route.outcome) {
    case 'accepted':
      return '已验证'
    case 'rejected':
      return '不支持'
    case 'operational_failure':
      return '探测暂不可用'
    case 'deferred':
      return '等待重试'
    case 'pending':
      return '等待探测'
  }
}

export const routeStatusTagType = (
  route: CapabilityRouteDiscoverySummary
): 'success' | 'warning' | 'danger' | 'info' => {
  switch (route.outcome) {
    case 'accepted':
      return 'success'
    case 'rejected':
      return 'danger'
    case 'operational_failure':
    case 'deferred':
      return 'warning'
    case 'pending':
      return 'info'
  }
}

export const discoveryBatchProgress = (
  receipt: ProbeAllCapabilitiesResponse,
  response: CapabilityDiscoveryResponse
): DiscoveryBatchProgress => {
  const models = new Map(
    response.models.map(model => [model.exposed_model_slug, model])
  )
  let completed = 0

  for (const candidate of receipt.candidates) {
    const route = models
      .get(candidate.exposed_model_slug)
      ?.routes.find(route =>
        route.route_id === candidate.route_id
        && route.upstream_id === candidate.upstream_id
        && route.runtime_model_slug === candidate.runtime_model_slug
        && route.protocol === candidate.protocol
      )
    if ((route?.last_attempt_at ?? 0) >= receipt.started_at) {
      completed += 1
    }
  }

  const total = receipt.candidates.length
  return {
    completed,
    total,
    settled: completed === total
  }
}

const sleepFor = (delayMs: number) =>
  new Promise<void>(resolve => setTimeout(resolve, delayMs))

export const pollCapabilityDiscovery = async ({
  receipt,
  initial,
  fetchDiscovery,
  now = Date.now,
  sleep = sleepFor,
  cancelled = () => false,
  onProgress,
  intervalMs = 2_500,
  timeoutMs = CAPABILITY_PROBE_WAIT_TIMEOUT_MS
}: PollCapabilityDiscoveryOptions): Promise<PollCapabilityDiscoveryResult> => {
  const deadline = now() + timeoutMs
  let discovery = initial
  let progress = discoveryBatchProgress(receipt, discovery)

  const result = (
    timedOut: boolean,
    wasCancelled: boolean
  ): PollCapabilityDiscoveryResult => ({
    discovery,
    progress,
    timedOut,
    cancelled: wasCancelled
  })

  while (!cancelled()) {
    const remainingBeforeRequest = deadline - now()
    if (remainingBeforeRequest <= 0) return result(true, false)

    try {
      discovery = await fetchDiscovery(Math.min(10_000, remainingBeforeRequest))
      if (cancelled()) return result(false, true)
      progress = discoveryBatchProgress(receipt, discovery)
      onProgress?.(progress)
      if (progress.settled) return result(false, false)
    } catch {
      if (cancelled()) return result(false, true)
    }

    const remainingBeforeSleep = deadline - now()
    if (remainingBeforeSleep <= 0) return result(true, false)
    await sleep(Math.min(intervalMs, remainingBeforeSleep))
    if (cancelled()) return result(false, true)
  }

  return result(false, true)
}
