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

  it('refreshes overview data quietly without layout jumps', () => {
    const overview = source('Overview')

    // 轮询 15s + 深比较：数据没变化就不重新赋值（避免整页重渲染跳动）
    expect(overview).toContain('LIVE // 15S REFRESH')
    expect(overview).toContain('}, 15000)')
    expect(overview).toContain('JSON.stringify(payload) !== JSON.stringify(data.value)')
    // 数字等宽 + 平滑过渡：位数变化不再引起容器宽度/高度跳动
    expect(overview).toContain('font-variant-numeric: tabular-nums')
  })

  it('surfaces backend errors on portal data loading', () => {
    for (const page of ['QuotaDetails', 'UsageHistory', 'Overview']) {
      const src = source(page)
      expect(src).not.toContain("ElMessage.error('加载数据失败')")
      expect(src).not.toContain("ElMessage.error('加载图表失败')")
      expect(src).not.toContain("ElMessage.error('加载日志失败')")
      expect(src).not.toContain("ElMessage.error('加载限额详情失败')")
      expect(src).toContain('(error as any)?.message')
    }
  })

  it('auto-generates portal key ids server-side (no user-filled key id)', () => {
    const keys = source('KeyManagement')
    const card = componentSource('KeyCard')

    // 用户不再填写“密钥 ID”：创建表单与轮换输入框都已移除
    expect(keys).not.toContain('newKeyForm.downstream_id')
    expect(keys).not.toContain("placeholder=\"sk-...\"")
    expect(card).not.toContain('newKeyId')
    // 服务端生成密钥；正文随时可回看/复制（不是一次性）
    expect(keys).toContain('portalApi.createKey({')
    expect(keys).toContain('plaintext_key')
    expect(keys).toContain('可随时在密钥卡片上查看并复制')
    expect(keys).toContain('复制密钥')
    expect(card).not.toContain('请输入新的密钥 ID')
    // 卡片只显示密钥一行：不显示内部密钥 ID
    expect(card).toContain('key-secret-row')
    expect(card).toContain('aria-label="Copy key"')
    expect(card).not.toContain('maskedKeyId')
    expect(card).not.toContain('Copy key ID')
    expect(card).not.toContain('***')
    expect(keys).not.toContain('secret-label">密钥 ID')
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
    expect(page).not.toContain('model-ranking')
    expect(page).not.toContain('sortedModelStats')
    expect(page).not.toContain('模型排序')
    expect(page).toContain('class="section-head config-section-head"')
    expect(page).not.toContain('integration-hero')

    const tabNames = [
      'name="codex"',
      'name="opencode"',
      'name="claude"',
      'name="cline"',
      'name="kilo"',
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
    expect(playground).toContain('aria-label="参数设置"')
    expect(playground).toContain('model-picker')
    expect(playground).toContain('<PlaygroundSettings')
    expect(playground).not.toContain("sidebarCollapsed ? '▶' : '◀'")
  })

  it('keeps message content and composer actions in stable bounded regions', () => {
    const playground = source('Playground')

    expect(playground).toContain('playground-message-stream')
    expect(playground).toContain('playground-message-stream__inner')
    expect(playground).toContain('message-reasoning')
    expect(playground).toContain('playground-composer')
    expect(playground).toContain('composer-input-row')
    expect(playground).toContain('composer-hint')
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
    const card = componentSource('KeyCard')
    const probe = source('ModelProbe')

    expect(keys).toContain('crc-page key-management-page')
    expect(card).toContain('key-card')
    expect(card).toContain('轮换密钥')
    expect(card).toContain('复制密钥')
    expect(probe).toContain('crc-page portal-model-probe-page')
    expect(probe).toContain('tone="portal"')
  })
})

describe('portal runtime concurrency display', () => {
  it('shows cost billing amounts instead of token usage', () => {
    const overview = source('Overview')

    expect(overview).toContain('每日金额')
    expect(overview).toContain('cost_daily')
    expect(overview).toContain('formatMoney')
    expect(overview).toContain('cost_summary')
    expect(overview).toContain('近 24 小时金额')
    expect(overview).toContain('本月金额')
  })

  it('labels the cost quota as account-scoped instead of per-key', () => {
    const overview = source('Overview')
    const details = source('QuotaDetails')

    // 费用限额按账号：门户概览与配额页的金额是账号口径，不出现「本密钥」类文案。
    expect(overview).toContain('账号每日金额')
    expect(overview).toContain('按账号汇总')
    expect(details).toContain('账号每日金额')
    expect(details).toContain('按账号')
    expect(overview.match(/本密钥/g)).toBeNull()
    expect(details.match(/本密钥/g)).toBeNull()
  })

  it('renders running waiting admitted and limit in the overview', () => {
    const overview = source('Overview')

    expect(overview).toContain('运行中')
    expect(overview).toContain('等待上游')
    expect(overview).toContain('已占用')
    expect(overview).toContain('上限')
  })

  it('renders concurrency as a status grid instead of a bare text strip', () => {
    const overview = source('Overview')

    expect(overview).toContain('overview-runtime-grid')
    expect(overview).toContain('overview-runtime-card')
    expect(overview).toContain('下游并发状态')
    expect(overview).toContain('overview-runtime-card__value')
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
    expect(page).toContain('history.logs')
    expect(page).toContain('history.total')
    expect(page).not.toContain('history.recent_logs')
  })

  it('keeps the shared loading state active until parallel requests settle', () => {
    const page = source('UsageHistory')

    expect(page).toContain('activeLoads')
    expect(page).toContain('loading.value = activeLoads.value > 0')
  })
})
