# Capability Discovery And Context Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make one-click DeepSeek/GLM reasoning discovery exact-route aware and operationally resilient, publish only verified model-level levels, and keep resumed contexts inside qualified limits without damaging tool or reasoning history.

**Architecture:** Bootstrap the repository capability policy only for an explicitly enabled deployment whose stored revision is zero. Move one-click candidate construction to the server so every key and protocol is represented by an anonymous route ID, preserve successful evidence across operational/deferred probes, and aggregate canonical model levels from current successful routes. Replace position-only context trimming with tool-pair-aware protection and use classified explicit overflow for one additional safe pre-output compaction attempt.

**Tech Stack:** Rust, Axum admin APIs, capability resolver, PostgreSQL/file state, Vue 3, TypeScript, Vitest, context JSON adapters.

---

## File Structure

- `src/capabilities/bootstrap.rs`: embeds and compiles the repository deployment policy.
- `src/capabilities/profile.rs`: records accepted/rejected/operational/deferred probe outcomes and bounded retry timing.
- `src/capabilities/types.rs`: adds backward-compatible probe outcome metadata.
- `src/state.rs`: bootstraps revision zero and creates exact-route manual batches.
- `src/server/gateway/capability_admin.rs`: exposes probe-all and model/route discovery summaries.
- `src/server/gateway.rs`: registers the new authenticated endpoints and uses verified route witnesses for Codex catalog metadata.
- `src/server/gateway/capability_probe.rs`: reschedules deferred and operational work without erasing evidence.
- `frontend/src/api/admin.ts`, `frontend/src/types/index.ts`: define server-owned batch/discovery contracts.
- `frontend/src/utils/capabilityDiscovery.ts`: pure model/route result helpers.
- `frontend/src/views/admin/ModelProbe.vue`: queues one server batch and renders model plus route outcomes.
- `src/server/gateway/context.rs`: analyzes protected entries and performs safe budget/overflow compaction.
- `src/server/gateway/upstream.rs`: invokes exactly one classified overflow retry.
- `templates/capabilities/current-deployment.example.json`: keeps conservative DeepSeek/GLM rollout values.
- `tests/capability_probe.rs`, `tests/admin_capabilities.rs`, `tests/gateway/capability_routing.rs`, `tests/gateway/chat/context.rs`, `tests/gateway/responses/history.rs`: regressions.

### Task 1: Bootstrap Revision Zero From The Embedded Repository Policy

**Files:**
- Create: `src/capabilities/bootstrap.rs`
- Modify: `src/capabilities/mod.rs`
- Modify: `src/state/types.rs:80-258`
- Modify: `src/main.rs:67-224`
- Modify: `src/state.rs:4008-4175`
- Modify: `docker-compose.yml`
- Modify: `.env.example`
- Test: `tests/capability_state.rs`
- Test: `tests/templates.rs`

- [ ] **Step 1: Add zero/nonzero bootstrap tests**

```rust
#[tokio::test]
async fn enabled_deployment_bootstrap_replaces_only_revision_zero() {
    let zero_store = RecordingCapabilityStore::with_configuration(
        CapabilityConfiguration::default(),
    );
    let zero = state_with_store_and_bootstrap(zero_store.clone(), true).await;
    assert!(zero.capability_snapshot().configuration.source().revision > 0);
    assert!(!zero.capability_snapshot().configuration.source().policies.is_empty());
    assert_eq!(zero_store.persist_count(), 1);

    let custom = custom_configuration(88);
    let custom_bytes = serde_json::to_vec(&custom).unwrap();
    let custom_store = RecordingCapabilityStore::with_configuration(custom);
    let loaded = state_with_store_and_bootstrap(custom_store.clone(), true).await;
    assert_eq!(
        serde_json::to_vec(loaded.capability_snapshot().configuration.source()).unwrap(),
        custom_bytes,
    );
    assert_eq!(custom_store.persist_count(), 0);
}
```

Also assert `capability_policy_bootstrap_on_zero=false` leaves revision zero unchanged for tests and non-deployment use.

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test capability_state enabled_deployment_bootstrap_replaces_only_revision_zero
```

Expected: config flag and bootstrap loader are missing.

- [ ] **Step 3: Embed and validate the deployment policy**

Create `bootstrap.rs`:

```rust
use super::CapabilityConfiguration;

