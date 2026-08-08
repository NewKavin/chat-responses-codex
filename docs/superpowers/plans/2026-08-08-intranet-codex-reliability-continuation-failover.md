# Continuation Compatibility Failover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a resumed Responses session prefer its producing route but fail over before semantic output to another exact route that proves the same replay semantics.

**Architecture:** Version continuation state into a preferred exact profile plus a value-comparable compatibility contract derived from current exact capability evidence. Default provider grouping uses normalized base URL and resolved runtime model; operators may explicitly group equivalent internal aliases. Candidate filtering requires exact contract equality, then existing health/quota/fairness ranking applies with the preferred route first.

**Tech Stack:** Rust, Serde, SHA-256, Responses history, capability resolver, stream commit tracker, PostgreSQL history persistence.

---

## File Structure

- `src/state/types.rs`: adds the optional upstream continuation provider group.
- `src/state/normalize.rs`: trims and validates the optional group.
- `src/capabilities/profile.rs`: reuses normalized route URLs for derived groups.
- `src/server/gateway/capability_routing.rs`: owns continuation V2 state, compatibility contracts, V1 derivation, and candidate comparison.
- `src/server/gateway.rs`: replaces exact-profile exclusion with compatibility filtering and preferred-profile ordering.
- `src/server/gateway/upstream.rs`: stores the selected route as the new preference while retaining the contract.
- `frontend/src/types/index.ts`, `frontend/src/views/admin/Upstreams.vue`: expose the optional group without leaking credentials.
- `tests/gateway/responses/reasoning.rs`, `history.rs`, `tools.rs`: failover, mismatch, migration, and replay barriers.
- `tests/postgres_roundtrip.rs`: V1/V2 history persistence.

### Task 1: Add A Stable Continuation Provider Group

**Files:**
- Modify: `src/state/types.rs:260-362`
- Modify: `src/state/normalize.rs`
- Modify: `src/state/freekey_sync.rs:380-708`
- Modify: `src/state/postgres.rs:62-120, 980-1050, 1555-1626`
- Modify: `src/server/admin.rs`
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Test: `tests/admin_upstreams.rs`
- Test: `tests/postgres_roundtrip.rs`

- [ ] **Step 1: Write validation and serialization tests**

Add cases asserting an omitted group stays `None`, whitespace becomes `None`, and a valid explicit group survives admin round-trip:

```rust
let upstream = UpstreamConfig {
    continuation_provider_group: Some(" internal-deepseek ".into()),
    ..fixture_upstream()
};
let mut normalized = upstream;
normalized.normalize_for_storage();
normalized.validate_configuration().unwrap();
assert_eq!(
    normalized.continuation_provider_group.as_deref(),
    Some("internal-deepseek"),
);
```

Reject values over 128 characters or containing control characters.

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test admin_upstreams continuation_provider_group_is_normalized_and_persisted
```

Expected: field does not exist.

- [ ] **Step 3: Add the optional field**

```rust
#[serde(default)]
pub continuation_provider_group: Option<String>,
```

Add `continuation_provider_group: None` to `UpstreamConfig::default`. Normalize with:

```rust
upstream.continuation_provider_group = upstream
    .continuation_provider_group
    .take()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());
