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
      /<el-card v-else data-testid="integration-config-tabs" class="tabs-card">/
    )
  })
})
