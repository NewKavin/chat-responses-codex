# Portal Playground Reliability And Troubleshooting Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the portal playground use only live routable models with minimal compatible requests, reject unsupported binary attachments, make its E2E non-mutating, and remove the portal troubleshooting surface while retaining admin troubleshooting.

**Architecture:** Keep protocol adaptation in the gateway. Add pure frontend helpers for live-catalog selection and attachment validation, then make `Playground.vue` consume those helpers. Remove only portal troubleshooting wrappers and routes; shared validators and the admin matrix remain untouched.

**Tech Stack:** Vue 3, TypeScript, Element Plus, Vitest, Rust 2021, Axum, Bash, jq.

---

## File Map

- Modify `frontend/src/utils/playground.ts`: live model intersection, minimal request shape, and text attachment classification.
- Modify `frontend/tests/utils/playground.spec.ts`: pure model, payload, attachment, and SSE contracts.
- Modify `frontend/src/views/portal/Playground.vue`: authoritative catalog loading, automatic optional controls, local binary rejection, and empty-stream failure.
- Modify `scripts/portal_playground_e2e.sh`: caller-provided key, no admin login or rotation, live-model-only smoke.
- Modify `tests/scripts.rs`: static no-rotation/no-secret-output contract.
- Modify `frontend/src/router/index.ts`, `frontend/src/views/portal/Portal.vue`, `frontend/src/api/portal.ts`: remove portal troubleshooting navigation/API.
- Delete `frontend/src/views/portal/Troubleshooting.vue`.
- Modify `frontend/tests/router/index.spec.ts`, `frontend/tests/api/portal.spec.ts`: absence contracts.
- Modify `src/server/gateway.rs`, `src/server/gateway/troubleshooting.rs`: remove portal troubleshooting endpoints and wrappers.
- Modify `tests/troubleshooting.rs`: assert portal 404 while keeping admin coverage.

### Task 1: Make Live Gateway Models The Playground Authority

**Files:**
- Modify: `frontend/src/utils/playground.ts`
- Test: `frontend/tests/utils/playground.spec.ts`

- [ ] **Step 1: Write failing live-model selection tests**

Add these imports and tests:

```ts
import {
  selectPlayableModels,
  type GatewayModelResponse
} from '../../src/utils/playground'

describe('playground live model selection', () => {
  const live: GatewayModelResponse = {
    data: [
      { id: 'Qwen/Qwen3-235B-A22B' },
      { id: 'deepseek-ai/deepseek-v4-flash' },
      { id: 'grok-4.20-fast' }
    ]
  }

  it('preserves allowlist order while intersecting the live catalog', () => {
    expect(selectPlayableModels(
      ['deepseek-v4-flash', 'grok-4.20-fast', 'Qwen/Qwen3-235B-A22B'],
      live
    )).toEqual(['grok-4.20-fast', 'Qwen/Qwen3-235B-A22B'])
  })

  it('uses every live model when the allowlist is empty', () => {
    expect(selectPlayableModels([], live)).toEqual([
      'Qwen/Qwen3-235B-A22B',
      'deepseek-ai/deepseek-v4-flash',
      'grok-4.20-fast'
    ])
  })

  it('never accepts historical model statistics as an input', () => {
    expect(selectPlayableModels(['stale-model'], { data: [] })).toEqual([])
  })
})
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `rtk npm --prefix frontend exec vitest run tests/utils/playground.spec.ts`

Expected: FAIL because `selectPlayableModels` and `GatewayModelResponse` do not exist.

- [ ] **Step 3: Implement the pure selector**

Add to `frontend/src/utils/playground.ts`:

```ts
export interface GatewayModelResponse {
  data?: Array<{ id?: unknown }>
}

export const selectPlayableModels = (
  allowlist: string[],
  response: GatewayModelResponse
): string[] => {
  const live = parseGatewayModels(response)
  if (allowlist.length === 0) return live

  const liveSet = new Set(live)
  const seen = new Set<string>()
  return allowlist
    .map(model => model.trim())
    .filter(model => {
      if (!model || !liveSet.has(model) || seen.has(model)) return false
      seen.add(model)
      return true
    })
}
```

- [ ] **Step 4: Run the focused test and verify GREEN**

Run: `rtk npm --prefix frontend exec vitest run tests/utils/playground.spec.ts`

Expected: PASS.

- [ ] **Step 5: Commit the live model selector**

```bash
rtk git add frontend/src/utils/playground.ts frontend/tests/utils/playground.spec.ts
rtk git commit -m "fix(portal): select playground models from live routes"
```

### Task 2: Make Playground Requests Minimal And Attachments Text-Only

**Files:**
- Modify: `frontend/src/utils/playground.ts`
- Test: `frontend/tests/utils/playground.spec.ts`

- [ ] **Step 1: Write failing minimal-payload and attachment tests**

Add:

```ts
import { classifyPlaygroundAttachment } from '../../src/utils/playground'

