# Configurable None Reasoning Effort Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `none` as the first configurable reasoning effort and expose model-summary batch editing for all current exact routes.

**Architecture:** Extend the existing route-scoped `effort_map` vocabulary rather than adding a disable flag. Keep discovery and Codex metadata on one six-value canonical order, then reuse the existing atomic `model_routes` API scope from a new model-summary editor while preserving exact-route editing.

**Tech Stack:** Rust, Axum, serde/serde_json, Tokio integration tests, Vue 3, TypeScript, Element Plus, Vitest.

---

### Task 1: Accept And Discover `none`

**Files:**
- Modify: `tests/admin_capabilities.rs:840-950`
- Modify: `src/server/gateway/reasoning_overrides.rs:10-60`
- Modify: `src/server/gateway/capability_admin.rs:852-865`

- [ ] **Step 1: Write the failing admin override assertions**

Change the existing upsert test request to include unsorted duplicate `none`
values and require the persisted mapping and discovery result to retain one
canonical first entry:

```rust
"levels": ["high", "none", "low", "none", "high"],
```

```rust
assert_eq!(
    managed["effort_map"],
    json!({"high": "high", "low": "low", "none": "none"})
);
assert_eq!(
    route["accepted_reasoning_levels"],
    json!(["none", "low", "high"])
);
```

Capture the invalid-level response once and require the complete vocabulary:

```rust
let invalid_level_body = response_json(invalid_level).await;
assert_eq!(
    invalid_level_body["error"]["code"],
    "capability_reasoning_override_invalid_level"
);
assert_eq!(
    invalid_level_body["error"]["message"],
    "levels must contain only none, low, medium, high, xhigh, or max"
);
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
rtk cargo test --test admin admin_capabilities::admin_reasoning_override_upserts_clears_and_reports_effective_source -- --exact --nocapture
```

Expected: FAIL because the API returns `400 Bad Request` for `none`.

Run:

```bash
rtk cargo test --test admin admin_capabilities::admin_reasoning_override_rejects_invalid_levels_and_stale_routes_atomically -- --exact --nocapture
```

Expected: FAIL because the old validation message omits `none`.

- [ ] **Step 3: Extend backend normalization and discovery ordering**

Use one six-value allowlist in `reasoning_overrides.rs`:

```rust
const REASONING_LEVELS: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];
```

Change the stable validation message to:

```rust
"levels must contain only none, low, medium, high, xhigh, or max"
```

Extend discovery filtering and ordering in `capability_admin.rs`:

```rust
fn sort_canonical_reasoning_levels(levels: &mut Vec<String>) {
    const ORDER: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];
    levels.retain(|level| ORDER.contains(&level.as_str()));
    levels.sort_by(|left, right| {
        ORDER
            .iter()
            .position(|candidate| candidate == left)
            .cmp(&ORDER.iter().position(|candidate| candidate == right))
            .then_with(|| left.cmp(right))
    });
    levels.dedup();
}
```

Do not change the `if !levels.is_empty()` branch: `["none"]` must create a
managed override while `[]` must continue clearing it.

- [ ] **Step 4: Run focused and module tests and verify GREEN**

Run:

```bash
rtk cargo test --test admin admin_capabilities::admin_reasoning_override_upserts_clears_and_reports_effective_source -- --exact --nocapture
rtk cargo test --test admin admin_capabilities::admin_reasoning_override_rejects_invalid_levels_and_stale_routes_atomically -- --exact --nocapture
rtk cargo test --test admin admin_capabilities -- --nocapture
```

Expected: all selected tests PASS.

- [ ] **Step 5: Commit the backend vocabulary slice**

```bash
rtk git add tests/admin_capabilities.rs src/server/gateway/reasoning_overrides.rs src/server/gateway/capability_admin.rs
rtk git commit -m "feat: support none reasoning overrides"
```

### Task 2: Preserve `none` In Codex Metadata

**Files:**
- Modify: `tests/gateway/capability_routing.rs:750-900`
- Modify: `tests/gateway/responses/fallback.rs:1649-1816`
- Modify: `src/server/gateway.rs:2404-2476`

