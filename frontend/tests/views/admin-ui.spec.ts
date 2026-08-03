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
    expect(dashboard).toContain("const chartRange = ref<ChartRange>('1d')")
    expect(dashboard).toContain("range: '1d'")
    expect(dashboard).not.toContain('hero-panel')
  })

  it('keeps model qualification and probe evidence in compact sections', () => {
    const adminProbe = source('views/admin/ModelProbe.vue')
    const board = source('components/ModelProbeBoard.vue')

    expect(adminProbe).toContain('crc-page model-probe-page')
    expect(adminProbe).toContain('crc-table-shell')
    expect(adminProbe).toContain('会消耗模型 token')
    expect(board).toContain('probe-page-header')
    expect(board).not.toContain('summary-card')
  })

  it('uses anonymous route ids for model probe channels', () => {
    const board = source('components/ModelProbeBoard.vue')
    const charts = source('utils/modelProbeCharts.ts')

    expect(board).toContain('channel.route_id')
    expect(board).not.toContain('channel.key_prefix')
    expect(charts).toContain('route_id')
    expect(charts).not.toContain('key_prefix')
  })

  it('uses anonymous route ids for capability profile actions', () => {
    const api = source('api/admin.ts')
    const types = source('types/index.ts')
    const center = source('components/TroubleshootingCenter.vue')
    const page = source('views/admin/Troubleshooting.vue')

    for (const contents of [api, types, center, page]) {
      expect(contents).toContain('route_id')
      expect(contents).not.toContain('key_fingerprint')
    }
  })

  it('uses the responsive upstream management workbench', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('crc-page upstreams-page')
    expect(page).toContain('crc-page-header')
    expect(page).toContain('crc-table-shell')
    expect(page).toContain('drawer-section')
    expect(page).toContain('drawer-footer')
    expect(page).toContain('size="var(--account-drawer-width)"')
    expect(page).toContain('upstream-account-drawer')
  })

  it('loads upstreams asynchronously without polling the whole workbench', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('onMounted(() =>')
    expect(page).not.toContain('setInterval')
    expect(page).not.toContain('startAutoRefresh')
    expect(page).not.toContain('onUnmounted')
  })

  it('labels the private concurrency adapter as opt-in and non-standard', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('v-model="form.concurrency_status_enabled"')
    expect(page).toContain('私有并发状态接口')
    expect(page).toContain('非 OpenAI 标准接口，默认关闭')
    expect(page).toContain('concurrency_status_enabled: Boolean(submitData.concurrency_status_enabled)')
  })

  it('labels indexed model discovery results without key prefixes', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('Key #${item.key_index + 1}')
    expect(page).not.toContain('item.key_prefix')
  })

  it('keeps discovered upstream models as explicit selection candidates', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('discoveredModelCandidates')
    expect(page).toContain('latestDiscoveryResults')
    expect(page).toContain('mergeDiscoveredModelCandidates')
    expect(page).toContain('buildSelectedKeyModelMappings')
    expect(page).toContain('v-for="model in selectableModelOptions"')
    expect(page).not.toContain('form.value.supported_models = mappedModels')
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
    expect(page).toContain('size="var(--account-drawer-width)"')
    expect(page).toContain('downstream-account-drawer')
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
    expect(page).toContain('label="延迟"')
    expect(page).toContain('首字')
    expect(page).toContain('总耗时')
    expect(page).toContain('formatLatencySeconds(row.first_token_latency_ms)')
    expect(page).toContain('formatLatencySeconds(row.latency_ms)')
    expect(page).not.toContain('{{ row.latency_ms }}ms')
  })

  it('uses the flattened server day and keeps runtime failures scoped to polling', () => {
    const logs = source('views/admin/Logs.vue')
    expect(logs).toContain('data.day')
    expect(logs).not.toContain('data.window?.day')

    const downstreams = source('views/admin/Downstreams.vue')
    const loadDataBody = downstreams.slice(
      downstreams.indexOf('const loadData = async () =>'),
      downstreams.indexOf('const loadRuntime = async () =>')
    )
    expect(loadDataBody).not.toContain('markRuntimeUnavailable()')
  })

  it('uses unframed troubleshooting sections and one matrix tool surface', () => {
    const page = source('views/admin/Troubleshooting.vue')
    const center = source('components/TroubleshootingCenter.vue')
    const matrix = source('components/CompatibilityMatrixPanel.vue')

    expect(page).toContain('crc-page troubleshooting-page')
    expect(center).toContain('evidence-section')
    expect(center).toContain('crc-table-shell')
    expect(center).toContain('diagnostic-workspace-container')
    expect(center).toContain('diagnostic-workspace')
    expect(center).toContain('diagnostic-results-stack')
    expect(center).toContain('container-name: diagnostic-workspace;')
    expect(center).toContain('grid-template-columns: minmax(320px, 0.75fr) minmax(560px, 1.25fr);')
    expect(center).toContain('@container diagnostic-workspace (max-width: 960px)')
    expect(center).not.toContain('<el-row')
    expect(center).not.toContain('<el-col')
    expect(center).toContain(':data="paginatedDialectProfiles"')
    expect(center).toContain('v-model:current-page="profilePage"')
    expect(center).toContain('v-model:page-size="profilePageSize"')
    expect(center).toContain(':page-sizes="profilePageSizes"')
    expect(center).toContain(':total="dialectProfiles.length"')
    expect(center).toContain('const profilePage = ref(1)')
    expect(center).toContain('const profilePageSize = ref(10)')
    expect(center).toContain('const profilePageSizes = [10, 20, 50]')
    expect(center).toContain('const paginatedDialectProfiles = computed')
    expect(center).toContain('normalizeProfilePage()')
    expect(center).toContain('class="capability-pagination"')
    expect(center).toContain(':data="selectedResolvedCapabilityRows"')
    expect(center).toContain(':data="selectedResolved.conflicts"')
    expect(matrix).toContain('compatibility-matrix-panel crc-surface')
    expect(matrix).toContain('matrix-table-shell')
    expect(matrix).toContain('container-name: compatibility-matrix;')
    expect(matrix).toContain('@container compatibility-matrix (max-width: 860px)')
    expect(matrix).toContain('max-width: 100%;')
    expect(center).not.toContain('<el-card')
    expect(matrix).not.toContain('<el-card')

    const workspaceStart = center.indexOf('<div class="diagnostic-workspace-container">')
    const capabilityStart = center.indexOf('class="evidence-section capability-panel"')
    expect(workspaceStart).toBeGreaterThan(-1)
    expect(capabilityStart).toBeGreaterThan(workspaceStart)
    expect(center).toMatch(
      /<\/div>\s*<\/div>\s*<section v-if="admin && exportCapabilities && importCapabilities" class="evidence-section capability-panel">/
    )
  })

  it('uses a focused unframed announcement form', () => {
    const page = source('views/admin/Announcement.vue')

    expect(page).toContain('crc-page announcement-page')
    expect(page).toContain('announcement-form-surface')
    expect(page).not.toContain('<el-card')
  })
})

describe('admin downstream runtime display', () => {
  it('polls only lightweight downstream runtime and clears the timer', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('adminApi.getDownstreamRuntime()')
    expect(page).toContain('window.setInterval(loadRuntime, 5000)')
    expect(page).toContain('clearInterval(runtimeTimer)')
    expect(page).not.toContain('window.setInterval(loadData')
  })

  it('renders running waiting admitted and limit in the admin view', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('运行中')
    expect(page).toContain('等待上游')
    expect(page).toContain('已占用')
    expect(page).toContain('上限')
  })

  it('marks runtime failures and missing ids unavailable without zero-filling counts', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('available: false')
    expect(page).toContain('runtimeById[row.id]?.available === false')
    expect(page).not.toContain('running ?? 0')
    expect(page).not.toContain('waiting_upstream ?? 0')
    expect(page).not.toContain('admitted ?? 0')
  })
})

describe('admin logs single-day picker', () => {
  it('uses a date-only picker and rejects datetimerange', () => {
    const page = source('views/admin/Logs.vue')

    expect(page).toContain('type="date"')
    expect(page).toContain('value-format="YYYY-MM-DD"')
    expect(page).not.toContain('type="datetimerange"')
  })
})
