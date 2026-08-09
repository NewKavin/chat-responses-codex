# Reasoning Probe Results Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move one-click reasoning-level discovery into a dedicated in-page tab while keeping the model status view compact.

**Architecture:** Keep all state and API behavior in the existing admin model-probe view. Add one local tab-selection ref, place existing status and reasoning sections into separate Element Plus tab panes, and switch to the reasoning pane when a capability probe starts.

**Tech Stack:** Vue 3 Composition API, TypeScript, Element Plus, Vitest, Vite

---

### Task 1: Lock The Tab Contract With A Failing View Test

**Files:**
- Modify: `frontend/tests/views/admin-ui.spec.ts:20`
- Test: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Extend the existing model-probe structure test**

Add these assertions to `keeps model qualification and probe evidence in compact sections`:

```ts
expect(adminProbe).toContain('<el-tabs v-model="activeProbeTab" class="model-probe-tabs">')
expect(adminProbe).toContain('<el-tab-pane label="模型状态" name="status">')
expect(adminProbe).toContain('<el-tab-pane label="思考档位" name="reasoning">')
expect(adminProbe).toContain('const activeProbeTab = ref<ProbeTab>(\'status\')')
expect(adminProbe).toContain("activeProbeTab.value = 'reasoning'")
expect(adminProbe).toContain('description="暂无思考档位探测结果"')
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```bash
rtk npm --prefix frontend test -- tests/views/admin-ui.spec.ts
```

Expected: FAIL because `ModelProbe.vue` does not yet contain the tab markup or `activeProbeTab` state.

- [ ] **Step 3: Preserve the red-test output as the TDD checkpoint**

Confirm the failure names the new tab assertion rather than an unrelated dependency, syntax, or environment error. Fix only the test if the failure is malformed; do not change production code before this checkpoint is established.

### Task 2: Implement The In-Page Status And Reasoning Tabs

**Files:**
- Modify: `frontend/src/views/admin/ModelProbe.vue:39-163`
- Modify: `frontend/src/views/admin/ModelProbe.vue:219-223`
- Modify: `frontend/src/views/admin/ModelProbe.vue:486-657`
- Test: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Add typed tab state**

Place the local type and ref with the other top-level view state:

```ts
type ProbeTab = 'status' | 'reasoning'

const activeProbeTab = ref<ProbeTab>('status')
```

- [ ] **Step 2: Switch to reasoning results as soon as probing starts**

At the beginning of `runCapabilityProbe`, before resetting progress, add:

```ts
activeProbeTab.value = 'reasoning'
```

This switch happens only for an explicit one-click probe. The discovery fetch on page mount must not change the selected tab.

- [ ] **Step 3: Split the existing page body into tab panes**

Keep the command bar above the tabs. Move the complete existing
`ModelProbeBoard` node and complete `qualificationResult` section, unchanged,
inside this opening wrapper:

```vue
<el-tabs v-model="activeProbeTab" class="model-probe-tabs">
  <el-tab-pane label="模型状态" name="status">
    <div class="model-probe-tab-panel">
```

Immediately after the qualification section, close the status pane and open
the reasoning pane:

```vue
    </div>
  </el-tab-pane>

  <el-tab-pane label="思考档位" name="reasoning">
    <div class="model-probe-tab-panel reasoning-probe-panel">
```

Move the complete existing `capability-probe-progress` block followed by the
complete existing `capability-probe-results` section, unchanged, into that
reasoning wrapper. Immediately after the results section, insert the empty
state and close the wrappers:

```vue
      <el-empty
        v-else-if="!probingCapabilities"
        class="capability-probe-empty"
        :image-size="64"
        description="暂无思考档位探测结果"
      />
    </div>
  </el-tab-pane>
</el-tabs>
```

The `v-else-if` empty state must immediately follow the capability-results
section, so it appears only when there are no model or route results and no
probe is running.

- [ ] **Step 4: Style the tab container and remove the nested result panel treatment**

Add focused styles using existing design tokens:

```css
.model-probe-tabs,
.model-probe-tab-panel {
  min-width: 0;
}

.model-probe-tabs :deep(.el-tabs__header) {
  margin: 0;
}

.model-probe-tabs :deep(.el-tabs__nav-wrap::after) {
  height: 1px;
  background-color: var(--crc-border);
}

.model-probe-tabs :deep(.el-tabs__item) {
  height: 42px;
  color: var(--crc-text-muted);
  font-weight: 600;
}

.model-probe-tabs :deep(.el-tabs__item.is-active) {
  color: var(--crc-accent);
}

.model-probe-tabs :deep(.el-tabs__content) {
  padding-top: 16px;
  overflow: visible;
}

.model-probe-tab-panel {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.capability-probe-empty {
  min-height: 240px;
}
```

Change `.capability-probe-progress` margins to `0`, and reduce `.capability-probe-results` to an unframed container with `min-width: 0`. Retain responsive table shells for narrow-screen horizontal scrolling.

- [ ] **Step 5: Run the focused test and type checker**

Run:

```bash
rtk npm --prefix frontend test -- tests/views/admin-ui.spec.ts
rtk npm --prefix frontend run type-check
```

Expected: both commands PASS.

- [ ] **Step 6: Commit the tested UI change**

Run:

```bash
rtk git add frontend/tests/views/admin-ui.spec.ts frontend/src/views/admin/ModelProbe.vue docs/superpowers/plans/2026-08-09-reasoning-probe-results-tab.md
rtk git commit -m "fix(admin): separate reasoning probe results"
```

Expected: one commit containing the plan, regression test, and view implementation.

### Task 3: Build And Inspect The Responsive Page

**Files:**
- Verify: `frontend/src/views/admin/ModelProbe.vue`

- [ ] **Step 1: Run the complete frontend test and production build**

Run:

```bash
rtk npm --prefix frontend test
rtk npm --prefix frontend run build
```

Expected: all Vitest suites pass and Vite produces `frontend/dist` without type or build errors.

- [ ] **Step 2: Start a local Vite server without replacing production**

Run:

```bash
rtk npm --prefix frontend run dev -- --host 127.0.0.1 --port 4173
```

Expected: Vite reports `http://127.0.0.1:4173/`. If that port is occupied, select the next free local port.

- [ ] **Step 3: Inspect desktop and mobile layouts with Playwright**

Open `/admin/model-probe` at 1440x900 and 390x844. Verify:

- the command bar and both tab labels are visible;
- the default `模型状态` panel is shown;
- the `思考档位` empty or result state fits its container;
- route tables scroll inside their shell and do not expand the viewport;
- buttons, tabs, text, progress, and tables do not overlap.

- [ ] **Step 4: Run final repository checks**

Run:

```bash
rtk git diff --check
rtk git status --short --branch
```

Expected: no whitespace errors and only intentional changes or commits are present.
