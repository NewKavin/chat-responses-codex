use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::CAPABILITY_SCHEMA_VERSION;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    ChatCompletions,
    Responses,
    Messages,
}

impl From<crate::routing::UpstreamProtocol> for WireProtocol {
    fn from(protocol: crate::routing::UpstreamProtocol) -> Self {
        match protocol {
            crate::routing::UpstreamProtocol::ChatCompletions => Self::ChatCompletions,
            crate::routing::UpstreamProtocol::Responses => Self::Responses,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    TextInput,
    NonStreamingResponse,
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

impl Capability {
    pub const ALL: [Self; 20] = [
        Self::TextInput,
        Self::NonStreamingResponse,
        Self::ImageHttps,
        Self::ImageDataUrl,
        Self::ImageDetail,
        Self::NativeFileId,
        Self::FunctionTools,
        Self::NamespaceTools,
        Self::CustomTools,
        Self::HostedTools,
        Self::ParallelToolCalls,
        Self::ForcedToolChoice,
        Self::ToolContinuation,
        Self::ReasoningOutput,
        Self::ReasoningReplay,
        Self::TextStream,
        Self::ReasoningStream,
        Self::IndexedToolArgumentStream,
        Self::UsageStream,
        Self::StructuredOutput,
    ];
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

impl DialectCorrectionRule {
    pub fn is_safe(&self) -> bool {
        match self {
            Self::SwitchTokenLimit {
                rejected,
                replacement,
            } => {
                rejected != replacement
                    && rejected.request_field().is_some()
                    && replacement.request_field().is_some()
            }
            Self::RemoveOptionalField { field } => matches!(
                field.as_str(),
                "service_tier"
                    | "safety_identifier"
                    | "prompt_cache_key"
                    | "prompt_cache_retention"
                    | "client_metadata"
                    | "verbosity"
                    | "parallel_tool_calls"
                    | "stream_options"
            ),
        }
    }

    pub fn matches_rejected_field(&self, rejected_field: &str) -> bool {
        match self {
            Self::SwitchTokenLimit { rejected, .. } => {
                rejected.request_field() == Some(rejected_field)
            }
            Self::RemoveOptionalField { field } => field == rejected_field,
        }
    }
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
    pub reasoning_controls: BTreeMap<String, Vec<Value>>,
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
            max_global_concurrency: 4,
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
    /// Version of the embedded builtin policy template that has been merged
    /// into this configuration. Configurations bootstrapped from the template
    /// carry the template's version; older stored configurations default to 0
    /// and receive a one-shot append-only merge of new builtin entries on
    /// upgrade (see `merge_builtin_policy_entries`).
    #[serde(default)]
    pub builtin_policy_version: u32,
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
            builtin_policy_version: 0,
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
    pub key_fingerprint: String,
    pub exposed_model_slug: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
    pub tags: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialectProfileKey {
    pub upstream_id: String,
    #[serde(default)]
    pub key_fingerprint: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
}

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

    pub fn from_route(route: &RouteIdentity) -> Self {
        Self::for_key(
            route.upstream_id.clone(),
            route.key_fingerprint.clone(),
            route.runtime_model_slug.clone(),
            route.protocol,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialectProfileState {
    Verified,
    Partial,
    Unsupported,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeProfileOutcome {
    Accepted,
    Rejected,
    OperationalFailure,
    Deferred,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamDialectProfile {
    pub key: DialectProfileKey,
    pub configuration_fingerprint: String,
    pub probe_schema_version: u32,
    pub state: DialectProfileState,
    pub capabilities: BTreeMap<Capability, EvidenceState>,
    pub token_limit_field: Option<TokenLimitField>,
    pub reasoning_carrier: Option<ReasoningCarrier>,
    pub correction_rules: Vec<DialectCorrectionRule>,
    pub reasoning_controls: BTreeMap<String, Vec<Value>>,
    pub extension_evidence: BTreeMap<String, EvidenceState>,
    pub last_attempt_at: Option<u64>,
    pub last_success_at: Option<u64>,
    pub last_operational_failure: Option<String>,
    #[serde(default)]
    pub last_probe_outcome: Option<ProbeProfileOutcome>,
    #[serde(default)]
    pub probe_retry_count: u32,
    #[serde(default)]
    pub next_probe_at: Option<u64>,
    pub evidence_codes: BTreeSet<String>,
    pub http_status: Option<u16>,
    pub event_types: BTreeSet<String>,
}

impl UpstreamDialectProfile {
    pub fn unknown(key: DialectProfileKey) -> Self {
        Self {
            key,
            configuration_fingerprint: String::new(),
            probe_schema_version: super::DIALECT_PROBE_SCHEMA_VERSION,
            state: DialectProfileState::Unknown,
            capabilities: BTreeMap::new(),
            token_limit_field: None,
            reasoning_carrier: None,
            correction_rules: Vec::new(),
            reasoning_controls: BTreeMap::new(),
            extension_evidence: BTreeMap::new(),
            last_attempt_at: None,
            last_success_at: None,
            last_operational_failure: None,
            last_probe_outcome: None,
            probe_retry_count: 0,
            next_probe_at: None,
            evidence_codes: BTreeSet::new(),
            http_status: None,
            event_types: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilityStateDocument {
    pub configuration: CapabilityConfiguration,
    #[serde(
        default,
        serialize_with = "serialize_capability_profiles",
        deserialize_with = "deserialize_capability_profiles"
    )]
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
                CapabilityConfiguration::default()
                    .compile()
                    .expect("default capability policy"),
            ),
            profiles: BTreeMap::new(),
        }
    }
}

fn serialize_capability_profiles<S>(
    profiles: &BTreeMap<DialectProfileKey, UpstreamDialectProfile>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    profiles.values().collect::<Vec<_>>().serialize(serializer)
}

fn deserialize_capability_profiles<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<DialectProfileKey, UpstreamDialectProfile>, D::Error>
where
    D: Deserializer<'de>,
{
    let profiles = Vec::<UpstreamDialectProfile>::deserialize(deserializer)?;
    Ok(profiles
        .into_iter()
        .map(|profile| (profile.key.clone(), profile))
        .collect())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RequestedFeatures {
    pub required: BTreeSet<Capability>,
    pub optional: BTreeSet<Capability>,
    pub explicitly_selected_tool_kind: Option<String>,
    pub allow_reasoning_history_downgrade: bool,
    pub continuation_profile: Option<DialectProfileKey>,
    pub continuation_reasoning_carrier: Option<ReasoningCarrier>,
}

impl RequestedFeatures {
    pub fn text_stream() -> Self {
        Self {
            required: BTreeSet::from([Capability::TextInput, Capability::TextStream]),
            ..Self::default()
        }
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
    pub request_patch: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCapabilities {
    pub values: BTreeMap<Capability, ResolvedCapability>,
    pub token_limit_field: TokenLimitField,
    pub reasoning_mode: ReasoningMode,
    pub reasoning_carrier: ReasoningCarrier,
    pub correction_rules: Vec<DialectCorrectionRule>,
    pub reasoning_control_field: Option<String>,
    pub effort_map: BTreeMap<String, Value>,
    pub omit_sampling_fields: BTreeSet<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub omit_optional_extensions: bool,
    pub profile_state: DialectProfileState,
    pub provisional: bool,
    pub native_preferred: bool,
    pub adapters: BTreeSet<String>,
    pub request_extensions: Vec<ResolvedRequestExtension>,
    pub field_sources: BTreeMap<String, CapabilitySource>,
}

pub(crate) fn baseline_capabilities() -> BTreeMap<Capability, ResolvedCapability> {
    Capability::ALL
        .into_iter()
        .map(|capability| {
            let state = if matches!(
                capability,
                Capability::TextInput
                    | Capability::NonStreamingResponse
                    | Capability::TextStream
                    | Capability::FunctionTools
                    | Capability::ForcedToolChoice
                    | Capability::ToolContinuation
            ) {
                EvidenceState::Supported
            } else {
                EvidenceState::Unobserved
            };
            (
                capability,
                ResolvedCapability {
                    state,
                    source: CapabilitySource::Baseline,
                },
            )
        })
        .collect()
}

/// Static dialect presets used when a route has no probe profile yet.
///
/// - `openai` / `minimax`: OpenAI-compatible neutral baseline (no stripping).
/// - `deepseek`: reasoning via `reasoning_content`, `reasoning_effort` passed
///   through verbatim on the native field.
/// - `glm`: reasoning via the object-valued `thinking` control; `stream_options`
///   is stripped (GLM rejects it).
/// - `generic-strict`: strip every optional non-standard field.
///
/// Presets are a static fallback below probe evidence: a verified profile
/// always wins. The resolved shape uses `DialectProfileState::Partial` so the
/// Auto conservative strip is not applied on top of the preset's own decisions.
pub fn compile_dialect_preset(preset: &str) -> Option<ResolvedCapabilities> {
    let mut values = baseline_capabilities();
    let mut reasoning_carrier = ReasoningCarrier::None;
    let mut reasoning_control_field = None;
    let mut effort_map: BTreeMap<String, Value> = BTreeMap::new();
    let mut omit_sampling_fields = BTreeSet::new();
    let mut omit_optional_extensions = false;

    let parallel_tool_support = |values: &mut BTreeMap<Capability, ResolvedCapability>| {
        values.insert(
            Capability::ParallelToolCalls,
            ResolvedCapability {
                state: EvidenceState::Supported,
                source: CapabilitySource::Policy,
            },
        );
    };
    match preset {
        "openai" | "minimax" => {
            parallel_tool_support(&mut values);
        }
        "deepseek" => {
            for capability in [Capability::ReasoningOutput, Capability::ReasoningReplay] {
                values.insert(
                    capability,
                    ResolvedCapability {
                        state: EvidenceState::Supported,
                        source: CapabilitySource::Policy,
                    },
                );
            }
            parallel_tool_support(&mut values);
            reasoning_carrier = ReasoningCarrier::ReasoningContent;
            reasoning_control_field = Some("reasoning_effort".to_string());
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                effort_map.insert(effort.to_string(), Value::String(effort.to_string()));
            }
        }
        "glm" => {
            for capability in [Capability::ReasoningOutput, Capability::ReasoningReplay] {
                values.insert(
                    capability,
                    ResolvedCapability {
                        state: EvidenceState::Supported,
                        source: CapabilitySource::Policy,
                    },
                );
            }
            parallel_tool_support(&mut values);
            reasoning_carrier = ReasoningCarrier::ReasoningContent;
            reasoning_control_field = Some("thinking".to_string());
            effort_map.insert("low".to_string(), serde_json::json!({ "type": "disabled" }));
            for effort in ["medium", "high", "xhigh", "max"] {
                effort_map.insert(effort.to_string(), serde_json::json!({ "type": "enabled" }));
            }
            omit_sampling_fields.insert("stream_options".to_string());
        }
        "generic-strict" => {
            omit_optional_extensions = true;
            omit_sampling_fields.insert("stream_options".to_string());
        }
        _ => return None,
    }

    let mut field_sources = BTreeMap::from([
        ("token_limit_field".to_owned(), CapabilitySource::Policy),
        (
            "reasoning_carrier".to_owned(),
            if reasoning_carrier == ReasoningCarrier::None {
                CapabilitySource::Baseline
            } else {
                CapabilitySource::Policy
            },
        ),
        (
            "effort_map".to_owned(),
            if effort_map.is_empty() {
                CapabilitySource::Baseline
            } else {
                CapabilitySource::Policy
            },
        ),
        (
            "omit_sampling_fields".to_owned(),
            if omit_sampling_fields.is_empty() {
                CapabilitySource::Baseline
            } else {
                CapabilitySource::Policy
            },
        ),
    ]);
    if omit_optional_extensions {
        field_sources.insert(
            "omit_optional_extensions".to_owned(),
            CapabilitySource::Policy,
        );
    }

    Some(ResolvedCapabilities {
        values,
        token_limit_field: TokenLimitField::MaxTokens,
        reasoning_mode: ReasoningMode::Optional,
        reasoning_carrier,
        correction_rules: Vec::new(),
        reasoning_control_field,
        effort_map,
        omit_sampling_fields,
        context_window: None,
        max_output_tokens: None,
        omit_optional_extensions,
        profile_state: DialectProfileState::Partial,
        provisional: false,
        native_preferred: true,
        adapters: BTreeSet::new(),
        request_extensions: Vec::new(),
        field_sources,
    })
}

impl ResolvedCapabilities {
    pub fn state(&self, capability: Capability) -> EvidenceState {
        self.values
            .get(&capability)
            .map(|resolved| resolved.state)
            .unwrap_or(EvidenceState::Unobserved)
    }

    pub fn source(&self, capability: Capability) -> CapabilitySource {
        self.values
            .get(&capability)
            .map(|resolved| resolved.source)
            .unwrap_or(CapabilitySource::Baseline)
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.state(capability) == EvidenceState::Supported
    }
}
