import { describe, expect, it } from 'vitest'
import {
  buildVisibleLogSummary,
  errorCategoryGroups,
  formatErrorCategory,
  formatInferenceStrength
} from '../../src/utils/logDisplay'

describe('formatInferenceStrength', () => {
  it('returns the trimmed raw value when present', () => {
    expect(formatInferenceStrength(' xhigh ')).toBe('xhigh')
    expect(formatInferenceStrength('high')).toBe('high')
  })

  it('falls back to a dash when the value is empty', () => {
    expect(formatInferenceStrength('')).toBe('-')
    expect(formatInferenceStrength('   ')).toBe('-')
    expect(formatInferenceStrength(undefined)).toBe('-')
  })
})

describe('error category display', () => {
  it('keeps gateway quota and upstream categories available', () => {
    const values = errorCategoryGroups.flatMap(group => group.options.map(option => option.value))
    expect(values).toContain('gateway_daily_token_quota_exceeded')
    expect(values).toContain('upstream_temporary_unavailable')
    expect(values).toContain('stream_idle_timeout')
  })

  it('formats known and unknown categories', () => {
    expect(formatErrorCategory('gateway_daily_token_quota_exceeded')).toBe('日 Token 限额')
    expect(formatErrorCategory('custom_error')).toBe('custom_error')
    expect(formatErrorCategory('')).toBe('-')
  })

  it('summarizes visible page failures by group', () => {
    const summary = buildVisibleLogSummary([
      { status_code: 200, error_category: '' },
      { status_code: 429, error_category: 'gateway_daily_token_quota_exceeded' },
      { status_code: 503, error_category: 'upstream_temporary_unavailable' },
      { status_code: 504, error_category: 'stream_idle_timeout' }
    ])

    expect(summary.total).toBe(4)
    expect(summary.failed).toBe(3)
    expect(summary.gatewayQuota).toBe(1)
    expect(summary.upstreamFeedback).toBe(1)
    expect(summary.streaming).toBe(1)
  })
})
