# Manual Reasoning Overrides And Routing Validity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add hot, exact-route reasoning overrides; repair capability-aware Responses fallback; and make model-mapping validity authoritative and visible.

**Architecture:** Extend the existing compiled capability policy with exact key selectors and operator effort fields, then expose narrowly-scoped admin handlers that resolve anonymous route IDs server-side. Route protocol selection and mapping status both consume the same capability resolver used by dispatch, while probe profiles remain untouched evidence.

**Tech Stack:** Rust 2021, Axum, Tokio, serde, PostgreSQL/file state stores, Vue 3, TypeScript, Element Plus, Vitest, Cargo integration tests.

---

## File Structure

- Modify `src/capabilities/types.rs`: add exact key selector and generic reasoning override fields.
- Modify `src/capabilities/policy.rs`: compile, match, validate, rank, and conflict-check the new fields.
- Modify `src/capabilities/resolver.rs`: give override effort control precedence and report its source.
- Modify `src/server/gateway/responses_fallback.rs`: own the pure Responses protocol-strategy decision.
- Modify `src/server/gateway.rs`: move strategy selection after exact capability evaluation and register new admin routes.
- Modify `src/server/gateway/upstream.rs`: distinguish absent models from configured-but-ineligible routes in the terminal error.
- Create `src/server/gateway/reasoning_overrides.rs`: validate exact routes and atomically upsert/clear admin-managed overrides.
- Create `src/server/gateway/model_mapping_status.rs`: calculate backend-authoritative mapping validity.
- Modify `src/server/gateway/capability_admin.rs`: expose effective reasoning source and managed-override state.
- Modify `src/state/freekey_sync.rs`: queue stale capability jobs after PATCH updates.
- Modify `src/state.rs`: reject global aliases that invalidate existing mappings.
- Modify `frontend/src/types/index.ts` and `frontend/src/api/admin.ts`: type and call the new APIs.
- Create `frontend/src/utils/reasoningOverrides.ts`: fixed vocabulary, source labels, and payload normalization.
- Create `frontend/src/utils/modelMappingStatus.ts`: status/reason presentation helpers.
- Modify `frontend/src/views/admin/ModelProbe.vue`: exact-route editor and source display.
- Modify `frontend/src/views/admin/ModelAliases.vue`: consume backend mapping status.

### Task 1: Capability-Aware Responses Protocol Strategy

**Files:**
- Modify: `src/server/gateway/responses_fallback.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/upstream.rs`
- Test: `tests/unit/server/gateway.rs`
- Test: `tests/gateway/responses/fallback.rs`

- [ ] **Step 1: Write failing pure strategy tests**

Add tests that express the decision without network setup:

```rust
#[test]
fn responses_tooling_falls_back_when_only_chat_has_an_eligible_route() {
    assert_eq!(
        responses_route_strategy(true, 0, 1),
        ResponsesRouteStrategy::ChatFallback,
    );
}

#[test]
fn responses_tooling_prefers_an_eligible_responses_route() {
    assert_eq!(
        responses_route_strategy(true, 1, 3),
        ResponsesRouteStrategy::Responses,
    );
}

#[test]
fn responses_tooling_reports_unavailable_when_neither_protocol_is_eligible() {
    assert_eq!(
        responses_route_strategy(true, 0, 0),
        ResponsesRouteStrategy::Unavailable,
    );
}
```

- [ ] **Step 2: Run the focused unit tests and confirm RED**

Run: `rtk cargo test --test unit responses_route_strategy -- --nocapture`

Expected: compilation fails because `responses_route_strategy` and
`ResponsesRouteStrategy` do not exist.

- [ ] **Step 3: Add the pure strategy and use exact eligible counts**

Implement in `responses_fallback.rs`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResponsesRouteStrategy {
    ProtocolAgnostic,
    Responses,
    ChatFallback,
    Unavailable,
}

