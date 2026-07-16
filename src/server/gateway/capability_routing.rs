use crate::capabilities::{
    Capability, CapabilityResolver, CapabilityRuntimeSnapshot, CapabilitySource, DialectProfileKey,
    DialectProfileState, ReasoningCarrier, RequestedFeatures, ResolutionInput,
    ResolvedCapabilities, RouteIdentity, SemanticPolicy, WireProtocol,
};
use crate::routing::UpstreamProtocol;
use crate::state::{AppState, UpstreamConfig};
use serde_json::Value;
use std::collections::BTreeSet;

use super::EndpointKind;

const GATEWAY_CONTINUATION_VERSION: u32 = 1;
const PROTOCOL_TRANSITION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProtocolTransitionIdentity {
    schema_version: u32,
    downstream_protocol: WireProtocol,
    upstream_protocol: WireProtocol,
}

impl ProtocolTransitionIdentity {
    pub(super) fn new(downstream_protocol: WireProtocol, upstream_protocol: WireProtocol) -> Self {
        Self {
            schema_version: PROTOCOL_TRANSITION_SCHEMA_VERSION,
            downstream_protocol,
            upstream_protocol,
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GatewayContinuationState {
    version: u32,
    profile_key: DialectProfileKey,
    configuration_fingerprint: String,
    probe_schema_version: u32,
    reasoning_carrier: Option<ReasoningCarrier>,
    required_capabilities: BTreeSet<Capability>,
    adapter_identity: ContinuationAdapterIdentity,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ContinuationAdapterIdentity {
    protocol_transition: ProtocolTransitionIdentity,
    tool_registry_version: Option<u32>,
}

impl GatewayContinuationState {
    pub(super) fn new(
        profile_key: DialectProfileKey,
        configuration_fingerprint: String,
        profile_reasoning_carrier: Option<ReasoningCarrier>,
        required_capabilities: BTreeSet<Capability>,
        downstream_protocol: WireProtocol,
        upstream_protocol: WireProtocol,
        tool_registry_version: Option<u32>,
    ) -> Self {
        Self {
            version: GATEWAY_CONTINUATION_VERSION,
            profile_key,
            configuration_fingerprint,
            probe_schema_version: crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
            reasoning_carrier: profile_reasoning_carrier
                .filter(|carrier| *carrier != ReasoningCarrier::None),
            required_capabilities,
            adapter_identity: ContinuationAdapterIdentity {
                protocol_transition: ProtocolTransitionIdentity::new(
                    downstream_protocol,
                    upstream_protocol,
                ),
                tool_registry_version,
            },
        }
    }

    pub(super) fn validate_version(&self) -> bool {
        self.version == GATEWAY_CONTINUATION_VERSION
    }

    pub(super) fn profile_key(&self) -> &DialectProfileKey {
        &self.profile_key
    }

    pub(super) fn configuration_fingerprint(&self) -> &str {
        &self.configuration_fingerprint
    }

    pub(super) fn probe_schema_version(&self) -> u32 {
        self.probe_schema_version
    }

    pub(super) fn apply_to_requested(&self, requested: &mut RequestedFeatures) {
        requested.continuation_profile = Some(self.profile_key.clone());
        requested.continuation_reasoning_carrier = self.reasoning_carrier;
        requested
            .required
            .extend(self.required_capabilities.iter().copied());
    }

    pub(super) fn observe_reasoning_carrier(&mut self) {
        if self.reasoning_carrier.is_some() {
            return;
        }
        self.reasoning_carrier = Some(
            match self.adapter_identity.protocol_transition.upstream_protocol {
                WireProtocol::ChatCompletions => ReasoningCarrier::ReasoningContent,
                WireProtocol::Responses => ReasoningCarrier::ResponsesReasoningItem,
                WireProtocol::Messages => ReasoningCarrier::MessagesThinking,
            },
        );
    }

    pub(super) fn has_protocol_transition(
        &self,
        downstream_protocol: WireProtocol,
        upstream_protocol: WireProtocol,
    ) -> bool {
        self.adapter_identity.protocol_transition
            == ProtocolTransitionIdentity::new(downstream_protocol, upstream_protocol)
    }

    pub(super) fn tool_registry_version(&self) -> Option<u32> {
        self.adapter_identity.tool_registry_version
    }

    pub(super) fn matches_route(
        &self,
        upstream: &UpstreamConfig,
        exposed_model: &str,
        protocol: UpstreamProtocol,
    ) -> bool {
        self.profile_key.upstream_id == upstream.id
            && self.profile_key.protocol == WireProtocol::from(protocol)
            && upstream.resolved_model_name(exposed_model).as_deref()
                == Some(self.profile_key.runtime_model_slug.as_str())
    }

    pub(super) fn has_current_configuration_fingerprint(
        &self,
        snapshot: &CapabilityRuntimeSnapshot,
        upstreams: &[UpstreamConfig],
        exposed_model: &str,
    ) -> bool {
        upstreams.iter().any(|upstream| {
            upstream.supported_protocols().into_iter().any(|protocol| {
                self.matches_route(upstream, exposed_model, protocol)
                    && AppState::route_configuration_fingerprint_with_snapshot(
                        snapshot,
                        upstream,
                        exposed_model,
                        &self.profile_key.runtime_model_slug,
                        protocol,
                    )
                    .is_ok_and(|fingerprint| fingerprint == self.configuration_fingerprint())
            })
        })
    }

    pub(super) fn has_current_probe_schema(&self, snapshot: &CapabilityRuntimeSnapshot) -> bool {
        snapshot
            .profiles
            .get(self.profile_key())
            .is_some_and(|profile| {
                self.probe_schema_version() == crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION
                    && profile.key == self.profile_key
                    && profile.configuration_fingerprint == self.configuration_fingerprint
                    && profile.probe_schema_version
                        == crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION
            })
    }
}

pub(super) fn request_has_unknown_tool_kind(endpoint: EndpointKind, body: &Value) -> bool {
    if endpoint != EndpointKind::Responses {
        return false;
    }

    let unknown_tool = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                let Some(object) = tool.as_object() else {
                    return true;
                };
                if object.get("function").and_then(Value::as_object).is_some() {
                    return false;
                }
                !matches!(
                    object.get("type").and_then(Value::as_str),
                    Some(
                        "function"
                            | "namespace"
                            | "custom"
                            | "web_search"
                            | "file_search"
                            | "computer_use"
                    )
                )
            })
        });
    let unknown_choice = body
        .get("tool_choice")
        .and_then(Value::as_object)
        .and_then(|choice| choice.get("type").and_then(Value::as_str))
        .is_some_and(|kind| {
            !matches!(
                kind,
                "function" | "namespace" | "custom" | "web_search" | "file_search" | "computer_use"
            )
        });

