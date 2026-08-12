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
    expect(dashboard).toContain("const chartRange = ref<ChartRange>('7d')")
    expect(dashboard).toContain("range: '7d'")
    expect(dashboard).toContain('item.day.slice(5)')
    expect(dashboard).not.toContain('hero-panel')
  })

  it('keeps model qualification and probe evidence in compact sections', () => {
    const adminProbe = source('views/admin/ModelProbe.vue')
    const board = source('components/ModelProbeBoard.vue')

    expect(adminProbe).toContain('crc-page model-probe-page')
    expect(adminProbe).toContain('crc-table-shell')
    expect(adminProbe).toContain('模型探测 / Model Probe')
    expect(adminProbe).toContain('模型探测页会实时探测所有上游模型，测试结果会实时刷新。')
    expect(adminProbe).toContain('runCapabilityProbe')
    expect(adminProbe).toContain('probeAllCapabilities')
    expect(adminProbe).toContain('getCapabilityDiscovery')
    expect(adminProbe).toContain('pollCapabilityDiscovery')
    expect(adminProbe).toContain('routeStatusLabel')
    expect(adminProbe).toContain('capabilityModelResults')
    expect(adminProbe).toContain('capabilityRouteResults')
    expect(adminProbe).toContain('模型汇总')
    expect(adminProbe).toContain('精确路由')
    expect(adminProbe).toContain('row.route_id')
    expect(adminProbe).not.toContain('queueDialectProbe')
    expect(adminProbe).not.toContain('capabilityProbeCandidates')
    expect(adminProbe).not.toContain('discoveryBatchProgress')
    expect(adminProbe).not.toContain('runWithConcurrency')
    expect(adminProbe).toContain('capabilityProbeProgress')
    expect(adminProbe).toContain('capability-probe-progress')
    expect(adminProbe).toContain('waitForProbesToSettle')
    expect(adminProbe).toContain('<el-tabs v-model="activeProbeTab" class="model-probe-tabs">')
    expect(adminProbe).toContain('<el-tab-pane label="模型状态" name="status">')
    expect(adminProbe).toContain('<el-tab-pane label="思考档位" name="reasoning">')
    expect(adminProbe).toContain('const activeProbeTab = ref<ProbeTab>(\'status\')')
    expect(adminProbe).toContain("activeProbeTab.value = 'reasoning'")
    expect(adminProbe).toContain('description="暂无思考档位探测结果"')
    expect(adminProbe).toContain("mode: 'reasoning'")
    expect(adminProbe).toContain('selectedProbeModels.length === 0')
    expect(adminProbe).toContain('请先选择要探测的模型')
    expect(adminProbe).toContain('probeModelScopeLoadFailed')
    expect(adminProbe).toContain('filterCapabilityDiscoveryByModels')
    expect(adminProbe).toContain('const showOnlyCurrentBatch = ref(true)')
    expect(adminProbe).toContain('capabilityDiagnosticTooltip')
    expect(adminProbe).toContain('全局 discovery')
    expect(adminProbe).not.toContain('selectedProbeModels.value = [...models]')
    expect(adminProbe).not.toContain('const { data: full } = await adminApi.getModels()')
    expect(adminProbe).not.toContain('后端按全量处理')
    const statusTabStart = adminProbe.indexOf('<el-tab-pane label="模型状态" name="status">')
    const reasoningTabStart = adminProbe.indexOf('<el-tab-pane label="思考档位" name="reasoning">')
    const tabsEnd = adminProbe.indexOf('</el-tabs>', reasoningTabStart)
    const statusTab = adminProbe.slice(statusTabStart, reasoningTabStart)
    const reasoningTab = adminProbe.slice(reasoningTabStart, tabsEnd)
    expect(statusTab).toContain('<ModelProbeBoard')
    expect(statusTab).toContain('v-if="qualificationResult"')
    expect(statusTab).not.toContain('capability-probe-results')
    expect(reasoningTab).toContain('capability-probe-progress')
    expect(reasoningTab).toContain('capability-probe-results')
    expect(reasoningTab).toContain('模型汇总')
    expect(reasoningTab).toContain('精确路由')
    expect(reasoningTab).not.toContain('<ModelProbeBoard')
    expect(adminProbe).toContain(':on-retry="loadData"')
    expect(board).toContain('probe-page-header')
    expect(board).toContain('重试')
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

  it('shows display IDs and persists the final-column upstream remark', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('template #default="{ $index }"')
    expect(page).toContain('{{ $index + 1 }}')
    expect(page).not.toContain('<el-table-column prop="id" label="ID"')
    expect(page).toContain('v-model="form.remark"')
    expect(page).toContain("{{ row.remark || '-' }}")
    expect(page.indexOf('label="备注"')).toBeLessThan(page.indexOf('label="操作"'))
    expect(page).toContain('remark: String(form.value.remark || \'\').trim()')
  })

  it('loads upstreams asynchronously without polling the whole workbench', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('onMounted(() =>')
    expect(page).not.toContain('setInterval')
    expect(page).not.toContain('startAutoRefresh')
    expect(page).not.toContain('onUnmounted')
  })

  it('supports batch enable disable and delete for upstreams', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('type="selection"')
    expect(page).toContain('@selection-change="handleSelectionChange"')
    expect(page).toContain('批量启用')
    expect(page).toContain('批量禁用')
    expect(page).toContain('批量删除')
    expect(page).toContain('handleBatchToggle')
    expect(page).toContain('handleBatchDelete')
    expect(page).toContain('batchToggleUpstreams')
    expect(page).toContain('batchDeleteUpstreams')
    expect(page).toContain('selectedUpstreams.length')
  })

  it('confirms and refreshes after resetting one upstream route cooldown', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('@click="handleResetRouteHealth(row)"')
    expect(page).toContain('确认解除临时冷却')
    expect(page).toContain('adminApi.resetUpstreamRouteHealth(row.id)')
    expect(page).toContain('await loadData()')
  })

  it('supports inline upstream priority updates', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('label="优先级/权重"')
    expect(page).toContain('@change="updateInlinePriority(row)"')
    expect(page).toContain('adminApi.updateUpstream(row.id, { priority })')
    expect(page).not.toContain('concurrency_status_enabled')
    expect(page).not.toContain('私有并发状态接口')
    expect(page).not.toContain('updateInlineConcurrencyStatus')
  })

  it('configures one per-key concurrency value across upstream form paths', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain('label="每 Key 最大并发"')
    expect(page).toContain('v-model="form.max_concurrency"')
    expect(page).toContain('adminApi.getRuntimeSettings()')
    expect(page).toContain('default_upstream_max_concurrency')
    expect(page).toContain('max_concurrency: row.max_concurrency')
    expect(page).toContain('submitData.max_concurrency = Number(form.value.max_concurrency)')
    expect(page).toContain('max_concurrency: Number(form.value.max_concurrency)')
    expect(page).not.toContain('delete submitData.max_concurrency')
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

  it('keeps the upstream model discovery action visible before prerequisites are entered', () => {
    const page = source('views/admin/Upstreams.vue')

    expect(page).toContain(':disabled="!form.base_url || !form.api_key"')
    expect(page).toContain(':icon="RefreshCw"')
    expect(page).toContain('获取模型列表')
    expect(page).not.toContain('v-if="form.base_url && form.api_key"')
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

  it('keeps upstream and downstream log filters inside the selected day', () => {
    const page = source('views/admin/Logs.vue')
    const api = source('api/admin.ts')

    expect(page).toContain('v-model="filters.downstream_id"')
    expect(page).toContain('v-model="filters.upstream_id"')
    expect(page).toContain('params.downstream_id = filters.value.downstream_id.trim()')
    expect(page).toContain('params.upstream_id = filters.value.upstream_id.trim()')
    expect(api).toContain('downstream_id?: string')
    expect(api).toContain('upstream_id?: string')
  })

  it('uses a focused unframed announcement form', () => {
    const page = source('views/admin/Announcement.vue')

    expect(page).toContain('crc-page announcement-page')
    expect(page).toContain('announcement-form-surface')
    expect(page).not.toContain('<el-card')
  })

  it('replaces stale settings feedback instead of stacking messages', () => {
    const page = source('views/admin/Settings.vue')
    const baseStyles = source('styles/base.css')
    const feedbackHelper = page.slice(
      page.indexOf('const showSettingsMessage'),
      page.indexOf('const loadSettings')
    )

    expect(feedbackHelper).toContain('ElMessage.closeAll()')
    expect(feedbackHelper).toContain("customClass: 'settings-feedback-message'")
    expect(baseStyles).toContain('.el-message.settings-feedback-message')
    expect(baseStyles).toContain('bottom: max(16px, env(safe-area-inset-bottom)) !important')
    expect(page).not.toMatch(/ElMessage\.(success|warning|error)\(/)
  })

  it('warns that automatic capability probing scans every visible model', () => {
    const page = source('views/admin/Settings.vue')
    const catalog = source('utils/runtimeSettings.ts')

    expect(catalog).toContain('开启后会周期性对所有下游可见模型自动探测（消耗 token）')
    expect(page).toContain('field.description')
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

  it('keeps downstream row height consistent with upstreams with icon-only key copy', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('class="compact-downstreams-table"')
    expect(page).not.toContain('@click="toggleKeyView(row.id)"')
    expect(page).not.toContain('expandedKeys')
    expect(page).toContain('content="复制秘钥"')
    expect(page).toContain('aria-label="复制秘钥"')
    expect(page).toContain('<Copy :size="13"')
    expect(page).toContain('class="row-actions"')
    expect(page).not.toContain('fixed="right"')
    expect(page).toContain('class="crc-table-shell downstreams-table-shell"')
    expect(page).toContain('<el-table-column label="运行并发" width="400">')
    expect(page).toMatch(/\.runtime-cell\s*\{[^}]*flex-wrap:\s*nowrap/s)
    expect(page).toMatch(/\.runtime-cell\s*\{[^}]*white-space:\s*nowrap/s)
    expect(page).not.toMatch(/\.compact-downstreams-table\s*:deep\(\.el-table__cell\)/)
    expect(page).toMatch(/\.downstreams-table-shell\s*\{[^}]*overflow:\s*hidden/s)
    expect(page).toMatch(/\.downstreams-table-shell\s*>\s*\.compact-downstreams-table\s*\{[^}]*min-width:\s*0/s)
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

describe('admin downstream cost billing', () => {
  it('offers a cost-billing mode with price and daily cost limit inputs', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('按金额')
    expect(page).toContain('value="cost"')
    expect(page).toContain('inputTokenPricePerMillion')
    expect(page).toContain('outputTokenPricePerMillion')
    expect(page).toContain('dailyCostLimit')
    expect(page).toContain('input_token_price_per_million_cents')
    expect(page).toContain('output_token_price_per_million_cents')
    expect(page).toContain('daily_cost_limit_cents')
    expect(page).not.toContain('按 Token')
    expect(page).not.toContain('value="token"')
  })

  it('converts yuan to cents on submit and cents back to yuan on edit', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toMatch(/input_token_price_per_million_cents:\s*isCost\s*\?/)
    expect(page).toMatch(/output_token_price_per_million_cents:\s*isCost\s*\?/)
    expect(page).toMatch(/daily_cost_limit_cents:\s*isCost\s*\?/)
    expect(page).toContain('input_token_price_per_million_cents / 100')
    expect(page).toContain('output_token_price_per_million_cents / 100')
    expect(page).toContain('daily_cost_limit_cents / 100')
    expect(page).toContain('billing_mode: isCost ? \'token\' : \'request\'')
  })

  it('keeps input/output prices and daily limit on the same row', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('price-row')
    expect(page).toContain('inputTokenPricePerMillion')
    expect(page).toContain('outputTokenPricePerMillion')
    expect(page).toContain('dailyCostLimit')
    expect(page).toMatch(/inputTokenPricePerMillion[\s\S]{0,400}outputTokenPricePerMillion[\s\S]{0,400}dailyCostLimit/)
  })

  it('shows the cost limit in the quota column for cost-billed rows', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toMatch(/金额计费/)
    expect(page).toContain('isCostRow(row)')
  })

  it('shows only the balance in the cost quota column', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('金额计费')
    expect(page).toContain('余额 {{ formatMoney')
    expect(page).not.toContain('余额 ¥¥')
    expect(page).not.toContain('已用')
    expect(page).not.toContain('已消耗')
    expect(page).not.toContain('IN ¥')
    expect(page).not.toContain('OUT ¥')
    expect(page).not.toContain('¥{{ (row.daily_cost_limit_cents ?? 0) / 100 }}/日')
    // 表单内仍保留单价与日上限设置
    expect(page).toContain('IN 单价/M')
    expect(page).toContain('OUT 单价/M')
    expect(page).toContain('日上限')
    expect(page).toContain('金额计费（元）')
  })

  it('keeps the batch dialog able to set cost billing fields', () => {
    const page = source('views/admin/Downstreams.vue')

    expect(page).toContain('batchForm.billing_mode === \'cost\'')
    expect(page).toContain('batchForm.input_token_price_per_million_cents')
    expect(page).toContain('batchForm.output_token_price_per_million_cents')
    expect(page).toContain('batchForm.daily_cost_limit_cents')
  })
})