pub(super) fn responses_route_strategy(
    requires_responses_tooling: bool,
    eligible_responses_routes: usize,
    eligible_chat_routes: usize,
) -> ResponsesRouteStrategy {
    if !requires_responses_tooling {
        ResponsesRouteStrategy::ProtocolAgnostic
    } else if eligible_responses_routes > 0 {
        ResponsesRouteStrategy::Responses
    } else if eligible_chat_routes > 0 {
        ResponsesRouteStrategy::ChatFallback
    } else {
        ResponsesRouteStrategy::Unavailable
    }
}
```

In `gateway.rs`, remove the pre-cache declaration-only
`responses_upstream_available` check. After building
`route_capability_cache`, count eligible entries by `WireProtocol`, select the
strategy, log both counts and its stable reason, and derive `candidate_protocols`
from the enum. Keep protocol-agnostic requests on the current native/opposite
ordering.

- [ ] **Step 4: Add the gateway regression fixture**

In `tests/gateway/responses/fallback.rs`, configure one upstream that declares
both protocols, install a current Chat profile that satisfies the request, and
install a Responses profile that rejects one required capability. Send a
Responses hosted-tool request and assert the Chat mock receives it while the
Responses mock receives zero calls.

- [ ] **Step 5: Improve the terminal no-route message**

Add a unit test where the model exists on an active upstream and assert the
message contains `configured but no exact route is eligible`, then update
`no_routable_model_error` to use that wording while retaining error code
`gateway_no_routable_upstream`. Preserve the current absent-model wording for
models not in any active upstream.

- [ ] **Step 6: Verify GREEN and commit**

Run:

```bash
rtk cargo test --test unit responses_route_strategy -- --nocapture
rtk cargo test --test gateway responses::fallback -- --nocapture
rtk cargo test --test gateway model_mappings::stale_model_mapping_is_skipped_and_revives_without_config_change -- --exact
```

Expected: all focused tests pass.

Commit: `fix(gateway): choose Responses routes by capability eligibility`

### Task 2: Exact-Key Reasoning Override Primitives

**Files:**
- Modify: `src/capabilities/types.rs`
- Modify: `src/capabilities/policy.rs`
- Modify: `src/capabilities/resolver.rs`
- Test: `tests/capability_policy.rs`
- Test: `tests/capability_resolver.rs`

- [ ] **Step 1: Write failing selector and resolver tests**

Add policy tests proving a selector with `key_fingerprint: Some("key-a")`
matches key A and not key B. Add resolver coverage:

```rust
let route_override = RouteCapabilityOverride {
    id: "operator-reasoning-route-a".into(),
    selector: CapabilitySelector {
        key_fingerprint: Some("key-a".into()),
        ..Default::default()
    },
    capabilities: [(Capability::ReasoningOutput, EvidenceState::Supported)].into(),
    reasoning_control_field: Some("reasoning_effort".into()),
    effort_map: BTreeMap::from([
        ("low".into(), json!("low")),
        ("high".into(), json!("high")),
    ]),
    ..Default::default()
};
```

Assert the resolved field/map equal the override and
`field_sources["effort_map"] == CapabilitySource::Override`, even when a probe
profile supplies a different accepted set.

- [ ] **Step 2: Run focused tests and confirm RED**

Run:

```bash
rtk cargo test --test capability_policy key_fingerprint -- --nocapture
rtk cargo test --test capability_resolver effort_override -- --nocapture
```

Expected: compilation fails on the new struct fields.

- [ ] **Step 3: Extend serde-defaulted types**

Add to `CapabilitySelector`:

```rust
#[serde(default)]
pub key_fingerprint: Option<String>,
```

Add to `RouteCapabilityOverride`:

```rust
#[serde(default)]
pub reasoning_control_field: Option<String>,
#[serde(default)]
pub effort_map: BTreeMap<String, Value>,
```

Because both parent structs already use serde defaults, old stored documents
remain readable.

- [ ] **Step 4: Compile and validate the fields**

Update `CompiledSelector` specificity, matching, overlap analysis, and bounded
selector-value validation for `key_fingerprint`. Extend override conflict
analysis so different values for `reasoning_control_field` or the same
`effort_map.<level>` are ambiguous at equal rank. Reject a control field with
an empty map and a nonempty map without a control field.

- [ ] **Step 5: Apply override precedence in the resolver**

Refactor `resolve_effort_control` to compute profile/preset values first, then
walk ordered overrides and replace both field and map whenever an override
provides them. Return `CapabilitySource::Override`; keep `Probe`, `Policy`, and
`Baseline` behavior unchanged otherwise.

- [ ] **Step 6: Add backward-compatible JSON coverage**

Deserialize a pre-change `CapabilityConfiguration` JSON document lacking all
new fields, compile it, and assert selectors and effort maps default empty.

- [ ] **Step 7: Verify GREEN and commit**

Run:

```bash
rtk cargo test --test capability_policy
rtk cargo test --test capability_resolver
rtk cargo test --test capability_profiles
```

Expected: all tests pass.

Commit: `feat(capabilities): support exact-route effort overrides`

### Task 3: Admin Reasoning Override API And Effective Source

**Files:**
- Create: `src/server/gateway/reasoning_overrides.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/capability_admin.rs`
- Test: `tests/admin_capabilities.rs`
- Test: `tests/gateway/capability_routing.rs`

- [ ] **Step 1: Extend the admin fixture with PUT and write failing API tests**

Add `put_json` beside `post_json`. Compute and submit the route identity in the
test:

```rust
let key_fingerprint = upstream_key_fingerprint("up-1", "upstream-secret");
let route_id = anonymous_route_id(
    "up-1",
    &key_fingerprint,
    "opaque",
    WireProtocol::ChatCompletions,
);
let request = json!({
    "upstream_id": "up-1",
    "route_id": route_id,
    "exposed_model_slug": "opaque",
    "runtime_model_slug": "opaque",
    "protocol": "chat_completions",
    "levels": ["low", "high"],
    "scope": "route"
});
```

Assert `PUT /api/admin/capabilities/reasoning-overrides` returns 200, revision
increments once, export contains one
`format!("operator-reasoning-{route_id}")` override,
and discovery returns levels `low/high`, `reasoning_source: "override"`, and
`managed_reasoning_override: true` without exposing a key fingerprint.

Add RED cases for an unknown level, stale route ID, mismatched runtime model,
empty-level clear, and `model_routes` applying to every current key/protocol
route atomically.

- [ ] **Step 2: Run the focused test and confirm RED**

Run: `rtk cargo test --test admin_capabilities admin_reasoning_override -- --nocapture`

Expected: the route returns 404 and the response fields are absent.

- [ ] **Step 3: Implement route resolution and atomic mutation**

Create request types with `#[serde(deny_unknown_fields)]`, a scope enum with
`route` and `model_routes`, and the fixed level order. Resolve current routes
by enumerating active upstreams, effective downstream models, model-owning
keys, and supported protocols. Match the selected anonymous route ID before
using its internal key fingerprint.

