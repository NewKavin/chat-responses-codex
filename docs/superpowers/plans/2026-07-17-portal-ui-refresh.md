# Portal UI Refresh And Visual QA Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refresh every portal page, complete cross-theme chart behavior, and verify the full console at desktop and mobile sizes in light and dark modes.

**Architecture:** Portal views consume the shared shell, authentication surface, theme state, chart palette, and global page primitives delivered by the first two plans. Business logic and API flows remain in their current views; template and scoped-style migrations remove nested cards, stabilize responsive layouts, and make the Playground a full-height work surface. Source contracts and existing utility/API tests guard behavior, followed by browser-based route-by-route visual QA.

**Tech Stack:** Vue 3, TypeScript, Element Plus, ECharts 5, CSS custom properties, Vitest, Vite, Google Chrome for manual/headless visual checks.

---

## Prerequisites

Complete, test, and commit:

- `docs/superpowers/plans/2026-07-17-console-ui-foundation.md`
- `docs/superpowers/plans/2026-07-17-admin-ui-refresh.md`

The portal plan assumes the shared `AppShell`, `AuthShell`, theme composable,
semantic CSS variables, global `.crc-*` layout classes, and `chartTheme.ts`
already exist.

## File Map

**Create:**

- `frontend/tests/views/portal-ui.spec.ts` - structural contracts for every portal view.

**Modify:**

- `frontend/src/views/portal/Overview.vue`
- `frontend/src/views/portal/QuotaDetails.vue`
- `frontend/src/views/portal/UsageHistory.vue`
- `frontend/src/views/portal/Integration.vue`
- `frontend/src/views/portal/Playground.vue`
- `frontend/src/views/portal/KeyManagement.vue`
- `frontend/src/views/portal/ModelProbe.vue`
- `frontend/tests/views/portal-integration.spec.ts`

`Portal.vue` and `PortalLogin.vue` are completed by the foundation plan and are
verified again here but not redesigned a second time.

### Task 1: First Portal Page Structure Contract

**Files:**

- Create: `frontend/tests/views/portal-ui.spec.ts`

- [ ] **Step 1: Add the source helper and failing overview contract**

```ts
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
})
```

- [ ] **Step 2: Run and record expected failures**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts`

Expected: FAIL on the old overview and quota-detail structure. Do not commit the
red test by itself. Include it in Task 2 after the quota views are green. Each
later task appends only its own contract immediately before implementing that
page group.

### Task 2: Portal Overview And Quota Details

**Files:**

- Modify: `frontend/src/views/portal/Overview.vue`
- Modify: `frontend/src/views/portal/QuotaDetails.vue`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Test: `frontend/tests/utils/percentage.spec.ts`
- Test: `frontend/tests/utils/portalQuotaModels.spec.ts`
- Test: `frontend/tests/api/portal.spec.ts`

- [ ] **Step 1: Run the focused failing quota contract from Task 1**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "flat quota summary"`

Expected: FAIL on nested `summary-card`/`quota-tile` markup.

- [ ] **Step 2: Flatten portal overview presentation**

Use `.crc-page portal-overview-page`, a compact header, and a
`.quota-summary-grid` for request, daily-token, and monthly-token limits. Each
metric is one repeated item, not an `el-card` nested in a summary card. Preserve
all data loading, polling, percentage helpers, reset labels, model counts,
progress values, allowlists, and error behavior.

- [ ] **Step 3: Organize quota and model details as sections**

Replace outer/detail cards with `.quota-detail-section` blocks using headings
and separators. Preserve `activeDetail`, available model filtering, empty model
messages, IP allowlist tags, and current API calls. Progress colors come from
semantic status variables or a semantic function result, not literal page CSS.

- [ ] **Step 4: Apply the same structure to unregistered `QuotaDetails.vue`**

Do not add a route. Keep its request/daily/monthly/model/IP sections and loading
logic, but use the same `.crc-page`, summary grid, progress, tag, and section
classes as `Overview.vue` so the component is ready wherever it is embedded.