    unknown_tool || unknown_choice
}

pub(super) fn requested_features_for_request(
    endpoint: EndpointKind,
    body: &Value,
) -> RequestedFeatures {
    let mut required = BTreeSet::new();
    let mut optional = BTreeSet::new();
    let mut explicitly_selected_tool_kind = None;
    match endpoint {
        EndpointKind::Responses => {
            scan_responses_images(body, &mut required);
            scan_responses_tools(
                body,
                &mut required,
                &mut optional,
                &mut explicitly_selected_tool_kind,
            );
            scan_responses_files(body, &mut required);
            scan_responses_reasoning(body, &mut required);
        }
        EndpointKind::ChatCompletions => {
            scan_chat_images(body, &mut required);
            scan_chat_tools(body, &mut required);
            scan_chat_files(body, &mut required);
            scan_chat_reasoning(body, &mut required, &mut optional);
        }
    }
    if body
        .get("parallel_tool_calls")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
    {
        required.insert(Capability::ParallelToolCalls);
    }
    RequestedFeatures {
        required,
        optional,
        explicitly_selected_tool_kind,
        ..RequestedFeatures::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ClaudeThinkingReplayRoute {
    NoReplay,
    Pinned {
        upstream_id: String,
        protocol: UpstreamProtocol,
    },
    InvalidOrUnavailable,
}

#[derive(Clone, Debug)]
struct ClaudeThinkingReplayBlock {
    thinking: String,
    signature: String,
    call_ids: Vec<String>,
}

pub(super) fn claude_thinking_replay_route(
    state: &AppState,
    snapshot: &CapabilityRuntimeSnapshot,
    upstreams: &[UpstreamConfig],
    exposed_model_slug: &str,
    body: &Value,
) -> ClaudeThinkingReplayRoute {
    let mut replay_blocks = Vec::new();
    let mut saw_replay = false;
    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(blocks) = message
            .get("_gateway_claude_thinking")
            .and_then(Value::as_array)
        else {
            continue;
        };
        if blocks.is_empty() {
            continue;
        }
        saw_replay = true;
        let message_call_ids = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|tool_calls| {
                tool_calls
                    .iter()
                    .filter_map(|tool_call| {
                        tool_call
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for block in blocks {
            let Some(thinking) = block
                .get("thinking")
                .and_then(Value::as_str)
                .filter(|thinking| !thinking.is_empty())
            else {
                return ClaudeThinkingReplayRoute::InvalidOrUnavailable;
            };
            let Some(signature) = block
                .get("signature")
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
            else {
                return ClaudeThinkingReplayRoute::InvalidOrUnavailable;
            };
            let call_ids = block
                .get("tool_use_ids")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .filter(|ids| !ids.is_empty())
                .unwrap_or_else(|| message_call_ids.clone());
            replay_blocks.push(ClaudeThinkingReplayBlock {
                thinking: thinking.to_string(),
                signature: signature.to_string(),
                call_ids,
            });
        }
    }
    if !saw_replay {
        return ClaudeThinkingReplayRoute::NoReplay;
    }
    if replay_blocks.is_empty() {
        return ClaudeThinkingReplayRoute::InvalidOrUnavailable;
    }

    let mut matched_route = None;
    for upstream in upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.supports_model(exposed_model_slug))
    {
        let Some(runtime_model_slug) = upstream.resolved_model_name(exposed_model_slug) else {
            continue;
        };
        for protocol in upstream.supported_protocols() {
            let Ok(profile_fingerprint) = AppState::route_configuration_fingerprint_with_snapshot(
                snapshot,
                upstream,
                exposed_model_slug,
                &runtime_model_slug,
                protocol,
            ) else {
                continue;
            };
            let protocol_label = wire_protocol_label(protocol.into());
            let matches = replay_blocks.iter().all(|block| {
                let call_ids = block
                    .call_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let input = super::thinking_signature::ThinkingSignatureInput {
                    thinking: &block.thinking,
                    model: &runtime_model_slug,
                    upstream_id: &upstream.id,
                    protocol: protocol_label,
                    profile_fingerprint: &profile_fingerprint,
                    call_ids: &call_ids,
                };
                super::thinking_signature::verify_thinking(
                    state.config.jwt_secret.as_bytes(),
                    &input,
                    &block.signature,
                )
            });
            if !matches {
                continue;
            }
            if matched_route.is_some() {
                return ClaudeThinkingReplayRoute::InvalidOrUnavailable;
            }
            matched_route = Some((upstream.id.clone(), protocol));
        }
    }
    matched_route
        .map(
            |(upstream_id, protocol)| ClaudeThinkingReplayRoute::Pinned {
                upstream_id,
                protocol,
            },
        )
        .unwrap_or(ClaudeThinkingReplayRoute::InvalidOrUnavailable)
}

pub(super) fn resolve_route_capabilities_with_snapshot(
    snapshot: &CapabilityRuntimeSnapshot,
    upstream: &UpstreamConfig,
    exposed_model_slug: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    requested: &RequestedFeatures,
) -> Option<ResolvedCapabilities> {
    let mut route = RouteIdentity {
        upstream_id: upstream.id.clone(),
        exposed_model_slug: exposed_model_slug.to_string(),
        runtime_model_slug: runtime_model_slug.to_string(),
        protocol: protocol.into(),
        tags: BTreeSet::new(),
    };
    snapshot.configuration.apply_route_tags(&mut route);
    let semantic = snapshot.configuration.semantic_for(&route);
    let route_overrides = snapshot.configuration.route_overrides_for(&route);
    let policy_extensions = snapshot.configuration.extensions_for(&route);
    let effective_profile = exact_route_effective_profile(
        snapshot,
        upstream,
        exposed_model_slug,
        runtime_model_slug,
        protocol,
    );

    let requested = adapt_requested_features_for_protocol(requested, protocol);

    let mut resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: semantic_or_empty(&semantic),
            route_overrides: &route_overrides,
            policy_extensions: &policy_extensions,
            profile: effective_profile.profile,
            strip_nonstandard_chat_fields: upstream.strip_nonstandard_chat_fields,
        })
        .ok()?;
    if let Some(route_context) = upstream.context_config_for_model(exposed_model_slug) {
        resolved.context_window = Some(
            resolved
                .context_window
                .map(|policy| policy.min(u64::from(route_context.context_limit)))
                .unwrap_or(u64::from(route_context.context_limit)),
        );
        resolved
            .field_sources
            .insert("context_window".into(), CapabilitySource::Override);
        if route_context.max_output_tokens > 0 {
            resolved.max_output_tokens = Some(
                resolved
                    .max_output_tokens
                    .map(|policy| policy.min(u64::from(route_context.max_output_tokens)))
                    .unwrap_or(u64::from(route_context.max_output_tokens)),
            );
            resolved
                .field_sources
                .insert("max_output_tokens".into(), CapabilitySource::Override);
        }
    }
    Some(resolved)
}

struct ExactRouteEffectiveProfile<'a> {
    key: DialectProfileKey,
    configuration_fingerprint: Option<String>,
    profile: Option<&'a crate::capabilities::UpstreamDialectProfile>,
}

