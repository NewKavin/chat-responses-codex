# Configurable Multi-Key And Capability Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make per-Key upstream capacity operator-configurable, distribute ordinary requests across eligible Keys, expose stable gateway error codes to clients, and make revision-zero capability discovery work consistently in every deployment mode.

**Architecture:** Keep `UpstreamConfig.max_concurrency` as the persisted per-upstream value that existing exact-account admission applies independently to `(upstream_id, key_fingerprint)`. Add a runtime setting used only as the default for future upstream creation, rotate request-local Key candidates deterministically while preserving exact continuation preference, and add client-only error formatting plus explicit capability-probe preparation errors at their existing boundaries.

**Tech Stack:** Rust/Axum/Tokio/Serde, Vue 3/TypeScript/Element Plus, Vitest, Cargo integration tests, Docker Compose.

---

## File Map

- `src/state/types.rs`: canonical application defaults and persisted upstream configuration.
- `src/state/runtime_settings.rs`: managed runtime-settings schema, compatibility default, validation, and apply metadata.
- `src/state.rs`: current runtime settings, exact-account admission, capability bootstrap, and manual probe preparation.
- `src/server/admin.rs`: single and batch upstream administration contracts.
- `src/server/gateway.rs`: request-local route and Key candidate construction.
- `src/server/gateway/errors.rs`: terminal gateway error aggregation and JSON/Anthropic serialization.
- `src/server/gateway/stream.rs`: Chat and Responses SSE error serialization.
- `src/server/gateway/capability_admin.rs`: capability probe HTTP error mapping.
- `src/server/gateway/capability_routing.rs`: continuation preference and route identity helpers.
- `src/main.rs`: process environment defaults and startup metadata logging.
- `frontend/src/types/index.ts`: runtime settings and upstream API types.
- `frontend/src/api/admin.ts`: batch upstream payload contract.
- `frontend/src/utils/runtimeSettings.ts`: Settings-page catalog and client validation.
- `frontend/src/views/admin/Upstreams.vue`: create, copy, edit, and batch form behavior.
- `Dockerfile`, `docker-compose.yml`, `.env.example`, `README.md`, `DEPLOYMENT.md`: deployment-default contract.
- `tests/runtime_settings.rs`, `tests/admin_runtime_settings.rs`, `tests/admin_upstreams.rs`: backend configuration coverage.
- `tests/gateway/chat/routing.rs`, `tests/gateway/chat/core.rs`, `tests/gateway/claude.rs`, `tests/gateway/responses/upstream_feedback.rs`: routing and client-error coverage.
- `tests/capability_state.rs`, `tests/admin_capabilities.rs`, `tests/docker.rs`: capability and deployment coverage.
- `frontend/src/utils/runtimeSettings.spec.ts`, `frontend/src/api/adminRuntimeSettings.spec.ts`, `frontend/tests/views/admin-ui.spec.ts`: frontend contract coverage.

### Task 1: Add The Global Creation Default To Runtime Settings

**Files:**
- Modify: `src/state/types.rs`
- Modify: `src/state/runtime_settings.rs`
- Modify: `tests/runtime_settings.rs`
- Modify: `tests/admin_runtime_settings.rs`
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/utils/runtimeSettings.ts`
- Modify: `frontend/src/utils/runtimeSettings.spec.ts`
- Modify: `frontend/src/api/adminRuntimeSettings.spec.ts`

- [ ] **Step 1: Write failing Rust compatibility, metadata, validation, and round-trip tests**

Add assertions that an old settings document with `default_upstream_max_concurrency` removed deserializes to the canonical upstream default, that the field appears exactly once in `IMMEDIATE_RUNTIME_SETTING_FIELDS`, and that zero fails validation with field `default_upstream_max_concurrency`.

```rust
let mut json = serde_json::to_value(RuntimeSettingsDocument::startup(&config)).unwrap();
json["settings"]
    .as_object_mut()
    .unwrap()
    .remove("default_upstream_max_concurrency");
let loaded: RuntimeSettingsDocument = serde_json::from_value(json).unwrap();
assert_eq!(loaded.settings.default_upstream_max_concurrency, 4);

