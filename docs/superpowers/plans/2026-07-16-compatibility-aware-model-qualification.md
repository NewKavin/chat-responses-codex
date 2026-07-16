# Compatibility-Aware Model Qualification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Discover and exercise every active upstream route, retain full or safely adapted models, remove only conclusively unusable routes, and atomically update the `test` downstream without ever erasing the last known-good model set on transient failures.

**Architecture:** Add a focused qualification module beside model discovery. Direct probes produce sanitized operational evidence; existing capability profiles determine full versus adapted compatibility. A single `AppState` mutation applies per-key mappings and the `test` downstream allowlist atomically after zero-result and final-route guards pass.

**Tech Stack:** Rust 2021, Axum 0.8, Tokio, reqwest, Serde, futures-util, Vue 3, TypeScript, Element Plus, Vitest.

---

## File Map

- Create `src/state/model_qualification.rs`: qualification types, meaningful-output parsing, sanitized categories, direct route probes, and pure apply decisions.
- Modify `src/state.rs`: module export, candidate orchestration, capability-level classification, and atomic application.
- Modify `src/server/admin.rs`: authenticated qualification handler.
- Modify `src/server/gateway.rs`: route registration.
- Modify `tests/admin_upstreams.rs`: mock discovery/probe/apply/guard/redaction coverage.
- Modify `tests/capability_profiles.rs`: full/adapted/operational classification coverage.
- Modify `frontend/src/types/index.ts`: qualification request/result types.
- Modify `frontend/src/api/admin.ts`: qualification API method.
- Modify `frontend/src/views/admin/ModelProbe.vue`: confirmation, action, and sanitized summary.
- Modify `frontend/tests/api/admin.spec.ts`: API contract.

### Task 1: Define Sanitized Qualification Evidence And Classification

**Files:**
- Create: `src/state/model_qualification.rs`
- Modify: `src/state.rs`
- Test: `tests/capability_profiles.rs`

- [ ] **Step 1: Write failing classification tests**

Add imports and tests to `tests/capability_profiles.rs`:

```rust
use chat_responses_codex::state::{
    classify_qualification_level, ModelQualificationCategory, ModelQualificationLevel,
};

#[test]
fn direct_success_with_complete_agent_profile_is_full() {
    let mut profile = verified_profile("up", "opaque", WireProtocol::ChatCompletions);
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::FunctionTools,
        Capability::ToolContinuation,
    ] {
        profile.capabilities.insert(capability, EvidenceState::Supported);
    }
    assert_eq!(
        classify_qualification_level(ModelQualificationCategory::Passed, Some(&profile)),
        ModelQualificationLevel::Full,
    );
}

#[test]
fn usable_text_with_partial_profile_is_adapted_not_unusable() {
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        upstream_id: "up".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Partial;
    profile.capabilities.insert(Capability::TextInput, EvidenceState::Supported);
    assert_eq!(
        classify_qualification_level(ModelQualificationCategory::Passed, Some(&profile)),
        ModelQualificationLevel::Adapted,
    );
}

#[test]
fn transient_failures_never_classify_as_unusable() {
    for category in [
        ModelQualificationCategory::Authentication,
        ModelQualificationCategory::RateLimit,
        ModelQualificationCategory::UpstreamUnavailable,
        ModelQualificationCategory::Timeout,
        ModelQualificationCategory::Network,
    ] {
        assert_eq!(
            classify_qualification_level(category, None),
            ModelQualificationLevel::OperationalFailure,
        );
    }
}
```

Use an existing test helper or add a local `verified_profile` helper that fills
`configuration_fingerprint`, state, and exact key without model-name logic.

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `rtk cargo test --test capability_profiles -- --nocapture`

Expected: FAIL because the qualification module and exports do not exist.

- [ ] **Step 3: Create the public evidence vocabulary**

Create `src/state/model_qualification.rs`:

```rust
use crate::capabilities::{
    Capability, DialectProfileState, EvidenceState, UpstreamDialectProfile,
};
use crate::routing::UpstreamProtocol;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelQualificationLevel {
    Full,
    Adapted,
    Unusable,
    OperationalFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelQualificationCategory {
    Passed,
    Authentication,
    RateLimit,
    UpstreamUnavailable,
    RequestRejected,
    ModelNotFound,
    MalformedResponse,
    EmptyResponse,
    Timeout,
    Network,
}

impl ModelQualificationCategory {
    pub fn is_operational(self) -> bool {
        matches!(self, Self::Authentication | Self::RateLimit
            | Self::UpstreamUnavailable | Self::Timeout | Self::Network)
    }

    pub fn requires_confirmation(self) -> bool {
        matches!(self, Self::RequestRejected | Self::ModelNotFound
            | Self::MalformedResponse | Self::EmptyResponse)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelQualificationEvidence {
    pub upstream_id: String,
    pub key_prefix: String,
    pub model: String,
    pub protocol: UpstreamProtocol,
    pub level: ModelQualificationLevel,
    pub category: ModelQualificationCategory,
    pub latency_ms: u64,
    pub attempted_at: u64,
}

pub fn classify_qualification_level(
    category: ModelQualificationCategory,
    profile: Option<&UpstreamDialectProfile>,
) -> ModelQualificationLevel {
    if category.is_operational() {
        return ModelQualificationLevel::OperationalFailure;
    }
    if category != ModelQualificationCategory::Passed {
        return ModelQualificationLevel::Unusable;
    }
    let full = profile.is_some_and(|profile| {
        profile.state == DialectProfileState::Verified
            && [Capability::TextInput, Capability::TextStream,
                Capability::FunctionTools, Capability::ToolContinuation]
                .into_iter().all(|capability|
                    profile.capabilities.get(&capability) == Some(&EvidenceState::Supported))
    });
    if full { ModelQualificationLevel::Full } else { ModelQualificationLevel::Adapted }
}
```

Declare the module in `src/state.rs` with `#[path = "state/model_qualification.rs"] mod model_qualification;` and re-export the public types/function. Remove the unused `WireProtocol` import if Clippy reports it.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `rtk cargo test --test capability_profiles -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit the qualification domain**

```bash
rtk git add src/state.rs src/state/model_qualification.rs tests/capability_profiles.rs
rtk git commit -m "feat(upstreams): define compatibility qualification levels"
```

### Task 2: Probe Exact Chat And Responses Routes Without Leaking Secrets

**Files:**
- Modify: `src/state/model_qualification.rs`
- Test: `tests/admin_upstreams.rs`

- [ ] **Step 1: Add mock probe cases before implementation**

Extend the existing Axum mock helpers in `tests/admin_upstreams.rs` with routes
that return meaningful Chat text, Responses output text, empty output, malformed
JSON, 401, 429, 404 model-not-found, and 503. Add:

```rust
#[tokio::test]
async fn qualification_probe_accepts_meaningful_chat_and_responses_output() {
    let mock = spawn_qualification_upstream().await;
    let client = reqwest::Client::new();

    let chat = qualify_model_on_upstream(&client, &mock, "secret-key", "chat-ok",
        UpstreamProtocol::ChatCompletions, 2).await;
    assert_eq!(chat.category, ModelQualificationCategory::Passed);

    let responses = qualify_model_on_upstream(&client, &mock, "secret-key", "responses-ok",
        UpstreamProtocol::Responses, 2).await;
    assert_eq!(responses.category, ModelQualificationCategory::Passed);
}

#[tokio::test]
async fn qualification_probe_returns_sanitized_categories() {
    let mock = spawn_qualification_upstream().await;
    let client = reqwest::Client::new();
    for (model, expected) in [
        ("empty", ModelQualificationCategory::EmptyResponse),
        ("malformed", ModelQualificationCategory::MalformedResponse),
        ("unauthorized", ModelQualificationCategory::Authentication),
        ("limited", ModelQualificationCategory::RateLimit),
        ("missing", ModelQualificationCategory::ModelNotFound),
        ("unavailable", ModelQualificationCategory::UpstreamUnavailable),
    ] {
        let result = qualify_model_on_upstream(&client, &mock, "secret-key", model,
            UpstreamProtocol::ChatCompletions, 2).await;
        assert_eq!(result.category, expected);
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("secret-key"));
        assert!(!serialized.contains(&mock));
    }
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run: `rtk cargo test --test admin_upstreams qualification_probe -- --nocapture`

