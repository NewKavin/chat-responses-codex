# Portal Codex None Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show catalog-published `none` as the first selectable Codex reasoning effort and copy it explicitly into both generated TOML files.

**Architecture:** Reuse the existing live-catalog resolver and TOML generators. Extend only the portal's canonical effort tuple so its existing type guard, supported-set construction, option rendering, and computed generators carry `none` through the same path as every other verified effort.

**Tech Stack:** Vue 3, TypeScript, Element Plus, Vitest, Vite.

---

### Task 1: Expose The Catalog `none` Effort

**Files:**
- Modify: `frontend/tests/utils/integration.spec.ts:310-422`
- Modify: `frontend/src/utils/integration.ts:15`

- [ ] **Step 1: Write the failing mixed-catalog expectations**

Rename the fixed-vocabulary test and require `none` first:

```typescript
it('offers the fixed six verified Codex reasoning strengths with none first', () => {
  const selection = resolveCodexReasoningSelection(
    {
      models: [{
        slug: 'verified/model',
        default_reasoning_level: 'high',
        supported_reasoning_levels: [
          { effort: 'none' },
          { effort: 'minimal' },
          { effort: 'low' },
          { effort: 'medium' },
          { effort: 'high' },
          { effort: 'xhigh' },
          { effort: 'max' },
          { effort: 'experimental' }
        ]
      }]
    },
    'verified/model'
  )

  expect(CODEX_REASONING_EFFORTS).toEqual([
    'none',
    'low',
    'medium',
    'high',
    'xhigh',
    'max'
  ])
  expect(selection.options).toEqual([
    { value: 'none', disabled: false },
    { value: 'low', disabled: false },
    { value: 'medium', disabled: false },
    { value: 'high', disabled: false },
    { value: 'xhigh', disabled: false },
    { value: 'max', disabled: false }
  ])
  expect(selection.defaultEffort).toBe('high')
  expect(selection.selectedEffort).toBe('high')
  expect(selection.configurable).toBe(true)
})
```

Update the bounded model's disabled-option vector for the new first item:

```typescript
expect(selected.options.map(option => option.disabled)).toEqual([
  true,
  false,
  true,
  false,
  true,
  true
])
```

Update the missing-default option list so its unpublished `none` remains
disabled while `low` remains enabled:

```typescript
expect(selection.options).toEqual([
  { value: 'none', disabled: true },
  { value: 'low', disabled: false },
  { value: 'medium', disabled: true },
  { value: 'high', disabled: true },
  { value: 'xhigh', disabled: true },
  { value: 'max', disabled: true }
])
```

- [ ] **Step 2: Write the failing none-only regression expectation**

Replace the internal-fallback assertions with visible, selectable behavior:

```typescript
it('keeps a catalog-published none effort visible and copyable', () => {
  const selection = resolveCodexReasoningSelection(
    {
      models: [{
        slug: 'conservative/model',
        default_reasoning_level: 'none',
        supported_reasoning_levels: [{ effort: 'none' }]
      }]
    },
    'conservative/model',
    'high'
  )

  expect(selection.options).toEqual([
    { value: 'none', disabled: false },
    { value: 'low', disabled: true },
    { value: 'medium', disabled: true },
    { value: 'high', disabled: true },
    { value: 'xhigh', disabled: true },
    { value: 'max', disabled: true }
  ])
  expect(selection.defaultEffort).toBe('none')
  expect(selection.selectedEffort).toBe('none')
  expect(selection.configurable).toBe(true)

  const input = {
    modelSlug: 'conservative/model',
    modelReasoningEffort: selection.selectedEffort
  }
  expect(
    buildCodexConfigToml({ gatewayBaseUrl: 'https://gw.example', ...input })
  ).toContain('model_reasoning_effort = "none"')
  expect(buildCodexDefaultAgentToml(input)).toContain(
    'model_reasoning_effort = "none"'
  )
})
```

- [ ] **Step 3: Run the focused test and verify RED**

Run from `frontend/`:

```bash
rtk npm test -- tests/utils/integration.spec.ts
```

Expected: FAIL because `CODEX_REASONING_EFFORTS` still omits `none`, the
mixed list has only five options, and the none-only selector is not
configurable.

- [ ] **Step 4: Make the minimal production change**

Change the canonical tuple in `frontend/src/utils/integration.ts`:

```typescript
export const CODEX_REASONING_EFFORTS = [
  'none',
  'low',
  'medium',
  'high',
  'xhigh',
  'max'
] as const
```

Do not change resolver branching or TOML generation. The existing type guard
must now recognize `none`, and the existing `supported.size > 0` check must
make a none-only catalog configurable.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run from `frontend/`:

```bash
rtk npm test -- tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
```

Expected: both test files PASS, including explicit `none` output in parent and
default-agent TOML.

- [ ] **Step 6: Commit the behavior slice**

```bash
rtk git add frontend/src/utils/integration.ts frontend/tests/utils/integration.spec.ts
rtk git commit -m "fix: expose none in portal Codex reasoning"
```

### Task 2: Verify And Deploy Without Configuration Mutation

**Files:**
- Verify: `frontend/src/utils/integration.ts`
- Verify: `frontend/tests/utils/integration.spec.ts`

- [ ] **Step 1: Run complete frontend verification**

Run from `frontend/`:

```bash
rtk npm test
rtk npm run type-check
rtk npm run build
```

Expected: all Vitest suites pass, Vue TypeScript reports no errors, and Vite
produces the production bundle.

- [ ] **Step 2: Check formatting and branch state**

Run from the worktree root:

```bash
rtk git diff --check HEAD~2
rtk git status --short --branch
```

Expected: no whitespace errors and no uncommitted implementation files.

- [ ] **Step 3: Deploy through the established project script**

Run from the worktree root:

```bash
rtk bash scripts/deploy.sh -d /home/kavin/docker/chat-responses-codex
```

Expected: the image builds and Compose recreates only the gateway application
as required by the existing deployment path. Do not edit upstreams, mappings,
capability overrides, downstream keys, PostgreSQL volumes, or Redis volumes.

- [ ] **Step 4: Verify the fresh deployment**

```bash
rtk curl -fsS http://127.0.0.1:3000/healthz
rtk docker inspect --format '{{.State.Status}} {{.State.Health.Status}} {{.RestartCount}}' chat-responses-codex
rtk docker logs --since 10m chat-responses-codex
```

Expected: `/healthz` returns `ok`, the container reports
`running healthy 0`, and fresh logs contain no startup panic or repeated
gateway errors.

- [ ] **Step 5: Inspect the deployed frontend asset**

Run this dynamic asset check, which reads the current hashed filename from the
deployed index before inspecting the utility chunk:

```bash
rtk node -e 'const base="http://127.0.0.1:3000/"; const index=await fetch(base).then(r=>r.text()); const asset=index.match(/assets\/integration-[A-Za-z0-9_-]+\.js/)?.[0]; if(!asset) throw new Error("integration asset not found"); const body=await fetch(new URL(asset,base)).then(r=>r.text()); if(!/\[`none`,`low`,`medium`,`high`,`xhigh`,`max`\]/.test(body)) throw new Error("none-first effort tuple not found"); if(!body.includes("model_reasoning_effort")) throw new Error("TOML effort generator not found"); console.log(asset, "none-first tuple and TOML generator verified")'
```

Expected: the deployed bundle contains the six-value order
`none, low, medium, high, xhigh, max` and explicit TOML generation.