fn exact_route_effective_profile<'a>(
    snapshot: &'a CapabilityRuntimeSnapshot,
    upstream: &UpstreamConfig,
    exposed_model_slug: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
) -> ExactRouteEffectiveProfile<'a> {
    let key = DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: runtime_model_slug.to_owned(),
        protocol: protocol.into(),
    };
    let configuration_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
        snapshot,
        upstream,
        exposed_model_slug,
        runtime_model_slug,
        protocol,
    )
    .ok();
    let profile = configuration_fingerprint
        .as_deref()
        .and_then(|fingerprint| {
            snapshot.profiles.get(&key).filter(|profile| {
                profile.key == key
                    && profile.configuration_fingerprint == fingerprint
                    && profile.probe_schema_version
                        == crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION
            })
        });
    ExactRouteEffectiveProfile {
        key,
        configuration_fingerprint,
        profile,
    }
}

fn adapt_requested_features_for_protocol(
    requested: &RequestedFeatures,
    protocol: UpstreamProtocol,
) -> RequestedFeatures {
    let mut adapted = requested.clone();
    if protocol == UpstreamProtocol::ChatCompletions {
        let uses_function_adapter = adapted.required.remove(&Capability::NamespaceTools)
            | adapted.required.remove(&Capability::CustomTools);
        if uses_function_adapter {
            adapted.required.insert(Capability::FunctionTools);
        }
    }
    adapted
}

