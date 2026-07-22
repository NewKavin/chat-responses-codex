import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import {
  buildClaudeCodeSettingsJson,
  buildCodexAuthLoginCommand,
  buildCodexDoctorCommand,
  buildCodexConfigToml,
  buildCodexModelCatalogJson,
  buildGatewayBaseUrl,
  buildAnthropicCompatibleConfig,
  buildOpenAiCompatibleConfig,
  buildGatewayModelsEndpoint,
  buildIntegrationCatalogViewState,
  buildModelUsageStats,
  buildHermesConfigYaml,
  buildOpenCodeConfig,
  extractGatewayModelSlugs,
  rankModelSlugsByUsage,
  sortPortalModelStats
} from '../../src/utils/integration'

const codexTemplate = readFileSync(new URL('../../../templates/codex/config.toml.example', import.meta.url), 'utf8')
const codexGuide = readFileSync(new URL('../../../docs/codex-integration-guide.md', import.meta.url), 'utf8')

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

  it('ranks gateway model slugs by portal usage and keeps unseen models at the end', () => {
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
    ).toEqual(['MiniMax/MiniMax-M2.7', 'Qwen/Qwen3'])
  })

  it('withholds model selection and stats when the live catalog is unavailable or empty', () => {
    const portalStats = [
      {
        model: 'stats-only-model',
        today_count: 8,
        month_count: 30,
        today_tokens: 800,
        month_tokens: 3000,
        avg_latency_ms: 90,
        success_rate: 0.99
      }
    ]

    for (const catalog of [null, { models: [] }]) {
      expect(
        buildIntegrationCatalogViewState({
          catalog,
          modelAllowlist: [],
          portalModelStats: portalStats
        })
      ).toEqual({
        allModelSlugs: [],
        primaryModelSlug: '',
        primaryModelReasoningEffort: 'none',
        sortedModelStats: [],
        canGenerateConfigurationContent: false
      })
    }
  })

  it('keeps upstream casing when stats contain lowercase aliases', () => {
    expect(
      buildModelUsageStats(
        ['ZhipuAI/GLM-5', 'MiniMax/MiniMax-M2.7'],
        [
          {
            model: 'legacy/lowercase-model',
            today_count: 1,
            month_count: 99,
            today_tokens: 10,
            month_tokens: 990,
            avg_latency_ms: 150,
            success_rate: 0.9
          },
          {
            model: 'zhipuai/glm-5',
            today_count: 2,
            month_count: 12,
            today_tokens: 20,
            month_tokens: 120,
            avg_latency_ms: 140,
            success_rate: 0.91
          },
          {
            model: 'minimax/minimax-m2.7',
            today_count: 8,
            month_count: 45,
            today_tokens: 80,
            month_tokens: 450,
            avg_latency_ms: 70,
            success_rate: 0.99
          }
        ]
      )
    ).toEqual([
      {
        model: 'MiniMax/MiniMax-M2.7',
        today_count: 8,
        month_count: 45,
        today_tokens: 80,
        month_tokens: 450,
        avg_latency_ms: 70,
        success_rate: 0.99
      },
      {
        model: 'ZhipuAI/GLM-5',
        today_count: 2,
        month_count: 12,
        today_tokens: 20,
        month_tokens: 120,
        avg_latency_ms: 140,
        success_rate: 0.91
      }
    ])
  })

  it('derives the primary Codex reasoning effort from the live catalog', () => {
    const state = buildIntegrationCatalogViewState({
      catalog: {
        models: [
          {
            slug: 'verified/model',
            default_reasoning_level: 'medium'
          }
        ]
      },
      modelAllowlist: [],
      portalModelStats: []
    })

    expect(state.primaryModelSlug).toBe('verified/model')
    expect(state.primaryModelReasoningEffort).toBe('medium')
  })

  it('matches the downstream allowlist without changing live catalog casing', () => {
    const state = buildIntegrationCatalogViewState({
      catalog: {
        models: [
          {
            slug: 'MiniMax/MiniMax-M2.7',
            default_reasoning_level: 'none'
          }
        ]
      },
      modelAllowlist: ['minimax/minimax-m2.7'],
      portalModelStats: []
    })

    expect(state.allModelSlugs).toEqual(['MiniMax/MiniMax-M2.7'])
    expect(state.primaryModelSlug).toBe('MiniMax/MiniMax-M2.7')
    expect(state.canGenerateConfigurationContent).toBe(true)
  })

  it('uses none when the catalog default reasoning effort is absent', () => {
    const state = buildIntegrationCatalogViewState({
      catalog: { models: [{ slug: 'unknown/model', default_reasoning_level: null }] },
      modelAllowlist: [],
      portalModelStats: []
    })

    expect(state.primaryModelReasoningEffort).toBe('none')
  })

  it('builds a codex config that keeps the key out of config.toml', () => {
    const input = {
      gatewayBaseUrl: 'https://portal.example.com',
      modelSlug: 'MiniMax/MiniMax-M2.7',
      modelReasoningEffort: 'medium'
    }
    const toml = buildCodexConfigToml(input)

    expect(toml).toContain('model_provider = "gateway"')
    expect(toml).toContain('model = "MiniMax/MiniMax-M2.7"')
    expect(toml).toContain('review_model = "MiniMax/MiniMax-M2.7"')
    expect(toml).toContain('model_reasoning_effort = "medium"')
    expect(toml).not.toContain('model_reasoning_effort = "high"')
    expect(toml).toContain('model_catalog_json = "model-catalog.json"')
    expect(toml).toContain('cli_auth_credentials_store = "file"')
    expect(toml).toContain('multi_agent = true')
    expect(toml).toContain('max_threads = 8')
    expect(toml).toContain('max_depth = 3')
    expect(toml).toContain('base_url = "https://portal.example.com/v1"')
    expect(toml).toContain('stream_max_retries = 0')
    expect(toml.indexOf('web_search = "disabled"')).toBeLessThan(toml.indexOf('[features]'))
    expect(toml).not.toContain('disable_response_storage')
    expect(toml).not.toContain('OPENAI_API_KEY')
    expect(toml).not.toContain('sk-downstream-123')
  })

  it('builds a hermes config using the current model schema', () => {
    const yaml = buildHermesConfigYaml({
      gatewayBaseUrl: 'https://portal.example.com',
      portalKey: 'sk-downstream-123',
      modelSlug: 'MiniMax/MiniMax-M2.7'
    })

    expect(yaml).toContain('model:')
    expect(yaml).toContain('provider: custom')
    expect(yaml).toContain('default: "MiniMax/MiniMax-M2.7"')
    expect(yaml).toContain('base_url: "https://portal.example.com/v1"')
    expect(yaml).toContain('api_key: "${CHAT2RESPONSES_KEY}"')
    expect(yaml).toContain('CHAT2RESPONSES_KEY=sk-downstream-123')
    expect(yaml).not.toContain('providers:')
    expect(yaml).not.toContain('key_env:')
  })

  it('uses live Codex capabilities without exposing legacy route diagnostics', () => {
    const live = {
      upstream_id: 'top-level-internal-upstream',
      configuration_fingerprint: 'top-level-internal-fingerprint',
      models: [
        {
          slug: 'opaque',
          input_modalities: ['text', 'image'],
          supports_parallel_tool_calls: false,
          apply_patch_tool_type: null,
          gateway_catalog_witness: {
            upstream_id: 'internal-upstream',
            configuration_id: 'sha256:internal',
            profile_key: { upstream_id: 'internal-upstream' }
          },
          upstream_id: 'model-level-internal-upstream',
          metadata: {
            safe_label: 'keep-me',
            profile_key: { upstream_id: 'nested-internal-upstream' },
            key_fingerprint: 'nested-internal-fingerprint'
          }
        }
      ]
    }

    const emitted = JSON.parse(buildCodexModelCatalogJson(live as never))
    expect(emitted).toEqual({
      models: [
        {
          slug: 'opaque',
          input_modalities: ['text', 'image'],
          supports_parallel_tool_calls: false,
          apply_patch_tool_type: null,
          metadata: { safe_label: 'keep-me' }
        }
      ]
    })
    expect(JSON.stringify(emitted)).not.toContain('upstream_id')
    expect(JSON.stringify(emitted)).not.toContain('configuration_id')
    expect(JSON.stringify(emitted)).not.toContain('profile_key')
    expect(JSON.stringify(emitted)).not.toContain('fingerprint')
  })

  it('fails catalog generation when the live catalog is unavailable', () => {
    expect(() => buildCodexModelCatalogJson(undefined as never)).toThrow(
      'live Codex catalog is unavailable'
    )
  })

  it('fails catalog generation when the live catalog has no models', () => {
    expect(() => buildCodexModelCatalogJson({ models: [] })).toThrow(
      'live Codex catalog is empty'
    )
  })

  it('rejects legacy model slug arrays instead of inventing Codex capabilities', () => {
    expect(() => buildCodexModelCatalogJson(['opaque/model'] as never)).toThrow(
      'live Codex catalog is unavailable'
    )
  })

  it('generates a Codex provider with hosted web search disabled', () => {
    const config = buildCodexConfigToml({
      gatewayBaseUrl: 'https://gw.example',
      modelSlug: 'opaque',
      modelReasoningEffort: 'none'
    })

    expect(config).toContain('web_search = "disabled"')
    expect(config).toContain('wire_api = "responses"')
    expect(config).toContain('stream_max_retries = 0')
  })

  it('maps every Claude Code alias to an arbitrary selected gateway slug', () => {
    const settings = JSON.parse(
      buildClaudeCodeSettingsJson({
        gatewayBaseUrl: 'https://gw.example',
        portalKey: 'downstream-key',
        modelSlugs: ['lab/opaque'],
        selectedModelSlug: 'lab/opaque'
      })
    )

    expect(settings.env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBe('lab/opaque')
    expect(settings.env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe('lab/opaque')
    expect(settings.env.ANTHROPIC_DEFAULT_HAIKU_MODEL).toBe('lab/opaque')
  })

  it('builds a codex login command that seeds auth.json', () => {
    expect(buildCodexAuthLoginCommand('sk-downstream-123')).toBe(
      `printf '%s' 'sk-downstream-123' | codex login --with-api-key`
    )
  })

  it('builds the strict-config doctor command for Codex', () => {
    expect(buildCodexDoctorCommand()).toBe('codex --strict-config doctor --summary')
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
    expect(config.permission).toEqual({ '*': 'deny', read: 'allow' })
    expect(config.model).toBe('gateway/MiniMax/MiniMax-M2.7')
    expect(config.small_model).toBe('gateway/DeepSeek/DeepSeek-V3')
    expect(config.provider.gateway.npm).toBe('@ai-sdk/openai-compatible')
    expect(config.provider.gateway.name).toBe('Chat Responses Gateway')
    expect(config.provider.gateway.options.baseURL).toBe('https://portal.example.com/v1')
    expect(config.provider.gateway.options.apiKey).toBe('sk-downstream-123')
    expect(config).not.toHaveProperty('multi_agent')
    expect(config).not.toHaveProperty('max_threads')
    expect(config).not.toHaveProperty('max_depth')
    expect(config.provider.gateway.models['MiniMax/MiniMax-M2.7']).toEqual({
      name: 'MiniMax/MiniMax-M2.7'
    })
    expect(config.provider.gateway.models['DeepSeek/DeepSeek-V3']).toEqual({
      name: 'DeepSeek/DeepSeek-V3'
    })
  })

  it('keeps Codex template and guide auth storage and provider naming aligned', () => {
    for (const content of [codexTemplate, codexGuide]) {
      expect(content).toContain('cli_auth_credentials_store = "file"')
      expect(content).toContain('name = "Chat Responses Gateway"')
    }

    expect(codexGuide).not.toContain('client_version=0.144.0')
    expect(codexGuide).not.toContain('client_version=0.144.4')
    expect(codexGuide).toContain('client_version=0.144.6')

    const guideConfigExamples = [...codexGuide.matchAll(/```toml\n([\s\S]*?)```/g)]
      .map(match => match[1])
      .filter(example => example.includes('model_provider = "gateway"'))

    expect(guideConfigExamples).toHaveLength(2)
    for (const example of guideConfigExamples) {
      expect(example).toContain('cli_auth_credentials_store = "file"')
      expect(example).toContain('multi_agent = true')
      expect(example).toContain('[agents]')
      expect(example).toContain('max_threads = 8')
      expect(example).toContain('max_depth = 3')
      expect(example).toContain('web_search = "disabled"')
      expect(example).toContain('stream_max_retries = 0')
      expect(example.indexOf('web_search = "disabled"')).toBeLessThan(
        example.indexOf('[features]')
      )
      expect(example).not.toContain('disable_response_storage')
    }
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
    expect(settings.env.ANTHROPIC_BASE_URL).toBe('https://portal.example.com')
    expect(settings.env.ANTHROPIC_API_KEY).toBe('sk-downstream-123')
    expect(settings.env.ANTHROPIC_AUTH_TOKEN).toBe('sk-downstream-123')
    expect(settings.env.CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY).toBe('1')
    expect(settings.env.ANTHROPIC_CUSTOM_MODEL_OPTION).toBe('MiniMax/MiniMax-M2.7')
    expect(settings.env.ANTHROPIC_CUSTOM_MODEL_OPTION_NAME).toBe('MiniMax/MiniMax-M2.7')
    expect(settings.env.ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION).toContain('portal')
  })

  it('builds an openai-compatible config for Cline and other generic clients', () => {
    const config = JSON.parse(
      buildOpenAiCompatibleConfig({
        gatewayBaseUrl: 'https://portal.example.com',
        portalKey: 'sk-downstream-123',
        modelSlugs: ['MiniMax/MiniMax-M2.7', 'DeepSeek/DeepSeek-V3'],
        selectedModelSlug: 'MiniMax/MiniMax-M2.7'
      })
    )

    expect(config.baseURL).toBe('https://portal.example.com/v1')
    expect(config.apiKey).toBe('sk-downstream-123')
    expect(config.model).toBe('MiniMax/MiniMax-M2.7')
    expect(config.modelsEndpoint).toBe('https://portal.example.com/v1/models')
  })


  it('builds an anthropic-compatible config for generic Messages clients', () => {
    const config = JSON.parse(
      buildAnthropicCompatibleConfig({
        gatewayBaseUrl: 'https://portal.example.com',
        portalKey: 'sk-downstream-123',
        modelSlugs: ['claude-3-5-sonnet-20241022', 'MiniMax/MiniMax-M2.7'],
        selectedModelSlug: 'claude-3-5-sonnet-20241022'
      })
    )

    expect(config.baseURL).toBe('https://portal.example.com')
    expect(config.apiKey).toBe('sk-downstream-123')
    expect(config.model).toBe('claude-3-5-sonnet-20241022')
    expect(config.protocol).toBe('messages')
    expect(config.modelsEndpoint).toBe('https://portal.example.com/v1/models')
  })

  it('anthropic-compatible config still works when the selected model is not Claude-prefixed', () => {
    const config = JSON.parse(
      buildAnthropicCompatibleConfig({
        gatewayBaseUrl: 'https://portal.example.com',
        portalKey: 'sk-downstream-123',
        modelSlugs: ['MiniMax/MiniMax-M2.7'],
        selectedModelSlug: 'MiniMax/MiniMax-M2.7'
      })
    )

    expect(config.baseURL).toBe('https://portal.example.com')
    expect(config.apiKey).toBe('sk-downstream-123')
    expect(config.model).toBe('MiniMax/MiniMax-M2.7')
    expect(config.protocol).toBe('messages')
  })

})
