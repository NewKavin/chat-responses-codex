import type {
  CapabilityDiscoveryResponse,
  CapabilityModelDiscoverySummary,
  CapabilityProbeBatchStatus,
  CapabilityProbeCandidateState,
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


const TERMINAL_PROBE_BATCH_STATES = new Set<CapabilityProbeCandidateState>([
  'completed',
  'failed',
  'cooldown_skipped',
  'superseded'
])

export const capabilityProbeBatchProgress = (
  status: CapabilityProbeBatchStatus
): DiscoveryBatchProgress => {
  const completed = status.candidates.filter(
    candidate => TERMINAL_PROBE_BATCH_STATES.has(candidate.state)
  ).length
  const total = status.candidates.length
  return {
    completed,
    total,
    settled: status.terminal_at !== null || completed === total
  }
}

export const probeBatchStateLabel = (
  state: CapabilityProbeCandidateState
): string => {
  switch (state) {
    case 'queued':
      return '排队中'
    case 'reused':
      return '复用探测中'
    case 'running':
      return '探测中'
    case 'completed':
      return '已完成'
    case 'failed':
      return '本轮失败'
    case 'cooldown_skipped':
      return '冷却跳过'
    case 'superseded':
      return '已被替代'
  }
}

export const probeBatchStateTagType = (
  state: CapabilityProbeCandidateState
): 'success' | 'warning' | 'danger' | 'info' => {
  switch (state) {
    case 'completed':
      return 'success'
    case 'failed':
      return 'danger'
    case 'running':
    case 'cooldown_skipped':
      return 'warning'
    case 'queued':
    case 'reused':
    case 'superseded':
      return 'info'
  }
}

export interface PollCapabilityProbeBatchOptions {
  initial: CapabilityProbeBatchStatus
  fetchBatch: (requestTimeoutMs: number) => Promise<CapabilityProbeBatchStatus>
  now?: () => number
  sleep?: (delayMs: number) => Promise<void>
  cancelled?: () => boolean
  onProgress?: (progress: DiscoveryBatchProgress) => void
  intervalMs?: number
  timeoutMs?: number
}

export interface PollCapabilityProbeBatchResult {
  status: CapabilityProbeBatchStatus
  progress: DiscoveryBatchProgress
  timedOut: boolean
  cancelled: boolean
}

// The batch endpoint reflects the current in-memory round state (queued,
// reused, running, completed, failed) independently from the durable profile
// results. Poll until the backend marks the round terminal, with the same 2h
// safety cap as the durable discovery poll.
export const pollCapabilityProbeBatch = async ({
  initial,
  fetchBatch,
  now = Date.now,
  sleep = sleepFor,
  cancelled = () => false,
  onProgress,
  intervalMs = 2_500,
  timeoutMs = CAPABILITY_PROBE_WAIT_TIMEOUT_MS
}: PollCapabilityProbeBatchOptions): Promise<PollCapabilityProbeBatchResult> => {
  const deadline = now() + timeoutMs
  let status = initial
  let progress = capabilityProbeBatchProgress(status)

  const result = (
    timedOut: boolean,
    wasCancelled: boolean
  ): PollCapabilityProbeBatchResult => ({
    status,
    progress,
    timedOut,
    cancelled: wasCancelled
  })

  while (!cancelled()) {
    const remainingBeforeRequest = deadline - now()
    if (remainingBeforeRequest <= 0) return result(true, false)

    try {
      status = await fetchBatch(Math.min(10_000, remainingBeforeRequest))
      if (cancelled()) return result(false, true)
      progress = capabilityProbeBatchProgress(status)
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

export const formatProbeEta = (seconds: number | null): string | null => {
  if (seconds === null) return null
  if (seconds <= 0) return '即将完成'
  const minutes = Math.max(1, Math.ceil(seconds / 60))
  if (minutes < 60) return `约 ${minutes} 分钟`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  return rest === 0 ? `约 ${hours} 小时` : `约 ${hours} 小时 ${rest} 分钟`
}