let error = RuntimeSettings {
    default_upstream_max_concurrency: 0,
    ..RuntimeSettings::from_app_config(&config)
}
.validate_and_normalize()
.unwrap_err();
assert_eq!(error.field(), "default_upstream_max_concurrency");
```

Extend the admin runtime-settings GET/PUT test fixture so a saved value such as `7` is returned and immediately visible from `state.runtime_settings()`.

- [ ] **Step 2: Run the focused Rust tests and confirm the new field is missing**

Run: `rtk cargo test --test runtime_settings --test admin_runtime_settings`

Expected: FAIL because `RuntimeSettings` has no `default_upstream_max_concurrency` field and compatibility deserialization cannot supply it.

- [ ] **Step 3: Implement the canonical default and runtime-settings field**

Expose one shared default function from `src/state/types.rs`, retain it on `UpstreamConfig.max_concurrency`, and use it as the Serde compatibility default for the new runtime setting.

```rust
pub const DEFAULT_UPSTREAM_MAX_CONCURRENCY: u32 = 4;

pub fn default_upstream_max_concurrency() -> u32 {
    DEFAULT_UPSTREAM_MAX_CONCURRENCY
}

#[serde(default = "default_upstream_max_concurrency")]
pub max_concurrency: u32,
```

Add the immediate setting, its compatibility default, conversion from and application to `AppConfig`, and positive validation.

```rust
#[serde(default = "default_upstream_max_concurrency")]
pub default_upstream_max_concurrency: u32,

require_positive_u32(
    self.default_upstream_max_concurrency,
    "default_upstream_max_concurrency",
)?;
```

Set `AppConfig::default().default_upstream_max_concurrency` to the same constant. The setting affects future administration calls only; it must not rewrite loaded upstream records.

- [ ] **Step 4: Add the frontend type, catalog entry, and validation coverage**

Add `default_upstream_max_concurrency: number` to `RuntimeSettings`, then add this immediate numeric field to the `concurrency` group:

```ts
{
  key: 'default_upstream_max_concurrency',
  group: 'concurrency',
  label: '新建上游每 Key 默认最大并发',
  apply: 'immediate',
  control: 'number',
  unit: '路',
  min: 1,
  max: MAX_U32
}
```

Update test fixtures and assert `validateRuntimeSettings` rejects zero for that field.

- [ ] **Step 5: Run focused backend and frontend tests**

Run: `rtk cargo test --test runtime_settings --test admin_runtime_settings`

Run: `rtk npm --prefix frontend test -- runtimeSettings.spec.ts adminRuntimeSettings.spec.ts`

Expected: PASS; metadata is complete and disjoint, old JSON loads with `4`, and PUT publishes the configured value.

- [ ] **Step 6: Commit the runtime-settings contract**

```bash
rtk git add src/state/types.rs src/state/runtime_settings.rs tests/runtime_settings.rs tests/admin_runtime_settings.rs frontend/src/types/index.ts frontend/src/utils/runtimeSettings.ts frontend/src/utils/runtimeSettings.spec.ts frontend/src/api/adminRuntimeSettings.spec.ts
rtk git commit -m "feat(settings): configure default per-key concurrency"
```

### Task 2: Unify Single, Batch, Copy, And Edit Upstream Capacity

**Files:**
- Modify: `src/server/admin.rs`
- Modify: `src/state.rs`
- Modify: `src/state/freekey_sync.rs`
- Modify: `tests/admin_upstreams.rs`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Modify: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Write failing backend administration tests**

Add endpoint tests for these exact cases:

```rust
// A legacy batch payload omits max_concurrency after runtime settings set it to 7.
assert_eq!(created.max_concurrency, 7);

// Explicit values win in both single and batch creation.
assert_eq!(single.max_concurrency, 3);
assert_eq!(batch.max_concurrency, 5);

// Zero is rejected consistently.
assert_eq!(single_response.status(), StatusCode::BAD_REQUEST);
assert_eq!(batch_response.status(), StatusCode::BAD_REQUEST);

