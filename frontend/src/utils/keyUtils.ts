export const hasUsablePlaintextKey = (key: unknown): key is string =>
  typeof key === 'string' && key.trim().length > 0

export const maskPlaintextKey = (key: string) => {
  if (key.length <= 10) return key
  return `${key.slice(0, 6)}...${key.slice(-4)}`
}

export const getCopyableKey = (key: unknown): string | null =>
  hasUsablePlaintextKey(key) ? key.trim() : null