it('omits optional controls in automatic mode', () => {
  expect(buildPlaygroundChatPayload({ model: 'opaque', question: 'hello', stream: true }))
    .toEqual({
      model: 'opaque',
      messages: [{ role: 'user', content: 'hello' }],
      stream: true
    })
})

describe('playground attachment classification', () => {
  it('accepts bounded text and structured-text files', () => {
    expect(classifyPlaygroundAttachment('notes.md', 'text/markdown')).toEqual({ accepted: true })
    expect(classifyPlaygroundAttachment('config.json', 'application/json')).toEqual({ accepted: true })
    expect(classifyPlaygroundAttachment('main.rs', '')).toEqual({ accepted: true })
  })

  it('rejects binary and image files before File.text()', () => {
    expect(classifyPlaygroundAttachment('photo.png', 'image/png')).toEqual({
      accepted: false,
      message: '当前训练场仅支持文本附件'
    })
    expect(classifyPlaygroundAttachment('archive.zip', 'application/zip').accepted).toBe(false)
  })
})
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `rtk npm --prefix frontend exec vitest run tests/utils/playground.spec.ts`

Expected: FAIL because the classifier is missing and existing tests still assume populated optional controls.

- [ ] **Step 3: Implement the classifier and preserve optional payload fields**

Add:

```ts
const TEXT_EXTENSIONS = new Set([
  'txt', 'md', 'markdown', 'json', 'jsonl', 'yaml', 'yml', 'xml', 'csv',
  'ts', 'tsx', 'js', 'jsx', 'vue', 'rs', 'py', 'go', 'java', 'kt', 'toml',
  'ini', 'conf', 'sh', 'sql', 'html', 'css'
])

export const classifyPlaygroundAttachment = (name: string, mime: string) => {
  const normalizedMime = mime.trim().toLowerCase()
  const extension = name.split('.').pop()?.toLowerCase() ?? ''
  const accepted = normalizedMime.startsWith('text/')
    || ['application/json', 'application/xml', 'application/yaml'].includes(normalizedMime)
    || TEXT_EXTENSIONS.has(extension)
  return accepted
    ? { accepted: true as const }
    : { accepted: false as const, message: '当前训练场仅支持文本附件' }
}
```

Keep `buildPlaygroundChatPayload` optional field insertion conditional exactly as it is; callers now pass `undefined` in automatic mode. Update old tests so explicit values still serialize and omitted values do not.

- [ ] **Step 4: Run the focused test and verify GREEN**

Run: `rtk npm --prefix frontend exec vitest run tests/utils/playground.spec.ts`

Expected: PASS for minimal defaults, explicit optional controls, and attachment rejection.

- [ ] **Step 5: Commit minimal request helpers**

```bash
rtk git add frontend/src/utils/playground.ts frontend/tests/utils/playground.spec.ts
rtk git commit -m "fix(portal): keep playground requests broadly compatible"
```

### Task 3: Integrate The Reliable Playground State

**Files:**
- Modify: `frontend/src/views/portal/Playground.vue`
- Test: `frontend/tests/utils/playground.spec.ts`

- [ ] **Step 1: Change optional controls to explicit automatic state**

Keep the existing concrete control values but gate serialization with switches:

```ts
const temperature = ref(0.7)
const maxTokens = ref(16384)
const inferenceStrength = ref<(typeof inferenceStrengthOptions)[number]>('high')
const temperatureEnabled = ref(false)
const maxTokensEnabled = ref(false)
const inferenceStrengthEnabled = ref(false)
```

For each sidebar control add a compact switch labelled `自动`/`自定义`; disable
the slider/input/select while its switch is off. Pass values to the builder only
when enabled:

```ts
temperature: temperatureEnabled.value ? temperature.value : undefined,
maxTokens: maxTokensEnabled.value ? maxTokens.value : undefined,
inferenceStrength: inferenceStrengthEnabled.value ? inferenceStrength.value : undefined,
```

