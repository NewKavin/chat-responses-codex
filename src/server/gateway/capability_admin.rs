use super::*;
use crate::capabilities::{
    Capability, CapabilityConfiguration, CapabilityResolver, DialectProfileKey, ProbeReason,
    RequestedFeatures, ResolutionInput, RouteIdentity, UpstreamDialectProfile, WireProtocol,
};
use crate::keys::upstream_key_fingerprint;
use axum::extract::{Path, Query};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub(super) struct CapabilityResolvedQuery {
    upstream_id: String,
    model: String,
    protocol: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ManualProbeRequest {
    upstream_id: String,
    #[serde(default)]
    exposed_model_slug: Option<String>,
    runtime_model_slug: String,
    protocol: String,
}

pub(super) async fn admin_capabilities_export(State(state): State<AppState>) -> Response {
    let snapshot = state.capability_snapshot();
    let mut configuration = snapshot.configuration.source().clone();
    crate::capabilities::sanitize_sensitive_urls(&mut configuration);
    (StatusCode::OK, Json(configuration)).into_response()
}

pub(super) async fn admin_capabilities_import(
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => {
            return capability_import_error().into_response();
        }
    };
    let configuration = match serde_json::from_value::<CapabilityConfiguration>(body) {
        Ok(configuration) => configuration,
        Err(_) => return capability_import_error().into_response(),
    };
    if configuration.compile().is_err() {
        return capability_import_error().into_response();
    }
    if state
        .replace_capability_configuration(configuration)
        .await
        .is_err()
    {
        return capability_persist_error();
    }
    Json(json!({"ok": true})).into_response()
}

pub(super) async fn admin_capability_profiles(State(state): State<AppState>) -> Response {
    let snapshot = state.capability_snapshot();
    let routing = state.routing_snapshot().await;
    let now = unix_seconds();
    let profiles = snapshot
        .profiles
        .values()
        .map(|profile| {
            capability_profile_summary(
                profile,
                now,
                profile_is_current_for_any_route(&snapshot, &routing.upstreams, profile),
            )
        })
        .collect::<Vec<_>>();
    Json(json!({"profiles": profiles})).into_response()
}