// Updating the global default does not mutate an existing upstream.
assert_eq!(existing.max_concurrency, 3);
```

Use the real batch route instead of constructing `UpstreamConfig` directly. Include two Keys in the payload so the test exercises the production multi-Key path.

- [ ] **Step 2: Run the upstream tests and confirm the legacy batch uses 10 or accepts zero**

Run: `rtk cargo test --test admin_upstreams`

Expected: FAIL on the new assertions because batch creation owns a separate default of `10`, and create/update validation is inconsistent.

- [ ] **Step 3: Make the batch field optional and resolve it from live runtime settings**

Change the batch request field to:

```rust
#[serde(default)]
max_concurrency: Option<u32>,
```

Resolve it once after reading the current runtime document:

```rust
let max_concurrency = payload
    .max_concurrency
    .unwrap_or(runtime.settings.default_upstream_max_concurrency);
if max_concurrency == 0 {
    return bad_request("max_concurrency must be greater than zero");
}
```

Persist `max_concurrency` in the constructed `UpstreamConfig`. Remove the batch-only `default_max_concurrency()` function and every implicit `10` fallback.

Validate `UpstreamConfig.max_concurrency > 0` in the shared single-create/update state boundary so all administration paths reject zero while existing nonzero values remain unchanged.

- [ ] **Step 4: Write failing frontend source and payload tests**

Assert the Upstreams drawer contains a required numeric control labelled `每 Key 最大并发`, does not delete `submitData.max_concurrency`, and includes the normalized number in both single and batch payloads.

```ts
expect(source).toContain('label="每 Key 最大并发"')
expect(source).toContain('max_concurrency: Number(form.value.max_concurrency)')
expect(source).not.toContain('delete submitData.max_concurrency')
```

- [ ] **Step 5: Implement the Upstreams form contract**

Add `max_concurrency?: number` to `BatchCreateUpstreamPayload`. Load runtime settings when opening a blank new upstream and initialize it from `settings.default_upstream_max_concurrency`. Copy and edit modes must preserve `row.max_concurrency`; copying changes identity and credentials but must not silently change the copied capacity.

Add the Element Plus numeric control with stable width and a minimum of one:

```vue
<el-form-item label="每 Key 最大并发" prop="max_concurrency">
  <el-input-number
    v-model="form.max_concurrency"
    :min="1"
    :max="4294967295"
    :step="1"
    controls-position="right"
  />
</el-form-item>
```

Normalize and submit the same value in all paths:

```ts
submitData.max_concurrency = Number(form.value.max_concurrency)

const batchPayload: BatchCreateUpstreamPayload = {
  // existing fields
  max_concurrency: Number(form.value.max_concurrency)
}
```

Copy and edit modes preserve the source upstream's persisted value. Only a blank create uses the current global default.

- [ ] **Step 6: Run focused upstream and frontend tests**

Run: `rtk cargo test --test admin_upstreams --test admin_runtime_settings`

Run: `rtk npm --prefix frontend test -- admin-ui.spec.ts`

Run: `rtk npm --prefix frontend run type-check`

Expected: PASS; the API and every UI submit path use the same nonzero field.

- [ ] **Step 7: Commit the unified administration contract**

```bash
rtk git add src/server/admin.rs src/state.rs src/state/freekey_sync.rs tests/admin_upstreams.rs frontend/src/api/admin.ts frontend/src/views/admin/Upstreams.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "fix(upstreams): unify per-key concurrency configuration"
```

### Task 3: Rotate Ordinary Key Candidates Without Breaking Continuations

**Files:**
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/capability_routing.rs`
- Modify: `tests/gateway/chat/routing.rs`
- Modify: `tests/gateway/capability_routing.rs`
- Modify: `tests/gateway/responses/fallback.rs`

- [ ] **Step 1: Write failing deterministic rotation tests**

Add a pure helper test proving the same tuple returns the same rotated order and a representative set of request IDs produces more than one first Key.

```rust
let first = rotate_route_keys(keys.clone(), "request-a", "upstream-a", "glm-5", Protocol::ChatCompletions);
let repeated = rotate_route_keys(keys.clone(), "request-a", "upstream-a", "glm-5", Protocol::ChatCompletions);
assert_eq!(first, repeated);

let first_keys = (0..32)
    .map(|index| rotate_route_keys(keys.clone(), &format!("request-{index}"), "upstream-a", "glm-5", Protocol::ChatCompletions)[0].fingerprint.clone())
    .collect::<HashSet<_>>();
assert!(first_keys.len() > 1);
```

