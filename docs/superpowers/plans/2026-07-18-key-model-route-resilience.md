# Multi-Key Model Route Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every request through a Key-model-protocol deployment that is explicitly capable and currently healthy, while keeping the persisted model catalog stable through transient upstream failures.

**Architecture:** Keep `UpstreamConfig` as the account and quota boundary, but derive an internal virtual route for each `(upstream_id, key_fingerprint, runtime_model_slug, protocol)`. Persist exact Key-model mappings and Key-scoped capability evidence; keep cooldowns, half-open leases, negative capability hints, and attempt ledgers in bounded process memory so request failures never rewrite configuration. Background discovery is the only automatic mapping writer and uses exact snapshots, last-known-good preservation, and two-observation removals.

**Tech Stack:** Rust 2021, Tokio, Axum, reqwest, serde/serde_json, SHA-256, PostgreSQL, Vue 3, TypeScript, Vitest, Cargo integration tests

---

This remains one dependent plan rather than separate subsystem plans: keyed mapping identity is required by capability persistence, route health requires the same identity, terminal routing consumes that health, and background discovery must not be enabled until all three safely consume exact mappings. Every task leaves legacy single-Key routing compiling and testable, while Task 12 is intentionally the last behavior-enabling state writer.

## File Structure

- Create `src/state/route_health.rs`: pure bounded Key/route health registry, deterministic cooldown policy, half-open leases, route quarantine, and safe snapshots.
- Create `src/server/gateway/route_attempts.rs`: virtual route candidates, request-scoped attempted set, failure ledger, same-route retry decision, and terminal error aggregation.
- Create `src/capabilities/runtime_hints.rs`: bounded 15-minute Key-route capability negative hints which are never serialized.
- Create `src/state/model_key_sync.rs`: periodic and targeted discovery coordinator, exact configuration snapshots, missing-model confirmation, and atomic writeback.
- Create `tests/multi_key_mapping.rs`: exact mapping normalization and file-store compatibility tests.
- Create `tests/model_key_sync.rs`: background/targeted discovery and stale-snapshot regressions.
- Modify `src/keys.rs`: stable upstream Key fingerprint and anonymous route ID helpers.
- Modify `src/state/normalize.rs`: authoritative mapping normalization, current-Key-only routing, and derived model union.
- Modify `src/state/freekey_sync.rs`: preserve submitted exact mappings in replace mode and derive the aggregate union.
- Modify `src/state/model_discovery.rs`: index-addressed, ordered, non-empty discovery results without Key prefixes.
- Modify `src/state.rs`: keyed capability jobs, profile migration, runtime registries, stable catalog, sync entry points, and removal cleanup.
- Modify `src/state/types.rs`: sync configuration semantics and safe runtime snapshot DTOs.
- Modify `src/state/postgres.rs`: transactional keyed profile schema migration and keyed profile queries.
- Modify `src/capabilities/types.rs`: Key-aware `RouteIdentity` and `DialectProfileKey` constructors with legacy deserialization.
- Modify `src/capabilities/mod.rs`: export runtime hint types.
- Modify `src/capabilities/resolver.rs`, `src/capabilities/policy.rs`, and `src/capabilities/probe_queue.rs`: propagate the keyed identity without changing policy selector semantics.
- Modify `src/server/admin.rs`: `key_index` discovery contract, anonymous `route_id`, safe route-health aggregate, and exact admin save behavior.
- Modify `src/server/gateway.rs`: virtual route selection, exact health actions, retry/fallback, terminal aggregation, stable trace fields, and no hot-path persistence.
- Modify `src/server/gateway/upstream.rs`: structured upstream feedback, physical-attempt admission, keyed hedging, and route attribution.
- Modify `src/server/gateway/capability_probe.rs`: one probe job per mapped Key and exact Key resolution at execution.
- Modify `src/server/gateway/capability_routing.rs`: Key-aware capability resolution and persistent-only catalog witnesses.
- Modify `src/server/gateway/capability_admin.rs` and `src/server/gateway/troubleshooting.rs`: keyed lookups with safe DTOs.
- Modify `src/server/gateway/stream.rs`: route-aware stream completion and cancellation semantics.
- Modify `src/server/gateway/errors.rs`: stable exhausted/model/credential/capability error codes and safe details.
- Modify `src/upstream_feedback.rs`: precedence-based error classification by status, structured code, headers, and narrow message patterns.
- Modify `src/main.rs`: allow sync interval `0`, start the bounded discovery loop only when enabled, and retain the single-process boundary.
- Modify `frontend/src/api/admin.ts`, `frontend/src/types/index.ts`, `frontend/src/views/admin/Upstreams.vue`, `frontend/src/components/ModelProbeBoard.vue`, and `frontend/src/utils/modelProbeCharts.ts`: index-based discovery and anonymous route display.
- Modify focused Rust tests under `tests/capability_*.rs`, `tests/gateway/`, `tests/unit/`, `tests/admin_*.rs`, and `tests/postgres_roundtrip.rs` as named in each task.
- Modify `frontend/tests/api/admin.spec.ts`, `frontend/tests/views/admin-ui.spec.ts`, and `frontend/tests/utils/modelProbeCharts.spec.ts`: frontend contract regressions.
- Modify `.env.example`, `docker-compose.yml`, `README.md`, `DEPLOYMENT.md`, `tests/templates.rs`, and `tests/docker.rs`: active sync settings, deprecated request-path rate-limit retries, and single-active-instance documentation.

## Task 1: Make Key-Model Mappings Exact And Authoritative

**Files:**
- Create: `tests/multi_key_mapping.rs`
- Modify: `src/state/normalize.rs:39`
- Modify: `src/state/freekey_sync.rs:485`
- Test: `tests/state_store.rs`
- Test: `tests/postgres_roundtrip.rs`

- [ ] **Step 1: Write the failing mapping-invariant tests**

Add a focused fixture and these assertions to `tests/multi_key_mapping.rs`:

```rust
fn mapping(key: &str, models: &[&str]) -> ApiKeyModelConfig {
    ApiKeyModelConfig {
        api_key: key.to_string(),
        supported_models: models.iter().map(|model| (*model).to_string()).collect(),
    }
}

#[test]
fn authoritative_normalization_preserves_empty_current_keys_and_derives_union() {
    let mut upstream = UpstreamConfig {
        api_key: " key-a ".into(),
        api_keys: vec!["key-b".into(), "key-a".into()],
        api_key_models: vec![
            mapping("key-b", &[]),
            mapping("key-a", &["glm-5.2"]),
            mapping("key-a", &["glm-4.7", "glm-5.2"]),
            mapping("deleted-key", &["stale-model"]),
        ],
        supported_models: vec!["stale-model".into()],
        ..UpstreamConfig::default()
    };

    upstream.normalize_for_storage();

    assert_eq!(upstream.available_keys(), vec!["key-a", "key-b"]);
    assert_eq!(
        upstream.api_key_models,
        vec![
            mapping("key-b", &[]),
            mapping("key-a", &["glm-5.2", "glm-4.7"]),
        ]
    );
    assert_eq!(upstream.supported_models, vec!["glm-5.2", "glm-4.7"]);
    assert!(upstream.keys_for_model("missing-model").is_empty());
}

#[test]
fn authoritative_normalization_appends_a_missing_current_key_as_empty() {
    let mut upstream = UpstreamConfig {
        api_key: "key-a".into(),
        api_keys: vec!["key-b".into()],
        api_key_models: vec![mapping("key-a", &["glm-5.2"])],
        ..UpstreamConfig::default()
    };
    upstream.normalize_for_storage();
    assert_eq!(
        upstream.api_key_models,
        vec![mapping("key-a", &["glm-5.2"]), mapping("key-b", &[])]
    );
}

#[test]
fn legacy_mapping_falls_back_only_to_the_current_configured_keys() {
    let upstream = UpstreamConfig {
        api_key: "key-a".into(),
        api_keys: vec!["key-b".into()],
        api_key_models: Vec::new(),
        ..UpstreamConfig::default()
    };
    assert_eq!(upstream.keys_for_model("glm-5.2"), vec!["key-a", "key-b"]);
}
```

Add file and PostgreSQL roundtrip cases which store an authoritative empty entry and assert it is still present after reload. Add a freekey replace-mode regression which submits `api_key_models` plus a disagreeing aggregate and asserts the backend derives the aggregate without clearing the mapping.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test multi_key_mapping -- --nocapture
rtk cargo test --locked --offline --test state_store authoritative -- --nocapture
rtk cargo test --locked --offline --test postgres_roundtrip authoritative -- --nocapture
```

Expected: empty entries are dropped, `deleted-key` is returned by `available_keys()`, or the stale aggregate survives. These failures prove the current mapping is not authoritative.

- [ ] **Step 3: Implement current-Key-only normalization**

Replace the existing one-record-per-non-empty-mapping helper with these responsibilities:

```rust
fn normalized_current_keys(api_key: &str, api_keys: Vec<String>) -> Vec<String> {
    normalized_string_list(
        std::iter::once(api_key.to_string())
            .chain(api_keys)
            .collect(),
    )
}

fn normalized_api_key_models(
    values: Vec<ApiKeyModelConfig>,
    current_keys: &[String],
) -> Vec<ApiKeyModelConfig> {
    let current = current_keys.iter().cloned().collect::<HashSet<_>>();
    let mut positions = std::collections::HashMap::<String, usize>::new();
    let mut mappings = Vec::<ApiKeyModelConfig>::new();

    for value in values {
        let api_key = value.api_key.trim().to_string();
        if api_key.is_empty() || !current.contains(&api_key) {
            continue;
        }
        let models = normalized_string_list(value.supported_models);
        if let Some(index) = positions.get(&api_key).copied() {
            let merged = mappings[index]
                .supported_models
                .iter()
                .cloned()
                .chain(models)
                .collect();
            mappings[index].supported_models = normalized_string_list(merged);
        } else {
            positions.insert(api_key.clone(), mappings.len());
            mappings.push(ApiKeyModelConfig { api_key, supported_models: models });
        }
    }

    for api_key in current_keys {
        if !positions.contains_key(api_key) {
            mappings.push(ApiKeyModelConfig {
                api_key: api_key.clone(),
                supported_models: Vec::new(),
            });
        }
    }
    mappings
}

fn derive_supported_models(mappings: &[ApiKeyModelConfig]) -> Vec<String> {
    normalized_string_list(
        mappings
            .iter()
            .flat_map(|mapping| mapping.supported_models.iter().cloned())
            .collect(),
    )
}
```

In `normalize_for_storage()`, capture `let authoritative = !self.api_key_models.is_empty()` before taking the vector. Build the current Key set from trimmed `api_key` followed by `api_keys`; if authoritative, normalize mappings against that set and replace `supported_models` with `derive_supported_models()`. If legacy, keep normalized persisted `supported_models`. Make `available_keys()` read only `api_key` and `api_keys`; keep `keys_for_model()` fallback only when `api_key_models.is_empty()`.

In freekey replace mode, delete the branch that clears `api_key_models` when aggregate sets disagree. Normalize the submitted mapping against the replacement Key set and always derive `supported_models` from the authoritative result.

- [ ] **Step 4: Run mapping and persistence tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test multi_key_mapping -- --nocapture
rtk cargo test --locked --offline --test state_store -- --nocapture
rtk cargo test --locked --offline --test postgres_roundtrip authoritative -- --nocapture
rtk cargo test --locked --offline --test admin_upstreams api_key_models -- --nocapture
```