const DEPLOYMENT_POLICY_JSON: &str =
    include_str!("../../templates/capabilities/current-deployment.example.json");

pub fn deployment_capability_configuration() -> Result<CapabilityConfiguration, String> {
    let configuration: CapabilityConfiguration =
        serde_json::from_str(DEPLOYMENT_POLICY_JSON).map_err(|error| error.to_string())?;
    configuration
        .clone()
        .compile()
        .map_err(|error| error.to_string())?;
    if configuration.revision == 0 || configuration.policies.is_empty() {
        return Err("embedded deployment capability policy is empty".into());
    }
    Ok(configuration)
}
```

Export it only as a crate API; do not copy the JSON into the runtime image separately.

- [ ] **Step 4: Add the deployment-only config switch**

Add `capability_policy_bootstrap_on_zero: bool` to `AppConfig`, default `false`, parse `CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO`, and set this in deployment templates:

```yaml
CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO: ${CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO:-true}
```

```dotenv
CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=true
```

- [ ] **Step 5: Persist bootstrap atomically during initialization**

Before compiling stored revision zero in `initialize_capability_snapshot_from_store`:

```rust
if self.config.capability_policy_bootstrap_on_zero
    && capability_state.configuration.revision == 0
{
    let bootstrap = crate::capabilities::deployment_capability_configuration()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    self.config_store
        .persist_capability_configuration(&bootstrap)
        .await?;
    capability_state.configuration = bootstrap;
}
```

Do not delete profiles here; normal fingerprint/schema reconciliation decides whether evidence is current.

- [ ] **Step 6: Run and commit bootstrap**

```bash
rtk cargo test --test capability_state enabled_deployment_bootstrap_replaces_only_revision_zero
rtk cargo test --test templates deployment_policies_cover_domestic_reasoning_families_with_verified_effort_maps
rtk git add src/capabilities src/state.rs src/state/types.rs src/main.rs docker-compose.yml .env.example tests
rtk git commit -m "fix(capabilities): bootstrap deployment policy at revision zero" -m "Constraint: Nonzero operator policy remains unchanged" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 2: Move One-Click Candidate Construction To The Server

**Files:**
- Modify: `src/state.rs:4178-4283`
- Modify: `src/server/gateway/capability_admin.rs`
- Modify: `src/server/gateway.rs:1650-1695`
- Modify: `tests/admin_capabilities.rs`

- [ ] **Step 1: Add policy-missing and exact-route batch tests**

Test a revision-zero/non-bootstrapped state:

```rust
let response = fixture.post_json("/api/admin/capabilities/probe-all", json!({})).await;
assert_eq!(response.status(), StatusCode::CONFLICT);
assert_eq!(response_json(response).await["error"]["code"], "capability_policy_missing");
```

Then configure one multi-key Chat upstream and one Responses-only upstream. Assert the response contains every exact route and protocol with no fingerprint:

```rust
assert_eq!(body["candidates"].as_array().unwrap().len(), 3);
for candidate in body["candidates"].as_array().unwrap() {
    assert!(candidate["route_id"].as_str().unwrap().starts_with("route_"));
    assert!(candidate.get("key_fingerprint").is_none());
}
assert!(body["candidates"].as_array().unwrap().iter().any(|route| {
    route["protocol"] == "responses"
}));
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test admin capability_probe_all_builds_every_exact_key_and_protocol
```

Expected: endpoint does not exist.

- [ ] **Step 3: Define the authenticated batch contract**

Add:

```rust
#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProbeAllRequest {
    upstream_ids: Vec<String>,
    models: Vec<String>,
}

#[derive(Clone, serde::Serialize)]
pub struct ProbeCandidateSummary {
    pub upstream_id: String,
    pub route_id: String,
    pub exposed_model_slug: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
}

pub struct ManualProbeBatchReceipt {
    pub configuration_revision: u64,
    pub started_at: u64,
    pub candidates: Vec<ProbeCandidateSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualProbeBatchError {
    CapabilityPolicyMissing,
    QueueUnavailable,
}
```

Register `POST /api/admin/capabilities/probe-all` behind the same admin JWT middleware as the single-route probe.

- [ ] **Step 4: Build jobs from authoritative routing state**

Add this typed AppState API:

```rust
pub async fn queue_manual_capability_probe_batch(
    &self,
    upstream_ids: &BTreeSet<String>,
    models: &BTreeSet<String>,
) -> Result<ManualProbeBatchReceipt, ManualProbeBatchError>
```

Its implementation takes one routing and one capability snapshot. Reject
revision zero or `probe.enabled == false` as `CapabilityPolicyMissing`. Iterate
active upstreams allowed by `upstream_ids`, reuse
`capability_probe_jobs_for_upstream`, retain only exposed model slugs allowed by
`models`, and call `build_capability_probe_job_for_key_with_snapshot` with
`ProbeReason::Manual` for every remaining exact key/protocol entry. Drop only
the individual exposed aliases that do not build; do not collapse keys sharing
an upstream.

Sort the prepared jobs by `DialectProfileKey`. Before moving them into one
`ProbeJobBatch`, build `ProbeCandidateSummary` values with
`anonymous_route_id(&job.key.upstream_id, &job.key.key_fingerprint,
&job.key.runtime_model_slug, job.key.protocol)`. If no job remains, return
`CapabilityPolicyMissing`. Clone `capability_probe_sender`; a missing sender,
`TrySendError::Full`, or `TrySendError::Closed` returns `QueueUnavailable`.
Only `Ok(())` returns a receipt, so the client never observes a partially
accepted batch. No response or log field receives `job.key.key_fingerprint`.

The handler response is:

```rust
Json(json!({
    "configuration_revision": receipt.configuration_revision,
    "started_at": receipt.started_at,
    "queued_routes": receipt.candidates.len(),
    "candidates": receipt.candidates,
}))
```

Map `CapabilityPolicyMissing` to 409 `capability_policy_missing` and
`QueueUnavailable` to 503 `gateway_capability_probe_unavailable`.

- [ ] **Step 5: Keep the single-route endpoint exact**

Do not loosen `/capabilities/probe`. It must still require a unique key or `route_id`; the new batch endpoint is the one-click API.

- [ ] **Step 6: Run and commit backend batching**

```bash
rtk cargo test --test admin capability_probe_all
rtk git add src/state.rs src/server/gateway.rs src/server/gateway/capability_admin.rs tests/admin_capabilities.rs
rtk git commit -m "fix(capabilities): queue one-click probes as exact route batches" -m "Constraint: Clients never select raw Key fingerprints" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 3: Preserve Evidence And Retry Operational Or Deferred Probes

**Files:**
- Modify: `src/capabilities/types.rs:450-490`
- Modify: `src/capabilities/profile.rs:43-225`
- Modify: `src/server/gateway/capability_probe.rs:413-711`
- Modify: `src/state.rs:4285-4344`
- Test: `tests/capability_probe.rs`

- [ ] **Step 1: Add operational and deferred lifecycle tests**

```rust
#[test]
fn operational_failure_preserves_prior_reasoning_evidence_and_schedules_retry() {
    let mut profile = verified_reasoning_profile(["low", "high"]);
    apply_probe_outcome(&mut profile, ProbeOutcome::OperationalFailure {
        code: "minimal_text_failed".into(),
        http_status: Some(503),
        attempted_at: 1_000,
    });
    assert_eq!(profile.reasoning_controls["reasoning_effort"], ["low", "high"]);
    assert_eq!(profile.last_success_at, Some(900));
    assert_eq!(profile.last_probe_outcome, ProbeProfileOutcome::OperationalFailure);
    assert!(profile.next_probe_at.is_some_and(|at| at > 1_000));
}

#[tokio::test]
async fn capacity_skipped_probe_is_deferred_without_route_cooldown() {
    // Hold local capacity, run a probe, then release it.
    assert_eq!(profile.last_probe_outcome, ProbeProfileOutcome::Deferred);
    assert!(state.route_health_snapshot(&route).await.unwrap().is_none());
    assert!(profile.next_probe_at.is_some_and(|at| at <= attempted_at + 2));
}
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test capability_probe operational_failure_preserves_prior_reasoning_evidence_and_schedules_retry
rtk cargo test --test capability_probe capacity_skipped_probe_is_deferred_without_route_cooldown
```

Expected: outcome metadata is missing and capacity-skipped partial work is considered fresh for the full refresh interval.

- [ ] **Step 3: Add backward-compatible profile metadata**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeProfileOutcome {
    Accepted,
    Rejected,
    OperationalFailure,
    Deferred,
}

// In UpstreamDialectProfile, all with #[serde(default)]:
pub last_probe_outcome: Option<ProbeProfileOutcome>,
pub probe_retry_count: u32,
pub next_probe_at: Option<u64>,
```

