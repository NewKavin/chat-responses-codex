# Generic Agent Protocol Fidelity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the gateway a model-agnostic, capability-driven protocol adapter that preserves Codex, OpenCode, Claude Code, and Hermes agent loops across third-party Chat Completions and Responses upstreams, with truthful degradation and measured latency.

**Architecture:** Keep the existing pairwise protocol converters and add a small shared capability domain in `gateway-core`. Persist capability policy and exact-route dialect profiles outside the main `PersistedState`, publish an immutable in-memory snapshot for request-time routing, and build focused image, tool, and reasoning adapters only after a route is selected. Live probes and compatibility expectations are diagnostic evidence; neither runs on the normal request path nor grants production capabilities.

**Tech Stack:** Rust 2021, Axum 0.8, Tokio, Serde/serde_json, ArcSwap, SHA-256/HMAC, PostgreSQL, Vue 3, TypeScript, Vitest, Bash/jq, local mock upstreams.

---

## Locked Boundaries

Capability persistence is a focused store, not another field on `PersistedState`:

- File deployments use `<main-state-file>.capabilities.json`, written with temporary-file rename under a dedicated async lock.
- PostgreSQL deployments use a singleton `capability_configuration` table and a keyed `dialect_profiles` table.
- `StateStore` receives default capability methods so existing test stores remain source-compatible.
- `AppState` owns `Arc<ArcSwap<CapabilityRuntimeSnapshot>>`; request routing performs no filesystem, database, or probe I/O.
- A candidate configuration is compiled and validated before persistence or snapshot swap. Failure retains the last valid snapshot.

Production dispatch must not inspect a model slug, provider label, or hostname. GLM, DeepSeek, MiniMax, Kimi, and the selected Qwen VLM appear only in importable deployment data and live acceptance output.

## File Map

**Capability domain**

- Create `src/capabilities/mod.rs`: public exports and schema/probe version constants.
- Create `src/capabilities/types.rs`: serializable policy, profile, expectation, feature, and resolution types.
- Create `src/capabilities/policy.rs`: schema validation, selector compilation, conflict detection, import/export digest.
- Create `src/capabilities/resolver.rs`: conservative baselines and precedence-based request-time resolution.
- Create `src/capabilities/profile.rs`: exact-route fingerprints, staleness, and profile invalidation.
- Create `src/capabilities/probe_queue.rs`: bounded, deduplicated scheduling state.
- Modify `crates/gateway-core/src/lib.rs` and `src/lib.rs`: expose the shared capability module.
- Modify `crates/gateway-core/Cargo.toml`: add `arc-swap` and `globset`.

**Persistence and runtime state**

- Modify `src/state/store.rs`: capability load/config/profile persistence methods.
- Modify `src/state/file_store.rs`: atomic sidecar operations.
- Modify `src/state/postgres.rs`: capability tables and keyed round trips.
- Modify `src/state.rs`: immutable capability snapshot, atomic reload, profile updates, and continuation state.
- Modify `src/state/types.rs` and `src/main.rs`: probe timing/concurrency configuration only; do not add capability data to `PersistedState`.

**Gateway behavior**

- Create `src/server/gateway/capability_probe.rs`: generic protocol probe runner and lifecycle.
- Create `src/server/gateway/capability_routing.rs`: feature extraction, candidate eligibility, and catalog witness selection.
- Create `src/server/gateway/dialect_retry.rs`: one bounded pre-stream correction retry.
- Create `src/protocol/image_adapter.rs`: Responses/Chat/Messages image mapping.
- Create `src/protocol/tool_adapter.rs`: namespace/custom registry and hosted-tool policy.
- Create `src/protocol/reasoning_adapter.rs`: Responses/Chat reasoning items and replay carrier.
- Modify `src/protocol.rs`: call focused adapters from existing pairwise JSON/SSE paths.
- Modify `src/server/gateway.rs`, `src/server/gateway/upstream.rs`, `src/server/gateway/stream.rs`, and `src/server/gateway/compat.rs`: capability selection, diagnostics, and removal of slug/hostname classifiers.
- Modify `src/server/gateway/claude.rs`: Messages images, adaptive thinking, signed replay, and official SSE ordering.

**Diagnostics, UI, and acceptance**

- Create `src/server/gateway/compatibility_semantics.rs`: JSON/SSE semantic validators.
- Modify `src/server/gateway/troubleshooting.rs`: four-client dynamic matrix and expectation assertions.
- Modify `frontend/src/types/index.ts`, `frontend/src/api/admin.ts`, `frontend/src/utils/integration.ts`, `frontend/src/utils/troubleshooting.ts`, `frontend/src/components/CompatibilityMatrixPanel.vue`, `frontend/src/components/TroubleshootingCenter.vue`, and `frontend/src/views/portal/Integration.vue`: capability surfaces and truthful presets.
- Create sanitized fixtures under `tests/fixtures/clients/` and semantic/profile/probe integration tests under `tests/`.
- Create `templates/capabilities/current-deployment.example.json`: replaceable deployment policy and expectations.
- Modify `scripts/compatibility_matrix.sh`, `tests/load.rs`, `README.md`, `DEPLOYMENT.md`, `docs/codex-integration-guide.md`, and client templates.

### Task 0: Capture The Pre-Change Streaming Baseline

**Files:**
- Modify: `tests/load.rs`
- Create (generated): `docs/verification/2026-07-10-agent-protocol-baseline.json`

- [ ] **Step 1: Add an ignored direct-versus-gateway first-event benchmark**

Before changing production code, add `load_gateway_first_meaningful_event_baseline` beside the existing ignored load test. Reuse the current gateway API and a local mock that sends one SSE data frame, waits 40 ms, then sends its terminal frame. Run direct and gateway rounds with 100 requests and concurrency 20.

```rust
const INLINE_IMAGE_BASELINE: &str = concat!(
    "data:image/png;base64,",
    "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAMElEQVR42mP4T2PAMGoB",
    "aRYwMFAHjVowasGoBaMWjFowasGoBaMWDHULRpuOA2EBAHmBeOr2sW6XAAAAAElFTkSuQmCC"
);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FirstEventBaseline {
    revision: String,
    direct_p50_ms: u64,
    direct_p95_ms: u64,
    gateway_p50_ms: u64,
    gateway_p95_ms: u64,
    gateway_added_p95_ms: i64,
    image_direct_p95_ms: u64,
    image_gateway_p95_ms: u64,
    image_gateway_added_p95_ms: i64,
    direct_requests: usize,
    gateway_requests: usize,
}

fn percentile(values: &mut [u64], percentile: usize) -> u64 {
    values.sort_unstable();
    values[(values.len() * percentile / 100).min(values.len() - 1)]
}
```

Read each response stream only until the first non-empty, non-comment SSE `data:` frame. Repeat both rounds with `INLINE_IMAGE_BASELINE`, still before production changes. Assert all four rounds complete 100 requests, record 200 direct plus 200 gateway requests, and make exactly 400 mock requests. Print one JSON `FirstEventBaseline`; do not add the final 50 ms gate yet.

Populate `revision` from required environment variable `PROTOCOL_BASELINE_REVISION` and fail the ignored test when it is absent.

When `PROTOCOL_BASELINE_OUTPUT` is set, serialize the same record to that path:

```rust
if let Ok(path) = std::env::var("PROTOCOL_BASELINE_OUTPUT") {
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_json::to_vec_pretty(&baseline).unwrap()).unwrap();
}
```

- [ ] **Step 2: Run the baseline on the approved design revision**

Run: `rtk env PROTOCOL_BASELINE_REVISION=a17cfe6 PROTOCOL_BASELINE_OUTPUT=docs/verification/2026-07-10-agent-protocol-baseline.json cargo test --release --test load load_gateway_first_meaningful_event_baseline -- --ignored --nocapture`

Expected: PASS and one JSON record for revision `a17cfe6`, the approved production-design baseline before implementation changes.

- [ ] **Step 3: Record the sanitized baseline**

Validate the generated evidence:

Run: `rtk jq -e '.revision == "a17cfe6" and .direct_requests == 200 and .gateway_requests == 200 and (.gateway_added_p95_ms | type == "number") and (.image_gateway_added_p95_ms | type == "number")' docs/verification/2026-07-10-agent-protocol-baseline.json`

Expected: prints `true`. The generated JSON contains only revision, aggregate latency values, and attempt counts.

- [ ] **Step 4: Commit the baseline harness and evidence**

```bash
rtk git add tests/load.rs docs/verification/2026-07-10-agent-protocol-baseline.json
rtk git commit -m "test: capture gateway streaming latency baseline"
```

### Task 1: Define And Validate The Generic Capability Contract

**Files:**
- Modify: `crates/gateway-core/Cargo.toml`
- Modify: `crates/gateway-core/src/lib.rs`
- Modify: `src/lib.rs`
- Create: `src/capabilities/mod.rs`
- Create: `src/capabilities/types.rs`
- Create: `src/capabilities/policy.rs`
- Create: `tests/capability_policy.rs`

- [ ] **Step 1: Write failing schema and selector tests**

Create `tests/capability_policy.rs` with synthetic names that prove the compiler is not classifying known vendors:

```rust
use chat_responses_codex::capabilities::{
    AgentClientProfile, Capability, CapabilityConfiguration, CapabilityPolicy,
    CapabilitySelector, CompatibilityBundle, CompatibilityExpectation,
    SemanticPolicy, WireProtocol,
};
use std::collections::BTreeSet;

fn policy(id: &str, runtime_model_glob: &str, context_window: u64) -> CapabilityPolicy {
    CapabilityPolicy {
        id: id.into(),
        priority: 10,
        selector: CapabilitySelector {
            runtime_model_glob: Some(runtime_model_glob.into()),
            protocol: Some(WireProtocol::ChatCompletions),
            ..CapabilitySelector::default()
        },
        semantic: SemanticPolicy {
            context_window: Some(context_window),
            ..SemanticPolicy::default()
        },
        evidence: Vec::new(),
        probe_candidates: Default::default(),
        extension_probes: Vec::new(),
    }
}

#[test]
fn arbitrary_slug_uses_external_selector_without_recompilation() {
    let mut config = CapabilityConfiguration::default();
    config.policies.push(policy("synthetic", "lab/*", 131_072));
    let compiled = config.compile().unwrap();
    let route = chat_responses_codex::capabilities::RouteIdentity {
        upstream_id: "up-random".into(),
        exposed_model_slug: "public-alias".into(),
        runtime_model_slug: "lab/model-that-did-not-exist-at-build-time".into(),
        protocol: WireProtocol::ChatCompletions,
        tags: BTreeSet::new(),
    };
    assert_eq!(compiled.semantic_for(&route).context_window, Some(131_072));
}

#[test]
fn administrator_route_tags_feed_policy_selection() {
    let mut config = CapabilityConfiguration::default();
    config.route_tags.push(chat_responses_codex::capabilities::RouteTagAssignment {
        id: "vision-tag".into(),
        selector: CapabilitySelector { upstream_id: Some("up-random".into()),
            runtime_model_glob: Some("lab/*".into()), ..CapabilitySelector::default() },
        tags: BTreeSet::from(["primary_vision".into()]),
    });
    let mut tagged = policy("tagged-policy", "lab/*", 65_536);
    tagged.selector = CapabilitySelector { tag: Some("primary_vision".into()),
        ..CapabilitySelector::default() };
    config.policies.push(tagged);
    let compiled = config.compile().unwrap();
    let mut route = chat_responses_codex::capabilities::RouteIdentity {
        upstream_id: "up-random".into(), exposed_model_slug: "public".into(),
        runtime_model_slug: "lab/new-model".into(),
        protocol: WireProtocol::ChatCompletions, tags: BTreeSet::new(),
    };
    compiled.apply_route_tags(&mut route);
    assert!(route.tags.contains("primary_vision"));
    assert_eq!(compiled.semantic_for(&route).context_window, Some(65_536));
}

#[test]
fn equal_priority_equal_specificity_conflict_rejects_bundle() {
    let mut config = CapabilityConfiguration::default();
    config.policies.push(policy("left", "lab/*", 32_000));
    config.policies.push(policy("right", "lab/*", 64_000));
    let error = config.compile().err().unwrap().to_string();
    assert!(error.contains("ambiguous semantic field context_window"));
}

#[test]
fn expectation_bundle_is_diagnostic_data() {
    let mut config = CapabilityConfiguration::default();
    config.bundles.push(CompatibilityBundle {
        id: "agent_core".into(),
        required: BTreeSet::from([Capability::FunctionTools]),
    });
    config.compatibility_expectations.push(CompatibilityExpectation {
        id: "acceptance-only".into(),
        selector: CapabilitySelector {
            exposed_model: Some("public-alias".into()),
            ..CapabilitySelector::default()
        },
        bundles: vec!["agent_core".into()],
        client_profiles: vec![AgentClientProfile::Codex],
        permitted_optional_downgrades: BTreeSet::new(),
        https_image_fixture: None,
    });
    let compiled = config.compile().unwrap();
    assert!(compiled.expectations()[0].required.contains(&Capability::FunctionTools));
    assert!(compiled.route_overrides().is_empty());
}

#[test]
fn protected_extension_paths_are_rejected() {
    let mut config: CapabilityConfiguration = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "policies": [{
            "id": "bad-extension",
            "priority": 1,
            "selector": {},
            "semantic": {},
            "probe_candidates": {},
            "extension_probes": [{
                "id": "rewrite-model",
                "protocol": "chat_completions",
                "request_patch": {"model": "forbidden"},
                "response_predicate": {"path": "/choices/0/message/content", "operator": "exists"}
            }]
        }]
    })).unwrap();
    let error = config.compile().err().unwrap().to_string();
    assert!(error.contains("protected request path /model"));
}
```

- [ ] **Step 2: Run the tests and confirm the module is missing**

Run: `rtk cargo test --test capability_policy -- --nocapture`

Expected: FAIL with `could not find capabilities in chat_responses_codex`.

- [ ] **Step 3: Add dependencies and module exports**

Add these exact dependencies to `crates/gateway-core/Cargo.toml`:

```toml
arc-swap = "1.7"
globset = "0.4"
```

Add this module declaration to `crates/gateway-core/src/lib.rs`:

```rust
#[path = "../../../src/capabilities/mod.rs"]
pub mod capabilities;
```

Add this re-export to `src/lib.rs`:

```rust
pub use gateway_core::capabilities;
```

- [ ] **Step 4: Implement the serializable schema**

Create `src/capabilities/mod.rs`:

```rust
mod policy;
mod types;

pub use policy::{CapabilityPolicyError, CompiledCapabilityConfiguration};
pub use types::*;

pub const CAPABILITY_SCHEMA_VERSION: u32 = 1;
pub const DIALECT_PROBE_SCHEMA_VERSION: u32 = 1;
```

Create `src/capabilities/types.rs` with this public vocabulary. Keep all maps ordered so policy digests and namespace mappings are deterministic:

```rust
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol { ChatCompletions, Responses, Messages }

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    TextInput,
    ImageHttps,
    ImageDataUrl,
    ImageDetail,
    NativeFileId,
    FunctionTools,
    NamespaceTools,
    CustomTools,
    HostedTools,
    ParallelToolCalls,
    ForcedToolChoice,
    ToolContinuation,
    ReasoningOutput,
    ReasoningReplay,
    TextStream,
    ReasoningStream,
    IndexedToolArgumentStream,
    UsageStream,
    StructuredOutput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState { Supported, Rejected, Unobserved }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource { Override, Probe, Policy, Baseline }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenLimitField { MaxTokens, MaxCompletionTokens, MaxOutputTokens, Omit }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode { Off, Optional, FixedOn }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningCarrier {
    None,
    ReasoningContent,
    ResponsesReasoningItem,
    MessagesThinking,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackStage { Native, ProtocolAdapted, HistoryReplayed, HistoryReduced }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DialectCorrectionRule {
    SwitchTokenLimit { rejected: TokenLimitField, replacement: TokenLimitField },
    RemoveOptionalField { field: String },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentClientProfile { Codex, Opencode, ClaudeCode, Hermes }

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilitySelector {
    pub exposed_model: Option<String>,
    pub runtime_model: Option<String>,
    pub runtime_model_glob: Option<String>,
    pub upstream_id: Option<String>,
    pub protocol: Option<WireProtocol>,
    pub tag: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticPolicy {
    pub reasoning_mode: Option<ReasoningMode>,
    pub reasoning_replay_required: Option<bool>,
    pub effort_map: BTreeMap<String, String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub omit_sampling_fields: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProbeCandidates {
    pub token_limit_fields: Vec<TokenLimitField>,
    pub reasoning_controls: BTreeMap<String, Vec<String>>,
    pub reasoning_carriers: Vec<ReasoningCarrier>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateOperator { Exists, Equals, Contains, EventSequence }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsePredicate {
    pub path: String,
    pub operator: PredicateOperator,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarativeProbeCase {
    pub id: String,
    pub protocol: WireProtocol,
    #[serde(default)]
    pub prerequisites: BTreeSet<Capability>,
    pub request_patch: serde_json::Value,
    pub response_predicate: ResponsePredicate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    pub title: String,
    pub url: String,
    pub retrieved_at: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpsImageFixture {
    pub url: String,
    pub expected_label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicy {
    pub id: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub selector: CapabilitySelector,
    #[serde(default)]
    pub semantic: SemanticPolicy,
    #[serde(default)]
    pub evidence: Vec<EvidenceReference>,
    #[serde(default)]
    pub probe_candidates: ProbeCandidates,
    #[serde(default)]
    pub extension_probes: Vec<DeclarativeProbeCase>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RouteCapabilityOverride {
    pub id: String,
    pub priority: i32,
    pub selector: CapabilitySelector,
    pub capabilities: BTreeMap<Capability, EvidenceState>,
    pub token_limit_field: Option<TokenLimitField>,
    pub reasoning_carrier: Option<ReasoningCarrier>,
    pub correction_rules: Vec<DialectCorrectionRule>,
    pub extensions: BTreeMap<String, EvidenceState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteTagAssignment {
    pub id: String,
    pub selector: CapabilitySelector,
    pub tags: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityBundle {
    pub id: String,
    pub required: BTreeSet<Capability>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityExpectation {
    pub id: String,
    pub selector: CapabilitySelector,
    pub bundles: Vec<String>,
    pub client_profiles: Vec<AgentClientProfile>,
    #[serde(default)]
    pub permitted_optional_downgrades: BTreeSet<String>,
    #[serde(default)]
    pub https_image_fixture: Option<HttpsImageFixture>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProbeConfiguration {
    pub enabled: bool,
    pub refresh_interval_seconds: u64,
    pub max_global_concurrency: usize,
    pub max_per_upstream_concurrency: usize,
    pub output_token_cap: u32,
    pub https_image_fixture: Option<HttpsImageFixture>,
}

impl Default for ProbeConfiguration {
    fn default() -> Self {
        Self {
            enabled: true,
            refresh_interval_seconds: 7 * 24 * 60 * 60,
            max_global_concurrency: 2,
            max_per_upstream_concurrency: 1,
            output_token_cap: 64,
            https_image_fixture: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilityConfiguration {
    pub schema_version: u32,
    pub revision: u64,
    pub policies: Vec<CapabilityPolicy>,
    pub route_overrides: Vec<RouteCapabilityOverride>,
    pub route_tags: Vec<RouteTagAssignment>,
    pub bundles: Vec<CompatibilityBundle>,
    pub compatibility_expectations: Vec<CompatibilityExpectation>,
    pub probe: ProbeConfiguration,
}

impl Default for CapabilityConfiguration {
    fn default() -> Self {
        Self {
            schema_version: super::CAPABILITY_SCHEMA_VERSION,
            revision: 0,
            policies: Vec::new(),
            route_overrides: Vec::new(),
            route_tags: Vec::new(),
            bundles: Vec::new(),
            compatibility_expectations: Vec::new(),
            probe: ProbeConfiguration::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteIdentity {
    pub upstream_id: String,
    pub exposed_model_slug: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
    pub tags: BTreeSet<String>,
}
```

- [ ] **Step 5: Implement strict policy compilation and deterministic matching**

Create `src/capabilities/policy.rs`. The implementation must compile globs once, reject duplicate IDs and missing bundle references, forbid patches to `/model`, `/messages`, `/input`, `/tools`, `/stream`, `/headers`, `/url`, and media-bearing paths, and select by `(priority, specificity)`:

```rust
use super::types::*;
use globset::{Glob, GlobMatcher};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum CapabilityPolicyError {
    #[error("unsupported capability schema version {0}")]
    SchemaVersion(u32),
    #[error("duplicate capability id {0}")]
    DuplicateId(String),
    #[error("unknown compatibility bundle {0}")]
    UnknownBundle(String),
    #[error("invalid selector glob {0}")]
    InvalidGlob(String),
    #[error("ambiguous semantic field {field} between {left} and {right}")]
    Ambiguous { field: &'static str, left: String, right: String },
    #[error("protected request path {0}")]
    ProtectedPath(String),
    #[error("HTTPS image fixture requires an https URL and non-empty expected label")]
    InvalidImageFixture,
    #[error("invalid bounded response predicate path {0}")]
    InvalidPredicate(String),
    #[error("declarative probe case {0} exceeds 16384 bytes")]
    ExtensionTooLarge(String),
    #[error("route tag assignment {0} cannot select by tag or assign an empty tag set")]
    InvalidTagAssignment(String),
}

#[derive(Clone)]
struct CompiledSelector {
    source: CapabilitySelector,
    runtime_glob: Option<GlobMatcher>,
    specificity: u8,
}

impl CompiledSelector {
    fn new(source: CapabilitySelector) -> Result<Self, CapabilityPolicyError> {
        let runtime_glob = source.runtime_model_glob.as_ref().map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|_| CapabilityPolicyError::InvalidGlob(pattern.clone()))
        }).transpose()?;
        let exact = [source.exposed_model.is_some(), source.runtime_model.is_some(),
            source.upstream_id.is_some(), source.protocol.is_some(), source.tag.is_some()]
            .into_iter().filter(|value| *value).count() as u8;
        let specificity = exact.saturating_mul(2) + u8::from(runtime_glob.is_some());
        Ok(Self { source, runtime_glob, specificity })
    }

    fn matches(&self, route: &RouteIdentity) -> bool {
        self.source.exposed_model.as_deref().map(|v| v == route.exposed_model_slug).unwrap_or(true)
            && self.source.runtime_model.as_deref().map(|v| v == route.runtime_model_slug).unwrap_or(true)
            && self.source.upstream_id.as_deref().map(|v| v == route.upstream_id).unwrap_or(true)
            && self.source.protocol.map(|v| v == route.protocol).unwrap_or(true)
            && self.source.tag.as_ref().map(|v| route.tags.contains(v)).unwrap_or(true)
            && self.runtime_glob.as_ref().map(|v| v.is_match(&route.runtime_model_slug)).unwrap_or(true)
    }
}

#[derive(Clone)]
struct CompiledPolicy { value: CapabilityPolicy, selector: CompiledSelector }

#[derive(Clone)]
struct CompiledOverride { value: RouteCapabilityOverride, selector: CompiledSelector }

#[derive(Clone)]
struct CompiledTagAssignment { value: RouteTagAssignment, selector: CompiledSelector }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledExpectation {
    pub id: String,
    pub required: BTreeSet<Capability>,
    pub client_profiles: Vec<AgentClientProfile>,
    pub permitted_optional_downgrades: BTreeSet<String>,
    pub https_image_fixture: Option<HttpsImageFixture>,
    pub selector: CapabilitySelector,
}

#[derive(Clone)]
pub struct CompiledCapabilityConfiguration {
    source: CapabilityConfiguration,
    digest: String,
    policies: Vec<CompiledPolicy>,
    overrides: Vec<CompiledOverride>,
    tag_assignments: Vec<CompiledTagAssignment>,
    expectations: Vec<CompiledExpectation>,
    expectation_selectors: Vec<CompiledSelector>,
}

impl CapabilityConfiguration {
    pub fn compile(&self) -> Result<CompiledCapabilityConfiguration, CapabilityPolicyError> {
        validate(self)?;
        let policies = self.policies.iter().cloned().map(|value| {
            Ok(CompiledPolicy { selector: CompiledSelector::new(value.selector.clone())?, value })
        }).collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let overrides = self.route_overrides.iter().cloned().map(|value| {
            Ok(CompiledOverride { selector: CompiledSelector::new(value.selector.clone())?, value })
        }).collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let tag_assignments = self.route_tags.iter().cloned().map(|value| {
            Ok(CompiledTagAssignment {
                selector: CompiledSelector::new(value.selector.clone())?, value })
        }).collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let bundles = self.bundles.iter().map(|bundle| (bundle.id.as_str(), &bundle.required))
            .collect::<BTreeMap<_, _>>();
        let expectations = self.compatibility_expectations.iter().map(|expectation| {
            let mut required = BTreeSet::new();
            for id in &expectation.bundles {
                required.extend(bundles.get(id.as_str()).ok_or_else(||
                    CapabilityPolicyError::UnknownBundle(id.clone()))?.iter().copied());
            }
            Ok(CompiledExpectation {
                id: expectation.id.clone(), required,
                client_profiles: expectation.client_profiles.clone(),
                permitted_optional_downgrades: expectation.permitted_optional_downgrades.clone(),
                https_image_fixture: expectation.https_image_fixture.clone(),
                selector: expectation.selector.clone(),
            })
        }).collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let expectation_selectors = self.compatibility_expectations.iter()
            .map(|value| CompiledSelector::new(value.selector.clone()))
            .collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let canonical = serde_json::to_vec(self).expect("serializable capability configuration");
        let digest = format!("{:x}", Sha256::digest(canonical));
        Ok(CompiledCapabilityConfiguration {
            source: self.clone(), digest, policies, overrides, tag_assignments, expectations,
            expectation_selectors,
        })
    }
}

impl CompiledCapabilityConfiguration {
    pub fn source(&self) -> &CapabilityConfiguration { &self.source }
    pub fn digest(&self) -> &str { &self.digest }
    pub fn expectations(&self) -> &[CompiledExpectation] { &self.expectations }
    pub fn expectations_for(&self, route: &RouteIdentity) -> Vec<&CompiledExpectation> {
        self.expectations.iter().zip(&self.expectation_selectors)
            .filter_map(|(value, selector)| selector.matches(route).then_some(value)).collect()
    }
    pub fn route_overrides(&self) -> &[RouteCapabilityOverride] {
        &self.source.route_overrides
    }
    pub fn route_overrides_for(&self, route: &RouteIdentity) -> Vec<&RouteCapabilityOverride> {
        let mut matches = self.overrides.iter().filter(|value| value.selector.matches(route))
            .collect::<Vec<_>>();
        matches.sort_by_key(|value| (value.value.priority, value.selector.specificity));
        matches.into_iter().map(|value| &value.value).collect()
    }
    pub fn apply_route_tags(&self, route: &mut RouteIdentity) {
        let tags = self.tag_assignments.iter()
            .filter(|value| value.selector.matches(route))
            .flat_map(|assignment| assignment.value.tags.iter().cloned())
            .collect::<BTreeSet<_>>();
        route.tags.extend(tags);
    }

    pub fn semantic_for(&self, route: &RouteIdentity) -> SemanticPolicy {
        merge_semantics(self.policies.iter().filter(|p| p.selector.matches(route))
            .map(|p| (&p.value, p.selector.specificity)))
            .expect("configuration conflicts were rejected during compile")
    }
    pub fn extensions_for(&self, route: &RouteIdentity) -> Vec<&DeclarativeProbeCase> {
        let mut policies = self.policies.iter().filter(|value| value.selector.matches(route))
            .collect::<Vec<_>>();
        policies.sort_by_key(|value| (value.value.priority, value.selector.specificity));
        policies.into_iter().flat_map(|value| value.value.extension_probes.iter()).collect()
    }
    pub fn policy_ids_for(&self, route: &RouteIdentity) -> Vec<String> {
        let mut policies = self.policies.iter().filter(|value| value.selector.matches(route))
            .collect::<Vec<_>>();
        policies.sort_by_key(|value| (value.value.priority, value.selector.specificity));
        policies.into_iter().map(|value| value.value.id.clone()).collect()
    }
}

fn merge_semantics<'a>(policies: impl Iterator<Item = (&'a CapabilityPolicy, u8)>)
    -> Result<SemanticPolicy, CapabilityPolicyError>
{
    let mut ranked = policies.collect::<Vec<_>>();
    ranked.sort_by_key(|(policy, specificity)| (policy.priority, *specificity));
    let mut result = SemanticPolicy::default();
    let mut ranks = BTreeMap::<&'static str, (i32, u8, String, String)>::new();
    for (policy, specificity) in ranked {
        merge_scalar("context_window", policy.semantic.context_window.map(|v| v.to_string()),
            policy, specificity, &mut ranks)?;
        merge_scalar("max_output_tokens", policy.semantic.max_output_tokens.map(|v| v.to_string()),
            policy, specificity, &mut ranks)?;
        if policy.semantic.context_window.is_some() { result.context_window = policy.semantic.context_window; }
        if policy.semantic.max_output_tokens.is_some() { result.max_output_tokens = policy.semantic.max_output_tokens; }
        if policy.semantic.reasoning_mode.is_some() { result.reasoning_mode = policy.semantic.reasoning_mode; }
        if policy.semantic.reasoning_replay_required.is_some() {
            result.reasoning_replay_required = policy.semantic.reasoning_replay_required;
        }
        result.effort_map.extend(policy.semantic.effort_map.clone());
        result.omit_sampling_fields.extend(policy.semantic.omit_sampling_fields.clone());
    }
    Ok(result)
}

fn merge_scalar(
    field: &'static str, value: Option<String>, policy: &CapabilityPolicy, specificity: u8,
    ranks: &mut BTreeMap<&'static str, (i32, u8, String, String)>,
) -> Result<(), CapabilityPolicyError> {
    let Some(value) = value else { return Ok(()); };
    let rank = (policy.priority, specificity);
    if let Some((priority, old_specificity, old_id, old_value)) = ranks.get(field) {
        if (*priority, *old_specificity) == rank && old_value != &value {
            return Err(CapabilityPolicyError::Ambiguous {
                field, left: old_id.clone(), right: policy.id.clone(),
            });
        }
    }
    ranks.insert(field, (rank.0, rank.1, policy.id.clone(), value));
    Ok(())
}

fn validate(config: &CapabilityConfiguration) -> Result<(), CapabilityPolicyError> {
    if config.schema_version != super::CAPABILITY_SCHEMA_VERSION {
        return Err(CapabilityPolicyError::SchemaVersion(config.schema_version));
    }
    let mut ids = BTreeSet::new();
    for id in config.policies.iter().map(|v| &v.id)
        .chain(config.route_overrides.iter().map(|v| &v.id))
        .chain(config.route_tags.iter().map(|v| &v.id))
        .chain(config.bundles.iter().map(|v| &v.id))
        .chain(config.compatibility_expectations.iter().map(|v| &v.id))
    {
        if id.trim().is_empty() || !ids.insert(id.clone()) {
            return Err(CapabilityPolicyError::DuplicateId(id.clone()));
        }
    }
    for assignment in &config.route_tags {
        if assignment.selector.tag.is_some() || assignment.tags.is_empty()
            || assignment.tags.iter().any(|tag| tag.trim().is_empty())
        {
            return Err(CapabilityPolicyError::InvalidTagAssignment(assignment.id.clone()));
        }
        CompiledSelector::new(assignment.selector.clone())?;
    }
    for case in config.policies.iter().flat_map(|policy| &policy.extension_probes) {
        if case.id.trim().is_empty() || !ids.insert(case.id.clone()) {
            return Err(CapabilityPolicyError::DuplicateId(case.id.clone()));
        }
        if serde_json::to_vec(case).expect("serializable declarative probe").len() > 16_384 {
            return Err(CapabilityPolicyError::ExtensionTooLarge(case.id.clone()));
        }
        if !case.response_predicate.path.starts_with('/')
            || case.response_predicate.path.len() > 256
        {
            return Err(CapabilityPolicyError::InvalidPredicate(
                case.response_predicate.path.clone()));
        }
        validate_patch_paths(&case.request_patch, "")?;
    }
    for expectation in &config.compatibility_expectations {
        CompiledSelector::new(expectation.selector.clone())?;
    }
    for fixture in config.compatibility_expectations.iter()
        .filter_map(|value| value.https_image_fixture.as_ref())
        .chain(config.probe.https_image_fixture.as_ref())
    {
        if !fixture.url.starts_with("https://") || fixture.expected_label.trim().is_empty() {
            return Err(CapabilityPolicyError::InvalidImageFixture);
        }
    }
    validate_ambiguous_selectors(config)?;
    Ok(())
}

fn validate_ambiguous_selectors(config: &CapabilityConfiguration)
    -> Result<(), CapabilityPolicyError>
{
    for (index, left) in config.policies.iter().enumerate() {
        for right in config.policies.iter().skip(index + 1) {
            if left.priority == right.priority
                && selector_specificity(&left.selector) == selector_specificity(&right.selector)
                && selectors_may_overlap(&left.selector, &right.selector)
            {
                let conflict = if option_conflicts(left.semantic.context_window,
                    right.semantic.context_window) { Some("context_window") }
                else if option_conflicts(left.semantic.max_output_tokens,
                    right.semantic.max_output_tokens) { Some("max_output_tokens") }
                else if option_conflicts(left.semantic.reasoning_mode,
                    right.semantic.reasoning_mode) { Some("reasoning_mode") }
                else if option_conflicts(left.semantic.reasoning_replay_required,
                    right.semantic.reasoning_replay_required) { Some("reasoning_replay_required") }
                else if left.semantic.effort_map.iter().any(|(key, value)|
                    right.semantic.effort_map.get(key).map(|other| other != value).unwrap_or(false))
                { Some("effort_map") } else { None };
                if let Some(field) = conflict {
                    return Err(CapabilityPolicyError::Ambiguous {
                        field, left: left.id.clone(), right: right.id.clone(),
                    });
                }
            }
        }
    }
    for (index, left) in config.route_overrides.iter().enumerate() {
        for right in config.route_overrides.iter().skip(index + 1) {
            if left.priority == right.priority
                && selector_specificity(&left.selector) == selector_specificity(&right.selector)
                && selectors_may_overlap(&left.selector, &right.selector)
            {
                let capability_conflict = left.capabilities.iter().any(|(capability, state)|
                    right.capabilities.get(capability).map(|other| other != state).unwrap_or(false))
                    || left.extensions.iter().any(|(id, state)|
                        right.extensions.get(id).map(|other| other != state).unwrap_or(false));
                let scalar_conflict = option_conflicts(left.token_limit_field, right.token_limit_field)
                    || option_conflicts(left.reasoning_carrier, right.reasoning_carrier)
                    || (!left.correction_rules.is_empty() && !right.correction_rules.is_empty()
                        && left.correction_rules != right.correction_rules);
                if capability_conflict || scalar_conflict {
                    return Err(CapabilityPolicyError::Ambiguous {
                        field: "route_capability", left: left.id.clone(), right: right.id.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn option_conflicts<T: Eq>(left: Option<T>, right: Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

fn selector_specificity(selector: &CapabilitySelector) -> u8 {
    let exact = [selector.exposed_model.is_some(), selector.runtime_model.is_some(),
        selector.upstream_id.is_some(), selector.protocol.is_some(), selector.tag.is_some()]
        .into_iter().filter(|value| *value).count() as u8;
    exact.saturating_mul(2) + u8::from(selector.runtime_model_glob.is_some())
}

fn selectors_may_overlap(left: &CapabilitySelector, right: &CapabilitySelector) -> bool {
    if option_differs(&left.exposed_model, &right.exposed_model)
        || option_differs(&left.runtime_model, &right.runtime_model)
        || option_differs(&left.upstream_id, &right.upstream_id)
        || option_differs(&left.protocol, &right.protocol)
    {
        return false;
    }
    if let (Some(exact), Some(pattern)) = (&left.runtime_model, &right.runtime_model_glob) {
        if !Glob::new(pattern).map(|glob| glob.compile_matcher().is_match(exact)).unwrap_or(true) {
            return false;
        }
    }
    if let (Some(pattern), Some(exact)) = (&left.runtime_model_glob, &right.runtime_model) {
        if !Glob::new(pattern).map(|glob| glob.compile_matcher().is_match(exact)).unwrap_or(true) {
            return false;
        }
    }
    true
}

fn option_differs<T: Eq>(left: &Option<T>, right: &Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

fn validate_patch_paths(value: &serde_json::Value, path: &str)
    -> Result<(), CapabilityPolicyError>
{
    const PROTECTED: [&str; 10] = ["/model", "/messages", "/input", "/tools", "/stream",
        "/headers", "/url", "/image_url", "/source", "/data"];
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                let child_path = format!("{path}/{escaped}");
                if PROTECTED.iter().any(|prefix|
                    child_path == *prefix || child_path.starts_with(&format!("{prefix}/")))
                {
                    return Err(CapabilityPolicyError::ProtectedPath(child_path));
                }
                validate_patch_paths(child, &child_path)?;
            }
        }
        serde_json::Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                validate_patch_paths(child, &format!("{path}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}
```

- [ ] **Step 6: Run focused and workspace library tests**

Run: `rtk cargo test --test capability_policy -- --nocapture`

Expected: PASS, including the arbitrary synthetic slug and ambiguous selector cases.

Run: `rtk cargo test --lib`

Expected: PASS with no existing protocol/state regression.

- [ ] **Step 7: Commit the capability contract**

```bash
rtk git add crates/gateway-core/Cargo.toml crates/gateway-core/src/lib.rs src/lib.rs src/capabilities tests/capability_policy.rs
rtk git commit -m "feat: add generic capability policy contract"
```

### Task 2: Resolve Requested Features Without Model Classification

**Files:**
- Modify: `src/capabilities/mod.rs`
- Modify: `src/capabilities/types.rs`
- Create: `src/capabilities/resolver.rs`
- Create: `tests/capability_resolver.rs`

- [ ] **Step 1: Write failing precedence, baseline, and expectation-isolation tests**

Create `tests/capability_resolver.rs`:

```rust
use chat_responses_codex::capabilities::*;
use std::collections::{BTreeMap, BTreeSet};

fn route(protocol: WireProtocol) -> RouteIdentity {
    RouteIdentity {
        upstream_id: "relay-17".into(),
        exposed_model_slug: "opaque-public-name".into(),
        runtime_model_slug: "opaque-runtime-name".into(),
        protocol,
        tags: BTreeSet::new(),
    }
}

#[test]
fn explicit_override_beats_probe_and_probe_beats_baseline() {
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route(
        WireProtocol::ChatCompletions,
    )));
    profile.capabilities.insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    let override_value = RouteCapabilityOverride {
        id: "operator".into(), priority: 100, selector: CapabilitySelector::default(),
        capabilities: BTreeMap::from([(Capability::ParallelToolCalls, EvidenceState::Rejected)]),
        token_limit_field: Some(TokenLimitField::MaxCompletionTokens),
        reasoning_carrier: None,
        correction_rules: Vec::new(),
        extensions: BTreeMap::new(),
    };
    let route_overrides = [&override_value];
    let resolved = CapabilityResolver::default().resolve(ResolutionInput {
        route: &route(WireProtocol::ChatCompletions),
        requested: &RequestedFeatures::text_stream(),
        semantic: &SemanticPolicy::default(),
        route_overrides: &route_overrides,
        policy_extensions: &[],
        profile: Some(&profile),
        strip_nonstandard_chat_fields: false,
    }).unwrap();
    assert_eq!(resolved.state(Capability::ParallelToolCalls), EvidenceState::Rejected);
    assert_eq!(resolved.source(Capability::ParallelToolCalls), CapabilitySource::Override);
    assert_eq!(resolved.token_limit_field, TokenLimitField::MaxCompletionTokens);
}

#[test]
fn unprobed_chat_is_conservative_and_unprobed_responses_is_restricted() {
    let resolver = CapabilityResolver::default();
    let chat = resolver.resolve(ResolutionInput::baseline(
        &route(WireProtocol::ChatCompletions), &RequestedFeatures::text_stream())).unwrap();
    assert!(chat.supports(Capability::FunctionTools));
    assert!(!chat.supports(Capability::ImageDataUrl));
    let responses = resolver.resolve(ResolutionInput::baseline(
        &route(WireProtocol::Responses), &RequestedFeatures::text_stream())).unwrap();
    assert!(responses.provisional);
    assert!(!responses.native_preferred);
}

#[test]
fn required_image_is_rejected_before_dispatch_without_positive_evidence() {
    let mut requested = RequestedFeatures::text_stream();
    requested.required.insert(Capability::ImageHttps);
    let error = CapabilityResolver::default().resolve(ResolutionInput::baseline(
        &route(WireProtocol::ChatCompletions), &requested)).unwrap_err();
    assert_eq!(error.category(), "gateway_protocol_capability_unsupported");
}

#[test]
fn legacy_strip_flag_cannot_remove_required_continuation_state() {
    let mut requested = RequestedFeatures::text_stream();
    requested.required.extend([Capability::FunctionTools, Capability::ReasoningReplay]);
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route(
        WireProtocol::ChatCompletions,
    )));
    profile.capabilities.insert(Capability::ReasoningReplay, EvidenceState::Supported);
    let resolved = CapabilityResolver::default().resolve(ResolutionInput {
        route: &route(WireProtocol::ChatCompletions), requested: &requested,
        semantic: &SemanticPolicy::default(), route_overrides: &[], policy_extensions: &[],
        profile: Some(&profile),
        strip_nonstandard_chat_fields: true,
    }).unwrap();
    assert!(resolved.supports(Capability::ReasoningReplay));
    assert!(resolved.omit_optional_extensions);
}
```

- [ ] **Step 2: Run the resolver test and confirm missing types**

Run: `rtk cargo test --test capability_resolver -- --nocapture`

Expected: FAIL because `CapabilityResolver`, `RequestedFeatures`, and dialect profile types do not exist.

- [ ] **Step 3: Add profile and resolution types**

Append these types to `src/capabilities/types.rs`:

```rust
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DialectProfileKey {
    pub upstream_id: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
}

impl DialectProfileKey {
    pub fn from_route(route: &RouteIdentity) -> Self {
        Self {
            upstream_id: route.upstream_id.clone(),
            runtime_model_slug: route.runtime_model_slug.clone(),
            protocol: route.protocol,
        }
    }
}

impl From<crate::routing::UpstreamProtocol> for WireProtocol {
    fn from(value: crate::routing::UpstreamProtocol) -> Self {
        match value {
            crate::routing::UpstreamProtocol::ChatCompletions => Self::ChatCompletions,
            crate::routing::UpstreamProtocol::Responses => Self::Responses,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialectProfileState { Verified, Partial, Unsupported, Unknown }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpstreamDialectProfile {
    pub key: DialectProfileKey,
    pub configuration_fingerprint: String,
    pub probe_schema_version: u32,
    pub state: DialectProfileState,
    pub capabilities: BTreeMap<Capability, EvidenceState>,
    pub token_limit_field: Option<TokenLimitField>,
    pub reasoning_carrier: Option<ReasoningCarrier>,
    pub correction_rules: Vec<DialectCorrectionRule>,
    pub reasoning_controls: BTreeMap<String, Vec<String>>,
    pub extension_evidence: BTreeMap<String, EvidenceState>,
    pub last_attempt_at: Option<u64>,
    pub last_success_at: Option<u64>,
    pub last_operational_failure: Option<String>,
    pub evidence_codes: BTreeSet<String>,
    pub http_status: Option<u16>,
    pub event_types: BTreeSet<String>,
}

impl UpstreamDialectProfile {
    pub fn unknown(key: DialectProfileKey) -> Self {
        Self {
            key, configuration_fingerprint: String::new(),
            probe_schema_version: super::DIALECT_PROBE_SCHEMA_VERSION,
            state: DialectProfileState::Unknown, capabilities: BTreeMap::new(),
            token_limit_field: None, reasoning_carrier: None,
            correction_rules: Vec::new(),
            reasoning_controls: BTreeMap::new(), extension_evidence: BTreeMap::new(),
            last_attempt_at: None,
            last_success_at: None, last_operational_failure: None,
            evidence_codes: BTreeSet::new(),
            http_status: None, event_types: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RequestedFeatures {
    pub required: BTreeSet<Capability>,
    pub optional: BTreeSet<Capability>,
    pub explicitly_selected_tool_kind: Option<String>,
    pub continuation_profile: Option<DialectProfileKey>,
    pub continuation_reasoning_carrier: Option<ReasoningCarrier>,
}

impl RequestedFeatures {
    pub fn text_stream() -> Self {
        Self { required: BTreeSet::from([Capability::TextInput, Capability::TextStream]),
            ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedCapability {
    pub state: EvidenceState,
    pub source: CapabilitySource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRequestExtension {
    pub id: String,
    pub request_patch: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCapabilities {
    pub values: BTreeMap<Capability, ResolvedCapability>,
    pub token_limit_field: TokenLimitField,
    pub reasoning_mode: ReasoningMode,
    pub reasoning_carrier: ReasoningCarrier,
    pub reasoning_control_field: Option<String>,
    pub effort_map: BTreeMap<String, String>,
    pub omit_sampling_fields: BTreeSet<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub omit_optional_extensions: bool,
    pub provisional: bool,
    pub native_preferred: bool,
    pub adapters: BTreeSet<String>,
    pub request_extensions: Vec<ResolvedRequestExtension>,
    pub field_sources: BTreeMap<String, CapabilitySource>,
}

impl ResolvedCapabilities {
    pub fn state(&self, capability: Capability) -> EvidenceState {
        self.values.get(&capability).map(|v| v.state).unwrap_or(EvidenceState::Unobserved)
    }
    pub fn source(&self, capability: Capability) -> CapabilitySource {
        self.values.get(&capability).map(|v| v.source).unwrap_or(CapabilitySource::Baseline)
    }
    pub fn supports(&self, capability: Capability) -> bool {
        self.state(capability) == EvidenceState::Supported
    }
}
```

- [ ] **Step 4: Implement resolver precedence and fail-closed eligibility**

Create `src/capabilities/resolver.rs` and export it from `src/capabilities/mod.rs`:

```rust
use super::types::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
#[error("route cannot preserve required capability {capability:?}")]
pub struct CapabilityResolutionError { pub capability: Capability }

impl CapabilityResolutionError {
    pub fn category(&self) -> &'static str { "gateway_protocol_capability_unsupported" }
}

pub struct ResolutionInput<'a> {
    pub route: &'a RouteIdentity,
    pub requested: &'a RequestedFeatures,
    pub semantic: &'a SemanticPolicy,
    pub route_overrides: &'a [&'a RouteCapabilityOverride],
    pub policy_extensions: &'a [&'a DeclarativeProbeCase],
    pub profile: Option<&'a UpstreamDialectProfile>,
    pub strip_nonstandard_chat_fields: bool,
}

impl<'a> ResolutionInput<'a> {
    pub fn baseline(route: &'a RouteIdentity, requested: &'a RequestedFeatures) -> Self {
        Self { route, requested, semantic: &EMPTY_SEMANTIC, route_overrides: &[],
            policy_extensions: &[],
            profile: None, strip_nonstandard_chat_fields: false }
    }
}

static EMPTY_SEMANTIC: SemanticPolicy = SemanticPolicy {
    reasoning_mode: None, reasoning_replay_required: None,
    effort_map: BTreeMap::new(), context_window: None, max_output_tokens: None,
    omit_sampling_fields: BTreeSet::new(),
};

#[derive(Default)]
pub struct CapabilityResolver;

impl CapabilityResolver {
    pub fn resolve(&self, input: ResolutionInput<'_>)
        -> Result<ResolvedCapabilities, CapabilityResolutionError>
    {
        let mut values = baseline(input.route.protocol);
        let continuation_carrier = input.requested.continuation_profile.as_ref()
            .filter(|key| *key == &DialectProfileKey::from_route(input.route))
            .and(input.requested.continuation_reasoning_carrier);
        if continuation_carrier == Some(ReasoningCarrier::ReasoningContent) {
            for capability in [Capability::ReasoningOutput, Capability::ReasoningReplay] {
                values.insert(capability, ResolvedCapability {
                    state: EvidenceState::Supported, source: CapabilitySource::Baseline });
            }
        }
        if let Some(profile) = input.profile {
            for (&capability, &state) in &profile.capabilities {
                if state != EvidenceState::Unobserved {
                    values.insert(capability, ResolvedCapability { state, source: CapabilitySource::Probe });
                }
            }
        }
        for override_value in input.route_overrides {
            for (&capability, &state) in &override_value.capabilities {
                values.insert(capability, ResolvedCapability { state, source: CapabilitySource::Override });
            }
        }
        let mut required_capabilities = input.requested.required.clone();
        if input.semantic.reasoning_mode == Some(ReasoningMode::FixedOn) {
            required_capabilities.insert(Capability::ReasoningOutput);
        }
        if input.semantic.reasoning_replay_required == Some(true) {
            required_capabilities.extend([Capability::ReasoningOutput, Capability::ReasoningReplay]);
        }
        for required in required_capabilities {
            if values.get(&required).map(|v| v.state) != Some(EvidenceState::Supported) {
                return Err(CapabilityResolutionError { capability: required });
            }
        }
        let profile_verified = input.profile.map(|p| p.state == DialectProfileState::Verified).unwrap_or(false);
        let override_token = input.route_overrides.iter()
            .filter_map(|value| value.token_limit_field).last();
        let profile_token = input.profile.and_then(|value| value.token_limit_field);
        let override_carrier = input.route_overrides.iter()
            .filter_map(|value| value.reasoning_carrier).last();
        let profile_carrier = input.profile.and_then(|value| value.reasoning_carrier);
        let (reasoning_control_field, effort_map) = resolve_effort_control(
            input.semantic, input.profile);
        let requested_capabilities = input.requested.required.iter()
            .chain(&input.requested.optional).copied().collect::<BTreeSet<_>>();
        let request_extensions = (!input.strip_nonstandard_chat_fields).then(||
            input.policy_extensions.iter().filter_map(|extension| {
            if extension.protocol != input.route.protocol
                || !extension.prerequisites.is_subset(&requested_capabilities)
            {
                return None;
            }
            let override_state = input.route_overrides.iter()
                .filter_map(|value| value.extensions.get(&extension.id)).last().copied();
            let state = override_state.or_else(|| input.profile
                .and_then(|profile| profile.extension_evidence.get(&extension.id).copied()))
                .unwrap_or(EvidenceState::Unobserved);
            (state == EvidenceState::Supported).then(|| ResolvedRequestExtension {
                id: extension.id.clone(), request_patch: extension.request_patch.clone() })
        }).collect()).unwrap_or_default();
        let mut field_sources = BTreeMap::new();
        field_sources.insert("token_limit_field".into(), if override_token.is_some() {
            CapabilitySource::Override
        } else if profile_token.is_some() { CapabilitySource::Probe }
        else { CapabilitySource::Baseline });
        field_sources.insert("reasoning_carrier".into(), if override_carrier.is_some() {
            CapabilitySource::Override
        } else if profile_carrier.is_some() { CapabilitySource::Probe }
        else { CapabilitySource::Baseline });
        for (name, present) in [("reasoning_mode", input.semantic.reasoning_mode.is_some()),
            ("context_window", input.semantic.context_window.is_some()),
            ("max_output_tokens", input.semantic.max_output_tokens.is_some())]
        {
            field_sources.insert(name.into(), if present {
                CapabilitySource::Policy } else { CapabilitySource::Baseline });
        }
        field_sources.insert("effort_map".into(), if effort_map.is_empty() {
            CapabilitySource::Baseline } else { CapabilitySource::Probe });
        field_sources.insert("request_extensions".into(), if request_extensions.is_empty() {
            CapabilitySource::Baseline } else if input.route_overrides.iter()
                .any(|value| !value.extensions.is_empty()) { CapabilitySource::Override }
            else { CapabilitySource::Probe });
        Ok(ResolvedCapabilities {
            values,
            token_limit_field: override_token.or(profile_token).unwrap_or(TokenLimitField::Omit),
            reasoning_mode: input.semantic.reasoning_mode.unwrap_or(ReasoningMode::Off),
            reasoning_carrier: override_carrier.or(profile_carrier).or(continuation_carrier)
                .unwrap_or(ReasoningCarrier::None),
            reasoning_control_field,
            effort_map,
            omit_sampling_fields: input.semantic.omit_sampling_fields.clone(),
            context_window: input.semantic.context_window,
            max_output_tokens: input.semantic.max_output_tokens,
            omit_optional_extensions: input.strip_nonstandard_chat_fields,
            provisional: !profile_verified,
            native_preferred: profile_verified || input.route.protocol == WireProtocol::ChatCompletions,
            adapters: BTreeSet::new(),
            request_extensions,
            field_sources,
        })
    }
}

fn resolve_effort_control(semantic: &SemanticPolicy,
    profile: Option<&UpstreamDialectProfile>) -> (Option<String>, BTreeMap<String, String>)
{
    let Some(profile) = profile else { return (None, BTreeMap::new()); };
    for (field, accepted) in &profile.reasoning_controls {
        let mapped = semantic.effort_map.iter()
            .filter(|(_, upstream)| accepted.contains(upstream))
            .map(|(client, upstream)| (client.clone(), upstream.clone()))
            .collect::<BTreeMap<_, _>>();
        if !mapped.is_empty() { return (Some(field.clone()), mapped); }
    }
    (None, BTreeMap::new())
}

fn baseline(protocol: WireProtocol) -> BTreeMap<Capability, ResolvedCapability> {
    let supported = match protocol {
        WireProtocol::ChatCompletions => [Capability::TextInput, Capability::TextStream,
            Capability::FunctionTools, Capability::ForcedToolChoice, Capability::ToolContinuation],
        WireProtocol::Responses => [Capability::TextInput, Capability::TextStream,
            Capability::FunctionTools, Capability::ForcedToolChoice, Capability::ToolContinuation],
        WireProtocol::Messages => [Capability::TextInput, Capability::TextStream,
            Capability::FunctionTools, Capability::ForcedToolChoice, Capability::ToolContinuation],
    };
    Capability::ALL.into_iter().map(|capability| {
        let state = if supported.contains(&capability) { EvidenceState::Supported }
            else { EvidenceState::Unobserved };
        (capability, ResolvedCapability { state, source: CapabilitySource::Baseline })
    }).collect()
}
```

Add this constant to `impl Capability` in `types.rs`:

```rust
pub const ALL: [Capability; 19] = [
    Capability::TextInput, Capability::ImageHttps, Capability::ImageDataUrl,
    Capability::ImageDetail, Capability::NativeFileId, Capability::FunctionTools,
    Capability::NamespaceTools,
    Capability::CustomTools, Capability::HostedTools, Capability::ParallelToolCalls,
    Capability::ForcedToolChoice, Capability::ToolContinuation,
    Capability::ReasoningOutput, Capability::ReasoningReplay, Capability::TextStream,
    Capability::ReasoningStream, Capability::IndexedToolArgumentStream,
    Capability::UsageStream, Capability::StructuredOutput,
];
```

Keep `native_preferred` false for unverified Responses; Chat remains a viable conservative fallback. Keep expectations entirely out of `ResolutionInput`.

- [ ] **Step 5: Run resolver and policy tests**

Run: `rtk cargo test --test capability_policy --test capability_resolver -- --nocapture`

Expected: PASS. The unprobed Responses assertion must report `native_preferred == false`.

- [ ] **Step 6: Commit the resolver**

```bash
rtk git add src/capabilities tests/capability_resolver.rs
rtk git commit -m "feat: resolve route capabilities from generic evidence"
```

### Task 3: Persist Capability State Separately And Publish Atomic Snapshots

**Files:**
- Modify: `src/capabilities/types.rs`
- Modify: `src/state/store.rs`
- Modify: `src/state/file_store.rs`
- Modify: `src/state/postgres.rs`
- Modify: `src/state.rs`
- Modify: `tests/state_store.rs`
- Modify: `tests/postgres_roundtrip.rs`
- Create: `tests/capability_state.rs`

- [ ] **Step 1: Write failing sidecar and last-valid-snapshot tests**

Create `tests/capability_state.rs`:

```rust
use chat_responses_codex::capabilities::*;
use chat_responses_codex::state::{AppConfig, AppState, PersistedState};
use tempfile::tempdir;

#[tokio::test]
async fn file_backend_keeps_capabilities_out_of_main_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("gateway-state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    let mut config = CapabilityConfiguration::default();
    config.revision = 7;
    state.replace_capability_configuration(config).await.unwrap();
    let main = tokio::fs::read_to_string(&path).await.unwrap_or_else(|_| "{}".into());
    assert!(!main.contains("compatibility_expectations"));
    let sidecar = tokio::fs::read_to_string(dir.path().join("gateway-state.json.capabilities.json"))
        .await.unwrap();
    assert!(sidecar.contains("\"revision\": 7"));
}

#[tokio::test]
async fn invalid_reload_retains_last_valid_snapshot() {
    let dir = tempdir().unwrap();
    let state = AppState::new(PersistedState::default(), dir.path().join("state.json"),
        AppConfig::default());
    let mut good = CapabilityConfiguration::default();
    good.revision = 11;
    state.replace_capability_configuration(good).await.unwrap();
    let mut bad = CapabilityConfiguration::default();
    bad.schema_version = 999;
    assert!(state.replace_capability_configuration(bad).await.is_err());
    assert_eq!(state.capability_snapshot().configuration.source().revision, 11);
}

#[tokio::test]
async fn profile_round_trip_uses_exact_case_sensitive_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    let key = DialectProfileKey {
        upstream_id: "up-1".into(), runtime_model_slug: "Lab/Case-Sensitive".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    state.upsert_dialect_profile(UpstreamDialectProfile::unknown(key.clone())).await.unwrap();
    let loaded = AppState::load_from_path(&path, AppConfig::default()).await.unwrap();
    assert!(loaded.capability_snapshot().profiles.contains_key(&key));
    assert!(!loaded.capability_snapshot().profiles.keys()
        .any(|candidate| candidate.runtime_model_slug == "lab/case-sensitive"));
}
```

- [ ] **Step 2: Run the state test and confirm missing store methods**

Run: `rtk cargo test --test capability_state -- --nocapture`

Expected: FAIL because capability state and `AppState` snapshot methods are not defined.

- [ ] **Step 3: Add the persisted document and immutable runtime snapshot**

Append to `src/capabilities/types.rs`:

```rust
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilityStateDocument {
    pub configuration: CapabilityConfiguration,
    pub profiles: BTreeMap<DialectProfileKey, UpstreamDialectProfile>,
}

#[derive(Clone)]
pub struct CapabilityRuntimeSnapshot {
    pub configuration: std::sync::Arc<super::CompiledCapabilityConfiguration>,
    pub profiles: BTreeMap<DialectProfileKey, UpstreamDialectProfile>,
}

impl Default for CapabilityRuntimeSnapshot {
    fn default() -> Self {
        Self {
            configuration: std::sync::Arc::new(
                CapabilityConfiguration::default().compile().expect("default capability policy")),
            profiles: BTreeMap::new(),
        }
    }
}
```

- [ ] **Step 4: Extend `StateStore` with source-compatible defaults**

Add imports and these methods to `src/state/store.rs`:

```rust
use crate::capabilities::{CapabilityConfiguration, CapabilityStateDocument, UpstreamDialectProfile};

fn load_capability_state<'a>(&'a self)
    -> StoreFuture<'a, io::Result<CapabilityStateDocument>>
{
    Box::pin(async { Ok(CapabilityStateDocument::default()) })
}

fn persist_capability_configuration<'a>(&'a self, _config: &'a CapabilityConfiguration)
    -> StoreFuture<'a, io::Result<()>>
{
    Box::pin(async { Ok(()) })
}

fn upsert_dialect_profile<'a>(&'a self, _profile: &'a UpstreamDialectProfile)
    -> StoreFuture<'a, io::Result<()>>
{
    Box::pin(async { Ok(()) })
}

fn delete_dialect_profiles_for_upstream<'a>(&'a self, _upstream_id: &'a str)
    -> StoreFuture<'a, io::Result<()>>
{
    Box::pin(async { Ok(()) })
}
```

Because every new method has a default, `CountingStore`, `SlowStore`, and `QueryStore` need no boilerplate changes.

- [ ] **Step 5: Implement atomic file sidecar operations**

Change `FileStateStore` to hold `capability_write_lock: Arc<tokio::sync::Mutex<()>>`, add:

```rust
fn capability_path(&self) -> PathBuf {
    let name = self.config_path.file_name().and_then(|v| v.to_str()).unwrap_or("state.json");
    self.config_path.with_file_name(format!("{name}.capabilities.json"))
}

async fn load_capability_document(&self) -> io::Result<CapabilityStateDocument> {
    let path = self.capability_path();
    if !fs::try_exists(&path).await? { return Ok(CapabilityStateDocument::default()); }
    serde_json::from_slice(&fs::read(path).await?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn write_capability_document(&self, document: &CapabilityStateDocument) -> io::Result<()> {
    let path = self.capability_path();
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).await?; }
    let bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).await?;
    fs::rename(tmp, path).await
}
```

Implement each `StateStore` capability method by taking the dedicated lock, loading the document, changing only `configuration` or the keyed profile, and calling `write_capability_document`. Never read or write the main state file from these methods.

- [ ] **Step 6: Add normalized PostgreSQL capability tables and methods**

Append to `POSTGRES_SCHEMA` in `src/state/postgres.rs`:

```sql
CREATE TABLE IF NOT EXISTS capability_configuration (
    singleton_id TEXT PRIMARY KEY CHECK (singleton_id = 'default'),
    document TEXT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS dialect_profiles (
    upstream_id TEXT NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
    runtime_model_slug TEXT NOT NULL,
    protocol TEXT NOT NULL,
    profile TEXT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (upstream_id, runtime_model_slug, protocol)
);
CREATE INDEX IF NOT EXISTS dialect_profiles_upstream_idx
    ON dialect_profiles (upstream_id);
```

Implement `load_capability_state`, `persist_capability_configuration`, `upsert_dialect_profile`, and `delete_dialect_profiles_for_upstream` with parameterized SQL and `serde_json`. Use the exact serialized `WireProtocol` value as the protocol key and preserve runtime slug case.

Override all four default methods in `impl StateStore for PostgresStateStore` so calls through `Arc<dyn StateStore>` reach the PostgreSQL implementation:

```rust
fn load_capability_state<'a>(&'a self) -> StoreFuture<'a, io::Result<CapabilityStateDocument>> {
    Box::pin(async move { PostgresStateStore::load_capability_state(self).await })
}

fn persist_capability_configuration<'a>(&'a self, config: &'a CapabilityConfiguration)
    -> StoreFuture<'a, io::Result<()>>
{
    Box::pin(async move { PostgresStateStore::persist_capability_configuration(self, config).await })
}

fn upsert_dialect_profile<'a>(&'a self, profile: &'a UpstreamDialectProfile)
    -> StoreFuture<'a, io::Result<()>>
{
    Box::pin(async move { PostgresStateStore::upsert_dialect_profile(self, profile).await })
}

fn delete_dialect_profiles_for_upstream<'a>(&'a self, upstream_id: &'a str)
    -> StoreFuture<'a, io::Result<()>>
{
    Box::pin(async move {
        PostgresStateStore::delete_dialect_profiles_for_upstream(self, upstream_id).await
    })
}
```

- [ ] **Step 7: Publish and swap compiled snapshots in `AppState`**

Add this field to `AppState` and initialize it in every constructor:

```rust
capability_snapshot: Arc<arc_swap::ArcSwap<CapabilityRuntimeSnapshot>>,
capability_update_lock: Arc<tokio::sync::Mutex<()>>,
```

Add these methods to `AppState`:

```rust
pub fn capability_snapshot(&self) -> Arc<CapabilityRuntimeSnapshot> {
    self.capability_snapshot.load_full()
}

pub async fn replace_capability_configuration(
    &self,
    configuration: CapabilityConfiguration,
) -> io::Result<()> {
    let _guard = self.capability_update_lock.lock().await;
    let compiled = configuration.compile()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    self.config_store.persist_capability_configuration(&configuration).await?;
    let current = self.capability_snapshot();
    self.capability_snapshot.store(Arc::new(CapabilityRuntimeSnapshot {
        configuration: Arc::new(compiled),
        profiles: current.profiles.clone(),
    }));
    Ok(())
}

pub async fn upsert_dialect_profile(&self, profile: UpstreamDialectProfile) -> io::Result<()> {
    let _guard = self.capability_update_lock.lock().await;
    self.config_store.upsert_dialect_profile(&profile).await?;
    let current = self.capability_snapshot();
    let mut profiles = current.profiles.clone();
    profiles.insert(profile.key.clone(), profile);
    self.capability_snapshot.store(Arc::new(CapabilityRuntimeSnapshot {
        configuration: current.configuration.clone(), profiles,
    }));
    Ok(())
}
```

In `load_from_path` and `load_from_database_url`, call `config_store.load_capability_state().await`, compile the candidate configuration, and initialize the snapshot before returning. Invalid persisted JSON is a startup error; an invalid administrative replacement at runtime returns 400 and retains the current snapshot.
Take `capability_update_lock` for configuration swaps, profile upserts, and profile deletion so concurrent probe completion and policy import cannot overwrite each other's snapshot changes.

- [ ] **Step 8: Test file and PostgreSQL round trips**

Run: `rtk cargo test --test capability_state --test state_store -- --nocapture`

Expected: PASS; the sidecar test proves the main `PersistedState` remains unchanged.

Run: `rtk cargo test --test postgres_roundtrip capability -- --nocapture`

Expected: PASS when `TEST_DATABASE_URL` is configured; otherwise existing PostgreSQL tests report their normal skip behavior.

- [ ] **Step 9: Commit focused capability persistence**

```bash
rtk git add src/capabilities/types.rs src/state.rs src/state/store.rs src/state/file_store.rs src/state/postgres.rs tests/capability_state.rs tests/state_store.rs tests/postgres_roundtrip.rs
rtk git commit -m "feat: persist capability evidence outside main state"
```

### Task 4: Fingerprint Profiles And Bound The Probe Queue

**Files:**
- Modify: `src/capabilities/mod.rs`
- Create: `src/capabilities/profile.rs`
- Create: `src/capabilities/probe_queue.rs`
- Modify: `src/state.rs`
- Create: `tests/capability_profiles.rs`
- Create: `tests/probe_queue.rs`

- [ ] **Step 1: Write failing fingerprint and queue tests**

Create `tests/capability_profiles.rs`:

```rust
use chat_responses_codex::capabilities::*;

#[test]
fn route_fingerprint_changes_for_every_dialect_input() {
    let base = RouteFingerprintInput {
        normalized_base_url: "https://relay.example/v1".into(),
        enabled_protocols: vec![WireProtocol::ChatCompletions],
        runtime_model_slug: "Lab/Model".into(),
        route_override_digest: "override-a".into(),
        probe_schema_version: DIALECT_PROBE_SCHEMA_VERSION,
    };
    let original = route_fingerprint(&base);
    let mut changed = base.clone(); changed.normalized_base_url = "https://relay-2.example/v1".into();
    assert_ne!(original, route_fingerprint(&changed));
    let mut changed = base.clone(); changed.runtime_model_slug = "Lab/model".into();
    assert_ne!(original, route_fingerprint(&changed));
    let mut changed = base.clone(); changed.enabled_protocols.push(WireProtocol::Responses);
    assert_ne!(original, route_fingerprint(&changed));
    let mut changed = base.clone(); changed.route_override_digest = "override-b".into();
    assert_ne!(original, route_fingerprint(&changed));
    let mut changed = base; changed.probe_schema_version += 1;
    assert_ne!(original, route_fingerprint(&changed));
}

#[test]
fn operational_probe_failure_does_not_erase_verified_evidence() {
    let key = DialectProfileKey { upstream_id: "u".into(), runtime_model_slug: "m".into(),
        protocol: WireProtocol::ChatCompletions };
    let mut profile = UpstreamDialectProfile::unknown(key);
    profile.state = DialectProfileState::Verified;
    profile.capabilities.insert(Capability::FunctionTools, EvidenceState::Supported);
    apply_probe_outcome(&mut profile, ProbeOutcome::OperationalFailure {
        code: "upstream_authentication".into(), http_status: Some(401), attempted_at: 99,
    });
    assert_eq!(profile.state, DialectProfileState::Verified);
    assert_eq!(profile.capabilities[&Capability::FunctionTools], EvidenceState::Supported);
    assert_eq!(profile.last_attempt_at, Some(99));
}
```

Create `tests/probe_queue.rs`:

```rust
use chat_responses_codex::capabilities::*;

fn job(upstream: &str, model: &str) -> ProbeJob {
    ProbeJob { key: DialectProfileKey { upstream_id: upstream.into(),
        runtime_model_slug: model.into(), protocol: WireProtocol::ChatCompletions },
        reason: ProbeReason::ConfigurationChanged }
}

#[test]
fn queue_deduplicates_and_limits_global_and_per_upstream_work() {
    let mut queue = ProbeQueueState::new(2, 1);
    assert!(queue.enqueue(job("u1", "m1")));
    assert!(!queue.enqueue(job("u1", "m1")));
    assert!(queue.enqueue(job("u1", "m2")));
    assert!(queue.enqueue(job("u2", "m3")));
    let first = queue.start_next().unwrap();
    let second = queue.start_next().unwrap();
    assert_ne!(first.key.upstream_id, second.key.upstream_id);
    assert!(queue.start_next().is_none());
    queue.finish(&first.key);
    assert!(queue.start_next().is_some());
}
```

- [ ] **Step 2: Run focused tests and confirm profile helpers are missing**

Run: `rtk cargo test --test capability_profiles --test probe_queue -- --nocapture`

Expected: FAIL because fingerprint, outcome, and queue types do not exist.

- [ ] **Step 3: Implement deterministic fingerprints and evidence-safe outcomes**

Create `src/capabilities/profile.rs`:

```rust
use super::*;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RouteFingerprintInput {
    pub normalized_base_url: String,
    pub enabled_protocols: Vec<WireProtocol>,
    pub runtime_model_slug: String,
    pub route_override_digest: String,
    pub probe_schema_version: u32,
}

pub fn normalize_route_base_url(value: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(value.trim()).map_err(|error| error.to_string())?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let normalized_path = format!("/{}", url.path().trim_matches('/'));
    url.set_path(normalized_path.trim_end_matches('/'));
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

pub fn route_fingerprint(input: &RouteFingerprintInput) -> String {
    let mut canonical = input.clone();
    canonical.enabled_protocols.sort();
    canonical.enabled_protocols.dedup();
    format!("{:x}", Sha256::digest(serde_json::to_vec(&canonical)
        .expect("serializable fingerprint input")))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeOutcome {
    Conclusive {
        capabilities: std::collections::BTreeMap<Capability, EvidenceState>,
        token_limit_field: Option<TokenLimitField>,
        reasoning_carrier: Option<ReasoningCarrier>,
        correction_rules: Vec<DialectCorrectionRule>,
        extension_evidence: std::collections::BTreeMap<String, EvidenceState>,
        evidence_codes: std::collections::BTreeSet<String>,
        event_types: std::collections::BTreeSet<String>,
        http_status: u16,
        attempted_at: u64,
    },
    OperationalFailure { code: String, http_status: Option<u16>, attempted_at: u64 },
}

impl ProbeOutcome {
    pub fn capability(&self, capability: Capability) -> EvidenceState {
        match self {
            Self::Conclusive { capabilities, .. } => capabilities.get(&capability)
                .copied().unwrap_or(EvidenceState::Unobserved),
            Self::OperationalFailure { .. } => EvidenceState::Unobserved,
        }
    }

    pub fn evidence_codes(&self) -> std::collections::BTreeSet<String> {
        match self {
            Self::Conclusive { evidence_codes, .. } => evidence_codes.clone(),
            Self::OperationalFailure { code, .. } => [code.clone()].into_iter().collect(),
        }
    }
}

pub fn apply_probe_outcome(profile: &mut UpstreamDialectProfile, outcome: ProbeOutcome) {
    match outcome {
        ProbeOutcome::OperationalFailure { code, http_status, attempted_at } => {
            profile.last_attempt_at = Some(attempted_at);
            profile.http_status = http_status;
            profile.last_operational_failure = Some(code);
        }
        ProbeOutcome::Conclusive { capabilities, token_limit_field, reasoning_carrier,
            correction_rules, extension_evidence,
            evidence_codes, event_types, http_status, attempted_at } => {
            profile.capabilities = capabilities;
            profile.token_limit_field = token_limit_field;
            profile.reasoning_carrier = reasoning_carrier;
            profile.correction_rules = correction_rules;
            profile.extension_evidence = extension_evidence;
            profile.evidence_codes = evidence_codes;
            profile.event_types = event_types;
            profile.http_status = Some(http_status);
            profile.last_attempt_at = Some(attempted_at);
            profile.last_success_at = Some(attempted_at);
            profile.last_operational_failure = None;
            let supported = profile.capabilities.values().filter(|v| **v == EvidenceState::Supported).count();
            let rejected = profile.capabilities.values().filter(|v| **v == EvidenceState::Rejected).count();
            profile.state = if supported == 0 && rejected > 0 { DialectProfileState::Unsupported }
                else if rejected == 0 { DialectProfileState::Verified }
                else { DialectProfileState::Partial };
        }
    }
}

pub fn profile_is_current(profile: &UpstreamDialectProfile, fingerprint: &str, now: u64,
    refresh_interval_seconds: u64) -> bool
{
    profile.configuration_fingerprint == fingerprint
        && profile.probe_schema_version == DIALECT_PROBE_SCHEMA_VERSION
        && profile.last_success_at.map(|at| now.saturating_sub(at) < refresh_interval_seconds)
            .unwrap_or(false)
}
```

- [ ] **Step 4: Implement the pure bounded queue state**

Create `src/capabilities/probe_queue.rs`:

```rust
use super::DialectProfileKey;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeReason { ConfigurationChanged, ModelDiscovered, ScheduledRefresh,
    DialectError, Manual }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeJob { pub key: DialectProfileKey, pub reason: ProbeReason }

pub struct ProbeQueueState {
    pending: VecDeque<ProbeJob>,
    known: BTreeSet<DialectProfileKey>,
    active: BTreeSet<DialectProfileKey>,
    active_by_upstream: BTreeMap<String, usize>,
    max_global: usize,
    max_per_upstream: usize,
}

impl ProbeQueueState {
    pub fn new(max_global: usize, max_per_upstream: usize) -> Self {
        Self { pending: VecDeque::new(), known: BTreeSet::new(), active: BTreeSet::new(),
            active_by_upstream: BTreeMap::new(), max_global: max_global.max(1),
            max_per_upstream: max_per_upstream.max(1) }
    }
    pub fn enqueue(&mut self, job: ProbeJob) -> bool {
        if !self.known.insert(job.key.clone()) { return false; }
        self.pending.push_back(job); true
    }
    pub fn set_limits(&mut self, max_global: usize, max_per_upstream: usize) {
        self.max_global = max_global.max(1);
        self.max_per_upstream = max_per_upstream.max(1);
    }
    pub fn start_next(&mut self) -> Option<ProbeJob> {
        if self.active.len() >= self.max_global { return None; }
        let position = self.pending.iter().position(|job|
            self.active_by_upstream.get(&job.key.upstream_id).copied().unwrap_or(0)
                < self.max_per_upstream)?;
        let job = self.pending.remove(position)?;
        self.active.insert(job.key.clone());
        *self.active_by_upstream.entry(job.key.upstream_id.clone()).or_default() += 1;
        Some(job)
    }
    pub fn finish(&mut self, key: &DialectProfileKey) {
        if self.active.remove(key) {
            if let Some(count) = self.active_by_upstream.get_mut(&key.upstream_id) {
                *count = count.saturating_sub(1);
                if *count == 0 { self.active_by_upstream.remove(&key.upstream_id); }
            }
        }
        self.known.remove(key);
    }
}
```

Export `profile` and `probe_queue` from `src/capabilities/mod.rs`.

- [ ] **Step 5: Reconcile profiles after configuration changes**

In `AppState`, add a method that computes exact runtime slugs with `UpstreamConfig::resolved_model_name`, deletes profiles whose upstream no longer exists, and queues a refresh rather than using a profile whose fingerprint differs. Policy revision changes must invalidate resolver caches but must not delete raw profile evidence unless the route override digest changed.

Use this exact return shape so the probe service can consume work without doing persistence under the state lock:

```rust
pub async fn reconcile_dialect_profiles(&self, now: u64) -> io::Result<Vec<ProbeJob>> {
    let routing = self.routing_snapshot().await;
    let snapshot = self.capability_snapshot();
    let mut jobs = Vec::new();
    for upstream in routing.upstreams.iter().filter(|upstream| upstream.active) {
        for exposed in upstream.route_models() {
            let exposed_to_downstream = routing.downstreams.iter().any(|downstream|
                downstream.active && (downstream.model_allowlist.is_empty()
                    || portal_model_is_allowed(&downstream.model_allowlist, &exposed)));
            if !exposed_to_downstream { continue; }
            let Some(runtime) = upstream.resolved_model_name(&exposed) else { continue; };
            for protocol in upstream.protocols.iter().copied() {
                let key = DialectProfileKey { upstream_id: upstream.id.clone(),
                    runtime_model_slug: runtime.clone(), protocol: protocol.into() };
                let fingerprint = self.route_configuration_fingerprint(upstream, &runtime, protocol);
                let current = snapshot.profiles.get(&key);
                if !current.map(|profile| profile_is_current(profile, &fingerprint, now,
                    snapshot.configuration.source().probe.refresh_interval_seconds)).unwrap_or(false)
                {
                    jobs.push(ProbeJob { key, reason: ProbeReason::ConfigurationChanged });
                }
            }
        }
    }
    Ok(jobs)
}
```

- [ ] **Step 6: Run profile, queue, and state regression tests**

Run: `rtk cargo test --test capability_profiles --test probe_queue --test capability_state -- --nocapture`

Expected: PASS; auth failure retains verified function evidence and the queue never starts two jobs for one upstream.

- [ ] **Step 7: Commit profile identity and scheduling state**

```bash
rtk git add src/capabilities src/state.rs tests/capability_profiles.rs tests/probe_queue.rs
rtk git commit -m "feat: fingerprint dialect profiles and bound probes"
```

### Task 5: Run Generic Background Dialect Probes

**Files:**
- Create: `src/server/gateway/capability_probe.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server.rs`
- Modify: `src/state.rs`
- Modify: `src/state/types.rs`
- Modify: `src/main.rs`
- Create: `tests/capability_probe.rs`

- [ ] **Step 1: Write failing semantic-probe tests against local mock upstreams**

Create `tests/capability_probe.rs` using Axum listeners. The mock must expose counters and deterministic JSON/SSE responses. Add these tests:

```rust
#[tokio::test]
async fn forced_tool_plain_text_is_not_positive_tool_evidence() {
    let mock = ProbeMock::chat(|request| {
        assert_eq!(request["tool_choice"]["function"]["name"], "gateway_compat_probe");
        serde_json::json!({
            "id": "chatcmpl-probe",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "done"},
                "finish_reason": "stop"}]
        })
    }).await;
    let outcome = run_probe_against(&mock, ProbePlan::agent_core()).await;
    assert_eq!(outcome.capability(Capability::FunctionTools), EvidenceState::Rejected);
    assert!(outcome.evidence_codes().contains("forced_tool_not_selected"));
}

#[tokio::test]
async fn auth_failure_stops_remaining_cases_and_is_operational() {
    let mock = ProbeMock::status(axum::http::StatusCode::UNAUTHORIZED).await;
    let outcome = run_probe_against(&mock, ProbePlan::full()).await;
    assert!(matches!(outcome, ProbeOutcome::OperationalFailure { http_status: Some(401), .. }));
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn tool_and_reasoning_pass_requires_linked_continuation() {
    let mock = ProbeMock::scripted(vec![
        ProbeReply::chat_tool_call("call_probe", "gateway_compat_probe", r#"{"nonce":"n-17"}"#,
            Some("think-exactly-once")),
        ProbeReply::chat_text("continuation-ok"),
    ]).await;
    let outcome = run_probe_against(&mock, ProbePlan::reasoning_agent()).await;
    assert_eq!(outcome.capability(Capability::FunctionTools), EvidenceState::Supported);
    assert_eq!(outcome.capability(Capability::ToolContinuation), EvidenceState::Supported);
    assert_eq!(outcome.capability(Capability::ReasoningReplay), EvidenceState::Supported);
    let requests = mock.requests();
    assert_eq!(requests[1]["messages"][1]["reasoning_content"], "think-exactly-once");
    assert_eq!(requests[1]["messages"][2]["tool_call_id"], "call_probe");
}

#[tokio::test]
async fn normal_gateway_request_never_launches_a_probe() {
    let mock = ProbeMock::chat_text_only().await;
    let app = app_with_unobserved_profile(mock.base_url()).await;
    let response = send_normal_chat_request(app).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(mock.request_count(), 1, "only the client request may reach upstream");
}
```

The fixture helper must also cover valid SSE order, token-field 400 responses, indexed tool argument fragments, usage chunks, restricted Responses, Data URL image selection, HTTPS image selection, and a generic declarative extension predicate. Keep prompts synthetic and store only request structure in assertions.

- [ ] **Step 2: Run the probe test and confirm the service is missing**

Run: `rtk cargo test --test capability_probe -- --nocapture`

Expected: FAIL because `CapabilityProbeService`, `ProbePlan`, and probe outcome helpers are not implemented.

- [ ] **Step 3: Add probe runtime configuration without changing persisted gateway state**

Add these fields to `AppConfig` in `src/state/types.rs`, defaults, and environment loading in `src/main.rs`:

```rust
pub capability_probe_queue_capacity: usize,
pub capability_probe_request_timeout_seconds: u64,
```

Use defaults `256` and `20`. The refresh interval and concurrency remain policy data in `ProbeConfiguration`; environment values only bound process resources.

- [ ] **Step 4: Define the protocol-owned probe cases**

In `src/server/gateway/capability_probe.rs`, define:

```rust
#[derive(Clone, Debug)]
pub enum CoreProbeCase {
    MinimalText { stream: bool },
    TokenLimit { field: TokenLimitField },
    ReasoningControl { field: String, value: String },
    FunctionSelection,
    ToolContinuation { reasoning_carrier: Option<ReasoningCarrier> },
    ParallelTools,
    IndexedToolArguments,
    UsageStream,
    ImageDataUrl,
    ImageHttps { url: String, expected_label: String },
    RestrictedResponses,
    Declarative(DeclarativeProbeCase),
}

const DATA_URL_IMAGE_FIXTURE: &str = concat!(
    "data:image/png;base64,",
    "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAMElEQVR42mP4T2PAMGoB",
    "aRYwMFAHjVowasGoBaMWjFowasGoBaMWDHULRpuOA2EBAHmBeOr2sW6XAAAAAElFTkSuQmCC"
);
const DATA_URL_IMAGE_EXPECTED_LABEL: &str = "red";

#[derive(Clone, Debug)]
pub struct ProbePlan {
    pub protocol: WireProtocol,
    pub cases: Vec<CoreProbeCase>,
    pub output_token_cap: u32,
}

impl ProbePlan {
    pub fn agent_core() -> Self {
        Self { protocol: WireProtocol::ChatCompletions, output_token_cap: 64, cases: vec![
            CoreProbeCase::MinimalText { stream: false },
            CoreProbeCase::MinimalText { stream: true },
            CoreProbeCase::FunctionSelection,
            CoreProbeCase::IndexedToolArguments,
            CoreProbeCase::UsageStream,
        ] }
    }
    pub fn reasoning_agent() -> Self {
        let mut plan = Self::agent_core();
        plan.cases.push(CoreProbeCase::ToolContinuation {
            reasoning_carrier: Some(ReasoningCarrier::ReasoningContent) });
        plan
    }
    pub fn full() -> Self {
        let mut plan = Self::reasoning_agent();
        plan.cases.extend([CoreProbeCase::ParallelTools,
            CoreProbeCase::ImageDataUrl, CoreProbeCase::RestrictedResponses]);
        plan
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeCaseVerdict {
    Supported { evidence_code: String },
    Rejected { evidence_code: String, http_status: Option<u16> },
    Unobserved { operational_code: String, http_status: Option<u16> },
}
```

Build the plan only from the protocol baseline, applicable `ProbeCandidates`, configured image fixture, and declarative cases. Do not pass model text to the case builder except as the opaque payload value.

- [ ] **Step 5: Implement bounded probe execution and semantic validation**

Implement `CapabilityProbeService::spawn(state)` with `tokio::sync::mpsc::channel(256)` and `ProbeQueueState`. Register its sender in `AppState`; `insert_upstream`, `update_upstream`, model discovery, and manual diagnostics enqueue exact `ProbeJob` values with `try_send`. Do not call this service from `build_router`; start it explicitly in `main.rs` after `AppState::load_from_path` so unit routers cannot launch background traffic.

Before each `start_next`, load the current immutable probe configuration and call `queue.set_limits(config.max_global_concurrency, config.max_per_upstream_concurrency)`. A policy reload therefore changes future scheduling without cancelling an active probe.

The runner must use this ordering and stop rule:

```rust
for case in plan.cases {
    let verdict = executor.run_case(&job.key, &case, plan.output_token_cap.min(64)).await;
    match verdict {
        ProbeCaseVerdict::Unobserved { operational_code, http_status }
            if matches!(http_status, Some(401 | 403 | 429 | 500..=599) | None) =>
        {
            return ProbeOutcome::OperationalFailure {
                code: operational_code, http_status, attempted_at: unix_seconds(),
            };
        }
        other => evidence.apply(case, other),
    }
}
evidence.into_conclusive_outcome(unix_seconds())
```

Each HTTP call must:

- resolve the API key already mapped to the exact runtime model;
- call `try_reserve_upstream_request` and release concurrency with an RAII guard;
- use `max_tokens`, `max_completion_tokens`, or `max_output_tokens` only for the case testing that field;
- cap completion at 64 tokens;
- tag internal tracing as `compatibility_probe`;
- skip downstream quota and normal route-health success/failure mutation;
- never store response text, reasoning, image data, tool arguments, or credentials.

Positive tool evidence requires `gateway_compat_probe`, a matching call ID, parseable complete JSON arguments containing the nonce, and a successful assistant/tool continuation. Positive image evidence requires the expected label in a forced structured tool call; HTTP 200 alone is `Rejected`. Positive streaming evidence requires ordered start/delta/terminal events and at least one meaningful delta.

For `CoreProbeCase::Declarative(case)`, store its verdict in `ProbeOutcome::Conclusive.extension_evidence[case.id]`. Normalization may apply that case's request patch only when this exact profile entry is `Supported`.

The image request prompt asks the model to report the observed dominant color through a schema containing several valid colors; it never includes `DATA_URL_IMAGE_EXPECTED_LABEL` or the configured HTTPS expected label. Only the response validator receives the expected value.

- [ ] **Step 6: Add recognized dialect-error refresh scheduling**

Expose this non-blocking state method and call it from normal error classification only after a recognized field-level 400:

```rust
pub fn queue_capability_probe(&self, job: ProbeJob) -> bool {
    self.capability_probe_sender.lock().expect("probe sender lock poisoned")
        .as_ref().map(|sender| sender.try_send(job).is_ok()).unwrap_or(false)
}
```

This method only queues future work; the failing request never waits for it.

- [ ] **Step 7: Run probe and state tests**

Run: `rtk cargo test --test capability_probe --test capability_profiles --test probe_queue -- --nocapture`

Expected: PASS. The normal request test records one upstream hit and auth failure records one probe hit.

- [ ] **Step 8: Commit the background probe engine**

```bash
rtk git add src/server/gateway/capability_probe.rs src/server/gateway.rs src/server.rs src/state.rs src/state/types.rs src/main.rs tests/capability_probe.rs
rtk git commit -m "feat: probe exact upstream dialects in background"
```

### Task 6: Filter Routes By Features And Publish A Truthful Codex Catalog

**Files:**
- Create: `src/server/gateway/capability_routing.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/upstream.rs`
- Replace: `src/server/gateway/compat.rs`
- Modify: `tests/gateway/chat/compatibility.rs`
- Modify: `tests/gateway/compatibility.rs`
- Create: `tests/gateway/capability_routing.rs`
- Create: `tests/generic_dispatch.rs`

- [ ] **Step 1: Write failing requested-feature and route-witness tests**

Add `tests/gateway/capability_routing.rs` to `tests/gateway.rs` and create tests with two same-model routes:

```rust
#[tokio::test]
async fn namespace_request_chooses_verified_chat_adapter_over_restricted_responses() {
    let fixture = CapabilityRoutingFixture::new("opaque/model")
        .responses_route("responses-weak", profile_with([Capability::TextStream]))
        .chat_route("chat-tooling", profile_with([
            Capability::FunctionTools, Capability::NamespaceTools, Capability::ToolContinuation,
            Capability::ReasoningReplay, Capability::TextStream,
        ])).await;
    let response = fixture.send_codex_namespace_request().await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.hit_count("responses-weak"), 0);
    assert_eq!(fixture.hit_count("chat-tooling"), 1);
}

#[tokio::test]
async fn required_image_never_routes_to_text_only_candidate() {
    let fixture = CapabilityRoutingFixture::new("opaque/model")
        .chat_route("text-only", profile_with([Capability::TextStream, Capability::FunctionTools]))
        .await;
    let response = fixture.send_responses_image_request().await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response_json(response).await["error"]["code"],
        "gateway_protocol_capability_unsupported");
    assert_eq!(fixture.total_hits(), 0);
}

#[tokio::test]
async fn catalog_uses_one_deterministic_witness_not_union_or_intersection() {
    let fixture = CapabilityRoutingFixture::new("opaque/model")
        .chat_route("priority-low", verified_image_tool_profile())
        .chat_route("priority-high", verified_text_profile())
        .await;
    let model = fixture.codex_catalog_model().await;
    assert_eq!(model["gateway_catalog_witness"]["upstream_id"], "priority-low");
    assert_eq!(model["input_modalities"], serde_json::json!(["text", "image"]));
    assert_eq!(model["supports_parallel_tool_calls"], true);
    assert_eq!(model["web_search_tool_type"], serde_json::Value::Null);
}

#[tokio::test]
async fn continuation_is_pinned_unless_an_equivalent_profile_exists() {
    let fixture = CapabilityRoutingFixture::two_non_equivalent_profiles().await;
    let first = fixture.start_reasoning_tool_loop().await;
    let response = fixture.continue_with_previous_response_id(first.id).await;
    assert_eq!(response.headers()["x-chat2responses-upstream-id"], first.upstream_id);
}
```

Create `tests/generic_dispatch.rs`:

```rust
#[test]
fn production_compatibility_dispatch_has_no_model_or_hostname_classifier() {
    let source = include_str!("../src/server/gateway/compat.rs").to_ascii_lowercase();
    for forbidden in ["deepseek", "minimax", "glm", "qwen", "kimi", "moonshot",
        "api.openai.com", "openai.azure.com", "chatcompatibilityfamily"]
    {
        assert!(!source.contains(forbidden), "found forbidden production classifier {forbidden}");
    }
}
```

- [ ] **Step 2: Run routing tests and see the current optimistic behavior fail**

Run: `rtk cargo test --test gateway capability_routing -- --nocapture`

Expected: FAIL because route filtering uses only protocol/model and the catalog advertises fixed capabilities.

Run: `rtk cargo test --test generic_dispatch -- --nocapture`

Expected: FAIL on the existing model-family and official-hostname classifiers.

- [ ] **Step 3: Extract downstream features without flattening payloads**

Create `src/server/gateway/capability_routing.rs` with:

```rust
impl EndpointKind {
    fn wire_protocol(self) -> WireProtocol {
        match self {
            EndpointKind::ChatCompletions => WireProtocol::ChatCompletions,
            EndpointKind::Responses => WireProtocol::Responses,
        }
    }
}

pub fn requested_features(endpoint: EndpointKind, body: &Value)
    -> Result<RequestedFeatures, GatewayError>
{
    let mut features = RequestedFeatures::default();
    features.required.insert(Capability::TextInput);
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        features.required.insert(Capability::TextStream);
    }
    scan_images(endpoint, body, &mut features)?;
    scan_tools(endpoint, body, &mut features)?;
    scan_reasoning_continuation(endpoint, body, &mut features)?;
    scan_structured_output(endpoint, body, &mut features)?;
    Ok(features)
}
```

`scan_tools` must distinguish standard function, namespace, custom, hosted, and unknown kinds; mark explicit `tool_choice` as required; keep auto-selected hosted tools optional; and record continuation IDs without copying arguments. `scan_images` must distinguish HTTPS, Data URL, and native `file_id`. The function returns flags and identities only.

When `previous_response_id` loads `gateway_continuation`, copy only its `profile_key` and `reasoning_carrier` into `RequestedFeatures.continuation_profile` and `continuation_reasoning_carrier` before resolving candidates. Keep the full registry/history payload in `ResponseHistoryContext`, not in `RequestedFeatures`.

Move Responses history lookup ahead of protocol candidate selection so Chat fallback receives the same registry and reasoning state. Do not run the existing reduced-history fallback until a selected high-fidelity replay attempt actually fails and records its stage.

- [ ] **Step 4: Resolve and filter every candidate before pressure ranking**

For each active upstream/protocol/model candidate, compute the final runtime slug with `resolved_model_name`, build `DialectProfileKey`, resolve capabilities from the current immutable snapshot, and keep only candidates that can preserve or reversibly adapt all required features.

Construct `RouteIdentity` with an empty tag set, call `configuration.apply_route_tags(&mut identity)`, then resolve matching semantic policies and route overrides. Tags are administrator data and never derived from model text.
Pass `configuration.extensions_for(&identity)` into `ResolutionInput.policy_extensions`; the resolver includes only extensions with positive profile/override evidence and matching requested-feature prerequisites.