Expected: all focused tests pass; authoritative empty entries survive file/PostgreSQL roundtrips and deleted Keys cannot re-enter `available_keys()`.

- [ ] **Step 5: Commit exact mapping semantics**

```bash
rtk git add src/state/normalize.rs src/state/freekey_sync.rs tests/multi_key_mapping.rs tests/state_store.rs tests/postgres_roundtrip.rs tests/admin_upstreams.rs
rtk git commit -m "fix(state): make per-key model mappings authoritative"
```

## Task 2: Return Discovery Results By Stable Key Index

**Files:**
- Modify: `src/state/model_discovery.rs:5`
- Modify: `src/server/admin.rs:1100`
- Modify: `frontend/src/api/admin.ts:35`
- Modify: `frontend/src/types/index.ts:270`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Test: `tests/admin_upstreams.rs`
- Test: `tests/admin_model_probe.rs`
- Test: `frontend/tests/api/admin.spec.ts`
- Test: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Write failing ordered-discovery contract tests**

Add backend cases for duplicate inputs, a failed middle Key, an empty successful payload, and a provider error body containing a sentinel secret. Assert every input position produces one result in request order and the serialized response contains neither the submitted Keys nor the sentinel body:

```rust
assert_eq!(payload["results"].as_array().unwrap().len(), 3);
assert_eq!(payload["results"][0]["key_index"], 0);
assert_eq!(payload["results"][1]["key_index"], 1);
assert_eq!(payload["results"][2]["key_index"], 2);
assert!(payload.to_string().find("key_prefix").is_none());
assert!(payload["results"][1]["error"].is_string());
assert!(!payload.to_string().contains("provider-body-secret"));
assert!(!payload.to_string().contains("submitted-key-secret"));
```

In the frontend API and view tests, return:

```ts
results: [
  { key_index: 0, models: 1, model_list: ['glm-5.2'] },
  { key_index: 1, error: 'upstream returned 503' },
]
```

Assert the editor associates result `0` with the first local Key, preserves the existing mapping for failed Key `1`, stores an empty mapping when failed Key `1` is newly added, and renders `Key #1`/`Key #2` without displaying either secret.

- [ ] **Step 2: Run backend and frontend tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test admin_upstreams discovery_results -- --nocapture
rtk cargo test --locked --offline --test admin_model_probe discovery_results -- --nocapture
rtk npm test -- tests/api/admin.spec.ts tests/views/admin-ui.spec.ts
```

Run the npm command from `frontend/`. Expected: backend responses expose `key_prefix`, and TypeScript/view assertions fail because the client has no `key_index` contract.

- [ ] **Step 3: Implement the ordered index contract**

Keep raw Keys internal and make the discovery result index-addressed:

```rust
pub const MODEL_DISCOVERY_MAX_CONCURRENCY: usize = 8;

#[derive(Debug, Clone)]
pub struct KeyModelDiscoveryResult {
    pub key_index: usize,
    pub models: Vec<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}
```

Deduplicate identical trimmed Keys only around the HTTP future, execute at most `MODEL_DISCOVERY_MAX_CONCURRENCY` HTTP futures with `buffer_unordered`, then expand the shared result back to every original index and sort by `key_index`. Continue treating an HTTP 2xx response with zero model IDs as `Err("upstream returned no models")`. For non-2xx and parse failures, return a bounded structural message containing the endpoint class and numeric status/error kind, never the response body or raw Key.

For batch create and `discover-models`, look the Key up as `payload.keys[result.key_index]`; never serialize it. Batch create stores every non-empty submitted Key, not only successful Keys, and builds one authoritative mapping per current Key: successful results use the discovered models and failed results use `supported_models: []`. This makes partial and all-failed batch creation safe instead of falling back to blind legacy routing. Emit exactly one of:

```json
{"key_index":0,"models":2,"model_list":["glm-5.2","glm-4.7"]}
```

```json
{"key_index":1,"error":"safe upstream discovery summary"}
```

In `Upstreams.vue`, rebuild mappings from the current editor Key array: successful indexed results replace that Key's models, failed existing Keys retain their prior models, failed new Keys receive `[]`, and removed Keys are not copied. Use `Key #${key_index + 1}` for display only.

- [ ] **Step 4: Run discovery and frontend tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test admin_upstreams discovery_results -- --nocapture
rtk cargo test --locked --offline --test admin_model_probe -- --nocapture
rtk npm test -- tests/api/admin.spec.ts tests/views/admin-ui.spec.ts
rtk npm run build
```

Run npm commands from `frontend/`. Expected: duplicate Keys still yield duplicate indexed result rows, failures preserve the correct local mapping, and no discovery response includes a Key prefix.

- [ ] **Step 5: Commit the index-based admin contract**

```bash
rtk git add src/state/model_discovery.rs src/server/admin.rs frontend/src/api/admin.ts frontend/src/types/index.ts frontend/src/views/admin/Upstreams.vue frontend/tests/api/admin.spec.ts frontend/tests/views/admin-ui.spec.ts tests/admin_upstreams.rs tests/admin_model_probe.rs
rtk git commit -m "feat(admin): address model discovery results by key index"
```

## Task 3: Introduce Stable Key And Virtual Route Identity

**Files:**
- Modify: `src/keys.rs`
- Modify: `src/capabilities/types.rs:388`
- Modify: `src/server/admin.rs:140`
- Modify: `src/state.rs`
- Modify: `src/state/postgres.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/upstream.rs`
- Modify: `src/server/gateway/capability_admin.rs`
- Modify: `src/server/gateway/capability_probe.rs`
- Modify: `src/server/gateway/capability_probe.rs`
- Modify: `src/server/gateway/capability_routing.rs`
- Modify: `src/server/gateway/troubleshooting.rs`
- Modify: `frontend/src/types/index.ts:280`
- Modify: `frontend/src/components/ModelProbeBoard.vue`
- Modify: `frontend/src/utils/modelProbeCharts.ts`
- Test: `tests/keys.rs`
- Test: `tests/admin_model_probe.rs`
- Test: `frontend/tests/utils/modelProbeCharts.spec.ts`
- Test fixtures: `tests/capability_policy.rs`, `tests/capability_resolver.rs`, `tests/capability_probe.rs`, `tests/capability_profiles.rs`, `tests/capability_state.rs`, `tests/admin_capabilities.rs`, `tests/admin_upstreams.rs`, `tests/probe_queue.rs`, `tests/postgres_roundtrip.rs`, `tests/load.rs`, `tests/troubleshooting.rs`, `tests/unit/server/gateway.rs`, `tests/gateway/capability_routing.rs`, `tests/gateway/dialect_retry.rs`, `tests/gateway/compatibility.rs`, `tests/gateway/aggregate.rs`, `tests/gateway/claude.rs`, `tests/gateway/images.rs`, `tests/gateway/stream_only.rs`, `tests/gateway/stream_only_learning.rs`, `tests/gateway/chat/core.rs`, `tests/gateway/chat/support.rs`, `tests/gateway/chat/context.rs`, `tests/gateway/responses/fallback.rs`, `tests/gateway/responses/history.rs`, `tests/gateway/responses/reasoning.rs`, and `tests/gateway/responses/tools.rs`

- [ ] **Step 1: Write fingerprint, rotation, and redaction tests**

Add deterministic vectors and isolation assertions:

```rust
#[test]
fn upstream_key_fingerprint_is_domain_separated_trimmed_and_upstream_scoped() {
    let a = upstream_key_fingerprint("up-a", " secret-key ");
    assert_eq!(a, upstream_key_fingerprint("up-a", "secret-key"));
    assert_ne!(a, upstream_key_fingerprint("up-b", "secret-key"));
    assert_ne!(a, upstream_key_fingerprint("up-a", "rotated-key"));
    assert_eq!(
        a,
        "60c9985cdf9ec0e721ca09fa7a92970b95d6da200aae8bae4ff09239c7206802"
    );
}

#[test]
fn route_id_does_not_embed_a_secret_or_key_fingerprint() {
    let fingerprint = upstream_key_fingerprint("up-a", "secret-key");
    let id = anonymous_route_id(
        "up-a",
        &fingerprint,
        "glm-5.2",
        WireProtocol::Responses,
    );
    assert_eq!(id, "route_741e07bf874c1e8e");
    assert!(id.starts_with("route_"));
    assert!(!id.contains("secret-key"));
    assert!(!id.contains(&fingerprint));
}
```

Add an admin model-probe assertion that the response contains `route_id`, does not contain `key_prefix`, and contains neither the secret nor full fingerprint.

- [ ] **Step 2: Run identity tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test keys upstream_key_fingerprint -- --nocapture
rtk cargo test --locked --offline --test admin_model_probe route_id -- --nocapture
rtk npm test -- tests/utils/modelProbeCharts.spec.ts
```

Expected: helpers and fields do not exist, and the current probe board still keys/sorts channels by secret-derived prefixes.

- [ ] **Step 3: Implement the stable identity types**

In `src/keys.rs`, implement lowercase hexadecimal SHA-256 over exact domain-separated bytes:

```rust
fn sha256_hex(parts: &[&[u8]]) -> String {
    use std::fmt::Write as _;

    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().iter().fold(
        String::with_capacity(64),
        |mut output, byte| {
            write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
            output
        },
    )
}

fn wire_protocol_identity(protocol: WireProtocol) -> &'static [u8] {
    match protocol {
        WireProtocol::ChatCompletions => b"chat_completions",
        WireProtocol::Responses => b"responses",
        WireProtocol::Messages => b"messages",
    }
}

pub fn upstream_key_fingerprint(upstream_id: &str, api_key: &str) -> String {
    sha256_hex(&[
        b"chat2responses:key:v1",
        b"\0",
        upstream_id.as_bytes(),
        b"\0",
        api_key.trim().as_bytes(),
    ])
}

pub fn anonymous_route_id(
    upstream_id: &str,
    key_fingerprint: &str,
    runtime_model_slug: &str,
    protocol: WireProtocol,
) -> String {
    let digest = sha256_hex(&[
        b"chat2responses:route-id:v1",
        b"\0",
        upstream_id.as_bytes(),
        b"\0",
        key_fingerprint.as_bytes(),
        b"\0",
        runtime_model_slug.as_bytes(),
        b"\0",
        wire_protocol_identity(protocol),
    ]);
    format!("route_{}", &digest[..16])
}
```

Add `key_fingerprint: String` to `RouteIdentity` and to `DialectProfileKey`. Mark only the profile field `#[serde(default)]` so missing persisted values deserialize as the legacy empty string. Define and use these constructors:

```rust
impl DialectProfileKey {
    pub fn for_key(
        upstream_id: impl Into<String>,
        key_fingerprint: impl Into<String>,
        runtime_model_slug: impl Into<String>,
        protocol: WireProtocol,
    ) -> Self {
        Self {
            upstream_id: upstream_id.into(),
            key_fingerprint: key_fingerprint.into(),
            runtime_model_slug: runtime_model_slug.into(),
            protocol,
        }
    }

    pub fn legacy(
        upstream_id: impl Into<String>,
        runtime_model_slug: impl Into<String>,
        protocol: WireProtocol,
    ) -> Self {
        Self::for_key(upstream_id, "", runtime_model_slug, protocol)
    }
}
```

Update production route construction to call `for_key`; use `legacy` only in tests that intentionally load old documents. Update the listed fixture factories once so downstream tests do not repeat raw struct literals.

In model-probe responses, derive the full Key fingerprint from `upstream.id` plus `keys[result.key_index]`, derive `route_id` using a stable catalog label such as `"models-probe"` and the upstream's primary protocol, and serialize only `route_id`. Update the Vue board and chart sorting to use `route_id`.

- [ ] **Step 4: Run identity, capability fixture, and frontend tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test keys -- --nocapture
rtk cargo test --locked --offline --test admin_model_probe -- --nocapture
rtk cargo test --locked --offline --test capability_policy -- --nocapture
rtk cargo test --locked --offline --test capability_resolver -- --nocapture
rtk cargo test --locked --offline --test capability_profiles -- --nocapture
rtk npm test -- tests/utils/modelProbeCharts.spec.ts tests/views/admin-ui.spec.ts
rtk npm run build
```

Expected: keyed and legacy constructors compile across the fixture matrix, model-probe DTOs expose only anonymous route IDs, and frontend types contain no model-probe `key_prefix`.

- [ ] **Step 5: Commit virtual route identity**

```bash
rtk git add src/keys.rs src/capabilities/types.rs src/server/admin.rs src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/capability_admin.rs src/server/gateway/capability_probe.rs src/server/gateway/capability_routing.rs src/server/gateway/troubleshooting.rs src/state.rs src/state/postgres.rs
rtk git add frontend/src/types/index.ts frontend/src/components/ModelProbeBoard.vue frontend/src/utils/modelProbeCharts.ts frontend/tests/utils/modelProbeCharts.spec.ts frontend/tests/views/admin-ui.spec.ts
rtk git add tests/keys.rs tests/admin_model_probe.rs tests/capability_policy.rs tests/capability_resolver.rs tests/capability_probe.rs tests/capability_profiles.rs tests/capability_state.rs tests/admin_capabilities.rs tests/admin_upstreams.rs tests/probe_queue.rs tests/postgres_roundtrip.rs tests/load.rs tests/troubleshooting.rs tests/unit/server/gateway.rs tests/gateway/capability_routing.rs tests/gateway/dialect_retry.rs tests/gateway/compatibility.rs tests/gateway/aggregate.rs tests/gateway/claude.rs tests/gateway/images.rs tests/gateway/stream_only.rs tests/gateway/stream_only_learning.rs tests/gateway/chat/core.rs tests/gateway/chat/support.rs tests/gateway/chat/context.rs tests/gateway/responses/fallback.rs tests/gateway/responses/history.rs tests/gateway/responses/reasoning.rs tests/gateway/responses/tools.rs
rtk git commit -m "feat(routing): add stable key-aware route identity"
```

## Task 4: Migrate Capability Profiles To Key-Scoped Persistence

**Files:**
- Modify: `src/state/postgres.rs:309`
- Modify: `src/state.rs:808`
- Modify: `src/state/file_store.rs`
- Test: `tests/postgres_roundtrip.rs`
- Test: `tests/capability_state.rs`
- Test: `tests/capability_profiles.rs`
- Test: `tests/admin_capabilities.rs`

- [ ] **Step 1: Write failing profile migration tests**

Add these four named regressions with exact postconditions:

```rust
#[tokio::test]
async fn postgres_migrates_the_legacy_dialect_profile_primary_key() {
    let columns = primary_key_columns(&client, "dialect_profiles").await;
    assert_eq!(
        columns,
        vec!["upstream_id", "key_fingerprint", "runtime_model_slug", "protocol"]
    );
}

#[tokio::test]
async fn postgres_roundtrips_two_key_profiles_for_the_same_model_protocol() {
    store.upsert_dialect_profile(&profile("fingerprint-a")).await.unwrap();
    store.upsert_dialect_profile(&profile("fingerprint-b")).await.unwrap();
    let loaded = store.load_capability_state().await.unwrap();
    assert!(loaded.profiles.contains_key(&profile_key("fingerprint-a")));
    assert!(loaded.profiles.contains_key(&profile_key("fingerprint-b")));
}

#[tokio::test]
async fn single_key_startup_rebinds_a_current_legacy_profile() {
    let snapshot = load_single_key_legacy_fixture().await.capability_snapshot();
    assert!(snapshot.profiles.contains_key(&profile_key(&expected_fingerprint())));
    assert!(!snapshot.profiles.contains_key(&DialectProfileKey::legacy(
        "up-1", "glm-5.2", WireProtocol::Responses,
    )));
}

#[tokio::test]
async fn multi_key_startup_discards_ambiguous_legacy_evidence_and_queues_both_keys() {
    let (state, jobs) = load_multi_key_legacy_fixture().await;
    assert!(state.capability_snapshot().profiles.is_empty());
    assert_eq!(jobs.iter().map(|job| &job.key.key_fingerprint).collect::<BTreeSet<_>>().len(), 2);
}
```

For rollback, start a transaction against the old schema, execute a test migration whose final statement intentionally references a missing column, assert the error, and then assert the original primary key and absence of `key_fingerprint` are unchanged after rollback.

For file mode, deserialize a profile whose `key` omits `key_fingerprint`, assert it becomes `DialectProfileKey::legacy(...)`, and verify startup either rebinds the single-Key row or removes the ambiguous multi-Key row without leaking the raw Key.

- [ ] **Step 2: Run persistence migration tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test capability_state legacy_profile -- --nocapture
rtk cargo test --locked --offline --test capability_profiles keyed_profile -- --nocapture
rtk cargo test --locked --offline --test postgres_roundtrip dialect_profile -- --nocapture
```

Expected: keyed profiles collide on the old primary key, old JSON fails `deny_unknown_fields` or remains ambiguous, and legacy rebind behavior is absent.

- [ ] **Step 3: Implement transactional schema and legacy migration**

Change profile SQL to select, order, insert, and conflict on `key_fingerprint`:

```sql
ALTER TABLE dialect_profiles
    ADD COLUMN IF NOT EXISTS key_fingerprint TEXT NOT NULL DEFAULT '';
ALTER TABLE dialect_profiles DROP CONSTRAINT IF EXISTS dialect_profiles_pkey;
ALTER TABLE dialect_profiles
    ADD CONSTRAINT dialect_profiles_pkey
    PRIMARY KEY (upstream_id, key_fingerprint, runtime_model_slug, protocol);
```

Run schema creation and this migration through one PostgreSQL transaction in `initialize_schema()`; commit only after every statement succeeds. Propagate any migration error through `load_from_database_url()` so `main.rs` aborts startup instead of publishing a partially compatible state. Query `pg_constraint`/`pg_attribute` first and run the drop/add sequence only when the existing primary-key column list is not already the four-column target. For new databases, define the four-column key directly in `CREATE TABLE`. Add `delete_dialect_profile(&DialectProfileKey)` so a rebound row can be upserted before deleting its empty-fingerprint predecessor.

Split route fingerprints into:

```rust
fn legacy_route_configuration_fingerprint_with_snapshot(
    snapshot: &CapabilityRuntimeSnapshot,
    upstream: &UpstreamConfig,
    exposed_model: &str,
    runtime_model: &str,
    protocol: UpstreamProtocol,
) -> io::Result<String>;

fn route_configuration_fingerprint_with_snapshot(
    snapshot: &CapabilityRuntimeSnapshot,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    exposed_model: &str,
    runtime_model: &str,
    protocol: UpstreamProtocol,
) -> io::Result<String>;
```

The new function hashes the legacy material plus `NUL` and `key_fingerprint`. During capability startup, group legacy rows by upstream: rebind only when exactly one current Key exists and the stored fingerprint still matches the legacy algorithm; otherwise delete the legacy row. On every configuration reconciliation, delete profiles whose fingerprint no longer matches a current Key or whose model/protocol is no longer routable; Key rotation therefore removes the old identity before new evidence is used. Publish the migrated snapshot only after file/PostgreSQL writes succeed. Queue keyed probes for every discarded ambiguous route.

- [ ] **Step 4: Run profile persistence tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test capability_state -- --nocapture
rtk cargo test --locked --offline --test capability_profiles -- --nocapture
rtk cargo test --locked --offline --test admin_capabilities -- --nocapture
rtk cargo test --locked --offline --test postgres_roundtrip dialect_profile -- --nocapture
```

Expected: file and PostgreSQL modes retain independent Key profiles, valid single-Key legacy evidence is rebound once, ambiguous evidence is removed, and failed DDL leaves the old table intact.

- [ ] **Step 5: Commit the keyed profile migration**

```bash
rtk git add src/state.rs src/state/postgres.rs src/state/file_store.rs tests/capability_state.rs tests/capability_profiles.rs tests/admin_capabilities.rs tests/postgres_roundtrip.rs
rtk git commit -m "feat(capabilities): persist profiles per upstream key"
```

## Task 5: Probe And Resolve Capabilities Per Key Route

**Files:**
- Modify: `src/state.rs:820`
- Modify: `src/server/gateway/capability_probe.rs:190`
- Modify: `src/server/gateway/capability_routing.rs:420`
- Modify: `src/server/gateway/capability_admin.rs`
- Modify: `src/server/gateway/troubleshooting.rs`
- Test: `tests/capability_probe.rs`
- Test: `tests/capability_state.rs`
- Test: `tests/gateway/capability_routing.rs`
- Test: `tests/gateway/claude.rs`
- Test: `tests/gateway/responses/history.rs`
- Test: `tests/admin_capabilities.rs`

- [ ] **Step 1: Write failing per-Key capability tests**

Create an authoritative upstream where Key A and Key B both map `glm-5.2`, but the mock rejects `reasoning_effort: "xhigh"` only for Key A. Assert:

```rust
let jobs = state.reconcile_dialect_profiles(now).await.unwrap();
assert_eq!(jobs.len(), 2);
assert_ne!(jobs[0].key.key_fingerprint, jobs[1].key.key_fingerprint);

let a = state.capability_snapshot().profiles.get(&key_a_profile).unwrap();
let b = state.capability_snapshot().profiles.get(&key_b_profile).unwrap();
assert_eq!(a.reasoning_controls["reasoning_effort"], vec!["low", "medium", "high"]);
assert!(b.reasoning_controls["reasoning_effort"].contains(&"xhigh".to_string()));
```

Add a stale-job case: rotate the raw Key after queueing and assert `run_probe_job()` performs no HTTP request and writes no profile. Add a catalog witness case in which only Key B has verified xhigh evidence and assert the witness profile key equals Key B.

- [ ] **Step 2: Run capability probe/routing tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test capability_probe per_key -- --nocapture
rtk cargo test --locked --offline --test capability_state reconcile_dialect_profiles -- --nocapture
rtk cargo test --locked --offline --test gateway capability_routing -- --nocapture
```