Build managed overrides with:

```rust
RouteCapabilityOverride {
    id: format!("operator-reasoning-{route_id}"),
    priority: 1_000_000,
    selector: CapabilitySelector {
        upstream_id: Some(upstream_id),
        key_fingerprint: Some(key_fingerprint),
        runtime_model: Some(runtime_model_slug),
        protocol: Some(protocol),
        ..Default::default()
    },
    capabilities: [(Capability::ReasoningOutput, EvidenceState::Supported)].into(),
    reasoning_control_field: Some("reasoning_effort".into()),
    effort_map: levels.into_iter().map(|level| (level.clone(), json!(level))).collect(),
    ..Default::default()
}
```

Resolve all targets before changing the cloned source configuration. Remove
only matching managed IDs, insert replacements when levels are nonempty,
increment revision once, compile/persist via
`replace_capability_configuration`, and return affected anonymous route IDs.

- [ ] **Step 4: Register the authenticated route**

Add `mod reasoning_overrides`, import its handler, and register:

```rust
.route(
    "/api/admin/capabilities/reasoning-overrides",
    axum::routing::put(admin_update_reasoning_overrides)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(), admin_auth_middleware,
        )),
)
```

- [ ] **Step 5: Add source and ownership to discovery**

Extend `CapabilityRouteDiscoverySummary` with `reasoning_source` and
`managed_reasoning_override`. Read the effective source from
`resolved.field_sources["effort_map"]`; detect the exact managed override from
the route's matched overrides. Keep probe `outcome` unchanged.

- [ ] **Step 6: Prove hot catalog behavior**

Add a gateway test that reads the Codex catalog before the PUT (safe `none`),
applies `low/high`, then reads it again from the same `AppState` and asserts the
two supported levels appear without recreating the router or state.