Expected: FAIL because `qualify_model_on_upstream` does not exist.

- [ ] **Step 3: Implement bounded direct probing**

Add a sanitized result carrying only category and latency, plus the complete
bounded request implementation:

```rust
#[derive(Clone, Debug, Serialize)]
pub struct DirectQualificationResult {
    pub category: ModelQualificationCategory,
    pub latency_ms: u64,
}

pub async fn qualify_model_on_upstream(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    protocol: UpstreamProtocol,
    timeout_seconds: u64,
) -> DirectQualificationResult {
    let started = std::time::Instant::now();
    let endpoint = match protocol {
        UpstreamProtocol::ChatCompletions => "/v1/chat/completions",
        UpstreamProtocol::Responses => "/v1/responses",
    };
    let body = match protocol {
        UpstreamProtocol::ChatCompletions => serde_json::json!({
            "model": model,
            "messages": [{"role":"user","content":"Reply with one short word."}],
            "stream": false
        }),
        UpstreamProtocol::Responses => serde_json::json!({
            "model": model,
            "input": "Reply with one short word.",
            "stream": false
        }),
    };
    let url = crate::util::join_upstream_url(base_url, endpoint);
    let response = client.post(url).bearer_auth(api_key)
        .timeout(std::time::Duration::from_secs(timeout_seconds.max(1)))
        .json(&body).send().await;
    let response = match response {
        Ok(response) => response,
        Err(error) => return DirectQualificationResult {
            category: if error.is_timeout() {
                ModelQualificationCategory::Timeout
            } else {
                ModelQualificationCategory::Network
            },
            latency_ms: started.elapsed().as_millis().max(1) as u64,
        },
    };
    let status = response.status();
    let bytes = match response.bytes().await {
        Ok(bytes) if bytes.len() <= 1_048_576 => bytes,
        Ok(_) | Err(_) => return DirectQualificationResult {
            category: ModelQualificationCategory::MalformedResponse,
            latency_ms: started.elapsed().as_millis().max(1) as u64,
        },
    };
    let parsed = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let error_code = parsed.as_ref()
        .and_then(|value| value.pointer("/error/code"))
        .and_then(serde_json::Value::as_str);
    let category = if matches!(status.as_u16(), 401 | 403) {
        ModelQualificationCategory::Authentication
    } else if status.as_u16() == 429 {
        ModelQualificationCategory::RateLimit
    } else if status.is_server_error() {
        ModelQualificationCategory::UpstreamUnavailable
    } else if status.as_u16() == 404 || error_code == Some("model_not_found") {
        ModelQualificationCategory::ModelNotFound
    } else if !status.is_success() {
        ModelQualificationCategory::RequestRejected
    } else if let Some(payload) = parsed {
        if meaningful_output(protocol, &payload) {
            ModelQualificationCategory::Passed
        } else {
            ModelQualificationCategory::EmptyResponse
        }
    } else {
        ModelQualificationCategory::MalformedResponse
    };
    DirectQualificationResult {
        category,
        latency_ms: started.elapsed().as_millis().max(1) as u64,
    }
}

fn non_empty(value: Option<&serde_json::Value>) -> bool {
    value.and_then(serde_json::Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

fn meaningful_output(protocol: UpstreamProtocol, value: &serde_json::Value) -> bool {
    match protocol {
        UpstreamProtocol::ChatCompletions => value.get("choices")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|choices| choices.iter().any(|choice| {
                let message = choice.get("message");
                non_empty(message.and_then(|item| item.get("content")))
                    || non_empty(message.and_then(|item| item.get("reasoning_content")))
                    || message.and_then(|item| item.get("tool_calls"))
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|calls| calls.iter().any(|call| {
                            non_empty(call.get("id"))
                                && non_empty(call.pointer("/function/name"))
                        }))
            })),
        UpstreamProtocol::Responses => non_empty(value.get("output_text"))
            || value.get("output").and_then(serde_json::Value::as_array)
                .is_some_and(|items| items.iter().any(|item| {
                    matches!(item.get("type").and_then(serde_json::Value::as_str),
                        Some("function_call" | "custom_tool_call"))
                        && (non_empty(item.get("call_id")) || non_empty(item.get("name")))
                        || item.get("content").and_then(serde_json::Value::as_array)
                            .is_some_and(|parts| parts.iter().any(|part| {
                                non_empty(part.get("text"))
                                    || non_empty(part.get("reasoning_text"))
                            }))
                })),
    }
}
```

