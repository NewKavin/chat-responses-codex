import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const integrationView = readFileSync(
  new URL('../../src/views/portal/Integration.vue', import.meta.url),
  'utf8'
)

describe('portal integration live catalog wiring', () => {
  it('shows only the empty state when the live catalog cannot produce configuration', () => {
    expect(integrationView).toContain('if (payload.models.length === 0)')
    expect(integrationView).toContain("throw new Error('live Codex catalog is empty')")
    expect(integrationView).toContain('codexCatalog.value = null')
    expect(integrationView).toContain('catalogViewState.value.canGenerateConfigurationContent')
    expect(integrationView).toMatch(
      /<el-empty\s+v-if="!hasConfigContent"\s+data-testid="integration-empty"/
    )
    expect(integrationView).toMatch(
      /<(section|div) v-else data-testid="integration-config-tabs" class="code-surface">/
    )
  })

  it('uses the explicit Codex catalog format and exposes model selection', () => {
    expect(integrationView).toContain('?format=codex')
    expect(integrationView).not.toContain('client_version=0.144.6')
    expect(integrationView).toContain('v-model="selectedCodexModelSlug"')
    expect(integrationView).toContain('v-for="modelSlug in allModelSlugs"')
    expect(integrationView).toContain('resolveCodexModelSelection')
    expect(integrationView).toContain('modelSlug: codexModelSelection.value.modelSlug')
    expect(integrationView).toContain('buildCodexAuthLoginCommand()')
    expect(integrationView).toContain('label="历史最常用模型"')
    expect(integrationView).toContain('历史最常用')
    expect(integrationView).not.toContain('label="默认模型"')
    expect(integrationView).toContain('codex --strict-config doctor --summary')
    expect(integrationView).toContain('max_threads')
    expect(integrationView).toContain('并发代理线程')
    expect(integrationView).toContain('max_depth')
    expect(integrationView).toContain('嵌套委派深度')
    expect(integrationView).toContain('不覆盖网关 quota')
    expect(integrationView).toContain('白名单中的全部模型')
    expect(integrationView).toContain('替换完整的 model-catalog.json')
    expect(integrationView).toContain('不要复制其他模型条目')
    expect(integrationView).toContain('不需要配置 upstream_id 或指纹')
    expect(integrationView).toContain('新建 Codex 会话')
    expect(integrationView).toContain('multi_agent_version')
    expect(integrationView).toContain('V1')
    expect(integrationView).toContain('multi_agent_v2')
    expect(integrationView).toContain('不要启用')
  })

  it('exposes a default subagent role file generated from the selected model', () => {
    expect(integrationView).toContain('~/.codex/agents/default.toml')
    expect(integrationView).toContain('codexDefaultAgentToml')
    expect(integrationView).toContain('buildCodexDefaultAgentToml')
    expect(integrationView).toContain('子代理')
  })
})
