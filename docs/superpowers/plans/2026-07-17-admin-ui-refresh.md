# Admin UI Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate every admin page and shared operational component to the approved dense, neutral, theme-aware console design without changing admin behavior.

**Architecture:** Page templates consume the global `.crc-*` layout primitives and semantic CSS variables established by the foundation plan while retaining page-local business logic. A pure chart palette helper centralizes light/dark ECharts colors, and chart owners recreate their instances only when the resolved theme changes. Source-contract tests guard page structure and existing utility/API tests guard behavior.

**Tech Stack:** Vue 3, TypeScript, Element Plus, ECharts 5, CSS custom properties, Vitest.

---

## Prerequisite

Complete `docs/superpowers/plans/2026-07-17-console-ui-foundation.md`. Confirm
`frontend/src/styles/tokens.css`, `frontend/src/styles/base.css`, the theme
composable, and shared shell are present and the full frontend suite passes.

## File Map

**Create:**

- `frontend/src/utils/chartTheme.ts` - semantic ECharts palette resolved from the current theme.
- `frontend/tests/utils/chartTheme.spec.ts` - palette contract tests.
- `frontend/tests/views/admin-ui.spec.ts` - source contracts for every refreshed admin surface.

**Modify:**

- `frontend/src/views/admin/Dashboard.vue`
- `frontend/src/views/admin/ModelProbe.vue`
- `frontend/src/views/admin/Upstreams.vue`
- `frontend/src/views/admin/Downstreams.vue`
- `frontend/src/views/admin/Logs.vue`
- `frontend/src/views/admin/Troubleshooting.vue`
- `frontend/src/views/admin/Announcement.vue`
- `frontend/src/components/ModelProbeBoard.vue`
- `frontend/src/components/TroubleshootingCenter.vue`
- `frontend/src/components/CompatibilityMatrixPanel.vue`

Business utilities and API clients are test dependencies but remain unchanged.

### Task 1: Shared ECharts Palette

**Files:**

- Create: `frontend/tests/utils/chartTheme.spec.ts`
- Create: `frontend/src/utils/chartTheme.ts`

- [ ] **Step 1: Write the failing palette tests**

```ts
import { describe, expect, it } from 'vitest'
import { buildChartTheme } from '../../src/utils/chartTheme'

describe('chart theme', () => {
  it('uses readable semantic colors in light mode', () => {
    const theme = buildChartTheme('light')
    expect(theme.text).toBe('#34413d')
    expect(theme.muted).toBe('#66716d')
    expect(theme.border).toBe('#dfe5e2')
    expect(theme.series).toHaveLength(8)
  })

  it('uses neutral charcoal contrast in dark mode', () => {
    const theme = buildChartTheme('dark')
    expect(theme.text).toBe('#d2dad6')
    expect(theme.tooltipBackground).toBe('#202624')
    expect(theme.series[0]).toBe('#39b99c')
  })
})
```

- [ ] **Step 2: Run the test and verify the missing module failure**

Run from `frontend/`: `rtk npx vitest run tests/utils/chartTheme.spec.ts`

Expected: FAIL because `chartTheme.ts` does not exist.

- [ ] **Step 3: Implement the pure palette**

```ts
import type { ResolvedTheme } from '@/composables/useTheme'

export interface ChartTheme {
  text: string
  muted: string
  border: string
  splitLine: string
  tooltipBackground: string
  tooltipBorder: string
  series: string[]
}

export const buildChartTheme = (mode: ResolvedTheme): ChartTheme => mode === 'dark'
  ? {
      text: '#d2dad6', muted: '#98a59f', border: '#343d39', splitLine: '#2a322f',
      tooltipBackground: '#202624', tooltipBorder: '#343d39',
      series: ['#39b99c', '#60a5d8', '#4ade80', '#f6ad55', '#fb7185', '#a78bda', '#67c7d4', '#d6b46c']
    }
  : {
      text: '#34413d', muted: '#66716d', border: '#dfe5e2', splitLine: '#e9eeec',
      tooltipBackground: '#ffffff', tooltipBorder: '#dfe5e2',
      series: ['#0f8f76', '#2563a6', '#15803d', '#b45309', '#c2413b', '#7456a6', '#258a9a', '#98732e']
    }
```

