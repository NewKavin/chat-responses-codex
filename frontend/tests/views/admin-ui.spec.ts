import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const source = (name: string) => readFileSync(
  new URL(`../../src/${name}`, import.meta.url),
  'utf8'
)

describe('admin ui structure', () => {
  it('removes the dashboard hero and uses compact page primitives', () => {
    const dashboard = source('views/admin/Dashboard.vue')

    expect(dashboard).toContain('crc-page dashboard-page')
    expect(dashboard).not.toContain('hero-panel')
  })

  it('keeps model qualification and probe evidence in compact sections', () => {
    const adminProbe = source('views/admin/ModelProbe.vue')
    const board = source('components/ModelProbeBoard.vue')

    expect(adminProbe).toContain('crc-page model-probe-page')
    expect(adminProbe).toContain('crc-table-shell')
    expect(board).toContain('probe-page-header')
    expect(board).not.toContain('summary-card')
  })

  it('uses the responsive upstream management workbench', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('crc-page upstreams-page')
    expect(page).toContain('crc-page-header')
    expect(page).toContain('crc-table-shell')
    expect(page).toContain('drawer-section')
    expect(page).toContain('drawer-footer')
  })

  it('uses the responsive downstream management workbench', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('crc-page downstreams-page')
    expect(page).toContain('crc-toolbar downstream-filters')
    expect(page).toContain('crc-table-shell')
    expect(page).toContain('drawer-footer')
    expect(page).toContain('rotate-key-dialog')
    expect(page).toContain('key-result-surface')
    expect(page).toContain('aria-label="复制新密钥"')
  })

  it('keeps log filters and evidence dense and responsive', () => {
    const page = source('views/admin/Logs.vue')

    expect(page).toContain('crc-page logs-page')
    expect(page).toContain('crc-toolbar logs-filters')
    expect(page).toContain('logs-filter-disclosure')
    expect(page).toContain('crc-table-shell')
    expect(page).toContain('log-summary-strip')
    expect(page).toContain('logs-table-region')
    expect(page).toContain('load-error-alert')
    expect(page).toContain('resetFilters')
  })

  it('uses unframed troubleshooting sections and one matrix tool surface', () => {
    const page = source('views/admin/Troubleshooting.vue')
    const center = source('components/TroubleshootingCenter.vue')
    const matrix = source('components/CompatibilityMatrixPanel.vue')

    expect(page).toContain('crc-page troubleshooting-page')
    expect(center).toContain('evidence-section')
    expect(center).toContain('crc-table-shell')
    expect(matrix).toContain('compatibility-matrix-panel crc-surface')
    expect(matrix).toContain('matrix-table-shell')
    expect(center).not.toContain('<el-card')
    expect(matrix).not.toContain('<el-card')
  })

  it('uses a focused unframed announcement form', () => {
    const page = source('views/admin/Announcement.vue')

    expect(page).toContain('crc-page announcement-page')
    expect(page).toContain('announcement-form-surface')
    expect(page).not.toContain('<el-card')
  })
})