- [ ] **Step 1: Write failing mixed and `none`-only catalog assertions**

In `codex_catalog_hot_applies_admin_reasoning_override_without_rebuilding_router`,
first apply a mixed set without `high`:

```rust
"levels": ["low", "none"],
```

Require canonical order, a non-`none` default, and summaries:

```rust
assert_eq!(
    after["models"][0]["supported_reasoning_levels"],
    json!([
        {"effort": "none", "description": "Do not use reasoning effort"},
        {"effort": "low", "description": "Use low reasoning effort"}
    ])
);
assert_eq!(after["models"][0]["default_reasoning_level"], "low");
assert_eq!(after["models"][0]["supports_reasoning_summaries"], true);
```

Then send a second update through the same router with `"levels": ["none"]`
and require:

```rust
assert_eq!(
    none_only["models"][0]["supported_reasoning_levels"],
    json!([{
        "effort": "none",
        "description": "Do not use reasoning effort"
    }])
);
assert_eq!(none_only["models"][0]["default_reasoning_level"], "none");
assert_eq!(none_only["models"][0]["supports_reasoning_summaries"], false);
```

Also add `none -> upstream-none` to the policy and `upstream-none` to the
profile in `codex_catalog_advertises_only_verified_reasoning_levels`, then
expect `none` before `low` while keeping `high` as the default.

- [ ] **Step 2: Run the focused catalog tests and verify RED**

Run:

```bash
rtk cargo test --test gateway capability_routing::codex_catalog_hot_applies_admin_reasoning_override_without_rebuilding_router -- --exact --nocapture
rtk cargo test --test gateway capability_routing::codex_catalog_advertises_only_verified_reasoning_levels -- --exact --nocapture
```

Expected: FAIL because `CODEX_REASONING_EFFORT_ORDER` filters out `none` and
the current non-empty metadata path does not distinguish `none`-only summary
support.

- [ ] **Step 3: Implement six-value Codex metadata semantics**

Extend the canonical order:

```rust
const CODEX_REASONING_EFFORT_ORDER: [&str; 6] =
    ["none", "low", "medium", "high", "xhigh", "max"];
```

Give the formal entry a stable description without changing the conservative
fallback wording:

```rust
fn codex_reasoning_description(effort: &str) -> String {
    if effort == "none" {
        "Do not use reasoning effort".to_owned()
    } else {
        format!("Use {effort} reasoning effort")
    }
}
```

Deduplicate after sorting, prefer `high`, otherwise select the first non-none
effort, and only advertise summaries for a non-none effort:

```rust
efforts.sort_by(|left, right| {
    codex_reasoning_effort_rank(left)
        .cmp(&codex_reasoning_effort_rank(right))
        .then_with(|| left.cmp(right))
});
efforts.dedup();

let default_effort = efforts
    .iter()
    .find(|effort| effort.as_str() == "high")
    .or_else(|| efforts.iter().find(|effort| effort.as_str() != "none"))
    .cloned()
    .unwrap_or_else(|| "none".to_owned());
let supports_summaries = efforts.iter().any(|effort| effort != "none");
```

Return `supports_summaries` in the normal metadata result. Keep
`codex_conservative_reasoning_metadata()` unchanged so absence of evidence
still has its existing description and false summary flag.