if upstream.continuation_provider_group.as_ref().is_some_and(|value| {
    value.len() > 128 || value.chars().any(char::is_control)
}) {
    return Err(UpstreamMutationError::InvalidInput(
        "continuation provider group must be 1-128 printable characters".into(),
    ));
}
```

In `src/state/freekey_sync.rs`, accept both a string and JSON `null` in `update_upstream_by_id` before `normalize_for_storage()`:

```rust
if let Some(group) = updates.get("continuation_provider_group") {
    upstream.continuation_provider_group = if group.is_null() {
        None
    } else {
        Some(
            group
                .as_str()
                .ok_or_else(|| UpstreamMutationError::InvalidInput(
                    "continuation_provider_group must be a string or null".into(),
                ))?
                .to_string(),
        )
    };
}
```

Persist the field in `src/state/postgres.rs`. Add `continuation_provider_group TEXT NULL` to `SCHEMA_SQL`. Append it after `COALESCE(remark, '')` in the `load_state` SELECT, making its zero-based row index 24, set it in the `UpstreamConfig` literal, and include it in the existing upsert parameters:

```sql
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS continuation_provider_group TEXT NULL;
```

```rust
continuation_provider_group: row.get::<_, Option<String>>(24),
```

Append `&upstream.continuation_provider_group` as parameter 26. Add the column to the `INSERT` column list, `$26` to `VALUES`, and this conflict update:

```sql
continuation_provider_group = EXCLUDED.continuation_provider_group
```

Extend `tests/postgres_roundtrip.rs` so `internal-deepseek` survives save/load and `None` remains SQL `NULL`.

- [ ] **Step 4: Add the admin field**

Use a normal text input labelled `续传兼容组` with empty value meaning automatic grouping. Do not display derived hashes in the UI.

- [ ] **Step 5: Run and commit configuration support**

```bash
rtk cargo test --test admin_upstreams continuation_provider_group_is_normalized_and_persisted
rtk cargo test --test postgres_roundtrip continuation_provider_group
rtk npm --prefix frontend run type-check
rtk git add src/state/types.rs src/state/normalize.rs src/state/freekey_sync.rs src/state/postgres.rs src/server/admin.rs frontend/src tests/admin_upstreams.rs tests/postgres_roundtrip.rs
rtk git commit -m "feat(upstreams): configure continuation provider groups" -m "Constraint: Empty group uses normalized-route derivation" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 2: Define Continuation V2 And Its Compatibility Contract

**Files:**
- Modify: `src/server/gateway/capability_routing.rs:15-192`
- Test: `src/server/gateway/capability_routing.rs` unit tests

- [ ] **Step 1: Write contract equality tests**

Add unit tests for equal accounts and each mismatch dimension. Construct two routes with different upstream IDs and keys but identical base URL, runtime model, protocol, current profile, effort map, corrections, and tools; assert equality. Then mutate one field at a time:

```rust
for mutation in [
    ContractMutation::ProviderGroup,
    ContractMutation::RuntimeModel,
    ContractMutation::ProtocolTransition,
    ContractMutation::ReasoningCarrier,
    ContractMutation::EffortMap,
    ContractMutation::CorrectionRules,
    ContractMutation::ToolRegistryVersion,
    ContractMutation::ProbeSchemaVersion,
] {
    assert_ne!(baseline, mutation.apply(baseline.clone()));
}
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --lib continuation_contract_matches_equivalent_accounts_not_exact_keys
```

Expected: compatibility types do not exist.

- [ ] **Step 3: Add V2 state and value types**

Replace the V1-only layout with:

```rust
const GATEWAY_CONTINUATION_VERSION: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContinuationCompatibilityContract {
    provider_group: String,
    runtime_model_slug: String,
    protocol_transition: ProtocolTransitionIdentity,
    required_capabilities: BTreeSet<Capability>,
    reasoning_carrier: Option<ReasoningCarrier>,
    effort_map: std::collections::BTreeMap<String, String>,
    correction_rules: Vec<crate::capabilities::DialectCorrectionRule>,
    tool_registry_version: Option<u32>,
    probe_schema_version: u32,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GatewayContinuationState {
    version: u32,
    #[serde(alias = "profile_key")]
    preferred_profile: DialectProfileKey,
    configuration_fingerprint: String,
    #[serde(default)]
    compatibility_contract: Option<ContinuationCompatibilityContract>,
    probe_schema_version: u32,
    reasoning_carrier: Option<ReasoningCarrier>,
    required_capabilities: BTreeSet<Capability>,
    adapter_identity: ContinuationAdapterIdentity,
}
```

The retained V1 fields are required for backward deserialization and exact-route validation. New V2 writes always set `compatibility_contract: Some(...)`.

Add stable accessors used by both routing and persistence:

```rust
impl GatewayContinuationState {
    pub(super) fn preferred_profile(&self) -> &DialectProfileKey {
        &self.preferred_profile
    }

    pub(super) fn contract(&self) -> Option<&ContinuationCompatibilityContract> {
        self.compatibility_contract.as_ref()
    }
}
```

- [ ] **Step 4: Derive a non-secret provider group**

Import the digest trait and add:

```rust
use sha2::Digest;

fn continuation_provider_group(
    upstream: &UpstreamConfig,
    runtime_model_slug: &str,
) -> Result<String, String> {
    let material = match upstream.continuation_provider_group.as_deref() {
        Some(explicit) => format!("explicit\0{}", explicit.trim().to_ascii_lowercase()),
        None => format!(
            "derived\0{}\0{}",
            crate::capabilities::normalize_route_base_url(&upstream.base_url)?,
            runtime_model_slug.trim().to_ascii_lowercase(),
        ),
    };
    Ok(format!("{:x}", sha2::Sha256::digest(material.as_bytes())))
}
```

The stored history contains only the digest, never a URL, credential, or explicit operator label.

- [ ] **Step 5: Build contracts only from current exact evidence**

Add a constructor taking the route's `ResolvedCapabilities` and current `UpstreamDialectProfile`:

```rust
fn continuation_contract_for_route(
    upstream: &UpstreamConfig,
    runtime_model_slug: &str,
    downstream_protocol: WireProtocol,
    upstream_protocol: WireProtocol,
    required_capabilities: &BTreeSet<Capability>,
    resolved: &ResolvedCapabilities,
    profile: &UpstreamDialectProfile,
    tool_registry_version: Option<u32>,
) -> Option<ContinuationCompatibilityContract> {
    if profile.state == DialectProfileState::Unknown
        || profile.probe_schema_version != DIALECT_PROBE_SCHEMA_VERSION
        || !required_capabilities
            .iter()
            .all(|capability| resolved.supports(*capability))
    {
        return None;
    }
    let provider_group = continuation_provider_group(upstream, runtime_model_slug).ok()?;
    Some(ContinuationCompatibilityContract {
        provider_group,
        runtime_model_slug: runtime_model_slug.to_string(),
        protocol_transition: ProtocolTransitionIdentity::new(
            downstream_protocol,
            upstream_protocol,
        ),
        required_capabilities: required_capabilities.clone(),
        reasoning_carrier: (resolved.reasoning_carrier != ReasoningCarrier::None)
            .then_some(resolved.reasoning_carrier),
        effort_map: resolved.effort_map.clone(),
        correction_rules: resolved.correction_rules.clone(),
        tool_registry_version,
        probe_schema_version: DIALECT_PROBE_SCHEMA_VERSION,
    })
}
```

If the exact current profile or resolved contract is unavailable, return `None`; do not synthesize family defaults.

- [ ] **Step 6: Run and commit types**

```bash
rtk cargo test --lib continuation_contract
rtk git add src/server/gateway/capability_routing.rs
rtk git commit -m "feat(responses): version continuation compatibility contracts" -m "Constraint: Contracts require current exact probe evidence" -m "Confidence: high" -m "Scope-risk: broad"
```

### Task 3: Derive V1 Contracts Safely

**Files:**
- Modify: `src/server/gateway/capability_routing.rs`
- Modify: `src/server/gateway.rs:4255-4312`
- Test: `tests/gateway/responses/history.rs`

- [ ] **Step 1: Add V1 migration tests**

Persist a serialized V1 continuation with `profile_key` and no contract. Assert an unchanged, unique exact current profile derives a contract and resumes. Add cases for missing profile, stale fingerprint, stale probe schema, and ambiguous model mapping; each must return 400 `gateway_response_history_invalid` before dispatch.

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test gateway v1_continuation_derives_contract_only_from_unique_current_profile
```

Expected: V1 either remains exact-only or is rejected by version validation.

- [ ] **Step 3: Implement an explicit load result**

```rust
pub(super) enum LoadedContinuation {
    V2(GatewayContinuationState),
    V1NeedsDerivation(GatewayContinuationState),
}

