use super::*;
use crate::capabilities::{
    CapabilitySelector, EvidenceState, RouteCapabilityOverride, WireProtocol,
};
use crate::keys::{anonymous_route_id, upstream_key_fingerprint};
use serde::Deserialize;

const MANAGED_REASONING_OVERRIDE_PREFIX: &str = "operator-reasoning-";
const MANAGED_REASONING_OVERRIDE_PRIORITY: i32 = 1_000_000;
const REASONING_LEVELS: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReasoningOverrideScope {
    Route,
    ModelRoutes,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReasoningOverrideRequest {
    upstream_id: String,
    route_id: String,
    exposed_model_slug: String,
    runtime_model_slug: String,
    protocol: WireProtocol,
    levels: Vec<String>,
    scope: ReasoningOverrideScope,
}

#[derive(Clone, Debug)]
struct CurrentReasoningRoute {
    upstream_id: String,
    key_fingerprint: String,
    runtime_model_slug: String,
    protocol: WireProtocol,
    route_id: String,
}

pub(super) fn managed_reasoning_override_id(route_id: &str) -> String {
    format!("{MANAGED_REASONING_OVERRIDE_PREFIX}{route_id}")
}

pub(super) async fn admin_update_reasoning_overrides(
    State(state): State<AppState>,
    Json(request): Json<ReasoningOverrideRequest>,
) -> Response {
    let levels = match normalize_reasoning_levels(&request.levels) {
        Some(levels) => levels,
        None => {
            return reasoning_override_error(
                StatusCode::BAD_REQUEST,
                "capability_reasoning_override_invalid_level",
                "levels must contain only none, low, medium, high, xhigh, or max",
            );
        }
    };
    let routing = state.routing_snapshot().await;
    let case_insensitive = state.runtime_settings().model_case_insensitive_matching;
    let Some(selected_route) =
        resolve_selected_route(&routing.upstreams, &request, case_insensitive)
    else {
        return reasoning_override_error(
            StatusCode::BAD_REQUEST,
            "capability_reasoning_override_invalid_route",
            "route identity is stale or does not match current configuration",
        );
    };
    let mut targets = match request.scope {
        ReasoningOverrideScope::Route => vec![selected_route],
        ReasoningOverrideScope::ModelRoutes => current_routes_for_exposed_model(
            &routing.upstreams,
            &request.exposed_model_slug,
            case_insensitive,
        ),
    };
    if targets.is_empty() {
        return reasoning_override_error(
            StatusCode::BAD_REQUEST,
            "capability_reasoning_override_invalid_route",
            "route identity is stale or does not match current configuration",
        );
    }
    targets.sort_by(|left, right| left.route_id.cmp(&right.route_id));
    targets.dedup_by(|left, right| left.route_id == right.route_id);

    let managed_ids = targets
        .iter()
        .map(|route| managed_reasoning_override_id(&route.route_id))
        .collect::<BTreeSet<_>>();
    let snapshot = state.capability_snapshot();
    let mut configuration = snapshot.configuration.source().clone();
    configuration
        .route_overrides
        .retain(|route_override| !managed_ids.contains(&route_override.id));
    if !levels.is_empty() {
        configuration.route_overrides.extend(
            targets
                .iter()
                .map(|route| managed_reasoning_override(route, &levels)),
        );
    }
    configuration.revision = configuration.revision.saturating_add(1);
    if configuration.compile().is_err() {
        return reasoning_override_error(
            StatusCode::CONFLICT,
            "capability_reasoning_override_conflict",
            "reasoning override conflicts with the current capability configuration",
        );
    }
    let revision = configuration.revision;
    if state
        .replace_capability_configuration(configuration)
        .await
        .is_err()
    {
        return reasoning_override_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "capability_reasoning_override_persist_failed",
            "failed to persist reasoning override",
        );
    }

    let affected_route_ids = targets
        .into_iter()
        .map(|route| route.route_id)
        .collect::<Vec<_>>();
    Json(json!({
        "ok": true,
        "configuration_revision": revision,
        "affected_route_count": affected_route_ids.len(),
        "affected_route_ids": affected_route_ids,
    }))
    .into_response()
}