- [ ] **Step 2: Replace the three-stage model fallback with one live fetch**

Replace `loadModels`, `loadGatewayModels`, and `fallbackToPortalModelStats` with:

```ts
const loadModels = async () => {
  const allowlist = await fetchPortalModelAllowlist()
  const response = await fetch(buildGatewayModelsEndpoint(gatewayBaseUrl.value), {
    headers: { Authorization: `Bearer ${downstreamKey.value}` }
  })
  if (!response.ok) throw new Error(await safeGetText(response))
  modelOptions.value = selectPlayableModels(allowlist, await response.json())
  if (modelOptions.value.length === 0) {
    throw new Error('当前下游没有可路由模型')
  }
  setStatus('实时模型列表已加载', 'success')
}
```

In `loadInitialData`, catch this error, clear `selectedModel`, show the message, and leave sending disabled. Delete `fallbackToPortalModelStats` and the call to `portalApi.getModels` from this page only.

- [ ] **Step 3: Reject binary attachments before reading**

At the start of each `files.map` callback, after the size check and before `file.text()`:

```ts
const classification = classifyPlaygroundAttachment(file.name, file.type)
if (!classification.accepted) {
  return {
    uid: `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
    name: file.name,
    size: file.size,
    type: file.type,
    content: '',
    isError: true,
    errorMessage: classification.message
  }
}
```

- [ ] **Step 4: Reject terminal empty streams**

After stream consumption and before `buildPlaygroundAssistantResult`:

```ts
const finalReasoning = streamingReasoning.value
if (!finalContent.trim() && !finalReasoning.trim()) {
  throw new Error('模型返回空响应，请更换模型或检查上游兼容性')
}
```

- [ ] **Step 5: Run frontend tests and build**

Run: `rtk npm --prefix frontend exec vitest run tests/utils/playground.spec.ts`

Expected: PASS.

Run: `rtk npm --prefix frontend run build`

Expected: PASS with no Vue/TypeScript error.

- [ ] **Step 6: Commit the page integration**

```bash
rtk git add frontend/src/views/portal/Playground.vue frontend/tests/utils/playground.spec.ts
rtk git commit -m "fix(portal): make the model playground route-aware"
```

### Task 4: Make Playground E2E Credential-Safe

**Files:**
- Modify: `scripts/portal_playground_e2e.sh`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Write failing script contract tests**

Add to `tests/scripts.rs`:

```rust
#[test]
fn portal_playground_e2e_never_rotates_or_prints_downstream_keys() {
    let script = fs::read_to_string("scripts/portal_playground_e2e.sh").unwrap();
    assert!(script.contains(": \"${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}\""));
    assert!(!script.contains("/rotate"));
    assert!(!script.contains("rotate_downstream_key"));
    assert!(!script.contains("ADMIN_PASSWORD"));
    assert!(script.contains("set +x"));
}

#[test]
fn portal_playground_e2e_uses_live_models_without_hardcoded_candidates() {
    let script = fs::read_to_string("scripts/portal_playground_e2e.sh").unwrap();
    assert!(script.contains("$BASE_URL/v1/models"));
    assert!(!script.contains("extra_default"));
    assert!(!script.contains("deepseek-chat"));
}
```

- [ ] **Step 2: Run the script test and verify RED**

Run: `rtk cargo test --test scripts portal_playground -- --nocapture`

Expected: FAIL because the script logs in as admin, rotates the key, and has hardcoded candidates.

- [ ] **Step 3: Rewrite preflight around a caller-provided key**

At the top use:

```bash
set -euo pipefail
set +x

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
TIMEOUT_SEC="${TIMEOUT_SEC:-60}"
```

Delete dotenv/admin login/key rotation/portal login functions. Fetch `/v1/models` with `DOWNSTREAM_KEY`, iterate only returned IDs, and use the minimal payload:

```bash
payload="$(jq -nc --arg model "$model" '{
  model: $model,
  messages: [{role:"user",content:"Reply with exactly PLAYGROUND_OK"}],
  stream: true
}')"
```

Log only model slug, status, duration, meaningful-frame count, error category, and terminal presence. Never log response content or the key.

- [ ] **Step 4: Run focused tests and syntax check**

Run: `rtk cargo test --test scripts portal_playground -- --nocapture`

Expected: PASS.

Run: `rtk bash -n scripts/portal_playground_e2e.sh`

Expected: exit 0.

- [ ] **Step 5: Commit the safe E2E**

```bash
rtk git add scripts/portal_playground_e2e.sh tests/scripts.rs
rtk git commit -m "test(portal): keep playground smoke non-mutating"
```

### Task 5: Remove Portal Troubleshooting Without Touching Admin Diagnostics

**Files:**
- Modify: `tests/troubleshooting.rs`
- Modify: `frontend/tests/router/index.spec.ts`
- Modify: `frontend/tests/api/portal.spec.ts`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/troubleshooting.rs`
- Modify: `frontend/src/router/index.ts`
- Modify: `frontend/src/views/portal/Portal.vue`
- Modify: `frontend/src/api/portal.ts`
- Delete: `frontend/src/views/portal/Troubleshooting.vue`