Initialize them to `None`, `0`, `None` in `unknown`.

- [ ] **Step 4: Apply bounded retry timing**

Add:

```rust
const OPERATIONAL_RETRY_DELAYS_SECONDS: [u64; 5] = [5, 15, 60, 300, 900];

fn next_operational_retry(attempted_at: u64, failures: u32) -> u64 {
    let index = usize::try_from(failures.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(OPERATIONAL_RETRY_DELAYS_SECONDS.len() - 1);
    attempted_at.saturating_add(OPERATIONAL_RETRY_DELAYS_SECONDS[index])
}
```

Operational failure updates only attempt/status/outcome/retry fields; it does not replace capabilities, controls, carrier, correction rules, last success, or evidence codes. Full conclusive success resets retry metadata. `CapacitySkipped` partial merge sets `Deferred`, `next_probe_at=attempted_at+1`, and does not clear prior operational diagnostics until a full conclusive probe succeeds.

- [ ] **Step 5: Make reconciliation honor `next_probe_at`**

In currentness checks, a profile is due when:

```rust
let retry_due = profile.next_probe_at.is_some_and(|at| now >= at);
let current = !retry_due
    && profile_is_current(profile, &fingerprint, now, refresh_interval_seconds);
```

Queue limits remain the configured `max_global_concurrency` and `max_per_upstream_concurrency`; do not create a parallel retry worker.

- [ ] **Step 6: Run and commit probe lifecycle**

```bash
rtk cargo test --test capability_probe operational_failure_preserves_prior_reasoning_evidence_and_schedules_retry
rtk cargo test --test capability_probe capacity_skipped_probe_is_deferred_without_route_cooldown
rtk cargo test --test capability_probe probe_service_honors_global_concurrency_across_upstreams
rtk git add src/capabilities src/server/gateway/capability_probe.rs src/state.rs tests/capability_probe.rs
rtk git commit -m "fix(capabilities): retry unavailable probes without erasing evidence" -m "Constraint: Probe concurrency stays below normal traffic" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 4: Publish Model-Level Verified Levels And Route Diagnostics

**Files:**
- Modify: `src/server/gateway/capability_admin.rs:534-710`
- Modify: `src/server/gateway.rs:2017-2235`
- Modify: `tests/admin_capabilities.rs`
- Modify: `tests/gateway/capability_routing.rs`

- [ ] **Step 1: Add one-success-one-failure aggregation tests**

Create two exact routes for `deepseek-v4-flash`. Route A has current successful controls `low/medium/high`; route B retains no controls and has operational 503. Assert discovery output:

```rust
assert_eq!(body["models"][0]["verified_reasoning_levels"], json!([
    "low", "medium", "high"
]));
assert_eq!(body["models"][0]["routes"].as_array().unwrap().len(), 2);
assert_eq!(body["models"][0]["routes"][1]["outcome"], "operational_failure");
assert_eq!(body["models"][0]["routes"][1]["operational_code"], "minimal_text_failed");
```

Assert the Codex catalog exposes the same verified union and never adds policy-only `xhigh/max`.

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test admin capability_discovery_unions_successful_routes_and_keeps_failures
rtk cargo test --test gateway codex_catalog_unions_current_verified_route_levels
```

Expected: admin endpoint lacks model aggregation; catalog selects one witness rather than a verified union.

- [ ] **Step 3: Add discovery summaries**

Register authenticated `GET /api/admin/capabilities/discovery`. Build summaries from routing plus current exact profiles:

```rust
#[derive(serde::Serialize)]
struct CapabilityModelDiscoverySummary {
    exposed_model_slug: String,
    verified_reasoning_levels: Vec<String>,
    routes: Vec<CapabilityRouteDiscoverySummary>,
}

#[derive(serde::Serialize)]
struct CapabilityRouteDiscoverySummary {
    upstream_id: String,
    route_id: String,
    runtime_model_slug: String,
    protocol: WireProtocol,
    outcome: &'static str,
    accepted_reasoning_levels: Vec<String>,
    http_status: Option<u16>,
    operational_code: Option<String>,
    last_attempt_at: Option<u64>,
    next_probe_at: Option<u64>,
}
```