#[allow(dead_code)]
pub(super) fn select_catalog_witness(
    state: &AppState,
    upstreams: &[UpstreamConfig],
    model: &str,
) -> Option<serde_json::Value> {
    select_catalog_witness_entry(state, upstreams, model).map(|entry| entry.diagnostic())
}

pub(super) struct CatalogWitnessEntry {
    pub profile_key: DialectProfileKey,
    pub configuration_fingerprint: Option<String>,
    pub profile_state: DialectProfileState,
    pub probe_schema_version: u32,
    pub profile_rank: u8,
    pub executable_capability_count: usize,
    pub native_protocol_fidelity: u8,
    pub upstream_priority: u32,
    pub upstream_failure_count: u32,
    pub capabilities: ResolvedCapabilities,
}

pub(super) fn is_compatible_catalog_superset(
    candidate: &ResolvedCapabilities,
    witness: &ResolvedCapabilities,
    candidate_transition: ProtocolTransitionIdentity,
    witness_transition: ProtocolTransitionIdentity,
) -> bool {
    candidate.profile_state == DialectProfileState::Verified
        && candidate_transition == witness_transition
        && candidate.reasoning_carrier == witness.reasoning_carrier
        && witness.values.iter().all(|(capability, resolved)| {
            resolved.state != crate::capabilities::EvidenceState::Supported
                || candidate.supports(*capability)
        })
}

impl CatalogWitnessEntry {
    pub fn diagnostic(&self) -> serde_json::Value {
        serde_json::json!({
            "upstream_id": self.profile_key.upstream_id,
            "runtime_model_slug": self.profile_key.runtime_model_slug,
            "protocol": wire_protocol_label(self.profile_key.protocol),
            "profile_key": self.profile_key,
            "configuration_fingerprint": self.configuration_fingerprint,
            "profile_state": profile_state_label(self.profile_state),
            "probe_schema_version": self.probe_schema_version,
            "rank": {
                "profile": self.profile_rank,
                "executable_capabilities": self.executable_capability_count,
                "native_protocol_fidelity": self.native_protocol_fidelity,
                "upstream_priority": self.upstream_priority,
                "upstream_failure_count": self.upstream_failure_count,
            },
        })
    }
}

