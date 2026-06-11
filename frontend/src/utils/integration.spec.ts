import { describe, expect, it } from 'vitest'
import {
  buildClaudeCodeSettingsJson,
  buildCodexAuthLoginCommand,
  buildCodexConfigToml,
  buildCodexModelCatalogJson,
  buildGatewayBaseUrl,
  buildGatewayModelsEndpoint,
  buildOpenCodeConfig,
  extractGatewayModelSlugs,
  rankModelSlugsByUsage,
  sortPortalModelStats
} from './integration'

describe('integration config generators', () => {
  it('builds a gateway base url from an origin and trims trailing slash', () => {
    expect(buildGatewayBaseUrl('http://localhost:3001/')).toBe('http://localhost:3001')
    expect(buildGatewayBaseUrl('https://portal.example.com')).toBe('https://portal.example.com')
  })

  it('builds the models endpoint from the gateway base url', () => {
    expect(buildGatewayModelsEndpoint('http://localhost:3001')).toBe('http://localhost:3001/v1/models')
  })

  it('extracts unique model ids from a /v1/models payload', () => {
    expect(
      extractGatewayModelSlugs({
        data: [
          { id: 'ZhipuAI/GLM-5' },
          { id: '  MiniMax/MiniMax-M2.7  ' },
          { id: 'ZhipuAI/GLM-5' },
          { id: '' },
          { id: null }
        ]
      })
    ).toEqual(['ZhipuAI/GLM-5', 'MiniMax/MiniMax-M2.7'])
  })

  it('sorts portal model stats by month usage, then today usage, then model name', () => {
    expect(
      sortPortalModelStats([
        {
          model: 'MiniMax/MiniMax-M2.7',
          today_count: 4,
          month_count: 9,
          today_tokens: 40,
          month_tokens: 90,
          avg_latency_ms: 120,
          success_rate: 0.95
        },
        {
          model: 'ZhipuAI/GLM-5',
          today_count: 6,
          month_count: 9,
          today_tokens: 60,
          month_tokens: 95,
          avg_latency_ms: 110,
          success_rate: 0.98
        },
        {
          model: 'DeepSeek/DeepSeek-V3',
          today_count: 1,
          month_count: 3,
          today_tokens: 10,
          month_tokens: 30,
          avg_latency_ms: 90,
          success_rate: 1
        }
      ]).map(stat => stat.model)
    ).toEqual([
      'ZhipuAI/GLM-5',
      'MiniMax/MiniMax-M2.7',
      'DeepSeek/DeepSeek-V3'
    ])
  })

  it('ranks model slugs by portal usage and keeps unseen models at the end', () => {
    expect(
      rankModelSlugsByUsage(
        ['MiniMax/MiniMax-M2.7', 'Qwen/Qwen3'],
        [
          {
            model: 'DeepSeek/DeepSeek-V3',
            today_count: 1,
            month_count: 12,
            today_tokens: 10,
            month_tokens: 120,
            avg_latency_ms: 100,
            success_rate: 0.9
          },
          {
            model: 'MiniMax/MiniMax-M2.7',
            today_count: 3,
            month_count: 20,
            today_tokens: 30,
            month_tokens: 200,
            avg_latency_ms: 80,
            success_rate: 0.99
          }
        ]
      )
    ).toEqual(['MiniMax/MiniMax-M2.7', 'DeepSeek/DeepSeek-V3', 'Qwen/Qwen3'])
  })

  it('builds a codex config that keeps the key out of config.toml', () => {
    const toml = buildCodexConfigToml({
      gatewayBaseUrl: 'https://portal.example.com',
      modelSlug: 'MiniMax/MiniMax-M2.7'
    })

    expect(toml).toContain('model_provider = "gateway"')
    expect(toml).toContain('model = "MiniMax/MiniMax-M2.7"')
    expect(toml).toContain('review_model = "MiniMax/MiniMax-M2.7"')
    expect(toml).toContain('model_catalog_json = "model-catalog.json"')
    expect(toml).toContain('cli_auth_credentials_store = "file"')
    expect(toml).toContain('base_url = "https://portal.example.com/v1"')
    expect(toml).not.toContain('OPENAI_API_KEY')
    expect(toml).not.toContain('sk-downstream-123')
  })

  it('builds a codex model catalog from ranked slugs', () => {
    const catalog = JSON.parse(
      buildCodexModelCatalogJson(['MiniMax/MiniMax-M2.7', 'DeepSeek/DeepSeek-V3'])
    )

    expect(catalog.models).toHaveLength(2)
    expect(catalog.models[0]).toMatchObject({
      slug: 'MiniMax/MiniMax-M2.7',
      display_name: 'MiniMax/MiniMax-M2.7',
      priority: 0
    })
    expect(catalog.models[1]).toMatchObject({
      slug: 'DeepSeek/DeepSeek-V3',
      display_name: 'DeepSeek/DeepSeek-V3',
      priority: 1
    })
  })

  it('builds a codex login command that seeds auth.json', () => {
    expect(buildCodexAuthLoginCommand('sk-downstream-123')).toBe(
      `printf '%s' 'sk-downstream-123' | codex login --with-api-key`
    )
  })

  it('builds an opencode config that is ready to copy', () => {
    const config = JSON.parse(
      buildOpenCodeConfig({
        gatewayBaseUrl: 'https://portal.example.com',
        portalKey: 'sk-downstream-123',
        modelSlugs: ['MiniMax/MiniMax-M2.7', 'DeepSeek/DeepSeek-V3'],
        selectedModelSlug: 'MiniMax/MiniMax-M2.7'
      })
    )

    expect(config.$schema).toBe('https://opencode.ai/config.json')
    expect(config.model).toBe('gateway/MiniMax/MiniMax-M2.7')
    expect(config.small_model).toBe('gateway/DeepSeek/DeepSeek-V3')
    expect(config.provider.gateway.npm).toBe('@ai-sdk/openai-compatible')
    expect(config.provider.gateway.name).toBe('Chat Responses Gateway')
    expect(config.provider.gateway.options.baseURL).toBe('https://portal.example.com/v1')
    expect(config.provider.gateway.options.apiKey).toBe('sk-downstream-123')
    expect(config.provider.gateway.models['MiniMax/MiniMax-M2.7']).toEqual({
      name: 'MiniMax/MiniMax-M2.7'
    })
    expect(config.provider.gateway.models['DeepSeek/DeepSeek-V3']).toEqual({
      name: 'DeepSeek/DeepSeek-V3'
    })
  })

  it('builds claude code settings that are ready to copy', () => {
    const settings = JSON.parse(
      buildClaudeCodeSettingsJson({
        gatewayBaseUrl: 'https://portal.example.com',
        portalKey: 'sk-downstream-123',
        modelSlugs: ['MiniMax/MiniMax-M2.7', 'DeepSeek/DeepSeek-V3'],
        selectedModelSlug: 'MiniMax/MiniMax-M2.7'
      })
    )

    expect(settings.model).toBe('MiniMax/MiniMax-M2.7')
    expect(settings.env.ANTHROPIC_BASE_URL).toBe('https://portal.example.com/v1')
    expect(settings.env.ANTHROPIC_API_KEY).toBe('sk-downstream-123')
    expect(settings.env.ANTHROPIC_AUTH_TOKEN).toBe('sk-downstream-123')
    expect(settings.env.CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY).toBe('1')
    expect(settings.env.ANTHROPIC_CUSTOM_MODEL_OPTION).toBe('MiniMax/MiniMax-M2.7')
    expect(settings.env.ANTHROPIC_CUSTOM_MODEL_OPTION_NAME).toBe('MiniMax/MiniMax-M2.7')
    expect(settings.env.ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION).toContain('portal')
  })
})
