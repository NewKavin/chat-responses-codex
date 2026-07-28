# Portal Codex Recommendation Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the portal generate safer, model-selectable Codex configuration with an 80% catalog compaction threshold, no pinned browser-side Codex version, tuned 4/2/8 agent defaults, and the requested usage-chart ranges.

**Architecture:** Extend the existing `/v1/models` query contract with an explicit `format=codex` selector while retaining `client_version` compatibility. Keep catalog ordering and allowlist filtering in the existing integration view-state helper, add a pure selected-model resolver for Codex-only configuration, and make the login command independent of the portal key. Update source-backed view tests and template/documentation consistency tests so generated UI, examples, and backend behavior remain aligned.

**Tech Stack:** Rust/Axum/Serde/Serde JSON, Vue 3 Composition API, TypeScript, Element Plus, Vitest, Cargo integration tests, Docker Compose deployment scripts.

---

### Task 1: Add the explicit Codex catalog query contract

**Files:**
- Modify: `tests/gateway/compatibility.rs`
- Modify: `src/server/gateway.rs`

- [ ] **Step 1: Write failing backend contract tests**

Expand the standard-response test to parse and assert the OpenAI list shape. Add a `format=codex` test and an unknown-format test using the same minimal state fixture. Retain the existing `client_version` test and change its expected compaction percentage to 80:

```rust
assert_eq!(payload["object"], "list");
assert_eq!(payload["data"], json!([{
    "id": "opaque/catalog-model",
    "object": "model"
}]));
assert!(payload.get("models").is_none());

// /v1/models?format=codex
assert!(payload["models"].is_array());
assert!(payload.get("data").is_none());

// /v1/models?format=unknown
assert_eq!(payload["object"], "list");
assert!(payload.get("models").is_none());

assert_eq!(model["effective_context_window_percent"], 80);
```

- [ ] **Step 2: Run the backend tests and verify RED**

Run:

```bash
rtk cargo test --test gateway compatibility::
```

Expected: the new `format=codex` test receives the standard `data` response, and the updated percentage assertion receives 95 instead of 80. The unknown-format and existing `client_version` compatibility behavior should already pass.

- [ ] **Step 3: Implement the minimal query behavior**

Add `format` to the query DTO and opt into Codex output only for the exact lowercase value or any present `client_version`:

```rust
#[derive(serde::Deserialize)]
struct ModelsQuery {
    client_version: Option<String>,
    format: Option<String>,
}

if query.client_version.is_some() || query.format.as_deref() == Some("codex") {
    return list_models_codex_format(&state, &secret).await;
}
```

Change the catalog field to:

```rust
"effective_context_window_percent": 80,
```

- [ ] **Step 4: Run the backend tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway compatibility::
```

Expected: every compatibility-module test passes, including standard, unknown-format, explicit Codex format, and legacy `client_version` requests.

- [ ] **Step 5: Commit the backend contract**

```bash
rtk git add src/server/gateway.rs tests/gateway/compatibility.rs
rtk git commit -m "feat(gateway): support explicit Codex model format"
```

### Task 2: Tune and secure the Codex configuration generators

**Files:**
- Modify: `frontend/tests/utils/integration.spec.ts`
- Modify: `frontend/src/utils/integration.ts`

- [ ] **Step 1: Write failing generator and selection tests**

Update the config expectations to 4 threads, depth 2, and retry 8. Replace the plaintext login expectation with the exact safe script and assert the sample key is absent. Add a pure selection test proving an explicit valid model controls both slug and live reasoning metadata, while an invalid selection falls back to the first usage-ranked model:

```ts
expect(toml).toContain('max_threads = 4')
expect(toml).toContain('max_depth = 2')
expect(toml).toContain('stream_max_retries = 8')

expect(buildCodexAuthLoginCommand()).toBe(
  `read -rsp 'Gateway downstream key: ' CHAT2RESPONSES_DOWNSTREAM_KEY\n` +
  `printf '\\n'\n` +
  `printf '%s' "$CHAT2RESPONSES_DOWNSTREAM_KEY" | codex login --with-api-key\n` +
  `unset CHAT2RESPONSES_DOWNSTREAM_KEY`
)
expect(buildCodexAuthLoginCommand()).not.toContain('sk-downstream-123')

expect(resolveCodexModelSelection(catalog, rankedSlugs, 'second/model')).toEqual({
  modelSlug: 'second/model',
  modelReasoningEffort: 'high'
})
```

- [ ] **Step 2: Run the frontend generator test and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts
```

Expected: assertions fail on current 8/3 values and plaintext login command, and the selected-model helper import is missing.

- [ ] **Step 3: Implement minimal pure generator changes**

Export a resolver that uses the existing exact-slug selection and live catalog reasoning lookup:

