export const resolveActiveNavigationPath = (
  currentPath: string,
  navigationPaths: readonly string[],
  fallback: string
) => {
  let bestMatch = ''

  for (const path of navigationPaths) {
    const matches = currentPath === path || currentPath.startsWith(`${path}/`)
    if (matches && path.length > bestMatch.length) bestMatch = path
  }

  return bestMatch || fallback
}