- [ ] **Step 4: Run the focused test**

Run from `frontend/`: `rtk npx vitest run tests/utils/chartTheme.spec.ts`

Expected: PASS, 2 tests.

- [ ] **Step 5: Commit the chart palette**

```bash
rtk git add frontend/src/utils/chartTheme.ts frontend/tests/utils/chartTheme.spec.ts
rtk git commit -m "feat(ui): add semantic chart palette" -m "Constraint: Keep chart business aggregation unchanged
Confidence: high
Scope-risk: narrow"
```

### Task 2: First Admin Page Structure Contract

**Files:**

- Create: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Add the source helper and failing dashboard contract**

```ts
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
})
```

- [ ] **Step 2: Run and record the expected structural failures**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts`

Expected: FAIL on the old dashboard root and hero class.

- [ ] **Step 3: Commit the red contracts with the first migrated slice, not alone**

Do not commit the failing test by itself. Include this file in the Task 3 commit
after the dashboard assertion becomes green. Each later task appends only its
own failing contract immediately before implementing that page group, so every
commit returns the full suite to green.

### Task 3: Admin Dashboard And Theme-Aware Charts

**Files:**

- Modify: `frontend/src/views/admin/Dashboard.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/tests/utils/dashboardCharts.spec.ts`
- Test: `frontend/tests/utils/userAgentChart.spec.ts`

- [ ] **Step 1: Narrow the red source contract to the dashboard**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "dashboard hero"`

Expected: FAIL because `hero-panel` still exists.

- [ ] **Step 2: Replace the hero with compact page context**

Change the root to `<div class="crc-page dashboard-page">`. Replace
`hero-panel` with `.crc-page-header`: title `控制台总览`, concise operational
subtitle, range selection, refresh action, and last-refresh label. Keep
`chartRange`, `handleRangeChange`, refresh data flow, and current API calls.

- [ ] **Step 3: Flatten KPI and chart structure**

Replace color-specific `metric-card--blue/teal/amber/violet` classes with
semantic metric items and status accents. Keep each chart as one bounded
`.crc-surface.chart-panel`; remove `el-card` wrappers around chart sections.
Preserve chart refs, summaries, links, loading directives, and all displayed
fields. Give chart containers explicit heights at desktop and mobile widths.

- [ ] **Step 4: Recreate charts on resolved theme changes**

Import `useTheme` and `buildChartTheme`. Pass the palette into every current
option builder or use it directly for axis, legend, tooltip, split-line, and
series colors. Add one watcher around the existing lifecycle:

```ts
const { resolvedTheme } = useTheme()

watch(resolvedTheme, async () => {
  disposeCharts()
  await nextTick()
  await renderCharts()
})
```

Use the page's actual render/dispose function names after reading the complete
component. If disposal is currently inline, extract only `disposeCharts()`;
do not restructure data fetching or option aggregation.

