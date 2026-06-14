const normalizeSlug = (value: string) => value.trim()

const dedupeSlugs = (slugs: string[]) => {
  const seen = new Set<string>()
  const result: string[] = []

  for (const slug of slugs) {
    const normalized = normalizeSlug(slug)
    if (!normalized || seen.has(normalized)) continue
    seen.add(normalized)
    result.push(normalized)
  }

  return result
}

export const resolvePortalQuotaModelSlugs = (
  modelAllowlist: string[],
  availableModelSlugs: string[]
) => {
  const allowlist = dedupeSlugs(modelAllowlist)
  return allowlist.length > 0 ? allowlist : dedupeSlugs(availableModelSlugs)
}