- [ ] **Step 7: Verify GREEN and commit**

Run:

```bash
rtk cargo test --test admin_capabilities admin_reasoning_override -- --nocapture
rtk cargo test --test gateway capability_routing::codex_catalog -- --nocapture
```

Expected: focused API and catalog tests pass.

Commit: `feat(admin): edit reasoning levels on exact routes`

### Task 4: Upstream PATCH Probe Refresh And Alias Reverse Validation

**Files:**
- Modify: `src/state/freekey_sync.rs`
- Modify: `src/state.rs`
- Test: `tests/runtime_capability_hints.rs`
- Test: `tests/capability_state.rs`

- [ ] **Step 1: Write a failing PATCH queue test**

Install an `mpsc` capability-probe sender, call `update_upstream_by_id` with a
protocol change, receive the submitted `ProbeJobBatch`, and assert it contains
the new Responses route with `ProbeReason::ConfigurationChanged`. Seed a
current profile and add a second case where a remark-only edit yields no new
job within a short timeout.

- [ ] **Step 2: Write the failing alias conflict test**

Persist an upstream mapping `gpt-4 -> gpt-4-premium`, then call
`update_model_aliases` with a rule whose alias is `gpt-4-premium`. Assert the
update fails and the message includes the upstream name, downstream mapping
name, and rule canonical. Assert the old alias registry remains unchanged.

- [ ] **Step 3: Run both tests and confirm RED**

Run:

```bash
rtk cargo test --test runtime_capability_hints patch_queues -- --nocapture
rtk cargo test --test capability_state alias_update_rejects -- --nocapture
```

Expected: no probe batch arrives and the conflicting alias currently succeeds.

- [ ] **Step 4: Submit only stale jobs after PATCH**

After route-health reconciliation in `update_upstream_by_id`, locate the
updated upstream in the fresh routing snapshot, call
`stale_capability_probe_jobs_for_upstreams([upstream], unix_seconds())`, and
submit with `ProbeReason::ConfigurationChanged`. Map runtime coordination and
persistence errors to the existing `UpstreamMutationError` variants.

- [ ] **Step 5: Validate aliases before persistence**

In `update_model_aliases`, compile `ModelAliasRegistry::from_rules`, then run
`validate_model_mappings_against_aliases` for every current upstream before
mutating persisted state. Wrap the existing validation error with upstream name
and the conflicting canonical rule while retaining atomic failure.

- [ ] **Step 6: Verify GREEN and commit**

Run:

```bash
rtk cargo test --test runtime_capability_hints
rtk cargo test --test capability_state alias
rtk cargo test --test admin_upstreams model_mapping
```

Expected: all focused tests pass.

Commit: `fix(state): refresh edited routes and protect mapping aliases`

### Task 5: Backend-Authoritative Model Mapping Status

**Files:**
- Create: `src/server/gateway/model_mapping_status.rs`
- Modify: `src/server/gateway.rs`
- Test: `tests/admin_upstreams.rs`

- [ ] **Step 1: Write failing endpoint cases**

Create upstreams covering active/effective, inactive, stale upstream model,
missing per-key ownership, two protocols with one capability-rejected route,
and all routes capability-rejected. Assert
`GET /api/admin/model-mappings/status` returns:

```json
{
  "mappings": [{
    "upstream_id": "up-effective",
    "upstream_model": "gpt-4",
    "downstream_model": "gpt-4-premium",
    "status": "effective",
    "reason": "eligible_routes_available",
    "eligible_routes": 1,
    "configured_routes": 1,
    "unverified_routes": 0
  }]
}
```

Assert no raw API key or key fingerprint appears.

- [ ] **Step 2: Run the focused test and confirm RED**

Run: `rtk cargo test --test admin_upstreams model_mapping_status -- --nocapture`

Expected: endpoint returns 404.

- [ ] **Step 3: Implement deterministic status calculation**

Define serialized `effective`, `partial`, and `inactive` statuses plus stable
reasons. For each stored mapping:

1. reject inactive upstreams;
2. resolve the current upstream model spelling;
3. enumerate `keys_for_model` and supported protocols;
4. call `resolve_route_capabilities_with_snapshot` with required
   `TextInput + NonStreamingResponse` for every exact route;