For each current exact route, resolve semantic policy and map accepted wire values back to canonical effort names through `resolved.effort_map`. Union only routes with supported `ReasoningOutput` and accepted controls. Sort by `low, medium, high, xhigh, max`; operational routes contribute diagnostics but no negative model evidence.

- [ ] **Step 4: Reuse the same aggregation for Codex metadata**

Extract a backend helper returning canonical verified levels per exposed model. `list_models_codex_format` uses that helper. If the union is empty, advertise only `none`; do not advertise policy candidates that no exact route accepted.

- [ ] **Step 5: Run and commit aggregation**

```bash
rtk cargo test --test admin capability_discovery_unions_successful_routes_and_keeps_failures
rtk cargo test --test gateway codex_catalog_unions_current_verified_route_levels
rtk git add src/server/gateway.rs src/server/gateway/capability_admin.rs tests
rtk git commit -m "fix(capabilities): publish verified model levels across routes" -m "Constraint: Operational failures never become unsupported evidence" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 5: Replace Client-Side Route Guessing In One-Click Discovery

**Files:**
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/types/index.ts`
- Create: `frontend/src/utils/capabilityDiscovery.ts`
- Create: `frontend/src/utils/capabilityDiscovery.spec.ts`
- Modify: `frontend/src/views/admin/ModelProbe.vue:23-100`
- Modify: `frontend/src/views/admin/ModelProbe.vue:206-386`

- [ ] **Step 1: Add frontend contract and polling tests**

Test that route identity includes `route_id`, Responses routes are preserved, model levels are taken from the server, and an operational route is not labelled unsupported.

```typescript
expect(indexDiscovery(response).models.get('deepseek-v4-flash')?.levels)
  .toEqual(['low', 'medium', 'high'])
expect(indexDiscovery(response).routes.get('route_responses')?.protocol)
  .toBe('responses')
expect(routeStatusLabel(operationalRoute)).toBe('探测暂不可用')
```

- [ ] **Step 2: Verify RED**

```bash
rtk npm --prefix frontend test -- --run src/utils/capabilityDiscovery.spec.ts
```

Expected: helper and server response types do not exist.

- [ ] **Step 3: Add typed API methods**

```typescript
probeAllCapabilities: (data: ProbeAllCapabilitiesRequest = {}) =>
  adminHttp.post<ProbeAllCapabilitiesResponse>('/admin/capabilities/probe-all', data),
getCapabilityDiscovery: () =>
  adminHttp.get<CapabilityDiscoveryResponse>('/admin/capabilities/discovery'),
```

The response types mirror Task 2 and Task 4 exactly and never contain `key_fingerprint`.

- [ ] **Step 4: Replace the four-wide client queue**

Delete `capabilityProbeCandidates`, `runWithConcurrency`, the hard-coded `chat_completions`, and the key that omits `route_id`. `runCapabilityProbe` calls one batch endpoint, then polls discovery until every returned candidate has `last_attempt_at >= started_at` or is still explicitly pending at the 90-second deadline.

- [ ] **Step 5: Render both scopes**

The first compact table has one row per model and verified level tags. A second route table shows upstream, anonymous route ID, protocol, outcome (`accepted`, `rejected`, `operational_failure`, `deferred`, `pending`), HTTP status, and retry time. Use `warning` for unavailable/deferred and `danger` only for explicit unsupported/rejected.

- [ ] **Step 6: Run and commit the frontend**