Extend the `pub use model_qualification::{...};` list in `src/state.rs` with
`qualify_model_on_upstream` and `DirectQualificationResult` so integration tests
exercise the production helper.

Do not include URL, raw body, prompt, output, or key in the result. The 1 MiB
bound applies before parsing so an upstream cannot turn qualification into an
unbounded admin response allocation.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `rtk cargo test --test admin_upstreams qualification_probe -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit direct probes**

```bash
rtk git add src/state/model_qualification.rs tests/admin_upstreams.rs
rtk git commit -m "feat(upstreams): probe runnable models by exact protocol"
```

### Task 3: Build Per-Key Decisions With Confirmation And Last-Known-Good Retention

**Files:**
- Modify: `src/state/model_qualification.rs`
- Modify: `src/state.rs`
- Test: `tests/admin_upstreams.rs`

- [ ] **Step 1: Write failing decision-builder tests**

Add pure tests for these invariants:

```rust
#[test]
fn decision_keeps_success_and_prior_models_after_operational_failure() {
    let previous = BTreeSet::from(["known-good".to_string()]);
    let observations = vec![
        observation("new-good", ModelQualificationLevel::Adapted),
        observation("known-good", ModelQualificationLevel::OperationalFailure),
    ];
    let decision = build_key_qualification_decision(previous, observations);
    assert_eq!(decision.retained, BTreeSet::from([
        "known-good".to_string(), "new-good".to_string()
    ]));
}

