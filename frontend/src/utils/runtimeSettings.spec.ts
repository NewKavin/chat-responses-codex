import { describe, expect, it } from 'vitest'
import type { RuntimeSettings } from '@/types'
import {
  changedRestartFields,
  cloneRuntimeSettings,
  formatProbeDelays,
  isRuntimeSettingsDirty,
  parseProbeDelays,
  runtimeSettingFields,
  runtimeSettingGroups,
  validateRuntimeSettings
} from './runtimeSettings'

const validSettings = (): RuntimeSettings => ({
  app_name: 'Internal Gateway',
  usage_log_archive_max_files: 10,
  usage_log_retention_days: 30,
  admin_logs_page_size_max: 500,
  admin_upstream_timeout_seconds: 30,
  troubleshooting_check_timeout_seconds: 60,
  model_probe_refresh_interval_seconds: 300,
  upstream_model_auto_discovery_enabled: true,
  upstream_model_key_sync_interval_seconds: 0,
  capability_probe_queue_capacity: 64,
  capability_probe_request_timeout_seconds: 30,
  capability_probe_reasoning_timeout_seconds: 90,
  automatic_capability_probes_enabled: false,
  upstream_rate_limit_default_retry_seconds: 1,
  routing_affinity_enabled: true,
  routing_affinity_ttl_seconds: 300,
  routing_affinity_escape_pressure_ratio: 1.5,
  upstream_hedge_enabled: true,
  upstream_hedge_delay_ms: 500,
  upstream_hedge_interval_ms: 750,
  upstream_hedge_max_extra_attempts: 2,
  upstream_same_route_retry_enabled: true,
  upstream_transient_same_route_retry_enabled: true,
  upstream_transient_route_cooldown_base_seconds: 5,
  upstream_transient_route_cooldown_max_seconds: 60,
  upstream_route_health_half_open_ttl_seconds: 30,
  upstream_route_exhaustion_retry_enabled: true,
  upstream_route_exhaustion_retry_max_wait_ms: 15_000,
  upstream_route_exhaustion_retry_max_rounds: 3,
  upstream_route_exhaustion_budget_alignment_enabled: true,
  upstream_common_mode_breaker_threshold: 2,
  upstream_common_mode_transient_threshold: 4,
  default_upstream_max_concurrency: 4,
  downstream_lease_ttl_seconds: 120,
  upstream_concurrency_recovery_max_wait_ms: 30_000,
  upstream_concurrency_recovery_max_rounds: 5,
  upstream_concurrency_probe_delays_ms: [1_000, 10_000, 20_000],
  upstream_http_pool_max_idle_per_host: 32,
  upstream_user_agent: 'chat2responses/1.0',
  upstream_connect_timeout_seconds: 10,
  upstream_response_header_timeout_seconds: 30,
  upstream_stream_keepalive_interval_seconds: 15,
  upstream_stream_idle_timeout_seconds: 60,
  upstream_stream_max_duration_seconds: 3_600,
  upstream_first_semantic_output_timeout_seconds: 180
})

const expectedKeys: Array<keyof RuntimeSettings> = [
  'app_name',
  'usage_log_archive_max_files',
  'usage_log_retention_days',
  'admin_logs_page_size_max',
  'admin_upstream_timeout_seconds',
  'troubleshooting_check_timeout_seconds',
  'model_probe_refresh_interval_seconds',
  'upstream_model_auto_discovery_enabled',
  'upstream_model_key_sync_interval_seconds',
  'capability_probe_queue_capacity',
  'capability_probe_request_timeout_seconds',
  'capability_probe_reasoning_timeout_seconds',
  'automatic_capability_probes_enabled',
  'upstream_rate_limit_default_retry_seconds',
  'routing_affinity_enabled',
  'routing_affinity_ttl_seconds',
  'routing_affinity_escape_pressure_ratio',
  'upstream_hedge_enabled',
  'upstream_hedge_delay_ms',
  'upstream_hedge_interval_ms',
  'upstream_hedge_max_extra_attempts',
  'upstream_same_route_retry_enabled',
  'upstream_transient_same_route_retry_enabled',
  'upstream_transient_route_cooldown_base_seconds',
  'upstream_transient_route_cooldown_max_seconds',
  'upstream_route_health_half_open_ttl_seconds',
  'upstream_route_exhaustion_retry_enabled',
  'upstream_route_exhaustion_retry_max_wait_ms',
  'upstream_route_exhaustion_retry_max_rounds',
  'upstream_route_exhaustion_budget_alignment_enabled',
  'upstream_common_mode_breaker_threshold',
  'upstream_common_mode_transient_threshold',
  'default_upstream_max_concurrency',
  'downstream_lease_ttl_seconds',
  'upstream_concurrency_recovery_max_wait_ms',
  'upstream_concurrency_recovery_max_rounds',
  'upstream_concurrency_probe_delays_ms',
  'upstream_http_pool_max_idle_per_host',
  'upstream_user_agent',
  'upstream_connect_timeout_seconds',
  'upstream_response_header_timeout_seconds',
  'upstream_stream_keepalive_interval_seconds',
  'upstream_stream_idle_timeout_seconds',
  'upstream_stream_max_duration_seconds',
  'upstream_first_semantic_output_timeout_seconds'
]