- [ ] **Step 1: Write backend route-absence tests first**

Replace `portal_troubleshooting_requires_auth` with:

```rust
#[tokio::test]
async fn portal_troubleshooting_routes_are_not_registered() {
    let (app, _, _) = app_with_model_state();
    for (method, uri) in [
        (Method::POST, "/api/portal/troubleshooting/run"),
        (Method::GET, "/api/portal/troubleshooting/active-requests"),
    ] {
        let response = app.clone().oneshot(
            Request::builder().method(method).uri(uri).body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
```

Delete the remaining tests whose names begin with `portal_troubleshooting_`. Keep admin matrix, semantic validator, route capture, and shared diagnostic tests.

- [ ] **Step 2: Write frontend absence tests**

In `frontend/tests/router/index.spec.ts` assert:

```ts
expect(router.getRoutes().some(route => route.name === 'PortalTroubleshooting')).toBe(false)
expect(router.getRoutes().some(route => route.name === 'AdminTroubleshooting')).toBe(true)
```

Delete only the two portal troubleshooting request tests from `frontend/tests/api/portal.spec.ts`.

- [ ] **Step 3: Run focused tests and verify RED**

Run: `rtk cargo test --test troubleshooting portal_troubleshooting_routes_are_not_registered -- --nocapture`

Expected: FAIL because routes are still registered.

Run: `rtk npm --prefix frontend exec vitest run tests/router/index.spec.ts tests/api/portal.spec.ts`

Expected: router test FAIL because the portal route exists.

- [ ] **Step 4: Remove the portal-only production surface**

Delete both route registrations from `build_router`. Delete
`portal_troubleshooting_run`, `portal_troubleshooting_active_requests`, and
`extract_portal_downstream_id_from_bearer`; graph/reference search confirms the helper has only those two callers.

Remove the child route, portal menu item, title-map entry, portal API methods,
and troubleshooting-only imports. Delete the wrapper view. Do not remove shared
troubleshooting types, `TroubleshootingCenter`, admin routes, or admin handlers.

- [ ] **Step 5: Verify GREEN and absence**

Run: `rtk cargo test --test troubleshooting -- --nocapture`

Expected: PASS.

Run: `rtk npm --prefix frontend exec vitest run tests/router/index.spec.ts tests/api/portal.spec.ts`

Expected: PASS.

Run: `rtk rg -n 'PortalTroubleshooting|/portal/troubleshooting|portal_troubleshooting_' frontend/src src tests frontend/tests`

Expected: no matches.

- [ ] **Step 6: Commit portal troubleshooting removal**

```bash
rtk git add src/server/gateway.rs src/server/gateway/troubleshooting.rs tests/troubleshooting.rs frontend/src frontend/tests
rtk git commit -m "feat(portal): remove troubleshooting surface"
```

### Task 6: Portal Regression Gate

**Files:**
- No source changes expected.

- [ ] **Step 1: Run all frontend tests**

Run: `rtk npm --prefix frontend exec vitest run`

Expected: all frontend test files pass.

- [ ] **Step 2: Build the frontend**

Run: `rtk npm --prefix frontend run build`

Expected: exit 0.

- [ ] **Step 3: Run affected Rust suites**

Run: `rtk cargo test --test troubleshooting --test scripts --test portal_api -- --nocapture`

Expected: all selected suites pass.

- [ ] **Step 4: Verify the worktree is ready for the next plan**

Run: `rtk git status --short`

Expected: no uncommitted files from this plan.