pub(super) fn select_catalog_witness_entry(
    state: &AppState,
    upstreams: &[UpstreamConfig],
    model: &str,
) -> Option<CatalogWitnessEntry> {
    let snapshot = state.capability_snapshot();
    upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.supports_model(model))
        .filter_map(|upstream| {
            upstream
                .resolved_model_name(model)
                .map(|runtime_model_slug| (upstream, runtime_model_slug))
        })
        .flat_map(|(upstream, runtime_model_slug)| {
            let snapshot = &snapshot;
            upstream
                .supported_protocols()
                .into_iter()
                .filter_map(move |protocol| {
                    let effective_profile = exact_route_effective_profile(
                        snapshot,
                        upstream,
                        model,
                        &runtime_model_slug,
                        protocol,
                    );
                    let resolved = resolve_route_capabilities_with_snapshot(
                        snapshot,
                        upstream,
                        model,
                        &runtime_model_slug,
                        protocol,
                        &RequestedFeatures::default(),
                    )?;
                    if resolved.profile_state == DialectProfileState::Unsupported {
                        return None;
                    }
                    if !resolved.supports(Capability::FunctionTools)
                        || !resolved.supports(Capability::ToolContinuation)
                    {
                        return None;
                    }
                    let rank = match resolved.profile_state {
                        DialectProfileState::Verified => 3u8,
                        DialectProfileState::Partial => 2u8,
                        DialectProfileState::Unknown => 1u8,
                        DialectProfileState::Unsupported => 0u8,
                    };
                    let supported = resolved
                        .values
                        .values()
                        .filter(|value| {
                            value.state == crate::capabilities::EvidenceState::Supported
                        })
                        .count();
                    Some((
                        rank,
                        supported,
                        u8::from(effective_profile.key.protocol == WireProtocol::Responses),
                        upstream.priority,
                        std::cmp::Reverse(upstream.failure_count),
                        upstream.id.clone(),
                        effective_profile.key,
                        effective_profile.configuration_fingerprint,
                        resolved,
                    ))
                })
        })
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.cmp(&right.1))
                .then(left.2.cmp(&right.2))
                .then(left.3.cmp(&right.3))
                .then(left.4.cmp(&right.4))
                .then_with(|| right.5.cmp(&left.5))
        })
        .map(
            |(
                rank,
                supported,
                native_fidelity,
                priority,
                failure_count,
                _,
                key,
                configuration_fingerprint,
                resolved,
            )| CatalogWitnessEntry {
                profile_key: key,
                configuration_fingerprint,
                profile_state: resolved.profile_state,
                probe_schema_version: crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
                profile_rank: rank,
                executable_capability_count: supported,
                native_protocol_fidelity: native_fidelity,
                upstream_priority: priority,
                upstream_failure_count: failure_count.0,
                capabilities: resolved,
            },
        )
}

fn scan_responses_images(body: &Value, required: &mut BTreeSet<Capability>) {
    let Some(input) = body.get("input").and_then(Value::as_array) else {
        return;
    };
    for item in input {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) != Some("input_image") {
                continue;
            }
            if let Some(url) = part.get("image_url").and_then(Value::as_str) {
                if url.starts_with("https://") {
                    required.insert(Capability::ImageHttps);
                } else if url.starts_with("data:") {
                    required.insert(Capability::ImageDataUrl);
                }
            }
        }
    }
}

fn scan_chat_images(body: &Value, required: &mut BTreeSet<Capability>) {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return;
    };
    for message in messages {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) != Some("image_url") {
                continue;
            }
            let image_url = part.get("image_url").and_then(|value| match value {
                Value::String(value) => Some(value.as_str()),
                Value::Object(object) => object.get("url").and_then(Value::as_str),
                _ => None,
            });
            if let Some(url) = image_url {
                if url.starts_with("https://") {
                    required.insert(Capability::ImageHttps);
                } else if url.starts_with("data:") {
                    required.insert(Capability::ImageDataUrl);
                }
            }
        }
    }
}

fn scan_responses_files(body: &Value, required: &mut BTreeSet<Capability>) {
    let Some(input) = body.get("input") else {
        return;
    };

    scan_file_capabilities(input, required);
}