Intersect semantic ceilings with explicit route context configuration after resolution:

```rust
if let Some(route_context) = upstream.context_config_for_model(&exposed_model) {
    resolved.context_window = Some(resolved.context_window
        .map(|policy| policy.min(route_context.context_limit as u64))
        .unwrap_or(route_context.context_limit as u64));
    if route_context.max_output_tokens > 0 {
        resolved.max_output_tokens = Some(resolved.max_output_tokens
            .map(|policy| policy.min(route_context.max_output_tokens as u64))
            .unwrap_or(route_context.max_output_tokens as u64));
    }
    resolved.field_sources.insert("context_window".into(), CapabilitySource::Override);
    if route_context.max_output_tokens > 0 {
        resolved.field_sources.insert("max_output_tokens".into(), CapabilitySource::Override);
    }
}
```

Without policy or route values, keep the existing conservative catalog default; never infer a limit from the slug.

Use this candidate structure:

```rust
#[derive(Clone)]
pub struct CapabilityRouteCandidate {
    pub upstream: UpstreamConfig,
    pub runtime_model_slug: String,
    pub profile_key: DialectProfileKey,
    pub capabilities: Arc<ResolvedCapabilities>,
    pub fidelity_score: u16,
}
```

Sort first by native/adapted fidelity, then preserve the existing quota, priority, health, affinity, and tie-break behavior. A verified restricted Responses route must lose to a verified Chat route when the request needs an adapter it lacks. An unverified Responses route is never preferred over a viable Chat route.

If filtering removes every candidate, return before quota reservation or dispatch and shape the error for the downstream endpoint:

```rust
#[derive(Clone, Copy)]
enum CapabilityErrorEnvelope { OpenAi, Anthropic }

fn capability_error(envelope: CapabilityErrorEnvelope, capability: Capability) -> Response {
    let message = format!("selected routes cannot preserve required capability {capability:?}");
    match envelope {
        CapabilityErrorEnvelope::Anthropic => anthropic_error_response(
            StatusCode::BAD_REQUEST, "invalid_request_error",
            "gateway_protocol_capability_unsupported", &message),
        CapabilityErrorEnvelope::OpenAi => openai_error_response(
            StatusCode::BAD_REQUEST, "invalid_request_error",
            "gateway_protocol_capability_unsupported", &message),
    }
}
```

Chat and Responses handlers pass `OpenAi`; the Messages adapter passes `Anthropic` before invoking its internal Chat path.

- [ ] **Step 5: Replace model/hostname compatibility normalization with resolved data**

Replace `ChatCompatibilityFamily`, `chat_compatibility_family`, `glm_model_supports_reasoning_effort`, `is_likely_official_openai_chat_upstream`, and token-family functions in `src/server/gateway/compat.rs` with:

```rust
pub(super) fn normalize_chat_payload_for_capabilities(
    body: &mut Value,
    resolved: &ResolvedCapabilities,
) {
    let Some(object) = body.as_object_mut() else { return; };
    for field in &resolved.omit_sampling_fields { object.remove(field); }
    if resolved.omit_optional_extensions {
        for key in ["service_tier", "safety_identifier", "prompt_cache_key",
            "prompt_cache_retention", "client_metadata", "store", "verbosity",
            "metadata", "user", "text", "parallel_tool_calls"]
        {
            object.remove(key);
        }
    }
    if !resolved.supports(Capability::ParallelToolCalls) {
        object.remove("parallel_tool_calls");
    }
    if !resolved.supports(Capability::UsageStream) {
        object.remove("stream_options");
    }
    let requested_limit = object.remove("max_output_tokens")
        .or_else(|| object.remove("max_completion_tokens"))
        .or_else(|| object.remove("max_tokens"));
    if let Some(value) = requested_limit {
        let key = match resolved.token_limit_field {
            TokenLimitField::MaxTokens => Some("max_tokens"),
            TokenLimitField::MaxCompletionTokens => Some("max_completion_tokens"),
            TokenLimitField::MaxOutputTokens => Some("max_output_tokens"),
            TokenLimitField::Omit => None,
        };
        if let Some(key) = key { object.insert(key.into(), value); }
    }
    let requested_effort = object.remove("reasoning_effort")
        .and_then(|value| value.as_str().map(str::to_owned));
    if let (Some(field), Some(mapped)) = (resolved.reasoning_control_field.as_deref(),
        requested_effort.as_deref().and_then(|effort| resolved.effort_map.get(effort)))
    {
        object.insert(field.into(), Value::String(mapped.clone()));
    }
    for extension in &resolved.request_extensions {
        if let Some(patch) = extension.request_patch.as_object() {
            merge_optional_object(object, patch);
        }
    }
}

fn merge_optional_object(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        match (target.get_mut(key), value) {
            (Some(Value::Object(target)), Value::Object(patch)) =>
                merge_optional_object(target, patch),
            _ => { target.insert(key.clone(), value.clone()); }
        }
    }
}
```

The legacy `strip_nonstandard_chat_fields` flag feeds `omit_optional_extensions`; it never removes tools, images, reasoning replay, call IDs, or tool results.
Only extensions with positive exact-profile evidence reach `resolved.request_extensions`; policy data alone never calls `merge_optional_object`.
Factor the extension loop into `apply_resolved_request_extensions` and invoke it on the final selected-route body for both Chat Completions and Responses; keep token/reasoning baseline normalization protocol-specific.

Replace the existing GLM/DeepSeek/MiniMax/Qwen-named tests in `tests/gateway/chat/compatibility.rs` with arbitrary slugs plus explicit profile/override fixtures. Retain assertions for effort mapping, token-field selection, strict optional-field removal, tool preservation, and third-party base URLs; the base URL must no longer affect results.

- [ ] **Step 6: Select and expose a deterministic catalog witness**

For each exposed slug, select one route by:

1. verified routes with the largest executable capability set;
2. partial routes;
3. provisional routes with conservative metadata;
4. existing priority and health rank;
5. upstream ID as the stable final tie-breaker.

Build Codex metadata exclusively from that route's `ResolvedCapabilities`. Emit `input_modalities`, reasoning levels from the external effort map, context limits, parallel tools, structured output, reasoning summaries, shell type, and apply-patch type only when executable. Add bounded diagnostic metadata:

```json
{
  "gateway_catalog_witness": {
    "upstream_id": "upstream-id",
    "protocol": "chat_completions",
    "profile_state": "verified",
    "probe_version": 1
  }
}
```

Set hosted search to disabled/null. Do not advertise a capability assembled from multiple routes. Store the witness profile identity in request history, and constrain capability-using requests to the witness or a compatible superset.

- [ ] **Step 7: Run compatibility and catalog tests**

Run: `rtk cargo test --test gateway capability -- --nocapture`

Expected: PASS for feature filtering, witness metadata, and continuation pinning.

Run: `rtk cargo test --test generic_dispatch --test templates -- --nocapture`

Expected: PASS with no production slug/hostname classifier.

- [ ] **Step 8: Commit capability-aware dispatch**

```bash
rtk git add src/server/gateway.rs src/server/gateway/capability_routing.rs src/server/gateway/upstream.rs src/server/gateway/compat.rs tests/gateway.rs tests/gateway/capability_routing.rs tests/gateway/chat/compatibility.rs tests/gateway/compatibility.rs tests/generic_dispatch.rs
rtk git commit -m "feat: route and advertise by verified capabilities"
```

### Task 7: Preserve Image Inputs Across Responses, Chat, And Messages

**Files:**
- Create: `src/protocol/image_adapter.rs`
- Modify: `src/protocol.rs`
- Modify: `src/server/gateway/claude.rs`
- Modify: `src/server/gateway/capability_routing.rs`
- Modify: `tests/protocol.rs`
- Modify: `tests/gateway/claude.rs`
- Create: `tests/gateway/images.rs`

- [ ] **Step 1: Write failing lossless image round-trip tests**

Add protocol tests for all structural mappings:

```rust
#[test]
fn responses_chat_round_trip_preserves_url_detail_and_mixed_order() {
    let responses = json!({"model":"opaque","input":[{"role":"user","content":[
        {"type":"input_text","text":"before"},
        {"type":"input_image","image_url":"https://images.example/red.png","detail":"high"},
        {"type":"input_text","text":"after"}
    ]}]});
    let chat = responses_request_to_chat_payload(&responses).unwrap();
    assert_eq!(chat["messages"][0]["content"][1]["image_url"]["url"],
        "https://images.example/red.png");
    assert_eq!(chat["messages"][0]["content"][1]["image_url"]["detail"], "high");
    let round_trip = chat_request_to_responses_payload(&chat).unwrap();
    assert_eq!(round_trip["input"][0]["content"], responses["input"][0]["content"]);
}

#[test]
fn messages_base64_image_maps_to_mime_qualified_data_url_without_decode() {
    let block = json!({"type":"image","source":{
        "type":"base64","media_type":"image/png","data":"AAEC"}});
    let chat = messages_image_to_chat_part(&block, ImageDialect::all()).unwrap();
    assert_eq!(chat.value["image_url"]["url"],
        "data:image/png;base64,AAEC");
}

#[test]
fn messages_url_image_maps_to_chat_url_without_fetch() {
    let block = json!({"type":"image","source":{
        "type":"url","url":"https://images.example/shape.png"}});
    let chat = messages_image_to_chat_part(&block, ImageDialect::all()).unwrap();
    assert_eq!(chat.value["image_url"]["url"],
        "https://images.example/shape.png");
}
```

Add gateway tests proving unsupported images and cross-provider `file_id` return `gateway_protocol_capability_unsupported` before an upstream hit, while unsupported optional `detail` preserves the image and emits `x-chat2responses-downgrade: optional_image_detail`.

- [ ] **Step 2: Run image tests and confirm Messages/detail loss**

Run: `rtk cargo test --test protocol image -- --nocapture`

Expected: FAIL on nested detail and missing Messages image conversion.

Run: `rtk cargo test --test gateway images -- --nocapture`

Expected: FAIL because capability rejection/downgrade is not wired.

- [ ] **Step 3: Implement a focused structural image adapter**