Add gateway tests using one upstream with multiple mapped Keys. Record the first Key received by each mock endpoint and assert ordinary requests spread across multiple Keys, while an exact continuation Key is always attempted first.

- [ ] **Step 2: Run focused routing tests and confirm stored-order concentration**

Run: `rtk cargo test --test gateway equal_model_accounts_rotate_when_their_pressure_ties`

Run: `rtk cargo test --test gateway continuation_is_pinned_to_history_upstream_when_capabilities_match`

Expected: the new Key-level assertions FAIL because `route_api_keys()` preserves stored order.

- [ ] **Step 3: Implement request-local deterministic rotation**

Use a stable hasher over bounded route identity fields; do not use randomized `HashMap` state and do not mutate stored Keys.

```rust
fn route_key_rotation_offset(
    request_id: &str,
    upstream_id: &str,
    runtime_model_slug: &str,
    protocol: Protocol,
    key_count: usize,
) -> usize {
    if key_count <= 1 {
        return 0;
    }
    let mut hasher = sha2::Sha256::new();
    for part in [request_id, upstream_id, runtime_model_slug, protocol.as_str()] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix must contain eight bytes"),
    ) as usize
        % key_count
}
```

Rotate the filtered request-local Key vector before physical candidate construction. If `preferred_profile` exactly matches upstream, fingerprint, protocol, and runtime model, move only that Key to index zero after rotation; keep all remaining Keys in rotated order.

- [ ] **Step 4: Add the configured-limit burst and no-health-poison tests**

Create eight mock Keys with a test-supplied `max_concurrency` and a barrier-controlled upstream. Assert each exact account's observed physical concurrency never exceeds the supplied value, more than one Key is used, and locally rejected candidates do not create route cooldown observations.

```rust
assert!(max_seen_by_key.values().all(|seen| *seen <= configured_limit));
assert!(max_seen_by_key.len() > 1);
assert!(route_health_snapshot.is_empty());
```

Do not embed `4` in the scheduling implementation or the burst helper; the test passes its own value.

- [ ] **Step 5: Run routing regressions**

Run: `rtk cargo test --test gateway downstream_chat_request_falls_back_to_next_mapped_key_after_unauthorized`

Run: `rtk cargo test --test gateway all_physically_attempted_key_routes_create_one_route_set_observation`

Run: `rtk cargo test --test gateway generic_500_retries_the_same_key_route_once_before_fallback`

Run: `rtk cargo test --test gateway continuation_is_pinned_to_history_upstream_when_capabilities_match`

Run: `rtk cargo test --test gateway chat_only_fallback_loads_exact_continuation_before_candidate_failover`

Expected: PASS; continuation remains exact and ordinary first-Key selection is distributed.

- [ ] **Step 6: Commit Key scheduling**

```bash
rtk git add src/server/gateway.rs src/server/gateway/capability_routing.rs tests/gateway/chat/routing.rs tests/gateway/capability_routing.rs tests/gateway/responses/fallback.rs
rtk git commit -m "fix(routing): rotate multi-key request candidates"
```

### Task 4: Expose Stable Codes And Physical Attempts In Client Errors

**Files:**
- Modify: `src/server/gateway/errors.rs`
- Modify: `src/server/gateway/stream.rs`
- Modify: `src/server/gateway.rs`
- Modify: `tests/gateway/chat/core.rs`
- Modify: `tests/gateway/chat/routing.rs`
- Modify: `tests/gateway/claude.rs`
- Modify: `tests/gateway/responses/upstream_feedback.rs`

- [ ] **Step 1: Write failing serialization-boundary tests**

For OpenAI JSON, Anthropic JSON, Chat SSE, Anthropic SSE, and both Responses SSE error carriers, assert the structured `code` remains and the message contains exactly one matching prefix.

