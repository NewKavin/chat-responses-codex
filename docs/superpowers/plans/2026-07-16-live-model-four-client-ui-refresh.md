# Live Model, Four-Client, And UI Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Qualify and configure every live upstream model, validate all exposed models through Codex, OpenCode, Claude Code, and Hermes, remove portal troubleshooting, and ship a restrained relay-console UI refresh.

**Architecture:** Keep the admin troubleshooting runner and existing pairwise protocol adapters. Add one authenticated admin qualification workflow that separates model listing from real inference and applies only successful per-key mappings. Remove only the portal troubleshooting wrappers. Refresh the shared Vue shells through one token stylesheet and responsive navigation without rewriting business pages.

**Tech Stack:** Rust 2021, Axum 0.8, Tokio, reqwest, Serde, Vue 3, TypeScript, Element Plus, Vitest, Bash/jq, Playwright.

---

## File Map

- Modify `src/state/model_discovery.rs`: bounded real-inference qualification helpers and sanitized result types.
- Modify `src/server/admin.rs`: authenticated all-upstream qualification handler and apply logic.
- Modify `src/server/gateway.rs`: qualification route; remove portal troubleshooting routes.
- Modify `src/server/gateway/troubleshooting.rs`: four-client matrix; remove portal-only wrappers.
- Modify `tests/admin_upstreams.rs`: mock qualification and apply tests.
- Modify `tests/troubleshooting.rs`: four-client defaults and removed portal route contract.
- Modify `scripts/compatibility_matrix.sh`: four-client default and fail-on-failed-cell behavior.
- Create `scripts/installed_client_smoke.sh`: real CLI orchestration with sanitized evidence.
- Modify `tests/scripts.rs`: shell contract tests.
- Create `frontend/src/styles/console.css`: shared tokens, Element Plus refinements, responsive primitives.
- Modify `frontend/src/main.ts`, `frontend/src/App.vue`, `frontend/src/views/portal/Portal.vue`: shared console shell.
- Modify `frontend/src/views/admin/Login.vue`, `frontend/src/views/portal/PortalLogin.vue`: restrained login layouts.
- Modify `frontend/src/router/index.ts`, `frontend/src/api/portal.ts`: remove portal troubleshooting.
- Delete `frontend/src/views/portal/Troubleshooting.vue`.
- Modify `frontend/src/utils/troubleshooting.ts`, `frontend/tests/router/index.spec.ts`, `frontend/tests/api/portal.spec.ts`, and `frontend/tests/utils/troubleshooting.spec.ts`: four-client and removal contracts.
- Create `docs/verification/2026-07-16-live-model-four-client.md`: sanitized live evidence.

### Task 1: Remove The Portal Troubleshooting Surface

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

- [ ] **Step 1: Write the backend removal contract first**

Replace the first portal troubleshooting authorization test with a route-absence test that does not depend on authentication:

```rust
#[tokio::test]
async fn portal_troubleshooting_routes_are_not_registered() {
    let (app, _, _) = app_with_custom_upstream("http://127.0.0.1:9".to_string());
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

Delete the remaining `portal_troubleshooting_*` endpoint tests. Retain admin matrix tests and shared check behavior tests.

- [ ] **Step 2: Run the Rust test and verify RED**

Run: `rtk cargo test --test troubleshooting portal_troubleshooting_routes_are_not_registered -- --nocapture`

Expected: FAIL because both portal routes are currently registered.

- [ ] **Step 3: Write the frontend route contract and verify RED**

Change `frontend/tests/router/index.spec.ts` to assert:

```ts
expect(routeNames).not.toContain('PortalTroubleshooting')
expect(routeNames).toContain('AdminTroubleshooting')
```

Delete the two portal troubleshooting API request tests from `frontend/tests/api/portal.spec.ts`.

Run: `rtk npm --prefix frontend exec vitest run tests/router/index.spec.ts`

Expected: FAIL because the portal route still exists.

- [ ] **Step 4: Remove the backend and frontend surface**

Remove both portal route registrations from `build_router`. Remove
`portal_troubleshooting_run`, `portal_troubleshooting_active_requests`, and
`extract_portal_downstream_id` after confirming no references remain. Keep
`run_troubleshooting_for_downstream`, route-capture helpers, admin handlers, and
the compatibility matrix.

Remove the portal router child, navigation item, title-map entry, API type
imports, and API methods. Delete the portal wrapper component.

- [ ] **Step 5: Verify GREEN and no dead references**

Run: `rtk cargo test --test troubleshooting -- --nocapture`

Expected: PASS with the two portal endpoints returning 404 and all admin tests green.

Run: `rtk npm --prefix frontend exec vitest run tests/router/index.spec.ts tests/api/portal.spec.ts`

Expected: PASS.

Run: `rtk rg -n 'PortalTroubleshooting|/portal/troubleshooting|portal_troubleshooting_' frontend/src src tests`

Expected: no production or active test matches.

- [ ] **Step 6: Commit the portal removal**

```bash
rtk git add src/server/gateway.rs src/server/gateway/troubleshooting.rs tests/troubleshooting.rs frontend/src frontend/tests
rtk git commit -m "feat: remove portal troubleshooting surface"
```

### Task 2: Make The Admin Matrix Cover Four Clients

**Files:**
- Modify: `tests/troubleshooting.rs`
- Modify: `src/server/gateway/troubleshooting.rs`
- Modify: `frontend/tests/utils/troubleshooting.spec.ts`
- Modify: `frontend/src/utils/troubleshooting.ts`
- Modify: `scripts/compatibility_matrix.sh`
- Modify: `tests/scripts.rs`

- [ ] **Step 1: Change matrix tests to the required four-client contract**

Change `admin_compatibility_matrix_runs_for_all_exposed_models` to omit
`client_profiles` and assert:

```rust
assert_eq!(
    payload["client_profiles"],
    json!(["codex", "opencode", "claude_code", "hermes"])
);
assert_eq!(payload["cells"].as_array().unwrap().len(), 4);
```

Replace `admin_compatibility_matrix_rejects_unsupported_client_profiles` with:

```rust
#[tokio::test]
async fn admin_compatibility_matrix_accepts_claude_code() {
    let capture = Arc::new(Mutex::new(Vec::<CapturedDiagnosticRequest>::new()));
    let upstream = spawn_diagnostic_upstream(capture).await;
    let (app, _, downstream_id) = app_with_custom_upstream(upstream);
    let admin_token = generate_admin_token("admin", "test_secret").unwrap();
    let response = app.oneshot(Request::builder()
        .method(Method::POST)
        .uri("/api/admin/troubleshooting/matrix/run")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "downstream_id": downstream_id,
            "client_profiles": ["claude_code"]
        }).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 2: Run the matrix tests and verify RED**

Run: `rtk cargo test --test troubleshooting compatibility_matrix -- --nocapture`

Expected: FAIL because the default omits Claude Code and validation rejects it.

- [ ] **Step 3: Add Claude Code to backend and frontend defaults**

Use this backend default order and allowlist:

```rust
let client_profiles = if body.client_profiles.is_empty() {
    vec![
        TroubleshootingClientProfile::Codex,
        TroubleshootingClientProfile::Opencode,
        TroubleshootingClientProfile::ClaudeCode,
        TroubleshootingClientProfile::Hermes,
    ]
} else {
    body.client_profiles
};
```

The supported profile match contains those same four variants. Change
`matrixClientProfiles` to:

```ts
export const matrixClientProfiles: TroubleshootingClientProfile[] = [
  'codex', 'opencode', 'claude_code', 'hermes'
]
```

- [ ] **Step 4: Make the shell runner fail on failed cells**

Set the script default to:

```bash
CLIENTS_JSON="${CLIENTS_JSON:-[\"codex\",\"opencode\",\"claude_code\",\"hermes\"]}"
```

After writing the response, require:

```bash
jq -e '.summary.failed == 0 and ([.cells[].status] | all(. != "failed"))' "$OUTPUT_FILE" >/dev/null
```

Update `tests/scripts.rs` to assert the literal four-client default and the
`jq -e` failure gate.

- [ ] **Step 5: Run backend, frontend, and script verification**

Run: `rtk cargo test --test troubleshooting compatibility_matrix -- --nocapture`

Expected: PASS.

