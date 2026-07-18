import type { ResolvedTheme } from '@/composables/useTheme'

export interface ChartTheme {
  text: string
  muted: string
  border: string
  splitLine: string
  tooltipBackground: string
  tooltipBorder: string
  series: string[]
}

export const buildChartTheme = (mode: ResolvedTheme): ChartTheme => mode === 'dark'
  ? {
      text: '#d2dad6',
      muted: '#98a59f',
      border: '#343d39',
      splitLine: '#2a322f',
      tooltipBackground: '#202624',
      tooltipBorder: '#343d39',
      series: [
        '#39b99c',
        '#60a5d8',
        '#4ade80',
        '#f6ad55',
        '#fb7185',
        '#a78bda',
        '#67c7d4',
        '#d6b46c'
      ]
  }
  : {
      text: '#34413d',
      muted: '#66716d',
      border: '#dfe5e2',
      splitLine: '#e9eeec',
      tooltipBackground: '#ffffff',
      tooltipBorder: '#dfe5e2',
      series: [
        '#0f8f76',
        '#2563a6',
        '#15803d',
        '#b45309',
        '#c2413b',
        '#7456a6',
        '#258a9a',
        '#98732e'
      ]
    }

/** 图表入场动效:本地计算,无外部依赖 */
export const chartEnterAnimation = {
  animationDuration: 700,
  animationEasing: 'cubicOut',
  animationDelay: (index: number) => Math.min(index * 45, 450)
} as const