```rust
assert_eq!(error["code"], "upstream_routes_exhausted");
assert!(error["message"].as_str().unwrap().starts_with(
    "[upstream_routes_exhausted] "
));
assert_eq!(error["message"].as_str().unwrap().matches(
    "[upstream_routes_exhausted]"
).count(), 1);
```

Add a same-route retry case asserting existing logical `attempt_count` stays unchanged while `physical_attempt_count` equals actual sends. Add all-local-full and real-502 cases asserting physical counts of zero and greater than zero respectively.

- [ ] **Step 2: Run focused gateway tests and confirm messages lack prefixes/details**

Run: `rtk cargo test --test gateway claude_gateway_error_uses_anthropic_error_envelope`

Run: `rtk cargo test --test gateway committed_concurrency_exhaustion_is_a_typed_responses_failure`

Run: `rtk cargo test --test gateway all_physically_attempted_key_routes_create_one_route_set_observation`

Expected: FAIL because structured codes exist but client-visible messages are unprefixed and terminal details omit request-wide physical attempts.

- [ ] **Step 3: Add one idempotent client-message formatter**

Keep `GatewayError::Display` unchanged. Add and use a serialization-only helper in `errors.rs`:

```rust
pub(crate) fn client_error_message(code: &str, message: &str) -> String {
    let prefix = format!("[{code}] ");
    if message.starts_with(&prefix) {
        message.to_owned()
    } else {
        format!("{prefix}{message}")
    }
}
```

Call it from OpenAI JSON, Anthropic JSON, Chat SSE, Anthropic SSE, `response.failed`, and the Responses `error` event. Always derive both arguments from the same `GatewayError` so the prefix and structured code cannot diverge.

- [ ] **Step 4: Thread the request-wide physical count into terminal aggregation**

Extend the aggregation boundary invoked from `src/server/gateway.rs` to accept the atomic `RequestRouteTracker::physical_attempt_count()` and add it without changing existing counters:

```rust
details.insert(
    "physical_attempt_count".to_string(),
    serde_json::Value::from(physical_attempt_count),
);
```

Do not derive it from `AttemptLedger::attempt_count()`: retries can create multiple physical sends for one logical failure record.

- [ ] **Step 5: Run client-error and redaction regressions**

Run: `rtk cargo test --test gateway claude_gateway_error_uses_anthropic_error_envelope`

Run: `rtk cargo test --test gateway committed_concurrency_exhaustion_is_a_typed_responses_failure`

Run: `rtk cargo test --test gateway all_physically_attempted_key_routes_create_one_route_set_observation`

Run: `rtk cargo test --test gateway generic_500_retries_the_same_key_route_once_before_fallback`

Expected: PASS; every public carrier has one stable prefix, details show physical sends, and existing key/body redaction assertions still pass.

- [ ] **Step 6: Commit the client error contract**

```bash
rtk git add src/server/gateway/errors.rs src/server/gateway/stream.rs src/server/gateway.rs tests/gateway/chat/core.rs tests/gateway/chat/routing.rs tests/gateway/claude.rs tests/gateway/responses/upstream_feedback.rs
rtk git commit -m "fix(errors): expose gateway codes and physical attempts"
```

### Task 5: Split Manual Capability Probe Failure Modes

**Files:**
- Modify: `src/state.rs`
- Modify: `src/server/gateway/capability_admin.rs`
- Modify: `tests/capability_state.rs`
- Modify: `tests/admin_capabilities.rs`

- [ ] **Step 1: Write failing state and HTTP classification tests**

Cover all four approved conditions for both applicable endpoints:

```rust
assert_probe_error(revision_zero, StatusCode::CONFLICT, "capability_policy_missing");
assert_probe_error(disabled, StatusCode::CONFLICT, "capability_probe_disabled");
assert_probe_error(no_jobs, StatusCode::UNPROCESSABLE_ENTITY, "capability_probe_no_eligible_routes");
assert_probe_error(no_sender, StatusCode::SERVICE_UNAVAILABLE, "gateway_capability_probe_unavailable");
```

For `no_jobs`, use a nonzero enabled policy and active upstream whose requested model filter or protocol produces no exact route job. Assert the message mentions active upstream state, per-Key model mappings, requested model filters, and supported protocols without credentials.

