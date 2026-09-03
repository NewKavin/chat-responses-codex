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
  capability_probe_concurrency: 4,
  automatic_capability_probes_enabled: false,
  model_case_insensitive_matching: true,
  tool_call_merge_strict: true,
  tool_arguments_strict: false,
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
  upstream_shared_host_failure_domain_enabled: true,
  upstream_common_mode_same_host_transient_enabled: true,
  upstream_capacity_failure_cooldown_enabled: false,
  upstream_transient_route_cooldown_base_seconds: 5,
  upstream_transient_route_cooldown_max_seconds: 60,
  upstream_transient_route_cooldown_max_step: 2,
  upstream_route_health_half_open_ttl_seconds: 30,
  upstream_route_health_enforcement_enabled: true,
  upstream_route_half_open_exclusive_window_ms: 3_000,
  upstream_route_half_open_busy_max_rounds: 10,
  upstream_retry_after_cap_seconds: 30,
  upstream_retry_after_cooldown_cap_seconds: 5,
  upstream_credentials_first_strike_seconds: 60,
  upstream_error_body_excerpt_enabled: false,
  upstream_error_body_excerpt_max_chars: 200,
  upstream_route_exhaustion_retry_enabled: true,
  upstream_route_exhaustion_retry_max_wait_ms: 15_000,
  upstream_route_exhaustion_retry_max_rounds: 3,
  upstream_route_exhaustion_budget_alignment_enabled: true,
  upstream_route_exhaustion_alignment_truncated_enabled: true,
  upstream_transient_last_resort_probe_enabled: true,
  upstream_common_mode_breaker_threshold: 2,
  upstream_common_mode_transient_threshold: 4,
  upstream_continuation_pin_escape_enabled: true,
  upstream_local_lease_ttl_seconds: 300,
  upstream_lease_stale_after_ms: 200_000,
  default_upstream_max_concurrency: 32,
  upstream_account_queue_enabled: true,
  upstream_account_queue_max_depth: 16,
  upstream_account_queue_max_wait_ms: 10_000,
  upstream_account_queue_poll_interval_ms: 100,
  upstream_account_queue_adaptive_budget_enabled: true,
  upstream_account_queue_skip_when_doomed_enabled: true,
  upstream_account_queue_adaptive_budget_factor: 1.5,
  upstream_account_queue_adaptive_budget_ceiling_ms: 60_000,
  upstream_local_gate_max_wait_ms: 3_000,
  upstream_local_gate_fast_fail_enabled: true,
  upstream_local_gate_distinct_error_code_enabled: true,
  stream_decode_error_code_split_enabled: true,
  stream_max_skipped_bad_frames: 8,
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
  upstream_first_semantic_output_timeout_seconds: 180,
  upstream_first_output_warn_after_seconds: 120,
  gateway_request_body_limit_mb: 32,
  portal_oidc_enabled: true,
  portal_oidc_registration_enabled: true,
  portal_oidc_allowed_email_domains: 'example.com',
  portal_session_ttl_seconds: 86400,
  portal_oidc_pkce_enabled: true,
  portal_oidc_verify_id_token: false,
  portal_oidc_client_id: 'client-id',
  portal_oidc_client_secret: 'client-secret',
  portal_oidc_redirect_url: 'http://gateway/api/portal/oidc/callback',
  portal_oidc_issuer_url: 'http://idp.example.com',
  portal_oidc_authorization_endpoint: '',
  portal_oidc_token_endpoint: '',
  portal_oidc_userinfo_endpoint: '',
  portal_oidc_scopes: 'openid profile email',
  portal_oidc_auth_style: 'auto',
  portal_oidc_user_id_field: 'sub',
  portal_oidc_email_field: 'email',
  portal_oidc_username_field: 'preferred_username',
  portal_oidc_display_name_field: 'name'
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
  'capability_probe_concurrency',
  'capability_probe_request_timeout_seconds',
  'capability_probe_reasoning_timeout_seconds',
  'automatic_capability_probes_enabled',
  'model_case_insensitive_matching',
  'tool_call_merge_strict',
  'tool_arguments_strict',
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
  'upstream_shared_host_failure_domain_enabled',
  'upstream_common_mode_same_host_transient_enabled',
  'upstream_capacity_failure_cooldown_enabled',
  'upstream_transient_route_cooldown_base_seconds',
  'upstream_transient_route_cooldown_max_seconds',
  'upstream_transient_route_cooldown_max_step',
  'upstream_route_health_half_open_ttl_seconds',
  'upstream_route_health_enforcement_enabled',
  'upstream_route_half_open_exclusive_window_ms',
  'upstream_route_half_open_busy_max_rounds',
  'upstream_retry_after_cap_seconds',
  'upstream_retry_after_cooldown_cap_seconds',
  'upstream_credentials_first_strike_seconds',
  'upstream_error_body_excerpt_enabled',
  'upstream_error_body_excerpt_max_chars',
  'upstream_route_exhaustion_retry_enabled',
  'upstream_route_exhaustion_retry_max_wait_ms',
  'upstream_route_exhaustion_retry_max_rounds',
  'upstream_route_exhaustion_budget_alignment_enabled',
  'upstream_route_exhaustion_alignment_truncated_enabled',
  'upstream_transient_last_resort_probe_enabled',
  'upstream_common_mode_breaker_threshold',
  'upstream_common_mode_transient_threshold',
  'upstream_continuation_pin_escape_enabled',
  'upstream_local_lease_ttl_seconds',
  'upstream_lease_stale_after_ms',
  'default_upstream_max_concurrency',
  'upstream_account_queue_enabled',
  'upstream_account_queue_max_depth',
  'upstream_account_queue_max_wait_ms',
  'upstream_account_queue_poll_interval_ms',
  'upstream_account_queue_adaptive_budget_enabled',
  'upstream_account_queue_skip_when_doomed_enabled',
  'upstream_account_queue_adaptive_budget_factor',
  'upstream_account_queue_adaptive_budget_ceiling_ms',
  'upstream_local_gate_max_wait_ms',
  'upstream_local_gate_fast_fail_enabled',
  'upstream_local_gate_distinct_error_code_enabled',
  'stream_decode_error_code_split_enabled',
  'stream_max_skipped_bad_frames',
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
  'upstream_first_semantic_output_timeout_seconds',
  'upstream_first_output_warn_after_seconds',
  'gateway_request_body_limit_mb',
  'portal_oidc_enabled',
  'portal_oidc_registration_enabled',
  'portal_oidc_allowed_email_domains',
  'portal_session_ttl_seconds',
  'portal_oidc_pkce_enabled',
  'portal_oidc_verify_id_token',
  'portal_oidc_client_id',
  'portal_oidc_client_secret',
  'portal_oidc_redirect_url',
  'portal_oidc_issuer_url',
  'portal_oidc_authorization_endpoint',
  'portal_oidc_token_endpoint',
  'portal_oidc_userinfo_endpoint',
  'portal_oidc_scopes',
  'portal_oidc_auth_style',
  'portal_oidc_user_id_field',
  'portal_oidc_email_field',
  'portal_oidc_username_field',
  'portal_oidc_display_name_field'
]

