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
      text: '#b3c4bd',
      muted: '#7c918a',
      border: '#24312c',
      splitLine: '#1c2723',
      tooltipBackground: '#18221e',
      tooltipBorder: '#34453d',
      series: [
        '#2fe0a8',
        '#6cb8f0',
        '#4ade80',
        '#fbbf24',
        '#fb7185',
        '#a78bfa',
        '#67e8f9',
        '#d6b46c'
      ]
  }
  : {
      text: '#2b3834',
      muted: '#5c6a65',
      border: '#dce3e0',
      splitLine: '#e7ece9',
      tooltipBackground: '#ffffff',
      tooltipBorder: '#dce3e0',
      series: [
        '#0a8f6f',
        '#2369ad',
        '#15803d',
        '#b45309',
        '#cf3f38',
        '#6d5bd0',
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