- [ ] **Step 2: Run focused capability tests and confirm errors are conflated**

Run: `rtk cargo test --test capability_state --test admin_capabilities`

Expected: FAIL because revision zero, disabled probing, and zero prepared jobs currently share `CapabilityPolicyMissing`, while single-probe submission returns only `bool`.

- [ ] **Step 3: Introduce explicit probe preparation/submission results**

Replace the compressed enum with explicit variants:

```rust
pub enum ManualProbeBatchError {
    CapabilityPolicyMissing,
    CapabilityProbeDisabled,
    NoEligibleRoutes,
    QueueUnavailable,
}
```

Return `CapabilityPolicyMissing` only for revision zero, `CapabilityProbeDisabled` only when `probe.enabled == false`, and `NoEligibleRoutes` only after preparation yields no exact jobs. Keep missing sender, closed queue, and full queue as `QueueUnavailable`.

Change single manual-probe submission to a typed result so its handler does not infer reasons from a boolean. Preserve automatic-probe behavior by adapting it to the typed result without enabling automatic requests.

- [ ] **Step 4: Map every variant to its approved HTTP contract**

Use these exact mappings in `capability_admin.rs`:

```rust
ManualProbeBatchError::CapabilityPolicyMissing =>
    capability_probe_error(StatusCode::CONFLICT, "capability_policy_missing", "capability policy is required before probing"),
ManualProbeBatchError::CapabilityProbeDisabled =>
    capability_probe_error(StatusCode::CONFLICT, "capability_probe_disabled", "capability probing is disabled by policy"),
ManualProbeBatchError::NoEligibleRoutes =>
    capability_probe_error(StatusCode::UNPROCESSABLE_ENTITY, "capability_probe_no_eligible_routes", NO_ELIGIBLE_ROUTES_MESSAGE),
ManualProbeBatchError::QueueUnavailable =>
    capability_probe_error(StatusCode::SERVICE_UNAVAILABLE, "gateway_capability_probe_unavailable", "capability probe queue is unavailable"),
```

Make `capability_probe_error` accept the specific bounded message. Existing frontend error presentation consumes the server message, so no separate UI error mapping is required.

- [ ] **Step 5: Run capability regressions**

Run: `rtk cargo test --test capability_state --test admin_capabilities`

Expected: PASS; missing policy, disabled policy, no eligible route, and unavailable queue are distinguishable by both status and stable code.

- [ ] **Step 6: Commit capability error classification**

```bash
rtk git add src/state.rs src/server/gateway/capability_admin.rs tests/capability_state.rs tests/admin_capabilities.rs
rtk git commit -m "fix(capabilities): distinguish manual probe failures"
```

### Task 6: Make Revision-Zero Bootstrap Consistent Across Deployments

**Files:**
- Modify: `src/state/types.rs`
- Modify: `src/state.rs`
- Modify: `src/main.rs`
- Modify: `Dockerfile`
- Modify: `docker-compose.yml`
- Modify: `.env.example`
- Modify: `README.md`
- Modify: `DEPLOYMENT.md`
- Modify: `tests/capability_state.rs`
- Modify: `tests/docker.rs`

- [ ] **Step 1: Write failing default, opt-out, persistence, and deployment tests**

Add tests asserting:

```rust
assert!(AppConfig::default().capability_policy_bootstrap_on_zero);

let disabled = AppConfig {
    capability_policy_bootstrap_on_zero: false,
    ..AppConfig::default()
};
// Initializing a revision-zero store with disabled leaves revision zero.
assert_eq!(state.capability_snapshot().revision(), 0);
```

Keep the existing test that a nonzero revision such as `88` remains unchanged. Exercise both the file store and the existing PostgreSQL test path when its test environment is available, asserting the bootstrapped nonzero document is persisted and reloadable. Capture startup tracing output or factor a bounded result value from initialization and assert it contains only `bootstrapped`, revision, and policy count metadata.

In `tests/docker.rs`, assert Dockerfile, Compose, and `.env.example` all default the variable to true and documentation includes the explicit `false` opt-out.

- [ ] **Step 2: Run focused tests and confirm process/image defaults disagree**

Run: `rtk cargo test --test capability_state --test docker`