describe('runtime settings catalog', () => {
  it('catalogs every managed field exactly once in seven groups', () => {
    expect(runtimeSettingGroups.map(group => group.id)).toEqual([
      'general',
      'discovery',
      'routing',
      'concurrency',
      'http',
      'logs',
      'observability',
      'portal'
    ])
    expect(runtimeSettingFields).toHaveLength(100)
    expect(new Set(runtimeSettingFields.map(field => field.key)).size).toBe(100)
    expect(runtimeSettingFields.map(field => field.key).sort()).toEqual(expectedKeys.sort())
    expect(runtimeSettingFields.filter(field => field.apply === 'immediate')).toHaveLength(87)
    expect(runtimeSettingFields.filter(field => field.apply === 'restart')).toHaveLength(13)
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
    invalid.upstream_first_output_warn_after_seconds = 101
    invalid.upstream_concurrency_recovery_max_wait_ms = 60_000
    invalid.upstream_concurrency_recovery_max_rounds = 2
    invalid.upstream_concurrency_probe_delays_ms = [100]
    invalid.capability_probe_concurrency = 0
    invalid.upstream_lease_stale_after_ms = 500
    invalid.upstream_account_queue_max_depth = 0
    invalid.upstream_account_queue_max_wait_ms = 50
    invalid.upstream_account_queue_poll_interval_ms = 5
    invalid.upstream_local_gate_max_wait_ms = 61_000

    const errors = validateRuntimeSettings(invalid)
    expect(errors.app_name).toBeTruthy()
    expect(errors.default_upstream_max_concurrency).toBeTruthy()
    expect(errors.capability_probe_reasoning_timeout_seconds).toBeTruthy()
    expect(errors.upstream_transient_route_cooldown_base_seconds).toBeTruthy()
    expect(errors.upstream_stream_keepalive_interval_seconds).toBeTruthy()
    expect(errors.upstream_stream_idle_timeout_seconds).toBeTruthy()
    expect(errors.upstream_first_semantic_output_timeout_seconds).toBeTruthy()
    expect(errors.upstream_first_output_warn_after_seconds).toBeTruthy()
    expect(errors.upstream_concurrency_recovery_max_rounds).toBeTruthy()
    expect(errors.capability_probe_concurrency).toBeTruthy()
    expect(errors.upstream_lease_stale_after_ms).toBeTruthy()
    expect(errors.upstream_account_queue_max_depth).toBeTruthy()
    expect(errors.upstream_account_queue_max_wait_ms).toBeTruthy()
    expect(errors.upstream_account_queue_poll_interval_ms).toBeTruthy()
    expect(errors.upstream_local_gate_max_wait_ms).toBeTruthy()
  })

  it('rejects a queue poll interval longer than the queue wait budget', () => {
    // The queue would time out before it ever polled, silently disabling it.
    const settings = validSettings()
    settings.upstream_account_queue_max_wait_ms = 1_000
    settings.upstream_account_queue_poll_interval_ms = 2_000

    expect(
      validateRuntimeSettings(settings).upstream_account_queue_poll_interval_ms
    ).toBeTruthy()

    settings.upstream_account_queue_poll_interval_ms = 1_000
    expect(
      validateRuntimeSettings(settings).upstream_account_queue_poll_interval_ms
    ).toBeFalsy()
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

describe('portal OIDC settings', () => {
  it('appears as a settings group with the six switches plus the thirteen wiring fields', () => {
    const portalGroup = runtimeSettingGroups.find(group => group.id === 'portal')
    expect(portalGroup).toBeDefined()
    const portalFields = runtimeSettingFields.filter(field => field.group === 'portal')
    expect(portalFields.map(field => field.key)).toEqual([
      'portal_oidc_enabled',
      'portal_oidc_registration_enabled',
      'portal_oidc_allowed_email_domains',
      'portal_session_ttl_seconds',
      'portal_oidc_pkce_enabled',
      'portal_oidc_verify_id_token',
      'portal_oidc_client_id',
      'portal_oidc_client_secret',
      'portal_oidc_redirect_url',
      'portal_oidc_issuer_url',
      'portal_oidc_authorization_endpoint',
      'portal_oidc_token_endpoint',
      'portal_oidc_userinfo_endpoint',
      'portal_oidc_scopes',
      'portal_oidc_auth_style',
      'portal_oidc_user_id_field',
      'portal_oidc_email_field',
      'portal_oidc_username_field',
      'portal_oidc_display_name_field'
    ])
    expect(portalFields.every(field => field.apply === 'immediate')).toBe(true)
  })
})

describe('portal OIDC settings save path', () => {
  it('allows an empty email-domain allowlist (empty = unrestricted)', () => {
    const settings = validSettings()
    settings.portal_oidc_allowed_email_domains = ''
    expect(validateRuntimeSettings(settings)).toEqual({})
  })

  it('still flags non-allowEmpty text fields as required', () => {
    const settings = validSettings()
    ;(settings as unknown as Record<string, unknown>).app_name = ''
    expect(validateRuntimeSettings(settings).app_name).toBeTruthy()
  })
})