Expected: only one job is created, the first mapped Key is reused, or both resolutions read the same profile.

- [ ] **Step 3: Build one probe and resolver input per mapped Key**

Change capability job construction to iterate:

```rust
for api_key in upstream.keys_for_model(&exposed) {
    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
    for protocol in upstream.supported_protocols() {
        let key = DialectProfileKey::for_key(
            upstream.id.clone(),
            key_fingerprint.clone(),
            runtime.clone(),
            protocol.into(),
        );
        if let Some(index) = queued.get(&key).copied() {
            jobs[index].exposed_model_slugs.insert(exposed.clone());
        } else if let Some(job) = Self::build_capability_probe_job_for_key_with_snapshot(
            &snapshot,
            upstream,
            &key_fingerprint,
            &exposed,
            &runtime,
            protocol,
            ProbeReason::ConfigurationChanged,
        )? {
            queued.insert(key, jobs.len());
            jobs.push(job);
        }
    }
}
```

The new builder accepts only the fingerprint and stores it in `ProbeJob.key`; the raw Key is not part of `ProbeJob`, `ProbeConfigurationBinding`, queue deduplication, serialization, or logging.

Change the public targeted builder to the same identity:

```rust
pub async fn build_capability_probe_job(
    &self,
    upstream_id: &str,
    key_fingerprint: &str,
    exposed_model_slug: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    reason: ProbeReason,
) -> io::Result<Option<ProbeJob>>;
```

At execution, resolve the current raw Key by recomputing fingerprints over `upstream.available_keys()` and require exactly one match. If none matches, finish the submission as stale without probing or persisting.

Change capability routing signatures to require `key_fingerprint: &str`:

```rust
pub(super) fn resolve_route_capabilities_with_snapshot(
    snapshot: &CapabilityRuntimeSnapshot,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    exposed_model_slug: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    requested: &RequestedFeatures,
) -> Option<ResolvedCapabilities>;
```

Use the same identity in continuation validation, troubleshooting, admin summaries, and catalog witness iteration. Change `GatewayContinuationState::matches_route` to accept the candidate fingerprint and require all four profile-key fields. Claude thinking-signature verification must iterate current mapped Key routes, compute the keyed route fingerprint once per candidate, and accept exactly one matching route; replay then pins that exact Key instead of only its upstream/protocol. Policy route selectors continue matching upstream/model/protocol/tags; the fingerprint changes evidence identity, not policy glob behavior.

- [ ] **Step 4: Run capability tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test capability_probe -- --nocapture
rtk cargo test --locked --offline --test capability_state -- --nocapture
rtk cargo test --locked --offline --test gateway capability_routing -- --nocapture
rtk cargo test --locked --offline --test gateway claude::thinking -- --nocapture
rtk cargo test --locked --offline --test gateway responses::history -- --nocapture
rtk cargo test --locked --offline --test admin_capabilities -- --nocapture
```

Expected: each mapped Key receives an independent job/profile, stale jobs do no I/O, and xhigh evidence from one Key is never reused by another.

- [ ] **Step 5: Commit Key-scoped capability execution**

```bash
rtk git add src/state.rs src/server/gateway/capability_probe.rs src/server/gateway/capability_routing.rs src/server/gateway/capability_admin.rs src/server/gateway/troubleshooting.rs tests/capability_probe.rs tests/capability_state.rs tests/gateway/capability_routing.rs tests/gateway/claude.rs tests/gateway/responses/history.rs tests/admin_capabilities.rs
rtk git commit -m "feat(capabilities): probe and resolve each key route"
```

## Task 6: Classify Upstream Failures And Aggregate Terminal Errors

**Files:**
- Create: `src/server/gateway/route_attempts.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/errors.rs`
- Modify: `src/upstream_feedback.rs`
- Modify: `src/state.rs`
- Modify: `src/state/types.rs`
- Test: `tests/unit/upstream_feedback.rs`
- Test: `tests/unit/server/gateway.rs`

- [ ] **Step 1: Write failing classifier and ledger tests**

Replace the old conflated model/protocol assertions with a table covering the production incidents:

```rust
fn assert_class(status: u16, body: &str, expected: FailureClass) {
    let headers = reqwest::header::HeaderMap::new();
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status,
        headers: &headers,
        body: Some(body),
        target_model: Some("glm-5.2"),
    });
    assert_eq!(classified.class, expected);
}