pub(super) async fn admin_capabilities_resolved(
    State(state): State<AppState>,
    Query(query): Query<CapabilityResolvedQuery>,
) -> Response {
    let routing = state.routing_snapshot().await;
    let Some(upstream) = routing
        .upstreams
        .iter()
        .find(|upstream| upstream.id == query.upstream_id)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"message": "upstream not found"}})),
        )
            .into_response();
    };
    let protocol = match parse_upstream_protocol(&query.protocol) {
        Some(protocol) => protocol,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": "invalid protocol"}})),
            )
                .into_response();
        }
    };
    let Some(runtime_model_slug) = upstream.resolved_model_name(&query.model) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"message": "model not configured for upstream"}})),
        )
            .into_response();
    };

    let capability_snapshot = state.capability_snapshot();
    let key_fingerprint = upstream
        .keys_for_model(&query.model)
        .first()
        .map(|api_key| upstream_key_fingerprint(&upstream.id, api_key))
        .unwrap_or_default();
    let mut route = RouteIdentity {
        upstream_id: upstream.id.clone(),
        key_fingerprint: key_fingerprint.clone(),
        exposed_model_slug: query.model.clone(),
        runtime_model_slug: runtime_model_slug.clone(),
        protocol: WireProtocol::from(protocol),
        tags: Default::default(),
    };
    capability_snapshot
        .configuration
        .apply_route_tags(&mut route);
    let semantic = capability_snapshot.configuration.semantic_for(&route);
    let route_overrides = capability_snapshot
        .configuration
        .route_overrides_for(&route);
    let policy_extensions = capability_snapshot.configuration.extensions_for(&route);
    let profile_key = DialectProfileKey::from_route(&route);
    let current_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
        &capability_snapshot,
        upstream,
        &key_fingerprint,
        &query.model,
        &runtime_model_slug,
        protocol,
    )
    .ok();
    let raw_profile = capability_snapshot.profiles.get(&profile_key);
    let profile = raw_profile.filter(|profile| {
        current_fingerprint.as_deref().is_some_and(|fingerprint| {
            profile_is_current_for_route(
                &capability_snapshot,
                upstream,
                &query.model,
                &runtime_model_slug,
                protocol,
                profile,
                fingerprint,
            )
        })
    });
    let now = unix_seconds();
    let resolved = match CapabilityResolver.resolve(ResolutionInput {
        route: &route,
        requested: &RequestedFeatures::default(),
        semantic: &semantic,
        route_overrides: &route_overrides,
        policy_extensions: &policy_extensions,
        profile,
        strip_nonstandard_chat_fields: upstream.strip_nonstandard_chat_fields,
    }) {
        Ok(resolved) => resolved,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": error.to_string()}})),
            )
                .into_response();
        }
    };

    let capabilities = Capability::ALL
        .into_iter()
        .map(|capability| {
            (
                enum_string(capability),
                json!({
                    "state": enum_string(resolved.state(capability)),
                    "source": enum_string(resolved.source(capability)),
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let profile_age_seconds = profile
        .and_then(|profile| profile.last_success_at.or(profile.last_attempt_at))
        .map(|at| now.saturating_sub(at));
    let profile_currentness = profile_currentness_label(raw_profile.is_some(), profile.is_some());
    let field_sources = resolved
        .field_sources
        .iter()
        .map(|(field, source)| (field.clone(), enum_string(*source)))
        .collect::<BTreeMap<_, _>>();
    let conflicts = resolved_conflicts(profile, &route_overrides);
    Json(json!({
        "configuration_revision": capability_snapshot.configuration.source().revision,
        "configuration_fingerprint": safe_fingerprint(capability_snapshot.configuration.digest()),
        "capabilities": capabilities,
        "profile_age_seconds": profile_age_seconds,
        "profile_currentness": profile_currentness,
        "profile_state": enum_string(resolved.profile_state),
        "profile": {
            "currentness": profile_currentness,
            "state": enum_string(resolved.profile_state),
            "age_seconds": profile_age_seconds,
            "fingerprint": profile.and_then(|profile| safe_fingerprint(&profile.configuration_fingerprint)),
        },
        "field_sources": field_sources,
        "token": {
            "field": enum_string(resolved.token_limit_field),
            "source": enum_string(*resolved.field_sources.get("token_limit_field").unwrap_or(&crate::capabilities::CapabilitySource::Baseline)),
        },
        "reasoning": {
            "mode": enum_string(resolved.reasoning_mode),
            "carrier": enum_string(resolved.reasoning_carrier),
            "control_field": resolved.reasoning_control_field,
            "source": enum_string(*resolved.field_sources.get("reasoning_carrier").unwrap_or(&crate::capabilities::CapabilitySource::Baseline)),
        },
        "extensions": {
            "ids": resolved.request_extensions.iter().map(|extension| sanitize_identifier(&extension.id)).collect::<Vec<_>>(),
            "source": enum_string(*resolved.field_sources.get("request_extensions").unwrap_or(&crate::capabilities::CapabilitySource::Baseline)),
        },
        "conflicts": conflicts,
    }))
    .into_response()
}

pub(super) async fn admin_capability_probe(
    State(state): State<AppState>,
    Json(body): Json<ManualProbeRequest>,
) -> Response {
    let Some(protocol) = parse_wire_protocol(&body.protocol) else {
        return capability_probe_error(
            StatusCode::BAD_REQUEST,
            "gateway_capability_probe_invalid_route",
        );
    };
    let exposed_model_slug = body
        .exposed_model_slug
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| body.runtime_model_slug.clone());
    let upstream_protocol = match protocol {
        WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
        WireProtocol::Responses => UpstreamProtocol::Responses,
        WireProtocol::Messages => {
            return capability_probe_error(
                StatusCode::BAD_REQUEST,
                "gateway_capability_probe_invalid_route",
            )
        }
    };
    let job = match state
        .build_capability_probe_job(
            &body.upstream_id,
            &exposed_model_slug,
            &body.runtime_model_slug,
            upstream_protocol,
            ProbeReason::Manual,
        )
        .await
    {
        Ok(Some(job)) => job,
        Ok(None) | Err(_) => {
            return capability_probe_error(
                StatusCode::BAD_REQUEST,
                "gateway_capability_probe_invalid_route",
            )
        }
    };
    if !state.queue_capability_probe(job) {
        return capability_probe_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_capability_probe_unavailable",
        );
    }
    (StatusCode::ACCEPTED, Json(json!({"queued": true}))).into_response()
}

pub(super) async fn admin_capability_profiles_delete(
    State(state): State<AppState>,
    Path(upstream_id): Path<String>,
) -> Response {
    match state
        .delete_dialect_profiles_for_upstream(&upstream_id)
        .await
    {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": error.to_string()}})),
        )
            .into_response(),
    }
}

fn capability_import_error() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": {
            "code": "gateway_capability_policy_invalid",
            "message": "capability policy is invalid"
        }})),
    )
}