Run: `rtk npm --prefix frontend exec vitest run tests/utils/troubleshooting.spec.ts`

Expected: PASS.

Run: `rtk cargo test --test scripts -- --nocapture`

Expected: PASS.

Run: `rtk bash -n scripts/compatibility_matrix.sh`

Expected: exit 0.

- [ ] **Step 6: Commit four-client defaults**

```bash
rtk git add src/server/gateway/troubleshooting.rs tests/troubleshooting.rs frontend/src/utils/troubleshooting.ts frontend/tests/utils/troubleshooting.spec.ts scripts/compatibility_matrix.sh tests/scripts.rs
rtk git commit -m "feat: cover claude code in compatibility matrix"
```

### Task 3: Qualify And Apply Actually Runnable Upstream Models

**Files:**
- Modify: `src/state/model_discovery.rs`
- Modify: `src/state.rs`
- Modify: `src/server/admin.rs`
- Modify: `src/server/gateway.rs`
- Modify: `tests/admin_upstreams.rs`
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/views/admin/ModelProbe.vue`

- [ ] **Step 1: Write failing model qualification helper tests**

Add local mock cases in `tests/admin_upstreams.rs` where `/v1/models` advertises
`model-ok`, `model-empty`, and `model-error`; Chat Completions returns content
only for `model-ok`. Call the new admin route with `{"apply":true}` and assert:

```rust
assert_eq!(response.status(), StatusCode::OK);
assert_eq!(payload["summary"]["qualified_models"], 1);
assert_eq!(payload["upstreams"][0]["qualified_models"], json!(["model-ok"]));
assert_eq!(payload["upstreams"][0]["results"][1]["error_category"], "empty_response");
assert!(payload.to_string().find("secret-key").is_none());

let stored = state.snapshot().await.upstreams.into_iter()
    .find(|upstream| upstream.id == "qualified-upstream").unwrap();
assert_eq!(stored.route_models(), vec!["model-ok"]);
assert_eq!(stored.api_key_models[0].supported_models, vec!["model-ok"]);
```

Add a second test proving that an upstream with zero successful models is not
mutated when `apply` is true.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `rtk cargo test --test admin_upstreams qualify -- --nocapture`

Expected: FAIL with 404 because the qualification endpoint does not exist.

- [ ] **Step 3: Implement protocol-aware bounded inference probes**

Add public serializable types in `src/state/model_discovery.rs`:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelQualificationResult {
    pub key_prefix: String,
    pub model: String,
    pub protocol: UpstreamProtocol,
    pub status: String,
    pub latency_ms: u64,
    pub error_category: Option<String>,
}
```

Implement `qualify_model_on_upstream` using the exact configured base URL, key,
runtime model slug, and protocol. Chat sends only `model`, one short user
message, and `stream:false`; Responses sends only `model`, a short `input`, and
`stream:false`. Do not send sampling or token-limit fields. Accept only a
successful parseable body containing non-empty text, reasoning, or a structured
tool call. Return sanitized categories `authentication`, `rate_limit`,
`upstream_unavailable`, `request_rejected`, `malformed_response`,
`empty_response`, `timeout`, or `network`.

Bound qualification with `buffer_unordered(4)` and the existing admin upstream
timeout. Never include keys, prompts, output text, URLs, or raw response bodies
in the result.

- [ ] **Step 4: Implement the authenticated qualification-and-apply handler**

Register:

```text
POST /api/admin/upstreams/qualify-models
```

Request:

```json
{"apply": true, "upstream_ids": []}
```

For every selected active upstream and available key, discover models and union
them with the already configured route models. Probe every `(key, model,
protocol)` tuple. Build per-key model lists only from successful results.

When `apply` is true and at least one tuple succeeds, update:

- `api_key` and `api_keys` to successful keys only
- `api_key_models` to successful per-key mappings
- `supported_models` to successful non-premium models
- `premium_models` to the successful subset of prior premium models

If no tuple succeeds, leave that upstream unchanged and return `applied:false`.
Normalize before calling `state.update_upstream`.

- [ ] **Step 5: Add the admin UI action**