fn resolve_selected_route(
    upstreams: &[UpstreamConfig],
    request: &ReasoningOverrideRequest,
    case_insensitive: bool,
) -> Option<CurrentReasoningRoute> {
    let upstream = upstreams
        .iter()
        .find(|upstream| upstream.active && upstream.id == request.upstream_id)?;
    let runtime_model_slug =
        upstream.resolved_model_name_with(&request.exposed_model_slug, case_insensitive)?;
    if runtime_model_slug != request.runtime_model_slug {
        return None;
    }
    let protocol = upstream_protocol(request.protocol)?;
    if !upstream.supports_protocol(protocol) {
        return None;
    }
    upstream
        .keys_for_model_with(&runtime_model_slug, case_insensitive)
        .into_iter()
        .map(|api_key| {
            current_reasoning_route(upstream, &api_key, &runtime_model_slug, request.protocol)
        })
        .find(|route| route.route_id == request.route_id)
}

fn current_routes_for_exposed_model(
    upstreams: &[UpstreamConfig],
    exposed_model_slug: &str,
    case_insensitive: bool,
) -> Vec<CurrentReasoningRoute> {
    let mut routes = Vec::new();
    for upstream in upstreams.iter().filter(|upstream| upstream.active) {
        let Some(runtime_model_slug) =
            upstream.resolved_model_name_with(exposed_model_slug, case_insensitive)
        else {
            continue;
        };
        for api_key in upstream.keys_for_model_with(&runtime_model_slug, case_insensitive) {
            for protocol in upstream.supported_protocols() {
                routes.push(current_reasoning_route(
                    upstream,
                    &api_key,
                    &runtime_model_slug,
                    WireProtocol::from(protocol),
                ));
            }
        }
    }
    routes
}

fn current_reasoning_route(
    upstream: &UpstreamConfig,
    api_key: &str,
    runtime_model_slug: &str,
    protocol: WireProtocol,
) -> CurrentReasoningRoute {
    let key_fingerprint = upstream_key_fingerprint(&upstream.id, api_key);
    CurrentReasoningRoute {
        upstream_id: upstream.id.clone(),
        route_id: anonymous_route_id(&upstream.id, &key_fingerprint, runtime_model_slug, protocol),
        key_fingerprint,
        runtime_model_slug: runtime_model_slug.to_owned(),
        protocol,
    }
}

fn managed_reasoning_override(
    route: &CurrentReasoningRoute,
    levels: &[String],
) -> RouteCapabilityOverride {
    RouteCapabilityOverride {
        id: managed_reasoning_override_id(&route.route_id),
        priority: MANAGED_REASONING_OVERRIDE_PRIORITY,
        selector: CapabilitySelector {
            upstream_id: Some(route.upstream_id.clone()),
            key_fingerprint: Some(route.key_fingerprint.clone()),
            runtime_model: Some(route.runtime_model_slug.clone()),
            protocol: Some(route.protocol),
            ..Default::default()
        },
        capabilities: [(Capability::ReasoningOutput, EvidenceState::Supported)].into(),
        reasoning_control_field: Some("reasoning_effort".to_owned()),
        effort_map: levels
            .iter()
            .map(|level| (level.clone(), Value::String(level.clone())))
            .collect(),
        ..Default::default()
    }
}

fn normalize_reasoning_levels(levels: &[String]) -> Option<Vec<String>> {
    if levels
        .iter()
        .any(|level| !REASONING_LEVELS.contains(&level.as_str()))
    {
        return None;
    }
    Some(
        REASONING_LEVELS
            .into_iter()
            .filter(|candidate| levels.iter().any(|level| level == candidate))
            .map(str::to_owned)
            .collect(),
    )
}

fn upstream_protocol(protocol: WireProtocol) -> Option<UpstreamProtocol> {
    match protocol {
        WireProtocol::ChatCompletions => Some(UpstreamProtocol::ChatCompletions),
        WireProtocol::Responses => Some(UpstreamProtocol::Responses),
        WireProtocol::Messages => None,
    }
}

fn reasoning_override_error(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}