#[test]
fn conclusive_failure_requires_two_matching_attempts_before_removal() {
    assert_eq!(confirmed_level(&[
        ModelQualificationCategory::EmptyResponse,
        ModelQualificationCategory::Passed,
    ]), ModelQualificationLevel::Adapted);
    assert_eq!(confirmed_level(&[
        ModelQualificationCategory::EmptyResponse,
        ModelQualificationCategory::EmptyResponse,
    ]), ModelQualificationLevel::Unusable);
}
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk cargo test --test admin_upstreams qualification_decision -- --nocapture`

Expected: FAIL because decision helpers do not exist.

- [ ] **Step 3: Implement internal decision types and confirmation**

Add non-serializable internal types:

```rust
#[derive(Clone, Debug)]
pub struct KeyQualificationDecision {
    pub api_key: String,
    pub retained: BTreeSet<String>,
    pub full: BTreeSet<String>,
    pub adapted: BTreeSet<String>,
    pub removed: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct UpstreamQualificationDecision {
    pub upstream_id: String,
    pub keys: Vec<KeyQualificationDecision>,
}
```

For each key, candidate models are the union of discovered models, its existing
`api_key_models` entry, and existing aggregate route models when no per-key map
exists. Probe supported protocols with `buffer_unordered(4)`. Re-run only
conclusive failure categories once; two matching conclusive failures remove the
tuple. Any pass retains it. Any operational failure retains previous evidence
but does not add a never-verified model.

- [ ] **Step 4: Resolve full versus adapted from exact profiles**

In `AppState::qualify_active_upstreams`, load one capability snapshot. For a
successful tuple, build the exact `DialectProfileKey`, find its profile, and call
`classify_qualification_level`. Do not inspect slug/provider/hostname text.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run: `rtk cargo test --test admin_upstreams qualification_decision -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit orchestration**

```bash
rtk git add src/state.rs src/state/model_qualification.rs tests/admin_upstreams.rs
rtk git commit -m "feat(upstreams): retain last-known-good qualified routes"
```

### Task 4: Apply Upstream Mappings And The Test Allowlist Atomically

**Files:**
- Modify: `src/state.rs`
- Test: `tests/admin_upstreams.rs`

- [ ] **Step 1: Write atomic application tests**

Add tests using a failing `StateStore` and a normal temporary state:

```rust
#[tokio::test]
async fn qualification_apply_updates_upstreams_and_test_downstream_together() {
    let state = qualification_state().await;
    let summary = state.apply_model_qualification(decisions(), "test").await.unwrap();
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].api_key_models[0].supported_models,
        vec!["adapted", "full"]);
    assert_eq!(snapshot.downstreams.iter().find(|d| d.id == "test").unwrap().model_allowlist,
        vec!["adapted", "full"]);
    assert_eq!(summary.retained_models, 2);
}

#[tokio::test]
async fn qualification_apply_refuses_to_erase_the_last_model() {
    let state = qualification_state().await;
    let error = state.apply_model_qualification(all_removed_decisions(), "test")
        .await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!state.snapshot().await.upstreams[0].route_models().is_empty());
}

#[tokio::test]
async fn persistence_failure_leaves_runtime_and_downstream_unchanged() {
    let state = qualification_state_with_failing_store().await;
    let before = state.snapshot().await;
    assert!(state.apply_model_qualification(decisions(), "test").await.is_err());
    assert_eq!(state.snapshot().await, before);
}
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk cargo test --test admin_upstreams qualification_apply -- --nocapture`

Expected: FAIL because the atomic method is missing.

- [ ] **Step 3: Implement one persisted-state mutation**

Add `AppState::apply_model_qualification`. Inside one
`mutate_persisted_state_io` closure:

```rust
for decision in &decisions {
    let upstream = state.upstreams.iter_mut()
        .find(|value| value.id == decision.upstream_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "upstream disappeared"))?;
    upstream.api_key_models = decision.keys.iter().map(|key| ApiKeyModelConfig {
        api_key: key.api_key.clone(),
        supported_models: key.retained.iter().cloned().collect(),
    }).collect();
    let retained = decision.keys.iter().flat_map(|key| key.retained.iter().cloned())
        .collect::<BTreeSet<_>>();
    upstream.premium_models.retain(|model| retained.contains(model));
    upstream.supported_models = retained.into_iter()
        .filter(|model| !upstream.premium_models.contains(model)).collect();
    upstream.normalize_for_storage();
}
let exposed = state.upstreams.iter().filter(|upstream| upstream.active)
    .flat_map(UpstreamConfig::route_models).collect::<BTreeSet<_>>();
if exposed.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidInput,
        "qualification would remove the final routable model"));
}
let downstream = state.downstreams.iter_mut().find(|value| value.id == downstream_id)
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "downstream not found"))?;
downstream.model_allowlist = exposed.into_iter().collect();
```

Return a sanitized apply summary. Raw keys stay internal and never implement
`Serialize`. Because persistence occurs before the shared state swap in the
existing mutation helper, persistence failure leaves runtime unchanged.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `rtk cargo test --test admin_upstreams qualification_apply -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit atomic application**

```bash
rtk git add src/state.rs tests/admin_upstreams.rs
rtk git commit -m "feat(upstreams): apply qualified model maps atomically"
```

### Task 5: Expose The Authenticated Qualification API

**Files:**
- Modify: `src/server/admin.rs`
- Modify: `src/server/gateway.rs`
- Test: `tests/admin_upstreams.rs`

- [ ] **Step 1: Write route, selection, and redaction tests**

Add:

```rust
#[tokio::test]
async fn qualify_models_admin_can_select_upstreams_without_applying() {
    let (app, state) = qualification_app().await;
    let response = admin_post(&app, "/api/admin/upstreams/qualify-models", json!({
        "apply": false,
        "upstream_ids": ["qualified-upstream"],
        "downstream_id": "test",
        "excluded_models": []
    })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(body["summary"]["retained_models"].as_u64().unwrap() > 0);
    assert!(!body.to_string().contains("secret-key"));
    assert_eq!(state.snapshot().await.upstreams[0].supported_models, vec!["old"]);
}

#[tokio::test]
async fn qualify_models_apply_is_admin_authenticated() {
    let app = qualification_app().await.0;
    let response = app.oneshot(Request::builder()
        .method(Method::POST).uri("/api/admin/upstreams/qualify-models")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}" )).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk cargo test --test admin_upstreams qualify_models -- --nocapture`

Expected: FAIL with 404.

- [ ] **Step 3: Implement request/response and handler**

In `src/server/admin.rs` add:

```rust
#[derive(serde::Deserialize)]
#[serde(default)]
struct QualifyModelsPayload {
    apply: bool,
    upstream_ids: Vec<String>,
    downstream_id: String,
    excluded_models: Vec<String>,
}

impl Default for QualifyModelsPayload {
    fn default() -> Self {
        Self {
            apply: false,
            upstream_ids: Vec::new(),
            downstream_id: "test".into(),
            excluded_models: Vec::new(),
        }
    }
}
```

The handler calls `state.qualify_active_upstreams(&payload.upstream_ids)`, then
removes exact case-sensitive slugs listed in `excluded_models` from every
decision before optionally calling `apply_model_qualification`. This field is
reserved for version-verified installed-client basic-text failures; it is not
populated by tool-only or infrastructure failures. The final-model guard still
applies. Return only evidence and counts grouped by upstream; no key, URL,
prompt, output, or raw error body.

Register `POST /api/admin/upstreams/qualify-models` with
`admin_auth_middleware`, adjacent to `discover-models`.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `rtk cargo test --test admin_upstreams qualify_models -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit the API**

```bash
rtk git add src/server/admin.rs src/server/gateway.rs tests/admin_upstreams.rs
rtk git commit -m "feat(admin): qualify and apply compatible upstream models"
```

### Task 6: Add The Admin Confirmation And Sanitized Result UI

**Files:**
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/views/admin/ModelProbe.vue`
- Test: `frontend/tests/api/admin.spec.ts`

- [ ] **Step 1: Write the API client contract test**

Add:

```ts
it('qualifies live upstream models with explicit apply intent', async () => {
  postSpy.mockResolvedValue({ data: { summary: { retained_models: 3 } } })
  await adminApi.qualifyUpstreamModels({
    apply: true,
    upstream_ids: [],
    downstream_id: 'test',
    excluded_models: []
  })
  expect(postSpy).toHaveBeenCalledWith('/admin/upstreams/qualify-models', {
    apply: true,
    upstream_ids: [],
    downstream_id: 'test',
    excluded_models: []
  })
})
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk npm --prefix frontend exec vitest run tests/api/admin.spec.ts`

Expected: FAIL because the method is missing.

- [ ] **Step 3: Add exact TypeScript types and API method**

Define snake-case union types matching Rust and a response containing only
sanitized evidence/counts. Add:

```ts
qualifyUpstreamModels: (data: QualifyModelsRequest) =>
  adminHttp.post<QualifyModelsResponse>('/admin/upstreams/qualify-models', data)
```

- [ ] **Step 4: Add a confirmed action to ModelProbe**

Add a `真实验证并应用` button. Before sending, show an Element Plus confirmation
that external inference calls will be made and the `test` allowlist may shrink.
Render retained/full/adapted/removed/operational counts and per-upstream model
slugs/categories only. Never render key prefixes in the browser UI even though
the API evidence is sanitized.

- [ ] **Step 5: Run frontend tests and build**

Run: `rtk npm --prefix frontend exec vitest run tests/api/admin.spec.ts`

Expected: PASS.

Run: `rtk npm --prefix frontend run build`

Expected: PASS.

- [ ] **Step 6: Commit the admin UI**

```bash
rtk git add frontend/src frontend/tests/api/admin.spec.ts
rtk git commit -m "feat(admin): expose safe live model qualification"
```

### Task 7: Qualification Regression Gate

**Files:**
- No source changes expected.

- [ ] **Step 1: Run qualification and capability suites**

Run: `rtk cargo test --test admin_upstreams qualify --test capability_profiles qualification --test capability_state -- --nocapture`

Expected: all selected tests pass.

- [ ] **Step 2: Run broader upstream/state tests**

Run: `rtk cargo test --test admin_upstreams --test state_store --test postgres_roundtrip -- --nocapture`

Expected: PASS; PostgreSQL tests use their existing configured/skip behavior.

- [ ] **Step 3: Run frontend tests and build**

Run: `rtk npm --prefix frontend exec vitest run`

Expected: all frontend tests pass.

Run: `rtk npm --prefix frontend run build`

Expected: exit 0.

- [ ] **Step 4: Verify clean checkpoint**

Run: `rtk git status --short`

Expected: no uncommitted files from this plan.