- [ ] **Step 5: Run dashboard behavior and source tests**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "dashboard hero" && rtk npx vitest run tests/utils/dashboardCharts.spec.ts tests/utils/userAgentChart.spec.ts tests/utils/chartTheme.spec.ts`

Expected: all selected tests pass.

- [ ] **Step 6: Commit dashboard refresh**

```bash
rtk git add frontend/src/views/admin/Dashboard.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(ui): refresh admin dashboard" -m "Constraint: Preserve dashboard data and chart semantics
Confidence: high
Scope-risk: moderate"
```

### Task 4: Model Qualification And Shared Probe Board

**Files:**

- Modify: `frontend/src/views/admin/ModelProbe.vue`
- Modify: `frontend/src/components/ModelProbeBoard.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/tests/utils/modelProbeCharts.spec.ts`
- Test: `frontend/tests/utils/modelProbePolling.spec.ts`

- [ ] **Step 1: Add and run a focused failing probe contract**

Add:

```ts
it('keeps model qualification and probe evidence in compact sections', () => {
  const adminProbe = source('views/admin/ModelProbe.vue')
  const board = source('components/ModelProbeBoard.vue')
  expect(adminProbe).toContain('crc-page model-probe-page')
  expect(board).toContain('probe-page-header')
  expect(board).toContain('crc-table-shell')
  expect(board).not.toContain('summary-card')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "model qualification"`

Expected: FAIL on the old probe hero/summary structure.

- [ ] **Step 2: Refresh admin qualification controls**

Use `.crc-page`, a compact page header, a `.crc-toolbar` qualification command
bar, a responsive metric strip, and a `.crc-table-shell` for evidence. Preserve
qualification levels, category/protocol labels, polling, refresh, result table,
loading state, and API calls exactly.

- [ ] **Step 3: Flatten `ModelProbeBoard`**

Replace the decorative hero with `.probe-page-header`, summary cards with a
responsive metric grid, and chart/channel cards with one surface per bounded
chart or repeated channel item. Keep search, status filter, anomaly sorting,
empty/error rules, tone filtering, all computed data, and channel/model fields.

- [ ] **Step 4: Apply chart palette and theme recreation**

Use `buildChartTheme(resolvedTheme.value)` inside `renderCharts`. On theme
change, dispose both `statusChart` and `coverageChart`, set them to `null`, wait
for the next tick, then call `renderCharts()`. Retain existing resize and
unmount cleanup.

- [ ] **Step 5: Run probe tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "model qualification" && rtk npx vitest run tests/utils/modelProbeCharts.spec.ts tests/utils/modelProbePolling.spec.ts tests/utils/chartTheme.spec.ts && rtk npm run build`

Expected: focused tests and type checking pass.

- [ ] **Step 6: Commit probe refresh**

```bash
rtk git add frontend/src/views/admin/ModelProbe.vue frontend/src/components/ModelProbeBoard.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(ui): refresh model qualification surfaces" -m "Constraint: Preserve probe data and portal tone boundaries
Confidence: high
Scope-risk: moderate"
```

### Task 5: Upstream Management Workbench

**Files:**

- Modify: `frontend/src/views/admin/Upstreams.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/tests/api/admin.spec.ts`

- [ ] **Step 1: Add a failing upstream contract**

```ts
it('uses the responsive upstream management workbench', () => {
  const page = source('views/admin/Upstreams.vue')
  expect(page).toContain('crc-page upstreams-page')
  expect(page).toContain('crc-page-header')
  expect(page).toContain('crc-table-shell')
  expect(page).toContain('drawer-section')
  expect(page).toContain('drawer-footer')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "upstream management"`

Expected: FAIL on the old container and card structure.

- [ ] **Step 2: Migrate header and table without changing row actions**

Use `.crc-page upstreams-page`, a page header with existing create action, and
a `.crc-table-shell` around the table. Retain protocol labels, model/key counts,
compatibility cleanup, protected parameter display, edit action, load behavior,
and fixed desktop action column.

- [ ] **Step 3: Structure the existing drawer with sections**

Keep the same form object, rules, create/edit modes, key input, model fetch,
cost configuration, context parameters, tabs/tables, and submit functions.
Group related fields with `.drawer-section` headings and separators. Keep one
sticky `.drawer-footer` with cancel and primary submit actions. Do not put cards
inside the drawer.

- [ ] **Step 4: Add responsive rules**

At 767px, make the drawer full width, reduce form label width by switching to
top labels, stack model input actions, and keep inner tables horizontally
scrollable. Do not hide business fields.

- [ ] **Step 5: Run focused tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "upstream management" && rtk npx vitest run tests/api/admin.spec.ts && rtk npm run build`

Expected: tests pass and API request shapes are unchanged.

- [ ] **Step 6: Commit upstream refresh**

```bash
rtk git add frontend/src/views/admin/Upstreams.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(ui): refresh upstream management" -m "Constraint: Preserve upstream form and nested configuration semantics
Confidence: high
Scope-risk: moderate"
```

### Task 6: Downstream Management Workbench

**Files:**

- Modify: `frontend/src/views/admin/Downstreams.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/tests/utils/keyUtils.spec.ts`
- Test: `frontend/tests/api/admin.spec.ts`

- [ ] **Step 1: Add a failing downstream contract**

```ts
it('uses the responsive downstream management workbench', () => {
  const page = source('views/admin/Downstreams.vue')
  expect(page).toContain('crc-page downstreams-page')
  expect(page).toContain('crc-toolbar downstream-filters')
  expect(page).toContain('crc-table-shell')
  expect(page).toContain('drawer-footer')
  expect(page).toContain('rotate-key-dialog')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "downstream management"`

Expected: FAIL on old wrapper/filter classes.

- [ ] **Step 2: Migrate page header, filters, and table**

Use the same workbench primitives as upstreams. Preserve search/status filters,
masked/plaintext/legacy key states, limits, lifecycle, enabled status, row
actions, pagination if present, and current computed filtering. Use icon actions
with accessible labels where the meaning is familiar; keep text for ambiguous
commands.

- [ ] **Step 3: Refresh drawer and rotation dialog**

Keep create/edit rules, expiry semantics, IP/model limits, secret visibility,
rotation behavior, and storage semantics. Use drawer sections and sticky footer.
Give the rotation result one bounded security surface, a copy icon with tooltip,
and a stable mobile dialog width. Keep destructive warning text explicit.

- [ ] **Step 4: Run downstream behavior tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "downstream management" && rtk npx vitest run tests/utils/keyUtils.spec.ts tests/api/admin.spec.ts && rtk npm run build`

Expected: all pass; key behavior is unchanged.

- [ ] **Step 5: Commit downstream refresh**

```bash
rtk git add frontend/src/views/admin/Downstreams.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(ui): refresh downstream management" -m "Constraint: Preserve key masking, rotation, quota, and expiry behavior
Confidence: high
Scope-risk: moderate"
```

### Task 7: Operational Logs Workbench

**Files:**

- Modify: `frontend/src/views/admin/Logs.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/tests/utils/logDisplay.spec.ts`
- Test: `frontend/tests/utils/errorDisplay.spec.ts`

- [ ] **Step 1: Add a failing logs contract**

```ts
it('keeps log filters and evidence dense and responsive', () => {
  const page = source('views/admin/Logs.vue')
  expect(page).toContain('crc-page logs-page')
  expect(page).toContain('crc-toolbar logs-filters')
  expect(page).toContain('logs-filter-disclosure')
  expect(page).toContain('crc-table-shell')
  expect(page).toContain('log-summary-strip')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "log filters"`

Expected: FAIL on the old card/inline-form shell.

- [ ] **Step 2: Refresh filters and summary without changing queries**

Keep every current filter field, quick category, reset, pagination reset, API
parameter, summary value, and error label. Present filters in a wrappable
desktop toolbar and an Element Plus collapse/disclosure region on mobile. The
active quick filter uses accent-soft styling and a visible label, not color alone.

- [ ] **Step 3: Stabilize the table and long content**

Wrap the existing table in `.crc-table-shell`; retain columns and fixed action
behavior. Use semantic typography for API, tokens, categories, statuses, and
errors. Keep overflow tooltips/details for long values. Set a stable minimum
table region height during loading; show distinct empty-filter and API-error
states without removing the filter/summary context.

- [ ] **Step 4: Run log tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "log filters" && rtk npx vitest run tests/utils/logDisplay.spec.ts tests/utils/errorDisplay.spec.ts && rtk npm run build`

Expected: all pass with unchanged classification behavior.

- [ ] **Step 5: Commit logs refresh**

```bash
rtk git add frontend/src/views/admin/Logs.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(ui): refresh operational logs workbench" -m "Constraint: Preserve filters, fields, and error categories
Confidence: high
Scope-risk: moderate"
```

### Task 8: Troubleshooting And Compatibility Evidence

**Files:**

- Modify: `frontend/src/views/admin/Troubleshooting.vue`
- Modify: `frontend/src/components/TroubleshootingCenter.vue`
- Modify: `frontend/src/components/CompatibilityMatrixPanel.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/tests/utils/troubleshooting.spec.ts`

- [ ] **Step 1: Add a focused failing evidence-surface contract**

```ts
it('uses unframed troubleshooting sections and one matrix tool surface', () => {
  const page = source('views/admin/Troubleshooting.vue')
  const center = source('components/TroubleshootingCenter.vue')
  const matrix = source('components/CompatibilityMatrixPanel.vue')
  expect(page).toContain('crc-page troubleshooting-page')
  expect(center).toContain('evidence-section')
  expect(matrix).toContain('compatibility-matrix-panel crc-surface')
  expect(center).not.toContain('<el-card')
  expect(matrix).not.toContain('<el-card')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "troubleshooting sections"`

Expected: FAIL because current components use multiple `el-card` wrappers.

- [ ] **Step 2: Refresh the troubleshooting center**

Replace card wrappers with unframed `.evidence-section` blocks separated by
borders and spacing. Preserve profile selection, model/check controls, run
actions, capability import/export, conflict/resolved summaries, result details,
active requests, loading, and error behavior. Use consistent status labels and
semantic variables; do not hide technical evidence on mobile.

- [ ] **Step 3: Refresh the compatibility matrix**

Use one `.compatibility-matrix-panel.crc-surface` around the bounded matrix tool.
Keep downstream selection, run/copy actions, last-run metadata, expandable
rows, adapter sets, nested check results, and current API events. Make the
toolbar wrap and the matrix scroll inside its surface.

- [ ] **Step 4: Run troubleshooting tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "troubleshooting sections" && rtk npx vitest run tests/utils/troubleshooting.spec.ts && rtk npm run build`

Expected: tests and Vue type checking pass.

- [ ] **Step 5: Commit evidence surfaces**

```bash
rtk git add frontend/src/views/admin/Troubleshooting.vue frontend/src/components/TroubleshootingCenter.vue frontend/src/components/CompatibilityMatrixPanel.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(ui): refresh troubleshooting evidence" -m "Constraint: Preserve compatibility and troubleshooting semantics
Confidence: high
Scope-risk: moderate"
```

### Task 9: Announcement Form

**Files:**

- Modify: `frontend/src/views/admin/Announcement.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/tests/utils/announcement.spec.ts`

- [ ] **Step 1: Add a failing announcement contract**

```ts
it('uses a focused unframed announcement form', () => {
  const page = source('views/admin/Announcement.vue')
  expect(page).toContain('crc-page announcement-page')
  expect(page).toContain('announcement-form-surface')
  expect(page).not.toContain('<el-card')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "announcement form"`

Expected: FAIL because the form is wrapped in a card.

- [ ] **Step 2: Refresh presentation only**

Use a compact page header and one narrow `.announcement-form-surface` section.
Preserve text, severity options, enabled state, version/update metadata, reset,
load/save functions, error extraction, and API payload. Use a stable action row
that does not move when validation or loading messages appear.

- [ ] **Step 3: Run announcement tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts -t "announcement form" && rtk npx vitest run tests/utils/announcement.spec.ts && rtk npm run build`

Expected: all pass.

- [ ] **Step 4: Commit announcement refresh**

```bash
rtk git add frontend/src/views/admin/Announcement.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(ui): refresh announcement management" -m "Constraint: Preserve announcement version and save behavior
Confidence: high
Scope-risk: narrow"
```

### Task 10: Admin Verification

**Files:**

- Verify all admin files and shared components listed above.

- [ ] **Step 1: Run the complete admin structure contract**

Run from `frontend/`: `rtk npx vitest run tests/views/admin-ui.spec.ts`

Expected: all page structure tests pass.

- [ ] **Step 2: Run all frontend tests**

Run from `frontend/`: `rtk npm test`

Expected: all tests pass with zero failures.

- [ ] **Step 3: Run the production build**

Run from `frontend/`: `rtk npm run build`

Expected: Vue type checking and Vite build succeed.

- [ ] **Step 4: Audit forbidden visual patterns in migrated admin files**

Run:

```bash
rtk rg -n 'linear-gradient|radial-gradient|backdrop-filter|box-shadow:' \
  frontend/src/views/admin frontend/src/components/ModelProbeBoard.vue \
  frontend/src/components/TroubleshootingCenter.vue \
  frontend/src/components/CompatibilityMatrixPanel.vue
```

Expected: no gradients or glass effects. Any shadow is restricted to an actual
drawer, dialog, dropdown, or menu and uses a semantic token.

- [ ] **Step 5: Check diff integrity**

Run: `rtk git diff --check && rtk git status --short`

Expected: no whitespace errors and no generated `frontend/dist` changes.