- [ ] **Step 4: Run focused and gateway capability tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway capability_routing::codex_catalog_hot_applies_admin_reasoning_override_without_rebuilding_router -- --exact --nocapture
rtk cargo test --test gateway capability_routing::codex_catalog_advertises_only_verified_reasoning_levels -- --exact --nocapture
rtk cargo test --test gateway capability_routing -- --nocapture
```

Expected: all selected tests PASS, including existing conservative fallback
tests.

- [ ] **Step 5: Cover exact `none` forwarding through the existing adapter**

Extend `mapped_reasoning_effort_precedes_generic_normalization` without
changing production adapter code:

```rust
effort_map: std::collections::BTreeMap::from([
    ("none".into(), "none".into()),
    ("xhigh".into(), "upstream-xhigh".into()),
    ("max".into(), "upstream-max".into()),
]),
```

```rust
profile.reasoning_controls.insert(
    "reasoning_effort".into(),
    vec!["none".into(), "upstream-xhigh".into(), "upstream-max".into()],
);
```

Send all three canonical efforts and assert their exact mapped upstream values:

```rust
for effort in ["none", "xhigh", "max"] {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "input": "hello",
                        "reasoning": {"effort": effort}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

let captured = captured.lock().unwrap();
assert_eq!(captured.len(), 3);
assert_eq!(captured[0]["reasoning_effort"], "none");
assert_eq!(captured[1]["reasoning_effort"], "upstream-xhigh");
assert_eq!(captured[2]["reasoning_effort"], "upstream-max");
```

Run:

```bash
rtk cargo test --test gateway responses_fallback::mapped_reasoning_effort_precedes_generic_normalization -- --exact --nocapture
```

Expected: PASS, proving the existing request adapter needs no special disable
field and forwards the configured `none` string exactly.

- [ ] **Step 6: Commit the catalog slice**

```bash
rtk git add tests/gateway/capability_routing.rs tests/gateway/responses/fallback.rs src/server/gateway.rs
rtk git commit -m "feat: advertise configurable none reasoning"
```

### Task 3: Add The Sixth Frontend Effort

**Files:**
- Modify: `frontend/src/utils/reasoningOverrides.spec.ts:8-27`
- Modify: `frontend/src/utils/reasoningOverrides.ts:3-20`
- Modify: `frontend/src/types/index.ts:705`

- [ ] **Step 1: Write the failing frontend vocabulary test**

Require the new canonical order and normalization behavior:

```typescript
expect(REASONING_EFFORT_LEVELS).toEqual([
  'none',
  'low',
  'medium',
  'high',
  'xhigh',
  'max'
])
expect(normalizeReasoningLevels([
  'max',
  'none',
  'low',
  'none',
  'high',
  'future-level'
])).toEqual(['none', 'low', 'high', 'max'])
```

- [ ] **Step 2: Run the utility test and verify RED**

Run from `frontend/`:

```bash
rtk npm test -- src/utils/reasoningOverrides.spec.ts
```

Expected: FAIL because `none` is missing from the constant and filtered out by
normalization.

- [ ] **Step 3: Extend the shared type and constant**

Change the type to:

```typescript
export type ReasoningEffortLevel =
  | 'none'
  | 'low'
  | 'medium'
  | 'high'
  | 'xhigh'
  | 'max'
```

Change the constant to:

```typescript
export const REASONING_EFFORT_LEVELS = [
  'none',
  'low',
  'medium',
  'high',
  'xhigh',
  'max'
] as const satisfies readonly ReasoningEffortLevel[]
```

No checkbox special case is added; the existing checkbox group must treat
`none` as combinable with every other value.

- [ ] **Step 4: Run utility tests and type checking and verify GREEN**

Run from `frontend/`:

```bash
rtk npm test -- src/utils/reasoningOverrides.spec.ts
rtk npm run type-check
```

Expected: both commands PASS.

- [ ] **Step 5: Commit the frontend vocabulary slice**

```bash
rtk git add frontend/src/types/index.ts frontend/src/utils/reasoningOverrides.ts frontend/src/utils/reasoningOverrides.spec.ts
rtk git commit -m "feat: add none reasoning level to admin ui"
```

### Task 4: Edit All Current Routes From Model Summary

**Files:**
- Modify: `frontend/tests/views/admin-ui.spec.ts:80-115`
- Modify: `frontend/src/views/admin/ModelProbe.vue:215-490`
- Modify: `frontend/src/views/admin/ModelProbe.vue:535-730`

- [ ] **Step 1: Write the failing UI structure assertions**

Extend the existing reasoning editor test with exact behavior markers:

```typescript
expect(page).toContain('aria-label="配置模型全部当前路由"')
expect(page).toContain('openModelReasoningOverrideEditor(row)')
expect(page).toContain("reasoningOverrideEditorMode.value = 'model'")
expect(page).toContain("reasoningOverrideScope.value = 'model_routes'")
expect(page).toContain('editingReasoningModel.routes.length')
expect(page).toContain('模型全部当前路由')
expect(page).toContain('openReasoningOverrideEditor(row)')
expect(page).toContain("reasoningOverrideEditorMode.value = 'route'")
expect(page).toContain('applyReasoningOverride(route, [], scope)')
```

Keep the existing assertions for `Pencil`, `RotateCcw`, the shared effort
constant, and the admin API call. Replace the static dialog-title assertion
with:

```typescript
expect(page).toContain(':title="reasoningOverrideDialogTitle"')
```

- [ ] **Step 2: Run the admin UI source test and verify RED**

Run from `frontend/`:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
```

Expected: FAIL because the model-summary table has no batch edit action or
model editor mode.

- [ ] **Step 3: Add typed model-summary rows and editor state**

Add the model row and mode types next to `EditableReasoningRoute`:

```typescript
type EditableReasoningModel = {
  exposed_model_slug: string
  levels: ReasoningEffortLevel[]
  routes: EditableReasoningRoute[]
}
type ReasoningOverrideEditorMode = 'model' | 'route'
```

Build both summary lists with route identities and normalized levels:

```typescript
const toEditableReasoningModels = (
  discovery: CapabilityDiscoveryResponse
): EditableReasoningModel[] => discovery.models.map(model => ({
  exposed_model_slug: model.exposed_model_slug,
  levels: normalizeReasoningLevels(model.verified_reasoning_levels),
  routes: model.routes.map(route => ({
    ...route,
    exposed_model_slug: model.exposed_model_slug
  }))
}))
```

Use this helper in `capabilityModelResults` and
`globalCapabilityModelResults`. Add state and derived display values:

```typescript
const reasoningOverrideEditorMode = ref<ReasoningOverrideEditorMode>('route')
const editingReasoningModel = ref<EditableReasoningModel | null>(null)
const reasoningOverrideDialogTitle = computed(() =>
  reasoningOverrideEditorMode.value === 'model'
    ? '配置模型思考档位'
    : '编辑路由思考档位'
)
const reasoningEditorHasManagedOverride = computed(() =>
  reasoningOverrideEditorMode.value === 'model'
    ? editingReasoningModel.value?.routes.some(route => route.managed_reasoning_override) === true
    : editingReasoningRoute.value?.managed_reasoning_override === true
)
```

- [ ] **Step 4: Add model and route entry functions**

Reset all relevant state explicitly when either entry opens:

```typescript
const openModelReasoningOverrideEditor = (model: EditableReasoningModel) => {
  const representativeRoute = model.routes[0]
  if (!representativeRoute) return
  reasoningOverrideEditorMode.value = 'model'
  editingReasoningModel.value = model
  editingReasoningRoute.value = representativeRoute
  reasoningOverrideScope.value = 'model_routes'
  selectedReasoningLevels.value = normalizeReasoningLevels(model.levels)
  reasoningOverrideDialogVisible.value = true
}

const openReasoningOverrideEditor = (route: EditableReasoningRoute) => {
  reasoningOverrideEditorMode.value = 'route'
  editingReasoningModel.value = null
  editingReasoningRoute.value = route
  reasoningOverrideScope.value = 'route'
  selectedReasoningLevels.value = normalizeReasoningLevels(
    route.accepted_reasoning_levels
  )
  reasoningOverrideDialogVisible.value = true
}
```

The existing `saveReasoningOverride` continues to pass the representative
route and current scope to `applyReasoningOverride`; the backend atomically
expands `model_routes`.

- [ ] **Step 5: Add the model-summary action and mode-aware dialog**

Add an icon-only action column to the primary model-summary table:

```vue
<el-table-column label="操作" width="72" align="center">
  <template #default="{ row }">
    <el-tooltip content="配置模型全部当前路由" placement="top">
      <el-button
        text
        :icon="Pencil"
        aria-label="配置模型全部当前路由"
        :disabled="row.routes.length === 0"
        @click="openModelReasoningOverrideEditor(row)"
      />
    </el-tooltip>
  </template>
</el-table-column>
```

Bind the dialog title and show model-wide scope without exposing the arbitrary
representative route:

```vue
<el-dialog
  v-model="reasoningOverrideDialogVisible"
  :title="reasoningOverrideDialogTitle"
>
  <template v-if="editingReasoningRoute">
    <dl class="reasoning-override-context">
      <div>
        <dt>模型</dt>
        <dd>{{ editingReasoningRoute.exposed_model_slug }}</dd>
      </div>
      <div v-if="reasoningOverrideEditorMode === 'model' && editingReasoningModel">
        <dt>应用范围</dt>
        <dd>模型全部当前路由 · {{ editingReasoningModel.routes.length }} 条</dd>
      </div>
      <template v-else>
        <div><dt>上游</dt><dd>{{ editingReasoningRoute.upstream_id }}</dd></div>
        <div><dt>协议</dt><dd>{{ capabilityProtocolLabel(editingReasoningRoute.protocol) }}</dd></div>
      </template>
    </dl>
  </template>
</el-dialog>
```

Render the existing scope radio group only in route mode. In model mode render
the fixed `模型全部当前路由` value. Change the clear button guard to
`reasoningEditorHasManagedOverride`; it must still call
`clearReasoningOverride(editingReasoningRoute, reasoningOverrideScope)`, whose
existing `levels: []` request clears only managed overrides.

- [ ] **Step 6: Run UI tests, type checking, and frontend build and verify GREEN**

Run from `frontend/`:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
rtk npm test -- src/utils/reasoningOverrides.spec.ts
rtk npm run type-check
rtk npm run build
```

Expected: all commands PASS without Vue template or TypeScript errors.

- [ ] **Step 7: Commit the model-summary workflow**

```bash
rtk git add frontend/tests/views/admin-ui.spec.ts frontend/src/views/admin/ModelProbe.vue
rtk git commit -m "feat: batch edit reasoning from model summary"
```

### Task 5: Full Verification And Deployment Qualification

**Files:**
- Verify: all files changed in Tasks 1-4

- [ ] **Step 1: Run formatting checks**

```bash
rtk rustfmt --edition 2021 --check src/server/gateway/reasoning_overrides.rs src/server/gateway/capability_admin.rs src/server/gateway.rs tests/admin_capabilities.rs tests/gateway/capability_routing.rs
rtk git diff --check
```

Expected: both commands exit successfully with no formatting errors in the
files changed by this feature. This intentionally avoids treating the
repository-wide historical rustfmt baseline as feature churn.

- [ ] **Step 2: Run the complete Rust test suite**

```bash
rtk cargo test --all-targets --all-features
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all Rust unit and integration tests PASS and clippy emits no warning.

- [ ] **Step 3: Run complete frontend verification**

Run from `frontend/`:

```bash
rtk npm test
rtk npm run type-check
rtk npm run build
```

Expected: all Vitest suites, Vue TypeScript checks, and the production Vite
build PASS.

- [ ] **Step 4: Inspect the final diff and worktree**

```bash
rtk git diff HEAD~4 --check
rtk git status --short --branch
rtk git log -5 --oneline
```

Expected: no uncommitted implementation changes, four feature commits after
the plan commit, and no unrelated files changed.

- [ ] **Step 5: Deploy through the established production path**

From the feature worktree run:

```bash
rtk bash scripts/deploy.sh -d /home/kavin/docker/chat-responses-codex
```

Expected: the image builds successfully and Compose recreates the gateway
without recreating or clearing PostgreSQL or Redis volumes.

- [ ] **Step 6: Verify the deployed gateway without mutating routing inputs**

```bash
rtk docker compose --env-file /home/kavin/docker/chat-responses-codex/.env -f /home/kavin/docker/chat-responses-codex/docker-compose.yml --project-directory /home/kavin/docker/chat-responses-codex ps
rtk curl -fsS http://127.0.0.1:3000/healthz
rtk docker inspect --format '{{.State.Status}} {{.State.Health.Status}} {{.RestartCount}}' chat-responses-codex
```

Expected: gateway, PostgreSQL, and Redis remain running; `/healthz` returns
`ok`; the gateway reports `running healthy 0`. Do not change any production
upstream protocol, API key, model mapping, capability override, or persistent
volume during this smoke test.
