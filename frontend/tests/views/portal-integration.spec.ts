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

  it('uses the Codex compatibility version and exposes strict doctor guidance', () => {
    expect(integrationView).toContain('client_version=0.144.6')
    expect(integrationView).not.toContain('client_version=0.144.4')
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
  })
})
