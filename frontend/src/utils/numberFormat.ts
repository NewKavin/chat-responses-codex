const trimTrailingZeros = (value: string) =>
  value.replace(/\.0+$|(\.\d*?[1-9])0+$/, '$1')

export const formatCompactNumber = (value: number, fallbackLocale = 'zh-CN') => {
  if (!Number.isFinite(value)) {
    return '0'
  }

  const abs = Math.abs(value)
  const units = [
    { threshold: 1_000_000_000, suffix: 'B' },
    { threshold: 1_000_000, suffix: 'M' },
    { threshold: 1_000, suffix: 'K' }
  ]

  for (const unit of units) {
    if (abs >= unit.threshold) {
      const scaled = value / unit.threshold
      const digits = Math.abs(scaled) >= 100 ? 0 : Math.abs(scaled) >= 10 ? 1 : 2
      return `${trimTrailingZeros(scaled.toFixed(digits))}${unit.suffix}`
    }
  }

  return value.toLocaleString(fallbackLocale)
}