fn scan_chat_files(body: &Value, required: &mut BTreeSet<Capability>) {
    let Some(messages) = body.get("messages") else {
        return;
    };

    scan_file_capabilities(messages, required);
}

fn scan_file_capabilities(value: &Value, required: &mut BTreeSet<Capability>) {
    match value {
        Value::Array(items) => {
            for item in items {
                scan_file_capabilities(item, required);
            }
        }
        Value::Object(object) => {
            if object.get("file_id").and_then(Value::as_str).is_some()
                && matches!(
                    object.get("type").and_then(Value::as_str),
                    Some("file") | Some("input_file")
                )
            {
                required.insert(Capability::NativeFileId);
            }

            if let Some(content) = object.get("content") {
                scan_file_capabilities(content, required);
            }

            if let Some(input) = object.get("input") {
                scan_file_capabilities(input, required);
            }

            if let Some(messages) = object.get("messages") {
                scan_file_capabilities(messages, required);
            }
        }
        _ => {}
    }
}

fn scan_responses_tools(
    body: &Value,
    required: &mut BTreeSet<Capability>,
    optional: &mut BTreeSet<Capability>,
    explicitly_selected_tool_kind: &mut Option<String>,
) {
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            match tool.get("type").and_then(Value::as_str) {
                Some("web_search" | "file_search" | "computer_use") => {
                    optional.insert(Capability::HostedTools);
                }
                Some("namespace") => {
                    required.insert(Capability::NamespaceTools);
                }
                Some("custom") => {
                    required.insert(Capability::CustomTools);
                }
                Some("function") | None => {
                    required.insert(Capability::FunctionTools);
                }
                Some(_) => {}
            }
        }
    }

    match body.get("tool_choice") {
        Some(Value::String(choice)) => {
            if choice == "required" {
                required.insert(Capability::ForcedToolChoice);
            } else if matches!(
                choice.as_str(),
                "web_search" | "file_search" | "computer_use"
            ) {
                required.insert(Capability::ForcedToolChoice);
                optional.remove(&Capability::HostedTools);
                required.insert(Capability::HostedTools);
                *explicitly_selected_tool_kind = Some(choice.clone());
            }
        }
        Some(Value::Object(choice)) => {
            required.insert(Capability::ForcedToolChoice);
            if let Some(kind) = choice.get("type").and_then(Value::as_str) {
                *explicitly_selected_tool_kind = Some(kind.to_string());
                if matches!(kind, "web_search" | "file_search" | "computer_use") {
                    optional.remove(&Capability::HostedTools);
                    required.insert(Capability::HostedTools);
                } else if kind == "namespace" {
                    required.insert(Capability::NamespaceTools);
                } else if kind == "custom" {
                    required.insert(Capability::CustomTools);
                } else if kind == "function" {
                    required.insert(Capability::FunctionTools);
                }
            }
        }
        _ => {}
    }
}

fn scan_responses_reasoning(body: &Value, required: &mut BTreeSet<Capability>) {
    fn scan(value: &Value, required: &mut BTreeSet<Capability>) {
        match value {
            Value::Array(items) => {
                for item in items {
                    scan(item, required);
                }
            }
            Value::Object(object) => {
                match object.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        required.insert(Capability::ReasoningOutput);
                        required.insert(Capability::ReasoningReplay);
                    }
                    Some("function_call")
                        if object
                            .get("namespace")
                            .and_then(Value::as_str)
                            .is_some_and(|namespace| !namespace.is_empty()) =>
                    {
                        required.insert(Capability::NamespaceTools);
                    }
                    Some("custom_tool_call") => {
                        required.insert(Capability::CustomTools);
                    }
                    Some("function_call_output" | "custom_tool_call_output") => {
                        required.insert(Capability::ToolContinuation);
                    }
                    _ => {}
                }
                if let Some(content) = object.get("content") {
                    scan(content, required);
                }
            }
            _ => {}
        }
    }

    if let Some(input) = body.get("input") {
        scan(input, required);
    }
}

fn scan_chat_tools(body: &Value, required: &mut BTreeSet<Capability>) {
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        required.insert(Capability::FunctionTools);
    }
    match body.get("tool_choice") {
        Some(Value::String(choice)) if choice == "required" => {
            required.insert(Capability::ForcedToolChoice);
        }
        Some(Value::Object(_)) => {
            required.insert(Capability::ForcedToolChoice);
        }
        _ => {}
    }
}

