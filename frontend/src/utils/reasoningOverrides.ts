import type { CapabilitySource, ReasoningEffortLevel } from '@/types'

export const REASONING_EFFORT_LEVELS = [
  'none',
  'low',
  'medium',
  'high',
  'xhigh',
  'max'
] as const satisfies readonly ReasoningEffortLevel[]

export const normalizeReasoningLevels = (
  levels: readonly string[]
): ReasoningEffortLevel[] => {
  const selected = new Set(levels)
  return REASONING_EFFORT_LEVELS.filter(level => selected.has(level))
}

export const reasoningSourceLabel = (source: CapabilitySource): string => {
  switch (source) {
    case 'override':
      return '手工'
    case 'probe':
      return '探测'
    case 'policy':
      return '预设'
    case 'baseline':
      return '未配置'
  }
}