Add typed request/response interfaces, `adminApi.qualifyUpstreamModels`, and a
`真实验证并应用` button on `ModelProbe.vue`. Require an Element Plus confirmation
because it sends external inference calls and may narrow route configuration.
Render only sanitized totals and per-upstream pass/failure counts; do not render
keys or raw upstream errors.

- [ ] **Step 6: Verify local qualification behavior**

Run: `rtk cargo test --test admin_upstreams qualify -- --nocapture`

Expected: PASS for successful filtering, no-success preservation, protocol
selection, and secret redaction.

Run: `rtk npm --prefix frontend exec vitest run tests/api/admin.spec.ts`

Expected: PASS after adding the API contract test.

- [ ] **Step 7: Commit model qualification**

```bash
rtk git add src/state/model_discovery.rs src/state.rs src/server/admin.rs src/server/gateway.rs tests/admin_upstreams.rs frontend/src frontend/tests/api/admin.spec.ts
rtk git commit -m "feat: qualify runnable upstream models by inference"
```

### Task 4: Refresh The Console UI Without AI Styling

**Files:**
- Create: `frontend/src/styles/console.css`
- Modify: `frontend/src/main.ts`
- Modify: `frontend/src/App.vue`
- Modify: `frontend/src/views/portal/Portal.vue`
- Modify: `frontend/src/views/admin/Login.vue`
- Modify: `frontend/src/views/portal/PortalLogin.vue`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Modify: `frontend/src/views/admin/Downstreams.vue`

- [ ] **Step 1: Add a static design contract test**

Create a Vitest file that reads the shared stylesheet and shell source, then
asserts:

```ts
expect(css).toContain('--console-accent: #0f8f76')
expect(css).not.toMatch(/linear-gradient|radial-gradient|backdrop-filter/)
expect(css).not.toMatch(/letter-spacing:\s*-/)
expect(adminShell).toContain('mobile-nav-button')
expect(portalShell).toContain('mobile-nav-button')
```

- [ ] **Step 2: Run the contract and verify RED**

Run: `rtk npm --prefix frontend exec vitest run tests/ui/console-style.spec.ts`

Expected: FAIL because the shared stylesheet and mobile navigation do not exist.

- [ ] **Step 3: Add shared tokens and Element Plus refinements**

Create `console.css` with the exact design tokens from the approved design.
Set stable global box sizing, canvas/background, typography, focus rings,
6-8 pixel card radius, one-pixel borders, compact table headers, drawer/dialog
shadows, and responsive page gutters. Import it from `main.ts`.

Do not style page sections as floating cards. Do not add gradients, glowing
backgrounds, bokeh, glass effects, negative letter spacing, or oversized type.

- [ ] **Step 4: Rebuild admin and portal shells**

Use Element Plus icons for every navigation item and familiar icon-only mobile
menu/close actions with tooltips. Group admin navigation into overview,
resources, and operations; group portal navigation into usage and integration.
Keep all route paths unchanged except the removed portal troubleshooting route.

Desktop uses a 216-pixel sidebar. At `max-width: 900px`, hide the fixed sidebar
and expose the same navigation inside `el-drawer`. The topbar stays 56 pixels,
shows the current page title, and never overlaps the drawer button.

- [ ] **Step 5: Align login and list-page visual language**

Use centered, bounded login forms on the neutral canvas with a real product
wordmark rendered as text, no hero marketing copy, and no decorative gradient.
Unify Upstreams/Downstreams page header spacing, table density, drawer label
width, action icon buttons, and empty/loading states through shared classes.

- [ ] **Step 6: Run frontend tests and build**

Run: `rtk npm --prefix frontend exec vitest run`

Expected: PASS.

Run: `rtk npm --prefix frontend run build`

Expected: PASS with Vue and TypeScript compilation clean.

- [ ] **Step 7: Run desktop/mobile browser QA**

Start Vite on a free port and capture admin login, portal login, admin model
probe, upstream list, and portal integration at `1440x1000` and `390x844`.
Assert screenshots are nonblank and inspect them for clipped text, overlapping
navigation, horizontal page overflow, missing active states, and accidental
gradients.

- [ ] **Step 8: Commit the UI refresh**

```bash
rtk git add frontend/src frontend/tests
rtk git commit -m "style: refine relay console interface"
```

### Task 5: Run Live Qualification And Four Real Clients

