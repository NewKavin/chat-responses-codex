# Adaptive Full-Width Console Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every business page fill the AppShell content area, enlarge account drawers with viewport-based proportions, and reorganize troubleshooting into a responsive workflow-first layout.

**Architecture:** Shared page width and account drawer proportions live in the global layout layer. Troubleshooting uses a named CSS container around a two-track Grid so it can collapse based on available content width rather than viewport width; Capability tables and the compatibility matrix remain full-width sections with their own horizontal scroll boundaries.

**Tech Stack:** Vue 3 SFCs, Element Plus, CSS Grid and container queries, Vitest structural contracts, Vite, Chrome DevTools Protocol.

---

### Task 1: Full-width pages and proportional account drawers

**Files:**
- Modify: `frontend/tests/views/ui-foundation.spec.ts`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Modify: `frontend/src/styles/base.css`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Modify: `frontend/src/views/admin/Downstreams.vue`

- [ ] **Step 1: Write the failing shared-width and drawer contracts**

Add this test to `frontend/tests/views/ui-foundation.spec.ts`:

```ts
it('lets business pages fill the shell while account drawers follow viewport proportions', () => {
  const base = readSource('../../src/styles/base.css')
  const pageRule = base.match(/\.crc-page\s*\{([\s\S]*?)\n\}/)?.[1]

  expect(pageRule).toContain('width: 100%;')
  expect(pageRule).toContain('min-width: 0;')
  expect(pageRule).not.toContain('max-width')
  expect(base).toContain('--account-drawer-width: 72vw;')
  expect(base).toContain('--account-drawer-width: 64vw;')
  expect(base).toContain('--account-drawer-width: 86vw;')
  expect(base).toContain('--account-drawer-width: 100vw;')
})
```

Extend the upstream and downstream tests in `frontend/tests/views/admin-ui.spec.ts`:

```ts
expect(page).toContain('size="var(--account-drawer-width)"')
expect(page).toContain('upstream-account-drawer')
```

```ts
expect(page).toContain('size="var(--account-drawer-width)"')
expect(page).toContain('downstream-account-drawer')
```

- [ ] **Step 2: Run the focused contracts and verify RED**

Run:

```bash
rtk npm test -- tests/views/ui-foundation.spec.ts tests/views/admin-ui.spec.ts
```

Expected: FAIL because `.crc-page` still contains `max-width: 1600px`, the drawer custom properties do not exist, and both drawers still use fixed pixel sizes.

- [ ] **Step 3: Implement the global fluid width and responsive drawer variables**

Change the shared rules in `frontend/src/styles/base.css` to:

```css
.crc-page {
  width: 100%;
  min-width: 0;
}

.crc-table-shell {
  width: 100%;
  min-width: 0;
  max-width: 100%;
  overflow-x: auto;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
}

.upstream-account-drawer {
  --account-drawer-width: 72vw;
}

.downstream-account-drawer {
  --account-drawer-width: 64vw;
}

@media (max-width: 1199px) {
  .upstream-account-drawer,
  .downstream-account-drawer {
    --account-drawer-width: 86vw;
  }
}
```

Inside the existing `@media (max-width: 767px)` block, add:

```css
.upstream-account-drawer,
.downstream-account-drawer {
  --account-drawer-width: 100vw;
}
```

Update the upstream drawer:

```vue
<el-drawer
  v-model="dialogVisible"
  :title="dialogMode === 'create' ? '创建上游' : '编辑上游'"
  direction="rtl"
  size="var(--account-drawer-width)"
  :destroy-on-close="false"
  class="form-drawer upstream-account-drawer"
>
```

Update the downstream drawer:

```vue
<el-drawer
  v-model="dialogVisible"
  :title="dialogMode === 'create' ? '创建下游' : '编辑下游'"
  direction="rtl"
  size="var(--account-drawer-width)"
  :destroy-on-close="false"
  class="form-drawer downstream-account-drawer"
>
```

- [ ] **Step 4: Run the focused contracts and verify GREEN**

Run:

```bash
rtk npm test -- tests/views/ui-foundation.spec.ts tests/views/admin-ui.spec.ts
```

Expected: both test files pass.

- [ ] **Step 5: Commit the shared layout change**

