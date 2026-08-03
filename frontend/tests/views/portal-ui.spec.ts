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
    expect(history).toContain("const timeRange = ref<ChartRange>('7d')")
    expect(history).not.toContain('history-card')
    expect(history).toContain('label="延迟"')
    expect(history).toContain('首字')
    expect(history).toContain('总耗时')
    expect(history).toContain('formatLatencySeconds(row.first_token_latency_ms)')
    expect(history).toContain('formatLatencySeconds(row.latency_ms)')
    expect(history).not.toContain('{{ row.latency_ms }}ms')
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
    expect(playground).toContain('append-to-body')
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

describe('portal runtime concurrency display', () => {
  it('renders running waiting admitted and limit in the overview', () => {
    const overview = source('Overview')

    expect(overview).toContain('运行中')
    expect(overview).toContain('等待上游')
    expect(overview).toContain('已占用')
    expect(overview).toContain('上限')
  })

  it('reuses the existing overview poll instead of adding a timer', () => {
    const overview = source('Overview')

    expect(overview).toContain('loadOverview')
    expect(overview).toContain('setInterval')
    expect(overview).not.toContain('setInterval(loadRuntime')
  })
})

describe('portal usage history independence', () => {
  it('uses a date-only picker and rejects datetimerange', () => {
    const page = source('UsageHistory')

    expect(page).toContain('type="date"')
    expect(page).toContain('value-format="YYYY-MM-DD"')
    expect(page).not.toContain('type="datetimerange"')
  })

  it('keeps chart summary and log detail requests separate', () => {
    const page = source('UsageHistory')

    expect(page).toContain('loadSummary')
    expect(page).toContain('loadLogs')
    expect(page).toContain('pagination.value.page = 1')
  })
})
