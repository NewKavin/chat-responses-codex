import { describe, expect, it } from 'vitest'
import {
  normalizeVisibleTableColumnKeys,
  readTableColumnPreference,
  serializeTableColumnPreference,
  writeTableColumnPreference
} from '@/composables/useTableColumns'

const columns = [
  { key: 'name', label: '名称' },
  { key: 'protocol', label: '协议' },
  { key: 'models', label: '模型数量' },
  { key: 'remark', label: '备注' }
]

describe('table column preferences', () => {
  it('normalizes stored selections while preserving editable order', () => {
    expect(
      normalizeVisibleTableColumnKeys(
        columns,
        ['remark', 'missing', 'name', 'remark'],
        ['protocol', 'name']
      )
    ).toEqual(['remark', 'name'])
  })

  it('falls back to valid defaults and then all available columns', () => {
    expect(normalizeVisibleTableColumnKeys(columns, 'invalid', ['missing', 'models'])).toEqual(['models'])
    expect(normalizeVisibleTableColumnKeys(columns, null, undefined)).toEqual([
      'name',
      'protocol',
      'models',
      'remark'
    ])
  })

  it('reads and writes JSON safely without throwing on storage failures', () => {
    const values = new Map<string, string>()
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => {
        values.set(key, value)
      }
    }

    writeTableColumnPreference(storage, 'columns', ['remark'])
    expect(values.get('columns')).toBe(serializeTableColumnPreference(['remark']))
    expect(readTableColumnPreference(storage, 'columns', columns)).toEqual(['remark'])

    values.set('columns', '{bad json')
    expect(readTableColumnPreference(storage, 'columns', columns, ['models'])).toEqual(['models'])

    const failing = {
      getItem: () => {
        throw new Error('read denied')
      },
      setItem: () => {
        throw new Error('write denied')
      }
    }
    expect(readTableColumnPreference(failing, 'columns', columns, ['name'])).toEqual(['name'])
    expect(() => writeTableColumnPreference(failing, 'columns', ['name'])).not.toThrow()
  })
})
