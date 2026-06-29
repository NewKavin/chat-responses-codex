import type { PortalModelStat } from '@/types'

type GatewayModelItem = {
  id?: unknown
}

type GatewayModelsResponse = {
  data?: GatewayModelItem[]
}

type CodexConfigInput = {
  gatewayBaseUrl: string
  modelSlug: string
}

type HermesConfigInput = {
  gatewayBaseUrl: string
  portalKey: string
  modelSlug: string
}

type IntegrationConfigInput = {
  gatewayBaseUrl: string
  portalKey: string
  modelSlugs: string[]
  selectedModelSlug?: string
}

const gatewayProviderId = 'gateway'
const jsonStringify = (value: unknown) => JSON.stringify(value, null, 2)

const normalizeSlug = (value: unknown) => {
  if (typeof value !== 'string') return ''
  return value.trim()
}

const normalizeModelMatchKey = (value: unknown) => {
  const slug = normalizeSlug(value)
  return slug ? slug.toLowerCase() : ''
}

const tomlEscape = (value: string) => value.replace(/\\/g, '\\\\').replace(/"/g, '\\"')

const tomlString = (value: string) => `"${tomlEscape(value)}"`

const shellSingleQuote = (value: string) => `'${value.replace(/'/g, `'\\''`)}'`

const comparePortalModelStats = (left: PortalModelStat, right: PortalModelStat) => {
  if (left.month_count !== right.month_count) {
    return right.month_count - left.month_count
  }

  if (left.today_count !== right.today_count) {
    return right.today_count - left.today_count
  }

  if (left.month_tokens !== right.month_tokens) {
    return right.month_tokens - left.month_tokens
  }

  if (left.today_tokens !== right.today_tokens) {
    return right.today_tokens - left.today_tokens
  }

  if (left.avg_latency_ms !== right.avg_latency_ms) {
    return left.avg_latency_ms - right.avg_latency_ms
  }

  return left.model.localeCompare(right.model)
}

const choosePrimaryModelSlug = (modelSlugs: string[], preferredModelSlug?: string) => {
  const preferred = normalizeSlug(preferredModelSlug)
  if (preferred && modelSlugs.includes(preferred)) {
    return preferred
  }

  return modelSlugs[0] ?? ''
}

const chooseSecondaryModelSlug = (modelSlugs: string[], primaryModelSlug: string) =>
  modelSlugs.find(slug => slug !== primaryModelSlug) ?? primaryModelSlug

const isClaudeCompatibleSlug = (modelSlug: string) => /^(claude|anthropic)([-/]|$)/i.test(modelSlug)

export const buildGatewayBaseUrl = (origin: string) => origin.replace(/\/+$/, '')

export const buildGatewayModelsEndpoint = (gatewayBaseUrl: string) =>
  `${buildGatewayBaseUrl(gatewayBaseUrl)}/v1/models`

export const extractGatewayModelSlugs = (response: GatewayModelsResponse) => {
  const seen = new Set<string>()
  const models: string[] = []

  for (const item of response.data ?? []) {
    const id = normalizeSlug(item?.id)
    if (!id || seen.has(id)) continue
    seen.add(id)
    models.push(id)
  }

  return models
}

export const sortPortalModelStats = (stats: PortalModelStat[]) =>
  [...stats].sort(comparePortalModelStats)

const buildStatsByModel = (stats: PortalModelStat[]) => {
  const statsByModel = new Map<string, PortalModelStat>()

  for (const stat of sortPortalModelStats(stats)) {
    const key = normalizeModelMatchKey(stat.model)
    if (!key || statsByModel.has(key)) continue
    statsByModel.set(key, stat)
  }

  return statsByModel
}

export const rankModelSlugsByUsage = (modelSlugs: string[], stats: PortalModelStat[]) => {
  const uniqueSlugs: string[] = []
  const seen = new Set<string>()
  const statsByModel = buildStatsByModel(stats)

  for (const slug of modelSlugs) {
    const normalized = normalizeSlug(slug)
    if (!normalized || seen.has(normalized)) continue
    seen.add(normalized)
    uniqueSlugs.push(normalized)
  }

  return uniqueSlugs.sort((leftSlug, rightSlug) => {
    const left = statsByModel.get(normalizeModelMatchKey(leftSlug))
    const right = statsByModel.get(normalizeModelMatchKey(rightSlug))

    if (left && right) {
      return comparePortalModelStats(left, right)
    }

    if (left) return -1
    if (right) return 1
    return 0
  })
}

const createEmptyPortalModelStat = (model: string): PortalModelStat => ({
  model,
  today_count: 0,
  month_count: 0,
  today_tokens: 0,
  month_tokens: 0,
  avg_latency_ms: 0,
  success_rate: 0
})

export const buildModelUsageStats = (modelSlugs: string[], stats: PortalModelStat[]) => {
  if (!modelSlugs.length) {
    return sortPortalModelStats(stats)
  }

  const rankedModelSlugs = rankModelSlugsByUsage(modelSlugs, stats)
  const statsByModel = buildStatsByModel(stats)

  return rankedModelSlugs.map(slug => {
    const matchedStat = statsByModel.get(normalizeModelMatchKey(slug))
    return matchedStat ? { ...matchedStat, model: slug } : createEmptyPortalModelStat(slug)
  })
}

export interface CodexModelContextEntry {
  context_window: number
  /// Currently unused when emitting the catalog: Codex automatically derives
  /// the auto-compaction threshold as 90% of `context_window`, which lines up
  /// with the upstream admin's `output_reserve` default (10%). We still accept
  /// `output_reserve` so callers can pass the full upstream config without
  /// massaging it; in the future we may emit `auto_compact_token_limit`
  /// when the operator configured a non-default reserve.
  output_reserve?: number
}

export const buildCodexModelCatalogJson = (
  modelSlugs: string[],
  contexts?: Record<string, CodexModelContextEntry>
) => {
  const catalog = {
    models: modelSlugs.map((slug, index) => {
      const entry: Record<string, unknown> = {
        slug,
        display_name: slug,
        default_reasoning_level: 'high',
        supported_reasoning_levels: [
          {
            effort: 'low',
            description: 'Fast responses with lighter reasoning'
          },
          {
            effort: 'medium',
            description: 'Balances speed and reasoning depth for everyday tasks'
          },
          {
            effort: 'high',
            description: 'Greater reasoning depth for complex problems'
          },
          {
            effort: 'xhigh',
            description: 'Extra high reasoning depth for complex problems'
          }
        ],
        shell_type: 'shell_command',
        visibility: 'list',
        supported_in_api: true,
        priority: index,
        base_instructions: 'You are Codex, a coding agent.',
        supports_reasoning_summaries: true,
        support_verbosity: false,
        truncation_policy: {
          mode: 'tokens',
          limit: 10000
        },
        supports_parallel_tool_calls: true,
        experimental_supported_tools: [],
        input_modalities: ['text'],
        supports_search_tool: false
      }

      const ctx = contexts?.[slug]
      if (ctx) {
        const window = Number(ctx.context_window)
        if (Number.isFinite(window) && window > 0) {
          entry.context_window = Math.floor(window)
        }
      }

      return entry
    })
  }

  return `${jsonStringify(catalog)}\n`
}

export const buildCodexConfigToml = (input: CodexConfigInput) => {
  const gatewayApiBaseUrl = `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1`

  return `model_provider = "gateway"
model = ${tomlString(input.modelSlug)}
review_model = ${tomlString(input.modelSlug)}
model_reasoning_effort = "high"
disable_response_storage = true
model_catalog_json = "model-catalog.json"
cli_auth_credentials_store = "file"

[features]
skill_mcp_dependency_install = true
tool_suggest = true

[model_providers.gateway]
name = "Chat Responses Gateway"
base_url = ${tomlString(gatewayApiBaseUrl)}
wire_api = "responses"
requires_openai_auth = true
`
}

export const buildCodexAuthLoginCommand = (portalKey: string) =>
  `printf '%s' ${shellSingleQuote(portalKey)} | codex login --with-api-key`

export const buildHermesConfigYaml = (input: HermesConfigInput) => {
  const gatewayApiBaseUrl = `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1`
  const modelSlug = input.modelSlug || ''
  const portalKey = input.portalKey || '<你的下游key>'

  return `# ~/.hermes/config.yaml
# Hermes 0.17+ 使用 model dict 作为主配置。
model:
  provider: custom
  default: ${JSON.stringify(modelSlug)}
  base_url: ${JSON.stringify(gatewayApiBaseUrl)}
  api_key: "\${CHAT2RESPONSES_KEY}"

max_turns: 90

# 项目根目录 .hermes.env:
# CHAT2RESPONSES_KEY=${portalKey}
`
}

export const buildOpenCodeConfig = (input: IntegrationConfigInput) => {
  const primaryModelSlug = choosePrimaryModelSlug(input.modelSlugs, input.selectedModelSlug)
  const secondaryModelSlug = chooseSecondaryModelSlug(input.modelSlugs, primaryModelSlug)
  const modelEntries = Object.fromEntries(
    input.modelSlugs.map(slug => [
      slug,
      {
        name: slug
      }
    ])
  )

  const config = {
    $schema: 'https://opencode.ai/config.json',
    model: primaryModelSlug ? `${gatewayProviderId}/${primaryModelSlug}` : '',
    small_model: secondaryModelSlug ? `${gatewayProviderId}/${secondaryModelSlug}` : '',
    provider: {
      [gatewayProviderId]: {
        npm: '@ai-sdk/openai-compatible',
        name: 'Chat Responses Gateway',
        options: {
          baseURL: `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1`,
          apiKey: input.portalKey
        },
        models: modelEntries
      }
    }
  }

  return `${jsonStringify(config)}\n`
}

export const buildClaudeCodeSettingsJson = (input: IntegrationConfigInput) => {
  const primaryModelSlug = choosePrimaryModelSlug(input.modelSlugs, input.selectedModelSlug)
  const gatewayBaseUrl = buildGatewayBaseUrl(input.gatewayBaseUrl)
  const env: Record<string, string> = {
    ANTHROPIC_BASE_URL: gatewayBaseUrl,
    ANTHROPIC_API_KEY: input.portalKey,
    ANTHROPIC_AUTH_TOKEN: input.portalKey,
    CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY: '1'
  }

  if (primaryModelSlug && !isClaudeCompatibleSlug(primaryModelSlug)) {
    env.ANTHROPIC_CUSTOM_MODEL_OPTION = primaryModelSlug
    env.ANTHROPIC_CUSTOM_MODEL_OPTION_NAME = primaryModelSlug
    env.ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION = 'Current portal gateway model'
  }

  return `${jsonStringify({
    model: primaryModelSlug,
    env
  })}\n`
}

export const buildOpenAiCompatibleConfig = (input: IntegrationConfigInput) => {
  const primaryModelSlug = choosePrimaryModelSlug(input.modelSlugs, input.selectedModelSlug)

  return `${jsonStringify({
    baseURL: `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1`,
    apiKey: input.portalKey,
    model: primaryModelSlug,
    modelsEndpoint: `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1/models`
  })}
`
}

export const buildAnthropicCompatibleConfig = (input: IntegrationConfigInput) => {
  const primaryModelSlug = choosePrimaryModelSlug(input.modelSlugs, input.selectedModelSlug)
  const gatewayBaseUrl = buildGatewayBaseUrl(input.gatewayBaseUrl)

  return `${jsonStringify({
    baseURL: gatewayBaseUrl,
    apiKey: input.portalKey,
    model: primaryModelSlug,
    protocol: 'messages',
    modelsEndpoint: `${gatewayBaseUrl}/v1/models`
  })}
`
}