fn scan_chat_reasoning(
    body: &Value,
    required: &mut BTreeSet<Capability>,
    optional: &mut BTreeSet<Capability>,
) {
    let adaptive_claude_thinking = body
        .pointer("/_gateway_claude/thinking/type")
        .and_then(Value::as_str)
        == Some("adaptive");
    let explicit_reasoning =
        body.get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .is_some_and(|thinking| !thinking.is_empty())
                        || message
                            .get("_gateway_claude_thinking")
                            .and_then(Value::as_array)
                            .is_some_and(|blocks| !blocks.is_empty())
                })
            });

    if explicit_reasoning {
        required.insert(Capability::ReasoningOutput);
        required.insert(Capability::ReasoningReplay);
    } else if adaptive_claude_thinking {
        optional.insert(Capability::ReasoningOutput);
        optional.insert(Capability::ReasoningReplay);
    }
}

fn wire_protocol_label(protocol: WireProtocol) -> &'static str {
    match protocol {
        WireProtocol::ChatCompletions => "chat_completions",
        WireProtocol::Responses => "responses",
        WireProtocol::Messages => "messages",
    }
}

fn profile_state_label(state: DialectProfileState) -> &'static str {
    match state {
        DialectProfileState::Verified => "verified",
        DialectProfileState::Partial => "partial",
        DialectProfileState::Unsupported => "unsupported",
        DialectProfileState::Unknown => "unknown",
    }
}