5. classify all eligible as effective, some as partial, and none as inactive.

Count current routes without a profile as `unverified_routes`, but do not make
them inactive when baseline capability resolution allows minimal text.

- [ ] **Step 4: Register the authenticated GET route**

Register `/api/admin/model-mappings/status` alongside the model-alias admin
routes and keep CRUD unchanged.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
rtk cargo test --test admin_upstreams model_mapping_status -- --nocapture
rtk cargo test --test gateway model_mappings -- --nocapture
```

Expected: status and existing routing tests pass.

Commit: `feat(admin): report effective model mapping routes`

### Task 6: Frontend API And Pure Presentation Helpers

**Files:**
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Create: `frontend/src/utils/reasoningOverrides.ts`
- Create: `frontend/src/utils/reasoningOverrides.spec.ts`
- Create: `frontend/src/utils/modelMappingStatus.ts`
- Create: `frontend/src/utils/modelMappingStatus.spec.ts`
- Test: `frontend/tests/api/admin.spec.ts`

- [ ] **Step 1: Write failing API contract tests**

Assert `adminApi.updateReasoningOverrides(payload)` issues a PUT to
`/admin/capabilities/reasoning-overrides`, and
`adminApi.getModelMappingStatuses()` issues a GET to
`/admin/model-mappings/status`.

- [ ] **Step 2: Write failing pure helper tests**

Assert the effort options are exactly
`low/medium/high/xhigh/max`, normalization preserves fixed order and removes
duplicates, source `override` labels as `手工`, and mapping status/reason codes
produce the expected Chinese labels. Unknown backend reasons must return a
neutral `状态未知`/raw-code tooltip instead of claiming success.

- [ ] **Step 3: Run tests and confirm RED**

Run:

```bash
rtk npm test -- --run tests/api/admin.spec.ts src/utils/reasoningOverrides.spec.ts src/utils/modelMappingStatus.spec.ts
```

Workdir: `frontend`

Expected: imports and methods do not exist.

- [ ] **Step 4: Add strict TypeScript contracts and helpers**

Add request/response types mirroring the Rust snake_case JSON, extend discovery
routes with `reasoning_source` and `managed_reasoning_override`, and implement
the two API methods. Keep helper functions exhaustive over known unions and
neutral for unknown reason strings.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
rtk npm test -- --run tests/api/admin.spec.ts src/utils/reasoningOverrides.spec.ts src/utils/modelMappingStatus.spec.ts
rtk npm run type-check
```

Workdir: `frontend`

Expected: focused tests and type checking pass.

Commit: `feat(frontend): add route override and mapping status clients`

### Task 7: Reasoning Route Editor UI

**Files:**
- Modify: `frontend/src/views/admin/ModelProbe.vue`
- Test: `frontend/tests/views/admin-ui.spec.ts`
- Test: `frontend/src/utils/capabilityDiscovery.spec.ts`

- [ ] **Step 1: Write failing view contract assertions**

Assert the exact-route table contains `生效档位`, a source column, an edit icon
button with an accessible tooltip, the override dialog, the `route` /
`model_routes` scope control, five effort checkboxes, and a clear action. Assert
probe outcome remains rendered separately.

- [ ] **Step 2: Run the focused test and confirm RED**

Run: `rtk npm test -- --run tests/views/admin-ui.spec.ts`

Workdir: `frontend`

Expected: new labels and dialog are absent.

- [ ] **Step 3: Implement the editor**

Add an operation column using Lucide `Pencil` and `RotateCcw` icons with
tooltips. The dialog locks route identity, binds a checkbox group to the five
levels, defaults scope to `route`, and uses a compact segmented/radio mode
control compatible with Element Plus 2.6.3. Saving calls the new PUT, reports
the affected route count, closes the dialog, and reloads discovery. Clearing
sends an empty level array after confirmation.

Render source tags as `手工`, `探测`, `预设`, or `未配置`. When the source is
override, the route status text is `手工生效`; do not mutate the probe outcome
or display it as verified.

- [ ] **Step 4: Verify responsive table constraints**

