import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const source = (name: string) => readFileSync(
  new URL(`../../src/views/portal/${name}.vue`, import.meta.url),
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
    expect(page).not.toContain('integration-hero')
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
})