#[test]
fn classifies_route_failures_by_precedence() {
    assert_class(500, r#"{"error":{"code":"openai_error"}}"#, FailureClass::TransientServer);
    assert_class(503, r#"{"error":{"message":"no available channel for model glm-5.2 under group free"}}"#, FailureClass::CapacityUnavailable);
    assert_class(400, r#"{"error":{"message":"model is not supported"}}"#, FailureClass::ModelUnsupported);
    assert_class(400, r#"{"error":{"message":"level \"xhigh\" not supported"}}"#, FailureClass::FeatureUnsupported);
    assert_class(404, r#"{"error":{"message":"endpoint not found"}}"#, FailureClass::ProtocolUnsupported);
    assert_class(400, r#"{"error":{"message":"invalid request"}}"#, FailureClass::RequestRejected);
    assert_class(401, "{}", FailureClass::Credentials);
    assert_class(429, "{}", FailureClass::RateLimited);
}

#[test]
fn no_available_channel_for_another_model_is_not_a_target_capacity_signal() {
    assert_class(
        503,
        r#"{"error":{"message":"no available channel for model other-model"}}"#,
        FailureClass::TransientServer,
    );
}
```

Explicitly assert an outer 503 with nested `inner_code: 400` remains `TransientServer`. Add terminal ledger tests for: any temporary candidate => 503 plus shortest retry; credentials-only => 502; model-only => 502; capability-only => 400; protocol-only => 502; non-temporary mixed exhaustion => 502.

- [ ] **Step 2: Run classifier tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --lib upstream_feedback -- --nocapture
rtk cargo test --locked --offline --lib route_attempts -- --nocapture
```

Expected: model, feature, and protocol messages collapse into `ProtocolUnsupported`, and no terminal ledger exists.

- [ ] **Step 3: Implement the pure classifier and ledger**

Define the shared stable, non-secret category in `src/state/types.rs`, add it to the explicit `pub use types::{...}` list in `src/state.rs`, and have the outer `src/upstream_feedback.rs` import it and re-export `RouteFailureClass as FailureClass` for concise gateway call sites:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteFailureClass {
    CapacityUnavailable,
    TransientServer,
    Transport,
    RateLimited,
    KeyQuota,
    Credentials,
    ModelUnsupported,
    FeatureUnsupported,
    ProtocolUnsupported,
    RequestRejected,
}

impl RouteFailureClass {
    pub fn is_temporary(self) -> bool {
        matches!(
            self,
            Self::CapacityUnavailable
                | Self::TransientServer
                | Self::Transport
                | Self::RateLimited
                | Self::KeyQuota
        )
    }
}

pub struct ClassifiedUpstreamFailure {
    pub class: RouteFailureClass,
    pub upstream_status: Option<u16>,
    pub retry_after: Option<Duration>,
}

pub struct UpstreamFeedbackInput<'a> {
    pub status: u16,
    pub headers: &'a reqwest::header::HeaderMap,
    pub body: Option<&'a str>,
    pub target_model: Option<&'a str>,
}
```

Classification order is structured status/code parsed through `serde_json::Value`, then the exact `no available channel`, model, xhigh/feature, and endpoint patterns, then status defaults. Emit `KeyQuota` only for an exact structured code such as `key_quota_exhausted` or a structured quota error whose scope is exactly `key`/`api_key`; free-text quota wording remains default route-scoped 429. Classify `no available channel` as capacity only when its normalized message names `target_model`; otherwise the outer 5xx default remains transient server failure. Never let a nested code override an outer 5xx. Parse `Retry-After` into the classified value without clipping it to the old maximum.

In `route_attempts.rs`, add:

```rust
pub struct AttemptFailure {
    pub route_id: String,
    pub upstream_status: Option<u16>,
    pub class: FailureClass,
    pub retry_after: Option<Duration>,
}

#[derive(Default)]
pub struct AttemptLedger {
    failures: Vec<AttemptFailure>,
    cooled_candidates: Vec<AttemptFailure>,
}

pub enum TerminalFailure {
    Temporary { retry_after: Duration },
    Credentials,
    ModelUnsupported,
    CapabilityUnsupported,
    ProtocolUnsupported,
    MixedRoutesExhausted,
}

impl AttemptLedger {
    pub fn terminal_failure(&self) -> TerminalFailure {
        let candidates = self
            .failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .collect::<Vec<_>>();
        assert!(!candidates.is_empty(), "terminal failure requires a candidate");

        if candidates.iter().any(|failure| failure.class.is_temporary()) {
            let retry_after = candidates
                .iter()
                .filter(|failure| failure.class.is_temporary())
                .filter_map(|failure| failure.retry_after)
                .min()
                .unwrap_or(Duration::from_secs(1));
            return TerminalFailure::Temporary { retry_after };
        }
        if candidates.iter().all(|failure| failure.class == FailureClass::Credentials) {
            return TerminalFailure::Credentials;
        }
        if candidates.iter().all(|failure| failure.class == FailureClass::ModelUnsupported) {
            return TerminalFailure::ModelUnsupported;
        }
        if candidates.iter().all(|failure| failure.class == FailureClass::FeatureUnsupported) {
            return TerminalFailure::CapabilityUnsupported;
        }
        if candidates.iter().all(|failure| failure.class == FailureClass::ProtocolUnsupported) {
            return TerminalFailure::ProtocolUnsupported;
        }
        TerminalFailure::MixedRoutesExhausted
    }
}
```

Map `TerminalFailure` to: temporary => `503 upstream_routes_exhausted`; credentials => `502 upstream_credentials_exhausted`; model => `502 upstream_model_unsupported`; capability => `400 capability_not_supported`; protocol => `502 upstream_protocol_unsupported`; mixed => `502 upstream_routes_exhausted`. Return only safe numeric class counts, attempt count, and retry seconds in `details`; exclude route IDs, fingerprints, raw bodies, prompts, and tool arguments while preserving existing OpenAI/Anthropic envelope shapes.

- [ ] **Step 4: Run classifier and envelope tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --lib upstream_feedback -- --nocapture
rtk cargo test --locked --offline --lib route_attempts -- --nocapture
rtk cargo test --locked --offline --test compatibility_semantics -- --nocapture
```

Expected: all incident patterns receive distinct stable classes, outer 5xx wins over nested codes, and terminal envelopes contain only safe aggregation fields.

- [ ] **Step 5: Commit classification and terminal aggregation**

```bash
rtk git add src/state.rs src/state/types.rs src/upstream_feedback.rs src/server/gateway/route_attempts.rs src/server/gateway/errors.rs src/server/gateway.rs tests/unit/upstream_feedback.rs tests/unit/server/gateway.rs tests/compatibility_semantics.rs
rtk git commit -m "feat(gateway): classify and aggregate route failures"
```

## Task 7: Add Bounded Route Health And Half-Open Leases

**Files:**
- Create: `src/state/route_health.rs`
- Modify: `src/state.rs:200`
- Modify: `src/state/types.rs`
- Test: `tests/unit/server/gateway.rs`

- [ ] **Step 1: Write deterministic health-state tests**

In the new module's test block, use `tokio::time::Instant` and paused time to assert:

```rust
fn key() -> KeyHealthKey {
    KeyHealthKey {
        upstream_id: "up-1".into(),
        key_fingerprint: "fingerprint-a".into(),
    }
}

fn route() -> RouteHealthKey {
    RouteHealthKey {
        upstream_id: "up-1".into(),
        key_fingerprint: "fingerprint-a".into(),
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    }
}

#[tokio::test(start_paused = true)]
async fn route_cooldown_has_one_half_open_lease_and_resets_after_success() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    registry.observe_route_failure(&route(), RouteFailureClass::TransientServer, None);
    assert!(matches!(registry.reserve(&route(), &key()), RouteAvailability::Cooling { .. }));

    tokio::time::advance(Duration::from_secs(12)).await;
    let lease = match registry.reserve(&route(), &key()) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route(), &key()),
        RouteAvailability::HalfOpenBusy { .. }
    ));
    registry.finish(lease, RouteOutcome::Success);
    assert!(matches!(
        registry.reserve(&route(), &key()),
        RouteAvailability::Ready(_)
    ));
}
```

Add named cases for exact route isolation, Key-wide credential isolation, 10-minute streak reset, 15-second capacity cooldown, 10-second generic 5xx cooldown, 15-minute credential cooldown, explicit Retry-After, increasing capped delays, deterministic 0.8-1.2 jitter, cancellation release, Key and route leases acquired atomically, aggregate non-blocking behavior, global 16384/per-upstream 4096 caps, and eviction that never removes an active lease. The Key half-open tests must assert: only the lease holder can clear Key state; credential/Key-quota failure advances its Key cooldown; route transport/5xx is uncertain, releases the Key lease without clearing or advancing its failure step, and records only the exact route failure; an ordinary request response clears a recoverable Key state only when it owns the Key lease.

- [ ] **Step 2: Run route-health tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline -p gateway-core --lib route_health -- --nocapture
```

Expected: the module and registry types do not exist.

- [ ] **Step 3: Implement the monotonic bounded registry**

Define exact health identities and keep them out of serde DTOs:

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeyHealthKey { pub upstream_id: String, pub key_fingerprint: String }

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RouteHealthKey {
    pub upstream_id: String,
    pub key_fingerprint: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RouteSetAggregateKey {
    pub upstream_id: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
}

#[derive(Debug)]
pub enum RouteAvailability<T> {
    Ready(T),
    Cooling { retry_after: Duration },
    HalfOpenBusy { retry_after: Duration },
}

#[derive(Debug)]
pub struct HealthLease {
    key_generation: Option<u64>,
    route_generation: Option<u64>,
    half_open: bool,
}

impl HealthLease {
    pub fn is_half_open(&self) -> bool {
        self.half_open
    }
}

pub struct RouteHealthPermit {
    registry: Arc<tokio::sync::Mutex<RouteHealthRegistry>>,
    lease: Option<HealthLease>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    Success,
    RouteFailure(RouteFailureClass),
    KeyFailure(RouteFailureClass),
    UncertainRouteFailure(RouteFailureClass),
    Cancelled,
}
```

Each state stores consecutive failures, last class/time, cooldown deadline, optional half-open generation, and last access. Use `tokio::time::Instant` for every deadline. Derive jitter from SHA-256 over a domain, the full health identity, and the failure step; map the first eight digest bytes into integer percent `80..=120`. Reset the step when the last failure is more than ten minutes old.

Use explicit policy constants so the action matrix cannot drift between call sites:

```rust
const TRANSIENT_ROUTE_BASE: Duration = Duration::from_secs(10);
const CAPACITY_ROUTE_BASE: Duration = Duration::from_secs(15);
const DEFAULT_RATE_LIMIT_BASE: Duration = Duration::from_secs(30);
const ROUTE_COOLDOWN_MAX: Duration = Duration::from_secs(5 * 60);
const CREDENTIAL_KEY_BASE: Duration = Duration::from_secs(15 * 60);
const KEY_COOLDOWN_MAX: Duration = Duration::from_secs(60 * 60);
const MODEL_QUARANTINE_BASE: Duration = Duration::from_secs(15 * 60);
const MODEL_QUARANTINE_MAX: Duration = Duration::from_secs(60 * 60);
const FAILURE_STREAK_RESET: Duration = Duration::from_secs(10 * 60);
const ROUTE_HEALTH_GLOBAL_CAPACITY: usize = 16_384;
const ROUTE_HEALTH_PER_UPSTREAM_CAPACITY: usize = 4_096;
```

Apply exponential steps plus deterministic jitter to locally derived cooldowns and cap them at the matching maximum. An upstream `Retry-After` is an explicit lower bound: preserve it without jitter and never shorten it to the generic five-minute cap.

Expose one pure, atomic `RouteHealthRegistry::reserve(route, key) -> RouteAvailability<HealthLease>` operation that checks both scopes and acquires every required half-open generation together. `AppState::reserve_route_health()` performs that mutation under its async lock and wraps a ready lease as `RouteHealthPermit`; `finish(outcome).await` takes the lease and records the approved recovery evidence, while `Drop` schedules release without punishment so downstream cancellation cannot strand half-open state. Add `AppState::observe_route_failure(route, class, retry_after)`, `observe_key_failure(key, class, retry_after)`, and `observe_route_set_failure(aggregate, class, retry_after)` wrappers for non-permit observations such as pre-existing quarantine and deterministic tests. No network future owns the registry lock. Aggregate observations rank upstreams but never make `reserve()` reject an otherwise healthy exact route. Update an aggregate only after at least one eligible route for that upstream was physically attempted and every eligible route for this request has ended in failure; routes filtered only by mapping, capability, or pre-existing cooldown do not create a new aggregate failure.

Add `route_health: Arc<tokio::sync::Mutex<RouteHealthRegistry>>` to every `AppState` constructor and explicitly re-export the route-health keys, availability, permit, and safe inspection types from `src/state.rs`. Add configuration-reconcile methods that retain only current Key/model/protocol identities and idle pruning that enforces both hard caps. At capacity, evict the least-recently-used entry whose cooldown has expired, then the least-recently-used inactive entry; never evict an active lease, and fail open without inserting when every eviction candidate is leased. Copy decisions/snapshots out of the lock before HTTP I/O, sleeps, capability queue submission, or configuration persistence.

- [ ] **Step 4: Run health-state tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline -p gateway-core --lib route_health -- --nocapture
rtk cargo test --locked --offline -p gateway-core --lib state -- --nocapture
```

Expected: all time tests complete instantly, only one half-open request is admitted, cancellations release leases, and unrelated routes remain healthy.

- [ ] **Step 5: Commit route health**

```bash
rtk git add src/state.rs src/state/types.rs src/state/route_health.rs tests/unit/server/gateway.rs
rtk git commit -m "feat(state): track bounded per-key route health"
```

## Task 8: Route Requests Through Virtual Deployments And Retry 5xx Once

**Files:**
- Modify: `src/server/gateway/route_attempts.rs`
- Modify: `src/server/gateway.rs:3478`
- Modify: `src/server/gateway/upstream.rs:620`
- Modify: `src/state.rs:1440`
- Test: `tests/gateway/chat/routing.rs`
- Test: `tests/gateway/responses/core.rs`
- Test: `tests/generic_dispatch.rs`

- [ ] **Step 1: Write failing virtual-route and same-route retry tests**

Reuse the Axum authorization capture already used by `downstream_chat_request_uses_key_mapped_to_requested_model` and add these named tests and terminal assertions:

```rust
#[tokio::test]
async fn authoritative_mapping_never_sends_glm_5_2_to_the_glm_4_7_key() {
    let result = run_multi_key_script(&[ok("key-b")], "glm-5.2").await;
    assert_eq!(result.status, StatusCode::OK);
    assert_eq!(result.authorizations, vec!["Bearer key-b"]);
}

#[tokio::test]
async fn generic_500_retries_the_same_route_once_and_success_clears_health() {
    let result = run_multi_key_script(&[server_error("key-b"), ok("key-b")], "glm-5.2").await;
    assert_eq!(result.status, StatusCode::OK);
    assert_eq!(result.authorizations, vec!["Bearer key-b", "Bearer key-b"]);
    assert_eq!(result.next_request_first_authorization, "Bearer key-b");
}

#[tokio::test]
async fn two_generic_500s_form_one_observation_before_the_next_key() {
    let result = run_multi_key_script(
        &[server_error("key-b"), server_error("key-b"), ok("key-c")],
        "glm-5.2",
    ).await;
    assert_eq!(result.status, StatusCode::OK);
    assert_eq!(
        result.authorizations,
        vec!["Bearer key-b", "Bearer key-b", "Bearer key-c"]
    );
    assert_eq!(result.key_b_failure_step, 1);
}
```

Implement `run_multi_key_script` beside the existing recording upstream helper using these fixture types: each handler asserts the Bearer header before popping the response.

```rust
struct ScriptedResponse {
    expected_key: &'static str,
    status: StatusCode,
    body: Value,
}

struct MultiKeyScriptResult {
    status: StatusCode,
    authorizations: Vec<String>,
    next_request_first_authorization: String,
    key_b_failure_step: u32,
}

fn ok(key: &'static str) -> ScriptedResponse {
    ScriptedResponse {
        expected_key: key,
        status: StatusCode::OK,
        body: json!({
            "id": "chatcmpl-ok",
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }),
    }
}

fn server_error(key: &'static str) -> ScriptedResponse {
    ScriptedResponse {
        expected_key: key,
        status: StatusCode::INTERNAL_SERVER_ERROR,
        body: json!({"error": {"code": "openai_error"}}),
    }
}
```

Add `authoritative_miss_makes_zero_upstream_requests`, `legacy_mode_tries_current_keys_in_order`, `attempted_set_does_not_repeat_a_route_in_a_later_pass`, and `every_physical_attempt_returns_upstream_in_flight_to_zero` with direct counter/in-flight assertions. Add `aggregate_requires_a_physical_attempt_and_never_hides_a_recovered_route`: pre-cool every route without issuing a request and assert aggregate failure steps do not change; then fail every physically attempted route once and assert one aggregate observation; finally clear one exact route and assert the next request uses it despite the older aggregate.

- [ ] **Step 2: Run gateway routing tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test gateway chat::routing::multi_key -- --nocapture
rtk cargo test --locked --offline --test gateway responses::core::multi_key -- --nocapture
rtk cargo test --locked --offline --test generic_dispatch -- --nocapture
```

Expected: capability filtering happens before Key identity, generic 5xx immediately rotates, or persisted upstream failure state changes.

- [ ] **Step 3: Build and execute virtual route candidates**

Define a candidate whose secret is private and whose log identity is safe:

```rust
pub struct VirtualRouteCandidate {
    pub upstream: UpstreamConfig,
    api_key: String,
    pub key_fingerprint: String,
    pub runtime_model_slug: String,
    pub protocol: UpstreamProtocol,
    pub route_id: String,
}
```

Build candidates in this order: existing upstream priority/quota/protocol eligibility; `keys_for_model(model)`; keyed capability eligibility; Key/route health availability; existing rotation/affinity. Cache the fingerprint/capability result on the candidate. Track `RouteHealthKey` in a request `HashSet` after the route is physically attempted. For each upstream, record an aggregate failure only when its eligible-route set had at least one physical attempt and every eligible route ended in failure; a healthy route immediately makes any older aggregate irrelevant.

Move upstream admission reservation inside the physical-attempt function so the initial call, same-route retry, existing dialect/context/stream recovery, hedge, and fallback each reserve independently. For generic 500/502/503/504, transport errors, and response-header timeout only, retry the same candidate once before any semantic output after deterministic 300-800ms backoff. Reuse the request-level idempotency identifier and derive the same upstream idempotency header on both attempts. If admission rejects the retry before HTTP dispatch, do not count that rejection as an upstream observation; finalize the initial upstream failure once and continue fallback under the existing local-admission semantics. Collapse an executed pair to one health observation: success clears state; two failures increment once. Existing request-shape correction attempts do not add a health failure for their intermediate ordinary 400 response.

Keep physical-attempt reservations in upstream request/concurrency accounting, including retry and hedge attempts. Do not append an extra downstream usage row or charge downstream quota per internal attempt; the existing single success/terminal usage-log path remains the downstream accounting boundary.

Stop reading `UpstreamConfig.failure_count` for candidate ranking and remove request-path calls to `mark_upstream_failure()`/`mark_upstream_success()`. Keep the serialized field for compatibility; set it to zero in `normalize_for_storage()` so the next successful administrator/background configuration write clears historical values without any request-path persistence.

- [ ] **Step 4: Run virtual routing tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test gateway chat::routing -- --nocapture
rtk cargo test --locked --offline --test gateway responses::core -- --nocapture
rtk cargo test --locked --offline --test generic_dispatch -- --nocapture
```

Expected: only mapped/capable Keys are called, a transient 5xx gets one same-route retry, two failed calls form one health observation, and no request failure persists configuration.

- [ ] **Step 5: Commit virtual deployment routing**

```bash
rtk git add src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/route_attempts.rs src/state.rs tests/gateway/chat/routing.rs tests/gateway/responses/core.rs tests/generic_dispatch.rs
rtk git commit -m "feat(gateway): route through key-model deployments"
```

## Task 9: Apply Key/Route Error Actions And Runtime Capability Hints

**Files:**
- Create: `src/capabilities/runtime_hints.rs`
- Modify: `src/capabilities/mod.rs`
- Modify: `src/state.rs`
- Modify: `src/server/gateway.rs:4390`
- Modify: `src/server/gateway/upstream.rs:1667`
- Modify: `src/server/gateway/capability_routing.rs`
- Modify: `src/server/gateway/route_attempts.rs`
- Test: `tests/gateway/chat/feedback.rs`
- Test: `tests/gateway/responses/upstream_feedback.rs`
- Test: `tests/gateway/capability_routing.rs`
- Test: `tests/unit/server/gateway.rs`

- [ ] **Step 1: Write failing action-matrix tests**

Script the exact actions with the Task 8 authorization-capture helper and assert both fallback and isolation scope:

```rust
#[tokio::test]
async fn no_available_channel_switches_immediately_and_cools_only_the_route() {
    let result = run_feedback_case(capacity_503_then_success()).await;
    assert_eq!(result.authorizations, vec!["Bearer key-a", "Bearer key-b"]);
    assert_eq!(result.route_a_cooling, true);
    assert_eq!(result.key_a_cooling, false);
}

#[tokio::test]
async fn credentials_cool_the_key_but_generic_429_cools_only_the_route() {
    let auth = run_feedback_case(credentials_401_then_success()).await;
    assert!(auth.key_a_cooling);
    assert!(auth.all_key_a_routes_filtered);

    let rate = run_feedback_case(rate_limit_429_then_success(73)).await;
    assert!(rate.route_a_cooling);
    assert!(!rate.key_a_cooling);
    assert_eq!(rate.retry_after_seconds, Some(73));
    assert!(rate.elapsed < Duration::from_secs(5));
}

#[tokio::test]
async fn model_and_feature_mismatches_have_different_actions() {
    let model = run_feedback_case(model_unsupported_then_success()).await;
    assert!(model.route_a_quarantined);
    assert_eq!(model.targeted_discovery_jobs, 1);

    let feature = run_feedback_case(xhigh_unsupported_then_success()).await;
    assert!(feature.xhigh_hint_active);
    assert!(!feature.route_a_cooling);
    assert_eq!(feature.plain_request_first_authorization, "Bearer key-a");
}

#[tokio::test]
async fn protocol_mismatch_tries_a_compatible_protocol_without_changing_models() {
    let result = run_feedback_case(responses_404_then_chat_success()).await;
    assert_eq!(result.protocols, vec!["responses", "chat_completions"]);
    assert!(result.protocol_hint_active);
    assert_eq!(result.persisted_models_before, result.persisted_models_after);
}

#[tokio::test]
async fn ordinary_request_errors_do_not_poll_or_punish_other_keys() {
    let result = run_feedback_case(request_400()).await;
    assert_eq!(result.status, StatusCode::BAD_REQUEST);
    assert_eq!(result.authorizations, vec!["Bearer key-a"]);
    assert!(!result.route_a_cooling && !result.key_a_cooling);
}
```

The feedback fixture returns explicit booleans/counters by inspecting runtime registries and the bounded targeted-discovery receiver; its upstream scripts use the literal 503 `no available channel`, 401, 429 `Retry-After`, 400 model, 400 xhigh, and ordinary 400 bodies from the classifier table.

Add terminal cases matching the approved matrix, including a cooled candidate with a 7-second earliest recovery which yields `503 upstream_routes_exhausted` and `Retry-After: 7`, and an occupied expired half-open lease which contributes one second.

- [ ] **Step 2: Run feedback/fallback tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test gateway chat::feedback -- --nocapture
rtk cargo test --locked --offline --test gateway responses::upstream_feedback -- --nocapture
rtk cargo test --locked --offline --test gateway capability_routing::negative_hint -- --nocapture
```

Expected: 429 sleeps on the same upstream, errors cool/persist the entire upstream, and feature/model/protocol mismatches are conflated.

- [ ] **Step 3: Implement exact error actions and negative hints**

Create a bounded registry keyed by profile plus capability discriminator:

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CapabilityHintKey {
    pub profile: DialectProfileKey,
    pub capability: Capability,
    pub value: Option<String>,
}

pub struct RuntimeCapabilityHints {
    entries: HashMap<CapabilityHintKey, tokio::time::Instant>,
    capacity: usize,
    ttl: Duration,
}
```

Default TTL is 15 minutes and capacity is bounded by the route-health cap with oldest-expiry eviction. Capability resolution overlays active negative hints after persisted evidence. The same feature/protocol success or a configuration fingerprint change removes its hint; a weaker request success does not remove xhigh. Queue a deduplicated independent capability probe on insertion; only probe completion may persist profile evidence. A conclusive supported or rejected probe clears the corresponding runtime hint because persistent evidence now owns the decision; an operational probe failure writes no capability conclusion and leaves the hint to expire naturally.

Call one `reconcile_runtime_route_state(&[UpstreamConfig])` hook after every successful upstream insert/update/delete. It prunes health, quarantine, hints, and pending probe bindings against current Key fingerprints/models/protocols without waiting for background sync; the hook performs no persistence and runs after the configuration transaction releases its locks.

Map each `FailureClass` to the approved scope and cooldown. Capacity switches immediately; generic 5xx follows Task 8; credentials affect `KeyHealthKey`; default 429 affects `RouteHealthKey` and clears a held recoverable Key half-open state; only structured Key-wide quota affects the Key; model mismatch clears older temporary route/Key health evidence, then quarantines the exact route for a 15-minute-to-1-hour stepped interval and requests targeted discovery; feature/protocol mismatch clears older temporary route health and any held recoverable Key half-open state, then writes only the runtime hint; request errors clear stale temporary route health but add no failure. A targeted discovery that still lists the model clears quarantine immediately; a failed discovery leaves it until cooldown expiry, when exactly one half-open request may revalidate it.

Delete the upstream-level 429 sleep/retry branch. Feed every attempted, cooled, quarantined, or explicitly capability/protocol-rejected mapped candidate into `AttemptLedger` with its stable class; continue while an unattempted healthy eligible route exists, and create the terminal error only after all candidates finish. If no configured/mapped candidate ever existed, do not call the ledger's non-empty terminal method; preserve the existing model/downstream-allowlist access error. Set downstream status from the ledger while retaining `upstream_status` as a separate numeric tracing field.

- [ ] **Step 4: Run action-matrix tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test gateway chat::feedback -- --nocapture
rtk cargo test --locked --offline --test gateway responses::upstream_feedback -- --nocapture
rtk cargo test --locked --offline --test gateway capability_routing -- --nocapture
rtk cargo test --locked --offline --lib route_attempts -- --nocapture
```

Expected: every failure affects only its designed scope, no upstream 429 sleep occurs, xhigh hints do not block weaker requests, and terminal status/category/retry-after follow the ledger.

- [ ] **Step 5: Commit exact feedback actions**

```bash
rtk git add src/capabilities/mod.rs src/capabilities/runtime_hints.rs src/state.rs src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/capability_routing.rs src/server/gateway/route_attempts.rs tests/gateway/chat/feedback.rs tests/gateway/responses/upstream_feedback.rs tests/gateway/capability_routing.rs tests/unit/server/gateway.rs
rtk git commit -m "feat(gateway): isolate route failures by scope"
```

## Task 10: Attribute Streaming And Hedged Attempts To Exact Routes

**Files:**
- Modify: `src/server/gateway.rs:1770`
- Modify: `src/server/gateway/stream.rs`
- Modify: `src/server/gateway/upstream.rs:620`
- Test: `tests/gateway/chat/streaming.rs`
- Test: `tests/gateway/responses/stream_lifecycle.rs`
- Test: `tests/gateway/responses/streaming.rs`
- Test: `tests/gateway/aggregate.rs`

- [ ] **Step 1: Write failing lifecycle attribution tests**

Add these named lifecycle tests and assert the exact route counters:

```rust
fn test_route_id(key: &str) -> String {
    anonymous_route_id(
        "up-1",
        &upstream_key_fingerprint("up-1", key),
        "glm-5.2",
        WireProtocol::Responses,
    )
}

#[tokio::test]
async fn pre_semantic_stream_failure_falls_back_with_exact_route_attribution() {
    let result = run_stream_script(pre_output_error_then_key_b_success()).await;
    assert_eq!(result.semantic_events, 1);
    assert_eq!(result.route_attempts, vec![test_route_id("key-a"), test_route_id("key-b")]);
    assert_eq!(result.failed_route_failure_step, 1);
}

#[tokio::test]
async fn post_output_failure_never_replays_to_another_route() {
    let result = run_stream_script(output_then_disconnect()).await;
    assert_eq!(result.route_attempts, vec![test_route_id("key-a")]);
    assert_eq!(result.downstream_replayed, false);
    assert_eq!(result.downstream_error_category, "stream_interrupted");
}

#[tokio::test]
async fn cancellation_and_hedge_loser_release_without_route_penalty() {
    let result = run_stream_script(cancel_primary_with_fast_hedge_winner()).await;
    assert_eq!(result.winner_route, test_route_id("key-b"));
    assert_eq!(result.loser_failure_step, 0);
    assert_eq!(result.downstream_cancelled_failure_steps, 0);
}

#[tokio::test]
async fn stream_to_json_recovery_does_not_repeat_the_same_virtual_route() {
    let result = run_stream_script(initial_sse_error_then_json_success()).await;
    assert_eq!(result.route_attempts, vec![test_route_id("key-a")]);
    assert_eq!(result.stream_to_json_recovery, true);
}
```

Assert route snapshots by anonymous route ID rather than upstream-only counters.

- [ ] **Step 2: Run streaming/hedging tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test gateway chat::streaming::route_health -- --nocapture
rtk cargo test --locked --offline --test gateway responses::stream_lifecycle -- --nocapture
rtk cargo test --locked --offline --test gateway aggregate::hedge -- --nocapture
```

Expected: `StreamCompletionContext` carries only `upstream_id`, canceled attempts can mark upstream failure, or hedge candidates have no exact identity.

- [ ] **Step 3: Carry route identity through every lifecycle**

Change stream completion context to own the exact identity and permit:

```rust
struct StreamCompletionContext {
    state: AppState,
    route: RouteHealthKey,
    route_id: String,
    permit: Option<RouteHealthPermit>,
    semantic_output_started: bool,
}
```

Before first semantic output, classified failure returns to the existing fallback loop and finishes the permit with that route outcome. Treat the existing stream-to-JSON recovery as one composite route observation: each HTTP dispatch reserves admission, a JSON recovery success clears the route, and only the final composite failure advances health once. Share one generic route-replay budget: when a streaming 5xx qualifies for stream-to-JSON recovery, that JSON dispatch is the single allowed same-route retry rather than an additional third attempt. Mark the route in the request attempted set before recovery so later candidate passes cannot select it again. After semantic output, never replay; record only an attributable upstream interruption. Treat downstream body drop as cancellation and drop/release the permit without punishment.

Make hedge candidates carry `RouteHealthKey` plus anonymous `route_id` separately from their raw Key. Exclude the primary route and already-attempted routes when scheduling hedges. The winner finishes success; aborted loser futures drop permits as cancellations. Preserve one upstream admission reservation per hedge HTTP request. Add `run_stream_script()` fixtures to the existing streaming harness; each fixture returns the named counters used above so the tests inspect route health rather than upstream-wide `failure_count`.

- [ ] **Step 4: Run lifecycle tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test gateway chat::streaming -- --nocapture
rtk cargo test --locked --offline --test gateway responses::stream_lifecycle -- --nocapture
rtk cargo test --locked --offline --test gateway responses::streaming -- --nocapture
rtk cargo test --locked --offline --test gateway aggregate -- --nocapture
```

Expected: only attributable upstream outcomes change exact route health; client cancellation and hedge loser cancellation do not.

- [ ] **Step 5: Commit streaming and hedge attribution**

```bash
rtk git add src/server/gateway.rs src/server/gateway/stream.rs src/server/gateway/upstream.rs tests/gateway/chat/streaming.rs tests/gateway/responses/stream_lifecycle.rs tests/gateway/responses/streaming.rs tests/gateway/aggregate.rs
rtk git commit -m "fix(stream): attribute outcomes to exact key routes"
```

## Task 11: Keep Model And Reasoning Catalogs Persisted And Stable

**Files:**
- Modify: `src/state.rs:2402`
- Modify: `src/server/gateway/capability_routing.rs:543`
- Modify: `src/server/gateway.rs:1372`
- Modify: `src/server/portal.rs`
- Modify: `src/server/gateway/troubleshooting.rs`
- Test: `tests/admin_models.rs`
- Test: `tests/portal_api.rs`
- Test: `tests/gateway/capability_routing.rs`
- Test: `tests/gateway/chat/feedback.rs`

- [ ] **Step 1: Write failing stable-catalog tests**

Add a mock `/v1/models` counter and assert:

```rust
let before = get_models(&app, &downstream_secret).await;
assert_eq!(mock_models_requests.load(Ordering::SeqCst), 0);

state
    .observe_route_failure(&route_a, RouteFailureClass::CapacityUnavailable, None)
    .await;
state
    .observe_route_failure(&route_b, RouteFailureClass::CapacityUnavailable, None)
    .await;
let during_cooldown = get_models(&app, &downstream_secret).await;
assert_eq!(during_cooldown, before);
assert_eq!(mock_models_requests.load(Ordering::SeqCst), 0);
```

Add an empty persisted catalog case which returns no models without live discovery. Add a Codex xhigh witness case: only a verified keyed profile may advertise xhigh; runtime cooldown does not remove it; requesting xhigh while all verified routes cool returns 503 rather than selecting an unverified Key.

- [ ] **Step 2: Run catalog tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test admin_models persisted_catalog -- --nocapture
rtk cargo test --locked --offline --test portal_api persisted_catalog -- --nocapture
rtk cargo test --locked --offline --test gateway capability_routing::catalog -- --nocapture
```

Expected: an empty catalog synchronously calls the upstream endpoint, or catalog witnesses are upstream-scoped rather than Key-scoped.

- [ ] **Step 3: Remove request-path discovery and use keyed persistent witnesses**

Make `available_models_for_downstream()` a pure persisted snapshot query:

```rust
for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
    for model in upstream.route_models() {
        if downstream.model_allowlist.is_empty()
            || portal_model_is_allowed(&downstream.model_allowlist, &model)
        {
            models.insert(model);
        }
    }
}
```

Delete the `fetch_models_from_endpoint()` branch from model, portal, and troubleshooting request paths. Catalog witness selection iterates persisted keyed profiles for current mapped Keys and never reads route health, quarantine, aggregate, or runtime negative hints. When xhigh is advertised, request routing requires that same verified keyed evidence; temporary unavailability yields the normal 503 terminal error.

- [ ] **Step 4: Run stable-catalog tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test admin_models -- --nocapture
rtk cargo test --locked --offline --test portal_api -- --nocapture
rtk cargo test --locked --offline --test gateway capability_routing -- --nocapture
rtk cargo test --locked --offline --test gateway chat::feedback -- --nocapture
```

Expected: catalog endpoints perform no upstream I/O, transient health never changes advertised models/reasoning metadata, and xhigh does not downgrade to an unverified route.

- [ ] **Step 5: Commit the stable persisted catalog**

```bash
rtk git add src/state.rs src/server/gateway.rs src/server/gateway/capability_routing.rs src/server/gateway/troubleshooting.rs src/server/portal.rs tests/admin_models.rs tests/portal_api.rs tests/gateway/capability_routing.rs tests/gateway/chat/feedback.rs
rtk git commit -m "fix(catalog): serve models from persisted capability data"
```

## Task 12: Restore Safe Background And Targeted Model Discovery

**Files:**
- Create: `src/state/model_key_sync.rs`
- Create: `tests/model_key_sync.rs`
- Modify: `src/state.rs`
- Modify: `src/state/model_discovery.rs`
- Modify: `src/state/types.rs:37`
- Modify: `src/main.rs:105`
- Test: `tests/templates.rs`

- [ ] **Step 1: Write failing synchronization and scheduler tests**

Use the historical `8498483:src/state/model_key_sync.rs` only as a discovery/helper reference and add these fresh tests with exact input/output assertions:

| Test name | Input | Required assertion |
|---|---|---|
| `authoritative_sync_replaces_success_and_preserves_failure` | Key A discovers `new-a`; Key B returns 503 with existing `old-b` | mappings equal `A:[new-a], B:[old-b]`; union is `[new-a, old-b]` |
| `new_failed_key_is_saved_as_an_empty_authoritative_mapping` | existing A succeeds; newly configured B times out | B remains configured with `[]` and is absent from `keys_for_model()` |
| `all_failed_sync_is_byte_for_byte_noop` | every Key fails | serialized config and `last_synced_at` equal the pre-sync bytes/value |
| `legacy_partial_success_does_not_switch_modes` | one of two legacy Keys succeeds | `api_key_models.is_empty()` remains true |
| `legacy_complete_success_switches_atomically` | both legacy Keys return non-empty models | two authoritative records appear in current-Key order |
| `any_snapshot_change_discards_the_whole_pass` | mutate base URL, ordered Keys, protocols, mapping, catalog, then fingerprint in separate subcases | every subcase leaves the newer config untouched |
| `deleted_replaced_or_reordered_keys_never_reappear` | mutate Keys while HTTP futures are pending | final mappings contain only write-time current Keys |
| `addition_is_immediate_but_removal_needs_two_observations` | first discovery adds `new`, omits `old`; second omission occurs after 60 seconds | `new` appears after pass one; `old` remains after pass one and disappears after pass two |
| `uncertain_discovery_never_confirms_removal` | timeout, 500, parse error, then empty list | missing count stays zero and `old` remains |
| `targeted_queue_is_deduplicated_and_bounded` | submit the same fingerprint repeatedly, then exceed capacity with distinct fingerprints | one duplicate job is received and distinct submissions stop at configured capacity |
| `zero_interval_disables_and_nonzero_interval_jitters_startup` | intervals `0` and `900` under paused time | zero performs no probe; nonzero first probes only after its deterministic 30-90 second deadline |

Use Tokio paused time for the two 15-minute cycles and the 60-second missing-model interval; do not use wall-clock sleeps.

- [ ] **Step 2: Run sync tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test model_key_sync -- --nocapture
rtk cargo test --locked --offline --test templates model_key_sync -- --nocapture
```

Expected: the coordinator does not exist and `main.rs` clamps `0` to `1`.

- [ ] **Step 3: Implement exact-snapshot discovery and writeback**

Define immutable snapshot identity and in-memory deletion evidence:

```rust
struct ModelKeySyncSnapshot {
    upstream_id: String,
    base_url: String,
    ordered_current_keys: Vec<(String, String)>,
    protocols: Vec<UpstreamProtocol>,
    api_key_models: Vec<ApiKeyModelConfig>,
    supported_models: Vec<String>,
    configuration_fingerprint: String,
}

struct MissingObservation {
    count: u8,
    last_successful_missing_at: tokio::time::Instant,
    configuration_fingerprint: String,
}

const TARGETED_DISCOVERY_QUEUE_CAPACITY: usize = 256;
```

The periodic loop waits a deterministic 30-90 second startup jitter, adds deterministic per-upstream jitter, and probes through a discovery-only global semaphore initialized with `MODEL_DISCOVERY_MAX_CONCURRENCY`. Discovery never calls `try_reserve_upstream_request()` and therefore cannot consume inference concurrency/request quota. Targeted submissions use a `TARGETED_DISCOVERY_QUEUE_CAPACITY` channel and an in-memory set keyed by `(upstream_id, key_fingerprint)`; removing a job from the set is guaranteed on success, error, or cancellation.

Before writeback, reload the upstream and recompute every snapshot field. If any differs, discard the whole pass. Rebuild output only from the write-time current Key list: successful Keys supply discovered models, failed existing Keys copy their current mapping, failed new Keys get empty mappings, and no historical Key is iterated. Derive the aggregate union. If every Key fails, skip persistence and `last_synced_at`.

For legacy mode, persist nothing unless every current Key has a non-empty successful result. For authoritative mode, add discovered models immediately; remove a model only after two successful missing observations for the same snapshot at least 60 seconds apart. A rediscovery clears its missing record. Administrator saves and explicit successful non-empty discovery bypass the two-cycle rule.

Keep missing observations only in memory and initialize the map empty on restart, deliberately requiring two new confirmations after every process start. Key removal itself is not a model-removal observation: configuration reconciliation immediately deletes that Key's mapping, profile, quarantine, hints, and health state.

For targeted mismatch discovery, apply confirmation to only `(upstream_id, key_fingerprint, model)`. Two confirmed misses remove the model from that Key's mapping; if another current Key still maps the model, the recomputed `supported_models` union and downstream catalog remain unchanged.

Expose `ModelKeySyncService::spawn(state)` and call it from `main.rs` only when `upstream_model_key_sync_interval_seconds > 0`; remove `.max(1)` from this environment value. Model mismatch calls the targeted submission method, and successful rediscovery containing the model clears its quarantine.

- [ ] **Step 4: Run sync and long-time tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test model_key_sync -- --nocapture
rtk cargo test --locked --offline --test templates model_key_sync -- --nocapture
rtk cargo test --locked --offline --test multi_key_mapping -- --nocapture
```

Expected: no stale task can revive a deleted Key, legacy mode changes only after complete discovery, deletion needs two valid observations, and interval zero is a true kill switch.

- [ ] **Step 5: Commit safe model synchronization**

```bash
rtk git add src/state.rs src/state/model_key_sync.rs src/state/model_discovery.rs src/state/types.rs src/main.rs tests/model_key_sync.rs tests/templates.rs
rtk git commit -m "feat(state): safely refresh per-key model mappings"
```

## Task 13: Add Safe Route Observability And Secret-Leak Regressions

**Files:**
- Modify: `src/state/route_health.rs`
- Modify: `src/state/types.rs`
- Modify: `src/server/admin.rs:430`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/upstream.rs`
- Modify: `src/server/gateway/capability_admin.rs`
- Test: `tests/admin_upstreams.rs`
- Test: `tests/admin_capabilities.rs`
- Test: `tests/admin_logs.rs`
- Test: `tests/capability_probe.rs`
- Test: `tests/gateway/chat/feedback.rs`
- Test: `tests/gateway/responses/upstream_feedback.rs`

- [ ] **Step 1: Write failing safe-observability tests**

Seed two routes with secrets, one healthy and one cooling, then assert the admin/runtime response contains only aggregate and anonymous fields:

```rust
assert_eq!(payload["route_health"]["healthy_routes"], 1);
assert_eq!(payload["route_health"]["cooldown_routes"], 1);
assert!(payload["route_health"]["earliest_retry_after_seconds"].is_number());
assert!(payload["route_health"]["failure_classes"]["capacity_unavailable"].is_number());

let serialized = payload.to_string();
assert!(!serialized.contains("upstream-secret"));
assert!(!serialized.contains(&full_key_fingerprint));
assert!(!serialized.contains("key_prefix"));
assert!(!serialized.contains("prompt-secret"));
assert!(!serialized.contains("tool-argument-secret"));
assert!(!serialized.contains("raw-provider-error-secret"));
```

Capture gateway tracing for the 500 -> 503 incident and assert structured fields contain separate numeric `upstream_status=500` and `downstream_status=503`, an anonymous `route_id`, failure class, action, cooldown seconds, and remaining candidates. Assert usage logs store only the stable terminal category and safe summary. Exercise capability queue submission/completion with a known fingerprint and assert ordinary probe tracing and warning output contain neither the fingerprint nor the raw Key.

- [ ] **Step 2: Run observability tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test admin_upstreams route_health -- --nocapture
rtk cargo test --locked --offline --test admin_capabilities redaction -- --nocapture
rtk cargo test --locked --offline --test admin_logs route_failure -- --nocapture
```

Expected: admin runtime is upstream-only, gateway logs use `selected_upstream_key_prefix`, or full keyed profile identities can appear in exported DTOs.

- [ ] **Step 3: Expose only safe aggregates and route IDs**

Add a serialization-only DTO:

```rust
#[derive(Clone, Debug, Serialize)]
pub struct RouteHealthSnapshotDto {
    pub healthy_routes: usize,
    pub cooldown_routes: usize,
    pub half_open_routes: usize,
    pub earliest_retry_after_seconds: Option<u64>,
    pub failure_classes: BTreeMap<String, usize>,
}
```

Build the current enumerable route universe from authoritative mappings and configured protocols, then overlay a copied registry snapshot: routes with no health entry count as healthy; active cooldown/half-open entries count in their respective buckets. Legacy arbitrary-model routes are included only after they have a bounded runtime entry, so the admin count cannot create unbounded identities. Release the registry lock before response construction and do not serialize internal Key/route map keys. Replace every upstream Key-prefix trace field with `route_id`; add `upstream_status`, final `downstream_status`, `failure_class`, `route_action`, `same_route_retry`, `cooldown_seconds`, and `remaining_candidates`. Keep raw bodies only inside the classifier call and continue using existing safe error-summary boundaries.

Make capability/admin exports map keyed profiles to safe DTOs; include anonymous `route_id` when an individual route is needed and never serialize `DialectProfileKey.key_fingerprint` directly. Audit capability probe/state tracing so keyed structs are never formatted with `Debug`; log upstream ID plus anonymous route ID instead.

- [ ] **Step 4: Run security and observability tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test admin_upstreams route_health -- --nocapture
rtk cargo test --locked --offline --test admin_capabilities -- --nocapture
rtk cargo test --locked --offline --test admin_logs -- --nocapture
rtk cargo test --locked --offline --test capability_probe redaction -- --nocapture
rtk cargo test --locked --offline --test gateway chat::feedback -- --nocapture
rtk cargo test --locked --offline --test gateway responses::upstream_feedback -- --nocapture
```

Expected: operators receive route counts/actions/statuses without any raw Key, fingerprint, request content, or provider body.

- [ ] **Step 5: Commit safe observability**

```bash
rtk git add src/state/route_health.rs src/state/types.rs src/server/admin.rs src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/capability_admin.rs src/server/gateway/capability_probe.rs tests/admin_upstreams.rs tests/admin_capabilities.rs tests/admin_logs.rs tests/capability_probe.rs tests/gateway/chat/feedback.rs tests/gateway/responses/upstream_feedback.rs
rtk git commit -m "feat(admin): expose safe route health diagnostics"
```

## Task 14: Document Deployment Semantics And Run The Complete Verification Matrix

**Files:**
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `README.md`
- Modify: `DEPLOYMENT.md`
- Modify: `tests/templates.rs`
- Modify: `tests/docker.rs`

- [ ] **Step 1: Write failing deployment-template assertions**

Assert all checked-in deployment surfaces state:

```text
UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=900
0 disables background model-key synchronization
UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS is deprecated for real upstream 429 responses
UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS is deprecated for route-health Retry-After
exact route health is process-local and requires one active gateway instance
```

Also assert the docs explain: authoritative empty mappings, persisted `/v1/models`, generic 5xx same-route retry once, no in-request 429 sleep, 503 route exhaustion, and 502 credential/model exhaustion.

- [ ] **Step 2: Run template tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test templates -- --nocapture
rtk cargo test --locked --offline --test docker -- --nocapture
```

Expected: the sync variable is still described as deprecated, interval zero is undocumented, and the single-active-instance health boundary is absent.

- [ ] **Step 3: Update deployment and operator documentation**

Activate the sync variable with default `900` and documented kill switch `0`. Mark the two upstream rate-limit retry controls as parsed only for compatibility; real upstream 429 now switches route and preserves the full `Retry-After`. Document that automatic replay reuses one request idempotency identifier but remains at-least-once for providers that do not honor an idempotency header, so a retry can duplicate inference or provider-side storage. Add an upgrade note: deployments whose persisted `supported_models` is empty must run one successful explicit discovery or allow a complete background legacy discovery before `/v1/models` advertises those models. Document that runtime health resets on restart, is never part of the model catalog, and is unsupported with multiple active gateway replicas sharing a database.

Include an operator table mapping the stable client outcomes:

```text
503 upstream_routes_exhausted       temporary/cooling routes; retry using Retry-After
502 upstream_credentials_exhausted  every eligible Key has credential/billing failure
502 upstream_model_unsupported      every attempted route rejected the model
400 capability_not_supported        no route can preserve an explicitly required feature
502 upstream_protocol_unsupported   no route supports the endpoint/protocol
```

- [ ] **Step 4: Run format, full backend, frontend, and deterministic regression tests**

Run from the repository root:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --locked --offline --workspace
rtk cargo test --locked --offline --test model_key_sync -- --nocapture
rtk cargo test --locked --offline --test load -- --nocapture
```

Run from `frontend/`:

```bash
rtk npm test
rtk npm run build
```

Expected: every command exits zero. The long-time tests use paused Tokio time, replay transient 503 followed by success, and prove the persisted model is never removed.

- [ ] **Step 5: Inspect the final diff for secrets and unintended persistence**

Run:

```bash
rtk rg -n "selected_upstream_key_prefix|key_prefix" src/server src/state frontend/src
rtk rg -n "mark_upstream_failure|mark_upstream_success" src/server/gateway.rs src/server/gateway
rtk rg -n "fetch_models_from_endpoint" src/server src/state.rs
rtk git diff --check
rtk git status --short
```

Expected: the first three searches return no request-path or public DTO matches, `git diff --check` is silent, and status lists only files belonging to this implementation.

- [ ] **Step 6: Commit documentation and verification coverage**

```bash
rtk git add .env.example docker-compose.yml README.md DEPLOYMENT.md tests/templates.rs tests/docker.rs
rtk git commit -m "docs: describe multi-key route resilience"
```