Create `src/protocol/image_adapter.rs` and include it from `src/protocol.rs`:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageSource {
    HttpsUrl(String),
    DataUrl(String),
    NativeFileId(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImagePart { pub source: ImageSource, pub detail: Option<String> }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageDialect {
    pub https_url: bool,
    pub data_url: bool,
    pub detail: bool,
    pub native_file_id: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageAdaptation { pub value: serde_json::Value, pub downgrade: Option<String> }

impl ImageDialect {
    pub fn all() -> Self { Self { https_url: true, data_url: true, detail: true,
        native_file_id: false } }
}

pub fn parse_data_url(value: &str) -> Result<(&str, &str), ProtocolError> {
    let rest = value.strip_prefix("data:").ok_or(ProtocolError::UnsupportedImageSource)?;
    let (media, data) = rest.split_once(";base64,")
        .ok_or(ProtocolError::UnsupportedImageSource)?;
    if !media.starts_with("image/") || data.is_empty() {
        return Err(ProtocolError::UnsupportedImageSource);
    }
    Ok((media, data))
}

pub fn messages_image_to_chat_part(block: &serde_json::Value, dialect: ImageDialect)
    -> Result<ImageAdaptation, ProtocolError>
{
    let source = block.get("source").and_then(serde_json::Value::as_object)
        .ok_or(ProtocolError::MissingField("source"))?;
    let image = match source.get("type").and_then(serde_json::Value::as_str) {
        Some("url") => ImagePart {
            source: classify_url(source.get("url").and_then(serde_json::Value::as_str)
                .ok_or(ProtocolError::MissingField("url"))?)?,
            detail: None,
        },
        Some("base64") => {
            let media = source.get("media_type").and_then(serde_json::Value::as_str)
                .ok_or(ProtocolError::MissingField("media_type"))?;
            let data = source.get("data").and_then(serde_json::Value::as_str)
                .ok_or(ProtocolError::MissingField("data"))?;
            let value = format!("data:{media};base64,{data}");
            parse_data_url(&value)?;
            ImagePart { source: ImageSource::DataUrl(value), detail: None }
        }
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    emit_chat_image(&image, dialect)
}

pub fn chat_image_to_responses_part(part: &serde_json::Value, dialect: ImageDialect)
    -> Result<ImageAdaptation, ProtocolError>
{
    let image_url = part.get("image_url").ok_or(ProtocolError::MissingField("image_url"))?;
    let (url, detail) = match image_url {
        serde_json::Value::String(url) => (url.as_str(), None),
        serde_json::Value::Object(object) => (
            object.get("url").and_then(serde_json::Value::as_str)
                .ok_or(ProtocolError::MissingField("url"))?,
            object.get("detail").and_then(serde_json::Value::as_str).map(str::to_owned),
        ),
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    emit_responses_image(&ImagePart { source: classify_url(url)?, detail }, dialect)
}

pub fn responses_image_to_chat_part(part: &serde_json::Value, dialect: ImageDialect)
    -> Result<ImageAdaptation, ProtocolError>
{
    let url = part.get("image_url").and_then(serde_json::Value::as_str)
        .ok_or(ProtocolError::MissingField("image_url"))?;
    let detail = part.get("detail").and_then(serde_json::Value::as_str).map(str::to_owned);
    emit_chat_image(&ImagePart { source: classify_url(url)?, detail }, dialect)
}

fn classify_url(value: &str) -> Result<ImageSource, ProtocolError> {
    if value.starts_with("https://") { return Ok(ImageSource::HttpsUrl(value.to_owned())); }
    if value.starts_with("data:") { parse_data_url(value)?; return Ok(ImageSource::DataUrl(value.to_owned())); }
    Err(ProtocolError::UnsupportedImageSource)
}

fn emit_chat_image(image: &ImagePart, dialect: ImageDialect)
    -> Result<ImageAdaptation, ProtocolError>
{
    let url = match &image.source {
        ImageSource::HttpsUrl(value) if dialect.https_url => value,
        ImageSource::DataUrl(value) if dialect.data_url => value,
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    let mut nested = serde_json::json!({"url":url});
    let downgrade = if let Some(detail) = image.detail.as_deref() {
        if dialect.detail {
            nested.as_object_mut().unwrap().insert("detail".into(), detail.into());
            None
        } else { Some("optional_image_detail".into()) }
    } else { None };
    Ok(ImageAdaptation { value: serde_json::json!({"type":"image_url","image_url":nested}),
        downgrade })
}

fn emit_responses_image(image: &ImagePart, dialect: ImageDialect)
    -> Result<ImageAdaptation, ProtocolError>
{
    let url = match &image.source {
        ImageSource::HttpsUrl(value) if dialect.https_url => value,
        ImageSource::DataUrl(value) if dialect.data_url => value,
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    let mut value = serde_json::json!({"type":"input_image","image_url":url});
    let downgrade = if let Some(detail) = image.detail.as_deref() {
        if dialect.detail {
            value.as_object_mut().unwrap().insert("detail".into(), detail.into());
            None
        } else { Some("optional_image_detail".into()) }
    } else { None };
    Ok(ImageAdaptation { value, downgrade })
}
```

Add a payload-free `UnsupportedImageSource` variant to `ProtocolError`, implement its display text as `unsupported image source`, and export `image_adapter` from `src/protocol.rs`. The functions may validate Data URL structure but must never Base64-decode/re-encode or fetch HTTPS URLs. Preserve array order. Native `file_id` is passed only by a capability-verified native route and never emitted into another protocol. Only remove `detail` when the selected dialect lacks it.

- [ ] **Step 4: Wire image capabilities into feature extraction and route conversion**

Map profile evidence to `ImageDialect`. Reject an HTTPS or Data URL image unless the selected route has `Supported` evidence or an explicit route override. Reject `file_id` unless the native route has positive `native_file_id` evidence for the same identifier domain. Pass the selected dialect to the pairwise converter and Claude Messages adapter.

```rust
impl ImageDialect {
    pub fn from_resolved(resolved: &ResolvedCapabilities) -> Self {
        Self {
            https_url: resolved.supports(Capability::ImageHttps),
            data_url: resolved.supports(Capability::ImageDataUrl),
            detail: resolved.supports(Capability::ImageDetail),
            native_file_id: resolved.supports(Capability::NativeFileId),
        }
    }
}

let downstream_wire_protocol = endpoint.wire_protocol();
if requested.required.contains(&Capability::NativeFileId)
    && (!dialect.native_file_id || selected_route.profile_key.protocol != downstream_wire_protocol)
{
    return Err(CapabilityResolutionError { capability: Capability::NativeFileId });
}
```

Never include URL/data content in error strings, downgrade headers, tracing fields, or usage logs.

- [ ] **Step 5: Run protocol, Claude, and gateway image tests**

Run: `rtk cargo test --test protocol image -- --nocapture`

Expected: PASS for URL, Data URL, MIME type, detail, and ordering.

Run: `rtk cargo test --test gateway image -- --nocapture`

Expected: PASS with zero upstream hits for unsupported required images.

- [ ] **Step 6: Commit the image bridge**

```bash
rtk git add src/protocol.rs src/protocol/image_adapter.rs src/server/gateway/claude.rs src/server/gateway/capability_routing.rs tests/protocol.rs tests/gateway.rs tests/gateway/claude.rs tests/gateway/images.rs
rtk git commit -m "feat: preserve image input across agent protocols"
```

### Task 8: Add A Reversible Namespace And Custom Tool Registry

**Files:**
- Create: `src/protocol/tool_adapter.rs`
- Modify: `src/protocol.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/capability_probe.rs`
- Modify: `src/server/gateway/stream.rs`
- Modify: `src/server/gateway/capability_routing.rs`
- Modify: `tests/protocol.rs`
- Create: `tests/tool_adapter.rs`
- Create: `tests/gateway/responses/tools.rs`
- Modify: `tests/gateway/responses.rs`

- [ ] **Step 1: Write failing deterministic registry and round-trip tests**

Create `tests/tool_adapter.rs`:

```rust
use chat_responses_codex::protocol::tool_adapter::*;
use serde_json::json;

#[test]
fn namespace_mapping_is_ascii_bounded_deterministic_and_reversible() {
    let tools = json!([
        {"type":"function","name":"gw_taken","description":"top","parameters":{"type":"object"}},
        {"type":"namespace","name":"mcp__docs","description":"Developer docs","tools":[
            {"type":"function","name":"search/reference with spaces","description":"search",
             "parameters":{"type":"object","properties":{"q":{"type":"string"}}}}
        ]}
    ]);
    let adapted = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    let identity = ToolIdentity::namespace("mcp__docs", "search/reference with spaces");
    let generated = adapted.registry.upstream_name(&identity).unwrap();
    assert!(generated.starts_with("gw_"));
    assert!(generated.len() <= 64);
    assert!(generated.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
    let restored = adapted.registry.restore_function_call(&json!({
        "id":"call_1","type":"function","function":{"name":generated,"arguments":"{\"q\":\"x\"}"}
    })).unwrap();
    assert_eq!(restored["namespace"], "mcp__docs");
    assert_eq!(restored["name"], "search/reference with spaces");
    assert_eq!(restored["call_id"], "call_1");
}

#[test]
fn generated_name_collision_extends_digest_without_changing_identity() {
    let first = ToolIdentity::namespace("n", "member");
    let occupied = generated_name(&first, 12, &std::collections::BTreeSet::new());
    let registry = ToolAdapterRegistry::from_identities(
        vec![ToolIdentity::function(&occupied), first.clone()]).unwrap();
    let mapped = registry.upstream_name(&first).unwrap();
    assert_ne!(mapped, occupied);
    assert!(mapped.len() <= 64);
    assert_eq!(registry.identity(mapped), Some(&first));
}

#[test]
fn custom_tool_uses_single_required_input_string_and_restores_raw_input() {
    let tools = json!([{"type":"custom","name":"apply_patch","description":"patch"}]);
    let adapted = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    assert_eq!(adapted.upstream_tools[0]["function"]["parameters"]["required"], json!(["input"]));
    let call = adapted.registry.restore_function_call(&json!({
        "id":"call_patch","type":"function",
        "function":{"name":adapted.upstream_tools[0]["function"]["name"],
                    "arguments":"{\"input\":\"*** Begin Patch\"}"}
    })).unwrap();
    assert_eq!(call["type"], "custom_tool_call");
    assert_eq!(call["input"], "*** Begin Patch");
}
```

Add JSON and SSE gateway tests that send the verified Codex namespace shape, require the restored `name` plus `namespace`, replay the call output, and assert argument bytes/call IDs are unchanged. Add hosted-tool tests for auto downgrade, explicit rejection, and unknown-kind rejection.

- [ ] **Step 2: Run tool tests and confirm current fallback strips tool kinds**

Run: `rtk cargo test --test tool_adapter -- --nocapture`

Expected: FAIL because the registry does not exist.

Run: `rtk cargo test --test gateway responses_tool -- --nocapture`

Expected: FAIL because namespace/custom tools are currently removed during fallback.

- [ ] **Step 3: Implement deterministic names and serializable mappings**

Create `src/protocol/tool_adapter.rs`:

```rust
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind { Function, NamespaceMember, Custom }

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ToolIdentity {
    pub kind: ToolKind,
    pub namespace: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolMapping { pub identity: ToolIdentity, pub upstream_name: String }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolAdapterRegistry { pub version: u32, pub mappings: Vec<ToolMapping> }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolTarget { NativeResponses, RestrictedResponses, FunctionsOnly }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolAdaptation {
    pub upstream_tools: Vec<serde_json::Value>,
    pub registry: ToolAdapterRegistry,
    pub downgrades: Vec<String>,
}

impl ToolIdentity {
    pub fn function(name: &str) -> Self {
        Self { kind: ToolKind::Function, namespace: None, name: name.to_owned() }
    }
    pub fn namespace(namespace: &str, name: &str) -> Self {
        Self { kind: ToolKind::NamespaceMember, namespace: Some(namespace.to_owned()),
            name: name.to_owned() }
    }
    pub fn custom(namespace: Option<&str>, name: &str) -> Self {
        Self { kind: ToolKind::Custom, namespace: namespace.map(str::to_owned),
            name: name.to_owned() }
    }
}

fn identity_bytes(identity: &ToolIdentity) -> Vec<u8> {
    let kind = match identity.kind { ToolKind::Function => "function",
        ToolKind::NamespaceMember => "namespace", ToolKind::Custom => "custom" };
    [kind.as_bytes(), b"\0", identity.namespace.as_deref().unwrap_or("").as_bytes(),
        b"\0", identity.name.as_bytes()].concat()
}

fn sanitized_middle(identity: &ToolIdentity) -> String {
    let raw = match identity.namespace.as_deref() {
        Some(namespace) => format!("{namespace}__{}", identity.name),
        None => identity.name.clone(),
    };
    let mut output = String::new();
    let mut replacing = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            output.push(ch); replacing = false;
        } else if !replacing {
            output.push('_'); replacing = true;
        }
    }
    let trimmed = output.trim_matches(['_', '-']);
    if trimmed.is_empty() { "tool".into() } else { trimmed.into() }
}

pub fn generated_name(identity: &ToolIdentity, digest_len: usize,
    occupied: &std::collections::BTreeSet<String>) -> String
{
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(identity_bytes(identity)));
    let sanitized = sanitized_middle(identity);
    const MAX_SUFFIX: usize = 56;
    let mut length = digest_len.max(12).min(MAX_SUFFIX);
    loop {
        let suffix = &digest[..length];
        let max_middle = 64usize.saturating_sub(4 + suffix.len());
        let middle = &sanitized[..sanitized.len().min(max_middle)];
        let candidate = format!("gw_{middle}_{suffix}");
        if !occupied.contains(&candidate) || length == MAX_SUFFIX { return candidate; }
        length = (length + 4).min(MAX_SUFFIX);
    }
}
```

Sort identities before assignment. Sanitize `namespace + "__" + member` by replacing each run outside `[A-Za-z0-9_-]` with `_`, trim separators, and use `tool` if empty. Prefix `gw_`, append the first 12 lowercase SHA-256 hex characters, and keep the full name at most 64 ASCII bytes. On collision, extend the hex suffix by four characters and shorten the middle. The `gw_` prefix, `tool` minimum middle, and separator leave 56 bytes for the largest possible suffix; retain the full 64-hex digest in the registry for collision comparison, and fail before dispatch if the 56-character candidate is still occupied by a different identity. Treat caller names, including caller-owned `gw_` names, as occupied.

- [ ] **Step 4: Adapt namespace, custom, choice, calls, and outputs**

Implement these registry operations with structured JSON parsing:

```rust
pub trait ReversibleToolAdapter: Sized {
    fn build(tools: &Value, target: ToolTarget) -> Result<ToolAdaptation, ProtocolError>;
    fn from_identities(identities: Vec<ToolIdentity>) -> Result<Self, ProtocolError>;
    fn adapt_tool_choice(&self, choice: &Value) -> Result<Value, ProtocolError>;
    fn restore_function_call(&self, call: &Value) -> Result<Value, ProtocolError>;
    fn adapt_call_output(&self, output: &Value) -> Result<Value, ProtocolError>;
    fn restore_streamed_call_name(&self, upstream_name: &str)
        -> Result<&ToolIdentity, ProtocolError>;
    fn upstream_name(&self, identity: &ToolIdentity) -> Option<&str>;
    fn identity(&self, upstream_name: &str) -> Option<&ToolIdentity>;
}
```

Namespace descriptions must prefix the member description. Custom functions use exactly one required string property named `input`; parse the function argument object and restore its string as raw custom input. Keep function arguments and call IDs byte-for-byte. Map namespace `tool_choice` and custom continuation outputs through the stored mapping.

- [ ] **Step 5: Apply explicit hosted and unknown tool policy**

When the selected route lacks the exact hosted capability:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolPolicyDecision {
    Keep,
    DropOptional { downgrade: String },
    Reject { category: &'static str },
}

pub fn hosted_tool_decision(kind: &str, route_supports_kind: bool,
    explicitly_selected: bool, executable_tool_count_after_drop: usize) -> ToolPolicyDecision
{
    let known = matches!(kind, "web_search" | "file_search" | "computer_use");
    if !known {
        return ToolPolicyDecision::Reject {
            category: "gateway_protocol_capability_unsupported" };
    }
    if route_supports_kind { return ToolPolicyDecision::Keep; }
    if explicitly_selected || executable_tool_count_after_drop == 0 {
        ToolPolicyDecision::Reject { category: "gateway_protocol_capability_unsupported" }
    } else {
        ToolPolicyDecision::DropOptional { downgrade: format!("optional_tool:{kind}") }
    }
}
```

- remove `web_search`, `file_search`, or `computer_use` only when optional under auto choice;
- append `optional_tool:<kind>` to the downgrade collector;
- reject an explicitly selected or last-required hosted tool with OpenAI-shaped HTTP 400;
- reject every unknown tool type with `gateway_protocol_capability_unsupported`.

Do not emulate hosted execution. Set generated Codex presets to `web_search = "disabled"` in Task 13.

- [ ] **Step 6: Carry registry through JSON, SSE, and continuation**

Build the registry after selecting the route. Pass it into `responses_request_to_chat_payload_with_context`, non-stream response conversion, and `StreamTranslator`. Restore the original namespace/custom identity before emitting `response.output_item.added`, so no generated name reaches Codex.

Add one focused converter context in `src/protocol.rs`:

```rust
#[derive(Clone)]
pub struct ConversionContext {
    pub image_dialect: ImageDialect,
    pub reasoning_carrier: ReasoningCarrier,
    pub tool_registry: ToolAdapterRegistry,
}

impl ConversionContext {
    pub fn new(resolved: &ResolvedCapabilities, tool_registry: ToolAdapterRegistry) -> Self {
        Self { image_dialect: ImageDialect::from_resolved(resolved),
            reasoning_carrier: resolved.reasoning_carrier, tool_registry }
    }

    pub fn reasoning_content() -> Self {
        Self { image_dialect: ImageDialect::all(),
            reasoning_carrier: ReasoningCarrier::ReasoningContent,
            tool_registry: ToolAdapterRegistry { version: 1, mappings: Vec::new() } }
    }
}
```

Persist this object under the internal response-history state key `gateway_tool_registry`:

```json
{
  "version": 1,
  "mappings": [
    {
      "identity": {"kind":"namespace_member","namespace":"mcp__docs","name":"search"},
      "upstream_name":"gw_mcp__docs__search_0123456789ab"
    }
  ]
}
```

On `previous_response_id`, deserialize and reuse the same registry rather than recomputing against the new tool list.

Extend `CoreProbeCase` with `NamespaceAdapter` and `CustomAdapter` now that the registry exists. Each case passes only when the registry restores the original identity and linked output continuation succeeds:

```rust
match case {
    CoreProbeCase::NamespaceAdapter => evidence.record_capability(
        Capability::NamespaceTools, run_namespace_adapter_probe(executor).await),
    CoreProbeCase::CustomAdapter => evidence.record_capability(
        Capability::CustomTools, run_custom_adapter_probe(executor).await),
    _ => run_existing_core_probe(case, executor, evidence).await,
}
```

Standard function success alone must not set either capability. Only positive `CustomAdapter` evidence enables non-null `apply_patch_tool_type` in the Codex catalog.

- [ ] **Step 7: Run registry, Responses JSON/SSE, and history tests**

Run: `rtk cargo test --test tool_adapter -- --nocapture`

Expected: PASS for collision, length, character set, namespace restoration, and custom input.

Run: `rtk cargo test --test gateway namespace -- --nocapture`

Expected: PASS on JSON, SSE, choice, output continuation, and `previous_response_id` reuse.

- [ ] **Step 8: Commit reversible tool adaptation**

```bash
rtk git add src/protocol.rs src/protocol/tool_adapter.rs src/server/gateway.rs src/server/gateway/capability_probe.rs src/server/gateway/stream.rs src/server/gateway/capability_routing.rs tests/protocol.rs tests/tool_adapter.rs tests/gateway.rs tests/gateway/responses.rs tests/gateway/responses/tools.rs
rtk git commit -m "feat: preserve namespace and custom tool loops"
```

### Task 9: Preserve Reasoning Through Responses And Chat Tool Loops

**Files:**
- Create: `src/protocol/reasoning_adapter.rs`
- Modify: `src/protocol.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/stream.rs`
- Modify: `src/server/gateway/upstream.rs`
- Modify: `tests/protocol.rs`
- Create: `tests/gateway/responses/reasoning.rs`
- Modify: `tests/gateway/responses.rs`
- Modify: `tests/gateway/responses/history.rs`

- [ ] **Step 1: Write failing non-streaming reasoning and replay tests**

Add this converter test to `tests/protocol.rs`:

```rust
#[test]
fn chat_reasoning_content_becomes_responses_reasoning_before_function_call() {
    let chat = json!({
        "id":"chatcmpl-reasoning","model":"opaque","choices":[{"index":0,
        "message":{"role":"assistant","content":null,"reasoning_content":"exact-thought",
        "tool_calls":[{"id":"call_7","type":"function","function":{
            "name":"lookup","arguments":"{\"key\":\"value\"}"}}]},
        "finish_reason":"tool_calls"}]
    });
    let response = chat_response_to_responses_payload_with_context(
        &chat, &ConversionContext::reasoning_content()).unwrap();
    assert_eq!(response["output"][0]["type"], "reasoning");
    assert_eq!(response["output"][0]["content"][0]["type"], "reasoning_text");
    assert_eq!(response["output"][0]["content"][0]["text"], "exact-thought");
    assert_eq!(response["output"][1]["type"], "function_call");
}

#[test]
fn responses_reasoning_and_call_replay_merge_into_one_chat_assistant_message() {
    let responses = json!({"model":"opaque","input":[
        {"type":"reasoning","id":"rs_7","content":[{"type":"reasoning_text","text":"exact-thought"}]},
        {"type":"function_call","call_id":"call_7","name":"lookup","arguments":"{\"key\":\"value\"}"},
        {"type":"function_call_output","call_id":"call_7","output":"result"}
    ]});
    let chat = responses_request_to_chat_payload_with_context(
        &responses, &ConversionContext::reasoning_content()).unwrap();
    assert_eq!(chat["messages"][0]["reasoning_content"], "exact-thought");
    assert_eq!(chat["messages"][0]["tool_calls"][0]["id"], "call_7");
    assert_eq!(chat["messages"][1]["tool_call_id"], "call_7");
}
```

Add `tests/gateway/responses/reasoning.rs` tests for `previous_response_id`: with an otherwise unobserved Chat profile, the first mock response emits the exact `reasoning_content` field plus a function call; the second captured request must stay on the same profile and contain the exact decoded reasoning string, call ID, and tool output. Add a separate negative initial-request test where external policy requires reasoning replay but neither a verified profile nor continuation observation exists; it must fail before dispatch.

- [ ] **Step 2: Write failing official reasoning SSE event test**

Feed fragmented Chat chunks containing `delta.reasoning_content`, then a function call, into the Responses translator and assert this ordered subsequence:

```rust
let event_types = parse_sse(&body).into_iter()
    .filter_map(|event| event.data.get("type").and_then(Value::as_str).map(str::to_owned))
    .collect::<Vec<_>>();
assert_subsequence(&event_types, &[
    "response.output_item.added",
    "response.reasoning_text.delta",
    "response.reasoning_text.done",
    "response.output_item.done",
    "response.output_item.added",
    "response.function_call_arguments.delta",
    "response.function_call_arguments.done",
    "response.output_item.done",
    "response.completed",
]);
```

Also assert each reasoning delta has `item_id`, `output_index`, `content_index`, and raw `delta`, and that downstream receives the first reasoning delta before the mock finishes its response.

- [ ] **Step 3: Run reasoning tests and observe missing items/replay**

Run: `rtk cargo test --test protocol reasoning -- --nocapture`

Expected: FAIL because Chat reasoning is not emitted as a Responses item.

Run: `rtk cargo test --test gateway reasoning -- --nocapture`

Expected: FAIL because history omits the reasoning carrier and stream events.

- [ ] **Step 4: Implement the focused reasoning adapter**

Create `src/protocol/reasoning_adapter.rs`:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningItem {
    pub id: String,
    pub text: String,
}

pub fn chat_reasoning_item(message: &Value, id: String, carrier: ReasoningCarrier)
    -> Result<Option<(ReasoningCarrier, Value)>, ProtocolError>
{
    if !matches!(carrier, ReasoningCarrier::None | ReasoningCarrier::ReasoningContent) {
        return Ok(None);
    }
    let Some(text) = message.get("reasoning_content").and_then(Value::as_str) else {
        return Ok(None);
    };
    if text.is_empty() { return Ok(None); }
    Ok(Some((ReasoningCarrier::ReasoningContent, json!({
        "id": id,
        "type": "reasoning",
        "summary": [],
        "content": [{"type":"reasoning_text","text":text}]
    }))))
}

pub fn responses_reasoning_text(item: &Value) -> Result<Option<String>, ProtocolError> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") { return Ok(None); }
    let content = item.get("content").and_then(Value::as_array)
        .ok_or(ProtocolError::MissingField("content"))?;
    let mut text = String::new();
    for part in content {
        if part.get("type").and_then(Value::as_str) == Some("reasoning_text") {
            text.push_str(part.get("text").and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("text"))?);
        }
    }
    Ok(Some(text))
}
```

Use only the profile's registered `ReasoningCarrier` or the exact conservative observation described next. Never infer a carrier from model name or copy reasoning into ordinary assistant text.

`ReasoningCarrier::None` permits one conservative observation only: if the actual Chat response contains the exact generic `reasoning_content` field, preserve it and return `ReasoningCarrier::ReasoningContent` for this response's continuation state. This does not update the route profile or catalog; a background replay probe remains required for general advertisement.

- [ ] **Step 5: Merge replay items with their associated assistant calls**

Extend the pending Responses-to-Chat assistant accumulator with `reasoning_content: Option<String>`. A reasoning item starts or extends the pending assistant. Following function/custom calls remain in that assistant message. Flush only before a tool output, new user/system input, or end of input.

Use this output shape for the initial supported carrier:

```rust
{
    let mut assistant = json!({"role":"assistant","content":Value::Null,
        "tool_calls": pending.tool_calls});
    if let Some(reasoning) = pending.reasoning_content {
        assistant.as_object_mut().unwrap().insert(
            "reasoning_content".into(), Value::String(reasoning));
    }
    messages.push(assistant);
}
```

Preserve the decoded Rust string exactly; do not normalize whitespace or escape sequences beyond JSON serialization.

- [ ] **Step 6: Emit incremental official Responses reasoning events**

Add a `ReasoningStreamState` to `StreamTranslator` containing item ID, output index, accumulated text for the final item, and started/done flags. Emit deltas immediately. Close the reasoning item before opening its associated function-call item. Do not buffer tool arguments or text behind reasoning completion.

Store the completed reasoning output item in `ResponseHistoryContext`. Serialize this exact private state under `gateway_continuation`, and remove that internal key before any upstream payload is built:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
struct GatewayContinuationState {
    profile_key: DialectProfileKey,
    profile_fingerprint: String,
    capability_policy_revision: u64,
    capability_policy_digest: String,
    capability_policy_ids: Vec<String>,
    adapter_types: Vec<String>,
    tool_registry: ToolAdapterRegistry,
    reasoning_carrier: ReasoningCarrier,
    fallback_stage: FallbackStage,
}
```

On `previous_response_id`, require the same profile or an equivalent profile with the same fingerprint, reasoning carrier, and registry semantics. `HistoryReduced` remains possible only through the existing explicit fallback path and must be reported as a warning.

- [ ] **Step 7: Run JSON, SSE, and continuation tests**

Run: `rtk cargo test --test protocol reasoning -- --nocapture`

Expected: PASS for item ordering and exact replay.

Run: `rtk cargo test --test gateway reasoning -- --nocapture`

Expected: PASS for non-stream, stream, direct input replay, and `previous_response_id` replay.

- [ ] **Step 8: Commit reasoning continuity**

```bash
rtk git add src/protocol.rs src/protocol/reasoning_adapter.rs src/server/gateway.rs src/server/gateway/stream.rs src/server/gateway/upstream.rs tests/protocol.rs tests/gateway/responses.rs tests/gateway/responses/reasoning.rs tests/gateway/responses/history.rs
rtk git commit -m "feat: preserve reasoning through codex tool loops"
```

### Task 10: Bridge Claude Adaptive Thinking With Authenticated Replay

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/server.rs`
- Modify: `src/server/gateway/claude.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/capability_routing.rs`
- Create: `src/server/gateway/thinking_signature.rs`
- Modify: `tests/gateway/claude.rs`
- Create: `tests/thinking_signature.rs`
- Create: `tests/fixtures/clients/claude-code-2.1.195-messages.json`

- [ ] **Step 1: Write failing signature unit tests**

Create `tests/thinking_signature.rs`:

```rust
use chat_responses_codex::server::{
    sign_thinking, verify_thinking, ThinkingSignatureInput,
};

static CALL_IDS: [&str; 2] = ["toolu_1", "toolu_2"];

fn input<'a>(thinking: &'a str) -> ThinkingSignatureInput<'a> {
    ThinkingSignatureInput {
        thinking,
        model: "opaque-runtime",
        upstream_id: "up-7",
        protocol: "chat_completions",
        profile_fingerprint: "profile-sha256",
        call_ids: &CALL_IDS,
    }
}

#[test]
fn gateway_signature_is_stable_opaque_and_bound_to_every_replay_field() {
    let secret = b"test-jwt-secret";
    let signature = sign_thinking(secret, &input("exact thought"));
    assert!(signature.starts_with("gw1."));
    assert!(!signature.contains("exact thought"));
    assert!(verify_thinking(secret, &input("exact thought"), &signature));
    assert!(!verify_thinking(secret, &input("changed"), &signature));
    let mut changed = input("exact thought"); changed.model = "other-runtime";
    assert!(!verify_thinking(secret, &changed, &signature));
}
```

- [ ] **Step 2: Write failing Messages JSON/SSE and replay tests**

Add Claude tests using the sanitized `2.1.195` structure:

```rust
#[tokio::test]
async fn adaptive_thinking_maps_effort_and_emits_signed_block_before_tool_use() {
    let fixture = ClaudeThinkingFixture::verified().await;
    let response = fixture.send(json!({
        "model":"opaque-public","max_tokens":32000,"stream":true,
        "thinking":{"type":"adaptive"},"output_config":{"effort":"high"},
        "context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]},
        "messages":[{"role":"user","content":"use the read tool"}],
        "tools":[{"name":"Read","description":"read","input_schema":{"type":"object"}}]
    })).await;
    let events = parse_anthropic_sse(response).await;
    assert_thinking_signature_then_tool_use(&events);
    assert_eq!(fixture.upstream_request()["reasoning_effort"], "high");
}

#[tokio::test]
async fn valid_signed_thinking_and_tool_result_restore_exact_chat_replay() {
    let fixture = ClaudeThinkingFixture::verified().await;
    let first = fixture.first_tool_response().await;
    let second = fixture.replay_with_tool_result(first.thinking, first.signature, first.tool_id).await;
    assert_eq!(second.status(), StatusCode::OK);
    let upstream = fixture.last_upstream_request();
    assert_eq!(upstream["messages"][1]["reasoning_content"], first.thinking);
    assert_eq!(upstream["messages"][1]["tool_calls"][0]["id"], first.tool_id);
    assert_eq!(upstream["messages"][2]["tool_call_id"], first.tool_id);
}

#[tokio::test]
async fn modified_or_foreign_thinking_signature_fails_before_dispatch() {
    let fixture = ClaudeThinkingFixture::verified().await;
    let response = fixture.replay_with_tool_result("modified", "gw1.invalid", "toolu_1").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(fixture.upstream_hits(), 0);
}
```

- [ ] **Step 3: Run signature and Claude tests**

Run: `rtk cargo test --test thinking_signature -- --nocapture`

Expected: FAIL because HMAC signing is not implemented.

Run: `rtk cargo test --test gateway claude_messages_stream_translates_reasoning -- --nocapture`

Expected: FAIL because the current stream has no signature and request replay discards thinking blocks.

- [ ] **Step 4: Implement domain-separated HMAC signatures**

Add direct `hmac = "0.12"`, `sha2 = "0.10"`, and `subtle = "2"` dependencies to the root `Cargo.toml`. Declare `pub(super) mod thinking_signature;` in the gateway and re-export only `sign_thinking`, `verify_thinking`, and `ThinkingSignatureInput` from `src/server.rs` for focused integration tests.

Create `src/server/gateway/thinking_signature.rs`:

```rust
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub struct ThinkingSignatureInput<'a> {
    pub thinking: &'a str,
    pub model: &'a str,
    pub upstream_id: &'a str,
    pub protocol: &'a str,
    pub profile_fingerprint: &'a str,
    pub call_ids: &'a [&'a str],
}

fn update_len_prefixed(mac: &mut Hmac<Sha256>, value: &[u8]) {
    mac.update(&(value.len() as u64).to_be_bytes());
    mac.update(value);
}

pub fn sign_thinking(secret: &[u8], input: &ThinkingSignatureInput<'_>) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"chat2responses/claude-thinking/v1\0");
    for value in [input.thinking, input.model, input.upstream_id, input.protocol,
        input.profile_fingerprint]
    {
        update_len_prefixed(&mut mac, value.as_bytes());
    }
    mac.update(&(input.call_ids.len() as u64).to_be_bytes());
    for call_id in input.call_ids { update_len_prefixed(&mut mac, call_id.as_bytes()); }
    format!("gw1.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

pub fn verify_thinking(secret: &[u8], input: &ThinkingSignatureInput<'_>, signature: &str) -> bool {
    let Some(encoded) = signature.strip_prefix("gw1.") else { return false; };
    let Ok(expected) = URL_SAFE_NO_PAD.decode(encoded) else { return false; };
    let generated = sign_thinking(secret, input);
    let Some(generated) = generated.strip_prefix("gw1.") else { return false; };
    let Ok(generated) = URL_SAFE_NO_PAD.decode(generated) else { return false; };
    use subtle::ConstantTimeEq;
    expected.as_slice().ct_eq(generated.as_slice()).into()
}
```

Use `AppConfig.jwt_secret` as the stable signing secret with the explicit HMAC domain separator. Emit a startup warning when the development default is used, document that production requires a strong secret, and document that rotating it invalidates outstanding thinking continuations.

- [ ] **Step 5: Map adaptive thinking and effort through resolved capabilities**

When Messages contains `thinking: {"type":"adaptive"}`, enable the route's configured reasoning mode. Map `output_config.effort` only through `ResolvedCapabilities.effort_map`. Fixed-on semantic policy remains fixed-on. If reasoning is required but the route lacks output plus replay carriers, fail before dispatch. If optional effort is unavailable, omit only that control and emit `optional_reasoning_effort` downgrade metadata.

```rust
fn apply_claude_thinking_controls(
    claude: &Value,
    chat: &mut Value,
    resolved: &ResolvedCapabilities,
    downgrades: &mut BTreeSet<String>,
) -> Result<(), CapabilityResolutionError> {
    let adaptive = claude.pointer("/thinking/type").and_then(Value::as_str) == Some("adaptive");
    if !adaptive { return Ok(()); }
    if !resolved.supports(Capability::ReasoningOutput)
        || !resolved.supports(Capability::ReasoningReplay)
    {
        return Err(CapabilityResolutionError { capability: Capability::ReasoningReplay });
    }
    if let Some(requested) = claude.pointer("/output_config/effort").and_then(Value::as_str) {
        if let Some(mapped) = resolved.effort_map.get(requested) {
            chat.as_object_mut().unwrap().insert(
                "reasoning_effort".into(), Value::String(mapped.clone()));
        } else {
            downgrades.insert("optional_reasoning_effort".into());
        }
    }
    Ok(())
}
```

Honor `clear_thinking_20251015` with `keep: "all"` by retaining every thinking block. Report unknown optional context edits and cache hints as downgrades; never change message text based on guessed semantics.

- [ ] **Step 6: Emit and verify official thinking block sequences**

For JSON responses, emit `thinking` before linked `tool_use` blocks and include `signature`. For SSE, emit:

```text
content_block_start(thinking)
content_block_delta(thinking_delta)*
content_block_delta(signature_delta)
content_block_stop
content_block_start(tool_use)*
content_block_delta(input_json_delta)*
content_block_stop*
message_delta
message_stop
```

Forward thinking deltas immediately while incrementally updating HMAC state. Delay only the thinking block's `signature_delta` and stop until linked call IDs are known; do not delay reasoning bytes. On replay, collect the immediately associated following `tool_use` IDs, verify the signature before conversion, and restore the exact thinking string to the verified carrier.

- [ ] **Step 7: Run Claude JSON, SSE, token, and replay coverage**

Run: `rtk cargo test --test thinking_signature --test gateway claude -- --nocapture`

Expected: PASS for JSON, stream order, valid replay, invalid signature rejection, tool-result linking, and non-zero token counting.

- [ ] **Step 8: Commit the Claude thinking bridge**

```bash
rtk git add Cargo.toml src/server.rs src/server/gateway/claude.rs src/server/gateway.rs src/server/gateway/capability_routing.rs src/server/gateway/thinking_signature.rs tests/thinking_signature.rs tests/gateway/claude.rs tests/fixtures/clients/claude-code-2.1.195-messages.json
rtk git commit -m "feat: authenticate claude thinking replay"
```

### Task 11: Add One Safe Dialect Correction And Bounded Diagnostics

**Files:**
- Modify: `src/capabilities/types.rs`
- Create: `src/server/gateway/dialect_retry.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/upstream.rs`
- Modify: `src/server/gateway/stream.rs`
- Modify: `src/state/types.rs`
- Modify: `src/state/postgres.rs`
- Create: `tests/gateway/dialect_retry.rs`
- Modify: `tests/gateway.rs`
- Modify: `tests/postgres_roundtrip.rs`

- [ ] **Step 1: Write failing single-attempt and correction tests**

Create `tests/gateway/dialect_retry.rs`:

```rust
#[tokio::test]
async fn healthy_request_is_exactly_one_upstream_attempt() {
    let fixture = DialectRetryFixture::healthy().await;
    assert_eq!(fixture.send().await.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 1);
}

#[tokio::test]
async fn recognized_token_field_400_gets_one_known_correction() {
    let fixture = DialectRetryFixture::scripted(vec![
        reply_400(json!({"error":{"param":"max_tokens","code":"unsupported_parameter"}})),
        reply_ok("corrected"),
    ]).with_correction(DialectCorrectionRule::SwitchTokenLimit {
        rejected: TokenLimitField::MaxTokens,
        replacement: TokenLimitField::MaxCompletionTokens,
    }).await;
    let response = fixture.send().await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.upstream_hits(), 2);
    assert!(fixture.requests()[0].get("max_tokens").is_some());
    assert!(fixture.requests()[1].get("max_completion_tokens").is_some());
    assert_eq!(response.headers()["x-chat2responses-dialect-retry"], "1");
}

#[tokio::test]
async fn correction_never_removes_semantic_state() {
    for protected in ["tools", "tool_choice", "messages", "input", "reasoning_content",
        "image_url", "response_format"]
    {
        assert!(!DialectCorrectionRule::RemoveOptionalField { field: protected.into() }.is_safe());
    }
}

#[tokio::test]
async fn auth_quota_arbitrary_4xx_and_started_stream_are_never_corrected() {
    for status in [401, 403, 409, 429, 500] {
        let fixture = DialectRetryFixture::status(status).await;
        let _ = fixture.send().await;
        assert_eq!(fixture.upstream_hits(), 1);
    }
    let fixture = DialectRetryFixture::stream_then_error().await;
    let _ = fixture.send_stream().await;
    assert_eq!(fixture.upstream_hits(), 1);
}
```

- [ ] **Step 2: Run retry tests and confirm current broad retry path fails invariants**

Run: `rtk cargo test --test gateway dialect_retry -- --nocapture`

Expected: FAIL because there is no recognized-field correction count or safe-rule implementation.

- [ ] **Step 3: Implement explicit safe correction rules**

Add these methods to the `DialectCorrectionRule` and `TokenLimitField` types already defined in `src/capabilities/types.rs`:

```rust
impl TokenLimitField {
    pub fn request_field(self) -> Option<&'static str> {
        match self {
            Self::MaxTokens => Some("max_tokens"),
            Self::MaxCompletionTokens => Some("max_completion_tokens"),
            Self::MaxOutputTokens => Some("max_output_tokens"),
            Self::Omit => None,
        }
    }
}

