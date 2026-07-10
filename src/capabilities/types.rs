use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::CAPABILITY_SCHEMA_VERSION;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    ChatCompletions,
    Responses,
    Messages,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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
pub enum EvidenceState {
    Supported,
    Rejected,
    Unobserved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    Override,
    Probe,
    Policy,
    Baseline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenLimitField {
    MaxTokens,
    MaxCompletionTokens,
    MaxOutputTokens,
    Omit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode {
    Off,
    Optional,
    FixedOn,
}

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
pub enum FallbackStage {
    Native,
    ProtocolAdapted,
    HistoryReplayed,
    HistoryReduced,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DialectCorrectionRule {
    SwitchTokenLimit {
        rejected: TokenLimitField,
        replacement: TokenLimitField,
    },
    RemoveOptionalField {
        field: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentClientProfile {
    Codex,
    Opencode,
    ClaudeCode,
    Hermes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateOperator {
    Exists,
    Equals,
    Contains,
    EventSequence,
}

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
#[serde(deny_unknown_fields)]
pub struct ResponsePredicate {
    pub path: String,
    pub operator: PredicateOperator,
    #[serde(default)]
    pub value: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarativeProbeCase {
    pub id: String,
    pub protocol: WireProtocol,
    #[serde(default)]
    pub prerequisites: BTreeSet<Capability>,
    pub request_patch: Value,
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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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
#[serde(deny_unknown_fields)]
pub struct RouteCapabilityOverride {
    pub id: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub selector: CapabilitySelector,
    #[serde(default)]
    pub capabilities: BTreeMap<Capability, EvidenceState>,
    #[serde(default)]
    pub token_limit_field: Option<TokenLimitField>,
    #[serde(default)]
    pub reasoning_carrier: Option<ReasoningCarrier>,
    #[serde(default)]
    pub correction_rules: Vec<DialectCorrectionRule>,
    #[serde(default)]
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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityExpectation {
    pub id: String,
    pub selector: CapabilitySelector,
    pub bundles: BTreeSet<String>,
    pub client_profiles: BTreeSet<AgentClientProfile>,
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
            schema_version: CAPABILITY_SCHEMA_VERSION,
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
