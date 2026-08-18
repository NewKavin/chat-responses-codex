import { ref, watch, type Ref } from 'vue'

export interface TableColumnDefinition {
  key: string
  label: string
}

export type ColumnPreferenceStorage = Pick<Storage, 'getItem' | 'setItem'>

const serializeTableColumnPreference = (keys: string[]) => JSON.stringify(keys)

const parseStoredTableColumnPreference = (value: string | null): string[] | null => {
  if (!value) return null
  try {
    const parsed = JSON.parse(value)
    return Array.isArray(parsed) ? parsed : null
  } catch {
    return null
  }
}

const normalizeVisibleTableColumnKeys = (
  columns: TableColumnDefinition[],
  storedKeys: string[] | string | null,
  defaultKeys?: readonly string[]
) => {
  const availableKeys = new Set(columns.map(column => column.key))
  const normalize = (keys: readonly string[]) => {
    const seen = new Set<string>()
    return keys.filter(key => {
      if (typeof key !== 'string' || !availableKeys.has(key) || seen.has(key)) return false
      seen.add(key)
      return true
    })
  }

  const stored = typeof storedKeys === 'string'
    ? parseStoredTableColumnPreference(storedKeys)
    : storedKeys
  const preferred = stored ?? defaultKeys
  const selected = normalize(preferred ?? [])
  return selected.length > 0 ? selected : columns.map(column => column.key)
}

const readTableColumnPreference = (
  storage: ColumnPreferenceStorage,
  storageKey: string,
  columns: TableColumnDefinition[],
  defaultKeys?: readonly string[]
) => {
  try {
    return normalizeVisibleTableColumnKeys(
      columns,
      parseStoredTableColumnPreference(storage.getItem(storageKey)),
      defaultKeys
    )
  } catch {
    return normalizeVisibleTableColumnKeys(columns, null, defaultKeys)
  }
}

const writeTableColumnPreference = (
  storage: ColumnPreferenceStorage,
  storageKey: string,
  keys: string[]
) => {
  try {
    storage.setItem(storageKey, serializeTableColumnPreference(keys))
  } catch {
    // Column preferences remain usable for this session when storage is unavailable.
  }
}

const useTableColumnPreferences = (
  columns: TableColumnDefinition[],
  storageKey: string,
  defaultKeys?: readonly string[]
): {
  visibleColumnKeys: Ref<string[]>
  isColumnVisible: (key: string) => boolean
} => {
  const visibleColumnKeys = ref(
    typeof window === 'undefined'
      ? normalizeVisibleTableColumnKeys(columns, null, defaultKeys)
      : readTableColumnPreference(window.localStorage, storageKey, columns, defaultKeys)
  )

  if (typeof window !== 'undefined') {
    watch(visibleColumnKeys, keys => {
      writeTableColumnPreference(window.localStorage, storageKey, keys)
    }, { deep: true })
  }

  const isColumnVisible = (key: string) => visibleColumnKeys.value.includes(key)

  return { visibleColumnKeys, isColumnVisible }
}

export {
  normalizeVisibleTableColumnKeys,
  readTableColumnPreference,
  serializeTableColumnPreference,
  useTableColumnPreferences,
  writeTableColumnPreference
}
