import { BarChart, LineChart, PieChart } from 'echarts/charts'
import {
  GridComponent,
  LegendComponent,
  TooltipComponent,
} from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'

export type EchartsModule = typeof import('echarts/core')

let echartsLoader: Promise<EchartsModule> | null = null

export const loadEcharts = (): Promise<EchartsModule> => {
  if (!echartsLoader) {
    echartsLoader = import('echarts/core').then(echarts => {
      echarts.use([
        BarChart,
        CanvasRenderer,
        GridComponent,
        LegendComponent,
        LineChart,
        PieChart,
        TooltipComponent,
      ])
      return echarts
    })
  }
  return echartsLoader
}

export const __resetEchartsLoaderForTests = (): void => {
  echartsLoader = null
}
