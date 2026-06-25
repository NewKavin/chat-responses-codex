export const formatInferenceStrength = (value?: string | null) => {
  const trimmed = value?.trim()
  return trimmed && trimmed.length > 0 ? trimmed : '-'
}