Expected: FAIL because `AppConfig::default()` and the process environment fallback are false and Dockerfile has no runtime default.

- [ ] **Step 3: Enable only revision-zero bootstrap by default**

Change both process and `AppConfig` defaults to true:

```rust
capability_policy_bootstrap_on_zero: env_bool(
    "CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO",
    true,
),
```

Keep `env_bool` unchanged so `0`, `false`, `no`, and `off` remain explicit opt-outs. Do not change the existing `revision == 0` guard and do not enable automatic capability probes.

Add the runtime image default:

```dockerfile
ENV CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=true
```

Keep Compose and `.env.example` at `true`, and update standalone run/deployment text to state that only revision zero is replaced and `-e CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=false` disables it.

- [ ] **Step 4: Emit bounded bootstrap result metadata once**

Return or compute a small initialization result and log only these fields after store initialization:

```rust
tracing::info!(
    capability_policy_bootstrapped = result.bootstrapped,
    capability_policy_revision = result.revision,
    capability_policy_count = result.policy_count,
    "initialized capability policy"
);
```

Never log policy documents, source URLs, route IDs, Key fingerprints, or credentials.

- [ ] **Step 5: Run capability and deployment regressions**

Run: `rtk cargo test --test capability_state --test admin_capabilities --test docker`

Run: `rtk docker compose config`

Expected: PASS; revision zero bootstraps by default, explicit false remains inert, nonzero policies are preserved, and all deployment surfaces agree.

- [ ] **Step 6: Commit bootstrap consistency**

```bash
rtk git add src/state/types.rs src/state.rs src/main.rs Dockerfile docker-compose.yml .env.example README.md DEPLOYMENT.md tests/capability_state.rs tests/docker.rs
rtk git commit -m "fix(capabilities): bootstrap revision-zero policy by default"
```

### Task 7: Full Verification And Review

**Files:**
- Modify only files required by failures attributable to Tasks 1-6.

- [ ] **Step 1: Format and inspect the isolated branch diff**

Run: `rtk cargo fmt --all -- --check`

Run: `rtk git diff --check main...HEAD`

Expected: PASS with no formatting or whitespace errors.

- [ ] **Step 2: Run static analysis**

Run: `rtk cargo clippy --all-targets --all-features -- -D warnings`

Run: `rtk npm --prefix frontend run type-check`

Expected: PASS with no warnings or TypeScript errors.

- [ ] **Step 3: Run the full automated suites**

Run: `rtk cargo test --all-targets --all-features`

Run: `rtk npm --prefix frontend test`

Run: `rtk npm --prefix frontend run build`

Expected: all Rust and frontend tests pass and the production frontend bundle builds.

- [ ] **Step 4: Validate deployment artifacts**

Run: `rtk docker compose config`

Run: `rtk docker build -t chat2responses:reliability-check .`

Expected: Compose resolves successfully and the production image builds.

- [ ] **Step 5: Run optional exact-account Redis tests when configured**

Run: `rtk test -n "$TEST_REDIS_URL"`

When that check succeeds, run: `rtk cargo test --test redis_runtime -- --ignored --test-threads=1`

Expected: when `TEST_REDIS_URL` is set, exact-account isolation and no-health-poisoning tests pass; otherwise record the suite as not run rather than inventing a Redis endpoint.

- [ ] **Step 6: Perform an independent regression review**

Review `main...HEAD` for hard-coded provider capacity, mutation of existing upstream records, generic 502 reclassification, duplicate client prefixes, credential leakage, and nonzero capability-policy overwrite. Resolve findings with a failing regression test before editing production code.

- [ ] **Step 7: Commit any verification-only fixes**

```bash
rtk git add -u
rtk git commit -m "test(reliability): close multi-key regressions"
```

Skip this commit when verification requires no changes.

- [ ] **Step 8: Prepare integration without touching unrelated worktrees**

Run: `rtk git status --short --branch`

Run: `rtk git log --oneline main..HEAD`

Expected: the isolated branch is clean and contains only the design, plan, and coherent implementation commits. Merge or cherry-pick only after comparing current `main`, because another worktree may have advanced it during implementation.