- [ ] **Step 5: Run quota/API tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "flat quota summary" && rtk npx vitest run tests/utils/percentage.spec.ts tests/utils/portalQuotaModels.spec.ts tests/api/portal.spec.ts && rtk npm run build`

Expected: all pass with unchanged quota calculations and API payloads.

- [ ] **Step 6: Commit quota refresh**

```bash
rtk git add frontend/src/views/portal/Overview.vue frontend/src/views/portal/QuotaDetails.vue frontend/tests/views/portal-ui.spec.ts
rtk git commit -m "feat(ui): refresh portal quota surfaces" -m "Constraint: Preserve quota and allowlist semantics
Confidence: high
Scope-risk: moderate"
```

### Task 3: Usage History And Theme-Aware Charts

**Files:**

- Modify: `frontend/src/views/portal/UsageHistory.vue`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Test: `frontend/tests/utils/usageHistoryChart.spec.ts`
- Test: `frontend/tests/utils/chartTheme.spec.ts`

- [ ] **Step 1: Add a focused failing history contract**

```ts
it('uses a compact history toolbar and stable chart surfaces', () => {
  const history = source('UsageHistory')
  expect(history).toContain('crc-page usage-history-page')
  expect(history).toContain('crc-toolbar history-toolbar')
  expect(history).toContain('history-chart-grid')
  expect(history).toContain('crc-table-shell')
  expect(history).not.toContain('history-card')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "history toolbar"`

Expected: FAIL on the outer history card and old header.

- [ ] **Step 2: Refresh range controls, charts, and table structure**

Use a page header, `.crc-toolbar history-toolbar`, two bounded chart surfaces,
and `.crc-table-shell` for recent requests. Preserve range selection, refresh,
pagination, sort indicators, token breakdown, current API queries, loading,
empty, and error behavior. Set explicit responsive chart heights.

- [ ] **Step 3: Apply the shared ECharts theme**

Import `useTheme` and `buildChartTheme`. Use the palette for both daily and
token charts. Add one watcher that disposes `dailyChart` and `tokenChart`, sets
them to `null`, waits for the next tick, and invokes the existing chart render
function. Keep current resize and unmount cleanup.

- [ ] **Step 4: Run history/chart tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "history toolbar" && rtk npx vitest run tests/utils/usageHistoryChart.spec.ts tests/utils/chartTheme.spec.ts && rtk npm run build`

Expected: all pass.

- [ ] **Step 5: Commit history refresh**

```bash
rtk git add frontend/src/views/portal/UsageHistory.vue frontend/tests/views/portal-ui.spec.ts
rtk git commit -m "feat(ui): refresh portal usage history" -m "Constraint: Preserve history buckets, sorting, and pagination
Confidence: high
Scope-risk: moderate"
```

### Task 4: Integration And Code Surfaces

**Files:**

- Modify: `frontend/src/views/portal/Integration.vue`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Modify: `frontend/tests/views/portal-integration.spec.ts`
- Test: `frontend/tests/utils/integration.spec.ts`
- Test: `frontend/tests/utils/highlight.spec.ts`

- [ ] **Step 1: Add a failing integration structure contract**

```ts
it('uses flat integration sections and bounded code examples', () => {
  const page = source('Integration')
  expect(page).toContain('crc-page integration-page')
  expect(page).toContain('integration-summary')
  expect(page).toContain('integration-section')
  expect(page).toContain('code-surface')
  expect(page).not.toContain('integration-hero')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "flat integration"`

Expected: FAIL on decorative card names.

- [ ] **Step 2: Flatten summary and compatibility sections**

Replace the hero card with a compact page header and `.integration-summary`.
Use unframed `.integration-section` blocks for compatibility families and model
catalog details. Preserve live catalog loading, status alerts, protocol/auth
labels, model data, and error fallbacks.

- [ ] **Step 3: Refresh tabs, empty state, and copy actions**

Replace the current `tabs-card` with one bounded `.code-surface` containing the
existing tabs and generated snippets. Use an Element Plus copy icon button with
`aria-label` and tooltip; preserve `copyText` and fallback behavior. Code blocks
use theme-safe highlight variables, horizontal overflow, long-word protection,
and a stable copied state.

Update `portal-integration.spec.ts` to expect the same
`data-testid="integration-config-tabs"` on a `<section>` or `<div>` with class
`code-surface`, while retaining all live-catalog and empty-state assertions:

```ts
expect(integrationView).toMatch(
  /<(section|div) v-else data-testid="integration-config-tabs" class="code-surface">/
)
```

- [ ] **Step 4: Run integration/highlight tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "flat integration" && rtk npx vitest run tests/views/portal-integration.spec.ts tests/utils/integration.spec.ts tests/utils/highlight.spec.ts && rtk npm run build`

Expected: all pass with live catalog wiring unchanged.

- [ ] **Step 5: Commit integration refresh**

```bash
rtk git add frontend/src/views/portal/Integration.vue frontend/tests/views/portal-ui.spec.ts frontend/tests/views/portal-integration.spec.ts
rtk git commit -m "feat(ui): refresh portal integration guide" -m "Constraint: Preserve live catalog and copy behavior
Confidence: high
Scope-risk: moderate"
```

### Task 5: Playground Workspace And Settings

**Files:**

- Modify: `frontend/src/views/portal/Playground.vue`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Test: `frontend/tests/utils/playground.spec.ts`

- [ ] **Step 1: Add a failing workspace/settings contract**

```ts
it('uses icon controls and a mobile settings drawer', () => {
  const playground = source('Playground')
  expect(playground).toContain('playground-workspace')
  expect(playground).toContain('settings-panel')
  expect(playground).toContain('settingsDrawerOpen')
  expect(playground).toContain('<el-drawer')
  expect(playground).toContain('aria-label="打开模型设置"')
  expect(playground).not.toContain("sidebarCollapsed ? '▶' : '◀'")
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "mobile settings drawer"`

Expected: FAIL on the old text triangle and absent drawer.

- [ ] **Step 2: Establish the stable full-height workspace**

Rename the root to `.playground-workspace`. Desktop uses a 280-pixel bounded
settings panel plus a flexible chat surface inside the shell's available height.
Replace the text triangle with familiar Element Plus expand/collapse icons,
36x36 controls, tooltips, and accessible labels. Preserve desktop collapse state.

- [ ] **Step 3: Reuse one settings template on mobile**

Add `settingsDrawerOpen = ref(false)` and a mobile settings icon in the chat
toolbar. Render the same model, temperature, max-token, reasoning-effort, and
clear controls in an Element Plus drawer below 768px. Extract a small local
settings component only if needed to avoid duplicate bindings; do not move API
or message logic. Close the drawer after the user applies/selects settings.

- [ ] **Step 4: Apply semantic settings styles**

Use token-based surfaces, borders, labels, slider marks, alerts, and disabled
states. The drawer uses viewport-constrained width and the desktop panel never
shrinks the chat below its minimum usable width.

- [ ] **Step 5: Run playground utility/source tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "mobile settings drawer" && rtk npx vitest run tests/utils/playground.spec.ts && rtk npm run build`

Expected: all pass with request-building behavior unchanged.

- [ ] **Step 6: Commit workspace/settings refresh**

```bash
rtk git add frontend/src/views/portal/Playground.vue frontend/tests/views/portal-ui.spec.ts
rtk git commit -m "feat(ui): refresh playground workspace" -m "Constraint: Preserve model settings and request construction
Confidence: high
Scope-risk: moderate"
```

### Task 6: Playground Messages And Composer

**Files:**

- Modify: `frontend/src/views/portal/Playground.vue`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Test: `frontend/tests/utils/playground.spec.ts`
- Test: `frontend/tests/utils/highlight.spec.ts`

- [ ] **Step 1: Add a failing message/composer contract**

```ts
it('keeps message content and composer actions in stable bounded regions', () => {
  const playground = source('Playground')
  expect(playground).toContain('playground-message-stream')
  expect(playground).toContain('message-reasoning')
  expect(playground).toContain('playground-composer')
  expect(playground).toContain('composer-actions')
  expect(playground).toContain('overflow-wrap: anywhere')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "message content"`

Expected: FAIL on old message/composer class names and overflow rules.

- [ ] **Step 2: Refresh empty and message states**

Use a restrained empty state with a familiar chat icon and concise existing
copy. Give user and assistant messages distinct but neutral alignment and
surfaces. Preserve markdown rendering, reasoning details, file tags, role labels,
streaming status, error details, usage, timing, and scroll behavior. Use semantic
status colors and explicit dark-theme rules for markdown/code/tables.

- [ ] **Step 3: Stabilize the composer**

Use `.playground-composer` with a bounded textarea region and separate
`.composer-actions`. Keep upload, attachment removal, send/cancel/loading,
keyboard behavior, and disabled logic. Use icon buttons with labels/tooltips for
attachment and send where familiar. Reserve action width so loading does not
resize or overlap entered text.

- [ ] **Step 4: Add robust overflow and mobile rules**

Apply `min-width: 0`, `overflow-wrap: anywhere`, code-block horizontal scroll,
and table overflow to rendered message content. On mobile, reduce message/avatar
gaps, stack metadata only when needed, and keep the composer above safe viewport
insets without using fixed positioning over content.

- [ ] **Step 5: Run playground/highlight tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "message content" && rtk npx vitest run tests/utils/playground.spec.ts tests/utils/highlight.spec.ts && rtk npm run build`

Expected: all pass.

- [ ] **Step 6: Commit message/composer refresh**

```bash
rtk git add frontend/src/views/portal/Playground.vue frontend/tests/views/portal-ui.spec.ts
rtk git commit -m "feat(ui): polish playground messages and composer" -m "Constraint: Preserve streaming, reasoning, file, and error behavior
Confidence: high
Scope-risk: moderate"
```

### Task 7: Key Management And Portal Probe Wrapper

**Files:**

- Modify: `frontend/src/views/portal/KeyManagement.vue`
- Modify: `frontend/src/views/portal/ModelProbe.vue`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Test: `frontend/tests/utils/keyUtils.spec.ts`
- Test: `frontend/tests/utils/modelProbeCharts.spec.ts`

- [ ] **Step 1: Add focused failing contracts**

```ts
it('uses focused key security and portal probe surfaces', () => {
  const keys = source('KeyManagement')
  const probe = source('ModelProbe')
  expect(keys).toContain('crc-page key-management-page')
  expect(keys).toContain('key-security-surface')
  expect(keys).toContain('rotate-key-dialog')
  expect(probe).toContain('crc-page portal-model-probe-page')
  expect(probe).toContain('tone="portal"')
})
```

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "key security"`

Expected: FAIL on old outer wrappers.

- [ ] **Step 2: Refresh key management presentation**

Use a compact header and one `.key-security-surface` for current key state,
metadata, copy, and rotate entry. Preserve masking, plaintext availability,
clipboard fallback, rotate API, new-key result, close behavior, messages, and
all security text. Use an icon copy action with tooltip and a stable,
viewport-constrained rotation dialog.

- [ ] **Step 3: Align the portal model-probe wrapper**

Use `.crc-page portal-model-probe-page` with the existing `ModelProbeBoard`
`tone="portal"`. Preserve polling, errors, loading, cleanup, and portal API.
Do not expose admin qualification commands or admin-only data.

- [ ] **Step 4: Run key/probe tests and build**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts -t "key security" && rtk npx vitest run tests/utils/keyUtils.spec.ts tests/utils/modelProbeCharts.spec.ts && rtk npm run build`

Expected: all pass.

- [ ] **Step 5: Commit final portal page refresh**

```bash
rtk git add frontend/src/views/portal/KeyManagement.vue frontend/src/views/portal/ModelProbe.vue frontend/tests/views/portal-ui.spec.ts
rtk git commit -m "feat(ui): refresh portal security and probe views" -m "Constraint: Preserve portal visibility and key semantics
Confidence: high
Scope-risk: moderate"
```

### Task 8: Automated Cross-Page Verification

**Files:**

- Verify all frontend source and tests.

- [ ] **Step 1: Run the complete portal structure contract**

Run from `frontend/`: `rtk npx vitest run tests/views/portal-ui.spec.ts tests/views/portal-integration.spec.ts`

Expected: all portal structure and live-catalog assertions pass.

- [ ] **Step 2: Run all frontend tests**

Run from `frontend/`: `rtk npm test`

Expected: all test files pass with zero failures.

- [ ] **Step 3: Run production type checking and build**

Run from `frontend/`: `rtk npm run build`

Expected: `vue-tsc` and Vite finish successfully.

- [ ] **Step 4: Audit forbidden patterns and accidental hard-coded theme colors**

Run:

```bash
rtk rg -n 'linear-gradient|radial-gradient|backdrop-filter' frontend/src
rtk rg -n '#[0-9a-fA-F]{3,8}' frontend/src/views frontend/src/components
```

Expected: no gradients or glass effects. Remaining component hex colors are
reviewed and removed unless they are data-driven ECharts values supplied by
`chartTheme.ts`; semantic UI colors live in `styles/tokens.css`.

- [ ] **Step 5: Check worktree integrity**

Run: `rtk git diff --check && rtk git status --short`

Expected: no whitespace errors, no generated `frontend/dist` changes, and only
intentional UI source/test changes.

### Task 9: Desktop And Mobile Visual QA

**Files:**

- Do not commit screenshots or temporary browser profiles.

- [ ] **Step 1: Start the frontend on a dedicated port**

Run from `frontend/`: `rtk npm run dev -- --host 127.0.0.1 --port 5174`

Expected: Vite serves `http://127.0.0.1:5174/`. Keep this session running until
all browser checks complete. If the API server is not already on port 3001,
start it using the repository's normal development configuration before testing
authenticated data flows.

- [ ] **Step 2: Check both public login routes in four visual states**

Open `/\#/admin/login` and `/\#/portal/login` at 1440x900 and 390x844. For each
viewport, select light, dark, and follow-system modes and reload once.

Expected: no wrong-theme flash, gradient, overlap, clipped form text, shifting
submit button, page-level horizontal scroll, or inaccessible theme control.

- [ ] **Step 3: Check every authenticated admin route**

After signing in with the existing development admin account, inspect:

```text
/#/admin/dashboard
/#/admin/model-probe
/#/admin/upstreams
/#/admin/downstreams
/#/admin/logs
/#/admin/troubleshooting
/#/admin/announcement
```

At 1440x900 and 390x844, verify both resolved themes, desktop collapse, mobile
drawer open/close after navigation, title/account/theme actions, filters,
tables, fixed columns, chart pixels, drawers, dialogs, expanded evidence, empty,
loading, and API-error states.

- [ ] **Step 4: Check every authenticated portal route**

After signing in with an existing development employee/key pair, inspect:

```text
/#/portal
/#/portal/model-probe
/#/portal/history
/#/portal/integration
/#/portal/playground
/#/portal/key
```

At both viewports and resolved themes, verify navigation, announcement dialog,
quota progress, charts, code/copy surfaces, model settings drawer, long markdown,
reasoning details, attachments, composer actions, key dialog, and empty/error
states. Confirm no admin-only probe or troubleshooting data appears.

- [ ] **Step 5: Capture headless login screenshots as durable local evidence**

With Vite running, create a temporary output directory outside the repository:

```bash
rtk mkdir -p /tmp/chat2responses-ui-qa
rtk google-chrome --headless --disable-gpu --hide-scrollbars \
  --window-size=1440,900 \
  --screenshot=/tmp/chat2responses-ui-qa/admin-login-light.png \
  http://127.0.0.1:5174/#/admin/login
rtk google-chrome --headless --disable-gpu --hide-scrollbars --force-dark-mode \
  --window-size=390,844 \
  --screenshot=/tmp/chat2responses-ui-qa/portal-login-dark-mobile.png \
  http://127.0.0.1:5174/#/portal/login
```

Expected: both PNG files are non-empty and show the full panel within the
viewport. Inspect them with the available image viewer; do not add them to git.

- [ ] **Step 6: Fix every visual defect before completion**

For each overlap, clipping, blank chart, theme leak, nested card, or unstable
control found, add the narrowest relevant source assertion or utility test,
reproduce the failure, apply the scoped fix, and rerun the affected focused test
plus `rtk npm run build`.

- [ ] **Step 7: Run final verification after all QA fixes**

Run:

```bash
rtk npm test
rtk npm run build
```

Run the two commands above from `frontend/`, then run these from the repository root:

```bash
rtk git diff --check
rtk git status --short --branch
```

Expected: zero test/build failures, no whitespace errors, no generated build
changes, and a clean `ui` worktree after the final intended commit.