**Files:**
- Create: `scripts/installed_client_smoke.sh`
- Modify: `tests/scripts.rs`
- Create: `docs/verification/2026-07-16-live-model-four-client.md`

- [ ] **Step 1: Add the installed-client script contract test**

Require the script to name all four clients, check their versions, require
`BASE_URL`, `DOWNSTREAM_KEY`, and `MODEL_SLUG`, use temporary config/home
directories, never echo the key, and emit one sanitized JSON record per run.

- [ ] **Step 2: Implement real client orchestration**

The script runs installed Codex, OpenCode, Claude Code, and Hermes commands in
non-interactive mode. It configures each exclusively with the gateway base URL,
downstream key, and selected exposed slug. It records client/version/model,
text-task status, read-only-task status, duration, and sanitized failure stage.
It stores no key, prompt, model output, tool arguments, or file content.

- [ ] **Step 3: Deploy the current main build without resetting live data**

Run: `rtk bash scripts/deploy.sh`

Expected: the gateway container returns `ok` from `/healthz`; PostgreSQL state
and downstream plaintext keys remain present.

- [ ] **Step 4: Qualify and apply every active upstream model**

Log in through `/api/admin/login`, call
`POST /api/admin/upstreams/qualify-models` with `{"apply":true}`, and save a
sanitized result containing only IDs, model slugs, protocols, status,
latency, timestamp, and error categories.

Call `/v1/models` with the current `test` downstream key and verify every
returned slug appears in at least one successful qualification tuple.

- [ ] **Step 5: Run the four-client matrix across every exposed model**

Run: `rtk env DOWNSTREAM_ID=test scripts/compatibility_matrix.sh`

Expected: exactly `4 * exposed_model_count` cells, all four client profiles are
present for every model, and `summary.failed == 0`.

- [ ] **Step 6: Install exact client versions and run smoke tests**

Use the versions verified by the 2026-07-10 protocol design unless package
registries no longer provide them. Record an explicit version substitution if
an exact version is unavailable. Run the script once per exposed model when
the client supports cheap non-interactive execution; otherwise run all four
clients on one qualified representative model and rely on the full API matrix
for per-model coverage.

- [ ] **Step 7: Record sanitized evidence**

Create `docs/verification/2026-07-16-live-model-four-client.md` with:

- deployed commit and timestamp
- active upstream IDs/names and qualified model slugs
- failed advertised model slugs with sanitized category
- downstream exposed-model list
- model-by-client matrix totals and failures
- exact four client versions and smoke exit status
- frontend screenshot paths and viewport sizes
- verification command/result summary

- [ ] **Step 8: Run the full completion gate**

Run: `rtk cargo fmt --all -- --check`

Run: `rtk cargo clippy --all-targets -- -D warnings`

Run: `rtk cargo test --all-targets -- --nocapture`

Run: `rtk cargo test --manifest-path crates/gateway-core/Cargo.toml --all-targets -- --nocapture`

Run: `rtk npm --prefix frontend exec vitest run`

Run: `rtk npm --prefix frontend run build`

Expected: every command exits 0; only explicitly ignored load tests remain ignored.

- [ ] **Step 9: Commit verification evidence on main**

```bash
rtk git add scripts/installed_client_smoke.sh tests/scripts.rs docs/verification/2026-07-16-live-model-four-client.md
rtk git commit -m "test: verify live models with four clients"
```

## Final Acceptance Checklist

- [ ] Every retained live model has successful real inference evidence.
- [ ] Failed advertised models are excluded from applied route mappings or explicitly reported.
- [ ] The `test` downstream exposes only qualified configured models.
- [ ] Every exposed model has Codex, OpenCode, Claude Code, and Hermes matrix cells.
- [ ] Four real client CLIs complete their required downstream smoke tasks.
- [ ] Portal troubleshooting UI, frontend API, backend routes, wrappers, and tests are absent.
- [ ] Admin troubleshooting and matrix functionality remain available.
- [ ] Admin and portal UI are responsive, dense, consistent, and contain no AI-style gradients or decoration.
- [ ] Rust, frontend, script, browser, and live verification gates pass.
- [ ] Sanitized verification evidence is committed directly on `main`.