impl DialectCorrectionRule {
    pub fn is_safe(&self) -> bool {
        match self {
            Self::SwitchTokenLimit { rejected, replacement } => rejected != replacement
                && rejected.request_field().is_some() && replacement.request_field().is_some(),
            Self::RemoveOptionalField { field } => matches!(field.as_str(),
                "service_tier" | "safety_identifier" | "prompt_cache_key" |
                "prompt_cache_retention" | "client_metadata" | "verbosity" |
                "parallel_tool_calls" | "stream_options"),
        }
    }

    pub fn matches_rejected_field(&self, rejected_field: &str) -> bool {
        match self {
            Self::SwitchTokenLimit { rejected, .. } =>
                rejected.request_field() == Some(rejected_field),
            Self::RemoveOptionalField { field } => field == rejected_field,
        }
    }
}
```

Only conclusive probes or trusted route overrides may populate these rules. A policy candidate alone does not make a correction known-safe.

- [ ] **Step 4: Implement exact pre-stream error classification and one retry**

Create `src/server/gateway/dialect_retry.rs`:

```rust
pub fn correction_for_response(
    status: StatusCode,
    error_body: &[u8],
    response_started: bool,
    rules: &[DialectCorrectionRule],
) -> Option<DialectCorrectionRule> {
    if status != StatusCode::BAD_REQUEST || response_started || error_body.len() > 65_536 {
        return None;
    }
    let value: Value = serde_json::from_slice(error_body).ok()?;
    let param = value.pointer("/error/param").and_then(Value::as_str)?;
    let code = value.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
    if !matches!(code, "unsupported_parameter" | "invalid_parameter" | "unknown_field") {
        return None;
    }
    rules.iter().find(|rule| rule.is_safe() && rule.matches_rejected_field(param)).cloned()
}
```

Buffer only the bounded error body for a pre-stream non-success response. Apply at most one rule to a fresh clone of the original selected-route payload, reuse the same upstream/key and reservation semantics, increment `dialect_retry_count`, and queue an asynchronous profile refresh. Never enter ordinary upstream failover between the original dialect error and its correction.

- [ ] **Step 5: Add structured compatibility diagnostics and downgrade headers**

Define a bounded metadata type and add `compatibility: Option<CompatibilityUsageMetadata>` to `UsageLog`:

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CompatibilityUsageMetadata {
    pub protocol_transition: String,
    pub adapter_types: Vec<String>,
    pub optional_downgrades: Vec<String>,
    pub policy_id: Option<String>,
    pub policy_schema_version: u32,
    pub policy_digest: String,
    pub profile_state: String,
    pub probe_version: u32,
    pub dialect_retry_count: u8,
    pub fallback_stage: Option<String>,
}
```

Add a nullable `compatibility TEXT` PostgreSQL column and JSON round-trip. Add the exact field `compatibility: None,` to every existing non-gateway `UsageLog` constructor under `src/` and `tests/`; populate it centrally in `append_gateway_usage_log` for gateway requests.

Build `x-chat2responses-downgrade` from sorted unique ASCII codes, comma-separated and truncated only at code boundaries to 512 bytes. Add `x-chat2responses-dialect-retry: 1` for a corrected request. Never put prompt, response, reasoning, image location/data, tool name/arguments/result, header, key, or raw upstream error text in metadata.

```rust
fn downgrade_header(codes: &BTreeSet<String>) -> Option<HeaderValue> {
    let mut value = String::new();
    for code in codes.iter().filter(|code| code.is_ascii()
        && code.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b':' | b'-')))
    {
        let separator = if value.is_empty() { 0 } else { 1 };
        if value.len() + separator + code.len() > 512 { break; }
        if separator == 1 { value.push(','); }
        value.push_str(code);
    }
    (!value.is_empty()).then(|| HeaderValue::from_str(&value)
        .expect("validated ASCII downgrade header"))
}
```

- [ ] **Step 6: Persist actual fallback stage in response history and matrix metadata**

Replace constant/null fallback reporting with the `FallbackStage` enum defined in Task 1. Store the selected value in both continuation and usage metadata:

```rust
history_state.fallback_stage = Some(stage);
usage_compatibility.fallback_stage = Some(
    serde_json::to_value(stage).expect("serializable fallback stage")
        .as_str().expect("fallback stage serializes as string").to_owned()
);
```

`HistoryReduced` is always a warning. A retry that would remove tools, choice, results, call IDs, reasoning, images, instructions, or required structured output returns the original classified error.

- [ ] **Step 7: Run retry, error-category, history, and PostgreSQL tests**

Run: `rtk cargo test --test gateway dialect_retry -- --nocapture`

Expected: PASS; healthy/auth/quota/streamed cases have one hit and only recognized field 400 has two.

Run: `rtk cargo test --test postgres_roundtrip compatibility -- --nocapture`

Expected: PASS or normal environment skip; diagnostics round-trip without sensitive payloads.

- [ ] **Step 8: Commit bounded correction and diagnostics**

```bash
rtk git add src/capabilities/types.rs src/server/gateway/dialect_retry.rs src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/stream.rs src/state/types.rs src/state/postgres.rs tests/gateway.rs tests/gateway/dialect_retry.rs tests/postgres_roundtrip.rs
rtk git commit -m "feat: bound dialect correction and downgrade diagnostics"
```

### Task 12: Enforce A Four-Client Semantic Compatibility Matrix

**Files:**
- Modify: `src/server.rs`
- Create: `src/server/gateway/compatibility_semantics.rs`
- Modify: `src/server/gateway/troubleshooting.rs`
- Modify: `src/server/gateway.rs`
- Modify: `tests/troubleshooting.rs`
- Create: `tests/compatibility_semantics.rs`
- Create: `tests/fixtures/clients/codex-0.144.0-responses.json`
- Create: `tests/fixtures/clients/opencode-1.17.9-chat.json`
- Create: `tests/fixtures/clients/hermes-0.14.0-chat.json`
- Modify: `tests/fixtures/clients/claude-code-2.1.195-messages.json`
- Create: `tests/fixtures/clients/malformed-responses.sse`
- Create: `tests/fixtures/clients/malformed-chat.sse`
- Create: `tests/fixtures/clients/malformed-messages.sse`

- [ ] **Step 1: Write failing validator negative and positive tests**

Create `tests/compatibility_semantics.rs`:

```rust
#[test]
fn http_200_empty_or_malformed_stream_is_not_a_pass() {
    for (profile, fixture) in [
        (AgentClientProfile::Codex, include_str!("fixtures/clients/malformed-responses.sse")),
        (AgentClientProfile::Opencode, include_str!("fixtures/clients/malformed-chat.sse")),
        (AgentClientProfile::ClaudeCode, include_str!("fixtures/clients/malformed-messages.sse")),
    ] {
        let result = validate_client_stream(profile, fixture.as_bytes(), &SemanticExpectation::text());
        assert!(!result.passed, "{profile:?} malformed stream passed");
        assert!(result.codes.iter().any(|code| code.starts_with("missing_") || code.starts_with("invalid_")));
    }
}

#[test]
fn plain_text_to_forced_tool_prompt_is_model_compatibility_failure() {
    let body = br#"{"choices":[{"message":{"content":"I would call the tool"},"finish_reason":"stop"}]}"#;
    let result = validate_client_json(AgentClientProfile::Hermes, body,
        &SemanticExpectation::forced_function("gateway_matrix_probe"));
    assert_eq!(result.error_category.as_deref(), Some("gateway_model_semantic_incompatible"));
}

#[test]
fn responses_namespace_and_reasoning_markers_must_be_restored() {
    let body = valid_responses_fixture_with_namespace_reasoning();
    let expected = SemanticExpectation::codex_namespace_reasoning(
        "multi_agent_v1", "spawn_agent", "reasoning-marker-17");
    assert!(validate_client_json(AgentClientProfile::Codex, &body, &expected).passed);
}
```

Validators must also test parseable complete arguments, linked output IDs, image-derived expected label, MIME/source/order/detail, usage when supplied, first meaningful event, and exact terminal event/finish reason.

- [ ] **Step 2: Write failing default-matrix and expectation tests**

Extend `tests/troubleshooting.rs`:

```rust
#[tokio::test]
async fn default_matrix_contains_codex_opencode_claude_code_and_hermes() {
    let response = run_matrix(MatrixRequest { client_profiles: vec![], models: vec![] }).await;
    assert_eq!(response["client_profiles"], json!(["codex","opencode","claude_code","hermes"]));
}

#[tokio::test]
async fn matrix_expands_dynamic_expectations_but_does_not_change_routing() {
    let fixture = matrix_fixture_with_expectation("opaque/*", ["agent_core", "reasoning_agent"]);
    let before = fixture.capability_snapshot_digest();
    let response = fixture.run_for_all_downstream_models().await;
    assert_eq!(response["models"], fixture.live_models());
    assert!(response["cells"].as_array().unwrap().iter()
        .all(|cell| cell["check_results"].is_array()));
    assert_eq!(fixture.capability_snapshot_digest(), before);
}

#[tokio::test]
async fn claude_matrix_requires_messages_order_signed_replay_and_positive_count_tokens() {
    let cell = run_deterministic_claude_matrix_cell().await;
    assert_eq!(cell["status"], "passed");
    assert!(cell["check_results"].as_array().unwrap().iter()
        .any(|check| check["id"] == "signed_thinking_replay" && check["passed"] == true));
    assert!(cell["check_results"].as_array().unwrap().iter()
        .any(|check| check["id"] == "count_tokens" && check["observed_value"].as_u64().unwrap() > 0));
}
```

- [ ] **Step 3: Run semantic and troubleshooting tests**

Run: `rtk cargo test --test compatibility_semantics -- --nocapture`

Expected: FAIL because semantic validators do not exist.

Run: `rtk cargo test --test troubleshooting compatibility_matrix -- --nocapture`

Expected: FAIL because Claude Code is excluded and HTTP 200/non-empty output is accepted.

- [ ] **Step 4: Implement protocol-specific semantic validators**

Create `src/server/gateway/compatibility_semantics.rs` with structured result types:

```rust
#[derive(Clone, Debug, Serialize)]
pub struct SemanticCheckResult {
    pub id: String,
    pub passed: bool,
    pub codes: Vec<String>,
    pub observed_value: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct SemanticExpectation {
    pub require_text_or_reasoning_or_tool_delta: bool,
    pub forced_function: Option<String>,
    pub expected_namespace: Option<(String, String)>,
    pub expected_reasoning_marker: Option<String>,
    pub expected_image_label: Option<String>,
    pub require_usage_if_present: bool,
    pub require_linked_continuation: bool,
}

#[derive(Clone, Debug)]
pub struct SemanticValidation {
    pub passed: bool,
    pub codes: Vec<String>,
    pub error_category: Option<String>,
    pub checks: Vec<SemanticCheckResult>,
    pub first_meaningful_event_ms: Option<u64>,
}

impl SemanticExpectation {
    pub fn text() -> Self {
        Self { require_text_or_reasoning_or_tool_delta: true, forced_function: None,
            expected_namespace: None, expected_reasoning_marker: None,
            expected_image_label: None, require_usage_if_present: true,
            require_linked_continuation: false }
    }

    pub fn forced_function(name: &str) -> Self {
        Self { forced_function: Some(name.to_owned()), require_linked_continuation: true,
            ..Self::text() }
    }

    pub fn codex_namespace_reasoning(namespace: &str, member: &str, marker: &str) -> Self {
        Self { expected_namespace: Some((namespace.to_owned(), member.to_owned())),
            expected_reasoning_marker: Some(marker.to_owned()),
            require_linked_continuation: true, ..Self::text() }
    }
}
```

Parse SSE frames by `event` and JSON `data`; do not search raw body substrings. Codex requires Responses item IDs and `response.completed`; OpenCode/Hermes require indexed Chat tool argument assembly and `[DONE]`; Claude Code requires official message/content block order, linked tool use/result, one signature delta for thinking, `message_delta`, and `message_stop`.

Re-export the validator entry points and expectation/result types from `src/server.rs` so `tests/compatibility_semantics.rs` exercises the same code used by the admin matrix.

- [ ] **Step 5: Generate deterministic semantic requests per client profile**

For each matrix cell, run only checks required by applicable expectation bundles:

```rust
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum MatrixSemanticCheck {
    Models, TextJson, TextStream, ForcedFunction, FragmentedArguments,
    ToolContinuation, UsageAndTerminal, ReasoningReplay, ImageHttps, ImageDataUrl,
    MixedImageOrder, ImageToolContinuation, NamespaceJson, NamespaceStream,
    PreviousResponseId, AdaptiveThinking, SignedThinkingReplay, CountTokens,
}

fn semantic_checks(profile: AgentClientProfile, required: &BTreeSet<Capability>)
    -> BTreeSet<MatrixSemanticCheck>
{
    let mut checks = BTreeSet::from([MatrixSemanticCheck::Models,
        MatrixSemanticCheck::TextJson, MatrixSemanticCheck::TextStream]);
    if required.contains(&Capability::FunctionTools) {
        checks.extend([MatrixSemanticCheck::ForcedFunction,
            MatrixSemanticCheck::FragmentedArguments, MatrixSemanticCheck::ToolContinuation,
            MatrixSemanticCheck::UsageAndTerminal]);
    }
    if required.contains(&Capability::ReasoningReplay) {
        checks.insert(MatrixSemanticCheck::ReasoningReplay);
    }
    if required.contains(&Capability::ImageHttps) {
        checks.extend([MatrixSemanticCheck::ImageHttps, MatrixSemanticCheck::ImageDataUrl,
            MatrixSemanticCheck::MixedImageOrder, MatrixSemanticCheck::ImageToolContinuation]);
    }
    if profile == AgentClientProfile::Codex {
        checks.extend([MatrixSemanticCheck::NamespaceJson,
            MatrixSemanticCheck::NamespaceStream, MatrixSemanticCheck::PreviousResponseId]);
    }
    if profile == AgentClientProfile::ClaudeCode {
        checks.extend([MatrixSemanticCheck::AdaptiveThinking,
            MatrixSemanticCheck::SignedThinkingReplay, MatrixSemanticCheck::CountTokens]);
    }
    checks
}
```

- `agent_core`: models, text JSON/SSE, forced function, fragmented arguments, linked result continuation, usage/terminal validation;
- `reasoning_agent`: reasoning output marker, forced tool call, exact replay marker, successful continuation;
- `image_agent`: HTTPS and Data URL fixtures, mixed ordering, image-derived forced function, streaming text;
- Codex additionally tests namespace JSON/SSE and `previous_response_id`;
- Claude Code additionally tests adaptive effort, gateway signature replay, and positive `count_tokens`.

Use synthetic `gateway_matrix_probe` tools and non-secret random nonces. A result is passed only when every required check passes. A safe optional downgrade is warning; `HistoryReduced` is warning even if the final HTTP status is 200.

- [ ] **Step 6: Expand matrix cells with route and semantic evidence**

Replace `fallback_stage: None` with actual metadata and add:

```rust
profile_state: String,
probe_version: u32,
runtime_model_slug: String,
adapter_set: Vec<String>,
dialect_retry_count: u8,
check_results: Vec<SemanticCheckResult>,
first_meaningful_event_ms: Option<u64>,
```

Default client order is Codex, OpenCode, Claude Code, Hermes. Enumerate models from the selected downstream when `models` is empty. Apply `compatibility_expectations` only as required diagnostic assertions; do not call `replace_capability_configuration`, mutate profiles, or pass expectation capabilities into route selection.

- [ ] **Step 7: Add sanitized exact-version structural fixtures**

Fixtures contain endpoint, headers names without values, request block/tool structure, and synthetic prompt/result values. Remove authorization, API keys, real URLs, user prompts, tool arguments/results, response prose, reasoning prose, and image data. Include source version/commit metadata in a top-level `_fixture_metadata` object that tests strip before sending.

```json
{
  "_fixture_metadata": {
    "client": "codex",
    "version": "0.144.0",
    "source_commit": "767822446c7a594caa19609ca435281a9ec67e0d",
    "captured_at": "2026-07-10",
    "sanitized": true
  },
  "method": "POST",
  "path": "/v1/responses",
  "header_names": ["authorization", "content-type", "user-agent"],
  "body": {
    "model": "synthetic-model",
    "input": "synthetic matrix request",
    "stream": true,
    "tools": [{"type":"function","name":"gateway_matrix_probe",
      "parameters":{"type":"object","properties":{"nonce":{"type":"string"}},
      "required":["nonce"]}}]
  }
}
```

- [ ] **Step 8: Run validator, matrix, and gateway suites**

Run: `rtk cargo test --test compatibility_semantics --test troubleshooting -- --nocapture`

Expected: PASS; malformed HTTP 200 fixtures fail and all four clients appear by default.

Run: `rtk cargo test --test gateway -- --nocapture`

Expected: PASS for existing Chat, Responses, and Messages behavior plus new semantics.

- [ ] **Step 9: Commit the semantic four-client matrix**

```bash
rtk git add src/server.rs src/server/gateway/compatibility_semantics.rs src/server/gateway/troubleshooting.rs src/server/gateway.rs tests/compatibility_semantics.rs tests/troubleshooting.rs tests/fixtures/clients
rtk git commit -m "feat: enforce four-client protocol semantics"
```

### Task 13: Expose Capability Administration And Truthful Client Presets

**Files:**
- Modify: `src/server/gateway.rs`
- Create: `src/server/gateway/capability_admin.rs`
- Modify: `tests/admin.rs`
- Create: `tests/admin_capabilities.rs`
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/utils/integration.ts`
- Modify: `frontend/src/utils/troubleshooting.ts`
- Modify: `frontend/src/components/CompatibilityMatrixPanel.vue`
- Modify: `frontend/src/components/TroubleshootingCenter.vue`
- Modify: `frontend/src/views/portal/Integration.vue`
- Modify: `frontend/tests/api/admin.spec.ts`
- Modify: `frontend/tests/utils/integration.spec.ts`
- Modify: `frontend/tests/utils/troubleshooting.spec.ts`

- [ ] **Step 1: Write failing admin API tests**

Add `tests/admin_capabilities.rs` to `tests/admin.rs`:

```rust
#[tokio::test]
async fn admin_can_export_import_and_inspect_capability_sources() {
    let fixture = AdminCapabilityFixture::new().await;
    let export = fixture.get("/api/admin/capabilities/export").await;
    assert_eq!(export.status(), StatusCode::OK);
    assert_eq!(response_json(export).await["schema_version"], 1);

    let mut bundle = fixture.valid_bundle();
    bundle["revision"] = json!(42);
    let import = fixture.post_json("/api/admin/capabilities/import", bundle).await;
    assert_eq!(import.status(), StatusCode::OK);

    let resolved = fixture.get(
        "/api/admin/capabilities/resolved?upstream_id=up-1&model=opaque&protocol=chat_completions"
    ).await;
    let body = response_json(resolved).await;
    assert_eq!(body["configuration_revision"], 42);
    assert!(body["capabilities"]["function_tools"]["source"].is_string());
    assert!(body["profile_age_seconds"].is_number() || body["profile_age_seconds"].is_null());
}

#[tokio::test]
async fn invalid_import_is_400_and_keeps_previous_revision() {
    let fixture = AdminCapabilityFixture::new().await;
    fixture.import_revision(9).await;
    let response = fixture.post_json("/api/admin/capabilities/import", json!({
        "schema_version":999,"revision":10
    })).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fixture.export().await["revision"], 9);
}