impl GatewayContinuationState {
    pub(super) fn load(self) -> Result<LoadedContinuation, &'static str> {
        match (self.version, self.compatibility_contract.is_some()) {
            (2, true) => Ok(LoadedContinuation::V2(self)),
            (1, false) => Ok(LoadedContinuation::V1NeedsDerivation(self)),
            _ => Err("unsupported or malformed continuation version"),
        }
    }
}
```

Derive V1 after capability cache construction by finding exactly one route matching the stored exact profile, configuration fingerprint, probe schema, protocol transition, and tool registry version. Use the constructor from Task 2; zero or multiple contracts is a safe 400.

- [ ] **Step 4: Run and commit migration**

```bash
rtk cargo test --test gateway v1_continuation_derives_contract_only_from_unique_current_profile
rtk cargo test --test postgres_roundtrip response_history
rtk git add src/server/gateway/capability_routing.rs src/server/gateway.rs tests/gateway/responses/history.rs tests/postgres_roundtrip.rs
rtk git commit -m "fix(responses): derive legacy continuation contracts safely" -m "Constraint: Ambiguous legacy history fails closed" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 4: Filter Compatible Routes And Prefer The Original Profile

**Files:**
- Modify: `src/server/gateway.rs:4314-4905`
- Modify: `src/server/gateway.rs:5003-5060`
- Test: `tests/gateway/responses/reasoning.rs:330-585`

- [ ] **Step 1: Turn the existing no-failover test into a compatible success test**

Rename it to `responses_continuation_503_fails_over_to_compatible_account`. Give both upstreams the same explicit continuation group and current identical profiles. Make the first response originate from the high-priority exact route, then return 503; assert:

```rust
assert_eq!(continuation_response.status(), StatusCode::OK);
assert!(exact_hits.load(Ordering::SeqCst) >= 2);
assert_eq!(alternative_hits.load(Ordering::SeqCst), 1);
assert_eq!(captured_alternative["input"], expected_complete_reasoning_tool_history);
```

- [ ] **Step 2: Add local-saturation failover**

Pre-hold all slots on the preferred account, leave a compatible account free, and assert the alternate is selected immediately, without a route-health snapshot on the preferred route.

- [ ] **Step 3: Verify RED**

```bash
rtk cargo test --test gateway responses_continuation_503_fails_over_to_compatible_account
rtk cargo test --test gateway responses_continuation_local_saturation_uses_compatible_account
```

Expected: exact-profile filtering prevents the alternate from being called.

- [ ] **Step 4: Replace exact equality with contract equality**

Define candidate matching as:

```rust
let route_matches_continuation =
    |upstream: &UpstreamConfig, key_fingerprint: &str, protocol: UpstreamProtocol| {
        let Some(continuation) = continuation.as_ref() else {
            return true;
        };
        let Some(contract) = continuation.contract() else {
            return false;
        };
        let Some(evaluation) = route_capability(upstream, key_fingerprint, protocol) else {
            return false;
        };
        let (Some(resolved), Some(profile), Some(runtime_model_slug)) = (
            evaluation.resolved.as_ref(),
            current_exact_profile(upstream, key_fingerprint, protocol),
            upstream.resolved_model_name(model),
        ) else {
            return false;
        };
        continuation_contract_for_route(
            upstream,
            &runtime_model_slug,
            WireProtocol::from(endpoint.native_protocol()),
            WireProtocol::from(protocol),
            &contract.required_capabilities,
            resolved,
            profile,
            contract.tool_registry_version,
        )
        .as_ref()
            == Some(contract)
    };
```

Do not use runtime health to decide semantic compatibility. Health affects ranking after this predicate.

- [ ] **Step 5: Keep the preferred exact profile first**

Continue moving its upstream to index zero. Change the existing binding to `let mut candidate_keys = ...collect::<Vec<_>>();`, then sort it so the stored preferred key fingerprint is first within that upstream:

```rust
candidate_keys.sort_by_key(|api_key| {
    let fingerprint = route_key_fingerprint(&upstream, api_key);
    (fingerprint != continuation.preferred_profile().key_fingerprint, fingerprint)
});
```

If the preferred route is cooling or locally saturated, normal candidate processing records request-local evidence and continues to compatible alternatives.

- [ ] **Step 6: Run and commit routing behavior**

```bash
rtk cargo test --test gateway responses_continuation_503_fails_over_to_compatible_account
rtk cargo test --test gateway responses_continuation_local_saturation_uses_compatible_account
rtk cargo test --test gateway responses_continuation_keeps_chat_profile_when_responses_becomes_eligible
rtk git add src/server/gateway.rs tests/gateway/responses/reasoning.rs
rtk git commit -m "fix(responses): fail over compatible continuations before output" -m "Constraint: Exact route remains preferred" -m "Confidence: high" -m "Scope-risk: broad"
```

### Task 5: Store The Successful Route As The New Preference

**Files:**
- Modify: `src/server/gateway/upstream.rs:1640-1679`
- Modify: `src/server/gateway/capability_routing.rs`
- Test: `tests/gateway/responses/reasoning.rs`

- [ ] **Step 1: Add a two-hop continuation test**

First response uses account A, first resume fails over to B, second resume keeps the same contract and tries B first. Assert A is not retried on the second resume while B is healthy.

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test gateway successful_continuation_failover_updates_preferred_profile
```

Expected: stored state either retains A or rebuilds an unrelated contract.

- [ ] **Step 3: Reuse the existing contract while updating preference**

Add:

```rust
pub(super) fn with_preferred_profile(
    &self,
    preferred_profile: DialectProfileKey,
    configuration_fingerprint: String,
) -> Self {
    Self {
        version: GATEWAY_CONTINUATION_VERSION,
        preferred_profile,
        configuration_fingerprint,
        compatibility_contract: self.compatibility_contract.clone(),
        probe_schema_version: DIALECT_PROBE_SCHEMA_VERSION,
        reasoning_carrier: self.reasoning_carrier,
        required_capabilities: self.required_capabilities.clone(),
        adapter_identity: self.adapter_identity.clone(),
    }
}
```

For a first response, construct the contract from the selected route. For a resume, call `with_preferred_profile` after verifying the selected route matched the existing contract.

- [ ] **Step 4: Run and commit preference storage**

```bash
rtk cargo test --test gateway successful_continuation_failover_updates_preferred_profile
rtk git add src/server/gateway/upstream.rs src/server/gateway/capability_routing.rs tests/gateway/responses/reasoning.rs
rtk git commit -m "fix(responses): remember successful continuation failover" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 6: Prove Every Mismatch And Replay Barrier

**Files:**
- Modify: `tests/gateway/responses/reasoning.rs`
- Modify: `tests/gateway/responses/tools.rs`
- Modify: `tests/gateway/stream_only_learning.rs`

- [ ] **Step 1: Add a table-driven incompatible-route test**

For each contract field mutation from Task 2, make the preferred route fail before output and assert the alternate hit count remains zero and the terminal error is stable.

- [ ] **Step 2: Add post-output barriers**

Cover text delta, reasoning delta, function-call identity, and partial function arguments. For each, fail the preferred stream and assert no compatible alternate is called and no tool ID is duplicated.

- [ ] **Step 3: Run all continuation tests**

```bash
rtk cargo test --test gateway responses_continuation_rejects_each_contract_mismatch
rtk cargo test --test gateway responses_continuation_after_semantic_output_never_replays
rtk cargo test --test gateway responses_previous_response_id
rtk cargo test --test gateway stream_only_learning
```

Expected: pre-output operational failures may fail over; every semantic event closes replay.

- [ ] **Step 4: Commit mismatch coverage**

```bash
rtk git add tests/gateway/responses tests/gateway/stream_only_learning.rs
rtk git commit -m "test(responses): enforce continuation compatibility boundaries" -m "Confidence: high" -m "Scope-risk: narrow"
```
