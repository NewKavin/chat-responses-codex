export const hasUsablePlaintextKey = (key: unknown): key is string =>
  typeof key === 'string' && key.trim().length > 0

export const maskPlaintextKey = (key: string) => {
  const normalized = key.trim()
  if (normalized.length <= 10) return '********'
  return `${normalized.slice(0, 6)}...${normalized.slice(-4)}`
}

export const getCopyableKey = (key: unknown): string | null =>
  hasUsablePlaintextKey(key) ? key.trim() : null
