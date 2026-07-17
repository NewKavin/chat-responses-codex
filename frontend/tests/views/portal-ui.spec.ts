import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const source = (name: string) => readFileSync(
  new URL(`../../src/views/portal/${name}.vue`, import.meta.url),
  'utf8'
)

const componentSource = (name: string) => readFileSync(
  new URL(`../../src/components/${name}.vue`, import.meta.url),
  'utf8'
)

describe('portal ui structure', () => {
  it('uses one flat quota summary and stable detail sections', () => {
    const overview = source('Overview')
    const details = source('QuotaDetails')

    expect(overview).toContain('crc-page portal-overview-page')
    expect(overview).toContain('quota-summary-grid')
    expect(overview).not.toContain('<el-card')
    expect(details).toContain('crc-page quota-details-page')
    expect(details).toContain('quota-detail-section')
  })

  it('uses a compact history toolbar and stable chart surfaces', () => {
    const history = source('UsageHistory')

    expect(history).toContain('crc-page usage-history-page')
    expect(history).toContain('crc-toolbar history-toolbar')
    expect(history).toContain('history-chart-grid')
    expect(history).toContain('crc-table-shell')
    expect(history).toContain('buildChartTheme')
    expect(history).toContain('watch(resolvedTheme')
    expect(history).not.toContain('history-card')
  })

  it('uses flat integration sections and bounded code examples', () => {
    const page = source('Integration')

    expect(page).toContain('crc-page integration-page')
    expect(page).toContain('integration-summary')
    expect(page).toContain('integration-section')
    expect(page).toContain('code-surface')
    expect(page).toContain('aria-label="复制代码"')
    expect(page).toContain('class="model-ranking"')
    expect(page).toContain('class="model-ranking__item"')
    expect(page).toContain('class="model-ranking__position"')
    expect(page).toContain('v-if="stat.model === primaryModelSlug"')
    expect(page).toContain('class="section-head config-section-head"')
    expect(page).not.toContain('integration-hero')

    const tabNames = [
      'name="codex"',
      'name="opencode"',
      'name="claude"',
      'name="cline"',
      'name="anthropic"',
      'name="hermes"'
    ]
    for (let index = 1; index < tabNames.length; index += 1) {
      expect(page.indexOf(tabNames[index])).toBeGreaterThan(page.indexOf(tabNames[index - 1]))
    }
  })

  it('uses icon controls and a mobile settings drawer', () => {
    const playground = source('Playground')

    expect(playground).toContain('playground-workspace')
    expect(playground).toContain('settings-panel')
    expect(playground).toContain('settingsDrawerOpen')
    expect(playground).toContain('<el-drawer')
    expect(playground).toContain('aria-label="打开模型设置"')
    expect(playground).toContain('<PlaygroundSettings')
    expect(playground).not.toContain("sidebarCollapsed ? '▶' : '◀'")
  })

  it('keeps message content and composer actions in stable bounded regions', () => {
    const playground = source('Playground')

    expect(playground).toContain('playground-message-stream')
    expect(playground).toContain('message-reasoning')
    expect(playground).toContain('playground-composer')
    expect(playground).toContain('composer-actions')
    expect(playground).toContain('placeholder="输入消息..."')
    expect(playground).not.toContain('placeholder="输入消息... (Enter')
    expect(playground).toContain('overflow-wrap: anywhere')
  })

  it('keeps automatic playground settings legible in the light theme', () => {
    const settings = componentSource('PlaygroundSettings')

    expect(settings.match(/inactive-text="自动"/g)).toHaveLength(3)
    expect(settings).toContain(
      '.playground-settings :deep(.el-switch:not(.is-checked) .el-switch__inner-wrapper)'
    )
    expect(settings).toContain('color: var(--crc-text-strong)')
  })

  it('uses focused key security and portal probe surfaces', () => {
    const keys = source('KeyManagement')
    const probe = source('ModelProbe')

    expect(keys).toContain('crc-page key-management-page')
    expect(keys).toContain('key-security-surface')
    expect(keys).toContain('rotate-key-dialog')
    expect(keys).toContain('aria-label="复制密钥"')
    expect(probe).toContain('crc-page portal-model-probe-page')
    expect(probe).toContain('tone="portal"')
  })
})