#[tokio::test]
async fn manual_probe_only_enqueues_and_returns_accepted() {
    let fixture = AdminCapabilityFixture::new().await;
    let response = fixture.post_json("/api/admin/capabilities/probe", json!({
        "upstream_id":"up-1","runtime_model_slug":"opaque",
        "protocol":"chat_completions"
    })).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(response).await["queued"], true);
}
```

- [ ] **Step 2: Run admin tests and confirm routes are missing**

Run: `rtk cargo test --test admin admin_capabilities -- --nocapture`

Expected: FAIL with 404 for the capability endpoints.

- [ ] **Step 3: Implement authenticated capability APIs**

Create `src/server/gateway/capability_admin.rs` and register these routes behind the existing admin middleware:

```text
GET  /api/admin/capabilities/export
POST /api/admin/capabilities/import
GET  /api/admin/capabilities/profiles
GET  /api/admin/capabilities/resolved
POST /api/admin/capabilities/probe
DELETE /api/admin/capabilities/profiles/:upstream_id
```

Import accepts `CapabilityConfiguration`, compiles it, persists it, then atomically swaps it. Export returns the versioned data document without dialect profiles or secrets. Profiles return exact keys, state, source/evidence codes, age, sanitized event/status summary, and fingerprint; they never return requests or response content. Resolved inspection requires exact upstream ID, runtime slug, and protocol and reports each value plus source/conflict.

Use this error response for invalid imports:

```rust
(
    StatusCode::BAD_REQUEST,
    Json(json!({"error": {
        "code": "gateway_capability_policy_invalid",
        "message": error.to_string()
    }})),
)
```

The message may contain policy IDs/field paths but no payload or credential data.

- [ ] **Step 4: Write failing frontend contract tests**

Extend `frontend/tests/api/admin.spec.ts` and `frontend/tests/utils/integration.spec.ts`:

```ts
it('uses the live Codex catalog without inventing capabilities', () => {
  const live = { models: [{ slug: 'opaque', input_modalities: ['text', 'image'],
    supports_parallel_tool_calls: false, apply_patch_tool_type: null }] }
  const emitted = JSON.parse(buildCodexModelCatalogJson(live))
  expect(emitted).toEqual(live)
})

it('fails catalog generation when the live catalog is unavailable', () => {
  expect(() => buildCodexModelCatalogJson(undefined)).toThrow('live Codex catalog is unavailable')
})

it('generates a Codex provider with hosted web search disabled', () => {
  const config = buildCodexConfigToml({ gatewayBaseUrl: 'https://gw.example', modelSlug: 'opaque' })
  expect(config).toContain('web_search = "disabled"')
  expect(config).toContain('wire_api = "responses"')
})

it('maps every Claude Code alias to an arbitrary selected gateway slug', () => {
  const settings = JSON.parse(buildClaudeCodeSettingsJson({ gatewayBaseUrl: 'https://gw.example',
    portalKey: 'downstream-key', modelSlugs: ['lab/opaque'], selectedModelSlug: 'lab/opaque' }))
  expect(settings.env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBe('lab/opaque')
  expect(settings.env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe('lab/opaque')
  expect(settings.env.ANTHROPIC_DEFAULT_HAIKU_MODEL).toBe('lab/opaque')
})
```

Add API mock tests for export/import/profiles/manual probe and a troubleshooting utility test that the default profiles are `['codex', 'opencode', 'claude_code', 'hermes']`.

- [ ] **Step 5: Run frontend tests and observe the optimistic catalog failure**

Run: `rtk npm --prefix frontend exec vitest run`

Expected: FAIL because `buildCodexModelCatalogJson` still constructs fixed capabilities and admin methods are absent.

- [ ] **Step 6: Add frontend capability types and API methods**

Add TypeScript interfaces matching the Rust response, including:

```ts
export type EvidenceState = 'supported' | 'rejected' | 'unobserved'
export type CapabilitySource = 'override' | 'probe' | 'policy' | 'baseline'

export interface ResolvedCapabilityValue {
  state: EvidenceState
  source: CapabilitySource
}

export interface DialectProfileSummary {
  upstream_id: string
  runtime_model_slug: string
  protocol: 'chat_completions' | 'responses'
  state: 'verified' | 'partial' | 'unsupported' | 'unknown'
  profile_age_seconds: number | null
  probe_version: number
  evidence_codes: string[]
}
```

Add typed `exportCapabilities`, `importCapabilities`, `getDialectProfiles`, `getResolvedCapabilities`, and `queueDialectProbe` methods to the existing admin client.

- [ ] **Step 7: Render capability evidence and semantic checks in troubleshooting**

Extend the existing matrix panel rather than creating a separate dashboard. For each cell, show profile state/age, protocol transition, adapter set, retry count, actual fallback stage, first meaningful event latency, and check-level pass/warning/failure. Add import/export and manual refresh controls to the existing troubleshooting center. Keep raw policy JSON in the existing code editor/dialog pattern and show validation errors visibly.

```vue
<el-table :data="selectedCell?.check_results || []" size="small">
  <el-table-column prop="id" label="检查项" min-width="180" />
  <el-table-column label="结果" width="100">
    <template #default="{ row }">
      <el-tag :type="row.passed ? 'success' : 'danger'" effect="plain">
        {{ row.passed ? '通过' : '失败' }}
      </el-tag>
    </template>
  </el-table-column>
  <el-table-column prop="codes" label="证据代码" min-width="240" />
</el-table>
```

Never show prompts, reasoning, image URLs/data, tool arguments/results, or credentials.

- [ ] **Step 8: Fetch and emit the live Codex model catalog in the portal**

Change `buildCodexModelCatalogJson` to accept the gateway response and serialize it unchanged apart from stable pretty printing:

```ts
export interface CodexCatalogResponse { models: Record<string, unknown>[] }

export const buildCodexModelCatalogJson = (catalog?: CodexCatalogResponse) => {
  if (!catalog || !Array.isArray(catalog.models)) {
    throw new Error('live Codex catalog is unavailable')
  }
  return `${jsonStringify(catalog)}\n`
}
```

In `Integration.vue`, fetch `/v1/models?client_version=0.144.0` with the current downstream key. Do not fall back to `allModelSlugs`; set the existing fatal error state and suppress copy-ready catalog output if the fetch fails. Add `web_search = "disabled"` to generated Codex config. Keep OpenCode on `@ai-sdk/openai-compatible`, map all Claude Code aliases to the selected slug, and keep Hermes on the Chat base URL.

Delete `isClaudeCompatibleSlug`. Always set `ANTHROPIC_DEFAULT_OPUS_MODEL`, `ANTHROPIC_DEFAULT_SONNET_MODEL`, and `ANTHROPIC_DEFAULT_HAIKU_MODEL` to the selected exposed slug; no frontend behavior may branch on a `claude`/`anthropic` prefix.

- [ ] **Step 9: Run backend admin and frontend suites**

Run: `rtk cargo test --test admin admin_capabilities -- --nocapture`

Expected: PASS for atomic import/export, source inspection, and queued manual probe.

Run: `rtk npm --prefix frontend exec vitest run`

Expected: PASS with no optimistic catalog construction.

Run: `rtk npm --prefix frontend run build`

Expected: PASS with Vue and TypeScript compilation clean.

- [ ] **Step 10: Commit administration and presets**

```bash
rtk git add src/server/gateway.rs src/server/gateway/capability_admin.rs tests/admin.rs tests/admin_capabilities.rs frontend/src frontend/tests
rtk git commit -m "feat: expose truthful capability administration"
```

### Task 14: Ship Replaceable Deployment Data And Real-Client Acceptance Scripts

**Files:**
- Create: `templates/capabilities/current-deployment.example.json`
- Modify: `templates/codex/config.toml.example`
- Modify: `templates/codex/model-catalog.json`
- Create: `templates/opencode/opencode.json`
- Create: `templates/claude-code/settings.json`
- Create: `templates/hermes/config.yaml`
- Modify: `scripts/compatibility_matrix.sh`
- Create: `scripts/render_live_capabilities.sh`
- Create: `scripts/installed_client_smoke.sh`
- Modify: `tests/templates.rs`
- Modify: `tests/scripts.rs`
- Modify: `README.md`
- Modify: `DEPLOYMENT.md`
- Modify: `docs/codex-integration-guide.md`
- Create: `docs/PROTOCOL_COMPATIBILITY.md`

- [ ] **Step 1: Write failing template and script contract tests**

Extend `tests/templates.rs`:

```rust
#[test]
fn deployment_capabilities_are_external_versioned_and_model_agnostic_in_code() {
    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("templates/capabilities/current-deployment.example.json").unwrap()
    ).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert!(value["bundles"].as_array().unwrap().iter().any(|v| v["id"] == "agent_core"));
    assert!(value["bundles"].as_array().unwrap().iter().any(|v| v["id"] == "reasoning_agent"));
    assert!(value["bundles"].as_array().unwrap().iter().any(|v| v["id"] == "image_agent"));
    assert!(value["compatibility_expectations"].as_array().unwrap().len() >= 6);
}

#[test]
fn all_client_templates_use_only_gateway_url_key_and_exposed_slug() {
    let codex = std::fs::read_to_string("templates/codex/config.toml.example").unwrap();
    assert!(codex.contains("web_search = \"disabled\""));
    for path in ["templates/opencode/opencode.json", "templates/claude-code/settings.json",
        "templates/hermes/config.yaml"]
    {
        let body = std::fs::read_to_string(path).unwrap();
        assert!(!body.contains("api.deepseek.com"));
        assert!(!body.contains("api.minimax.io"));
        assert!(!body.contains("api.moonshot.cn"));
    }
}
```

Extend `tests/scripts.rs` to assert the matrix default contains all four client profiles, uses `jq -e` to fail on semantic failures, and the installed-client script pins exact verified versions.

- [ ] **Step 2: Run template/script tests and confirm missing artifacts**

Run: `rtk cargo test --test templates --test scripts -- --nocapture`

Expected: FAIL because the capability template and smoke scripts do not exist and Claude Code is missing from matrix defaults.

- [ ] **Step 3: Create importable current-deployment data**

Create `templates/capabilities/current-deployment.example.json` with schema version 1, no credentials/URLs, and these reusable bundles:

```json
{
  "id": "agent_core",
  "required": [
    "text_input", "text_stream", "function_tools", "forced_tool_choice",
    "tool_continuation", "indexed_tool_argument_stream"
  ]
}
```

```json
{
  "id": "reasoning_agent",
  "required": ["reasoning_output", "reasoning_replay", "reasoning_stream"]
}
```

```json
{
  "id": "image_agent",
  "required": ["image_https", "image_data_url", "text_stream", "function_tools"]
}
```

Add current-deployment selectors for `glm-5.2`, `deepseek-v4-flash`, `MiniMax/MiniMax-M2.5`, `MiniMax/MiniMax-M2.7`, `moonshotai/Kimi-K2.5`, and `moonshotai/kimi-k2.6` as data only. Assign `agent_core` to every entry and `reasoning_agent` where the deployment policy requires thinking replay. Put source title/URL/retrieval date/version in each policy's typed `evidence` array, and put reasoning invariants, effort mappings, sampling omissions, ceilings, replay requirements, and generic extension probe cases in their corresponding schema fields. Do not add any trusted wire capability override to this file.

The selected Qwen VLM is added by the render script from `QWEN_VLM_SLUG`; it receives `agent_core` plus `image_agent`, all four clients, and the configured HTTPS fixture/expected label. This keeps the template reusable without compiling or guessing a Qwen slug.

- [ ] **Step 4: Render and import exact live acceptance configuration**

Create `scripts/render_live_capabilities.sh` with required environment variables `QWEN_VLM_SLUG`, `IMAGE_FIXTURE_URL`, and `IMAGE_FIXTURE_EXPECTED_LABEL`. Use `jq` to append this exact expectation shape:

```json
{
  "id": "selected-qwen-vlm",
  "selector": {"exposed_model": "value from QWEN_VLM_SLUG"},
  "bundles": ["agent_core", "image_agent"],
  "client_profiles": ["codex", "opencode", "claude_code", "hermes"],
  "permitted_optional_downgrades": ["optional_image_detail"],
  "https_image_fixture": {
    "url": "value from IMAGE_FIXTURE_URL",
    "expected_label": "value from IMAGE_FIXTURE_EXPECTED_LABEL"
  }
}
```

Validate all variables are non-empty. Accept `--output PATH` and optional `--import`, emit the rendered document to `PATH`, and call the admin import endpoint only when `--import` is supplied. Never put keys or admin passwords in the output JSON or command trace.

- [ ] **Step 5: Make the matrix script semantic and four-client by default**

Set:

```bash
CLIENTS_JSON="${CLIENTS_JSON:-[\"codex\",\"opencode\",\"claude_code\",\"hermes\"]}"
```

After saving the response, use `jq -e` to require `summary.failed == 0`, every required check `passed == true`, and no unpermitted downgrade. Print model, client, exact runtime slug, upstream, profile state/version, transition, adapters, retry count, fallback stage, first-event latency, and duration. Keep raw secrets and semantic payloads out of output.

- [ ] **Step 6: Add exact-version installed-client smoke orchestration**

Create `scripts/installed_client_smoke.sh` that verifies and records:

```text
Codex CLI 0.144.0
OpenCode 1.17.9
Claude Code 2.1.195
Hermes Agent 0.14.0
```

Require `BASE_URL`, `DOWNSTREAM_KEY`, and `MODEL_SLUG`; never echo the key. Each client performs one text task and one safe read-only tool task in a temporary directory. Codex additionally performs a namespace-backed lookup when `CODEX_NAMESPACE_TEST=1`. Run attachment workflows only when that installed CLI exposes a documented attachment flag; otherwise report `protocol_matrix_covered` rather than inventing a command. Capture exit status, client version, duration, and sanitized tool/event types only.

- [ ] **Step 7: Update client templates and protocol documentation**

Make every preset require only gateway URL, downstream key, and exposed slug. Codex uses Responses and `web_search = "disabled"`; OpenCode uses `@ai-sdk/openai-compatible`; Claude Code sets `ANTHROPIC_BASE_URL` and maps Haiku/Sonnet/Opus aliases to the selected slug; Hermes uses the Chat base URL.

The repository's static `templates/codex/model-catalog.json` becomes a non-optimistic scaffold:

```json
{
  "models": []
}
```

Update `tests/templates.rs` to require an empty scaffold and require the portal to fetch the live capability-backed catalog before offering copy-ready output.

Document:

- third-party/self-hosted upstreams are the primary target;
- exact-route probes override vendor documentation for wire syntax;
- policy semantics do not prove relay support;
- preserve/adapt/downgrade/reject rules;
- capability import/export and manual probes;
- expectation bundles and selected Qwen VLM setup;
- no request-path probes and one healthy attempt;
- official client/model references already enumerated in the approved design.

Create `docs/PROTOCOL_COMPATIBILITY.md` with maturity labels based on semantic evidence, not HTTP reachability, and broaden `docs/codex-integration-guide.md` with the shared gateway prerequisites and links to all four presets.

- [ ] **Step 8: Run static template/script validation**

Run: `rtk cargo test --test templates --test scripts -- --nocapture`

Expected: PASS.

Run: `rtk bash -n scripts/compatibility_matrix.sh scripts/render_live_capabilities.sh scripts/installed_client_smoke.sh`

Expected: exit 0 with no shell syntax errors.

Run: `rtk jq -e '.schema_version == 1' templates/capabilities/current-deployment.example.json`

Expected: prints `true` and exits 0.

- [ ] **Step 9: Commit deployment data, scripts, templates, and docs**

```bash
rtk git add templates scripts tests/templates.rs tests/scripts.rs README.md DEPLOYMENT.md docs/codex-integration-guide.md docs/PROTOCOL_COMPATIBILITY.md
rtk git commit -m "docs: ship generic agent compatibility rollout data"
```

### Task 15: Prove Streaming Performance And Complete Acceptance

**Files:**
- Modify: `tests/load.rs`
- Modify: `docs/PROTOCOL_COMPATIBILITY.md`
- Create: `docs/verification/2026-07-10-agent-protocol-fidelity.md`

- [ ] **Step 1: Extend the baseline benchmark with the final first-event contract**

Retain the Task 0 harness, rename the final test `load_gateway_first_meaningful_event`, and run the same local streaming mock directly and through the release gateway with `TOTAL_REQUESTS = 100` and `CONCURRENCY = 20`. Measure from request start until the first meaningful non-keepalive SSE data event, not until body completion. Load `docs/verification/2026-07-10-agent-protocol-baseline.json` for regression comparison.

Use this result type and assertion:

```rust
#[derive(Debug)]
struct LatencyComparison {
    direct_ms: Vec<u64>,
    gateway_ms: Vec<u64>,
}

impl LatencyComparison {
    fn gateway_added_p95_ms(&mut self) -> i64 {
        self.direct_ms.sort_unstable();
        self.gateway_ms.sort_unstable();
        let index = (self.gateway_ms.len() * 95 / 100).min(self.gateway_ms.len() - 1);
        self.gateway_ms[index] as i64 - self.direct_ms[index] as i64
    }
}

assert!(comparison.gateway_added_p95_ms() < 50,
    "gateway-added first meaningful event P95 must remain below 50 ms");
let baseline: FirstEventBaseline = serde_json::from_slice(include_bytes!(
    "../docs/verification/2026-07-10-agent-protocol-baseline.json")).unwrap();
assert!(comparison.gateway_added_p95_ms() <= baseline.gateway_added_p95_ms + 10,
    "gateway-added P95 regressed by more than the 10 ms measurement allowance");
assert_eq!(upstream_hits.load(Ordering::SeqCst), TOTAL_REQUESTS * 2,
    "direct and gateway rounds must each make one healthy attempt");
```

Add a second bounded Data URL image round with the same comparison and these assertions:

```rust
assert!(image_comparison.gateway_added_p95_ms() < 50);
assert!(image_comparison.gateway_added_p95_ms()
    <= baseline.image_gateway_added_p95_ms + 10);
assert_eq!(image_upstream_hits.load(Ordering::SeqCst), TOTAL_REQUESTS * 2);
```

The upstream must emit one meaningful event, delay its terminal event, and prove the gateway forwards the first event before completion.

- [ ] **Step 2: Run the release performance test before optimization**

Run: `rtk cargo test --release --test load load_gateway_first_meaningful_event -- --ignored --nocapture`

Expected: the test prints direct P50/P95, gateway P50/P95, gateway-added P95, total duration, and hit count. If the new assertion fails, keep the measurement and proceed to Step 3.

- [ ] **Step 3: Remove measured conversion hot spots without changing semantics**

Use the failing measurement to inspect only the selected path. Acceptable changes are:

```rust
// Load immutable capability state once per request, not once per candidate field.
let capability_snapshot = state.capability_snapshot();

// Build one deterministic registry after route selection.
let conversion = ConversionContext::new(&resolved, tool_registry);

// Forward each translated frame immediately.
while let Some(chunk) = upstream.next().await {
    for frame in translator.push(&chunk?)? { yield frame; }
}
```

Do not add full-response aggregation, Base64 decode/re-encode, remote image fetch, request-path probes, or speculative retries. Re-run the focused semantic tests after each optimization.

- [ ] **Step 4: Pass the performance contract**

Run: `rtk cargo test --release --test load load_gateway_first_meaningful_event -- --ignored --nocapture`

Expected: PASS with gateway-added first meaningful event P95 below 50 ms for text and bounded inline image rounds, and exactly one gateway upstream hit per healthy request.

- [ ] **Step 5: Run formatting, lint, complete Rust, and frontend verification**

Run: `rtk cargo fmt --all -- --check`

Expected: exit 0.

Run: `rtk cargo clippy --all-targets -- -D warnings`

Expected: exit 0 with no warnings.

Run: `rtk cargo test --all-targets -- --nocapture`

Expected: all non-ignored root-package tests pass.

Run: `rtk cargo test --manifest-path crates/gateway-core/Cargo.toml --all-targets -- --nocapture`

Expected: all shared capability/protocol/state tests pass.

Run: `rtk npm --prefix frontend exec vitest run`

Expected: all frontend tests pass.

Run: `rtk npm --prefix frontend run build`

Expected: Vue type-check and production build pass.

- [ ] **Step 6: Prove production dispatch contains no compiled deployment classifier**

Run:

```bash
rtk rg -n -i 'deepseek|minimax|moonshot|kimi|qwen|zhipu|glm|api\.openai\.com|openai\.azure\.com|ChatCompatibilityFamily' src --glob '*.rs'
```

Expected: no matches in production normalization/routing/capability code. Legitimate Claude downstream protocol naming may remain; deployment model names must be confined to tests, docs, and `templates/capabilities/`.

- [ ] **Step 7: Run the dynamic `test` downstream live matrix**

Render and import the deployment data with the administrator-selected Qwen VLM, then run:

```bash
rtk env QWEN_VLM_SLUG="$QWEN_VLM_SLUG" IMAGE_FIXTURE_URL="$IMAGE_FIXTURE_URL" IMAGE_FIXTURE_EXPECTED_LABEL="$IMAGE_FIXTURE_EXPECTED_LABEL" scripts/render_live_capabilities.sh --output /tmp/chat2responses-live-capabilities.json --import
rtk env DOWNSTREAM_ID=test CLIENTS_JSON='["codex","opencode","claude_code","hermes"]' scripts/compatibility_matrix.sh
```

Expected:

- every model currently exposed by downstream `test` has four matrix cells;
- the six configured GLM/DeepSeek/MiniMax/Kimi entries have at least one verified route and pass their configured agent/reasoning checks;
- the selected Qwen VLM passes HTTPS image, Data URL image, mixed ordering, text stream, and image-derived tool continuation;
- Codex namespace JSON/SSE restoration passes;
- Claude Code signed thinking/tool/result replay and positive count tokens pass;
- no required check is hidden behind HTTP 200 or an unreported history reduction;
- upstream auth/quota failures retain their operational categories.

Do not convert an upstream authentication, quota, availability, or model-quality failure into a protocol pass. Record unresolved operational failures explicitly in the verification report.

- [ ] **Step 8: Run installed-client smoke tests**

Run:

```bash
rtk env BASE_URL="$BASE_URL" DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" scripts/installed_client_smoke.sh
```

Expected: exact-version Codex, OpenCode, Claude Code, and Hermes text/read-only-tool tasks exit 0. Codex namespace smoke passes when enabled. Attachment tasks either pass through a documented CLI workflow or are explicitly marked `protocol_matrix_covered`.

- [ ] **Step 9: Record evidence and acceptance-criterion mapping**

Create `docs/verification/2026-07-10-agent-protocol-fidelity.md` containing only sanitized commands/results, version/commit identifiers, profile states, semantic check summaries, latency statistics, and a table mapping acceptance criteria 1-13 to evidence. Do not include keys, prompts, response/reasoning text, image data/URLs, tool arguments/results, or raw upstream error bodies.

Update `docs/PROTOCOL_COMPATIBILITY.md` maturity levels from the final deterministic and live evidence. Mark a feature verified only when its semantic check passed.

- [ ] **Step 10: Commit final verification artifacts**

```bash
rtk git add tests/load.rs docs/PROTOCOL_COMPATIBILITY.md docs/verification/2026-07-10-agent-protocol-fidelity.md
rtk git commit -m "test: verify agent fidelity and streaming latency"
```

## Final Acceptance Checklist

- [ ] All Rust and frontend suites pass.
- [ ] Production request dispatch contains no model slug, vendor substring, provider hostname, or model-family classifier.
- [ ] The `test` downstream matrix dynamically enumerates every exposed model for Codex, OpenCode, Claude Code, and Hermes.
- [ ] Configured GLM, DeepSeek, MiniMax, and Kimi expectations pass required text/tool/reasoning semantics on at least one verified exact route.
- [ ] The administrator-selected Qwen VLM passes HTTPS/Data URL image understanding, mixed order, streaming text, and an image-derived tool loop.
- [ ] Codex namespace identity survives JSON, SSE, output continuation, and `previous_response_id`.
- [ ] Required reasoning routes replay exact reasoning through tool loops.
- [ ] Claude Code accepts and replays the gateway thinking signature with official SSE order.
- [ ] Optional hosted tools and image detail produce bounded diagnostics; required unsupported features fail before dispatch.
- [ ] URL/protocol/slug/override/probe-version changes invalidate profiles; auth/quota failures preserve last verified evidence.
- [ ] Catalog capabilities come from one witness and capability-using requests never route to a weaker profile.
- [ ] Healthy requests use one upstream attempt and add less than 50 ms P95 first meaningful event latency.
- [ ] Authentication, quota, availability, converter, and model-semantic failures remain distinguishable.
