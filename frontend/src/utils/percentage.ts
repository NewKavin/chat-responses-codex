export const formatPercentageTwoDecimals = (value: number): number => {
  if (!Number.isFinite(value)) return 0
  return Math.round(value * 100) / 100
}

export const formatPercentageLabel = (value: number): string =>
  `${formatPercentageTwoDecimals(value).toFixed(2)}%`
