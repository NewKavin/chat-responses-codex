import { describe, expect, it } from 'vitest'
import {
  extractReadableErrorMessage,
  summarizeErrorText,
  type ReadableErrorSource
} from '../../src/utils/errorDisplay'

describe('errorDisplay', () => {
  it('extracts OpenAI-compatible error messages', () => {
    const source: ReadableErrorSource = {
      status: 503,
      statusText: 'Service Unavailable',
      body: {
        error: {
          message: 'upstream temporary unavailable',
          code: 'upstream_temporary_unavailable',
          category: 'upstream_temporary_unavailable'
        }
      }
    }

    expect(extractReadableErrorMessage(source)).toBe(
      '503 Service Unavailable：upstream temporary unavailable（upstream_temporary_unavailable）'
    )
  })

  it('extracts message from JSON text bodies', () => {
    expect(
      extractReadableErrorMessage({
        status: 429,
        statusText: 'Too Many Requests',
        bodyText: '{"error":{"message":"日 Token 限额已用尽","code":"gateway_daily_token_quota_exceeded"}}'
      })
    ).toBe('429 Too Many Requests：日 Token 限额已用尽（gateway_daily_token_quota_exceeded）')
  })

  it('extracts top-level structured object messages', () => {
    expect(
      extractReadableErrorMessage({
        status: 400,
        statusText: 'Bad Request',
        body: {
          message: 'plain object rejected',
          code: 'gateway_invalid_request',
          category: 'gateway_invalid_request'
        }
      })
    ).toBe('400 Bad Request：plain object rejected（gateway_invalid_request）')
  })

  it('extracts string error object bodies', () => {
    expect(
      extractReadableErrorMessage({
        status: 502,
        statusText: 'Bad Gateway',
        body: {
          error: 'upstream exploded'
        }
      })
    ).toBe('502 Bad Gateway：upstream exploded')
  })

  it('summarizes long plain text without breaking short messages', () => {
    expect(summarizeErrorText('short message')).toBe('short message')
    expect(summarizeErrorText('x'.repeat(220), 24)).toBe(`${'x'.repeat(24)}...`)
    expect(summarizeErrorText('')).toBe('-')
  })
})