```bash
rtk git add frontend/tests/views/ui-foundation.spec.ts frontend/tests/views/admin-ui.spec.ts frontend/src/styles/base.css frontend/src/views/admin/Upstreams.vue frontend/src/views/admin/Downstreams.vue
rtk git commit -m "fix(ui): make console pages and account drawers adaptive"
```

### Task 2: Workflow-first troubleshooting workspace

**Files:**
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Modify: `frontend/src/components/TroubleshootingCenter.vue`

- [ ] **Step 1: Write the failing troubleshooting structure contract**

Extend the troubleshooting test in `frontend/tests/views/admin-ui.spec.ts`:

```ts
expect(center).toContain('diagnostic-workspace-container')
expect(center).toContain('diagnostic-workspace')
expect(center).toContain('diagnostic-results-stack')
expect(center).toContain('container-name: diagnostic-workspace;')
expect(center).toContain('grid-template-columns: minmax(320px, 0.75fr) minmax(560px, 1.25fr);')
expect(center).toContain('@container diagnostic-workspace (max-width: 960px)')
expect(center).not.toContain('<el-row')
expect(center).not.toContain('<el-col')

const workspaceStart = center.indexOf('<div class="diagnostic-workspace-container">')
const capabilityStart = center.indexOf('class="evidence-section capability-panel"')
expect(workspaceStart).toBeGreaterThan(-1)
expect(capabilityStart).toBeGreaterThan(workspaceStart)
expect(center).toMatch(
  /<\/div>\s*<\/div>\s*<section v-if="admin && exportCapabilities && importCapabilities" class="evidence-section capability-panel">/
)
```

- [ ] **Step 2: Run the focused contract and verify RED**

Run:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
```

Expected: FAIL because the component still uses Element Plus `el-row`/`el-col`, Capability is inside the narrow column, and no named container exists.

- [ ] **Step 3: Replace the fixed column grid and move Capability to full width**

In `frontend/src/components/TroubleshootingCenter.vue`:

1. Move the complete existing block beginning with
   `<section v-if="admin && exportCapabilities && importCapabilities" class="evidence-section capability-panel">`
   and ending at its matching `</section>` to immediately after the diagnostic workspace closing tags and before
   the troubleshooting-center closing `</div>`. Do not change any handler, binding, column, or resolved-detail
   markup inside the moved section.

2. Replace:

```vue
<el-row :gutter="16">
  <el-col :xs="24" :lg="8">
```

with:

```vue
<div class="diagnostic-workspace-container">
  <div class="diagnostic-workspace">
```

The existing `diagnostic-config-section` becomes the first direct child of `diagnostic-workspace`.

3. After the `diagnostic-config-section` closing tag, replace:

```vue
</el-col>

<el-col :xs="24" :lg="16">
```

with:

```vue
<div class="diagnostic-results-stack">
```

The existing `diagnostic-results-section` and `active-panel` remain in their current order inside this stack.

4. After the `active-panel` closing tag, replace:

```vue
  </el-col>
</el-row>
```

with:

```vue
    </div>
  </div>
</div>
```

5. Add these component styles:

```css
.diagnostic-workspace-container {
  container-name: diagnostic-workspace;
  container-type: inline-size;
}

.diagnostic-workspace {
  display: grid;
  grid-template-columns: minmax(320px, 0.75fr) minmax(560px, 1.25fr);
  gap: 24px;
  align-items: start;
}

.diagnostic-results-stack {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 16px;
}

.diagnostic-config-section,
.diagnostic-results-section,
.active-panel,
.capability-panel {
  min-width: 0;
}

.result-toolbar,
.result-title {
  flex-wrap: wrap;
}

.result-toolbar .el-button {
  flex: 0 0 auto;
}

@container diagnostic-workspace (max-width: 960px) {
  .diagnostic-workspace {
    grid-template-columns: minmax(0, 1fr);
  }
}
```

Do not change the existing mobile rules for `.check-group`, `.capability-actions`, or Capability buttons.

- [ ] **Step 4: Run the focused contract and verify GREEN**

Run:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
```

Expected: PASS with Capability outside the adaptive diagnostic workspace.

- [ ] **Step 5: Commit the troubleshooting workspace**

```bash
rtk git add frontend/tests/views/admin-ui.spec.ts frontend/src/components/TroubleshootingCenter.vue
rtk git commit -m "fix(ui): reorganize troubleshooting around adaptive workflow"
```

### Task 3: Container-responsive compatibility matrix