```ts
export const resolveCodexModelSelection = (
  catalog: CodexCatalogResponse | null,
  modelSlugs: string[],
  selectedModelSlug?: string
) => {
  const modelSlug = choosePrimaryModelSlug(modelSlugs, selectedModelSlug)
  return {
    modelSlug,
    modelReasoningEffort: catalog && modelSlug
      ? chooseCodexReasoningEffort(catalog, modelSlug)
      : 'none'
  }
}
```

Set `[agents]` to `max_threads = 4` and `max_depth = 2`; leave `stream_max_retries = 8`. Replace the auth generator with a zero-argument fixed string that reads silently, passes the variable by stdin, and unsets it.

- [ ] **Step 4: Run the generator test and verify GREEN**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts
```

Expected: all generator, selection, template, and guide assertions in the file pass after Task 4 documentation updates; before Task 4, only the intentionally stale documentation assertions may remain red.

- [ ] **Step 5: Commit the generator behavior**

```bash
rtk git add frontend/src/utils/integration.ts frontend/tests/utils/integration.spec.ts
rtk git commit -m "feat(portal): tune and secure Codex config generation"
```

### Task 3: Wire the Codex model selector and chart defaults

**Files:**
- Modify: `frontend/tests/views/portal-integration.spec.ts`
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Modify: `frontend/src/views/portal/Integration.vue`
- Modify: `frontend/src/views/portal/UsageHistory.vue`
- Modify: `frontend/src/views/admin/Dashboard.vue`

- [ ] **Step 1: Write failing source-backed view tests**

Require the new endpoint selector and reject the fixed browser version. Assert that the Codex tab contains a live model select bound to page-local state, that the generated config uses the resolved selection, and that the usage defaults are explicit:

```ts
expect(integrationView).toContain('/v1/models?format=codex')
expect(integrationView).not.toContain('client_version=0.144.6')
expect(integrationView).toContain('v-model="selectedCodexModelSlug"')
expect(integrationView).toContain('v-for="modelSlug in allModelSlugs"')
expect(integrationView).toContain('resolveCodexModelSelection')

expect(source('UsageHistory')).toContain("const timeRange = ref<ChartRange>('7d')")
expect(source('views/admin/Dashboard.vue')).toContain(
  "const chartRange = ref<ChartRange>('1d')"
)
expect(source('views/admin/Dashboard.vue')).toContain("range: '1d'")
```

- [ ] **Step 2: Run the view tests and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/views/portal-integration.spec.ts tests/views/portal-ui.spec.ts tests/views/admin-ui.spec.ts
```

Expected: failures show the pinned version, missing selector, portal 1d default, and admin 7d defaults.

- [ ] **Step 3: Implement page-local Codex selection**

Add a compact Element Plus select inside the Codex tab, sourced only from `allModelSlugs`:

```vue
<el-select
  v-model="selectedCodexModelSlug"
  aria-label="Codex 默认模型"
  filterable
  placeholder="选择 Codex 模型"
>
  <el-option
    v-for="modelSlug in allModelSlugs"
    :key="modelSlug"
    :label="modelSlug"
    :value="modelSlug"
  />
</el-select>
```

Introduce `selectedCodexModelSlug`, derive `codexModelSelection` with `resolveCodexModelSelection`, and feed its slug/reasoning into `buildCodexConfigToml`. In `applyCodexCatalog`, preserve a still-valid selection and otherwise assign the resolver fallback. Keep all non-Codex client generators on `primaryModelSlug`.

Change the catalog request to `/v1/models?format=codex`, change the visible compaction copy to 80%, and label the initially chosen model as the historically most-used default rather than a universal recommendation. Call `buildCodexAuthLoginCommand()` without a key.

- [ ] **Step 4: Apply the usage range defaults**

Set portal `timeRange` to `'7d'`. Set both admin `chartRange` and the initial empty analytics `range` to `'1d'`. Do not change controls, request mapping, or refresh behavior.

