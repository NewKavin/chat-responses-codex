import { describe, expect, it } from 'vitest'
import type {
  CapabilityDiscoveryResponse,
  ProbeAllCapabilitiesResponse
} from '@/types'
import {
  discoveryBatchProgress,
  indexDiscovery,
  pollCapabilityDiscovery,
  routeStatusLabel,
  routeStatusTagType
} from './capabilityDiscovery'

const discovery = (): CapabilityDiscoveryResponse => ({
  models: [
    {
      exposed_model_slug: 'deepseek-v4-flash',
      verified_reasoning_levels: ['low', 'medium', 'high'],
      routes: [
        {
          upstream_id: 'deepseek',
          route_id: 'route_chat',
          runtime_model_slug: 'deepseek-v4-flash',
          protocol: 'chat_completions',
          outcome: 'accepted',
          accepted_reasoning_levels: ['low', 'medium'],
          http_status: 200,
          operational_code: null,
          last_attempt_at: 1_001,
          next_probe_at: null
        },
        {
          upstream_id: 'deepseek',
          route_id: 'route_responses',
          runtime_model_slug: 'deepseek-v4-flash',
          protocol: 'responses',
          outcome: 'operational_failure',
          accepted_reasoning_levels: [],
          http_status: 503,
          operational_code: 'minimal_text_failed',
          last_attempt_at: 999,
          next_probe_at: 1_004
        }
      ]
    }
  ]
})

const receipt: ProbeAllCapabilitiesResponse = {
  configuration_revision: 7,
  started_at: 1_000,
  queued_routes: 2,
  candidates: [
    {
      upstream_id: 'deepseek',
      route_id: 'route_chat',
      exposed_model_slug: 'deepseek-v4-flash',
      runtime_model_slug: 'deepseek-v4-flash',
      protocol: 'chat_completions'
    },
    {
      upstream_id: 'deepseek',
      route_id: 'route_responses',
      exposed_model_slug: 'deepseek-v4-flash',
      runtime_model_slug: 'deepseek-v4-flash',
      protocol: 'responses'
    }
  ]
}

describe('capability discovery', () => {
  it('indexes server-owned model levels and exact route identities', () => {
    const indexed = indexDiscovery(discovery())

    expect(indexed.models.get('deepseek-v4-flash')?.levels)
      .toEqual(['low', 'medium', 'high'])
    expect(indexed.routes.get('route_responses')?.protocol).toBe('responses')
    expect(indexed.routes.size).toBe(2)
  })

  it('keeps operational and deferred routes distinct from unsupported routes', () => {
    const operationalRoute = discovery().models[0].routes[1]
    const deferredRoute = { ...operationalRoute, outcome: 'deferred' as const }
    const rejectedRoute = { ...operationalRoute, outcome: 'rejected' as const }

    expect(routeStatusLabel(operationalRoute)).toBe('探测暂不可用')
    expect(routeStatusLabel(deferredRoute)).toBe('等待重试')
    expect(routeStatusLabel(rejectedRoute)).toBe('不支持')
    expect(routeStatusTagType(operationalRoute)).toBe('warning')
    expect(routeStatusTagType(deferredRoute)).toBe('warning')
    expect(routeStatusTagType(rejectedRoute)).toBe('danger')
  })

  it('counts only routes attempted in the current batch as settled', () => {
    const partial = discoveryBatchProgress(receipt, discovery())
    expect(partial).toEqual({ completed: 1, total: 2, settled: false })

    const current = discovery()
    current.models[0].routes[1].last_attempt_at = receipt.started_at
    expect(discoveryBatchProgress(receipt, current))
      .toEqual({ completed: 2, total: 2, settled: true })
  })

  it('retries transient discovery failures until the batch settles', async () => {
    const oneRouteReceipt = {
      ...receipt,
      queued_routes: 1,
      candidates: [receipt.candidates[0]]
    }
    let now = 0
    let attempts = 0

    const result = await pollCapabilityDiscovery({
      receipt: oneRouteReceipt,
      initial: { models: [] },
      fetchDiscovery: async () => {
        attempts += 1
        if (attempts === 1) throw new Error('temporary network failure')
        return discovery()
      },
      now: () => now,
      sleep: async delay => { now += delay },
      intervalMs: 25,
      timeoutMs: 100
    })

    expect(attempts).toBe(2)
    expect(result.progress.settled).toBe(true)
    expect(result.timedOut).toBe(false)
  })

  it('caps request and sleep timing at the hard deadline', async () => {
    const stale = discovery()
    stale.models[0].routes.forEach(route => { route.last_attempt_at = 999 })
    let now = 0
    const requestTimeouts: number[] = []
    const sleeps: number[] = []

    const result = await pollCapabilityDiscovery({
      receipt,
      initial: stale,
      fetchDiscovery: async requestTimeoutMs => {
        requestTimeouts.push(requestTimeoutMs)
        throw new Error('still unavailable')
      },
      now: () => now,
      sleep: async delay => {
        sleeps.push(delay)
        now += delay
      },
      intervalMs: 75,
      timeoutMs: 100
    })

    expect(requestTimeouts).toEqual([100, 25])
    expect(sleeps).toEqual([75, 25])
    expect(result.timedOut).toBe(true)
    expect(now).toBe(100)
  })

  it('stops without publishing progress after cancellation', async () => {
    let cancelled = false
    let progressUpdates = 0

    const result = await pollCapabilityDiscovery({
      receipt,
      initial: { models: [] },
      fetchDiscovery: async () => {
        cancelled = true
        return discovery()
      },
      cancelled: () => cancelled,
      onProgress: () => { progressUpdates += 1 }
    })

    expect(result.cancelled).toBe(true)
    expect(progressUpdates).toBe(0)
  })
})
