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
})
