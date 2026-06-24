export const DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS = 15

export const normalizeModelProbeRefreshIntervalSeconds = (
  intervalSeconds?: number | null
): number => {
  const normalized = intervalSeconds ?? DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS
  if (!Number.isFinite(normalized)) {
    return DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS
  }
  return Math.max(1, Math.floor(normalized))
}

export const getModelProbeRefreshDelayMs = (
  response: { refresh_interval_seconds?: number | null }
): number => normalizeModelProbeRefreshIntervalSeconds(response.refresh_interval_seconds) * 1000