Keep stable widths for source and operation columns, preserve horizontal
scrolling inside the existing `crc-table-shell`, and use a dialog width with
`max-width: calc(100vw - 32px)` so controls do not overflow mobile.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
rtk npm test -- --run tests/views/admin-ui.spec.ts src/utils/capabilityDiscovery.spec.ts
rtk npm run type-check
rtk npm run build
```

Workdir: `frontend`

Expected: view tests, type check, and production build pass.

Commit: `feat(frontend): edit effective reasoning levels by route`

### Task 8: Model Mapping Status UI

**Files:**
- Modify: `frontend/src/views/admin/ModelAliases.vue`
- Test: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Write failing mapping-status assertions**

Assert `ModelAliases.vue` loads `getModelMappingStatuses`, joins rows by
upstream/upstream-model/downstream-model, displays `生效` / `部分生效` /
`未生效`, and displays `状态未知` when the status request fails. Assert the old
`stale: !models.some(...)` authority is removed.

- [ ] **Step 2: Run the focused test and confirm RED**

Run: `rtk npm test -- --run tests/views/admin-ui.spec.ts`

Workdir: `frontend`

Expected: backend status method is not used and the stale calculation remains.

- [ ] **Step 3: Load and join authoritative statuses**

Fetch upstream configs and mapping status together during `reloadAll`. Store a
status lookup keyed by normalized mapping identity. Populate each `MappingRow`
with backend status, reason, and counts. If only the status call fails, retain
editable mapping rows but mark every row unknown and show a nonblocking warning.

- [ ] **Step 4: Render stable tags and tooltips**

Use success/warning/danger/info tags for effective/partial/inactive/unknown.
Tooltip text includes the backend reason and `eligible_routes/configured_routes`.
Do not derive success from the local model list.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
rtk npm test -- --run tests/views/admin-ui.spec.ts
rtk npm run type-check
rtk npm run build
```

Workdir: `frontend`

Expected: mapping UI tests and builds pass.

Commit: `feat(frontend): show authoritative mapping validity`

### Task 9: Full Verification And Deployment

**Files:**
- Modify only if verification exposes a scoped defect.
- Verify: `DEPLOYMENT.md`, current Docker compose labels, health endpoint, logs,
  and PostgreSQL capability revision.

- [ ] **Step 1: Run formatting and static checks**

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: exit 0 with no warnings.

- [ ] **Step 2: Run the complete backend suite**

Run: `rtk cargo test --all-targets --all-features`

Expected: zero failed tests.

- [ ] **Step 3: Run the complete frontend suite and build**

```bash
rtk npm test
rtk npm run type-check
rtk npm run build
```

Workdir: `frontend`

Expected: zero failed tests and a successful Vite production build.

- [ ] **Step 4: Inspect the final diff and requirement checklist**

```bash
rtk git diff --check
rtk git status --short
rtk git log --oneline --decorate -10
```

Confirm each design goal has a test and no raw key/fingerprint is present in
new admin responses.

- [ ] **Step 5: Build and deploy using the repository procedure**

Read the current deployment section in `DEPLOYMENT.md`, resolve the active
compose working directory from container labels, build a tagged image, and
restart only `chat-responses-codex`. Do not mutate upstream protocol settings.

- [ ] **Step 6: Run post-deploy smoke checks**

Verify:

```bash
rtk curl -fsS http://127.0.0.1:3000/healthz
rtk docker ps --format {{.Names}}\t{{.Status}}\t{{.Ports}}
rtk docker logs --since 5m --tail 300 chat-responses-codex
```

Query PostgreSQL read-only for the capability configuration revision and the
last five minutes of gateway error categories. Confirm the service loaded the
existing configuration, no startup/persistence errors appeared, and
`gateway_no_routable_upstream` did not start recurring spontaneously.

- [ ] **Step 7: Perform a non-destructive admin smoke test**

Using an authenticated admin session, read discovery and mapping statuses.
Apply and then clear a route override only on a current test/non-production
route if one exists; otherwise rely on the automated hot-swap test and do not
alter a production route. Confirm the UI loads and the new APIs return no
secrets.

- [ ] **Step 8: Record final deployment evidence**

Add a concise verification note under `docs/verification/` containing the image
ID, commit, commands, pass counts, health response, and any smoke-test limitation.
Commit it with `docs: record reasoning override deployment verification`.
