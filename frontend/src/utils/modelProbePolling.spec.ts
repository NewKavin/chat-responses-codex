import { describe, expect, it } from 'vitest'
import {
  DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS,
  getModelProbeRefreshDelayMs,
  normalizeModelProbeRefreshIntervalSeconds
} from './modelProbePolling'

describe('modelProbePolling', () => {
  it('uses the backend-provided refresh interval when it is valid', () => {
    expect(normalizeModelProbeRefreshIntervalSeconds(9)).toBe(9)
    expect(getModelProbeRefreshDelayMs({ refresh_interval_seconds: 9 })).toBe(9000)
  })

  it('falls back to the default refresh interval when the value is missing or invalid', () => {
    expect(normalizeModelProbeRefreshIntervalSeconds(undefined)).toBe(
      DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS
    )
    expect(normalizeModelProbeRefreshIntervalSeconds(0)).toBe(1)
    expect(normalizeModelProbeRefreshIntervalSeconds(Number.NaN)).toBe(
      DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS
    )
    expect(getModelProbeRefreshDelayMs({})).toBe(DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS * 1000)
    expect(getModelProbeRefreshDelayMs({ refresh_interval_seconds: null })).toBe(
      DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS * 1000
    )
  })
})
