use crate::capabilities::{
    Capability, CapabilityResolver, DialectProfileKey, DialectProfileState, RequestedFeatures,
    ResolvedCapabilities, ResolutionInput, RouteIdentity, SemanticPolicy, WireProtocol,
};
use crate::routing::UpstreamProtocol;
use crate::state::{AppState, UpstreamConfig};
use serde_json::Value;
use std::collections::BTreeSet;

use super::EndpointKind;

pub(super) fn required_capabilities_for_request(
    endpoint: EndpointKind,
    body: &Value,
) -> BTreeSet<Capability> {
    requested_features_for_request(endpoint, body).required
}

pub(super) fn requested_features_for_request(
    endpoint: EndpointKind,
    body: &Value,
) -> RequestedFeatures {
    let mut required = BTreeSet::new();
    match endpoint {
        EndpointKind::Responses => {
            scan_responses_images(body, &mut required);
            scan_responses_tools(body, &mut required);
        }
        EndpointKind::ChatCompletions => {
            scan_chat_images(body, &mut required);
            scan_chat_tools(body, &mut required);
        }
    }
    RequestedFeatures {
        required,
        ..RequestedFeatures::default()
    }
}

pub(super) fn any_route_supports_required_capabilities(
    state: &AppState,
    upstreams: &[UpstreamConfig],
    model: &str,
    required: &BTreeSet<Capability>,
) -> bool {
    if required.is_empty() {
        return true;
    }
    upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.supports_model(model))
        .any(|upstream| {
            upstream.supported_protocols().into_iter().any(|protocol| {
                route_supports_required_capabilities(state, upstream, model, protocol, required)
            })
        })
}

pub(super) fn route_supports_required_capabilities(
    state: &AppState,
    upstream: &UpstreamConfig,
    exposed_model_slug: &str,
    protocol: UpstreamProtocol,
    required: &BTreeSet<Capability>,
) -> bool {
    if required.is_empty() {
        return true;
    }
    let Some(runtime_model_slug) = upstream.resolved_model_name(exposed_model_slug) else {
        return false;
    };

    let snapshot = state.capability_snapshot();
    let mut route = RouteIdentity {
        upstream_id: upstream.id.clone(),
        exposed_model_slug: exposed_model_slug.to_string(),
        runtime_model_slug,
        protocol: protocol.into(),
        tags: BTreeSet::new(),
    };
    snapshot.configuration.apply_route_tags(&mut route);
    let semantic = snapshot.configuration.semantic_for(&route);
    let route_overrides = snapshot.configuration.route_overrides_for(&route);
    let policy_extensions = snapshot.configuration.extensions_for(&route);
    let requested = RequestedFeatures {
        required: required.clone(),
        ..RequestedFeatures::default()
    };
    let profile_key = DialectProfileKey::from_route(&route);
    let profile = snapshot.profiles.get(&profile_key);

    CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: semantic_or_empty(&semantic),
            route_overrides: &route_overrides,
            policy_extensions: &policy_extensions,
            profile,
            strip_nonstandard_chat_fields: upstream.strip_nonstandard_chat_fields,
        })
        .is_ok()
}

pub(super) fn resolve_route_capabilities(
    state: &AppState,
    upstream: &UpstreamConfig,
    exposed_model_slug: &str,
    protocol: UpstreamProtocol,
    requested: &RequestedFeatures,
) -> Option<ResolvedCapabilities> {
    let Some(runtime_model_slug) = upstream.resolved_model_name(exposed_model_slug) else {
        return None;
    };

    let snapshot = state.capability_snapshot();
    let mut route = RouteIdentity {
        upstream_id: upstream.id.clone(),
        exposed_model_slug: exposed_model_slug.to_string(),
        runtime_model_slug,
        protocol: protocol.into(),
        tags: BTreeSet::new(),
    };
    snapshot.configuration.apply_route_tags(&mut route);
    let semantic = snapshot.configuration.semantic_for(&route);
    let route_overrides = snapshot.configuration.route_overrides_for(&route);
    let policy_extensions = snapshot.configuration.extensions_for(&route);
    let profile_key = DialectProfileKey::from_route(&route);
    let profile = snapshot.profiles.get(&profile_key);

    CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested,
            semantic: semantic_or_empty(&semantic),
            route_overrides: &route_overrides,
            policy_extensions: &policy_extensions,
            profile,
            strip_nonstandard_chat_fields: upstream.strip_nonstandard_chat_fields,
        })
        .ok()
}

pub(super) fn select_catalog_witness(
    state: &AppState,
    upstreams: &[UpstreamConfig],
    model: &str,
) -> Option<serde_json::Value> {
    select_catalog_witness_entry(state, upstreams, model).map(|entry| entry.witness)
}

pub(super) struct CatalogWitnessEntry {
    pub witness: serde_json::Value,
    pub image_supported: bool,
    pub parallel_tool_calls_supported: bool,
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
            let runtime_model_slug = upstream.resolved_model_name(model)?;
            let protocol = upstream.supported_protocols().into_iter().next()?;
            let key = DialectProfileKey {
                upstream_id: upstream.id.clone(),
                runtime_model_slug,
                protocol: protocol.into(),
            };
            let profile = snapshot.profiles.get(&key);
            let profile_state = profile
                .map(|profile| profile.state)
                .unwrap_or(DialectProfileState::Unknown);
            let rank = match profile_state {
                DialectProfileState::Verified => 3u8,
                DialectProfileState::Partial => 2u8,
                DialectProfileState::Unsupported => 1u8,
                DialectProfileState::Unknown => 0u8,
            };
            let supported = profile
                .map(|profile| {
                    profile
                        .capabilities
                        .values()
                        .filter(|value| {
                            **value == crate::capabilities::EvidenceState::Supported
                        })
                        .count()
                })
                .unwrap_or_default();
            Some((rank, supported, upstream.id.clone(), key, profile.cloned()))
        })
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.cmp(&right.1))
                .then_with(|| right.2.cmp(&left.2))
        })
        .map(|(_, _, upstream_id, key, profile)| CatalogWitnessEntry {
            witness: serde_json::json!({
                "upstream_id": upstream_id,
                "protocol": wire_protocol_label(key.protocol),
                "profile_state": profile_state_label(profile.as_ref().map(|profile| profile.state).unwrap_or(DialectProfileState::Unknown)),
                "probe_version": profile.as_ref().map(|profile| profile.probe_schema_version).unwrap_or(crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION)
            }),
            image_supported: profile.as_ref().is_some_and(|profile| {
                profile.capabilities.get(&Capability::ImageHttps).copied()
                    == Some(crate::capabilities::EvidenceState::Supported)
                    || profile.capabilities.get(&Capability::ImageDataUrl).copied()
                        == Some(crate::capabilities::EvidenceState::Supported)
            }),
            parallel_tool_calls_supported: profile.as_ref().is_some_and(|profile| {
                profile.capabilities.get(&Capability::ParallelToolCalls).copied()
                    == Some(crate::capabilities::EvidenceState::Supported)
            }),
        })
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
            let image_url = part
                .get("image_url")
                .and_then(|value| match value {
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

fn scan_responses_tools(body: &Value, required: &mut BTreeSet<Capability>) {
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        required.insert(Capability::FunctionTools);
    }
    if body
        .get("tool_choice")
        .and_then(Value::as_str)
        .is_some_and(|choice| choice == "required")
    {
        required.insert(Capability::ForcedToolChoice);
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
    if body.get("tool_choice").is_some() {
        required.insert(Capability::ForcedToolChoice);
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