fn capability_persist_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": {
            "code": "gateway_capability_policy_persist_failed",
            "message": "capability policy could not be persisted"
        }})),
    )
        .into_response()
}

fn capability_probe_error(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error": {
            "code": code,
            "message": "capability probe is unavailable for this route"
        }})),
    )
        .into_response()
}

fn parse_upstream_protocol(value: &str) -> Option<UpstreamProtocol> {
    match value.trim() {
        "chat_completions" => Some(UpstreamProtocol::ChatCompletions),
        "responses" => Some(UpstreamProtocol::Responses),
        _ => None,
    }
}

fn parse_wire_protocol(value: &str) -> Option<WireProtocol> {
    match value.trim() {
        "chat_completions" => Some(WireProtocol::ChatCompletions),
        "responses" => Some(WireProtocol::Responses),
        _ => None,
    }
}

fn profile_is_current_for_any_route(
    snapshot: &crate::capabilities::CapabilityRuntimeSnapshot,
    upstreams: &[crate::state::UpstreamConfig],
    profile: &UpstreamDialectProfile,
) -> bool {
    let protocol = match profile.key.protocol {
        WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
        WireProtocol::Responses => UpstreamProtocol::Responses,
        WireProtocol::Messages => return false,
    };
    upstreams
        .iter()
        .filter(|upstream| {
            upstream.id == profile.key.upstream_id
                && upstream.active
                && upstream.supports_protocol(protocol)
        })
        .flat_map(|upstream| {
            upstream
                .route_models()
                .into_iter()
                .filter_map(move |exposed| {
                    (upstream.resolved_model_name(&exposed).as_deref()
                        == Some(profile.key.runtime_model_slug.as_str()))
                    .then_some((upstream, exposed))
                })
        })
        .any(|(upstream, exposed)| {
            AppState::route_configuration_fingerprint_with_snapshot(
                snapshot,
                upstream,
                &profile.key.key_fingerprint,
                &exposed,
                &profile.key.runtime_model_slug,
                protocol,
            )
            .is_ok_and(|fingerprint| {
                profile_is_current_for_route(
                    snapshot,
                    upstream,
                    &exposed,
                    &profile.key.runtime_model_slug,
                    protocol,
                    profile,
                    &fingerprint,
                )
            })
        })
}

fn profile_is_current_for_route(
    snapshot: &crate::capabilities::CapabilityRuntimeSnapshot,
    upstream: &crate::state::UpstreamConfig,
    exposed_model_slug: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    profile: &UpstreamDialectProfile,
    fingerprint: &str,
) -> bool {
    profile.key
        == DialectProfileKey::for_key(
            upstream.id.clone(),
            profile.key.key_fingerprint.clone(),
            runtime_model_slug,
            WireProtocol::from(protocol),
        )
        && upstream
            .keys_for_model(runtime_model_slug)
            .iter()
            .any(|api_key| {
                upstream_key_fingerprint(&upstream.id, api_key) == profile.key.key_fingerprint
            })
        && profile.configuration_fingerprint == fingerprint
        && profile.probe_schema_version == crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION
        && AppState::route_configuration_fingerprint_with_snapshot(
            snapshot,
            upstream,
            &profile.key.key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )
        .is_ok_and(|current| current == fingerprint)
}

fn profile_currentness_label(has_profile: bool, is_current: bool) -> &'static str {
    if is_current {
        "current"
    } else if has_profile {
        "stale"
    } else {
        "missing"
    }
}

