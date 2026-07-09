import { describe, expect, it } from 'vitest'
import {
  buildTroubleshootingCopySummary,
  getFallbackStageLabel,
  getActiveRequestHealth,
  getClientProfileDefaults,
  getTroubleshootingStatusMeta,
  getTroubleshootingSuggestion
} from '../../src/utils/troubleshooting'

describe('troubleshooting utils', () => {
  it('defaults Cline to model, chat stream, and tools diagnostics', () => {
    expect(getClientProfileDefaults('cline').checks).toEqual(['models', 'chat_stream', 'tools'])
  })

  it('defaults Claude Code to messages stream and count_tokens', () => {
    expect(getClientProfileDefaults('claude_code').checks).toEqual([
      'models',
      'messages_stream',
      'count_tokens'
    ])
  })

  it('labels result statuses', () => {
    expect(getTroubleshootingStatusMeta('passed').label).toBe('通过')
    expect(getTroubleshootingStatusMeta('warning').type).toBe('warning')
    expect(getTroubleshootingStatusMeta('failed').type).toBe('danger')
    expect(getTroubleshootingStatusMeta('timeout').label).toBe('超时')
  })

  it('builds a copy summary without secrets', () => {
    const summary = buildTroubleshootingCopySummary({
      run_id: 'diag_1',
      client_profile: 'cline',
      model: 'GLM-5.1',
      status: 'completed',
      summary: {
        passed: 0,
        warning: 0,
        failed: 1,
        timeout: 0
      },
      duration_ms: 1000,
      copy_summary: 'copy',
      log_filter: 'downstream_id=test',
      results: [
        {
          id: 'chat_stream',
          label: 'Chat Completions stream',
          status: 'failed',
          protocol: 'chat',
          http_status: 503,
          duration_ms: 1000,
          summary: 'upstream temporary unavailable',
          details: 'upstream key sk-secret must not leak',
          error_category: 'upstream_temporary_unavailable',
          suggestion: '稍后重试',
          copy_summary: 'Chat stream failed',
          log_filter: { model: 'GLM-5.1', time_range: '1h' }
        }
      ]
    })
    expect(summary).toContain('diag_1')
    expect(summary).toContain('upstream_temporary_unavailable')
    expect(summary).not.toContain('sk-secret')
  })

  it('marks active requests idle after 120 seconds', () => {
    expect(getActiveRequestHealth({ idle_seconds: 121, status: 'streaming' }).label).toBe('无增量')
    expect(getActiveRequestHealth({ idle_seconds: 10, status: 'streaming' }).label).toBe('运行中')
    expect(getActiveRequestHealth({ idle_seconds: 1, status: 'error' }).type).toBe('danger')
  })

  it('explains Cline model capability warning separately from gateway errors', () => {
    expect(getClientProfileDefaults('cline').description).toContain('模型能力提示')
  })

  it('maps quota and upstream categories to user actions', () => {
    expect(getTroubleshootingSuggestion('gateway_daily_token_quota_exceeded')).toContain('Token 限额')
    expect(getTroubleshootingSuggestion('upstream_rate_limited')).toContain('上游限流')
    expect(getTroubleshootingSuggestion('stream_idle_timeout')).toContain('流式')
  })

  it('formats compatibility matrix fallback stage labels', () => {
    expect(getFallbackStageLabel('high_fidelity')).toBe('高保真')
    expect(getFallbackStageLabel('extension_cleanup')).toBe('扩展字段清理')
    expect(getFallbackStageLabel('tool_replay_reduction')).toBe('工具重放缩减')
    expect(getFallbackStageLabel('history_compaction')).toBe('历史压缩')
    expect(getFallbackStageLabel(undefined)).toBe('原生')
  })
})
