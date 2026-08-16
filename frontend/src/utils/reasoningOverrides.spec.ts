import { describe, expect, it } from 'vitest'
import {
  REASONING_EFFORT_LEVELS,
  normalizeReasoningLevels,
  reasoningSourceLabel
} from './reasoningOverrides'

describe('reasoning overrides', () => {
  it('keeps the editable effort vocabulary in canonical order', () => {
    expect(REASONING_EFFORT_LEVELS).toEqual([
      'low',
      'medium',
      'high',
      'xhigh',
      'max'
    ])
    expect(normalizeReasoningLevels([
      'max',
      'low',
      'high',
      'low',
      'future-level'
    ])).toEqual(['low', 'high', 'max'])
  })

  it('labels effective reasoning sources without conflating probes and overrides', () => {
    expect(reasoningSourceLabel('override')).toBe('手工')
    expect(reasoningSourceLabel('probe')).toBe('探测')
    expect(reasoningSourceLabel('policy')).toBe('预设')
    expect(reasoningSourceLabel('baseline')).toBe('未配置')
  })
})