fn semantic_or_empty(semantic: &SemanticPolicy) -> &SemanticPolicy {
    semantic
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn verified_capabilities(reasoning_carrier: ReasoningCarrier) -> ResolvedCapabilities {
        ResolvedCapabilities {
            values: std::collections::BTreeMap::from([(
                Capability::FunctionTools,
                crate::capabilities::ResolvedCapability {
                    state: crate::capabilities::EvidenceState::Supported,
                    source: CapabilitySource::Probe,
                },
            )]),
            token_limit_field: crate::capabilities::TokenLimitField::Omit,
            reasoning_mode: crate::capabilities::ReasoningMode::Off,
            reasoning_carrier,
            correction_rules: Vec::new(),
            reasoning_control_field: None,
            effort_map: std::collections::BTreeMap::new(),
            omit_sampling_fields: BTreeSet::new(),
            context_window: None,
            max_output_tokens: None,
            omit_optional_extensions: false,
            profile_state: DialectProfileState::Verified,
            provisional: false,
            native_preferred: true,
            adapters: BTreeSet::new(),
            request_extensions: Vec::new(),
            field_sources: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn catalog_superset_requires_matching_explicit_protocol_transition() {
        let witness = verified_capabilities(ReasoningCarrier::ReasoningContent);
        let candidate = witness.clone();
        let responses_to_chat =
            ProtocolTransitionIdentity::new(WireProtocol::Responses, WireProtocol::ChatCompletions);
        let responses_to_responses =
            ProtocolTransitionIdentity::new(WireProtocol::Responses, WireProtocol::Responses);

        assert!(is_compatible_catalog_superset(
            &candidate,
            &witness,
            responses_to_chat,
            responses_to_chat,
        ));
        assert!(!is_compatible_catalog_superset(
            &candidate,
            &witness,
            responses_to_responses,
            responses_to_chat,
        ));
    }

    #[test]
    fn responses_tool_scan_distinguishes_adaptable_and_hosted_kinds() {
        let requested = requested_features_for_request(
            EndpointKind::Responses,
            &json!({
                "tools": [
                    {"type": "function", "name": "read_file"},
                    {"type": "namespace", "name": "mcp", "tools": []},
                    {"type": "custom", "name": "apply_patch"},
                    {"type": "web_search"}
                ],
                "tool_choice": "auto"
            }),
        );

        assert_eq!(
            requested.required,
            BTreeSet::from([
                Capability::FunctionTools,
                Capability::NamespaceTools,
                Capability::CustomTools,
            ])
        );
        assert_eq!(
            requested.optional,
            BTreeSet::from([Capability::HostedTools])
        );
        assert_eq!(requested.explicitly_selected_tool_kind, None);
    }

    #[test]
    fn chat_auto_tool_choice_does_not_require_forced_selection() {
        let requested = requested_features_for_request(
            EndpointKind::ChatCompletions,
            &json!({
                "tools": [{"type": "function", "function": {"name": "read_file"}}],
                "tool_choice": "auto"
            }),
        );

        assert_eq!(
            requested.required,
            BTreeSet::from([Capability::FunctionTools])
        );
    }

    #[test]
    fn parallel_tool_calls_true_requires_parallel_capability() {
        for endpoint in [EndpointKind::ChatCompletions, EndpointKind::Responses] {
            let requested = requested_features_for_request(
                endpoint,
                &json!({
                    "tools": [{"type": "function", "name": "read_file"}],
                    "parallel_tool_calls": true
                }),
            );
            assert_eq!(
                requested.required,
                BTreeSet::from([Capability::FunctionTools, Capability::ParallelToolCalls])
            );
        }
    }

    #[test]
    fn parallel_tool_calls_false_does_not_require_parallel_capability() {
        for endpoint in [EndpointKind::ChatCompletions, EndpointKind::Responses] {
            let requested = requested_features_for_request(
                endpoint,
                &json!({
                    "tools": [{"type": "function", "name": "read_file"}],
                    "parallel_tool_calls": false
                }),
            );
            assert!(!requested.required.contains(&Capability::ParallelToolCalls));
        }
    }

    #[test]
    fn responses_reasoning_tool_continuation_requires_replay_capabilities() {
        let requested = requested_features_for_request(
            EndpointKind::Responses,
            &json!({
                "input": [
                    {"type": "reasoning", "id": "rs_1", "summary": []},
                    {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
                ]
            }),
        );

        assert!(requested.required.contains(&Capability::ReasoningOutput));
        assert!(requested.required.contains(&Capability::ReasoningReplay));
        assert!(requested.required.contains(&Capability::ToolContinuation));
    }

    #[test]
    fn responses_replayed_tool_calls_require_their_native_tool_capabilities() {
        let requested = requested_features_for_request(
            EndpointKind::Responses,
            &json!({
                "input": [
                    {
                        "type": "function_call",
                        "call_id": "call_namespace",
                        "name": "search",
                        "namespace": "mcp",
                        "arguments": "{}"
                    },
                    {
                        "type": "custom_tool_call",
                        "call_id": "call_custom",
                        "name": "apply_patch",
                        "input": "*** Begin Patch"
                    }
                ]
            }),
        );

        assert!(requested.required.contains(&Capability::NamespaceTools));
        assert!(requested.required.contains(&Capability::CustomTools));

        let native = adapt_requested_features_for_protocol(&requested, UpstreamProtocol::Responses);
        assert!(native.required.contains(&Capability::NamespaceTools));
        assert!(native.required.contains(&Capability::CustomTools));

        let adapted =
            adapt_requested_features_for_protocol(&requested, UpstreamProtocol::ChatCompletions);
        assert!(adapted.required.contains(&Capability::FunctionTools));
        assert!(!adapted.required.contains(&Capability::NamespaceTools));
        assert!(!adapted.required.contains(&Capability::CustomTools));
    }

    #[test]
    fn initial_claude_adaptive_thinking_is_optional_without_replay_history() {
        let requested = requested_features_for_request(
            EndpointKind::ChatCompletions,
            &json!({
                "_gateway_claude": {"thinking": {"type": "adaptive"}},
                "messages": [{"role": "user", "content": "hello"}]
            }),
        );

        assert!(!requested.required.contains(&Capability::ReasoningOutput));
        assert!(!requested.required.contains(&Capability::ReasoningReplay));
        assert!(requested.optional.contains(&Capability::ReasoningOutput));
        assert!(requested.optional.contains(&Capability::ReasoningReplay));
    }

    #[test]
    fn claude_thinking_history_requires_output_and_replay_capabilities() {
        let requested = requested_features_for_request(
            EndpointKind::ChatCompletions,
            &json!({
                "_gateway_claude": {"thinking": {"type": "adaptive"}},
                "messages": [{
                    "role": "assistant",
                    "content": null,
                    "_gateway_claude_thinking": [{
                        "thinking": "preserve exactly",
                        "signature": "gw1.signature",
                        "tool_use_ids": ["toolu_1"]
                    }],
                    "tool_calls": [{"id": "toolu_1", "type": "function"}]
                }]
            }),
        );

        assert!(requested.required.contains(&Capability::ReasoningOutput));
        assert!(requested.required.contains(&Capability::ReasoningReplay));
    }
}