```bash
rtk npm --prefix frontend test -- --run src/utils/capabilityDiscovery.spec.ts
rtk npm --prefix frontend run type-check
rtk npm --prefix frontend run build
rtk git add frontend/src
rtk git commit -m "fix(admin): use exact server routes for reasoning discovery" -m "Constraint: Multi-Key and Responses-only routes must be probed" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 6: Protect Tool Pairs, Instructions, Current Input, And Recent Reasoning

**Files:**
- Modify: `src/server/gateway/context.rs:1-285`
- Test: `tests/gateway/chat/context.rs`
- Test: `tests/gateway/responses/history.rs`

- [ ] **Step 1: Add unsafe-compaction regressions**

Create payloads containing an old completed tool pair, an unresolved tool call, system/developer instructions, recent reasoning, recent user input, and large old safe messages. Assert only the completed tool output and old safe messages are summarized:

```rust
assert_eq!(find_call(&trimmed, "open-call")["arguments"], original_arguments);
assert!(find_output(&trimmed, "open-call").is_none());
assert_eq!(system_text(&trimmed), "system invariant");
assert_eq!(developer_text(&trimmed), "developer invariant");
assert_eq!(recent_reasoning(&trimmed), original_recent_reasoning);
assert_eq!(current_user_input(&trimmed), original_current_input);
assert!(find_output(&trimmed, "closed-call")["output"]
    .as_str().unwrap().contains("gateway-summary"));
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test gateway context_compaction_preserves_unresolved_tool_pairs_and_recent_reasoning
```

Expected: the current candidate selector can truncate old function `arguments` without pairing knowledge.

- [ ] **Step 3: Add entry analysis**

```rust
use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct ContextProtection {
    protected: HashSet<usize>,
    compactable_tool_results: HashSet<usize>,
}

fn nested_ids(entry: &Value, block_type: &str, id_field: &str) -> Vec<String> {
    entry
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(block_type))
        .filter_map(|block| block.get(id_field).and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn tool_call_ids(entry: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if entry_type(entry) == Some("function_call") {
        ids.extend(
            entry
                .get("call_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        );
    }
    ids.extend(
        entry
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|call| call.get("id").and_then(Value::as_str))
            .map(str::to_owned),
    );
    ids.extend(nested_ids(entry, "tool_use", "id"));
    ids
}

fn tool_result_ids(entry: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if entry_type(entry) == Some("function_call_output") {
        ids.extend(
            entry
                .get("call_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        );
    }
    if entry_role(entry) == Some("tool") {
        ids.extend(
            entry
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        );
    }
    if entry_type(entry) == Some("tool_result") {
        ids.extend(
            entry
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        );
    }
    ids.extend(nested_ids(entry, "tool_result", "tool_use_id"));
    ids
}

fn entry_contains_reasoning(entry: &Value) -> bool {
    matches!(entry_type(entry), Some("reasoning" | "thinking" | "redacted_thinking"))
        || entry.get("reasoning_content").is_some()
        || entry
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("reasoning" | "thinking" | "redacted_thinking")
                    )
                })
            })
}

fn entry_is_plain_conversation(entry: &Value) -> bool {
    (matches!(entry_role(entry), Some("user" | "assistant"))
        || entry_type(entry) == Some("message"))
        && tool_call_ids(entry).is_empty()
        && tool_result_ids(entry).is_empty()
        && !entry_contains_reasoning(entry)
}

fn analyze_context_entries(entries: &[Value]) -> ContextProtection {
    let mut protection = ContextProtection::default();
    let recent_start = entries.len().saturating_sub(CONTEXT_KEEP_RECENT_ITEMS);
    protection.protected.extend(recent_start..entries.len());

    let mut calls = HashMap::<String, Vec<usize>>::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry_is_system(entry) || entry_contains_reasoning(entry) {
            protection.protected.insert(index);
        }
        for call_id in tool_call_ids(entry) {
            calls.entry(call_id).or_default().push(index);
            // Arguments are semantic replay state even after the output exists.
            protection.protected.insert(index);
        }
    }

    if let Some(current_input) = entries
        .iter()
        .rposition(|entry| entry_role(entry) == Some("user"))
    {
        protection.protected.insert(current_input);
    }

    for (index, entry) in entries.iter().enumerate() {
        let result_ids = tool_result_ids(entry);
        if result_ids.is_empty() {
            continue;
        }
        let all_have_earlier_call = result_ids.iter().all(|call_id| {
            calls
                .get(call_id)
                .is_some_and(|indices| indices.iter().any(|call_index| *call_index < index))
        });
        if all_have_earlier_call && !protection.protected.contains(&index) {
            protection.compactable_tool_results.insert(index);
        } else {
            protection.protected.insert(index);
        }
    }

    for (index, entry) in entries.iter().enumerate() {
        if !protection.compactable_tool_results.contains(&index)
            && !entry_is_plain_conversation(entry)
        {
            protection.protected.insert(index);
        }
    }
    protection
}
```

Recognize Responses `function_call.call_id` plus `function_call_output.call_id`, Chat assistant `tool_calls[].id` plus tool `tool_call_id`, and Messages `tool_use.id` plus `tool_result.tool_use_id`. The analyzer must never invent a missing result.

- [ ] **Step 4: Restrict truncation and compaction**

Change `trim_entries_to_budget` to accept `&ContextProtection`. Delete the
`arguments` branch from `truncate_entry_content`; no code path may summarize or
truncate it. Build candidates exactly as:

```rust
let mut candidates = protection
    .compactable_tool_results
    .iter()
    .copied()
    .collect::<Vec<_>>();
