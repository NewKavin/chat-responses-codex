import { afterEach, describe, expect, it, vi } from 'vitest'
import { portalApi, portalHttp } from './portal'

afterEach(() => {
  vi.restoreAllMocks()
})

describe('Portal Keys API', () => {
  describe('listKeys', () => {
    it('returns array of keys', async () => {
      const mockKeys = [
        {
          downstream_id: 'key1',
          label: 'Production Key',
          model_group_id: 'default',
          created_at: 1704067200,
          usage_count: 42,
          is_default: true
        },
        {
          downstream_id: 'key2',
          label: 'Test Key',
          model_group_id: 'testing',
          created_at: 1704153600,
          usage_count: 5,
          is_default: false
        }
      ]

      const get = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: mockKeys })

      const result = await portalApi.listKeys()

      expect(get).toHaveBeenCalledWith('/portal/keys')
      expect(result.data).toEqual(mockKeys)
      expect(result.data).toHaveLength(2)
      expect(result.data[0].downstream_id).toBe('key1')
    })

    it('returns empty array when no keys exist', async () => {
      vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: [] })

      const result = await portalApi.listKeys()

      expect(result.data).toEqual([])
      expect(result.data).toHaveLength(0)
    })
  })

  describe('createKey', () => {
    it('posts correct data for key creation', async () => {
      const post = vi.spyOn(portalHttp, 'post').mockResolvedValue({ data: { success: true } })
      const request = {
        downstream_id: 'new-key',
        label: 'New Key',
        model_group_id: 'prod'
      }

      await portalApi.createKey(request)

      expect(post).toHaveBeenCalledWith('/portal/keys', request)
    })

    it('posts minimal data when optional fields omitted', async () => {
      const post = vi.spyOn(portalHttp, 'post').mockResolvedValue({ data: { success: true } })
      const request = {
        downstream_id: 'minimal-key'
      }

      await portalApi.createKey(request)

      expect(post).toHaveBeenCalledWith('/portal/keys', request)
    })
  })

  describe('getKeyDetails', () => {
    it('fetches single key by downstream_id', async () => {
      const mockKey = {
        downstream_id: 'key1',
        label: 'Production Key',
        model_group_id: 'default',
        created_at: 1704067200,
        usage_count: 42,
        is_default: true
      }

      const get = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: mockKey })

      const result = await portalApi.getKeyDetails('key1')

      expect(get).toHaveBeenCalledWith('/portal/keys/key1')
      expect(result.data.downstream_id).toBe('key1')
      expect(result.data.label).toBe('Production Key')
    })
  })

  describe('rotateKeyById', () => {
    it('posts rotation request with new downstream_id', async () => {
      const post = vi.spyOn(portalHttp, 'post').mockResolvedValue({ data: { success: true } })

      await portalApi.rotateKeyById('old-key', 'new-key')

      expect(post).toHaveBeenCalledWith('/portal/keys/old-key/rotate', {
        new_downstream_id: 'new-key'
      })
    })
  })

  describe('setDefaultKey', () => {
    it('sends PUT request to set default', async () => {
      const put = vi.spyOn(portalHttp, 'put').mockResolvedValue({ data: { success: true } })

      await portalApi.setDefaultKey('key1')

      expect(put).toHaveBeenCalledWith('/portal/keys/key1/default')
    })
  })

  describe('deleteKey', () => {
    it('sends DELETE request to remove key', async () => {
      const del = vi.spyOn(portalHttp, 'delete').mockResolvedValue({ data: { success: true } })

      await portalApi.deleteKey('key1')

      expect(del).toHaveBeenCalledWith('/portal/keys/key1')
    })
  })

  describe('listModelGroups', () => {
    it('returns the available model groups', async () => {
      const mockGroups = {
        groups: [
          {
            id: 'basic',
            name: 'Basic',
            description: null,
            allowed_models: ['gpt-3.5-turbo'],
            created_at: 1,
            updated_at: 1
          },
          {
            id: 'premium',
            name: 'Premium',
            description: null,
            allowed_models: ['gpt-4'],
            created_at: 1,
            updated_at: 1
          }
        ]
      }

      const get = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: mockGroups })

      const result = await portalApi.listModelGroups()

      expect(get).toHaveBeenCalledWith('/portal/model-groups')
      expect(result.data.groups).toHaveLength(2)
      expect(result.data.groups[0].id).toBe('basic')
      expect(result.data.groups[1].id).toBe('premium')
    })
  })

  describe('updateKeyModelGroup', () => {
    it('sends PUT request to change a key model group', async () => {
      const put = vi.spyOn(portalHttp, 'put').mockResolvedValue({ data: { success: true } })

      await portalApi.updateKeyModelGroup('key1', 'premium')

      expect(put).toHaveBeenCalledWith('/portal/keys/key1/model-group', {
        model_group_id: 'premium'
      })
    })
  })
})
