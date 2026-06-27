<template>
  <div class="integration-page">
    <el-card class="integration-hero">
      <div class="hero-top">
        <div>
          <p class="eyebrow">复制即用</p>
          <h2>门户集成配置</h2>
          <p class="hero-copy">
            本页会自动读取当前下游的 key、当前网关 URL 和 live
            `/v1/models`，生成可以直接复制到本地的配置文件。
          </p>
        </div>
        <el-tag type="success" effect="light">
          {{ primaryModelSlug || '未发现模型' }}
        </el-tag>
      </div>

      <el-descriptions class="summary-grid" :column="2" border>
        <el-descriptions-item label="网关地址">
          <code>{{ gatewayApiBaseUrl || '等待加载' }}</code>
        </el-descriptions-item>
        <el-descriptions-item label="当前 key">
          <code>{{ portalKey || '未读取到 key' }}</code>
        </el-descriptions-item>
        <el-descriptions-item label="可用模型数">
          <code>{{ allModelSlugs.length }}</code>
        </el-descriptions-item>
        <el-descriptions-item label="默认模型">
          <code>{{ primaryModelSlug || '未发现模型' }}</code>
        </el-descriptions-item>
      </el-descriptions>

      <el-alert
        class="status-alert"
        type="info"
        :closable="false"
        show-icon
        title="Codex 不把 key 写进 config.toml，而是通过 `codex login --with-api-key` 写进 `~/.codex/auth.json`。"
      />

      <el-alert
        class="status-alert"
        type="info"
        :closable="false"
        show-icon
        title="模型名直接使用上游 `/v1/models` 返回的原始 slug；如果多个上游账号暴露同一个模型，网关会自动按压力分摊，不需要手工转成小写或拆成别名。"
      />

      <el-alert
        v-for="warning in loadWarnings"
        :key="warning"
        class="status-alert"
        type="warning"
        :closable="false"
        show-icon
        :title="warning"
      />

      <el-alert
        v-if="fatalError"
        class="status-alert"
        type="error"
        :closable="false"
        show-icon
        :title="fatalError"
      />
    </el-card>


    <el-card class="compat-matrix-card">
      <template #header>
        <div class="section-head">
          <div>
            <h3>客户端兼容矩阵</h3>
            <p>按协议族分组，每个客户端只需要一个配置即可连接网关。</p>
          </div>
        </div>
      </template>

      <div class="compat-grid">
        <div class="compat-family">
          <h4>Codex</h4>
          <span class="compat-protocol">Responses 协议</span>
          <code>{{ gatewayApiBaseUrl }}/responses</code>
          <span class="compat-auth">codex login --with-api-key</span>
          <span class="compat-models">model-catalog.json + /v1/models</span>
        </div>

        <div class="compat-family">
          <h4>OpenAI 兼容客户端</h4>
          <span class="compat-protocol">Chat Completions 协议</span>
          <code>{{ gatewayApiBaseUrl }}/chat/completions</code>
          <span class="compat-auth">Bearer 下游 Key</span>
          <span class="compat-models">/v1/models</span>
          <div class="compat-clients">
            <el-tag size="small" effect="plain">Cline</el-tag>
            <el-tag size="small" effect="plain">OpenCode</el-tag>
            <el-tag size="small" effect="plain">其他兼容工具</el-tag>
          </div>
        </div>

        <div class="compat-family">
          <h4>Anthropic 兼容客户端</h4>
          <span class="compat-protocol">Messages 协议</span>
          <code>{{ gatewayApiBaseUrl }}/messages</code>
          <span class="compat-auth">Bearer 下游 Key</span>
          <span class="compat-models">/v1/models</span>
          <span class="compat-adapter">网关内部转成 Chat Completions 再发给上游</span>
          <div class="compat-clients">
            <el-tag size="small" effect="plain">Claude Code</el-tag>
            <el-tag size="small" effect="plain">其他兼容工具</el-tag>
          </div>
        </div>
      </div>

      <el-alert
        class="status-alert"
        type="info"
        :closable="false"
        show-icon
        title="网关同时暴露 `/v1/chat/completions`、`/v1/responses`、`/v1/models`、`/v1/messages` 和 `/v1/messages/count_tokens`。上游只支持 Chat Completions 和 Responses 两种协议；`/v1/messages` 是网关的适配层，收到 Anthropic 格式请求后内部转成 Chat Completions 再发给上游。客户端只需要根据自己支持的协议族选对应的 endpoint 和 preset。"
      />
    </el-card>

    <el-card v-if="sortedModelStats.length" class="model-card">
      <template #header>
        <div class="section-head">
          <div>
            <h3>模型排序</h3>
            <p>按月使用量优先，月使用量相同再看今日使用量。</p>
          </div>
          <el-tag effect="plain">{{ sortedModelStats.length }} 个统计项</el-tag>
        </div>
      </template>

      <div class="model-grid">
        <div v-for="stat in sortedModelStats" :key="stat.model" class="model-chip">
          <strong>{{ stat.model }}</strong>
          <span>
            月 {{ stat.month_count }} · 今 {{ stat.today_count }} · 成功率
            {{ Math.round(stat.success_rate * 100) }}%
          </span>
        </div>
      </div>
    </el-card>

    <el-empty
      v-if="!hasConfigContent"
      class="integration-empty"
      description="当前还不能生成可直接复制的配置"
    >
      <p class="empty-copy">
        请先确认当前下游 key 可读，并且网关至少能返回一个模型。然后刷新本页即可生成完整配置。
      </p>
    </el-empty>

    <el-card v-else class="tabs-card">
      <el-tabs v-model="activeTab" class="integration-tabs">
        <el-tab-pane label="Codex" name="codex">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="Codex 需要 3 个内容：`config.toml`、`model-catalog.json`，以及通过 `codex login --with-api-key` 写入的 `auth.json`。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 写入 `~/.codex/config.toml`</h4>
                  <p>
                    这个文件只放非敏感配置，key 不写在这里。直接把下面内容保存到
                    <code>~/.codex/config.toml</code>。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(codexConfigToml)">复制</el-button>
              </div>
              <pre class="code-block">{{ codexConfigToml }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 2: 写入 `~/.codex/model-catalog.json`</h4>
                  <p>
                    这个文件按门户统计排序，优先展示最近一个月最常用的模型。每个模型的
                    <code>context_window</code> 字段直接取自网关上游配置，
                    Codex 默认会在累计 token 达到该窗口的
                    <strong>90%</strong> 时自动压缩历史，无需在 <code>config.toml</code>
                    再设全局阈值；切换模型时压缩点会跟着模型的实际窗口变。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(codexModelCatalogJson)">复制</el-button>
              </div>
              <pre class="code-block">{{ codexModelCatalogJson }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 3: 写入 `~/.codex/auth.json`</h4>
                  <p>
                    先确保 <code>config.toml</code> 里已经设置了
                    <code>cli_auth_credentials_store = "file"</code>，然后执行下面命令把当前门户
                    key 写进 <code>auth.json</code>。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(codexAuthLoginCommand)">复制</el-button>
              </div>
              <pre class="code-block">{{ codexAuthLoginCommand }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="完成后直接启动 Codex。默认模型已经按门户统计排好顺序，Codex 会优先使用最常用的模型。"
            />
          </div>
        </el-tab-pane>

        <el-tab-pane label="OpenCode" name="opencode">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="OpenCode 直接使用一个完整的 `opencode.json`。把当前门户 key 和网关 URL 写进去后，保存即可使用。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 写入 `~/.config/opencode/opencode.json`</h4>
                  <p>
                    这个 JSON 文件已经填好当前 key、网关 URL 和模型列表，复制后可直接使用。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(opencodeConfig)">复制</el-button>
              </div>
              <pre class="code-block">{{ opencodeConfig }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="保存后重新打开 OpenCode 即可。模型名、网关 URL 和 key 都已经按当前门户的最新值写好了。"
            />
          </div>
        </el-tab-pane>
        <el-tab-pane label="Cline / OpenAI 兼容" name="cline">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="Cline 和其他 OpenAI 兼容客户端共用同一个配置格式：只需要 `baseURL`、`apiKey` 和默认模型。模型列表来自网关 `/v1/models`，不需要手工维护。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 配置 Cline 或其他 OpenAI 兼容客户端</h4>
                  <p>
                    复制下面的 JSON，在客户端里填入 <code>Base URL</code>、<code>API Key</code>
                    和 <code>Model</code> 即可。模型列表可以从网关的
                    <code>/v1/models</code> 实时获取。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(openAiCompatibleConfig)">复制</el-button>
              </div>
              <pre class="code-block">{{ openAiCompatibleConfig }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="保存后重新打开客户端即可。模型名、网关 URL 和 key 都已经按当前门户的最新值写好了。"
            />
          </div>
        </el-tab-pane>


        <el-tab-pane label="Anthropic / Messages 兼容" name="anthropic">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="所有支持 Anthropic Messages 协议的客户端共用同一个配置格式：只需要 `baseURL`、`apiKey` 和默认模型。网关同时暴露 `/v1/messages` 和 `/v1/messages/count_tokens`，客户端把 baseURL 指向网关根地址即可，SDK 会自己拼接 `/v1/messages`。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 配置 Anthropic 兼容客户端</h4>
                  <p>
                    复制下面的 JSON，在客户端里填入 <code>Base URL</code>、<code>API Key</code>
                    和 <code>Model</code> 即可。请求发到网关的
                    <code>/v1/messages</code>，模型列表可以从
                    <code>/v1/models</code> 实时获取。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(anthropicCompatibleConfig)">复制</el-button>
              </div>
              <pre class="code-block">{{ anthropicCompatibleConfig }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="保存后重新打开客户端即可。模型名、网关 URL 和 key 都已经按当前门户的最新值写好了。"
            />
          </div>
        </el-tab-pane>
        <el-tab-pane label="Claude Code" name="claude">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="Claude Code 直接使用 `~/.claude/settings.json`。当前 key、网关根地址和默认模型都会写进去；Claude Code 会自己拼接 `/v1/messages`。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 写入 `~/.claude/settings.json`</h4>
                  <p>
                    这个配置已经写好了 Anthropic 兼容网关所需的环境变量；如果模型名不是 Claude
                    前缀，页面会自动补一个 custom model option。`ANTHROPIC_BASE_URL` 填的是网关根地址，
                    不要再手工加 `/v1`。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(claudeCodeSettingsJson)">复制</el-button>
              </div>
              <pre class="code-block">{{ claudeCodeSettingsJson }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="保存后重启 Claude Code 即可。默认模型会跟随门户使用量最高的模型，非 Claude 前缀模型会自动补 custom model option。"
            />
          </div>
        </el-tab-pane>

        <el-tab-pane label="Hermes Agent" name="hermes">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="Hermes Agent 是自我进化的 AI agent。通过 bun 安装 npm 桥接器,再用 venv 装 Python 运行时,配置 provider 指向网关即可。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 安装 npm 桥接器</h4>
                  <p>在项目根目录用 bun 安装 <code>hermes-agent</code> 桥接包。</p>
                </div>
                <el-button size="small" @click="copyCode(hermesInstallNpm)">复制</el-button>
              </div>
              <pre class="code-block">{{ hermesInstallNpm }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 2: 安装 Python 运行时</h4>
                  <p>hermes-agent 是 Python 包的桥接器,需 Python 3.11+。受 PEP 668 限制,用 venv 安装。</p>
                </div>
                <el-button size="small" @click="copyCode(hermesInstallPython)">复制</el-button>
              </div>
              <pre class="code-block">{{ hermesInstallPython }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 3: 写入 ~/.hermes/config.yaml</h4>
                  <p>把下游 key 填入项目根目录 <code>.hermes.env</code> 的 <code>CHAT2RESPONSES_KEY</code>,然后保存配置文件。</p>
                </div>
                <el-button size="small" @click="copyCode(hermesConfigYaml)">复制</el-button>
              </div>
              <pre class="code-block">{{ hermesConfigYaml }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 4: 启动</h4>
                  <p>项目提供了启动脚本,自动加载 <code>.hermes.env</code> 和 venv。</p>
                </div>
                <el-button size="small" @click="copyCode(hermesLaunch)">复制</el-button>
              </div>
              <pre class="code-block">{{ hermesLaunch }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="完成后即可用 hermes chat 与模型对话,所有请求经网关路由,门户日志可见。"
            />
          </div>
        </el-tab-pane>
      </el-tabs>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'
import type { ModelContextEntry, PortalModelStat } from '@/types'
import {
  buildClaudeCodeSettingsJson,
  buildCodexAuthLoginCommand,
  buildCodexConfigToml,
  buildCodexModelCatalogJson,
  buildModelUsageStats,
  buildGatewayBaseUrl,
  buildGatewayModelsEndpoint,
  buildAnthropicCompatibleConfig,
  buildOpenAiCompatibleConfig,
  buildOpenCodeConfig,
  extractGatewayModelSlugs,
  rankModelSlugsByUsage,
  sortPortalModelStats
} from '@/utils/integration'
import { resolvePortalQuotaModelSlugs } from '@/utils/portalQuotaModels'

type GatewayModelsResponse = {
  data?: Array<{
    id?: unknown
  }>
}

const activeTab = ref('codex')
const loading = ref(true)
const gatewayBaseUrl = ref('')
const portalKey = ref('')
const portalModelStats = ref<PortalModelStat[]>([])
const modelAllowlist = ref<string[]>([])
const gatewayModelSlugs = ref<string[]>([])
const modelContexts = ref<Record<string, ModelContextEntry>>({})
const loadWarnings = ref<string[]>([])
const fatalError = ref('')

const gatewayApiBaseUrl = computed(() =>
  gatewayBaseUrl.value ? `${gatewayBaseUrl.value}/v1` : ''
)

const allModelSlugs = computed(() => {
  const unfiltered = gatewayModelSlugs.value.length
    ? rankModelSlugsByUsage(gatewayModelSlugs.value, portalModelStats.value)
    : sortPortalModelStats(portalModelStats.value).map(stat => stat.model)
  return resolvePortalQuotaModelSlugs(modelAllowlist.value, unfiltered)
})


const primaryModelSlug = computed(() => allModelSlugs.value[0] ?? '')

const hermesInstallNpm = computed(() => `# 在项目根目录
bun install`)

const hermesInstallPython = computed(() => `# Python 3.11+ 需求,用 venv 隔离
python3 -m venv .hermes-venv
.hermes-venv/bin/python -m pip install --upgrade pip
.hermes-venv/bin/pip install hermes-agent`)

const hermesConfigYaml = computed(() => {
  const base = gatewayBaseUrl.value || 'http://127.0.0.1:3000'
  const model = primaryModelSlug.value || 'gpt-4.1-mini'
  return `# ~/.hermes/config.yaml
model: ${model}
max_turns: 90

providers:
  chat2responses:
    name: chat2responses
    base_url: ${base}
    api_mode: openai
    key_env: CHAT2RESPONSES_KEY
    discover_models: true
    default_model: ${model}

# 项目根目录 .hermes.env:
# CHAT2RESPONSES_KEY=${portalKey.value || '<你的下游key>'}`
})

const hermesLaunch = computed(() => {
  const model = primaryModelSlug.value || 'gpt-4.1-mini'
  return `# 项目根目录执行
./scripts/hermes.sh chat

# 指定模型
./scripts/hermes.sh -m ${model} chat`
})
const sortedModelStats = computed(() => {
  const stats = buildModelUsageStats(gatewayModelSlugs.value, portalModelStats.value)
  if (!modelAllowlist.value.length) return stats
  const allowed = new Set(modelAllowlist.value.map(s => s.trim()).filter(Boolean))
  return stats.filter(stat => allowed.has(stat.model))
})

const hasConfigContent = computed(
  () => Boolean(portalKey.value.trim()) && allModelSlugs.value.length > 0 && !fatalError.value
)

const codexConfigToml = computed(() =>
  primaryModelSlug.value
    ? buildCodexConfigToml({
        gatewayBaseUrl: gatewayBaseUrl.value,
        modelSlug: primaryModelSlug.value
      })
    : ''
)

const codexModelCatalogJson = computed(() => buildCodexModelCatalogJson(allModelSlugs.value, modelContexts.value))

const codexAuthLoginCommand = computed(() =>
  portalKey.value ? buildCodexAuthLoginCommand(portalKey.value) : ''
)

const opencodeConfig = computed(() =>
  buildOpenCodeConfig({
    gatewayBaseUrl: gatewayBaseUrl.value,
    portalKey: portalKey.value,
    modelSlugs: allModelSlugs.value,
    selectedModelSlug: primaryModelSlug.value
  })
)

const claudeCodeSettingsJson = computed(() =>
  buildClaudeCodeSettingsJson({
    gatewayBaseUrl: gatewayBaseUrl.value,
    portalKey: portalKey.value,
    modelSlugs: allModelSlugs.value,
    selectedModelSlug: primaryModelSlug.value
  })
)

const anthropicCompatibleConfig = computed(() =>
  buildAnthropicCompatibleConfig({
    gatewayBaseUrl: gatewayBaseUrl.value,
    portalKey: portalKey.value,
    modelSlugs: allModelSlugs.value,
    selectedModelSlug: primaryModelSlug.value
  })
)

const openAiCompatibleConfig = computed(() =>
  buildOpenAiCompatibleConfig({
    gatewayBaseUrl: gatewayBaseUrl.value,
    portalKey: portalKey.value,
    modelSlugs: allModelSlugs.value,
    selectedModelSlug: primaryModelSlug.value
  })
)

const fetchGatewayModelSlugs = async (key: string) => {
  const response = await fetch(buildGatewayModelsEndpoint(gatewayBaseUrl.value), {
    headers: {
      Authorization: `Bearer ${key}`
    }
  })

  if (!response.ok) {
    throw new Error(`网关模型接口返回 ${response.status}`)
  }

  const payload = (await response.json()) as GatewayModelsResponse
  return extractGatewayModelSlugs(payload)
}

const loadIntegrationData = async () => {
  loading.value = true
  fatalError.value = ''
  loadWarnings.value = []
  portalKey.value = ''
  portalModelStats.value = []
  modelAllowlist.value = []
  gatewayModelSlugs.value = []
  modelContexts.value = {}

  try {
    gatewayBaseUrl.value = buildGatewayBaseUrl(window.location.origin)

    const [keyResult, modelsResult, quotaResult] = await Promise.allSettled([
      portalApi.getKey(),
      portalApi.getModels(),
      portalApi.getQuota()
    ])

    if (keyResult.status === 'rejected') {
      fatalError.value = '无法读取当前门户 key，无法生成可直接复制的配置。'
      return
    }

    portalKey.value = keyResult.value.data.plaintext_key?.trim() ?? ''
    if (!portalKey.value) {
      fatalError.value = '当前门户没有可用的下游 key，请先到“秘钥管理”生成或轮换一次。'
      return
    }

    if (quotaResult.status === 'fulfilled') {
      modelAllowlist.value = quotaResult.value.data.model_allowlist ?? []
      modelContexts.value = quotaResult.value.data.model_contexts ?? {}
    }

    if (modelsResult.status === 'fulfilled') {
      portalModelStats.value = modelsResult.value.data ?? []
    } else {
      loadWarnings.value.push('模型统计读取失败，将改为使用网关模型列表生成排序。')
    }

    try {
      gatewayModelSlugs.value = await fetchGatewayModelSlugs(portalKey.value)
    } catch (error) {
      gatewayModelSlugs.value = []
      const message = error instanceof Error ? error.message : '无法读取网关模型列表'
      loadWarnings.value.push(`${message}，将仅使用模型统计生成配置。`)
    }

    if (!allModelSlugs.value.length) {
      fatalError.value = '未能发现任何可用模型，请先在网关后台配置上游模型。'
    }
  } finally {
    loading.value = false
  }
}

const copyCode = async (content: string) => {
  const text = content.trim()
  if (!text) {
    ElMessage.warning('当前没有可复制的内容')
    return
  }

  try {
    await navigator.clipboard.writeText(content)
    ElMessage.success('已复制到剪贴板')
    return
  } catch {
    // Fall back to a hidden textarea for browsers that block Clipboard API.
  }

  const textarea = document.createElement('textarea')
  textarea.value = content
  textarea.setAttribute('readonly', 'true')
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  textarea.style.pointerEvents = 'none'
  document.body.appendChild(textarea)
  textarea.select()

  try {
    document.execCommand('copy')
    ElMessage.success('已复制到剪贴板')
  } catch {
    ElMessage.error('复制失败，请手动复制')
  } finally {
    document.body.removeChild(textarea)
  }
}

onMounted(() => {
  void loadIntegrationData()
})
</script>

<style scoped>
.integration-page {
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 20px;
  background:
    radial-gradient(circle at top right, rgba(64, 158, 255, 0.12), transparent 30%),
    linear-gradient(180deg, #f8fbff 0%, #f4f7fb 100%);
  min-height: 100%;
}

.integration-hero,
.model-card,
.tabs-card,
.integration-empty {
  border-radius: 16px;
  box-shadow: 0 12px 32px rgba(15, 23, 42, 0.06);
}

.hero-top {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 20px;
}

.eyebrow {
  margin: 0 0 8px;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: #409eff;
}

h2,
h3,
h4,
p {
  margin: 0;
}

.hero-copy {
  margin-top: 8px;
  color: #606266;
  line-height: 1.8;
  max-width: 760px;
}

.summary-grid {
  margin-top: 20px;
}

.status-alert {
  margin-top: 12px;
}

.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.section-head h3 {
  font-size: 16px;
  margin-bottom: 4px;
}

.section-head p {
  color: #8a8f98;
  font-size: 13px;
}

.model-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 12px;
}

.model-chip {
  padding: 14px 16px;
  border-radius: 12px;
  background: linear-gradient(180deg, #ffffff 0%, #f8fbff 100%);
  border: 1px solid #e6eef9;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.model-chip strong {
  font-size: 13px;
  line-height: 1.5;
  word-break: break-word;
}

.model-chip span {
  color: #606266;
  font-size: 12px;
}

.integration-empty {
  margin-top: 0;
  padding: 40px 20px;
}

.empty-copy {
  margin-top: 12px;
  color: #606266;
  line-height: 1.7;
}

.tabs-card {
  padding-top: 4px;
}

.tab-body {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.section-alert {
  border-radius: 12px;
}

.step-card {
  border: 1px solid #e6eef9;
  background: #fff;
  border-radius: 14px;
  padding: 16px;
}

.step-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.step-head h4 {
  margin-bottom: 8px;
  font-size: 15px;
  color: #1f2d3d;
}

.step-head p {
  color: #606266;
  line-height: 1.8;
}

.step-head code,
.summary-grid code {
  background: #f3f6fa;
  border: 1px solid #e3eaf3;
  padding: 2px 6px;
  border-radius: 6px;
  color: #1f2d3d;
}

.code-block {
  margin: 14px 0 0;
  padding: 16px;
  border-radius: 12px;
  background: #111827;
  color: #e5e7eb;
  overflow-x: auto;
  overflow-y: auto;
  max-height: 420px;
  white-space: pre;
  line-height: 1.7;
  font-size: 13px;
  font-family:
    'SFMono-Regular',
    'Consolas',
    'Liberation Mono',
    'Courier New',
    monospace;
}

.integration-tabs :deep(.el-tabs__header) {
  margin-bottom: 18px;
}

.integration-tabs :deep(.el-tabs__item) {
  font-size: 14px;
}

.integration-tabs :deep(.el-tabs__nav-wrap::after) {
  height: 1px;
}

@media (max-width: 768px) {
  .integration-page {
    padding: 12px;
    gap: 12px;
  }

  .hero-top,
  .step-head,
  .section-head {
    flex-direction: column;
    align-items: flex-start;
  }

  .summary-grid :deep(.el-descriptions__body) {
    display: block;
  }
}

.compat-matrix-card {
  border-radius: 16px;
  box-shadow: 0 12px 32px rgba(15, 23, 42, 0.06);
}

.compat-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 16px;
}

.compat-family {
  padding: 16px;
  border-radius: 12px;
  background: linear-gradient(180deg, #ffffff 0%, #f8fbff 100%);
  border: 1px solid #e6eef9;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.compat-family h4 {
  font-size: 15px;
  color: #1f2d3d;
  margin: 0;
}

.compat-protocol {
  font-size: 12px;
  color: #409eff;
  font-weight: 600;
}

.compat-family code {
  background: #f3f6fa;
  border: 1px solid #e3eaf3;
  padding: 2px 6px;
  border-radius: 6px;
  color: #1f2d3d;
  font-size: 12px;
}

.compat-auth,
.compat-models {
  font-size: 12px;
  color: #606266;
}

.compat-clients {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  margin-top: 4px;
}
</style>