**Files:**
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Modify: `frontend/src/components/CompatibilityMatrixPanel.vue`

- [ ] **Step 1: Write the failing matrix responsiveness contract**

Add to the troubleshooting test in `frontend/tests/views/admin-ui.spec.ts`:

```ts
expect(matrix).toContain('container-name: compatibility-matrix;')
expect(matrix).toContain('@container compatibility-matrix (max-width: 860px)')
expect(matrix).toContain('max-width: 100%;')
```

- [ ] **Step 2: Run the focused contract and verify RED**

Run:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
```

Expected: FAIL because the matrix uses only a viewport breakpoint at 767px.

- [ ] **Step 3: Add matrix container behavior**

Update `frontend/src/components/CompatibilityMatrixPanel.vue`:

```css
.compatibility-matrix-panel {
  container-name: compatibility-matrix;
  container-type: inline-size;
  min-width: 0;
  max-width: 100%;
  padding: 20px;
}

.panel-head {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.summary-tags {
  flex: 0 0 auto;
}

@container compatibility-matrix (max-width: 860px) {
  .panel-head,
  .matrix-toolbar {
    align-items: stretch;
    flex-direction: column;
  }

  .matrix-toolbar .el-button {
    margin-left: 0;
  }

  .downstream-select {
    width: 100%;
  }
}
```

Do not change the existing `matrix-table-shell` and `check-table-shell` overflow declarations. Remove
`.panel-head`, `.matrix-toolbar`, `.matrix-toolbar .el-button`, and `.downstream-select` responsive
declarations from the 767px viewport block because the named container query now owns those rules.

- [ ] **Step 4: Run the focused contract and verify GREEN**

Run:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
```

Expected: PASS.

- [ ] **Step 5: Commit the matrix behavior**

```bash
rtk git add frontend/tests/views/admin-ui.spec.ts frontend/src/components/CompatibilityMatrixPanel.vue
rtk git commit -m "fix(ui): adapt compatibility controls to content width"
```

### Task 4: Full verification and browser QA

**Files:**
- Verify: `frontend/src/styles/base.css`
- Verify: `frontend/src/views/admin/Logs.vue`
- Verify: `frontend/src/views/admin/Upstreams.vue`
- Verify: `frontend/src/views/admin/Downstreams.vue`
- Verify: `frontend/src/components/TroubleshootingCenter.vue`
- Verify: `frontend/src/components/CompatibilityMatrixPanel.vue`

- [ ] **Step 1: Run the complete frontend suite**

```bash
rtk npm test
```

Expected: all Vitest files and tests pass.

- [ ] **Step 2: Run type checking and the production build**

```bash
rtk npm run build
```

Expected: `vue-tsc` and Vite exit successfully.

- [ ] **Step 3: Run static layout checks**

```bash
rtk git diff --check
rtk rg -n -i "gradient\(" frontend/src
rtk rg -n "#[0-9A-Fa-f]{6}\b|rgba?\(" frontend/src/views frontend/src/components
```

Expected: `git diff --check` passes; the two `rg` commands return no matches.

- [ ] **Step 4: Verify desktop and mobile layouts in Chrome**

Use the current Vite server and authenticated local admin session. Capture and inspect:

- `2560x1440` light: admin dashboard, logs, troubleshooting, upstream drawer, downstream drawer, portal overview;
- `1440x900` dark: logs, troubleshooting, both account drawers;
- `390x844` dark: logs, troubleshooting, both account drawers.

For every route assert:

```js
document.documentElement.scrollWidth <= document.documentElement.clientWidth + 2
```

Also confirm:

- normal pages use the AppShell width instead of a centered 1600px strip;
- Logs filter, summary, table shell, and pagination span the content area;
- Capability appears below the diagnostic workspace at full width;
- matrix and log tables scroll only inside their own shells;
- upstream drawer is 72vw on wide desktop, downstream is 64vw, both are 86vw at medium width and 100vw on mobile;
- no control overlap, clipped titles, or theme leakage.

- [ ] **Step 5: Request independent code review**

Dispatch a clean-context reviewer for the implementation range. Fix all Critical and Important findings before continuing.

- [ ] **Step 6: Re-run final verification after review fixes**

```bash
rtk npm test
rtk npm run build
rtk git diff --check
```

Expected: all commands pass with fresh output.
