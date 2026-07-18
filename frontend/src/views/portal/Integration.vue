<template>
  <div class="crc-page integration-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">集成配置</h1>
        <p class="crc-page-description">自动读取当前下游 key、网关地址与实时模型目录，生成可直接使用的客户端配置。</p>
      </div>
      <div>
        <el-tag type="success" effect="light">
          {{ primaryModelSlug || '未发现模型' }}
        </el-tag>
      </div>
    </header>

    <section v-loading="loading" class="integration-summary">

      <el-descriptions class="summary-grid" :column="2" size="small" border>
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
    </section>

    <section class="integration-section">
      <div class="section-head">
        <div>
          <h2>客户端兼容矩阵</h2>
          <p>按协议族分组，每个客户端只需要一个配置即可连接网关。</p>
        </div>
      </div>

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
    </section>

    <section v-if="sortedModelStats.length" class="integration-section">
      <div class="section-head">
        <div>
          <h2>模型排序</h2>
          <p>按月使用量优先，月使用量相同再看今日使用量。</p>
        </div>
        <el-tag effect="plain">{{ sortedModelStats.length }} 个统计项</el-tag>
      </div>

      <div class="model-ranking">
        <div
          v-for="(stat, index) in sortedModelStats"
          :key="stat.model"
          class="model-ranking__item"
        >
          <span class="model-ranking__position">{{ index + 1 }}</span>
          <div class="model-ranking__identity">
            <strong>{{ stat.model }}</strong>
            <el-tag
              v-if="stat.model === primaryModelSlug"
              size="small"
              type="success"
              effect="plain"
            >
              默认
            </el-tag>
          </div>
          <span class="model-ranking__metrics">
            月 {{ stat.month_count }} · 今 {{ stat.today_count }} · 成功率
            {{ Math.round(stat.success_rate * 100) }}%
          </span>
        </div>
      </div>
    </section>

    <el-empty
      v-if="!hasConfigContent"
      data-testid="integration-empty"
      class="integration-empty"
      description="当前还不能生成可直接复制的配置"
    >
      <p class="empty-copy">
        请先确认当前下游 key 可读，并且网关至少能返回一个模型。然后刷新本页即可生成完整配置。
      </p>
    </el-empty>

    <section v-else data-testid="integration-config-tabs" class="code-surface">
      <div class="section-head config-section-head">
        <div>
          <h2>客户端配置</h2>
          <p>优先提供 Codex 与 OpenCode，其他客户端按协议兼容方式配置。</p>
        </div>
        <el-tag effect="plain">实时目录已同步</el-tag>
      </div>
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
            <p class="codex-agent-limits">
              <code>max_threads</code> 表示并发代理线程，<code>max_depth</code> 表示嵌套委派深度；这些本地限制不覆盖网关 quota。
            </p>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 写入 `~/.codex/config.toml`</h4>
                  <p>
                    这个文件只放非敏感配置，key 不写在这里。直接把下面内容保存到
                    <code>~/.codex/config.toml</code>。
                  </p>
                </div>
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(codexConfigToml)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
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
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(codexModelCatalogJson)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
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
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(codexAuthLoginCommand)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
              </div>
              <pre class="code-block">{{ codexAuthLoginCommand }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="完成后直接启动 Codex。默认模型已经按门户统计排好顺序，Codex 会优先使用最常用的模型。运行 codex --strict-config doctor --summary 可检查配置。"
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
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(opencodeConfig)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
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
                    这个配置已经写好了 Anthropic 兼容网关所需的环境变量；三套默认模型 alias 都会指向当前选中的网关模型。`ANTHROPIC_BASE_URL` 填的是网关根地址，
                    不要再手工加 `/v1`。
                  </p>
                </div>
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(claudeCodeSettingsJson)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
              </div>
              <pre class="code-block">{{ claudeCodeSettingsJson }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="保存后重启 Claude Code 即可。默认模型 alias 会统一映射到当前门户选择的模型。"
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
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(openAiCompatibleConfig)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
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
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(anthropicCompatibleConfig)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
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
        <el-tab-pane label="Hermes Agent" name="hermes">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="Hermes Agent 是自我进化的 AI agent。通过 bun 安装 npm 桥接器,再用 venv 装 Python 运行时,配置 model.provider=custom 和 model.base_url 指向网关即可。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 安装 npm 桥接器</h4>
                  <p>在项目根目录用 bun 安装 <code>hermes-agent</code> 桥接包。</p>
                </div>
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(hermesInstallNpm)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
              </div>
              <pre class="code-block">{{ hermesInstallNpm }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 2: 安装 Python 运行时</h4>
                  <p>hermes-agent 是 Python 包的桥接器,需 Python 3.11+。受 PEP 668 限制,用 venv 安装。</p>
                </div>
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(hermesInstallPython)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
              </div>
              <pre class="code-block">{{ hermesInstallPython }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 3: 写入 ~/.hermes/config.yaml</h4>
                  <p>把下游 key 填入项目根目录 <code>.hermes.env</code> 的 <code>CHAT2RESPONSES_KEY</code>,然后保存新的 <code>model</code> 配置。</p>
                </div>
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(hermesConfigYaml)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
              </div>
              <pre class="code-block">{{ hermesConfigYaml }}</pre>
            </div>

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 4: 启动</h4>
                  <p>项目提供了启动脚本,自动加载 <code>.hermes.env</code> 和 venv。</p>
                </div>
                <el-tooltip content="复制代码" placement="top">
                  <el-button aria-label="复制代码" circle size="small" @click="copyCode(hermesLaunch)">
                    <el-icon><CopyDocument /></el-icon>
                  </el-button>
                </el-tooltip>
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
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { CopyDocument } from '@element-plus/icons-vue'
import { portalApi } from '@/api/portal'
import type { ModelContextEntry, PortalModelStat } from '@/types'
import {
  buildClaudeCodeSettingsJson,
  type CodexCatalogResponse,
  buildCodexAuthLoginCommand,
  buildCodexConfigToml,
  buildCodexModelCatalogJson,
  buildIntegrationCatalogViewState,
  isCodexCatalogResponse,
  buildGatewayBaseUrl,
  buildGatewayModelsEndpoint,
  buildAnthropicCompatibleConfig,
  buildHermesConfigYaml,
  buildOpenAiCompatibleConfig,
  buildOpenCodeConfig
} from '@/utils/integration'

const activeTab = ref('codex')
const loading = ref(true)
const gatewayBaseUrl = ref('')
const portalKey = ref('')
const portalModelStats = ref<PortalModelStat[]>([])
const modelAllowlist = ref<string[]>([])
const codexCatalog = ref<CodexCatalogResponse | null>(null)
const modelContexts = ref<Record<string, ModelContextEntry>>({})
const loadWarnings = ref<string[]>([])
const fatalError = ref('')

const gatewayApiBaseUrl = computed(() =>
  gatewayBaseUrl.value ? `${gatewayBaseUrl.value}/v1` : ''
)

const catalogViewState = computed(() =>
  buildIntegrationCatalogViewState({
    catalog: codexCatalog.value,
    modelAllowlist: modelAllowlist.value,
    portalModelStats: portalModelStats.value
  })
)

const allModelSlugs = computed(() => catalogViewState.value.allModelSlugs)
const primaryModelSlug = computed(() => catalogViewState.value.primaryModelSlug)
const primaryModelReasoningEffort = computed(
  () => catalogViewState.value.primaryModelReasoningEffort
)

const hermesInstallNpm = computed(() => `# 在项目根目录
bun install`)

const hermesInstallPython = computed(() => `# Python 3.11+ 需求,用 venv 隔离
python3 -m venv .hermes-venv
.hermes-venv/bin/python -m pip install --upgrade pip
.hermes-venv/bin/pip install hermes-agent`)

const hermesConfigYaml = computed(() => {
  if (!canGenerateConfigContent.value) return ''
  return buildHermesConfigYaml({
    gatewayBaseUrl: gatewayBaseUrl.value || 'http://127.0.0.1:3000',
    portalKey: portalKey.value,
    modelSlug: primaryModelSlug.value
  })
})

const hermesLaunch = computed(() => {
  if (!canGenerateConfigContent.value) return ''
  const model = primaryModelSlug.value
  return `# 项目根目录执行
./scripts/hermes.sh chat

# 指定模型
./scripts/hermes.sh -m ${model} chat`
})
const sortedModelStats = computed(() => catalogViewState.value.sortedModelStats)

const canGenerateConfigContent = computed(
  () =>
    Boolean(portalKey.value.trim()) &&
    catalogViewState.value.canGenerateConfigurationContent &&
    !fatalError.value
)
const hasConfigContent = canGenerateConfigContent

const codexConfigToml = computed(() =>
  canGenerateConfigContent.value
    ? buildCodexConfigToml({
        gatewayBaseUrl: gatewayBaseUrl.value,
        modelSlug: primaryModelSlug.value,
        modelReasoningEffort: primaryModelReasoningEffort.value
      })
    : ''
)

const codexModelCatalogJson = computed(() => {
  if (!canGenerateConfigContent.value || !codexCatalog.value) return ''
  return buildCodexModelCatalogJson(codexCatalog.value)
})

const codexAuthLoginCommand = computed(() =>
  canGenerateConfigContent.value ? buildCodexAuthLoginCommand(portalKey.value) : ''
)

const opencodeConfig = computed(() =>
  canGenerateConfigContent.value
    ? buildOpenCodeConfig({
      gatewayBaseUrl: gatewayBaseUrl.value,
      portalKey: portalKey.value,
      modelSlugs: allModelSlugs.value,
      selectedModelSlug: primaryModelSlug.value
    })
    : ''
)

const claudeCodeSettingsJson = computed(() =>
  canGenerateConfigContent.value
    ? buildClaudeCodeSettingsJson({
      gatewayBaseUrl: gatewayBaseUrl.value,
      portalKey: portalKey.value,
      modelSlugs: allModelSlugs.value,
      selectedModelSlug: primaryModelSlug.value
    })
    : ''
)

const anthropicCompatibleConfig = computed(() =>
  canGenerateConfigContent.value
    ? buildAnthropicCompatibleConfig({
      gatewayBaseUrl: gatewayBaseUrl.value,
      portalKey: portalKey.value,
      modelSlugs: allModelSlugs.value,
      selectedModelSlug: primaryModelSlug.value
    })
    : ''
)

const openAiCompatibleConfig = computed(() =>
  canGenerateConfigContent.value
    ? buildOpenAiCompatibleConfig({
      gatewayBaseUrl: gatewayBaseUrl.value,
      portalKey: portalKey.value,
      modelSlugs: allModelSlugs.value,
      selectedModelSlug: primaryModelSlug.value
    })
    : ''
)

const fetchGatewayCodexCatalog = async (key: string) => {
  const response = await fetch(`${buildGatewayModelsEndpoint(gatewayBaseUrl.value)}?client_version=0.144.4`, {
    headers: {
      Authorization: `Bearer ${key}`
    }
  })

  if (!response.ok) {
    throw new Error(`网关模型接口返回 ${response.status}`)
  }

  const payload: unknown = await response.json()
  if (!isCodexCatalogResponse(payload)) {
    throw new Error('live Codex catalog is unavailable')
  }
  if (payload.models.length === 0) {
    throw new Error('live Codex catalog is empty')
  }
  return payload
}

const applyCodexCatalog = (catalog: CodexCatalogResponse) => {
  codexCatalog.value = catalog
}

const loadIntegrationData = async () => {
  loading.value = true
  fatalError.value = ''
  loadWarnings.value = []
  portalKey.value = ''
  portalModelStats.value = []
  modelAllowlist.value = []
  codexCatalog.value = null
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
      applyCodexCatalog(await fetchGatewayCodexCatalog(portalKey.value))
    } catch (error) {
      codexCatalog.value = null
      const message = error instanceof Error ? error.message : '无法读取 live Codex catalog'
      fatalError.value = `${message}，已停止生成可复制的 Codex 配置。`
      return
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
  display: flex;
  flex-direction: column;
  gap: 24px;
  min-height: 100%;
}

h4,
p {
  margin: 0;
}

.integration-summary {
  padding-bottom: 20px;
  border-bottom: 1px solid var(--crc-border);
}

.summary-grid {
  margin-bottom: 12px;
}

.summary-grid :deep(.el-descriptions__cell) {
  padding: 10px 12px;
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

.integration-section {
  padding-top: 20px;
  border-top: 1px solid var(--crc-border);
}

.section-head h2 {
  margin: 0 0 4px;
  color: var(--crc-text-strong);
  font-size: 16px;
}

.section-head p {
  color: var(--crc-text-muted);
  font-size: 13px;
}

.model-ranking {
  margin-top: 12px;
  overflow: hidden;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.model-ranking__item {
  display: grid;
  grid-template-columns: 28px minmax(0, 1fr) auto;
  align-items: center;
  gap: 12px;
  min-height: 44px;
  padding: 9px 12px;
  border-bottom: 1px solid var(--crc-border);
  transition: background-color var(--crc-duration-fast) var(--crc-ease);
}

.model-ranking__item:hover {
  background: var(--crc-surface-hover);
}

.model-ranking__item:last-child {
  border-bottom: 0;
}

.model-ranking__position,
.model-ranking__metrics {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.model-ranking__identity {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 8px;
}

.model-ranking__identity strong {
  min-width: 0;
  color: var(--crc-text-strong);
  font-size: 13px;
  overflow-wrap: anywhere;
}

.model-ranking__metrics {
  white-space: nowrap;
}

.integration-empty {
  margin-top: 0;
  padding: 40px 20px;
}

.empty-copy {
  margin-top: 12px;
  color: var(--crc-text-muted);
  line-height: 1.7;
}

.code-surface {
  padding: 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
}

.config-section-head {
  margin-bottom: 12px;
}

.tab-body {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.section-alert {
  border-radius: var(--crc-radius-sm);
}

.step-card {
  padding: 18px 0;
  border-top: 1px solid var(--crc-border);
}

.step-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.step-head h4 {
  margin-bottom: 8px;
  color: var(--crc-text-strong);
  font-size: 15px;
}

.step-head p {
  color: var(--crc-text);
  line-height: 1.8;
}

.step-head code,
.summary-grid code {
  padding: 2px 6px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-strong);
  background: var(--crc-surface-muted);
  overflow-wrap: anywhere;
}

.code-block {
  max-width: 100%;
  max-height: 420px;
  margin: 14px 0 0;
  padding: 16px;
  overflow-x: auto;
  overflow-y: auto;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text);
  background: var(--crc-surface-muted);
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
  .step-head,
  .section-head {
    flex-direction: column;
    align-items: flex-start;
  }

  .summary-grid :deep(.el-descriptions__body) {
    display: block;
  }
}

.compat-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 16px;
}

.compat-family {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-surface);
}

.compat-family h4 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 15px;
}

.compat-protocol {
  color: var(--crc-accent);
  font-size: 12px;
  font-weight: 600;
}

.compat-family code {
  padding: 2px 6px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-strong);
  background: var(--crc-surface-muted);
  font-size: 12px;
  overflow-wrap: anywhere;
}

.compat-auth,
.compat-models,
.compat-adapter {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.compat-clients {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  margin-top: 4px;
}

@media (max-width: 767px) {
  .code-surface {
    padding: 12px;
  }

  .step-head {
    gap: 10px;
  }

  .step-head > .el-tooltip__trigger {
    align-self: flex-end;
  }

  .compat-grid {
    grid-template-columns: 1fr;
  }

  .model-ranking__item {
    grid-template-columns: 24px minmax(0, 1fr);
  }

  .model-ranking__metrics {
    grid-column: 2;
    white-space: normal;
  }
}
</style>
