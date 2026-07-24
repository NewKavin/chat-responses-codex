import type { JsonValue, PortalModelStat } from '@/types'

type GatewayModelItem = {
  id?: unknown
}

type GatewayModelsResponse = {
  data?: GatewayModelItem[]
}

export interface CodexCatalogResponse {
  models: Array<{ [key: string]: JsonValue }>
}

export type IntegrationCatalogViewState = {
  allModelSlugs: string[]
  primaryModelSlug: string
  primaryModelReasoningEffort: string
  sortedModelStats: PortalModelStat[]
  canGenerateConfigurationContent: boolean
}

type IntegrationCatalogViewStateInput = {
  catalog: CodexCatalogResponse | null
  modelAllowlist: string[]
  portalModelStats: PortalModelStat[]
}

export const isCodexCatalogResponse = (value: unknown): value is CodexCatalogResponse =>
  typeof value === 'object' &&
  value !== null &&
  !Array.isArray(value) &&
  'models' in value &&
  Array.isArray(value.models) &&
  value.models.every(model => typeof model === 'object' && model !== null && !Array.isArray(model))

type CodexConfigInput = {
  gatewayBaseUrl: string
  modelSlug: string
  modelReasoningEffort: string
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
const internalCodexCatalogFields = new Set([
  'gateway_catalog_witness',
  'upstream_id',
  'route_id',
  'runtime_model_slug',
  'profile_key',
  'configuration_id',
  'configuration_fingerprint',
  'key_fingerprint',
  'fingerprint'
])

const sanitizeCodexCatalogValue = (value: JsonValue): JsonValue => {
  if (Array.isArray(value)) {
    return value.map(sanitizeCodexCatalogValue)
  }
  if (typeof value !== 'object' || value === null) {
    return value
  }

  const sanitized: { [key: string]: JsonValue } = {}
  for (const [key, nestedValue] of Object.entries(value)) {
    if (!internalCodexCatalogFields.has(key)) {
      sanitized[key] = sanitizeCodexCatalogValue(nestedValue)
    }
  }
  return sanitized
}

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

const chooseCodexReasoningEffort = (
  catalog: CodexCatalogResponse,
  modelSlug: string
) => {
  const model = catalog.models.find(item => normalizeSlug(item.slug) === modelSlug)
  const effort = normalizeSlug(model?.default_reasoning_level)
  return effort || 'none'
}

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

const emptyIntegrationCatalogViewState = (): IntegrationCatalogViewState => ({
  allModelSlugs: [],
  primaryModelSlug: '',
  primaryModelReasoningEffort: 'none',
  sortedModelStats: [],
  canGenerateConfigurationContent: false
})

export const buildIntegrationCatalogViewState = ({
  catalog,
  modelAllowlist,
  portalModelStats
}: IntegrationCatalogViewStateInput): IntegrationCatalogViewState => {
  if (!catalog || catalog.models.length === 0) {
    return emptyIntegrationCatalogViewState()
  }

  const rankedLiveModelSlugs = rankModelSlugsByUsage(
    catalog.models.map(model => normalizeSlug(model.slug)),
    portalModelStats
  )
  const allowedModelSlugs = new Set(
    modelAllowlist.map(normalizeModelMatchKey).filter(Boolean)
  )
  const allModelSlugs = allowedModelSlugs.size
    ? rankedLiveModelSlugs.filter(slug => allowedModelSlugs.has(normalizeModelMatchKey(slug)))
    : rankedLiveModelSlugs

  if (allModelSlugs.length === 0) {
    return emptyIntegrationCatalogViewState()
  }

  const primaryModelSlug = allModelSlugs[0]
  return {
    allModelSlugs,
    primaryModelSlug,
    primaryModelReasoningEffort: chooseCodexReasoningEffort(catalog, primaryModelSlug),
    sortedModelStats: buildModelUsageStats(allModelSlugs, portalModelStats),
    canGenerateConfigurationContent: true
  }
}

export const buildCodexModelCatalogJson = (catalog?: CodexCatalogResponse) => {
  if (!catalog || !Array.isArray(catalog.models)) {
    throw new Error('live Codex catalog is unavailable')
  }
  if (catalog.models.length === 0) {
    throw new Error('live Codex catalog is empty')
  }
  const sanitizedCatalog = {
    models: catalog.models.map(model => sanitizeCodexCatalogValue(model))
  }
  return `${jsonStringify(sanitizedCatalog)}\n`
}

export const buildCodexConfigToml = (input: CodexConfigInput) => {
  const gatewayApiBaseUrl = `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1`
  const modelReasoningEffort = normalizeSlug(input.modelReasoningEffort) || 'none'

  return `model_provider = "gateway"
model = ${tomlString(input.modelSlug)}
review_model = ${tomlString(input.modelSlug)}
model_reasoning_effort = ${tomlString(modelReasoningEffort)}
model_catalog_json = "model-catalog.json"
cli_auth_credentials_store = "file"
web_search = "disabled"

[features]
skill_mcp_dependency_install = true
tool_suggest = true
multi_agent = true

[agents]
max_threads = 8
max_depth = 3

[model_providers.gateway]
name = "Chat Responses Gateway"
base_url = ${tomlString(gatewayApiBaseUrl)}
wire_api = "responses"
requires_openai_auth = true
stream_max_retries = 8
`
}

export const buildCodexDoctorCommand = () => 'codex --strict-config doctor --summary'

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
    permission: { '*': 'deny', read: 'allow' },
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
    CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY: '1',
    ANTHROPIC_DEFAULT_OPUS_MODEL: primaryModelSlug,
    ANTHROPIC_DEFAULT_SONNET_MODEL: primaryModelSlug,
    ANTHROPIC_DEFAULT_HAIKU_MODEL: primaryModelSlug,
    ANTHROPIC_CUSTOM_MODEL_OPTION: primaryModelSlug,
    ANTHROPIC_CUSTOM_MODEL_OPTION_NAME: primaryModelSlug,
    ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION: 'Current portal gateway model'
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