describe('runtime settings catalog', () => {
  it('catalogs every managed field exactly once in six groups', () => {
    expect(runtimeSettingGroups.map(group => group.id)).toEqual([
      'general',
      'discovery',
      'routing',
      'concurrency',
      'http',
      'logs'
    ])
    expect(runtimeSettingFields).toHaveLength(45)
    expect(new Set(runtimeSettingFields.map(field => field.key)).size).toBe(45)
    expect(runtimeSettingFields.map(field => field.key).sort()).toEqual(expectedKeys.sort())
    expect(runtimeSettingFields.filter(field => field.apply === 'immediate')).toHaveLength(33)
    expect(runtimeSettingFields.filter(field => field.apply === 'restart')).toHaveLength(12)
  })
})

describe('runtime settings helpers', () => {
  it('parses, sorts, deduplicates, and formats probe delays', () => {
    expect(parseProbeDelays('1000, 100, 100, 400')).toEqual([100, 400, 1_000])
    expect(formatProbeDelays([100, 400, 1_000])).toBe('100, 400, 1000')
    expect(() => parseProbeDelays('')).toThrowError()
    expect(() => parseProbeDelays('0, 100')).toThrowError()
    expect(() => parseProbeDelays('100, 60001')).toThrowError()
    expect(() => parseProbeDelays('100, nope')).toThrowError()
  })

  it('mirrors backend scalar and relationship validation', () => {
    expect(validateRuntimeSettings(validSettings())).toEqual({})

    const invalid = validSettings()
    invalid.app_name = '  '
    invalid.default_upstream_max_concurrency = 0
    invalid.capability_probe_reasoning_timeout_seconds = 0
    invalid.upstream_transient_route_cooldown_base_seconds = 61
    invalid.upstream_stream_keepalive_interval_seconds = 120
    invalid.upstream_stream_idle_timeout_seconds = 120
    invalid.upstream_stream_max_duration_seconds = 90
    invalid.upstream_first_semantic_output_timeout_seconds = 100
    invalid.upstream_concurrency_recovery_max_wait_ms = 60_000
    invalid.upstream_concurrency_recovery_max_rounds = 2
    invalid.upstream_concurrency_probe_delays_ms = [100]

    const errors = validateRuntimeSettings(invalid)
    expect(errors.app_name).toBeTruthy()
    expect(errors.default_upstream_max_concurrency).toBeTruthy()
    expect(errors.capability_probe_reasoning_timeout_seconds).toBeTruthy()
    expect(errors.upstream_transient_route_cooldown_base_seconds).toBeTruthy()
    expect(errors.upstream_stream_keepalive_interval_seconds).toBeTruthy()
    expect(errors.upstream_stream_idle_timeout_seconds).toBeTruthy()
    expect(errors.upstream_first_semantic_output_timeout_seconds).toBeTruthy()
    expect(errors.upstream_concurrency_recovery_max_rounds).toBeTruthy()
  })

  it('tracks dirty state and restart-only differences without sharing arrays', () => {
    const loaded = validSettings()
    const edited = cloneRuntimeSettings(loaded)

    expect(edited).not.toBe(loaded)
    expect(edited.upstream_concurrency_probe_delays_ms).not.toBe(
      loaded.upstream_concurrency_probe_delays_ms
    )
    expect(isRuntimeSettingsDirty(loaded, edited)).toBe(false)
    expect(changedRestartFields(loaded, edited)).toEqual([])

    edited.routing_affinity_ttl_seconds += 1
    expect(isRuntimeSettingsDirty(loaded, edited)).toBe(true)
    expect(changedRestartFields(loaded, edited)).toEqual([])

    edited.upstream_connect_timeout_seconds += 1
    expect(changedRestartFields(loaded, edited)).toEqual([
      'upstream_connect_timeout_seconds'
    ])
  })
})