candidates.sort_unstable();
candidates.extend((0..entries.len()).filter(|index| {
    !protection.protected.contains(index)
        && !protection.compactable_tool_results.contains(index)
        && entry_is_plain_conversation(&entries[*index])
}));
```

This orders completed old tool results before old safe text messages.
Add the payload adapters and minimum estimator as concrete shared helpers:

```rust
fn context_entries(payload: &Value) -> Option<&[Value]> {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .or_else(|| {
            payload
                .get("input")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
        })
}

fn context_protection(payload: &Value) -> ContextProtection {
    context_entries(payload)
        .map(analyze_context_entries)
        .unwrap_or_default()
}

fn estimate_context_minimum_tokens(
    payload: &Value,
    protection: &ContextProtection,
) -> u64 {
    context_entries(payload)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            if protection.protected.contains(&index) {
                return estimate_tokens_from_value(entry);
            }
            let mut minimum = entry.clone();
            let tool_result = protection.compactable_tool_results.contains(&index);
            if tool_result || entry_is_plain_conversation(entry) {
                compact_entry(&mut minimum, tool_result);
            }
            estimate_tokens_from_value(&minimum)
        })
        .sum()
}

fn trim_context_entries_with_protection(
    payload: &mut Value,
    target_tokens: u64,
    protection: &ContextProtection,
) -> ContextTrimStats {
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(messages, target_tokens, protection);
    }
    if let Some(input) = payload.get_mut("input").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(input, target_tokens, protection);
    }
    ContextTrimStats::default()
}

fn trim_context_entries(payload: &mut Value, target_tokens: u64) -> ContextTrimStats {
    let protection = context_protection(payload);
    trim_context_entries_with_protection(payload, target_tokens, &protection)
}
```

Protected entries remain byte-for-byte unchanged even when their minimum
exceeds the target.

Add `protected_minimum_tokens` and `compacted_items` to
`ContextBudgetReport`. Compute the minimum by cloning each unprotected
candidate, applying `compact_entry` to the clone, and estimating that clone;
protected/non-candidate entries retain their full estimate. Include the
payload baseline in this value. Set `compacted_items` from
`trim_stats.compacted_entries` and expose only counts, never entry text.

- [ ] **Step 5: Run and commit protection**

```bash
rtk cargo test --test gateway context_compaction_preserves_unresolved_tool_pairs_and_recent_reasoning
rtk cargo test --test gateway context_budget_compacts_payload_before_retrying_upstream
rtk git add src/server/gateway/context.rs tests/gateway
rtk git commit -m "fix(context): preserve unresolved tools and recent reasoning" -m "Constraint: Never invent or drop unresolved tool state" -m "Confidence: high" -m "Scope-risk: broad"
```

### Task 7: Perform One Safe Overflow Compaction Retry

**Files:**
- Modify: `src/server/gateway/context.rs:287-418`
- Modify: `src/server/gateway/upstream.rs:2023-2062`
- Test: `tests/gateway/chat/context.rs`
- Test: `tests/gateway/responses/history.rs`

- [ ] **Step 1: Add explicit overflow retry tests**

For 400, 413, 502, and 503 structured context overflow, make attempt one reject and attempt two accept only if an old completed tool output was compacted. Assert two attempts, route health empty, protected entries unchanged, and success. Add a case where protected minimum cannot fit and assert one terminal 400 `upstream_context_limit`.

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test gateway context_overflow_503_compacts_once_without_cooling_route
rtk cargo test --test gateway protected_context_minimum_returns_stable_context_error
```

Expected: 503 is transient without the classification plan, and current retry only halves output tokens.

- [ ] **Step 3: Add the overflow retry function**