- [ ] **Step 5: Run the view and generator tests and verify GREEN**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts tests/views/portal-ui.spec.ts tests/views/admin-ui.spec.ts
```

Expected: all selected frontend tests pass with no warnings or failures except any documentation consistency assertion intentionally pending Task 4.

- [ ] **Step 6: Commit the portal behavior**

```bash
rtk git add frontend/src/views/portal/Integration.vue frontend/src/views/portal/UsageHistory.vue frontend/src/views/admin/Dashboard.vue frontend/tests/views/portal-integration.spec.ts frontend/tests/views/portal-ui.spec.ts frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(portal): optimize recommended Codex setup"
```

### Task 4: Align templates and documentation

**Files:**
- Modify: `tests/templates.rs`
- Modify: `frontend/tests/utils/integration.spec.ts`
- Modify: `templates/codex/config.toml.example`
- Modify: `docs/codex-integration-guide.md`

- [ ] **Step 1: Update consistency tests first**

Require the example fetch URL to contain `format=codex` and reject `client_version=0.144.6` in portal-facing Codex instructions. Require both TOML examples and the template to use 4/2/8, and require the guide to describe 80% catalog-relative compaction.

Do not change the unrelated `0.144.6` deployment User-Agent default, installed-client pin, historical verification records, or compatibility test inputs.

- [ ] **Step 2: Run consistency tests and verify RED**

Run:

```bash
rtk cargo test --test templates
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts
```

Expected: old template/guide assertions or source content fail on the pinned portal URL, 8/3 defaults, and 95% copy.

- [ ] **Step 3: Update examples and prose minimally**

Use:

```text
/v1/models?format=codex
max_threads = 4
max_depth = 2
stream_max_retries = 8
effective_context_window_percent = 80
```

Explain that `client_version` remains supported for real Codex clients, while portal/manual discovery uses the semantic `format=codex` query. Include the secure `read -rsp` login snippet and ensure no example embeds a literal key into shell history.

- [ ] **Step 4: Run consistency tests and stale-marker scan**

Run:

```bash
rtk cargo test --test templates
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts
rtk rg -n 'client_version=0\.144\.6|max_threads = 8|max_depth = 3|effective_context_window_percent[^[:cntrl:]]*95|<strong>95%</strong>' frontend/src frontend/tests templates/codex docs/codex-integration-guide.md src/server/gateway.rs tests/templates.rs tests/gateway/compatibility.rs
```

Expected: tests pass. The scan has no portal/template/guide old-value matches; explicitly retained compatibility fixtures are reviewed individually.

- [ ] **Step 5: Commit consistency updates**

```bash
rtk git add tests/templates.rs frontend/tests/utils/integration.spec.ts templates/codex/config.toml.example docs/codex-integration-guide.md
rtk git commit -m "docs(codex): align optimized portal defaults"
```

### Task 5: Verify, review, build, deploy, and smoke test

**Files:**
- Verify all modified files
- Generated by build: `frontend/dist/**`
- Deployment target: `/home/kavin/docker/chat-responses-codex`

- [ ] **Step 1: Run focused and full automated verification**

```bash
rtk cargo test --test gateway compatibility::
rtk cargo test --test templates
rtk npm --prefix frontend test -- --run tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts tests/views/portal-ui.spec.ts tests/views/admin-ui.spec.ts
rtk npm --prefix frontend run build
rtk cargo test
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo build --release
```

Expected: all commands exit 0, full Rust tests report zero failures, strict Clippy reports no warnings, and both frontend and release builds complete.

- [ ] **Step 2: Request an independent read-only code review**

Give the reviewer the design, plan, base SHA, and current HEAD. Fix every Critical or Important issue with a failing regression test first, then rerun the affected and full verification commands.

- [ ] **Step 3: Commit the verified implementation if needed**

```bash
rtk git status --short
rtk git add src/server/gateway.rs tests/gateway/compatibility.rs frontend/src/utils/integration.ts frontend/src/views/portal/Integration.vue frontend/src/views/portal/UsageHistory.vue frontend/src/views/admin/Dashboard.vue frontend/tests/utils/integration.spec.ts frontend/tests/views/portal-integration.spec.ts frontend/tests/views/portal-ui.spec.ts frontend/tests/views/admin-ui.spec.ts templates/codex/config.toml.example docs/codex-integration-guide.md tests/templates.rs
rtk git commit -m "fix(portal): address Codex optimization review"
```

Do not include unrelated user changes and do not push.

- [ ] **Step 4: Deploy without overwriting the existing environment**

Run from the repository root without `--force-copy-config`:

```bash
rtk bash scripts/deploy.sh
```

Expected: image build and Compose rollout complete, preserving `/home/kavin/docker/chat-responses-codex/.env`.

- [ ] **Step 5: Verify live health and both model response shapes**

Use the existing deployment key without echoing it. Verify:

```bash
rtk curl --retry 30 --retry-delay 1 --retry-all-errors -fsS http://127.0.0.1:3000/healthz
```

Then check authenticated requests for:

```text
/v1/models                 -> object=list and data array
/v1/models?format=codex    -> non-empty models array, no data, all percentages 80
/v1/models?format=unknown  -> object=list and data array
/v1/models?client_version=0.144.6 -> non-empty models array
```

Never print the downstream key or full auth header.

- [ ] **Step 6: Run the installed GLM/Codex acceptance smoke**

Run `scripts/installed_client_smoke.sh` with `MODEL_SLUG=glm-5.2` and `CLIENTS_JSON='["codex"]'`, sourcing the deployment environment without tracing. This covers a normal response, read-only tool use, namespace/MCP tool calls, streaming, and the real pinned Codex `client_version` catalog path.

Expected: every Codex/GLM smoke case passes with zero failures. If the deployed exposed slug differs only by catalog casing/name, use the exact live catalog slug rather than inventing an alias.

- [ ] **Step 7: Final requirement audit**

Re-read the confirmed design and inspect `rtk git diff HEAD~4 --stat` plus `rtk git status --short`. Confirm all ten requirements are represented by code/tests/docs, generated assets are current, deployment is healthy, secrets were not logged, retry remains exactly 8, and no unrelated files were changed.