fn capability_profile_summary(
    profile: &UpstreamDialectProfile,
    now: u64,
    is_current: bool,
) -> Value {
    let capability_evidence = Capability::ALL
        .into_iter()
        .map(|capability| {
            let state = is_current
                .then(|| profile.capabilities.get(&capability).copied())
                .flatten()
                .unwrap_or(crate::capabilities::EvidenceState::Unobserved);
            (enum_string(capability), enum_string(state))
        })
        .collect::<BTreeMap<_, _>>();
    let capability_sources = Capability::ALL
        .into_iter()
        .map(|capability| {
            let source = if is_current && profile.capabilities.contains_key(&capability) {
                "probe"
            } else {
                "baseline"
            };
            (enum_string(capability), source)
        })
        .collect::<BTreeMap<_, _>>();
    let extension_evidence = if is_current {
        profile
            .extension_evidence
            .iter()
            .map(|(id, state)| (sanitize_identifier(id), enum_string(*state)))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let extension_sources = if is_current {
        profile
            .extension_evidence
            .keys()
            .map(|id| (sanitize_identifier(id), "probe"))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let age_seconds = is_current
        .then(|| {
            profile
                .last_success_at
                .or(profile.last_attempt_at)
                .map(|at| now.saturating_sub(at))
        })
        .flatten();
    let probe_version = is_current.then_some(profile.probe_schema_version);
    let fingerprint = is_current
        .then(|| safe_fingerprint(&profile.configuration_fingerprint))
        .flatten();
    let evidence_codes = if is_current {
        sanitized_identifiers(profile.evidence_codes.iter())
    } else {
        Vec::new()
    };
    let event_types = if is_current {
        sanitized_identifiers(profile.event_types.iter())
    } else {
        Vec::new()
    };
    let http_status = is_current
        .then(|| {
            profile
                .http_status
                .filter(|status| (100..=599).contains(status))
        })
        .flatten();
    let operational_code = is_current
        .then(|| {
            profile
                .last_operational_failure
                .as_deref()
                .map(sanitize_identifier)
        })
        .flatten();
    json!({
        "key": {
            "upstream_id": profile.key.upstream_id,
            "runtime_model_slug": profile.key.runtime_model_slug,
            "protocol": enum_string(profile.key.protocol),
        },
        "upstream_id": profile.key.upstream_id,
        "runtime_model_slug": profile.key.runtime_model_slug,
        "protocol": enum_string(profile.key.protocol),
        "state": if is_current { enum_string(profile.state) } else { "unknown".into() },
        "currentness": profile_currentness_label(true, is_current),
        "age_seconds": age_seconds,
        "profile_age_seconds": age_seconds,
        "probe_version": probe_version,
        "fingerprint": fingerprint,
        "sources": {
            "capabilities": capability_sources,
            "extensions": extension_sources,
        },
        "evidence": {
            "capabilities": capability_evidence,
            "extensions": extension_evidence,
            "codes": evidence_codes,
        },
        "event_summary": {
            "types": event_types,
        },
        "status_summary": {
            "http_status": http_status,
            "operational_code": operational_code,
        },
    })
}

fn resolved_conflicts(
    profile: Option<&UpstreamDialectProfile>,
    route_overrides: &[&crate::capabilities::RouteCapabilityOverride],
) -> Vec<Value> {
    let Some(profile) = profile else {
        return Vec::new();
    };
    let mut conflicts = Vec::new();
    for capability in Capability::ALL {
        let Some(probe_state) = profile.capabilities.get(&capability).copied() else {
            continue;
        };
        if probe_state == crate::capabilities::EvidenceState::Unobserved {
            continue;
        }
        let Some(policy_state) = route_overrides
            .iter()
            .find_map(|override_policy| override_policy.capabilities.get(&capability).copied())
        else {
            continue;
        };
        if policy_state == crate::capabilities::EvidenceState::Unobserved
            || policy_state == probe_state
        {
            continue;
        }
        conflicts.push(json!({
            "subject": format!("capability.{}", enum_string(capability)),
            "probe": {
                "code": format!("probe_{}", enum_string(probe_state)),
                "state": enum_string(probe_state),
            },
            "policy": {
                "code": format!("policy_override_{}", enum_string(policy_state)),
                "state": enum_string(policy_state),
            },
            "winner": "override",
        }));
    }
    conflicts
}

fn safe_fingerprint(value: &str) -> Option<String> {
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("sha256:{}", &value[..16]))
}

fn sanitize_identifier(value: &str) -> String {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        "redacted".into()
    } else {
        value.to_owned()
    }
}

fn sanitized_identifiers<'a>(values: impl Iterator<Item = &'a String>) -> Vec<String> {
    values
        .map(|value| sanitize_identifier(value))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn enum_string<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_sanitizer_redacts_existing_sensitive_urls() {
        let mut configuration = CapabilityConfiguration::default();
        configuration.probe.https_image_fixture = Some(crate::capabilities::HttpsImageFixture {
            url: "https://user:password@fixture.invalid/image.png?signature=secret".into(),
            expected_label: "fixture".into(),
        });
        let mut sanitized = configuration;
        crate::capabilities::sanitize_sensitive_urls(&mut sanitized);
        let serialized = serde_json::to_string(&sanitized).unwrap();

        assert!(!serialized.contains("password"));
        assert!(!serialized.contains("signature=secret"));
        assert!(serialized.contains("https://redacted.invalid/"));
    }
}