```rust
#[derive(Debug, Clone, Copy)]
pub(super) struct ContextOverflowRetryReport {
    pub(super) changed: bool,
    pub(super) protected_minimum_tokens: u64,
    pub(super) compacted_items: u32,
}

pub(super) fn compact_for_context_overflow_retry(
    payload: &mut Value,
    budget: &ContextBudgetReport,
) -> ContextOverflowRetryReport {
    let target = budget.allowed_input_tokens.saturating_mul(9) / 10;
    let baseline_tokens = estimate_payload_baseline_tokens(payload);
    let protection = context_protection(payload);
    let protected_minimum_tokens = baseline_tokens.saturating_add(
        estimate_context_minimum_tokens(payload, &protection),
    );
    if protected_minimum_tokens > target {
        return ContextOverflowRetryReport {
            changed: false,
            protected_minimum_tokens,
            compacted_items: 0,
        };
    }
    let target_entry_tokens = target.saturating_sub(baseline_tokens);
    let stats = trim_context_entries_with_protection(
        payload,
        target_entry_tokens,
        &protection,
    );
    let generation_changed = halve_generation_cap_for_context_retry(payload).is_some();
    ContextOverflowRetryReport {
        changed: generation_changed || stats.compacted_entries > 0 || stats.truncated_blocks > 0,
        protected_minimum_tokens,
        compacted_items: stats.compacted_entries,
    }
}
```

- [ ] **Step 4: Retry only before semantic output**

In `send_to_upstream`, handle
`classified_feedback.semantic == ExplicitContextOverflow` in the existing
non-success HTTP response branch. That branch runs before a `DispatchResult` is
returned and before any upstream body is attached to the downstream response,
so semantic output cannot yet have been committed; do not reference or create a
`StreamCommitTracker` that is not in this function's scope.

Call the function only when `!context_retry_attempted` and
`!stream_only_recovery.consumed`. Set `context_retry_attempted = true` before
attempting compaction. If `changed`, call
`upstream_request_guard.reserve_next(state, upstream, &key_fingerprint, model)`
using the exact-account API from the admission plan, then continue the request
loop. If it is unchanged, immediately return `upstream_context_limit` with the
protected-minimum count. A generic 5xx never calls this function. Streaming
errors after a successful HTTP response remain governed by the existing
`StreamCommitTracker` and never return to this pre-response retry loop.

- [ ] **Step 5: Run and commit overflow recovery**

```bash
rtk cargo test --test gateway context_overflow_503_compacts_once_without_cooling_route
rtk cargo test --test gateway protected_context_minimum_returns_stable_context_error
rtk cargo test --test gateway generic_503_does_not_compact_history
rtk git add src/server/gateway/context.rs src/server/gateway/upstream.rs tests/gateway
rtk git commit -m "fix(context): compact once on explicit overflow before output" -m "Constraint: Generic 5xx and post-output failures never compact or replay" -m "Confidence: high" -m "Scope-risk: broad"
```

### Task 8: Pin Conservative Deployment Context Data

**Files:**
- Modify: `templates/capabilities/current-deployment.example.json:117-193`
- Modify: `tests/templates.rs:480-550`
- Modify: `DEPLOYMENT.md`

- [ ] **Step 1: Add conservative template assertions**

```rust
assert_eq!(policy("glm-5.2").semantic.context_window, Some(131_072));
assert_eq!(
    policy("deepseek-v4-flash").semantic.context_window,
    Some(131_072),
);
assert!(policy("glm-5.2").semantic.context_window.unwrap() < 1_000_000);
assert!(policy("deepseek-v4-flash").semantic.context_window.unwrap() < 142_000);
```

- [ ] **Step 2: Run template tests**

```bash
rtk cargo test --test templates deployment_context_limits_are_conservative_until_qualified
```

Expected: pass with the current 131,072 values; this test prevents a future unqualified increase.

- [ ] **Step 3: Document promotion rules**

State that 32k, 64k, 128k, and configured maximum are serial qualification tiers; each must pass text, reasoning, and read-only tool flow three consecutive times. Only the largest passing tier may be imported as a new explicit revision. A failed 32k blocks model qualification; ordinary traffic never auto-learns a higher limit.

- [ ] **Step 4: Commit deployment data guardrails**

```bash
rtk git add templates/capabilities/current-deployment.example.json tests/templates.rs DEPLOYMENT.md
rtk git commit -m "docs(context): pin qualified deployment limits" -m "Constraint: Context promotion requires a serial live matrix" -m "Confidence: high" -m "Scope-risk: narrow"
```
