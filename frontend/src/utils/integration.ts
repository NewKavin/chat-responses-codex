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

export const rankModelSlugsByUsage = (modelSlugs: string[], stats: PortalModelStat[]) => {
  const uniqueSlugs: string[] = []
  const seen = new Set<string>()

  for (const slug of [...modelSlugs, ...stats.map(stat => stat.model)]) {
    const normalized = normalizeSlug(slug)
    if (!normalized || seen.has(normalized)) continue
    seen.add(normalized)
    uniqueSlugs.push(normalized)
  }

  const statsByModel = new Map(stats.map(stat => [normalizeSlug(stat.model), stat]))

  return uniqueSlugs.sort((leftSlug, rightSlug) => {
    const left = statsByModel.get(leftSlug)
    const right = statsByModel.get(rightSlug)

    if (left && right) {
      return comparePortalModelStats(left, right)
    }

    if (left) return -1
    if (right) return 1
    return 0
  })
}

export const buildCodexModelCatalogJson = (modelSlugs: string[]) => {
  const catalog = {
    models: modelSlugs.map((slug, index) => ({
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
      supports_reasoning_summaries: false,
      support_verbosity: false,
      truncation_policy: {
        mode: 'tokens',
        limit: 10000
      },
      supports_parallel_tool_calls: true,
      experimental_supported_tools: [],
      input_modalities: ['text'],
      supports_search_tool: false
    }))
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
  const env: Record<string, string> = {
    ANTHROPIC_BASE_URL: `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1`,
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
