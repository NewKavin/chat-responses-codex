use super::*;
use crate::state::UpstreamModelMapping;
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelMappingStatus {
    Effective,
    Partial,
    Inactive,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelMappingStatusReason {
    EligibleRoutesAvailable,
    SomeRoutesIneligible,
    UpstreamInactive,
    UpstreamModelUnavailable,
    NoKeyForUpstreamModel,
    NoEligibleRoutes,
}

#[derive(Serialize)]
struct ModelMappingStatusSummary {
    upstream_id: String,
    upstream_model: String,
    downstream_model: String,
    status: ModelMappingStatus,
    reason: ModelMappingStatusReason,
    eligible_routes: usize,
    configured_routes: usize,
    unverified_routes: usize,
}

impl ModelMappingStatusSummary {
    fn inactive(
        upstream: &UpstreamConfig,
        mapping: &UpstreamModelMapping,
        reason: ModelMappingStatusReason,
    ) -> Self {
        Self {
            upstream_id: upstream.id.clone(),
            upstream_model: mapping.upstream_model.clone(),
            downstream_model: mapping.downstream_model.clone(),
            status: ModelMappingStatus::Inactive,
            reason,
            eligible_routes: 0,
            configured_routes: 0,
            unverified_routes: 0,
        }
    }
}

#[derive(Serialize)]
struct ModelMappingStatusResponse {
    mappings: Vec<ModelMappingStatusSummary>,
}

pub(super) async fn admin_model_mapping_status(State(state): State<AppState>) -> Response {
    let routing = state.routing_snapshot().await;
    let capability_snapshot = state.capability_snapshot();
    let requested = RequestedFeatures {
        required: BTreeSet::from([Capability::TextInput, Capability::NonStreamingResponse]),
        ..RequestedFeatures::default()
    };

    let mut mappings = routing
        .upstreams
        .iter()
        .flat_map(|upstream| {
            upstream
                .model_mappings
                .iter()
                .map(|mapping| mapping_status(upstream, mapping, &capability_snapshot, &requested))
        })
        .collect::<Vec<_>>();
    mappings.sort_by(|left, right| {
        left.upstream_id
            .cmp(&right.upstream_id)
            .then(left.upstream_model.cmp(&right.upstream_model))
            .then(left.downstream_model.cmp(&right.downstream_model))
    });

    Json(ModelMappingStatusResponse { mappings }).into_response()
}

fn mapping_status(
    upstream: &UpstreamConfig,
    mapping: &UpstreamModelMapping,
    snapshot: &CapabilityRuntimeSnapshot,
    requested: &RequestedFeatures,
) -> ModelMappingStatusSummary {
    if !upstream.active {
        return ModelMappingStatusSummary::inactive(
            upstream,
            mapping,
            ModelMappingStatusReason::UpstreamInactive,
        );
    }
    if !upstream.supports_stored_model(&mapping.upstream_model) {
        return ModelMappingStatusSummary::inactive(
            upstream,
            mapping,
            ModelMappingStatusReason::UpstreamModelUnavailable,
        );
    }

    let keys = upstream.keys_for_model(&mapping.upstream_model);
    if keys.is_empty() {
        return ModelMappingStatusSummary::inactive(
            upstream,
            mapping,
            ModelMappingStatusReason::NoKeyForUpstreamModel,
        );
    }

    let protocols = upstream.supported_protocols();
    let configured_routes = keys.len() * protocols.len();
    let mut eligible_routes = 0;
    let mut unverified_routes = 0;
    for api_key in keys {
        let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
        for protocol in &protocols {
            if exact_route_effective_profile(
                snapshot,
                upstream,
                &key_fingerprint,
                &mapping.downstream_model,
                &mapping.upstream_model,
                *protocol,
            )
            .is_none()
            {
                unverified_routes += 1;
            }
            if resolve_route_capabilities_with_snapshot(
                snapshot,
                upstream,
                &key_fingerprint,
                &mapping.downstream_model,
                &mapping.upstream_model,
                *protocol,
                requested,
            )
            .is_some()
            {
                eligible_routes += 1;
            }
        }
    }

    let (status, reason) = if eligible_routes == configured_routes && configured_routes > 0 {
        (
            ModelMappingStatus::Effective,
            ModelMappingStatusReason::EligibleRoutesAvailable,
        )
    } else if eligible_routes > 0 {
        (
            ModelMappingStatus::Partial,
            ModelMappingStatusReason::SomeRoutesIneligible,
        )
    } else {
        (
            ModelMappingStatus::Inactive,
            ModelMappingStatusReason::NoEligibleRoutes,
        )
    };

    ModelMappingStatusSummary {
        upstream_id: upstream.id.clone(),
        upstream_model: mapping.upstream_model.clone(),
        downstream_model: mapping.downstream_model.clone(),
        status,
        reason,
        eligible_routes,
        configured_routes,
        unverified_routes,
    }
}
