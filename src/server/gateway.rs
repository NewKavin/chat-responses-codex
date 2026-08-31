use super::admin::*;
use super::portal::*;
use crate::capabilities::{
    Capability, CapabilityHintKey, CapabilityRuntimeSnapshot, CapabilitySource, DialectProfileKey,
    EvidenceState, ProbeReason, RequestedFeatures, ResolvedCapabilities,
    RuntimeCapabilityHintSnapshot, WireProtocol,
};
use crate::keys::{anonymous_route_id, upstream_key_fingerprint};
use crate::protocol::{
    chat_request_to_responses_payload_with_context,
    chat_response_to_responses_payload_with_tool_registry, normalize_tool_arguments,
    responses_response_to_chat_payload_with_tool_registry,
    tool_adapter::{ToolAdapterRegistry, ToolTarget},
    ChatStreamCanonicalizer, ConversionContext, FirstUsableOutputClassifier,
    FirstUsableOutputResult, ProtocolError, StreamAggregateResult, StreamResponseAggregator,
    StreamTranslator,
};
use crate::routing::UpstreamProtocol;
use crate::state::{
    join_upstream_url, portal_model_is_allowed, unix_millis, unix_seconds, AccountConcurrencyKey,
    AccountProbeOutcome, ActiveGatewayRequestStart, AppConfig, AppState,
    CompatibilityUsageMetadata, DownstreamConcurrencyLease, DownstreamModelEntry,
    GlobalContextProfile, KeyHealthKey, RouteAvailability, RouteHealthKey, RouteHealthPermit,
    RouteOutcome, RouteRecovery, RouteSetAggregateKey, RuntimeCoordinationError, RuntimeSettings,
    StreamDecodeCounter, StreamDiagnostics, UpstreamConfig, UpstreamRequestLease, UsageLog,
};
use axum::body::{Body, BodyDataStream};
use axum::extract::{rejection::JsonRejection, ConnectInfo, Json, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures_util::{stream as futures_stream, FutureExt, StreamExt};
use mime_guess::from_path;
use reqwest::Url;
use rust_embed::RustEmbed;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch, Mutex as TokioMutex};
use tokio::task::AbortHandle;
use tokio::time::Instant as TokioInstant;
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

mod account_recovery;
mod capability_admin;
mod capability_probe;
mod capability_routing;
mod claude;
mod compat;
pub(crate) mod compatibility_semantics;
mod context;
mod dialect_retry;
mod errors;
mod model_mapping_status;
mod reasoning_overrides;
mod responses_fallback;
mod route_attempts;
mod route_retry;
mod stream;
mod stream_commit;
pub(super) mod thinking_signature;
mod troubleshooting;
mod upstream;

use account_recovery::{AccountAdmission, AccountRecoverySession};
use capability_admin::*;
pub use capability_probe::*;
use capability_routing::*;
use claude::*;
use compat::*;
use context::*;
use errors::*;
use model_mapping_status::*;
use reasoning_overrides::*;
use responses_fallback::*;
use route_attempts::*;
use route_retry::{RouteRetryBudget, RouteRetryPolicy, RouteRetryWait};
use stream::*;
use troubleshooting::*;
use upstream::*;

#[derive(RustEmbed)]
#[folder = "frontend/dist"]
struct FrontendAssets;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EndpointKind {
    ChatCompletions,
    Responses,
}

impl EndpointKind {
    fn native_protocol(self) -> UpstreamProtocol {
        match self {
            EndpointKind::ChatCompletions => UpstreamProtocol::ChatCompletions,
            EndpointKind::Responses => UpstreamProtocol::Responses,
        }
    }

    fn path(self) -> &'static str {
        match self {
            EndpointKind::ChatCompletions => "/v1/chat/completions",
            EndpointKind::Responses => "/v1/responses",
        }
    }

    fn opposite(self) -> UpstreamProtocol {
        match self.native_protocol() {
            UpstreamProtocol::ChatCompletions => UpstreamProtocol::Responses,
            UpstreamProtocol::Responses => UpstreamProtocol::ChatCompletions,
        }
    }
}

fn gateway_response_id() -> String {
    format!("resp_{}", Uuid::new_v4().simple())
}

fn gateway_scoped_responses_body(mut response: Value) -> Value {
    let response_id = gateway_response_id();
    if let Some(object) = response.as_object_mut() {
        let upstream_response_id = object
            .insert("id".to_string(), Value::String(response_id))
            .and_then(|value| value.as_str().map(str::to_owned));
        if let Some(upstream_response_id) = upstream_response_id {
            tracing::debug!(
                upstream_response_id,
                "captured upstream response id for response diagnostics"
            );
        }
    }
    response
}

fn responses_event_response_id(event: &Value) -> Option<&str> {
    event
        .get("response_id")
        .or_else(|| event.pointer("/response/id"))
        .or_else(|| {
            (event.get("object").and_then(Value::as_str) == Some("response.chunk"))
                .then(|| event.get("id"))
                .flatten()
        })
        .and_then(Value::as_str)
}

fn rewrite_responses_event_response_id(event: &mut Value, response_id: &str) -> bool {
    let mut rewritten = false;
    if let Some(value) = event.get_mut("response_id") {
        *value = Value::String(response_id.to_string());
        rewritten = true;
    }
    if let Some(value) = event.pointer_mut("/response/id") {
        *value = Value::String(response_id.to_string());
        rewritten = true;
    }
    if event.get("object").and_then(Value::as_str) == Some("response.chunk") {
        if let Some(value) = event.get_mut("id") {
            *value = Value::String(response_id.to_string());
            rewritten = true;
        }
    }
    rewritten
}

#[derive(Clone, Debug)]
struct RouteCapabilityEvaluation {
    eligible: bool,
    optional_misses: usize,
    failed_capability: Option<Capability>,
    resolved: Option<ResolvedCapabilities>,
}

#[cfg(test)]
fn build_request_route_capability_cache(
    snapshot: &CapabilityRuntimeSnapshot,
    upstreams: &[UpstreamConfig],
    model: &str,
    endpoint: EndpointKind,
    requested: &RequestedFeatures,
) -> BTreeMap<(WireProtocol, String, String), RouteCapabilityEvaluation> {
    build_request_route_capability_cache_with_hints(
        snapshot,
        upstreams,
        model,
        endpoint,
        requested,
        &RuntimeCapabilityHintSnapshot::default(),
        None,
        true,
    )
}

#[allow(clippy::too_many_arguments)] // runtime switch threading; see existing gateways above
fn build_request_route_capability_cache_with_hints(
    snapshot: &CapabilityRuntimeSnapshot,
    upstreams: &[UpstreamConfig],
    model: &str,
    endpoint: EndpointKind,
    requested: &RequestedFeatures,
    runtime_hints: &RuntimeCapabilityHintSnapshot,
    requested_value: Option<&str>,
    case_insensitive: bool,
) -> BTreeMap<(WireProtocol, String, String), RouteCapabilityEvaluation> {
    let mut cache = BTreeMap::new();
    for upstream in upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.supports_model_with(model, case_insensitive))
    {
        let Some(runtime_model_slug) = upstream.resolved_model_name_with(model, case_insensitive)
        else {
            continue;
        };
        for api_key in route_api_keys(upstream, &runtime_model_slug, case_insensitive) {
            let key_fingerprint = route_key_fingerprint(upstream, &api_key);
            for protocol in upstream.supported_protocols() {
                let route_requested = adapt_requested_features_for_protocol(requested, protocol);
                let evaluation = evaluate_route_capabilities_with_runtime_hints(
                    RouteCapabilityRoute::new(
                        snapshot,
                        upstream,
                        &key_fingerprint,
                        model,
                        &runtime_model_slug,
                        protocol,
                    ),
                    requested,
                    runtime_hints,
                    requested_value,
                );
                let (resolved, mut failed_capability) = match evaluation {
                    RouteCapabilityResolution::Resolved(resolved) => (Some(*resolved), None),
                    RouteCapabilityResolution::Rejected(error) => (None, Some(error.capability)),
                    RouteCapabilityResolution::Unavailable => (None, None),
                };
                let native_file_route_is_valid =
                    !requested.required.contains(&Capability::NativeFileId)
                        || protocol == endpoint.native_protocol();
                if !native_file_route_is_valid {
                    failed_capability = Some(Capability::NativeFileId);
                }
                let eligible = native_file_route_is_valid && resolved.is_some();
                let optional_misses =
                    resolved
                        .as_ref()
                        .map_or(route_requested.optional.len(), |resolved| {
                            route_requested
                                .optional
                                .iter()
                                .filter(|capability| !resolved.supports(**capability))
                                .count()
                        });
                cache.insert(
                    (
                        WireProtocol::from(protocol),
                        upstream.id.clone(),
                        key_fingerprint.clone(),
                    ),
                    RouteCapabilityEvaluation {
                        eligible,
                        optional_misses,
                        failed_capability,
                        resolved,
                    },
                );
            }
        }
    }
    cache
}

fn route_api_keys(upstream: &UpstreamConfig, model: &str, case_insensitive: bool) -> Vec<String> {
    let keys = upstream.keys_for_model_with(model, case_insensitive);
    if keys.is_empty() && upstream.api_key_models.is_empty() {
        vec![upstream.api_key.clone()]
    } else {
        keys
    }
}

fn rotate_route_keys_for_request(
    candidate_keys: &mut [String],
    request_id: &str,
    upstream_id: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
) {
    if candidate_keys.len() <= 1 {
        return;
    }

    let mut hasher = Sha256::new();
    for part in [request_id, upstream_id, runtime_model_slug] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.update([match protocol {
        UpstreamProtocol::ChatCompletions => 0,
        UpstreamProtocol::Responses => 1,
    }]);
    let digest = hasher.finalize();
    let value = u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix must contain eight bytes"),
    );
    candidate_keys.rotate_left(value as usize % candidate_keys.len());
}

fn promote_preferred_route_key(
    upstream: &UpstreamConfig,
    candidate_keys: &mut Vec<String>,
    preferred_fingerprint: &str,
) {
    let Some(position) = candidate_keys
        .iter()
        .position(|api_key| route_key_fingerprint(upstream, api_key) == preferred_fingerprint)
    else {
        return;
    };
    if position > 0 {
        let preferred = candidate_keys.remove(position);
        candidate_keys.insert(0, preferred);
    }
}

fn route_key_fingerprint(upstream: &UpstreamConfig, api_key: &str) -> String {
    upstream_key_fingerprint(&upstream.id, api_key)
}

fn runtime_hint_capability(
    requested: &RequestedFeatures,
    requested_value: Option<&str>,
) -> Option<(Capability, Option<String>)> {
    if let Some(value) = requested_value {
        return Some((Capability::ReasoningOutput, Some(value.to_string())));
    }
    const PRIORITY: [Capability; 18] = [
        Capability::ReasoningStream,
        Capability::TextStream,
        Capability::ReasoningReplay,
        Capability::ReasoningOutput,
        Capability::IndexedToolArgumentStream,
        Capability::UsageStream,
        Capability::ForcedToolChoice,
        Capability::ParallelToolCalls,
        Capability::ToolContinuation,
        Capability::NamespaceTools,
        Capability::CustomTools,
        Capability::HostedTools,
        Capability::FunctionTools,
        Capability::StructuredOutput,
        Capability::NativeFileId,
        Capability::ImageDetail,
        Capability::ImageDataUrl,
        Capability::ImageHttps,
    ];
    PRIORITY
        .into_iter()
        .find(|capability| {
            requested.required.contains(capability) || requested.optional.contains(capability)
        })
        .map(|capability| (capability, None))
}

#[allow(clippy::too_many_arguments)]
async fn apply_runtime_capability_failure_hint(
    state: &AppState,
    capability_snapshot: &CapabilityRuntimeSnapshot,
    requested: &RequestedFeatures,
    requested_value: Option<&str>,
    exposed_model_slug: &str,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    class: FailureClass,
) {
    let profile = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint,
        runtime_model_slug,
        WireProtocol::from(protocol),
    );
    let key = match class {
        FailureClass::FeatureUnsupported => {
            let Some((capability, value)) = runtime_hint_capability(requested, requested_value)
            else {
                return;
            };
            CapabilityHintKey::feature(profile, capability, value)
        }
        FailureClass::ProtocolUnsupported => CapabilityHintKey::protocol(profile),
        _ => return,
    };
    let Ok(configuration_fingerprint) = AppState::route_configuration_fingerprint_with_snapshot(
        capability_snapshot,
        upstream,
        key_fingerprint,
        exposed_model_slug,
        runtime_model_slug,
        protocol,
    ) else {
        return;
    };
    if !state.insert_runtime_capability_hint(key, configuration_fingerprint) {
        return;
    }
    if let Ok(Some(job)) = state
        .build_capability_probe_job(
            &upstream.id,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
            ProbeReason::DialectError,
        )
        .await
    {
        state.queue_capability_probe(job);
    }
}

#[allow(clippy::too_many_arguments)]
fn clear_runtime_capability_hints_for_success(
    state: &AppState,
    capability_snapshot: &CapabilityRuntimeSnapshot,
    requested: &RequestedFeatures,
    requested_value: Option<&str>,
    exposed_model_slug: &str,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
) {
    let Ok(configuration_fingerprint) = AppState::route_configuration_fingerprint_with_snapshot(
        capability_snapshot,
        upstream,
        key_fingerprint,
        exposed_model_slug,
        runtime_model_slug,
        protocol,
    ) else {
        return;
    };
    let profile = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint,
        runtime_model_slug,
        WireProtocol::from(protocol),
    );
    let mut capabilities = requested.required.clone();
    capabilities.extend(requested.optional.iter().copied());
    if requested_value.is_some() {
        capabilities.insert(Capability::ReasoningOutput);
    }
    state.clear_runtime_capability_hints_for_success(
        &profile,
        &configuration_fingerprint,
        &capabilities,
        requested_value,
        true,
    );
}

fn log_route_retry_wait(
    request_id: &str,
    route_attempts: &RequestRouteAttempts,
    budget: &RouteRetryBudget,
    wait: RouteRetryWait,
    recovery: Option<RouteRecovery>,
) {
    tracing::info!(
        request_id = %request_id,
        routing_round = route_attempts.routing_round(),
        route_retry_rounds = wait.next_round,
        route_retry_wait_ms = wait.sleep_for.as_millis() as u64,
        route_retry_alignment = wait.alignment,
        route_retry_alignment_truncated = wait.alignment_truncated,
        route_retry_required_delay_ms = wait.required_delay.as_millis() as u64,
        route_retry_remaining_wait_budget_ms = wait.remaining_after.as_millis() as u64,
        route_retry_waited_ms = budget.waited().as_millis() as u64,
        failure_class = recovery
            .map(|recovery| recovery.class.as_str())
            .unwrap_or("temporary"),
        physical_attempt_count = route_attempts.physical_attempt_count(),
        "scheduling bounded route retry after temporary exhaustion"
    );
}

#[derive(Clone, Copy)]
struct RouteAttemptContext<'a> {
    state: &'a AppState,
    route_attempts: &'a RequestRouteAttempts,
    route_health_key: &'a RouteHealthKey,
    route: RouteCapabilityRoute<'a>,
    requested: &'a RequestedFeatures,
    requested_value: Option<&'a str>,
    retry_after_cap: Duration,
    /// T1.2: bounds upstream Retry-After before it may influence the
    /// gateway's own route/key cooldown and the attempt ledger (the value
    /// surfaced as `cooldown_seconds`).  Distinct from `retry_after_cap`,
    /// which only bounds the client-facing Retry-After header/message.
    retry_after_cooldown_cap: Duration,
}

async fn record_route_attempt(
    input: RouteAttemptContext<'_>,
    error: &GatewayError,
) -> Result<(), GatewayError> {
    let RouteAttemptContext {
        state,
        route_attempts,
        route_health_key,
        route,
        requested,
        requested_value,
        retry_after_cap,
        retry_after_cooldown_cap,
    } = input;
    let RouteCapabilityRoute {
        snapshot: capability_snapshot,
        upstream,
        key_fingerprint,
        exposed_model_slug,
        runtime_model_slug,
        protocol,
    } = route;
    let class = if matches!(error, GatewayError::ConcurrencyFull { .. }) {
        FailureClass::ConcurrencySaturated
    } else if let Some(class) = error.route_failure_class() {
        class
    } else {
        return Ok(());
    };
    if class == FailureClass::RequestRejected {
        return Ok(());
    }
    if class == FailureClass::ModelUnsupported {
        state.submit_targeted_model_discovery(&upstream.id, key_fingerprint, runtime_model_slug);
    }
    apply_runtime_capability_failure_hint(
        state,
        capability_snapshot,
        requested,
        requested_value,
        exposed_model_slug,
        upstream,
        key_fingerprint,
        runtime_model_slug,
        protocol,
        class,
    )
    .await;
    // T1.2: upstream Retry-After is a client-side hint, not a route-removal
    // duration; bound it with the dedicated cooldown cap so it cannot starve
    // the intra-gateway wait budget.  ConcurrencySaturated is exempt because
    // a concurrency-limited upstream's Retry-After is real slot information
    // (the client-facing cap still applies there).
    let ledger_cap = if class == FailureClass::ConcurrencySaturated {
        retry_after_cap
    } else {
        retry_after_cooldown_cap
    };
    let retry_after = clamp_upstream_retry_after(error.retry_after(), ledger_cap);
    route_attempts.record_failure_with_status(
        route_health_key,
        class,
        retry_after,
        error.upstream_status(),
        error.upstream_error_code().map(str::to_owned),
        error.upstream_error_body_excerpt().map(str::to_owned),
        Some(upstream.name.clone()),
        upstream_host(&upstream.base_url),
    );
    for observation in route_attempts.take_newly_exhausted() {
        state
            .observe_route_set_failure(&observation.key, observation.class, observation.retry_after)
            .await
            .map_err(|_| runtime_coordination_unavailable_gateway_error())?;
    }
    Ok(())
}

fn route_set_aggregate_key(
    upstream: &UpstreamConfig,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
) -> RouteSetAggregateKey {
    RouteSetAggregateKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: runtime_model_slug.to_string(),
        protocol: WireProtocol::from(protocol),
    }
}

fn route_health_keys(
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
) -> (RouteHealthKey, KeyHealthKey) {
    (
        RouteHealthKey {
            upstream_id: upstream.id.clone(),
            key_fingerprint: key_fingerprint.to_string(),
            runtime_model_slug: runtime_model_slug.to_string(),
            protocol: WireProtocol::from(protocol),
        },
        KeyHealthKey {
            upstream_id: upstream.id.clone(),
            key_fingerprint: key_fingerprint.to_string(),
        },
    )
}

/// Names the capability that actually blocks the request: a failed
/// capability that intersects this request's required set when one exists
/// (never an arbitrary route's failure), otherwise the required set itself.
fn capability_name_for_failure(
    constrained_failure: Option<Capability>,
    route_profile_constraint_active: bool,
    claude_replay_route: &ClaudeThinkingReplayRoute,
    cache: &BTreeMap<(WireProtocol, String, String), RouteCapabilityEvaluation>,
    required_capabilities: &BTreeSet<Capability>,
) -> String {
    constrained_failure
        .or_else(|| {
            (!route_profile_constraint_active
                && matches!(claude_replay_route, ClaudeThinkingReplayRoute::NoReplay))
            .then(|| {
                cache
                    .values()
                    .filter_map(|route| route.failed_capability)
                    .find(|capability| required_capabilities.contains(capability))
            })
            .flatten()
        })
        .map(|capability| format!("{capability:?}"))
        .unwrap_or_else(|| {
            if required_capabilities.is_empty() {
                "Unknown".to_string()
            } else {
                required_capabilities
                    .iter()
                    .map(|capability| format!("{capability:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
}

fn duration_seconds_ceil(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
        .max(1)
}

fn clamp_upstream_retry_after(retry_after: Option<Duration>, cap: Duration) -> Option<Duration> {
    retry_after.map(|retry_after| retry_after.min(cap))
}

fn route_health_outcome(
    error: &GatewayError,
    repeat_within_request: bool,
    sole_candidate: bool,
    capacity_sole_route: bool,
    retry_after_cap: Duration,
) -> RouteOutcome {
    route_health_outcome_with_cooldown_cap(
        error,
        repeat_within_request,
        sole_candidate,
        capacity_sole_route,
        retry_after_cap,
        retry_after_cap,
        false,
    )
}

/// T1.2: `retry_after_cooldown_cap` bounds the upstream Retry-After before it
/// may influence the gateway's own route/key cooldown, while `retry_after_cap`
/// (client-facing) is used for the `ConcurrencyFull` branch only: a
/// concurrency-saturated upstream's Retry-After is real slot information and
/// must not be cut, otherwise recovery probes storm the upstream.
///
/// T1.4: `shared_host_failure_domain` is set when several candidate routes of
/// this request resolve to the same upstream host (a single aggregated
/// gateway).  Only transient-family classes enter the shared failure domain;
/// ConcurrencySaturated keeps its real slot-based Retry-After and
/// Credentials/KeyQuota always take the per-key path, never this flag.
fn route_health_outcome_with_cooldown_cap(
    error: &GatewayError,
    repeat_within_request: bool,
    sole_candidate: bool,
    capacity_sole_route: bool,
    retry_after_cap: Duration,
    retry_after_cooldown_cap: Duration,
    shared_host_failure_domain: bool,
) -> RouteOutcome {
    if matches!(error, GatewayError::ConcurrencyFull { .. }) {
        let retry_after = clamp_upstream_retry_after(error.retry_after(), retry_after_cap);
        let upstream_status = error.upstream_status();
        return retry_after
            .map(|retry_after| RouteOutcome::RouteFailureWithRetry {
                class: FailureClass::ConcurrencySaturated,
                retry_after,
                upstream_status,
                repeat_within_request,
                sole_candidate,
                capacity_sole_route,
                shared_host_failure_domain: false,
            })
            .unwrap_or(RouteOutcome::RouteFailure {
                class: FailureClass::ConcurrencySaturated,
                upstream_status,
                repeat_within_request,
                sole_candidate,
                capacity_sole_route,
                shared_host_failure_domain: false,
            });
    }
    let retry_after = clamp_upstream_retry_after(error.retry_after(), retry_after_cooldown_cap);
    let upstream_status = error.upstream_status();
    match error.route_failure_class() {
        Some(class @ (FailureClass::Credentials | FailureClass::KeyQuota)) => retry_after
            .map(|retry_after| RouteOutcome::KeyFailureWithRetry { class, retry_after })
            .unwrap_or(RouteOutcome::KeyFailure(class)),
        Some(FailureClass::RequestRejected) => RouteOutcome::Success,
        Some(class) => {
            let shared_host = shared_host_failure_domain && is_common_mode_transient_class(class);
            retry_after
                .map(|retry_after| RouteOutcome::RouteFailureWithRetry {
                    class,
                    retry_after,
                    upstream_status,
                    repeat_within_request,
                    sole_candidate,
                    capacity_sole_route,
                    shared_host_failure_domain: shared_host,
                })
                .unwrap_or(RouteOutcome::RouteFailure {
                    class,
                    upstream_status,
                    repeat_within_request,
                    sole_candidate,
                    capacity_sole_route,
                    shared_host_failure_domain: shared_host,
                })
        }
        None => RouteOutcome::Cancelled,
    }
}

fn account_attempt_outcome(result: &Result<DispatchResult, GatewayError>) -> AccountProbeOutcome {
    match result {
        Ok(_) => AccountProbeOutcome::Accepted,
        Err(GatewayError::ConcurrencyFull { retry_after, .. }) => {
            AccountProbeOutcome::ConcurrencyRejected {
                retry_after: *retry_after,
            }
        }
        Err(error) if error.upstream_status().is_some() => AccountProbeOutcome::Accepted,
        Err(_) => AccountProbeOutcome::AttemptFailed,
    }
}

fn should_retry_same_route_once(error: &GatewayError) -> bool {
    matches!(
        error.route_failure_class(),
        Some(FailureClass::TransientServer | FailureClass::Transport)
    ) && (error.status_code().is_server_error()
        || error.error_category() == "upstream_timeout"
        || error.error_category() == "upstream_network_error")
}

async fn finish_route_health_permit(
    permit: &Arc<TokioMutex<Option<RouteHealthPermit>>>,
    outcome: RouteOutcome,
) -> Result<(), GatewayError> {
    let permit = permit.lock().await.take();
    if let Some(permit) = permit {
        permit
            .finish(outcome)
            .await
            .map_err(|_| runtime_coordination_unavailable_gateway_error())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn record_cooled_route_attempt(
    route_attempts: &RequestRouteAttempts,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    class: FailureClass,
    retry_after: Duration,
    upstream_status: Option<u16>,
    upstream_error_code: Option<String>,
    half_open_busy: bool,
    local_gate_rejected: bool,
) {
    let failure = AttemptFailure {
        route_id: anonymous_route_id(
            &upstream.id,
            key_fingerprint,
            runtime_model_slug,
            WireProtocol::from(protocol),
        ),
        upstream_status,
        upstream_error_code,
        upstream_error_body_excerpt: None,
        upstream_name: Some(upstream.name.clone()),
        upstream_host: upstream_host(&upstream.base_url),
        class,
        retry_after: Some(retry_after.max(Duration::from_secs(1))),
        half_open_busy,
    };
    if local_gate_rejected {
        route_attempts.record_cooled_local_gate(failure);
    } else {
        route_attempts.record_cooled(failure);
    }
}

/// Failure classes whose identical repetition across different routes of
/// the same pool within one request indicates a request-shape problem shared
/// by the whole pool (B2 common-mode breaker).  `RequestRejected` keeps the
/// request-shape semantics; transient classes get the shared-gateway-outage
/// treatment (one delayed replay round before a request-level 502).
const LOCAL_SLOT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// C3: bounded wait for a free local pre-dispatch concurrency slot on
/// `account_key`.  The upstream account's `max_concurrency` is a hard ceiling
/// on real slots, so overflow is *served by waiting* rather than by raising
/// the ceiling.  Returns `Ok(true)` once a slot is observed free (the caller
/// re-enters the routing round and reserves it through the ordinary
/// candidate path); `Ok(false)` means the queue was already at `max_depth` or
/// the `max_wait_ms` deadline elapsed and the caller falls through to the
/// terminal (fast-fail) flow.  Local backend only: the Redis backend enforces
/// concurrency inside Lua and never produces the LocalConcurrency rejection
/// this queue exists to absorb.
///
/// The wait is a short-interval poll rather than a sleep of the retry-after
/// estimate: after C1/C2 a lease is released synchronously when its request
/// finishes (not at the TTL), so the oldest-lease TTL remaining is no longer
/// a meaningful release ETA and could be up to `upstream_local_lease_ttl_seconds`
/// (default 300s) — far past the queue deadline.  A slot can free at any
/// instant, so 100ms polling (the same cadence `AccountRecoverySession` uses)
/// keeps the queue responsive and cheap.
async fn wait_for_local_slot_free(
    state: &AppState,
    upstream: &UpstreamConfig,
    account_key: &AccountConcurrencyKey,
    max_depth: usize,
    max_wait_ms: u64,
    request_id: &str,
) -> Result<bool, GatewayError> {
    let queue_position = state.local_slot_waiter_count(account_key) + 1;
    // E5.2: the request is now parked in the C3 local-slot queue — mark the
    // active-request phase so the admin in-flight list can distinguish
    // "waiting for a real slot" from "still choosing a route".
    state.mark_active_gateway_request_queued(request_id, queue_position);
    if !state.try_enter_local_slot_wait(account_key, max_depth) {
        return Ok(false);
    }
    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_millis(max_wait_ms);
    let freed = loop {
        if state.local_account_lease_count(account_key) < upstream.max_concurrency.max(1) as usize {
            break true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break false;
        }
        let sleep_for = LOCAL_SLOT_POLL_INTERVAL.min(deadline.saturating_duration_since(now));
        tokio::time::sleep(sleep_for).await;
    };
    state.leave_local_slot_wait(account_key);
    let waited_ms = started.elapsed().as_millis() as u64;
    if freed {
        tracing::info!(
            request_id = %request_id,
            upstream_id = %upstream.id,
            queue_position,
            waited_ms,
            max_wait_ms,
            "local concurrency queue hit: a slot freed up"
        );
    } else {
        tracing::info!(
            request_id = %request_id,
            upstream_id = %upstream.id,
            queue_position,
            waited_ms,
            max_wait_ms,
            "local concurrency queue gave up (depth limit or wait deadline)"
        );
    }
    Ok(freed)
}

fn is_common_mode_breaker_class(class: FailureClass) -> bool {
    matches!(
        class,
        FailureClass::TransientServer
            | FailureClass::EdgeProxyError
            | FailureClass::RequestRejected
    )
}

fn is_common_mode_transient_class(class: FailureClass) -> bool {
    matches!(
        class,
        FailureClass::TransientServer | FailureClass::EdgeProxyError
    )
}

/// The host part of an upstream `base_url`, used to tell "two keys on the
/// same aggregated gateway" (route-local fault) apart from "two genuinely
/// distinct upstream hosts failed identically" (pool-wide transient
/// signature).  Unparseable URLs count as distinct hosts (fall back to
/// route-based counting).
fn upstream_host(base_url: &str) -> Option<String> {
    let url = Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// T1.4: whether this request's candidate pool has >= 2 candidate routes
/// resolving to the same upstream host.  A single aggregated gateway
/// (new-api) makes the "different routes" the same physical hop, so 502s are
/// a shared outage: the health layer then cools them on the edge-proxy curve
/// and never escalates the step.  The class check (transient family only) is
/// applied by the caller when the flag reaches `RouteOutcome`.  Disable by
/// turning `upstream_shared_host_failure_domain_enabled` off.
fn shared_host_failure_domain(
    host: Option<&str>,
    host_candidate_counts: &std::collections::HashMap<String, usize>,
    enabled: bool,
) -> bool {
    enabled
        && host
            .and_then(|host| host_candidate_counts.get(host))
            .copied()
            .unwrap_or(0)
            >= 2
}

/// B2 common-mode streak, scoped to one downstream request.  The streak only
/// grows when a *different route on a different upstream host* fails with the
/// exact same (class, status); the same route failing again is a route-local
/// fault that restarts the streak at 1.
///
/// T2.2: for the *transient* family (with
/// `upstream_common_mode_same_host_transient_enabled` on), a repeated
/// (class, status) failure on the *same upstream host* but a different route
/// also grows the streak — under a single aggregated gateway (new-api) the
/// "different routes" are the same physical hop, so identical transient
/// signatures there are one shared outage, not independent evidence.
/// `RequestRejected` keeps its strict different-host semantics (deliberate
/// 2026-08-12 design, never relaxed).
#[derive(Clone)]
struct CommonModeStreak {
    class: FailureClass,
    upstream_status: Option<u16>,
    last_route: RouteHealthKey,
    last_host: Option<String>,
    count: u32,
    hosts: Vec<String>,
    retry_after: Option<Duration>,
}

impl CommonModeStreak {
    fn new(
        class: FailureClass,
        upstream_status: Option<u16>,
        route: RouteHealthKey,
        host: Option<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        let hosts = host.clone().map(|host| vec![host]).unwrap_or_default();
        Self {
            class,
            upstream_status,
            last_route: route,
            last_host: host,
            count: 1,
            hosts,
            retry_after,
        }
    }

    fn same_signature(&self, class: FailureClass, upstream_status: Option<u16>) -> bool {
        self.class == class && self.upstream_status == upstream_status
    }

    /// Whether this failure is a route-local fault (restarts the streak)
    /// rather than pool-wide evidence.  Under T2.2 a same-host transient
    /// failure counts as pool-wide (`same_host_counts`), so only the exact
    /// same route restarts; for `RequestRejected` (and when the switch is
    /// off) the old strict different-host semantics are preserved.
    fn route_local_fault(
        &self,
        route: &RouteHealthKey,
        host: &Option<String>,
        same_host_counts: bool,
    ) -> bool {
        self.last_route == *route
            || (!same_host_counts
                && host.is_some()
                && self.last_host.is_some()
                && self.last_host == *host)
    }
}

/// P4: snapshot of the request-level common-mode verdict, taken at the exact
/// moment the latch trips.  The latch can immediately spend its remaining
/// budget on a delayed replay round that resets `common_mode` to a fresh
/// streak — so without this snapshot the terminal error would lose the
/// common-mode fields on exactly the single-aggregation-gateway shape T0 was
/// built for.
struct CommonModeVerdict {
    threshold: u32,
    failed_route_count: usize,
    distinct_hosts: Vec<String>,
    streak_count: u32,
}

/// Request-level error for a common-mode breaker trip: the upstream pool
/// rejected this request shape on multiple routes, so the gateway stops
/// replaying it and reports the first upstream error instead of burning all
/// routes into cooldown (HTTP 502/400 depending on the failure class, never
/// the all-routes-unavailable 503).
fn common_mode_breaker_error(
    class: FailureClass,
    upstream_status: Option<u16>,
    first_upstream_message: &str,
    streak: &CommonModeStreak,
    threshold: u32,
    failed_route_count: usize,
) -> GatewayError {
    let upstream_status = upstream_status.filter(|status| *status != 0);
    let status_summary = upstream_status
        .map(|status| format!(" (upstream HTTP {status})"))
        .unwrap_or_default();
    let message = format!(
        "upstream rejected this request on multiple routes with the same failure ({} consecutive similar failures{status_summary}); the request was not replayed across the remaining routes. First upstream error: {first_upstream_message}",
        class.as_str(),
    );
    let (status, code) = match class {
        FailureClass::RequestRejected => (StatusCode::BAD_REQUEST, "upstream_request_rejected"),
        FailureClass::EdgeProxyError => (StatusCode::BAD_GATEWAY, "upstream_edge_proxy_error"),
        _ => (StatusCode::BAD_GATEWAY, "upstream_request_shape_rejected"),
    };
    let mut details = Map::from_iter([
        ("scope".to_string(), json!("upstream")),
        ("common_mode".to_string(), json!(true)),
        ("failed_route_count".to_string(), json!(failed_route_count)),
        ("distinct_hosts".to_string(), json!(streak.hosts)),
        ("streak".to_string(), json!(streak.count)),
        ("threshold".to_string(), json!(threshold)),
        ("retried".to_string(), json!(false)),
    ]);
    if let Some(status) = upstream_status {
        details.insert("upstream_status".to_string(), json!(status));
    }
    GatewayError::classified(
        status,
        message,
        "upstream_error",
        code,
        code,
        None,
        Some(Value::Object(details)),
    )
}

/// Request-level error for a *transient* common-mode verdict: multiple
/// distinct upstream hosts failed with the identical transient signature
/// even after one delayed replay round, so the gateway stops replaying,
/// reverts the cooldowns this request wrote, and reports the likely shared
/// gateway outage to the operator (HTTP 502 + Retry-After).
fn common_mode_transient_pool_error(
    first_upstream_message: &str,
    streak: &CommonModeStreak,
    threshold: u32,
    failed_route_count: usize,
    retried: bool,
) -> GatewayError {
    let upstream_status = streak.upstream_status.filter(|status| *status != 0);
    let status_summary = upstream_status
        .map(|status| format!(" (upstream HTTP {status})"))
        .unwrap_or_default();
    let message = format!(
        "multiple routes failed with identical transient upstream errors{status_summary} — likely a shared upstream gateway outage; the request was retried once after a short backoff and still failed. First upstream error: {first_upstream_message}",
    );
    let mut details = Map::from_iter([
        ("scope".to_string(), json!("upstream")),
        ("common_mode".to_string(), json!(true)),
        ("failed_route_count".to_string(), json!(failed_route_count)),
        ("distinct_hosts".to_string(), json!(streak.hosts)),
        ("streak".to_string(), json!(streak.count)),
        ("threshold".to_string(), json!(threshold)),
        ("retried".to_string(), json!(retried)),
    ]);
    if let Some(status) = upstream_status {
        details.insert("upstream_status".to_string(), json!(status));
    }
    GatewayError::classified(
        StatusCode::BAD_GATEWAY,
        message,
        "upstream_error",
        "upstream_transient_pool_failure",
        "upstream_transient_pool_failure",
        streak.retry_after.map(duration_seconds_ceil),
        Some(Value::Object(details)),
    )
}

/// C4.2: terminal error for a request that fast-failed at the *local*
/// pre-dispatch concurrency gate (no upstream was ever called).  HTTP status
/// stays 429 for compatibility, but the code is distinct so a gateway-side
/// capacity fact can never be misread as an upstream rate limit again.  The
/// `gateway_` category makes `route_failure_class()` return `None`, so the
/// terminal block returns this error unchanged (no route-exhaustion
/// aggregation) — exactly what a local-gate verdict wants.
#[allow(clippy::too_many_arguments)] // all args are distinct scalar facts of the gate snapshot
fn local_gate_concurrency_saturated_error(
    message: &str,
    in_flight: usize,
    max_concurrency: u32,
    stale_lease_count: usize,
    queue_depth: usize,
    queue_position: usize,
    retry_after_seconds: u64,
    max_wait_ms: u64,
) -> GatewayError {
    let entry_message = format!(
        "{message} — the gateway's own local concurrency gate is full ({} of {} slots in use), not upstream rate limiting; retry after {}s",
        in_flight, max_concurrency, retry_after_seconds
    );
    let details = Map::from_iter([
        ("scope".to_string(), json!("upstream")),
        ("in_flight".to_string(), json!(in_flight)),
        ("max_concurrency".to_string(), json!(max_concurrency)),
        ("stale_lease_count".to_string(), json!(stale_lease_count)),
        ("queue_depth".to_string(), json!(queue_depth)),
        ("queue_position".to_string(), json!(queue_position)),
        ("physical_attempt_count".to_string(), json!(0)),
        ("retry_after_source".to_string(), json!("local_gate")),
        ("max_wait_ms".to_string(), json!(max_wait_ms)),
    ]);
    GatewayError::classified(
        StatusCode::TOO_MANY_REQUESTS,
        entry_message,
        "rate_limit_error",
        "gateway_concurrency_saturated",
        "gateway_concurrency_saturated",
        Some(retry_after_seconds),
        Some(Value::Object(details)),
    )
}

/// P4: enrich a request-level common-mode terminal error's `details` with the
/// aggregated T0 routing details (from `terminal_route_failure_error`) plus
/// the common-mode verdict fields, while keeping the terminal status, `code`
/// and message — the client contract — untouched.  The two groups are
/// complementary, not mutually exclusive: the latch only decides whether the
/// gateway keeps replaying, never how richly the client is told what happened.
fn merge_common_mode_terminal_details(
    error: GatewayError,
    t0: &GatewayError,
    verdict: Option<CommonModeVerdict>,
    transient_pool_retried: bool,
) -> GatewayError {
    let mut extra = Map::new();
    if let Value::Object(t0_details) = t0.safe_details() {
        for (key, value) in t0_details {
            extra.insert(key, value);
        }
    }
    if let Some(verdict) = verdict {
        extra.insert("common_mode".to_string(), json!(true));
        extra.insert(
            "failed_route_count".to_string(),
            json!(verdict.failed_route_count),
        );
        extra.insert("distinct_hosts".to_string(), json!(verdict.distinct_hosts));
        extra.insert("streak".to_string(), json!(verdict.streak_count));
        extra.insert("threshold".to_string(), json!(verdict.threshold));
        extra.insert("retried".to_string(), json!(transient_pool_retried));
    }
    error.merge_details(extra)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ChatFallbackStage {
    HighFidelity,
    ExtensionCleanup,
    ToolReplayReduction,
    HistoryCompaction,
}

impl ChatFallbackStage {
    const ORDERED: [Self; 4] = [
        Self::HighFidelity,
        Self::ExtensionCleanup,
        Self::ToolReplayReduction,
        Self::HistoryCompaction,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::HighFidelity => "high_fidelity",
            Self::ExtensionCleanup => "extension_cleanup",
            Self::ToolReplayReduction => "tool_replay_reduction",
            Self::HistoryCompaction => "history_compaction",
        }
    }
}

#[derive(Debug)]
enum DispatchBody {
    Json(Value),
    Stream(Body),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpstreamAttemptMode {
    Json,
    SsePassThrough,
    SseAggregate,
}

#[derive(Clone)]
struct RouteHedgeCandidate {
    upstream: UpstreamConfig,
    api_key: String,
    key_fingerprint: String,
    route_health_key: RouteHealthKey,
    protocol: UpstreamProtocol,
    resolved_capabilities: Option<ResolvedCapabilities>,
}

#[derive(Clone, Default)]
struct HedgeAttemptControl {
    loser: Arc<AtomicBool>,
}

impl HedgeAttemptControl {
    fn cancel_as_loser(&self) {
        self.loser.store(true, Ordering::Release);
    }

    fn is_loser(&self) -> bool {
        self.loser.load(Ordering::Acquire)
    }
}

#[derive(Debug, Default)]
struct StreamOnlyRecoveryState {
    consumed: bool,
    final_attempt: bool,
}

const STREAM_ONLY_RECOVERY_MAX_FLIGHTS: usize = 256;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct StreamOnlyRecoveryKey {
    state_instance: String,
    profile_key: DialectProfileKey,
    configuration_fingerprint: String,
}

#[derive(Debug)]
struct StreamOnlyRecoveryFlight {
    completed: watch::Sender<bool>,
}

type StreamOnlyRecoveryRegistry = HashMap<StreamOnlyRecoveryKey, Arc<StreamOnlyRecoveryFlight>>;

fn stream_only_recovery_registry() -> &'static Mutex<StreamOnlyRecoveryRegistry> {
    static REGISTRY: OnceLock<Mutex<StreamOnlyRecoveryRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug)]
struct StreamOnlyRecoveryLeader {
    key: StreamOnlyRecoveryKey,
    flight: Arc<StreamOnlyRecoveryFlight>,
    completed: bool,
}

impl StreamOnlyRecoveryLeader {
    fn complete(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        if self.completed {
            return;
        }
        self.completed = true;
        self.flight.completed.send_replace(true);
        let mut registry = stream_only_recovery_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if registry
            .get(&self.key)
            .is_some_and(|flight| Arc::ptr_eq(flight, &self.flight))
        {
            registry.remove(&self.key);
        }
    }
}

impl Drop for StreamOnlyRecoveryLeader {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Debug)]
struct StreamOnlyRecoveryFollower {
    completed: watch::Receiver<bool>,
}

impl StreamOnlyRecoveryFollower {
    async fn wait(mut self) {
        while !*self.completed.borrow() {
            if self.completed.changed().await.is_err() {
                break;
            }
        }
    }
}

#[derive(Debug)]
enum StreamOnlyRecoveryRole {
    Leader(StreamOnlyRecoveryLeader),
    Follower(StreamOnlyRecoveryFollower),
    AtCapacity,
}

fn begin_stream_only_recovery(
    state: &AppState,
    profile_key: DialectProfileKey,
    configuration_fingerprint: String,
) -> StreamOnlyRecoveryRole {
    let key = StreamOnlyRecoveryKey {
        state_instance: state.troubleshooting_route_capture_token().to_string(),
        profile_key,
        configuration_fingerprint,
    };
    let mut registry = stream_only_recovery_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(flight) = registry.get(&key) {
        return StreamOnlyRecoveryRole::Follower(StreamOnlyRecoveryFollower {
            completed: flight.completed.subscribe(),
        });
    }
    if registry.len() >= STREAM_ONLY_RECOVERY_MAX_FLIGHTS {
        return StreamOnlyRecoveryRole::AtCapacity;
    }

    let (completed, _) = watch::channel(false);
    let flight = Arc::new(StreamOnlyRecoveryFlight { completed });
    registry.insert(key.clone(), flight.clone());
    StreamOnlyRecoveryRole::Leader(StreamOnlyRecoveryLeader {
        key,
        flight,
        completed: false,
    })
}

impl UpstreamAttemptMode {
    fn uses_upstream_sse(self) -> bool {
        matches!(self, Self::SsePassThrough | Self::SseAggregate)
    }

    fn passes_sse_downstream(self) -> bool {
        self == Self::SsePassThrough
    }

    fn aggregates_sse(self) -> bool {
        self == Self::SseAggregate
    }

    fn needs_stream_completion_context(self) -> bool {
        self.passes_sse_downstream()
    }

    fn requests_usage_stream(self, resolved: Option<&ResolvedCapabilities>) -> bool {
        let exact_usage = resolved
            .and_then(|resolved| resolved.values.get(&Capability::UsageStream))
            .filter(|capability| {
                matches!(
                    capability.source,
                    CapabilitySource::Probe | CapabilitySource::Override
                )
            });
        match self {
            Self::Json => false,
            Self::SseAggregate => {
                exact_usage.is_some_and(|capability| capability.state == EvidenceState::Supported)
            }
            Self::SsePassThrough => {
                !exact_usage.is_some_and(|capability| capability.state == EvidenceState::Rejected)
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::SsePassThrough => "sse_pass_through",
            Self::SseAggregate => "sse_aggregate",
        }
    }
}

fn select_upstream_attempt_mode(
    downstream_stream: bool,
    resolved: Option<&ResolvedCapabilities>,
) -> UpstreamAttemptMode {
    if downstream_stream {
        return UpstreamAttemptMode::SsePassThrough;
    }
    let Some(resolved) = resolved else {
        return UpstreamAttemptMode::Json;
    };
    let text_stream = resolved
        .values
        .get(&Capability::TextStream)
        .copied()
        .unwrap_or(crate::capabilities::ResolvedCapability {
            state: EvidenceState::Unobserved,
            source: CapabilitySource::Baseline,
        });
    if text_stream.state != EvidenceState::Supported
        || !matches!(
            text_stream.source,
            CapabilitySource::Probe | CapabilitySource::Override
        )
    {
        return UpstreamAttemptMode::Json;
    }
    let nonstream = resolved
        .values
        .get(&Capability::NonStreamingResponse)
        .copied()
        .unwrap_or(crate::capabilities::ResolvedCapability {
            state: EvidenceState::Supported,
            source: CapabilitySource::Baseline,
        });
    if nonstream.state == EvidenceState::Rejected
        || (nonstream.source == CapabilitySource::Baseline
            && matches!(
                text_stream.source,
                CapabilitySource::Probe | CapabilitySource::Override
            ))
    {
        UpstreamAttemptMode::SseAggregate
    } else {
        UpstreamAttemptMode::Json
    }
}

fn route_has_raw_stream_delivery_evidence(resolved: Option<&ResolvedCapabilities>) -> bool {
    let Some(resolved) = resolved else {
        return false;
    };
    [Capability::NonStreamingResponse, Capability::TextStream]
        .into_iter()
        .all(|capability| {
            resolved
                .values
                .get(&capability)
                .is_some_and(|value| value.source == CapabilitySource::Baseline)
        })
}

fn request_allows_stream_only_recovery(endpoint: EndpointKind, body: &Value) -> bool {
    if body.get("previous_response_id").is_some()
        || body
            .get("conversation")
            .is_some_and(|value| !value.is_null())
        || body.get("background").and_then(Value::as_bool) == Some(true)
        || body.get("store").and_then(Value::as_bool) == Some(true)
        || body
            .pointer("/_gateway_claude/stream_only_recovery_unsafe_tool")
            .and_then(Value::as_bool)
            == Some(true)
        || body
            .pointer("/_gateway_claude/context_management")
            .is_some()
    {
        return false;
    }
    let has_continuation = body
        .get(if endpoint == EndpointKind::Responses {
            "input"
        } else {
            "messages"
        })
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                matches!(
                    item.get("role").and_then(Value::as_str),
                    Some("tool" | "function")
                ) || item.get("tool_call_id").is_some()
                    || item
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .is_some_and(|calls| !calls.is_empty())
                    || item
                        .get("function_call")
                        .is_some_and(|call| !call.is_null())
                    || value_has_non_empty_text(item.get("reasoning_content"))
                    || item
                        .get("_gateway_claude_thinking")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| !blocks.is_empty())
                    || item
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| {
                            kind == "reasoning"
                                || kind.ends_with("_call")
                                || kind.ends_with("_call_output")
                                || kind.ends_with("_result")
                        })
            })
        });
    if has_continuation {
        return false;
    }
    !body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind != "function")
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageLogTiming {
    Immediate,
    DeferredUntilStreamEnd,
}

#[derive(Debug)]
struct DispatchResult {
    status: StatusCode,
    body: DispatchBody,
    request_id: String,
    response_headers: HeaderMap,
    applied_effort_control: Option<AppliedEffortControl>,
    claude_thinking_signature: Option<ClaudeThinkingSignatureContext>,
    compatibility: Option<CompatibilityUsageMetadata>,
    usage: (u64, u64, u64),
    usage_log_timing: UsageLogTiming,
    usage_log_context: Option<GatewayUsageLogContext>,
    selected_upstream_id: String,
    selected_upstream_name: String,
    selected_upstream_key_fingerprint: String,
    selected_upstream_protocol: UpstreamProtocol,
}

#[derive(Debug, Clone)]
struct AppliedEffortControl {
    requested: String,
    field: String,
    value: serde_json::Value,
}

#[derive(Clone, Debug)]
struct ClaudeThinkingSignatureContext {
    secret: String,
    model: String,
    upstream_id: String,
    protocol: String,
    profile_fingerprint: String,
}

#[derive(Clone)]
struct GatewayUsageLogContext {
    state: AppState,
    request_id: String,
    downstream_id: String,
    downstream_name: String,
    upstream_id: String,
    upstream_name: Option<String>,
    endpoint: String,
    model: String,
    inference_strength: Option<String>,
    user_agent: Option<String>,
    compatibility: Option<CompatibilityUsageMetadata>,
    started: Instant,
}

impl std::fmt::Debug for GatewayUsageLogContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayUsageLogContext")
            .field("request_id", &self.request_id)
            .field("downstream_id", &self.downstream_id)
            .field("upstream_id", &self.upstream_id)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish()
    }
}

impl GatewayUsageLogContext {
    async fn emit(
        self,
        status_code: StatusCode,
        error_message: Option<String>,
        error_category: Option<String>,
        usage: (u64, u64, u64),
    ) -> std::io::Result<()> {
        append_gateway_usage_log(
            &self.state,
            &self.request_id,
            &self.downstream_id,
            &self.downstream_name,
            &self.upstream_id,
            self.upstream_name.as_deref(),
            &self.endpoint,
            &self.model,
            self.inference_strength.as_deref(),
            self.user_agent.as_deref(),
            self.compatibility,
            status_code,
            error_message,
            error_category,
            usage.0,
            usage.1,
            usage.2,
            self.started,
        )
        .await
    }

    async fn emit_fail_closed(
        self,
        status_code: StatusCode,
        error_message: Option<String>,
        error_category: Option<String>,
        usage: (u64, u64, u64),
    ) -> Result<(), GatewayError> {
        match self
            .emit(status_code, error_message, error_category, usage)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => runtime_coordination_gateway_error(&error).map_or(Ok(()), Err),
        }
    }
}

struct AggregateCancellationLogContext {
    usage_log: GatewayUsageLogContext,
}

struct ActiveGatewayRequestGuard {
    state: AppState,
    request_id: String,
    active: bool,
    aggregate_cancellation_log: Option<AggregateCancellationLogContext>,
    downgrade_reported: bool,
}

impl ActiveGatewayRequestGuard {
    fn new(state: AppState, request_id: String) -> Self {
        Self {
            state,
            request_id,
            active: true,
            aggregate_cancellation_log: None,
            downgrade_reported: false,
        }
    }

    fn arm_aggregate_cancellation_log(&mut self, context: GatewayUsageLogContext) {
        debug_assert!(
            self.aggregate_cancellation_log.is_none(),
            "aggregate cancellation log context re-armed"
        );
        self.aggregate_cancellation_log =
            Some(AggregateCancellationLogContext { usage_log: context });
    }

    fn clear_aggregate_cancellation_log(&mut self) {
        self.aggregate_cancellation_log.take();
    }

    fn finish(&mut self) {
        self.clear_aggregate_cancellation_log();
        if self.active {
            self.state.finish_active_gateway_request(&self.request_id);
            self.active = false;
        }
    }

    fn fail_and_finish(&mut self, error_category: &str) {
        self.clear_aggregate_cancellation_log();
        if self.active {
            self.state
                .fail_active_gateway_request(&self.request_id, error_category);
            self.finish();
        }
    }

    fn disarm(&mut self) {
        self.clear_aggregate_cancellation_log();
        self.active = false;
    }
}

impl Drop for ActiveGatewayRequestGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.clear_aggregate_cancellation_log();
            self.finish();
            return;
        }
        if let Some(context) = self.aggregate_cancellation_log.take() {
            self.fail_and_finish("stream_client_cancelled");
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = context
                        .usage_log
                        .emit(
                            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
                            Some("client cancelled while awaiting aggregated SSE output".into()),
                            Some("stream_client_cancelled".into()),
                            (0, 0, 0),
                        )
                        .await;
                });
            } else {
                tracing::warn!(
                    "aggregate cancellation log context dropped outside runtime; log skipped"
                );
            }
            return;
        }
        self.finish();
    }
}

#[derive(Clone, Copy)]
struct StreamTimeouts {
    keepalive_interval: Duration,
    idle_timeout: Duration,
    max_duration: Duration,
}

impl StreamTimeouts {
    fn from_sources(config: &AppConfig, runtime_settings: &RuntimeSettings) -> Self {
        Self {
            keepalive_interval: Duration::from_secs(
                config.upstream_stream_keepalive_interval_seconds.max(1),
            ),
            idle_timeout: Duration::from_secs(
                runtime_settings.upstream_stream_idle_timeout_seconds.max(1),
            ),
            max_duration: Duration::from_secs(config.upstream_stream_max_duration_seconds.max(1)),
        }
    }
}

#[derive(Clone, Debug)]
struct StreamDiagnosticContext {
    request_id: String,
    upstream_id: String,
    upstream_protocol: UpstreamProtocol,
    endpoint: String,
}

#[derive(Clone)]
struct StreamBodyReadDiagnosticContext {
    request_id: String,
    upstream_id: String,
    route_id: String,
    upstream_protocol: UpstreamProtocol,
    endpoint: String,
    started: Instant,
    route_attempts: RequestRouteAttempts,
    first_token_latency: FirstTokenLatency,
}

#[derive(Clone, Debug, Default)]
struct FirstTokenLatency(Arc<OnceLock<u64>>);

impl FirstTokenLatency {
    fn observe(&self, started: Instant) {
        let elapsed = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let _ = self.0.set(elapsed);
    }

    fn get(&self) -> Option<u64> {
        self.0.get().copied()
    }
}

#[derive(Clone)]
struct StreamUsageLogContext {
    state: AppState,
    request_id: String,
    downstream_key_id: String,
    downstream_name: Option<String>,
    upstream_key_id: String,
    upstream_name: Option<String>,
    upstream_protocol: UpstreamProtocol,
    endpoint: String,
    model: String,
    inference_strength: Option<String>,
    user_agent: Option<String>,
    compatibility: Option<CompatibilityUsageMetadata>,
    normalized_model: String,
    status: StatusCode,
    wire_status: StatusCode,
    transport_committed: bool,
    error_message: Option<String>,
    error_category: Option<String>,
    started: Instant,
    account_wait_ms: u64,
    first_token_latency: FirstTokenLatency,
    hedge_control: Option<HedgeAttemptControl>,
    stream_diagnostics: Option<StreamDiagnostics>,
}

impl std::fmt::Debug for StreamUsageLogContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamUsageLogContext")
            .field("request_id", &self.request_id)
            .field("downstream_key_id", &self.downstream_key_id)
            .field("upstream_key_id", &self.upstream_key_id)
            .field("upstream_protocol", &self.upstream_protocol)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("normalized_model", &self.normalized_model)
            .field("status", &self.status)
            .field("wire_status", &self.wire_status)
            .field("transport_committed", &self.transport_committed)
            .field("error_category", &self.error_category)
            .finish()
    }
}

impl StreamDiagnosticContext {
    fn from_usage(context: &StreamUsageLogContext) -> Self {
        Self {
            request_id: context.request_id.clone(),
            upstream_id: context.upstream_key_id.clone(),
            upstream_protocol: context.upstream_protocol,
            endpoint: context.endpoint.clone(),
        }
    }
}

impl StreamUsageLogContext {
    fn is_hedge_loser(&self) -> bool {
        self.hedge_control
            .as_ref()
            .is_some_and(HedgeAttemptControl::is_loser)
    }

    fn touch_active_request(&self) {
        self.state.touch_active_gateway_request(&self.request_id);
    }

    fn finish_active_request(&self) {
        self.state.finish_active_gateway_request(&self.request_id);
    }

    fn fail_active_request(&self, error_category: &str) {
        self.state
            .fail_active_gateway_request(&self.request_id, error_category);
        self.finish_active_request();
    }

    async fn emit(self, usage: (u64, u64, u64)) -> std::io::Result<()> {
        let StreamUsageLogContext {
            state,
            request_id,
            downstream_key_id,
            downstream_name,
            upstream_key_id,
            upstream_name,
            upstream_protocol,
            endpoint,
            model,
            inference_strength,
            user_agent,
            compatibility,
            normalized_model,
            status,
            wire_status,
            transport_committed,
            error_message,
            error_category,
            started,
            account_wait_ms: _,
            first_token_latency,
            hedge_control: _,
            stream_diagnostics,
        } = self;
        let wire_status_code = if transport_committed {
            wire_status.as_u16()
        } else {
            status.as_u16()
        };

        let (billing_label, total_cost_cents) =
            downstream_billing_info(&state, &downstream_key_id, usage.0, usage.1).await;

        let log = UsageLog {
            id: request_id.clone(),
            downstream_key_id: downstream_key_id.clone(),
            upstream_key_id: upstream_key_id.clone(),
            downstream_name,
            upstream_name,
            endpoint: endpoint.clone(),
            model: model.clone(),
            inference_strength,
            billing_mode: Some(billing_label),
            request_count: Some(1),
            user_agent,
            request_id: request_id.clone(),
            status_code: status.as_u16(),
            wire_status_code,
            error_message,
            error_category,
            prompt_tokens: usage.0,
            completion_tokens: usage.1,
            total_tokens: usage.2,
            total_cost_cents,
            first_token_latency_ms: first_token_latency.get(),
            latency_ms: started.elapsed().as_millis() as u64,
            created_at: unix_seconds(),
            compatibility,
            stream_diagnostics,
        };

        let result = state.append_usage_log(log).await;
        if let Err(error) = &result {
            tracing::error!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint,
                original_model = %model,
                normalized_model = %&normalized_model,
                selected_upstream_id = %upstream_key_id,
                selected_upstream_protocol = ?upstream_protocol,
                error = %error,
                "failed to save usage log"
            );
        }
        result
    }
}

fn stream_usage_from_value(value: &Value) -> Option<(u64, u64, u64)> {
    if let Some(usage) = value.get("usage") {
        return Some(usage_from_usage_value(usage));
    }

    value
        .get("response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("usage"))
        .map(usage_from_usage_value)
}

fn bounded_codex_version(user_agent: Option<&str>) -> Option<String> {
    let user_agent = user_agent?.trim();
    let lower = user_agent.to_ascii_lowercase();
    if !(lower.starts_with("codex/")
        || lower.starts_with("codex-cli/")
        || lower.starts_with("codex_cli_rs/"))
    {
        return None;
    }
    let version = user_agent.split('/').nth(1)?.split_whitespace().next()?;
    if version.is_empty()
        || version.len() > 32
        || !version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
    {
        return None;
    }
    Some(version.to_string())
}

fn stream_event_has_usable_output(event: &Value) -> bool {
    chat_stream_event_has_usable_output(event) || responses_stream_event_has_usable_output(event)
}

fn chat_stream_event_has_usable_output(event: &Value) -> bool {
    event
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                choice
                    .get("delta")
                    .or_else(|| choice.get("message"))
                    .is_some_and(chat_message_has_usable_output)
            })
        })
}

fn responses_stream_event_has_usable_output(event: &Value) -> bool {
    if value_has_non_empty_text(event.get("delta")) {
        return true;
    }

    if event
        .get("item")
        .is_some_and(responses_output_item_has_usable_output)
    {
        return true;
    }

    event
        .get("response")
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(responses_output_item_has_usable_output))
}

fn stream_output_tokens_are_zero_or_unknown(usage: Option<(u64, u64, u64)>) -> bool {
    usage
        .map(|(_, completion_tokens, _)| completion_tokens == 0)
        .unwrap_or(true)
}

fn parse_u64_token(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_i64().and_then(|value| u64::try_from(value).ok())),
        Value::String(value) => value.parse::<u64>().ok(),
        _ => None,
    }
}

fn usage_from_usage_value(usage: &Value) -> (u64, u64, u64) {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(parse_u64_token)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(parse_u64_token)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(parse_u64_token)
        .unwrap_or(prompt_tokens + completion_tokens);
    (prompt_tokens, completion_tokens, total_tokens)
}

fn extract_inference_strength(body: &Value) -> Option<String> {
    body.get("inference_strength")
        .and_then(Value::as_str)
        .or_else(|| body.get("reasoning_effort").and_then(Value::as_str))
        .or_else(|| {
            body.get("reasoning")
                .and_then(Value::as_object)
                .and_then(|reasoning| reasoning.get("effort"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn metric_exceeds_ratio(value: f64, baseline: f64, ratio: f64) -> bool {
    if baseline <= 0.0 {
        value > 0.0
    } else {
        value > baseline * ratio
    }
}

/// Resolve the billing label and per-request cost (cents) for a downstream.
/// Cost is only computed for cost-billed downstreams (token mode + price).
async fn downstream_billing_info(
    state: &AppState,
    downstream_key_id: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> (String, Option<u64>) {
    match state.downstream_config(downstream_key_id).await.as_ref() {
        Some(downstream) if downstream.token_billing_mode() => {
            let cost = downstream
                .cost_billing_mode()
                .then(|| downstream.cost_for_tokens(prompt_tokens, completion_tokens));
            ("Token 计费".to_string(), cost)
        }
        _ => ("请求计费".to_string(), None),
    }
}

#[allow(clippy::too_many_arguments)]
async fn append_gateway_usage_log(
    state: &AppState,
    request_id: &str,
    downstream_id: &str,
    downstream_name: &str,
    upstream_id: &str,
    upstream_name: Option<&str>,
    endpoint: &str,
    model: &str,
    inference_strength: Option<&str>,
    user_agent: Option<&str>,
    compatibility: Option<CompatibilityUsageMetadata>,
    status_code: StatusCode,
    error_message: Option<String>,
    error_category: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    started: Instant,
) -> std::io::Result<()> {
    let (billing_label, total_cost_cents) =
        downstream_billing_info(state, downstream_id, prompt_tokens, completion_tokens).await;
    let log = UsageLog {
        id: request_id.to_string(),
        downstream_key_id: downstream_id.to_string(),
        upstream_key_id: upstream_id.to_string(),
        downstream_name: Some(downstream_name.to_string()),
        upstream_name: upstream_name.map(str::to_string),
        endpoint: endpoint.to_string(),
        model: model.to_string(),
        inference_strength: inference_strength.map(str::to_string),
        billing_mode: Some(billing_label),
        request_count: Some(1),
        user_agent: user_agent.map(str::to_string),
        request_id: request_id.to_string(),
        status_code: status_code.as_u16(),
        wire_status_code: status_code.as_u16(),
        error_message,
        error_category,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        total_cost_cents,
        first_token_latency_ms: None,
        latency_ms: started.elapsed().as_millis() as u64,
        created_at: unix_seconds(),
        compatibility,
        stream_diagnostics: None,
    };

    let result = state.append_usage_log(log).await;
    if let Err(error) = &result {
        tracing::error!(
            request_id = %request_id,
            downstream_key_id = %downstream_id,
            path = %endpoint,
            model = %model,
            status = status_code.as_u16(),
            error = %error,
            "failed to save usage log"
        );
    }
    result
}

fn runtime_coordination_gateway_error(error: &std::io::Error) -> Option<GatewayError> {
    error
        .get_ref()
        .is_some_and(|source| source.is::<RuntimeCoordinationError>())
        .then(runtime_coordination_unavailable_gateway_error)
}

fn runtime_coordination_unavailable_gateway_error() -> GatewayError {
    GatewayError::downstream_admission_rejection(
        crate::state::DownstreamAdmissionRejection::RuntimeCoordinationUnavailable,
    )
}

fn upstream_admission_gateway_error(
    error: crate::state::UpstreamAdmissionError,
    capacity_message: &str,
) -> GatewayError {
    if error.is_runtime_coordination_unavailable() {
        runtime_coordination_unavailable_gateway_error()
    } else {
        GatewayError::Upstream(capacity_message.into())
    }
}

fn replace_error_on_runtime_rollback_failure(
    original: GatewayError,
    rollback: Result<(), RuntimeCoordinationError>,
) -> GatewayError {
    if rollback.is_err() {
        GatewayError::downstream_admission_rejection(
            crate::state::DownstreamAdmissionRejection::RuntimeCoordinationUnavailable,
        )
    } else {
        original
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/v1/messages", post(claude_messages))
        .route("/v1/messages/count_tokens", post(claude_count_tokens))
        .route("/api/admin/login", post(admin_login))
        .route(
            "/api/admin/dashboard",
            get(admin_dashboard).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/model-probe",
            get(admin_model_probe).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/export",
            get(admin_capabilities_export).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/import",
            post(admin_capabilities_import).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/policy/rebootstrap",
            post(admin_capability_policy_rebootstrap).route_layer(
                axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware),
            ),
        )
        .route(
            "/api/admin/capabilities/profiles",
            get(admin_capability_profiles).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/discovery",
            get(admin_capability_discovery).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/reasoning-overrides",
            axum::routing::put(admin_update_reasoning_overrides).route_layer(
                axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware),
            ),
        )
        .route(
            "/api/admin/capabilities/resolved",
            get(admin_capabilities_resolved).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/probe",
            post(admin_capability_probe).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/probe-all",
            post(admin_capability_probe_all).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/probe-batches/{batch_id}",
            get(admin_capability_probe_batch).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/capabilities/profiles/{upstream_id}",
            axum::routing::delete(admin_capability_profiles_delete).route_layer(
                axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware),
            ),
        )
        // Admin API - Upstreams
        .route(
            "/api/admin/upstreams",
            get(admin_list_upstreams)
                .post(admin_create_upstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/upstreams/batch",
            post(admin_create_upstreams_batch).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/keys",
            get(admin_list_upstream_keys).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/discover-models",
            post(admin_discover_upstream_models).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/qualify-models",
            post(admin_qualify_upstream_models).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/models",
            get(admin_list_models).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/announcement",
            get(admin_get_announcement)
                .put(admin_update_announcement)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/global-context-profiles",
            get(admin_get_global_context_profiles)
                .put(admin_set_global_context_profiles)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/runtime-settings",
            get(admin_get_runtime_settings)
                .put(admin_update_runtime_settings)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/model-aliases",
            get(admin_get_model_aliases)
                .put(admin_update_model_aliases)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/model-mappings/status",
            get(admin_model_mapping_status).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/integrations/freekey/sync",
            post(admin_sync_freekey_upstreams).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/{id}",
            get(admin_get_upstream)
                .put(admin_update_upstream)
                .delete(admin_delete_upstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/upstreams/{id}/toggle",
            post(admin_toggle_upstream).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/{id}/route-health/reset",
            post(admin_reset_upstream_route_health).route_layer(
                axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware),
            ),
        )
        .route(
            "/api/admin/upstreams/{id}/concurrency/reset",
            post(admin_reset_upstream_concurrency).route_layer(
                axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware),
            ),
        )
        .route(
            "/api/admin/upstreams/batch-toggle",
            post(admin_batch_toggle_upstreams).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/batch-delete",
            post(admin_batch_delete_upstreams).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/upstreams/batch-update",
            post(admin_batch_update_upstreams).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        // Admin API - Downstreams
        .route(
            "/api/admin/downstreams",
            get(admin_list_downstreams)
                .post(admin_create_downstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/downstreams/runtime",
            get(admin_downstream_runtime).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/downstreams/batch-mode",
            post(admin_batch_set_downstream_mode).route_layer(
                axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware),
            ),
        )
        .route(
            "/api/admin/downstreams/batch-update",
            post(admin_batch_update_downstreams).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/downstreams/{id}",
            get(admin_get_downstream)
                .put(admin_update_downstream)
                .delete(admin_delete_downstream)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    admin_auth_middleware,
                )),
        )
        .route(
            "/api/admin/downstreams/{id}/toggle",
            post(admin_toggle_downstream).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/downstreams/{id}/rotate",
            post(admin_rotate_downstream).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        // Admin API - Logs
        .route(
            "/api/admin/logs",
            get(admin_list_logs).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/troubleshooting/run",
            post(admin_troubleshooting_run).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/troubleshooting/matrix/run",
            post(admin_compatibility_matrix_run).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        .route(
            "/api/admin/troubleshooting/active-requests",
            get(admin_troubleshooting_active_requests).route_layer(
                axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware),
            ),
        )
        .route(
            "/api/admin/retry-amplification",
            get(admin_retry_amplification).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
        )
        // Portal API
        .route("/api/portal/login", post(portal_login))
        .route("/api/portal/overview", get(portal_overview))
        .route("/api/portal/quota", get(portal_quota))
        .route("/api/portal/usage-history", get(portal_usage_history))
        .route("/api/portal/usage-summary", get(portal_usage_summary))
        .route("/api/portal/models", get(portal_models))
        .route("/api/portal/model-probe", get(portal_model_probe))
        .route("/api/portal/announcement", get(portal_announcement))
        .route("/api/portal/key", get(portal_get_key))
        .route("/api/portal/key/rotate", post(portal_rotate_key))
        // Frontend assets and SPA fallback (with static-only compression);
        // merged so the nested router's fallback becomes the app fallback.
        .merge(static_frontend_router())
        .layer(axum::extract::DefaultBodyLimit::max(
            usize::try_from(
                state
                    .config
                    .gateway_request_body_limit_mb
                    .saturating_mul(1024 * 1024),
            )
            .unwrap_or(usize::MAX),
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<Body>| {
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        uri = %request.uri()
                    )
                })
                .on_request(|request: &Request<Body>, _span: &tracing::Span| {
                    tracing::info!(
                        method = %request.method(),
                        uri = %request.uri(),
                        client_addr = ?request_client_addr(request),
                        forwarded_for = ?header_value(
                            request.headers(),
                            header::HeaderName::from_static("x-forwarded-for")
                        ),
                        x_real_ip = ?header_value(
                            request.headers(),
                            header::HeaderName::from_static("x-real-ip")
                        ),
                        user_agent = ?header_value(request.headers(), header::USER_AGENT),
                        "request started"
                    );
                })
                .on_response(
                    |response: &Response, latency: Duration, _span: &tracing::Span| {
                        tracing::info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis() as u64,
                            content_type = ?header_value(response.headers(), header::CONTENT_TYPE),
                            "request completed"
                        );
                    },
                )
                .on_failure(
                    |failure_class: ServerErrorsFailureClass,
                     latency: Duration,
                     _span: &tracing::Span| {
                        tracing::warn!(
                            classification = %failure_class,
                            latency_ms = latency.as_millis() as u64,
                            "request failed"
                        );
                    },
                ),
        )
        .with_state(state)
}

fn request_client_addr<B>(request: &Request<B>) -> Option<SocketAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0)
}

fn header_value(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Static frontend and SPA fallback router.
///
/// Kept as a standalone nested router so the compression layer only ever sees
/// static assets; API and streaming responses bypass it entirely.
fn static_frontend_router() -> Router<AppState> {
    Router::new()
        .fallback(serve_frontend)
        .layer(CompressionLayer::new())
}

async fn serve_frontend(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(asset) = FrontendAssets::get(path) {
        let mime_type = from_path(path).first_or_octet_stream().as_ref().to_string();
        // Vite emits content-hashed files under assets/; everything else
        // (index.html, favicon) must revalidate so deploys take effect.
        let cache_control = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, mime_type),
                (header::CACHE_CONTROL, cache_control.to_string()),
            ],
            asset.data.into_response(),
        )
            .into_response();
    }

    if path.starts_with("api/") || path.starts_with("v1/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Some(asset) = FrontendAssets::get("index.html") {
        let mime_type = "text/html; charset=utf-8".to_string();
        return (
            [
                (header::CONTENT_TYPE, mime_type),
                (header::CACHE_CONTROL, "no-cache".to_string()),
            ],
            asset.data.into_response(),
        )
            .into_response();
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

async fn healthz(State(state): State<AppState>) -> Response {
    match state.runtime_coordination_healthcheck().await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime coordination unavailable",
        )
            .into_response(),
    }
}

async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ModelsQuery>,
) -> Response {
    let Ok(secret) = downstream_secret_from_headers(&headers) else {
        return GatewayError::Unauthorized("missing authorization header or x-api-key".into())
            .into_response();
    };

    // Codex sends `?client_version=x.y.z`; portal callers opt in explicitly
    // with `?format=codex` without pinning a browser-side client version.
    if query.client_version.is_some() || query.format.as_deref() == Some("codex") {
        return list_models_codex_format(&state, &secret).await;
    }

    // Standard OpenAI-compatible clients get `{"object":"list","data":[...]}`.
    let models = state.available_models_for_downstream(&secret).await;
    Json(json!({
        "object": "list",
        "data": models.into_iter().map(|model| json!({
            "id": model,
            "object": "model"
        })).collect::<Vec<_>>()
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct ModelsQuery {
    client_version: Option<String>,
    format: Option<String>,
}

struct CodexReasoningMetadata {
    supported_levels: Vec<Value>,
    default_level: Value,
    supports_summaries: bool,
}

const CODEX_REASONING_EFFORT_ORDER: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];

fn codex_reasoning_effort_rank(effort: &str) -> usize {
    CODEX_REASONING_EFFORT_ORDER
        .iter()
        .position(|candidate| *candidate == effort)
        .unwrap_or(CODEX_REASONING_EFFORT_ORDER.len())
}

fn codex_reasoning_description(effort: &str) -> String {
    if effort == "none" {
        "Do not use reasoning effort".to_owned()
    } else {
        format!("Use {effort} reasoning effort")
    }
}

fn codex_reasoning_metadata(verified_levels: &[String]) -> CodexReasoningMetadata {
    let mut efforts = verified_levels
        .iter()
        .filter(|effort| CODEX_REASONING_EFFORT_ORDER.contains(&effort.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    efforts.sort_by(|left, right| {
        codex_reasoning_effort_rank(left)
            .cmp(&codex_reasoning_effort_rank(right))
            .then_with(|| left.cmp(right))
    });
    efforts.dedup();

    if efforts.is_empty() {
        return CodexReasoningMetadata {
            supported_levels: vec![json!({
                "effort": "none",
                "description": "Do not request a configurable reasoning effort"
            })],
            default_level: Value::String("none".into()),
            supports_summaries: false,
        };
    }

    let default_effort = efforts
        .iter()
        .find(|effort| effort.as_str() == "high")
        .or_else(|| efforts.iter().find(|effort| effort.as_str() != "none"))
        .cloned()
        .unwrap_or_else(|| "none".to_owned());
    let supports_summaries = efforts.iter().any(|effort| effort != "none");
    let supported_levels = efforts
        .into_iter()
        .map(|effort| {
            json!({
                "description": codex_reasoning_description(&effort),
                "effort": effort,
            })
        })
        .collect();

    CodexReasoningMetadata {
        supported_levels,
        default_level: Value::String(default_effort),
        supports_summaries,
    }
}

fn codex_conservative_reasoning_metadata() -> CodexReasoningMetadata {
    CodexReasoningMetadata {
        supported_levels: vec![json!({
            "effort": "none",
            "description": "Do not request a configurable reasoning effort"
        })],
        default_level: Value::String("none".into()),
        supports_summaries: false,
    }
}

fn codex_catalog_context_window(
    upstreams: &[UpstreamConfig],
    model: &str,
    case_insensitive: bool,
) -> Option<i64> {
    upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.supports_model_with(model, case_insensitive))
        .filter_map(|upstream| upstream.context_config_for_model_with(model, case_insensitive))
        .map(|config| i64::from(config.context_limit))
        .max()
}

fn codex_exposed_models(
    upstreams: &[UpstreamConfig],
    allowlist: &[String],
    case_insensitive: bool,
) -> Vec<String> {
    // Canonical-grouped dedup: one displayed slug per canonical model id.
    // Without explicit alias rules the display spelling is the canonical
    // (trimmed, lowercased) form; stored upstream spellings are never
    // rewritten on the wire. Admin-picked per-upstream mapping labels are
    // the exception: they are exposed verbatim (the operator typed them),
    // see DownstreamModelEntry::from_mapping.
    let group_models = |entries: Vec<DownstreamModelEntry>| -> Vec<String> {
        let mut grouped = BTreeMap::<String, (String, bool)>::new();
        for entry in entries {
            let slug = entry.model.trim();
            let key = crate::state::model_identity_key_with(slug, case_insensitive);
            if key.is_empty() {
                continue;
            }
            let display = if entry.from_mapping || !case_insensitive {
                slug.to_owned()
            } else {
                key.clone()
            };
            match grouped.entry(key) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert((display, entry.from_mapping));
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    let (current_display, current_from_mapping) = slot.get();
                    if (entry.from_mapping && !current_from_mapping)
                        || (entry.from_mapping == *current_from_mapping
                            && display < *current_display)
                    {
                        slot.insert((display, entry.from_mapping));
                    }
                }
            }
        }
        grouped.into_values().map(|(display, _)| display).collect()
    };

    if allowlist.is_empty() {
        let slugs = upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .flat_map(UpstreamConfig::effective_downstream_models_detailed)
            .collect::<Vec<_>>();
        return group_models(slugs);
    }

    let mut allowed_slugs = BTreeMap::new();
    for allowed in allowlist {
        let slug = allowed.trim();
        if !slug.is_empty() {
            allowed_slugs
                .entry(slug.to_ascii_lowercase())
                .or_insert_with(|| slug.to_owned());
        }
    }
    let mut matched_allowlist_keys = BTreeSet::new();
    let mut exposed: Vec<String> = group_models(
        upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .flat_map(UpstreamConfig::effective_downstream_models_detailed)
            .filter_map(|entry| {
                let match_key = entry.model.trim().to_ascii_lowercase();
                if match_key.is_empty() || !allowed_slugs.contains_key(&match_key) {
                    return None;
                }
                matched_allowlist_keys.insert(match_key);
                Some(entry)
            })
            .collect::<Vec<_>>(),
    );
    for (match_key, _slug) in allowed_slugs {
        if !matched_allowlist_keys.contains(&match_key) {
            // Allowlist-only models have no upstream spelling to preserve:
            // display them in canonical form like every other entry.
            exposed.push(match_key);
        }
    }
    exposed.sort();
    exposed
}

/// Build a Codex-compatible model catalog response (`{"models": [ModelInfo]}`).
///
/// Each model entry includes `context_window` (from the upstream's
/// `model_contexts` configuration) so Codex can display real-time context
/// usage percentage in its status bar.
async fn list_models_codex_format(state: &AppState, secret: &str) -> Response {
    let Some(downstream) = state.downstream_for_secret(secret).await else {
        return GatewayError::Unauthorized("invalid downstream key".into()).into_response();
    };
    let snapshot = state.routing_snapshot().await;
    let case_insensitive = state.runtime_settings().model_case_insensitive_matching;
    let verified_reasoning_levels =
        capability_verified_reasoning_levels_by_model(state, &snapshot.upstreams, case_insensitive);

    let model_infos = codex_exposed_models(
        &snapshot.upstreams,
        &downstream.model_allowlist,
        case_insensitive,
    )
        .into_iter()
        .map(|slug| {
            let witness = select_catalog_witness_entry(
                state,
                &snapshot.upstreams,
                &slug,
                case_insensitive,
            );
            let capabilities = witness.as_ref().map(|entry| &entry.capabilities);
            let context_window = capabilities
                .and_then(|capabilities| {
                    capabilities
                        .context_window
                        .and_then(|limit| i64::try_from(limit).ok())
                })
                .or_else(|| {
                    codex_catalog_context_window(
                        &snapshot.upstreams,
                        &slug,
                        case_insensitive,
                    )
                });
            let reasoning_key = crate::state::model_identity_key_with(&slug, case_insensitive);
            let reasoning = verified_reasoning_levels
                .get(&reasoning_key)
                .map(|levels| codex_reasoning_metadata(levels))
                .unwrap_or_else(codex_conservative_reasoning_metadata);
            let supports_custom_tools = capabilities
                .is_some_and(|capabilities| capabilities.supports(Capability::CustomTools));
            let supports_parallel_tool_calls = capabilities
                .is_some_and(|capabilities| capabilities.supports(Capability::ParallelToolCalls));
            let supports_images = capabilities.is_some_and(|capabilities| {
                capabilities.supports(Capability::ImageHttps)
                    && capabilities.supports(Capability::ImageDataUrl)
            });
            json!({
                "slug": slug,
                "display_name": slug,
                "description": null,
                "supported_reasoning_levels": reasoning.supported_levels,
                "default_reasoning_level": reasoning.default_level,
                "multi_agent_version": "v1",
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": 0,
                "base_instructions": "",
                "web_search_tool_type": "text",
                "truncation_policy": {
                    "mode": "bytes",
                    "limit": 10_000
                },
                "supports_reasoning_summaries": reasoning.supports_summaries,
                "default_reasoning_summary": "auto",
                "support_verbosity": false,
                "apply_patch_tool_type": supports_custom_tools.then_some("freeform"),
                "supports_parallel_tool_calls": supports_parallel_tool_calls,
                "supports_image_detail_original": false,
                "context_window": context_window,
                "max_context_window": context_window,
                "effective_context_window_percent": 80,
                "additional_speed_tiers": [],
                "service_tiers": [],
                "experimental_supported_tools": [],
                "input_modalities": if supports_images { json!(["text", "image"]) } else { json!(["text"]) },
            })
        })
        .collect::<Vec<_>>();

    Json(json!({ "models": model_infos })).into_response()
}

/// Translate a JSON extractor rejection into the gateway error shape.
///
/// Body-size rejections (raised by the router-level `DefaultBodyLimit`)
/// surface as 413 with a dedicated code so clients can tell oversized
/// payloads apart from malformed JSON (400).
fn gateway_json_rejection_response(
    state: &AppState,
    rejection: JsonRejection,
    anthropic: bool,
) -> Response {
    let error = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        GatewayError::payload_too_large(state.config.gateway_request_body_limit_mb)
    } else {
        GatewayError::BadRequest("invalid json request body".into())
    };
    if anthropic {
        error.into_anthropic_response()
    } else {
        error.into_response()
    }
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return gateway_json_rejection_response(&state, rejection, false);
        }
    };
    let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if is_stream {
        // G0: Box::pin the ~54KB streaming future instead of inlining it into
        // this handler frame (see translated_stream_state_* guard test).
        return Box::pin(dispatch_streaming_request(
            state,
            headers,
            body,
            EndpointKind::ChatCompletions,
        ))
        .await;
    }
    match process_gateway_request(state, headers, body, EndpointKind::ChatCompletions).await {
        Ok(result) => dispatch_success(result),
        Err(error) => error.into_response(),
    }
}

async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return gateway_json_rejection_response(&state, rejection, false);
        }
    };
    let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if is_stream {
        // G0: Box::pin the ~54KB streaming future instead of inlining it into
        // this handler frame (see translated_stream_state_* guard test).
        return Box::pin(dispatch_streaming_request(
            state,
            headers,
            body,
            EndpointKind::Responses,
        ))
        .await;
    }
    match process_gateway_request(state, headers, body, EndpointKind::Responses).await {
        Ok(result) => dispatch_success(result),
        Err(error) => error.into_response(),
    }
}

async fn claude_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return gateway_json_rejection_response(&state, rejection, true);
        }
    };
    let runtime_settings = state.runtime_settings();
    let claude_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let chat_payload = match claude_messages_to_chat_payload(&body) {
        Ok(payload) => payload,
        Err(message) => return GatewayError::BadRequest(message).into_anthropic_response(),
    };

    // E4: the Anthropic exit carries the same gateway request id as every
    // other client-visible error/success response.
    let request_id = Uuid::new_v4().to_string();
    match Box::pin(process_gateway_request_inner(
        state,
        headers,
        chat_payload,
        EndpointKind::ChatCompletions,
        runtime_settings,
        true,
        None,
        Some(request_id.clone()),
        None,
    ))
    .await
    {
        Ok(result) => dispatch_claude_success(result, claude_stream).await,
        Err(error) => error
            .with_request_id(Some(request_id))
            .into_anthropic_response(),
    }
}

async fn claude_count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return gateway_json_rejection_response(&state, rejection, true);
        }
    };
    let Ok(secret) = downstream_secret_from_headers(&headers) else {
        return GatewayError::Unauthorized("missing authorization header or x-api-key".into())
            .into_anthropic_response();
    };
    let Some(downstream) = state.downstream_for_secret(&secret).await else {
        return GatewayError::Unauthorized("invalid downstream key".into())
            .into_anthropic_response();
    };

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::BadRequest("missing model".into()));
    let model = match model {
        Ok(model) => model,
        Err(error) => return error.into_anthropic_response(),
    };
    if !portal_model_is_allowed(downstream.model_allowlist.as_slice(), model) {
        return GatewayError::gateway_forbidden("model not allowed", "gateway_model_not_allowed")
            .into_anthropic_response();
    }

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::BadRequest("missing messages".into()));
    let messages = match messages {
        Ok(messages) => messages,
        Err(error) => return error.into_anthropic_response(),
    };

    let mut character_count = 0u64;
    for message in messages {
        character_count = character_count
            .saturating_add(extract_claude_content_text(message).chars().count() as u64);
    }
    if let Some(system) = body.get("system") {
        character_count = character_count
            .saturating_add(extract_claude_system_text(system).chars().count() as u64);
    }
    let input_tokens = (character_count / 4).max(1);

    Json(json!({
        "input_tokens": input_tokens
    }))
    .into_response()
}

const GUARD_RELEASE_ACTIVE: u8 = 0;
const GUARD_RELEASE_RELEASING: u8 = 1;
const GUARD_RELEASED: u8 = 2;

struct GatewayReleaseGuard {
    state: Arc<AtomicU8>,
    completed: bool,
}

impl GatewayReleaseGuard {
    fn acquire(state: &Arc<AtomicU8>) -> Result<Option<Self>, RuntimeCoordinationError> {
        match state.compare_exchange(
            GUARD_RELEASE_ACTIVE,
            GUARD_RELEASE_RELEASING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(Some(Self {
                state: state.clone(),
                completed: false,
            })),
            Err(GUARD_RELEASED) => Ok(None),
            Err(_) => Err(RuntimeCoordinationError),
        }
    }

    fn complete(mut self) {
        self.state.store(GUARD_RELEASED, Ordering::Release);
        self.completed = true;
    }
}

impl Drop for GatewayReleaseGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.state.store(GUARD_RELEASE_ACTIVE, Ordering::Release);
        }
    }
}

struct DownstreamConcurrencyGuardInner {
    state: AppState,
    lease: DownstreamConcurrencyLease,
    release_state: Arc<AtomicU8>,
    last_renewed_at: Arc<AtomicU64>,
}

impl DownstreamConcurrencyGuardInner {
    fn spawn_release(
        &self,
    ) -> Result<
        Option<tokio::task::JoinHandle<Result<(), RuntimeCoordinationError>>>,
        RuntimeCoordinationError,
    > {
        // C1.2 (downstream counterpart): the local backend's
        // `downstream_runtime` sits behind a plain `std::sync::Mutex` whose
        // removal is synchronous, so release it right here instead of spawning
        // a task.  A spawned release task is only polled when the runtime next
        // schedules it; synchronous upstream release (C1.2) removed the
        // implicit yield point that used to give that task a chance to run, and
        // the downstream slot must not live or die by that scheduling detail.
        if !self.state.is_redis_runtime_backend() {
            let Some(release_guard) = GatewayReleaseGuard::acquire(&self.release_state)? else {
                return Ok(None);
            };
            let removed = self.state.expire_downstream_request_lease_sync(&self.lease);
            release_guard.complete();
            if removed {
                tracing::debug!(
                    downstream_id = %self.lease.downstream_id(),
                    "downstream concurrency lease released synchronously (local backend)"
                );
            }
            return Ok(None);
        }

        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(
                    downstream_id = %self.lease.downstream_id(),
                    error = %error,
                    "downstream concurrency guard dropped outside Tokio runtime"
                );
                return Ok(None);
            }
        };
        let Some(release_guard) = GatewayReleaseGuard::acquire(&self.release_state)? else {
            return Ok(None);
        };
        let state = self.state.clone();
        let lease = self.lease.clone();
        Ok(Some(runtime.spawn(async move {
            let result = state.release_downstream_concurrency(lease).await;
            if let Err(error) = &result {
                tracing::warn!(
                    error = %error,
                    "failed to release downstream concurrency lease"
                );
            } else {
                release_guard.complete();
            }
            result
        })))
    }
}

impl Drop for DownstreamConcurrencyGuardInner {
    fn drop(&mut self) {
        if let Ok(Some(task)) = self.spawn_release() {
            drop(task);
        }
    }
}

#[derive(Clone)]
struct DownstreamConcurrencyGuard {
    inner: Arc<DownstreamConcurrencyGuardInner>,
}

impl DownstreamConcurrencyGuard {
    fn new(state: AppState, lease: DownstreamConcurrencyLease) -> Self {
        Self {
            inner: Arc::new(DownstreamConcurrencyGuardInner {
                state,
                lease,
                release_state: Arc::new(AtomicU8::new(GUARD_RELEASE_ACTIVE)),
                last_renewed_at: Arc::new(AtomicU64::new(unix_millis())),
            }),
        }
    }

    /// Periodically extends the downstream concurrency lease while the stream
    /// is actively producing chunks.  Long-running streams (> lease TTL) would
    /// otherwise see their lease expire and the portal "running" count drop to
    /// zero while the request is still in flight.  Renewal is throttled to half
    /// the configured TTL and never fails the stream: coordination errors are
    /// logged and the next chunk retries.
    async fn renew_if_due(&self) {
        let interval_ms = (self.inner.state.config.downstream_lease_ttl_seconds / 2)
            .max(30)
            .saturating_mul(1_000);
        let now_ms = unix_millis();
        let last = self.inner.last_renewed_at.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) < interval_ms {
            return;
        }
        if self
            .inner
            .last_renewed_at
            .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        if let Err(error) = self
            .inner
            .state
            .renew_downstream_concurrency(&self.inner.lease)
            .await
        {
            tracing::warn!(
                downstream_id = %self.inner.lease.downstream_id(),
                error = %error,
                "failed to renew downstream concurrency lease; retrying on next chunk"
            );
        }
    }

    async fn release(&self) {
        match self.inner.spawn_release() {
            Ok(Some(task)) => {
                if let Err(error) = task.await {
                    tracing::error!(
                        downstream_id = %self.inner.lease.downstream_id(),
                        error = %error,
                        "downstream concurrency release task failed"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                downstream_id = %self.inner.lease.downstream_id(),
                error = %error,
                "downstream concurrency release already in progress"
            ),
        }
    }
}

struct UpstreamRequestGuardInner {
    state: AppState,
    lease: UpstreamRequestLease,
    release_state: Arc<AtomicU8>,
}

impl UpstreamRequestGuardInner {
    fn spawn_release(
        &self,
    ) -> Result<
        Option<tokio::task::JoinHandle<Result<(), RuntimeCoordinationError>>>,
        RuntimeCoordinationError,
    > {
        // C1.2: the local backend's lease table sits behind a plain
        // `std::sync::Mutex` and its release is a synchronous `remove`, so
        // release it right here instead of spawning a task.  This closes the
        // §2.2(a) leak where a runtime that is shutting down spawns the
        // release task but never polls it, silently pinning the slot for the
        // whole TTL.  C1.3: on any failure the `GatewayReleaseGuard` drops
        // back to ACTIVE, so a later drop of another guard clone retries.
        if !self.state.is_redis_runtime_backend() {
            let Some(release_guard) = GatewayReleaseGuard::acquire(&self.release_state)? else {
                return Ok(None);
            };
            // C1.4: `expire_upstream_request_lease_sync` no longer uses
            // `try_lock`; it removes the lease synchronously (or reports it
            // as already gone, which is an idempotent success).
            let removed = self.state.expire_upstream_request_lease_sync(&self.lease);
            release_guard.complete();
            if removed {
                tracing::debug!(
                    upstream_id = %self.lease.upstream_id(),
                    "upstream request lease released synchronously (local backend)"
                );
            }
            return Ok(None);
        }

        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                // Synchronous fallback (P7): the release task cannot be
                // spawned, so mark the lease immediately expired instead of
                // only logging and pinning the slot until the TTL sweep.
                // The local backend was handled above; this branch only runs
                // for the Redis backend, whose lease TTL self-heals natively.
                if self.state.expire_upstream_request_lease_sync(&self.lease) {
                    tracing::warn!(
                        upstream_id = %self.lease.upstream_id(),
                        error = %error,
                        "upstream request guard dropped outside Tokio runtime; lease reclaimed synchronously"
                    );
                } else {
                    tracing::error!(
                        upstream_id = %self.lease.upstream_id(),
                        error = %error,
                        "upstream request guard dropped outside Tokio runtime; lease left for TTL reclamation"
                    );
                }
                return Ok(None);
            }
        };
        let Some(release_guard) = GatewayReleaseGuard::acquire(&self.release_state)? else {
            return Ok(None);
        };
        let state = self.state.clone();
        let lease = self.lease.clone();
        let upstream_id = lease.upstream_id().to_string();
        Ok(Some(runtime.spawn(async move {
            let result = state.release_upstream_request(lease).await;
            if let Err(error) = &result {
                let failure_count = state.redis_upstream_release_failure_count();
                tracing::warn!(
                    upstream_id = %upstream_id,
                    error = %error,
                    failure_count,
                    "failed to release upstream request lease"
                );
            } else {
                release_guard.complete();
            }
            result
        })))
    }
}

impl Drop for UpstreamRequestGuardInner {
    fn drop(&mut self) {
        if let Ok(Some(task)) = self.spawn_release() {
            drop(task);
        }
    }
}

#[derive(Clone)]
struct UpstreamRequestGuard {
    inner: Arc<UpstreamRequestGuardInner>,
}

impl UpstreamRequestGuard {
    fn new(state: AppState, lease: UpstreamRequestLease) -> Self {
        Self {
            inner: Arc::new(UpstreamRequestGuardInner {
                state,
                lease,
                release_state: Arc::new(AtomicU8::new(GUARD_RELEASE_ACTIVE)),
            }),
        }
    }

    async fn renew(&self) -> Result<(), RuntimeCoordinationError> {
        self.inner
            .state
            .renew_upstream_request(&self.inner.lease)
            .await
    }

    async fn release(&self) -> Result<(), RuntimeCoordinationError> {
        match self.inner.spawn_release()? {
            Some(task) => match task.await {
                Ok(result) => result,
                Err(error) => {
                    tracing::error!(
                        upstream_id = %self.inner.lease.upstream_id(),
                        error = %error,
                        "upstream request release task failed"
                    );
                    Err(RuntimeCoordinationError)
                }
            },
            None => Ok(()),
        }
    }
}

/// C2.1: the shared `AbortHandle` slot for the spawned lease heartbeat.
/// The heartbeat renews the upstream lease at ttl/3 regardless of chunk flow,
/// so neither a long unary request nor a silent stretch inside a stream is
/// ever reclaimed as stale (C2.3) once the TTL is small (C2.2).  The spawned
/// task owns only `state` + `lease` clones, never the guard, so it cannot
/// prevent the final guard clone from dropping and releasing synchronously
/// (C1.2).
#[derive(Default)]
struct HeartbeatSlot {
    abort: TokioMutex<Option<AbortHandle>>,
}

#[derive(Clone)]
struct UpstreamRequestReservation {
    guard: Arc<TokioMutex<Option<UpstreamRequestGuard>>>,
    /// Last renewal wall-clock ms (P7).  Long streaming requests renew their
    /// local/Redis upstream lease at half the configured TTL so the slot is
    /// never reclaimed mid-stream; leaked guards (dropped without release)
    /// stop producing chunks and therefore stop renewing, letting the TTL
    /// lapse and the lazy sweep reclaim the slot.  C2.1 adds a ttl/3 heartbeat
    /// (see `heartbeat`) so even unary requests and silent streams renew.
    last_renewed_at: Arc<AtomicU64>,
    /// C2.1: handle to the spawned ttl/3 heartbeat for the current guard's
    /// lease.  Shared across clones so the last clone's `Drop` can abort it.
    heartbeat: Arc<HeartbeatSlot>,
}

impl UpstreamRequestReservation {
    fn new(guard: UpstreamRequestGuard) -> Self {
        let reservation = Self {
            guard: Arc::new(TokioMutex::new(Some(guard))),
            last_renewed_at: Arc::new(AtomicU64::new(unix_millis())),
            heartbeat: Arc::new(HeartbeatSlot::default()),
        };
        reservation.spawn_heartbeat();
        reservation
    }

    /// C2.1: (re)start the ttl/3 lease heartbeat for the current guard's
    /// lease.  Silently skipped when no Tokio runtime is usable (the per-chunk
    /// `renew_if_due` backstop and the TTL fallback still apply) or when the
    /// guard is already released.  Replaces and aborts any previous heartbeat,
    /// which is how `reserve_next` moves the heartbeat onto a fresh lease.
    fn spawn_heartbeat(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let Some(guard) = self.guard.try_lock().ok().and_then(|slot| slot.clone()) else {
            return;
        };
        let state = guard.inner.state.clone();
        let lease = guard.inner.lease.clone();
        let runtime_settings_changes = state
            .is_redis_runtime_backend()
            .then(|| state.runtime_settings_change_receiver());
        let handle = runtime.spawn(upstream_lease_heartbeat(
            state,
            lease,
            runtime_settings_changes,
        ));
        if let Ok(mut slot) = self.heartbeat.abort.try_lock() {
            if let Some(previous) = slot.replace(handle.abort_handle()) {
                previous.abort();
            }
        }
    }

    /// C2.1: stop the running heartbeat (if any).  Called from `release` so a
    /// released lease is not renewed forever by a straggling task, and from
    /// the last-clone `Drop` so a dropped reservation does not leak the task.
    fn abort_heartbeat(&self) {
        if let Ok(mut slot) = self.heartbeat.abort.try_lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
    }

    /// Renews the upstream lease if due, mirroring
    /// `DownstreamConcurrencyGuard::renew_if_due`: throttled to half the
    /// configured local lease TTL and never fatal (coordination errors are
    /// logged and the next chunk retries).  Called per chunk from the
    /// streaming body loop.
    async fn renew_if_due(&self) {
        let Some(guard) = self.guard.lock().await.clone() else {
            return;
        };
        let interval_secs = (upstream_lease_ttl_seconds(&guard.inner.state) / 2).max(1);
        let interval_ms = interval_secs.saturating_mul(1_000);
        let now_ms = unix_millis();
        let last = self.last_renewed_at.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) < interval_ms {
            return;
        }
        if self
            .last_renewed_at
            .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        if let Err(error) = guard.renew().await {
            tracing::warn!(
                upstream_id = %guard.inner.lease.upstream_id(),
                error = %error,
                "failed to renew upstream request lease; retrying on next chunk"
            );
        }
    }

    async fn release(&self) -> Result<(), RuntimeCoordinationError> {
        // C2.1: stop renewing the about-to-be-released lease before it goes.
        self.abort_heartbeat();
        let guard = self.guard.lock().await.clone();
        let Some(guard) = guard else {
            return Ok(());
        };
        guard.release().await?;

        let mut slot = self.guard.lock().await;
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.inner, &guard.inner))
        {
            slot.take();
        }
        Ok(())
    }

    async fn reserve_next(
        &self,
        state: &AppState,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        model: &str,
    ) -> Result<(), GatewayError> {
        self.release()
            .await
            .map_err(|_| runtime_coordination_unavailable_gateway_error())?;
        let lease = state
            .try_reserve_upstream_account_request(upstream, key_fingerprint, model)
            .await
            .map_err(|error| {
                upstream_admission_gateway_error(
                    error,
                    "failed to reserve capacity for an internal upstream retry",
                )
            })?;
        *self.guard.lock().await = Some(UpstreamRequestGuard::new(state.clone(), lease));
        // C2.1: the released heartbeat was aborted by `self.release()`; start a
        // fresh one so the new lease is covered for its whole lifetime.
        self.spawn_heartbeat();
        Ok(())
    }
}

impl Drop for UpstreamRequestReservation {
    fn drop(&mut self) {
        // C2.1: when the last reservation clone goes away, stop the heartbeat
        // so the spawned task does not keep renewing a lease that is (or is
        // about to be) released by the final guard clone's `Drop`.  Without
        // this the detached task would leak and pin the lease as live.
        if Arc::strong_count(&self.heartbeat) == 1 {
            self.abort_heartbeat();
        }
    }
}

/// C2.1: renews an upstream lease every `ttl/3` independent of any
/// chunk flow, so long unary requests and silent streams keep their lease
/// alive.  Redis runtime settings are hot-swappable: a settings update wakes
/// the task and renews immediately before the next interval is calculated.
/// That immediate renewal is needed in both directions: a decreased TTL must
/// be adopted promptly, while an increased TTL must not skip a renewal that
/// was due under the old, shorter TTL.  Renewing a lease that was already
/// released or reclaimed is a no-op success (`renew_upstream_request`), so a
/// tick that races a release is harmless.  The task exits only when aborted
/// by the reservation lifecycle.
async fn upstream_lease_heartbeat(
    state: AppState,
    lease: UpstreamRequestLease,
    runtime_settings_changes: Option<watch::Receiver<u64>>,
) {
    if let Some(mut runtime_settings_changes) = runtime_settings_changes {
        loop {
            let interval = Duration::from_secs((upstream_lease_ttl_seconds(&state) / 3).max(1));
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    renew_upstream_lease_from_heartbeat(&state, &lease).await;
                }
                changed = runtime_settings_changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    renew_upstream_lease_from_heartbeat(&state, &lease).await;
                }
            }
        }
    }

    let interval = Duration::from_secs((state.config.upstream_local_lease_ttl_seconds / 3).max(1));
    loop {
        tokio::time::sleep(interval).await;
        renew_upstream_lease_from_heartbeat(&state, &lease).await;
    }
}

async fn renew_upstream_lease_from_heartbeat(state: &AppState, lease: &UpstreamRequestLease) {
    if let Err(error) = state.renew_upstream_request(lease).await {
        tracing::warn!(
            upstream_id = %lease.upstream_id(),
            error = %error,
            "upstream lease heartbeat failed; relying on per-chunk renewal and the TTL backstop"
        );
    }
}

fn upstream_lease_ttl_seconds(state: &AppState) -> u64 {
    if state.is_redis_runtime_backend() {
        state.runtime_settings().upstream_local_lease_ttl_seconds
    } else {
        state.config.upstream_local_lease_ttl_seconds
    }
}

#[derive(Clone)]
struct StreamCompletionContext {
    state: AppState,
    route_health_key: RouteHealthKey,
    route_attempts: RequestRouteAttempts,
    route_health_permit: Arc<TokioMutex<Option<RouteHealthPermit>>>,
    upstream_request_guard: UpstreamRequestReservation,
    downstream_concurrency_guard: DownstreamConcurrencyGuard,
    hedge_control: Option<HedgeAttemptControl>,
    /// One-shot guard: the stream body sets this once the first semantic
    /// output is observed, and `mark_healthy_verdict` settles the half-open
    /// lease as healthy (T2). The atomic makes the settle idempotent across
    /// the prefetch and body loops and any concurrent cleanup paths.
    health_verdict_pending: Arc<AtomicBool>,
}

impl StreamCompletionContext {
    async fn release_all(&self) {
        if !self.is_hedge_loser() {
            self.downstream_concurrency_guard.release().await;
        }
        let _ = self.upstream_request_guard.release().await;
    }

    fn is_hedge_loser(&self) -> bool {
        self.hedge_control
            .as_ref()
            .is_some_and(HedgeAttemptControl::is_loser)
    }

    async fn mark_success(&self) {
        if let Err(error) =
            finish_route_health_permit(&self.route_health_permit, RouteOutcome::Success).await
        {
            tracing::error!(
                error = %error,
                "failed to finish route health after stream success"
            );
        }
    }

    /// Settle the route-health lease as healthy at the first semantic output
    /// of a streaming response (T2). The lease is finished with Success (the
    /// route cooldown is cleared and the half-open exclusive window is
    /// released) while the stream is still open, so concurrent requests are
    /// no longer blocked by the probe stream. A settled permit turns later
    /// stream failures into fresh no-lease observations.
    async fn mark_healthy_verdict(&self) {
        if !self.health_verdict_pending.swap(false, Ordering::AcqRel) {
            return;
        }
        // The permit stays inside the mutex for the whole settle: taking it
        // out across the await opened a window in which a concurrent
        // completion path (stream error, pre-header cancellation) found an
        // empty slot and silently dropped its outcome. Holding the guard
        // makes those paths wait instead — settle_healthy only awaits the
        // registry mutex / Redis round-trip, never this slot (T9).
        let mut slot = self.route_health_permit.lock().await;
        let Some(permit) = slot.as_mut() else {
            return;
        };
        // After a successful settle the slot holds a `Settled` permit whose
        // later success/cancellation are no-ops and whose later failures
        // become no-lease observations (the stream may still fail after the
        // first semantic output); after a coordination error it keeps the
        // live lease for the normal completion path. Errors are logged only
        // and never affect the request.
        if let Err(error) = permit.settle_healthy().await {
            tracing::error!(
                error = %error,
                "failed to settle route health after first semantic output"
            );
        }
    }

    async fn mark_failure(&self) {
        if let Err(error) = finish_route_health_permit(
            &self.route_health_permit,
            RouteOutcome::UncertainRouteFailure(FailureClass::Transport),
        )
        .await
        {
            tracing::error!(
                error = %error,
                "failed to finish route health after stream failure"
            );
        }
        self.route_attempts
            .record_failure(&self.route_health_key, FailureClass::Transport, None);
        for observation in self.route_attempts.take_newly_exhausted() {
            if let Err(error) = self
                .state
                .observe_route_set_failure(
                    &observation.key,
                    observation.class,
                    observation.retry_after,
                )
                .await
            {
                tracing::error!(
                    error = %error,
                    "failed to record route-set health after stream failure"
                );
            }
        }
    }

    async fn mark_cancelled(&self) {
        if let Err(error) =
            finish_route_health_permit(&self.route_health_permit, RouteOutcome::Cancelled).await
        {
            tracing::error!(
                error = %error,
                "failed to cancel route health after stream completion"
            );
        }
    }
}

#[derive(Clone, Default)]
struct PreHeaderStreamCancellation {
    armed: Arc<Mutex<Option<PreHeaderStreamCancellationContext>>>,
}

struct PreHeaderStreamCancellationContext {
    completion: StreamCompletionContext,
    usage_log: StreamUsageLogContext,
}

impl PreHeaderStreamCancellation {
    fn arm(&self, completion: StreamCompletionContext, usage_log: StreamUsageLogContext) {
        let mut armed = self
            .armed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        debug_assert!(armed.is_none(), "pre-header cancellation context re-armed");
        *armed = Some(PreHeaderStreamCancellationContext {
            completion,
            usage_log,
        });
    }

    fn disarm(&self) {
        self.armed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    async fn cancel(&self) {
        let context = self
            .armed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(context) = context else {
            return;
        };
        finalize_stream_interruption(
            Some(context.completion),
            Some(context.usage_log),
            None,
            StreamInterruption::DownstreamBodyDropped {
                usable_output_delivered: false,
            },
        )
        .await;
    }
}

#[cfg(test)]
struct PreHeaderPreparationTestGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
const PRE_HEADER_PREPARATION_TEST_GATE_HEADER: &str = "x-gateway-test-pre-header-gate";

#[cfg(test)]
static PRE_HEADER_PREPARATION_TEST_GATE: Mutex<Option<PreHeaderPreparationTestGate>> =
    Mutex::new(None);

#[cfg(test)]
static UPSTREAM_RESERVATION_FAILURE_TEST_UPSTREAM: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
fn install_pre_header_preparation_test_gate() -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let mut gate = PRE_HEADER_PREPARATION_TEST_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        gate.is_none(),
        "pre-header preparation test gate already installed"
    );
    *gate = Some(PreHeaderPreparationTestGate {
        entered,
        release: release_rx,
    });
    (entered_rx, release)
}

#[cfg(test)]
fn install_upstream_reservation_failure_test_hook(upstream_id: impl Into<String>) {
    let mut failure = UPSTREAM_RESERVATION_FAILURE_TEST_UPSTREAM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    failure.push(upstream_id.into());
}

#[cfg(test)]
fn take_upstream_reservation_failure_test_hook(upstream_id: &str) -> bool {
    let mut failure = UPSTREAM_RESERVATION_FAILURE_TEST_UPSTREAM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = failure.iter().position(|id| id == upstream_id) {
        failure.swap_remove(index);
        true
    } else {
        false
    }
}

#[cfg(test)]
async fn wait_on_pre_header_preparation_test_gate(gated: bool) {
    if !gated {
        return;
    }
    let gate = PRE_HEADER_PREPARATION_TEST_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
}

#[derive(Clone)]
struct ResponseHistoryContext {
    state: AppState,
    downstream_key_id: String,
    history_input_items: Vec<Value>,
    history_request_state: Map<String, Value>,
    tool_registry: Option<ToolAdapterRegistry>,
}

/// The provider profile that produced a stored response history entry
/// (T3.1).  Recorded at dispatch time from the actually-selected route, and
/// compared against the route selected on a later `previous_response_id`
/// replay so cross-provider history reuse can be detected and sanitized.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewaySourceProfile {
    upstream_id: String,
    key_fingerprint: String,
    dialect_profile_key: DialectProfileKey,
    protocol: WireProtocol,
}

impl GatewaySourceProfile {
    fn from_route(
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> Self {
        let wire_protocol = WireProtocol::from(protocol);
        Self {
            upstream_id: upstream.id.clone(),
            key_fingerprint: key_fingerprint.to_string(),
            dialect_profile_key: DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint.to_string(),
                runtime_model_slug.to_string(),
                wire_protocol,
            ),
            protocol: wire_protocol,
        }
    }
}

impl ResponseHistoryContext {
    fn with_fallback_stage(&self, stage: ChatFallbackStage) -> Self {
        let mut history_request_state = self.history_request_state.clone();
        history_request_state.insert(
            "fallback_stage".to_string(),
            Value::String(stage.as_str().to_string()),
        );
        Self {
            state: self.state.clone(),
            downstream_key_id: self.downstream_key_id.clone(),
            history_input_items: self.history_input_items.clone(),
            history_request_state,
            tool_registry: self.tool_registry.clone(),
        }
    }

    fn with_selected_route(
        &self,
        continuation: GatewayContinuationState,
        fallback_stage: Option<ChatFallbackStage>,
    ) -> Result<Self, GatewayError> {
        let mut history_request_state = self.history_request_state.clone();
        let continuation = serde_json::to_value(continuation).map_err(|error| {
            GatewayError::upstream_invalid_response(
                format!("failed to serialize gateway continuation state: {error}"),
                "gateway_response_history_invalid",
            )
        })?;
        history_request_state.insert("_gateway_continuation".to_string(), continuation);
        if let Some(stage) = fallback_stage {
            history_request_state.insert(
                "fallback_stage".to_string(),
                Value::String(stage.as_str().to_string()),
            );
        }
        Ok(Self {
            state: self.state.clone(),
            downstream_key_id: self.downstream_key_id.clone(),
            history_input_items: self.history_input_items.clone(),
            history_request_state,
            tool_registry: self.tool_registry.clone(),
        })
    }

    /// The source profile recorded when this history context's response was
    /// (or will be) captured, if one has been set yet (T3.1).
    fn source_profile(&self) -> Option<GatewaySourceProfile> {
        self.history_request_state
            .get("_gateway_source_profile")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// Record which provider profile will have produced the response stored
    /// from this context, so a later `previous_response_id` replay can detect
    /// that the history crossed provider profiles (T3.1).
    fn with_source_profile(&self, profile: GatewaySourceProfile) -> Self {
        let mut history_request_state = self.history_request_state.clone();
        if let Ok(value) = serde_json::to_value(&profile) {
            history_request_state.insert("_gateway_source_profile".to_string(), value);
        }
        Self {
            state: self.state.clone(),
            downstream_key_id: self.downstream_key_id.clone(),
            history_input_items: self.history_input_items.clone(),
            history_request_state,
            tool_registry: self.tool_registry.clone(),
        }
    }

    fn tool_registry(&self) -> Option<&ToolAdapterRegistry> {
        self.tool_registry.as_ref()
    }

    fn set_tool_registry(&mut self, registry: ToolAdapterRegistry) {
        let registry = self
            .tool_registry
            .as_ref()
            .map(|existing| existing.merged_with(&registry))
            .unwrap_or(registry);
        if let Ok(value) = serde_json::to_value(&registry) {
            self.history_request_state
                .insert("gateway_tool_registry".to_string(), value);
        }
        self.tool_registry = Some(registry);
    }

    fn continuation_upstream_id(&self) -> Option<&str> {
        self.history_request_state
            .get("_gateway_continuation")
            .and_then(Value::as_object)
            .and_then(|object| {
                object.get("upstream_id").or_else(|| {
                    object
                        .get("profile_key")
                        .or_else(|| object.get("preferred_profile"))
                        .and_then(Value::as_object)
                        .and_then(|profile| profile.get("upstream_id"))
                })
            })
            .and_then(Value::as_str)
    }

    fn exact_continuation_state(&self) -> Result<Option<LoadedContinuation>, GatewayError> {
        let Some(value) = self.history_request_state.get("_gateway_continuation") else {
            return Ok(None);
        };
        let Some(object) = value.as_object() else {
            return Err(response_history_invalid(
                "cached gateway continuation state is malformed",
            ));
        };
        if !object.contains_key("version") {
            return Ok(None);
        }
        let continuation = serde_json::from_value::<GatewayContinuationState>(value.clone())
            .map_err(|_| {
                response_history_invalid("cached gateway continuation state is malformed")
            })?;
        continuation.load().map(Some).map_err(|_| {
            response_history_invalid("cached gateway continuation version is unsupported")
        })
    }

    fn legacy_continuation_upstream_id(&self) -> Result<Option<&str>, GatewayError> {
        let Some(value) = self.history_request_state.get("_gateway_continuation") else {
            return Ok(None);
        };
        let Some(object) = value.as_object() else {
            return Err(response_history_invalid(
                "cached gateway continuation state is malformed",
            ));
        };
        if object.contains_key("version") {
            return Ok(None);
        }
        let upstream_id = object
            .get("upstream_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|upstream_id| !upstream_id.is_empty())
            .ok_or_else(|| {
                response_history_invalid("cached legacy gateway continuation state is malformed")
            })?;
        Ok(Some(upstream_id))
    }

    fn tool_registry_version(&self) -> Option<u32> {
        self.tool_registry.as_ref().map(|registry| registry.version)
    }

    fn has_trusted_tool_registry_version(&self, continuation: &GatewayContinuationState) -> bool {
        match continuation.tool_registry_version() {
            Some(expected) => {
                expected == ToolAdapterRegistry::VERSION
                    && self
                        .tool_registry
                        .as_ref()
                        .is_some_and(|registry| registry.version == expected)
            }
            None => {
                self.tool_registry.is_none()
                    && !self
                        .history_request_state
                        .contains_key("gateway_tool_registry")
            }
        }
    }

    fn store_from_completed_event(&self, event: &Value) -> bool {
        if event.get("type").and_then(Value::as_str) != Some("response.completed") {
            return false;
        }
        self.store_from_response_value(event.get("response").unwrap_or(&Value::Null))
    }

    fn store_from_response_body(&self, response: &Value) -> bool {
        self.store_from_response_value(response)
    }

    fn store_from_response_value(&self, response: &Value) -> bool {
        let Some(response_id) = response
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        let Some(output) = response.get("output").and_then(Value::as_array) else {
            return false;
        };

        let mut items = self.history_input_items.clone();
        items.extend(output.iter().cloned());
        let mut request_state = self.history_request_state.clone();
        if output
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        {
            if let Some(value) = request_state.get_mut("_gateway_continuation") {
                if let Ok(mut continuation) =
                    serde_json::from_value::<GatewayContinuationState>(value.clone())
                {
                    continuation.observe_reasoning_carrier();
                    if let Ok(observed) = serde_json::to_value(continuation) {
                        *value = observed;
                    }
                }
            }
        }
        if let Some(registry) = self.tool_registry.as_ref() {
            if let Ok(value) = serde_json::to_value(registry) {
                request_state.insert("gateway_tool_registry".to_string(), value);
            }
        }
        self.state.store_response_history(
            self.downstream_key_id.clone(),
            response_id.to_string(),
            items,
            request_state,
        );
        true
    }
}

fn response_history_invalid(message: impl Into<String>) -> GatewayError {
    GatewayError::classified(
        StatusCode::BAD_REQUEST,
        message,
        "invalid_request_error",
        "gateway_response_history_invalid",
        "gateway_response_history_invalid",
        None,
        Some(json!({ "scope": "gateway" })),
    )
}

const RESPONSE_HISTORY_STATE_FIELDS: &[&str] = &[
    "instructions",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "fallback_stage",
];

fn normalize_responses_input_items(input: &Value) -> Result<Vec<Value>, GatewayError> {
    match input {
        Value::String(content) => Ok(vec![json!({
            "role": "user",
            "content": content,
        })]),
        Value::Array(items) => Ok(items.clone()),
        Value::Object(_) => Ok(vec![input.clone()]),
        _ => Err(GatewayError::BadRequest(
            "unsupported responses input payload".into(),
        )),
    }
}

fn responses_input_item_is_chat_fallback_safe(item: &Value) -> bool {
    match item {
        Value::String(_) => true,
        Value::Object(object) => {
            if matches!(
                object.get("type").and_then(Value::as_str),
                Some(
                    "function_call"
                        | "function_call_output"
                        | "custom_tool_call"
                        | "custom_tool_call_output",
                )
            ) {
                return false;
            }
            if object.contains_key("tool_call_id") || object.contains_key("tool_calls") {
                return false;
            }
            !matches!(
                object.get("role").and_then(Value::as_str),
                Some("tool" | "function")
            )
        }
        _ => false,
    }
}

fn simplify_responses_input_for_chat_fallback(input: &Value) -> Value {
    match input {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter(|item| responses_input_item_is_chat_fallback_safe(item))
                .cloned()
                .collect(),
        ),
        Value::Object(_) if responses_input_item_is_chat_fallback_safe(input) => input.clone(),
        Value::String(_) => input.clone(),
        _ => Value::Array(Vec::new()),
    }
}

fn compact_responses_input_for_chat_fallback(input: &Value) -> Value {
    match simplify_responses_input_for_chat_fallback(input) {
        Value::Array(items) => items
            .into_iter()
            .rev()
            .find(|item| match item {
                Value::String(text) => !text.trim().is_empty(),
                Value::Object(object) => object
                    .get("content")
                    .or_else(|| object.get("text"))
                    .is_some_and(|value| value_has_payload(Some(value))),
                _ => false,
            })
            .map(|item| Value::Array(vec![item]))
            .unwrap_or_else(|| Value::Array(Vec::new())),
        Value::String(text) if !text.trim().is_empty() => Value::Array(vec![Value::String(text)]),
        Value::Object(object) => Value::Array(vec![Value::Object(object)]),
        _ => Value::Array(Vec::new()),
    }
}

fn capture_response_history_state(object: &Map<String, Value>) -> Map<String, Value> {
    let mut state = Map::new();
    for field in RESPONSE_HISTORY_STATE_FIELDS {
        if let Some(value) = object.get(*field) {
            state.insert((*field).to_string(), value.clone());
        }
    }
    state
}

fn apply_response_history_state(object: &mut Map<String, Value>, state: &Map<String, Value>) {
    for field in RESPONSE_HISTORY_STATE_FIELDS {
        if let Some(value) = state.get(*field) {
            object
                .entry((*field).to_string())
                .or_insert_with(|| value.clone());
        }
    }
}

async fn prepare_response_history_context(
    state: &AppState,
    downstream_key_id: &str,
    body: &mut Value,
) -> Result<ResponseHistoryContext, GatewayError> {
    prepare_response_history_context_with_replay(state, downstream_key_id, body, true).await
}

async fn prepare_response_history_context_with_replay(
    state: &AppState,
    downstream_key_id: &str,
    body: &mut Value,
    replay_prior_history: bool,
) -> Result<ResponseHistoryContext, GatewayError> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| GatewayError::BadRequest("responses body must be an object".into()))?;
    let previous_response_id = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut history_request_state = capture_response_history_state(object);
    let mut tool_registry = None;
    object.remove("_gateway_continuation");
    object.remove("gateway_tool_registry");
    let current_input_items = match object.get("input") {
        Some(input) => normalize_responses_input_items(input)?,
        None if previous_response_id.is_some() => Vec::new(),
        None => return Err(GatewayError::BadRequest("missing input".into())),
    };

    let effective_input_items = if let Some(previous_response_id) = previous_response_id.as_deref()
    {
        let prior_history = state
            .response_history(downstream_key_id, previous_response_id)
            .await
            .ok_or_else(|| {
                GatewayError::classified(
                    StatusCode::BAD_REQUEST,
                    "unknown previous_response_id; cached response history is unavailable",
                    "invalid_request_error",
                    "gateway_response_history_invalid",
                    "gateway_response_history_invalid",
                    None,
                    Some(json!({ "scope": "gateway" })),
                )
            })?;
        history_request_state = prior_history.request_state;
        history_request_state.extend(capture_response_history_state(object));
        apply_response_history_state(object, &history_request_state);
        tool_registry = history_request_state
            .get("gateway_tool_registry")
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolAdapterRegistry>(value).ok());
        if replay_prior_history {
            let mut prior_items = prior_history.items;
            prior_items.extend(current_input_items);
            prior_items
        } else {
            current_input_items
        }
    } else {
        current_input_items
    };

    object.insert("input".into(), Value::Array(effective_input_items.clone()));
    object.remove("previous_response_id");

    Ok(ResponseHistoryContext {
        state: state.clone(),
        downstream_key_id: downstream_key_id.to_string(),
        history_input_items: effective_input_items,
        history_request_state,
        tool_registry,
    })
}

/// P2: strip supplier-bound fields from replayed conversation history before
/// it is dispatched to a different provider on the continuation-pin escape
/// pass.  The whitelist removes only vendor-bound artifacts:
///
/// - `encrypted_content` on reasoning / message items (opaque vendor payload,
///   meaningless and unsafe to replay elsewhere);
/// - gateway-issued thinking signatures (`gw1.` prefix) and the gateway's own
///   `_gateway_claude_thinking` carrier (single-provider internal state);
/// - the originating provider's item `id` (identity is supplier-scoped).
///
/// Every text and tool-call payload is preserved item-for-item: only the
/// whitelist above is removed, nothing else (invariant 6).
fn sanitize_history_for_cross_provider_replay(body: &mut Value) {
    fn sanitize_item(value: &mut Value) {
        match value {
            Value::Array(items) => {
                for item in items {
                    sanitize_item(item);
                }
            }
            Value::Object(object) => {
                object.remove("id");
                object.remove("encrypted_content");
                object.remove("_gateway_claude_thinking");
                if object
                    .get("signature")
                    .and_then(Value::as_str)
                    .is_some_and(thinking_signature::is_gateway_issued_thinking_signature)
                {
                    object.remove("signature");
                }
                for child in object.values_mut() {
                    sanitize_item(child);
                }
            }
            _ => {}
        }
    }
    if let Some(input) = body.get_mut("input") {
        sanitize_item(input);
    }
}

/// T3.3: on a cross-profile history replay, additionally normalize every
/// replayed `function_call` item's `arguments` through
/// [`normalize_tool_arguments`].  History written before T2 can already carry
/// `{}`-prefixed pollutant strings (the `{}{...}` extra-data shape), so the
/// replay path repairs them once more before they reach the upstream.
fn normalize_replayed_history_tool_arguments(
    body: &mut Value,
    model: Option<&str>,
    request_id: &str,
) {
    let Some(input) = body.get_mut("input") else {
        return;
    };
    let Some(items) = input.as_array_mut() else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        let Some(raw) = object.get("arguments").and_then(Value::as_str) else {
            continue;
        };
        let (normalized, repair) = normalize_tool_arguments(raw);
        let Some(reason) = repair else {
            continue;
        };
        let call_id = object
            .get("call_id")
            .or_else(|| object.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        tracing::warn!(
            event = "tool_call_arguments_anomaly",
            reason = reason.as_str(),
            call_id,
            model = %model.unwrap_or(""),
            request_id,
            fragment = %raw,
            phase = "cross_profile_history_replay",
            "tool call arguments anomaly during cross-profile history replay"
        );
        object.insert("arguments".into(), Value::String(normalized.into_owned()));
    }
}

/// P2: candidate protocol list used once the continuation-pin escape lifts
/// the per-pinned-protocol lock.  Reuses the unconstrained routing strategy
/// (the same choice the gateway makes when no continuation pin is active), so
/// a `Messages`-pinned continuation can still reach the endpoint's native or
/// opposite protocol on the escape pass.
fn continuation_escape_candidate_protocols(
    endpoint: EndpointKind,
    responses_strategy: ResponsesRouteStrategy,
) -> Vec<UpstreamProtocol> {
    match responses_strategy {
        ResponsesRouteStrategy::ProtocolAgnostic => {
            vec![endpoint.native_protocol(), endpoint.opposite()]
        }
        ResponsesRouteStrategy::Responses => vec![UpstreamProtocol::Responses],
        ResponsesRouteStrategy::ChatFallback => vec![UpstreamProtocol::ChatCompletions],
        ResponsesRouteStrategy::Unavailable => Vec::new(),
    }
}

fn apply_chat_fallback_stage(body: &mut Value, stage: ChatFallbackStage) {
    match stage {
        ChatFallbackStage::HighFidelity => {}
        ChatFallbackStage::ExtensionCleanup => {
            strip_responses_chat_fallback_extensions(body);
            if let Some(object) = body.as_object_mut() {
                if let Some(input) = object.get("input").cloned() {
                    object.insert(
                        "input".into(),
                        simplify_responses_input_for_chat_fallback(&input),
                    );
                }
            }
        }
        ChatFallbackStage::ToolReplayReduction => {
            apply_chat_fallback_stage(body, ChatFallbackStage::ExtensionCleanup);
            if let Some(object) = body.as_object_mut() {
                object.remove("tool_choice");
            }
        }
        ChatFallbackStage::HistoryCompaction => {
            apply_chat_fallback_stage(body, ChatFallbackStage::ToolReplayReduction);
            if let Some(object) = body.as_object_mut() {
                object.remove("tools");
                if let Some(input) = object.get("input").cloned() {
                    object.insert(
                        "input".into(),
                        compact_responses_input_for_chat_fallback(&input),
                    );
                }
            }
        }
    }
}

async fn prepare_responses_chat_fallback_request(
    state: &AppState,
    downstream_key_id: &str,
    source_body: &Value,
    stage: ChatFallbackStage,
) -> Result<(Value, ResponseHistoryContext), GatewayError> {
    let mut body = source_body.clone();
    let tool_adaptation = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| build_chat_fallback_tool_adaptation(tools).map_err(protocol_error_to_gateway))
        .transpose()?;
    let mut response_history_context = prepare_response_history_context_with_replay(
        state,
        downstream_key_id,
        &mut body,
        matches!(stage, ChatFallbackStage::HighFidelity),
    )
    .await?;
    if let Some(adaptation) = tool_adaptation {
        response_history_context.set_tool_registry(adaptation.registry);
    }
    apply_chat_fallback_stage(&mut body, stage);
    Ok((body, response_history_context))
}

fn infer_client_family(user_agent: Option<&str>, endpoint: EndpointKind) -> &'static str {
    let ua = user_agent.unwrap_or_default().trim().to_ascii_lowercase();
    if ua.starts_with("codex") {
        "codex"
    } else if ua.starts_with("opencode") {
        "opencode"
    } else if ua.starts_with("hermes") {
        "hermes"
    } else {
        match endpoint {
            EndpointKind::Responses => "responses_generic",
            EndpointKind::ChatCompletions => "chat_generic",
        }
    }
}

fn responses_body_contains_tool_replay_semantics(body: &Value) -> bool {
    if body.get("previous_response_id").is_some() {
        return true;
    }

    let Some(items) = body.get("input").and_then(Value::as_array) else {
        return false;
    };

    items.iter().any(|item| match item {
        Value::Object(object) => {
            matches!(
                object.get("type").and_then(Value::as_str),
                Some(
                    "function_call"
                        | "function_call_output"
                        | "custom_tool_call"
                        | "custom_tool_call_output",
                )
            ) || object.contains_key("tool_call_id")
                || object.contains_key("tool_calls")
                || matches!(
                    object.get("role").and_then(Value::as_str),
                    Some("tool" | "function")
                )
        }
        _ => false,
    })
}

fn initial_chat_fallback_stage(
    state: &AppState,
    downstream_id: &str,
    client_family: &str,
    model_slug: &str,
    upstream_id: &str,
    source_body: &Value,
) -> ChatFallbackStage {
    let should_skip_to_tool_replay_reduction =
        responses_body_contains_tool_replay_semantics(source_body)
            && state.fallback_stage_failure_count(
                downstream_id,
                client_family,
                model_slug,
                upstream_id,
                ChatFallbackStage::HighFidelity.as_str(),
            ) >= 3;

    let start_index = if should_skip_to_tool_replay_reduction {
        ChatFallbackStage::ORDERED
            .iter()
            .position(|stage| *stage == ChatFallbackStage::ToolReplayReduction)
            .unwrap_or(0)
    } else {
        0
    };

    ChatFallbackStage::ORDERED[start_index..]
        .iter()
        .copied()
        .into_iter()
        .find(|stage| {
            state.fallback_stage_failure_count(
                downstream_id,
                client_family,
                model_slug,
                upstream_id,
                stage.as_str(),
            ) < 3
        })
        .unwrap_or(ChatFallbackStage::HistoryCompaction)
}

fn should_advance_fallback_stage(status: StatusCode, error_text: &str) -> bool {
    let normalized = error_text.to_ascii_lowercase();
    status.is_client_error()
        && (normalized.contains("tool_config_missing")
            || normalized.contains("toolconfig")
            || normalized.contains("content_length_exceeds_threshold")
            || normalized.contains("content length exceeds threshold")
            || normalized.contains("input is too long")
            || normalized.contains("unsupported")
            || normalized.contains("invalid request")
            || normalized.contains("upstream rejected the request"))
}

fn maybe_record_chat_fallback_stage_failure(
    state: &AppState,
    downstream_id: &str,
    client_family: &str,
    model_slug: &str,
    upstream_id: &str,
    stage: Option<ChatFallbackStage>,
    error: &GatewayError,
) {
    let Some(stage) = stage else {
        return;
    };
    if should_advance_fallback_stage(error.status_code(), error.message()) {
        state.record_fallback_stage_failure(
            downstream_id,
            client_family,
            model_slug,
            upstream_id,
            stage.as_str(),
        );
    }
}

fn classify_stream_failure(error_message: &str) -> (StatusCode, &'static str) {
    let normalized = error_message.to_ascii_lowercase();
    if normalized.contains("max duration")
        || normalized.contains("maximum duration")
        || normalized.contains("stream duration")
        || normalized.contains("hard timeout")
    {
        (StatusCode::GATEWAY_TIMEOUT, "stream_max_duration")
    } else if normalized.contains("idle timeout")
        || normalized.contains("idle-timeout")
        || normalized.contains("waiting for sse")
        || (normalized.contains("timeout") && normalized.contains("sse"))
        || (normalized.contains("timed out") && normalized.contains("sse"))
    {
        (StatusCode::GATEWAY_TIMEOUT, "stream_idle_timeout")
    } else if normalized.contains("before any upstream output") {
        (
            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
            "stream_client_cancelled",
        )
    } else if normalized.contains("partial output received") {
        (
            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
            "stream_incomplete_close",
        )
    } else {
        (
            StatusCode::from_u16(499).expect("499 is a valid HTTP status code"),
            "stream_interrupted",
        )
    }
}

/// Describe the observed downstream body lifecycle without inferring human
/// intent from an Axum body drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamInterruption {
    DownstreamBodyDropped { usable_output_delivered: bool },
}

impl StreamInterruption {
    fn status_and_category(self) -> (StatusCode, &'static str) {
        let status = StatusCode::from_u16(499).expect("499 is a valid HTTP status code");
        match self {
            Self::DownstreamBodyDropped {
                usable_output_delivered: true,
            } => (status, "stream_incomplete_close"),
            Self::DownstreamBodyDropped {
                usable_output_delivered: false,
            } => (status, "stream_client_cancelled"),
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::DownstreamBodyDropped {
                usable_output_delivered: true,
            } => {
                "downstream response body dropped before semantic completion \
                 (partial output delivered)"
            }
            Self::DownstreamBodyDropped {
                usable_output_delivered: false,
            } => "downstream response body dropped before semantic completion",
        }
    }
}

fn classify_upstream_stream_error(
    error_message: &str,
    is_timeout: bool,
    is_decode: bool,
    // G2: when the split is enabled, the transport-layer decode failure gets
    // its own code so it can no longer be confused with an SSE parse failure.
    split_decode_code: bool,
) -> (StatusCode, &'static str) {
    let normalized = error_message.to_ascii_lowercase();
    if is_timeout || normalized.contains("timed out") || normalized.contains("timeout") {
        (StatusCode::GATEWAY_TIMEOUT, "stream_upstream_timeout")
    } else if is_decode || normalized.contains("error decoding response body") {
        (
            StatusCode::BAD_GATEWAY,
            if split_decode_code {
                "stream_upstream_transport_decode_error"
            } else {
                "stream_upstream_body_decode_error"
            },
        )
    } else {
        (StatusCode::BAD_GATEWAY, "stream_upstream_read_error")
    }
}

async fn finalize_stream_error(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    status: StatusCode,
    error_category: &'static str,
    error_message: String,
    attribute_route_failure: bool,
) {
    // A body decode failure that is also classified as a timeout (is_timeout
    // on the transport error) otherwise records the identical message as a
    // pure decode error; keep the two categories distinguishable in logs.
    let error_message = if error_category == "stream_upstream_timeout"
        && !error_message.to_ascii_lowercase().contains("timeout")
    {
        format!("upstream stream timed out while awaiting a response body: {error_message}")
    } else {
        error_message
    };
    let hedge_loser = completion_context
        .as_ref()
        .is_some_and(StreamCompletionContext::is_hedge_loser)
        || log_context
            .as_ref()
            .is_some_and(StreamUsageLogContext::is_hedge_loser);
    if let Some(context) = completion_context {
        context.release_all().await;
        if hedge_loser {
            context.mark_cancelled().await;
            return;
        }
        if attribute_route_failure {
            context.mark_failure().await;
        } else {
            context.mark_cancelled().await;
        }
    }

    if hedge_loser {
        return;
    }

    if let Some(mut log_context) = log_context {
        log_context.fail_active_request(error_category);
        if !log_context.transport_committed {
            log_context.wire_status = status;
        }
        log_context.status = status;
        log_context.error_message = Some(error_message);
        log_context.error_category = Some(error_category.to_string());
        let _ = log_context.emit(usage.unwrap_or((0, 0, 0))).await;
    }
}

async fn finalize_stream_interruption(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    interruption: StreamInterruption,
) {
    let (status, error_category) = interruption.status_and_category();
    finalize_stream_error(
        completion_context,
        log_context,
        usage,
        status,
        error_category,
        interruption.message().to_string(),
        false,
    )
    .await;
}

async fn finalize_stream_interruption_message(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    error_message: String,
) {
    let (status, error_category) = classify_stream_failure(&error_message);
    let attribute_route_failure = status != StatusCode::from_u16(499).expect("valid status code");
    finalize_stream_error(
        completion_context,
        log_context,
        usage,
        status,
        error_category,
        error_message,
        attribute_route_failure,
    )
    .await;
}

fn spawn_stream_interruption_cleanup(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    interruption: StreamInterruption,
) {
    if completion_context.is_none() && log_context.is_none() {
        return;
    }

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            finalize_stream_interruption(completion_context, log_context, usage, interruption)
                .await;
        });
    } else {
        tracing::warn!("stream cleanup dropped outside runtime; cleanup skipped");
    }
}

/// When a stream finished normally (received [DONE]) but the downstream client
/// disconnected before all pending frames were delivered, finalize as success
/// rather than recording a spurious "stream disconnected" error.
fn spawn_stream_terminal_cleanup(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
) {
    if completion_context.is_none() && log_context.is_none() {
        return;
    }

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let hedge_loser = completion_context
                .as_ref()
                .is_some_and(StreamCompletionContext::is_hedge_loser)
                || log_context
                    .as_ref()
                    .is_some_and(StreamUsageLogContext::is_hedge_loser);
            if hedge_loser {
                if let Some(context) = completion_context {
                    context.release_all().await;
                    context.mark_cancelled().await;
                }
                return;
            }
            if let Some(mut ctx) = log_context {
                ctx.finish_active_request();
                ctx.status = StatusCode::OK;
                ctx.error_message = None;
                ctx.error_category = None;
                let _ = ctx.emit(usage.unwrap_or((0, 0, 0))).await;
            }
            if let Some(context) = completion_context {
                context.release_all().await;
                context.mark_success().await;
            }
        });
    } else {
        tracing::warn!("stream cleanup dropped outside runtime; cleanup skipped");
    }
}

enum StreamReadOutcome {
    Chunk(Result<Option<Bytes>, reqwest::Error>),
    Heartbeat,
    IdleTimeout,
    MaxDurationExceeded,
}

struct StreamWatchdog {
    heartbeat_interval: Duration,
    idle_timeout: Duration,
    max_duration: Duration,
    started_at: TokioInstant,
    last_upstream_activity_at: TokioInstant,
    last_heartbeat_at: TokioInstant,
    /// How many heartbeats have been sent since the last real upstream data.
    /// Each heartbeat can extend the idle deadline by one heartbeat_interval,
    /// but once this count reaches `max_heartbeat_extensions`, no further
    /// extensions are granted. This prevents the original bug where heartbeats
    /// indefinitely reset the idle timeout, causing 499 errors on long streams.
    heartbeat_extensions_since_last_data: u32,
    /// Maximum heartbeat extensions allowed: ceil(idle_timeout / keepalive_interval) + 1.
    /// Heartbeats can bridge at most one idle_timeout period of upstream silence.
    max_heartbeat_extensions: u32,
}

struct UpstreamStreamReader {
    response: reqwest::Response,
    replay: VecDeque<Bytes>,
    watchdog: StreamWatchdog,
}

impl UpstreamStreamReader {
    fn new(response: reqwest::Response, timeouts: StreamTimeouts) -> Self {
        Self {
            response,
            replay: VecDeque::new(),
            watchdog: StreamWatchdog::new(timeouts),
        }
    }

    fn replay_later(&mut self, chunk: Bytes) {
        self.replay.push_back(chunk);
    }

    async fn next_chunk(&mut self) -> StreamReadOutcome {
        if let Some(chunk) = self.replay.pop_front() {
            return StreamReadOutcome::Chunk(Ok(Some(chunk)));
        }
        self.next_network_chunk().await
    }

    async fn next_network_chunk(&mut self) -> StreamReadOutcome {
        let outcome = wait_for_upstream_chunk(&mut self.response, &self.watchdog).await;
        match &outcome {
            StreamReadOutcome::Chunk(Ok(Some(_))) => {
                self.watchdog.record_upstream_activity(TokioInstant::now());
            }
            StreamReadOutcome::Heartbeat => {
                self.watchdog.record_heartbeat(TokioInstant::now());
            }
            _ => {}
        }
        outcome
    }

    fn debug_state(&self, now: TokioInstant) -> String {
        self.watchdog.debug_state(now)
    }
}

impl StreamWatchdog {
    fn new(timeouts: StreamTimeouts) -> Self {
        let now = TokioInstant::now();
        let max_heartbeat_extensions = (timeouts.idle_timeout.as_secs()
            / timeouts.keepalive_interval.as_secs().max(1))
        .saturating_add(1) as u32;
        Self {
            heartbeat_interval: timeouts.keepalive_interval,
            idle_timeout: timeouts.idle_timeout,
            max_duration: timeouts.max_duration,
            started_at: now,
            last_upstream_activity_at: now,
            last_heartbeat_at: now,
            heartbeat_extensions_since_last_data: 0,
            max_heartbeat_extensions,
        }
    }

    fn heartbeat_deadline(&self) -> TokioInstant {
        self.last_heartbeat_at + self.heartbeat_interval
    }

    fn idle_deadline(&self) -> TokioInstant {
        let base = self.last_upstream_activity_at + self.idle_timeout;
        if self.heartbeat_extensions_since_last_data == 0 {
            return base;
        }
        let extension = self.heartbeat_interval * self.heartbeat_extensions_since_last_data;
        base + extension
    }

    fn max_deadline(&self) -> TokioInstant {
        self.started_at + self.max_duration
    }

    fn record_upstream_activity(&mut self, at: TokioInstant) {
        self.last_upstream_activity_at = at;
        self.last_heartbeat_at = at;
        self.heartbeat_extensions_since_last_data = 0;
    }

    fn record_heartbeat(&mut self, at: TokioInstant) {
        // Heartbeats extend the idle deadline, but only up to
        // max_heartbeat_extensions times. Prevents indefinite idle reset.
        self.last_heartbeat_at = at;
        if self.heartbeat_extensions_since_last_data < self.max_heartbeat_extensions {
            self.heartbeat_extensions_since_last_data += 1;
        }
    }

    fn debug_state(&self, now: TokioInstant) -> String {
        let idle_elapsed = now.duration_since(self.last_upstream_activity_at).as_secs();
        let heartbeat_elapsed = now.duration_since(self.last_heartbeat_at).as_secs();
        let total_elapsed = now.duration_since(self.started_at).as_secs();
        format!(
            "total={}s idle_elapsed={}s/{}s heartbeat_elapsed={}s/{}s hb_ext={}/{}",
            total_elapsed,
            idle_elapsed,
            self.idle_timeout.as_secs(),
            heartbeat_elapsed,
            self.heartbeat_interval.as_secs(),
            self.heartbeat_extensions_since_last_data,
            self.max_heartbeat_extensions,
        )
    }
}

async fn wait_for_upstream_chunk(
    response: &mut reqwest::Response,
    watchdog: &StreamWatchdog,
) -> StreamReadOutcome {
    let idle_deadline = watchdog.idle_deadline();
    let max_deadline = watchdog.max_deadline();
    let next_deadline = std::cmp::min(
        watchdog.heartbeat_deadline(),
        std::cmp::min(idle_deadline, max_deadline),
    );

    tokio::select! {
        chunk = response.chunk() => StreamReadOutcome::Chunk(chunk),
        _ = tokio::time::sleep_until(next_deadline) => {
            let now = TokioInstant::now();
            if now >= max_deadline {
                StreamReadOutcome::MaxDurationExceeded
            } else if now >= idle_deadline {
                StreamReadOutcome::IdleTimeout
            } else {
                StreamReadOutcome::Heartbeat
            }
        }
    }
}

async fn process_gateway_request(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    endpoint: EndpointKind,
) -> Result<DispatchResult, GatewayError> {
    let runtime_settings = state.runtime_settings();
    // G0: Box the ~51.6KB nested awaitee so this wrapper future stays small.
    Box::pin(process_gateway_request_with_runtime_settings(
        state,
        headers,
        body,
        endpoint,
        runtime_settings,
    ))
    .await
}

async fn process_gateway_request_with_runtime_settings(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    endpoint: EndpointKind,
    runtime_settings: Arc<RuntimeSettings>,
) -> Result<DispatchResult, GatewayError> {
    // E4: generate the gateway request id up front so error exits (and the
    // success response header) carry the same id the usage log stores under.
    let request_id = Uuid::new_v4().to_string();
    // G0: Box the ~50KB inner future instead of inlining it (stack regression).
    Box::pin(process_gateway_request_inner(
        state,
        headers,
        body,
        endpoint,
        runtime_settings,
        false,
        None,
        Some(request_id.clone()),
        None,
    ))
    .await
    .map_err(|error| error.with_request_id(Some(request_id)))
}

#[allow(clippy::too_many_arguments)]
async fn process_gateway_request_with_pre_header_cancellation(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    endpoint: EndpointKind,
    runtime_settings: Arc<RuntimeSettings>,
    request_id: String,
    cancellation: PreHeaderStreamCancellation,
    first_semantic_deadline: stream_commit::FirstSemanticDeadline,
) -> Result<DispatchResult, GatewayError> {
    // G0: Box the ~50KB inner future instead of inlining it (stack regression).
    Box::pin(process_gateway_request_inner(
        state,
        headers,
        body,
        endpoint,
        runtime_settings,
        false,
        Some(cancellation),
        Some(request_id.clone()),
        Some(first_semantic_deadline),
    ))
    .await
    .map_err(|error| error.with_request_id(Some(request_id)))
}

#[allow(unused_assignments)]
#[allow(clippy::too_many_arguments)]
async fn process_gateway_request_inner(
    state: AppState,
    headers: HeaderMap,
    mut body: Value,
    endpoint: EndpointKind,
    runtime_settings: Arc<RuntimeSettings>,
    defer_success_usage_log: bool,
    pre_header_cancellation: Option<PreHeaderStreamCancellation>,
    request_id: Option<String>,
    first_semantic_deadline: Option<stream_commit::FirstSemanticDeadline>,
) -> Result<DispatchResult, GatewayError> {
    let secret = downstream_secret_from_headers(&headers)?;
    let downstream = state
        .downstream_for_secret(&secret)
        .await
        .ok_or_else(|| GatewayError::Unauthorized("invalid downstream key".into()))?;
    let routing_snapshot = state.routing_snapshot().await;
    let request_id = request_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let request_path = endpoint.path();
    let started = Instant::now();
    let upstream_retry_after_cap =
        Duration::from_secs(runtime_settings.upstream_retry_after_cap_seconds.max(1));
    // T1.2: separate, tighter cap for upstream Retry-After entering the
    // gateway's own route/key cooldown and the attempt ledger.  The upstream
    // hint tells *clients* when to retry; the local backoff owns route
    // removal, so a large hint must not starve the intra-gateway wait budget.
    let upstream_retry_after_cooldown_cap = Duration::from_secs(
        runtime_settings
            .upstream_retry_after_cooldown_cap_seconds
            .max(1),
    );
    let inference_strength = extract_inference_strength(&body);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let capture_route_metadata = troubleshooting_route_capture_requested(&state, &headers);
    let model_owned = match body.get("model").and_then(Value::as_str) {
        Some(model) => model.to_string(),
        None => {
            let error = GatewayError::BadRequest("missing model".into());
            let _ = append_gateway_usage_log(
                &state,
                &request_id,
                &downstream.id,
                &downstream.name,
                "",
                None,
                request_path,
                "",
                inference_strength.as_deref(),
                user_agent.as_deref(),
                None,
                error.status_code(),
                Some(error.to_string()),
                Some(error.error_category().to_string()),
                0,
                0,
                0,
                started,
            )
            .await;
            return Err(error);
        }
    };
    let model = model_owned.as_str();
    let normalized_model = {
        let alias_registry = state.model_alias_registry();
        if let Some(canonical) = alias_registry.resolve_alias(model) {
            canonical.to_string()
        } else {
            model.to_string()
        }
    };
    let case_insensitive = runtime_settings.model_case_insensitive_matching;
    let request_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let stream_only_recovery_request_safe =
        !request_stream && request_allows_stream_only_recovery(endpoint, &body);
    state.start_active_gateway_request(ActiveGatewayRequestStart {
        request_id: request_id.clone(),
        downstream_id: downstream.id.clone(),
        downstream_name: downstream.name.clone(),
        endpoint: request_path.to_string(),
        model: model.to_string(),
        protocol: format!("{:?}", endpoint.native_protocol()),
        user_agent: user_agent.clone(),
    });
    let mut active_request_guard =
        ActiveGatewayRequestGuard::new(state.clone(), request_id.clone());
    tracing::info!(
        request_id = %request_id,
        downstream_key_id = %downstream.id,
        path = %request_path,
        original_model = %model,
        normalized_model = %&normalized_model,
        stream = request_stream,
        "received downstream request"
    );

    if let Some(expires_at) = downstream.expires_at {
        if unix_seconds() > expires_at {
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %&normalized_model,
                expires_at,
                "downstream key expired"
            );
            let error =
                GatewayError::gateway_forbidden("downstream key expired", "gateway_key_expired");
            let _ = append_gateway_usage_log(
                &state,
                &request_id,
                &downstream.id,
                &downstream.name,
                "",
                None,
                request_path,
                model,
                inference_strength.as_deref(),
                user_agent.as_deref(),
                None,
                error.status_code(),
                Some(error.to_string()),
                Some(error.error_category().to_string()),
                0,
                0,
                0,
                started,
            )
            .await;
            active_request_guard.fail_and_finish(error.error_category());
            return Err(error);
        }
    }

    if let Some(client_ip) = client_ip_from_headers(&headers) {
        if !downstream.ip_allowlist.is_empty()
            && !downstream
                .ip_allowlist
                .iter()
                .any(|allowed| allowed == &client_ip)
        {
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %&normalized_model,
                client_ip = %client_ip,
                "client IP not allowed"
            );
            let error = GatewayError::gateway_forbidden("ip not allowed", "gateway_ip_not_allowed");
            let _ = append_gateway_usage_log(
                &state,
                &request_id,
                &downstream.id,
                &downstream.name,
                "",
                None,
                request_path,
                model,
                inference_strength.as_deref(),
                user_agent.as_deref(),
                None,
                error.status_code(),
                Some(error.to_string()),
                Some(error.error_category().to_string()),
                0,
                0,
                0,
                started,
            )
            .await;
            active_request_guard.fail_and_finish(error.error_category());
            return Err(error);
        }
    }

    if !portal_model_is_allowed(downstream.model_allowlist.as_slice(), model) {
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %&normalized_model,
            "model not allowed"
        );
        let error =
            GatewayError::gateway_forbidden("model not allowed", "gateway_model_not_allowed");
        let _ = append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            "",
            None,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            None,
            error.status_code(),
            Some(error.to_string()),
            Some(error.error_category().to_string()),
            0,
            0,
            0,
            started,
        )
        .await;
        active_request_guard.fail_and_finish(error.error_category());
        return Err(error);
    }

    if request_has_unknown_tool_kind(endpoint, &body) {
        let error = GatewayError::classified(
            StatusCode::BAD_REQUEST,
            "request contains an unsupported tool type",
            "invalid_request_error",
            "gateway_protocol_capability_unsupported",
            "gateway_protocol_capability_unsupported",
            None,
            Some(json!({ "scope": "gateway" })),
        );
        let _ = append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            "",
            None,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            None,
            error.status_code(),
            Some(error.to_string()),
            Some(error.error_category().to_string()),
            0,
            0,
            0,
            started,
        )
        .await;
        active_request_guard.fail_and_finish(error.error_category());
        return Err(error);
    }

    let (downstream_request_reservation, downstream_concurrency_lease) = match state
        .reserve_downstream_admission(&downstream, &normalized_model)
        .await
    {
        Ok(admission) => admission,
        Err(rejection) => {
            let retry_after_seconds = rejection.retry_after_seconds();
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %&normalized_model,
                retry_after_seconds,
                max_concurrency = downstream.max_concurrency,
                rejection = ?rejection,
                "downstream admission rejected (request quota or concurrency)"
            );
            if let crate::state::DownstreamAdmissionRejection::DailyCostQuotaExceeded {
                retry_after_seconds: lockout_seconds,
                limit,
                used,
            } = &rejection
            {
                // A daily cost lockout usually emits exactly one WARN (the
                // client stops retrying), then stays silent for the rest
                // of the window.  Escalate long lockouts so an exhausted
                // downstream cannot drain its budget unnoticed.
                if *lockout_seconds >= 3600 {
                    tracing::error!(
                        request_id = %request_id,
                        downstream_key_id = %downstream.id,
                        path = %request_path,
                        original_model = %model,
                        normalized_model = %&normalized_model,
                        daily_cost_limit_cents = limit,
                        daily_cost_used_cents = used,
                        lockout_seconds,
                        "downstream daily cost quota exhausted; downstream is locked until the quota window resets"
                    );
                }
            }
            let error = GatewayError::downstream_admission_rejection(rejection);
            let _ = append_gateway_usage_log(
                &state,
                &request_id,
                &downstream.id,
                &downstream.name,
                "",
                None,
                request_path,
                model,
                inference_strength.as_deref(),
                user_agent.as_deref(),
                None,
                error.status_code(),
                Some(error.to_string()),
                Some(error.error_category().to_string()),
                0,
                0,
                0,
                started,
            )
            .await;
            active_request_guard.fail_and_finish(error.error_category());
            return Err(error);
        }
    };
    let account_recovery_deadline = TokioInstant::now()
        + Duration::from_millis(runtime_settings.upstream_concurrency_recovery_max_wait_ms);
    let mut account_recovery = AccountRecoverySession::new(
        state.clone(),
        request_id.clone(),
        downstream_concurrency_lease.clone(),
        account_recovery_deadline,
        runtime_settings.upstream_concurrency_recovery_max_rounds,
    );
    let downstream_concurrency_guard =
        DownstreamConcurrencyGuard::new(state.clone(), downstream_concurrency_lease);

    let original_responses_body = (endpoint == EndpointKind::Responses).then(|| body.clone());
    let mut response_history_context = if endpoint == EndpointKind::Responses {
        match prepare_response_history_context(&state, &downstream.id, &mut body).await {
            Ok(context) => Some(context),
            Err(error) => {
                let _ = append_gateway_usage_log(
                    &state,
                    &request_id,
                    &downstream.id,
                    &downstream.name,
                    "",
                    None,
                    request_path,
                    model,
                    inference_strength.as_deref(),
                    user_agent.as_deref(),
                    None,
                    error.status_code(),
                    Some(error.to_string()),
                    Some(error.error_category().to_string()),
                    0,
                    0,
                    0,
                    started,
                )
                .await;
                return Err(error);
            }
        }
    } else {
        None
    };

    if endpoint == EndpointKind::Responses {
        if let Some(context) = response_history_context.as_mut() {
            if let Some(tools) = original_responses_body
                .as_ref()
                .and_then(|request| request.get("tools"))
                .and_then(Value::as_array)
            {
                if let Ok(adaptation) = ToolAdapterRegistry::build(
                    &Value::Array(tools.clone()),
                    ToolTarget::FunctionsOnly,
                ) {
                    context.set_tool_registry(adaptation.registry);
                }
            }
        }
    }

    let requires_responses_tooling =
        endpoint == EndpointKind::Responses && responses_request_requires_responses_upstream(&body);
    let client_family = infer_client_family(user_agent.as_deref(), endpoint);

    let loaded_exact_continuation = response_history_context
        .as_ref()
        .map(ResponseHistoryContext::exact_continuation_state)
        .transpose()?
        .flatten();
    let loaded_continuation_state = loaded_exact_continuation
        .as_ref()
        .map(LoadedContinuation::state);
    let has_loaded_continuation = loaded_continuation_state.is_some();
    if loaded_continuation_state.is_some_and(|continuation| {
        !continuation.has_protocol_transition(
            WireProtocol::from(endpoint.native_protocol()),
            continuation.profile_key().protocol,
        )
    }) {
        return Err(response_history_invalid(
            "cached gateway continuation adapter identity is incompatible",
        ));
    }
    if loaded_continuation_state.is_some_and(|continuation| {
        !response_history_context
            .as_ref()
            .is_some_and(|context| context.has_trusted_tool_registry_version(continuation))
    }) {
        return Err(response_history_invalid(
            "cached gateway continuation tool registry is missing or incompatible",
        ));
    }
    let legacy_continuation_upstream_id = response_history_context
        .as_ref()
        .map(ResponseHistoryContext::legacy_continuation_upstream_id)
        .transpose()?
        .flatten()
        .map(str::to_owned);
    let mut requested_features = requested_features_for_request(endpoint, &body);
    if legacy_continuation_upstream_id.is_some() {
        requested_features.allow_reasoning_history_downgrade = false;
    }
    if let Some(continuation) = loaded_continuation_state {
        let downgraded = continuation.apply_to_requested(&mut requested_features);
        if !downgraded.is_empty() {
            tracing::debug!(
                request_id = %request_id,
                downgraded = ?downgraded,
                "downgraded stale stored continuation required capabilities (V2)"
            );
        }
    }
    let required_capabilities = requested_features.required.clone();
    let capability_snapshot = state.capability_snapshot();
    if loaded_continuation_state.is_some_and(|continuation| {
        !continuation.has_current_configuration_fingerprint(
            &capability_snapshot,
            &routing_snapshot.upstreams,
            model,
        )
    }) {
        return Err(response_history_invalid(
            "cached gateway continuation route configuration has changed",
        ));
    }
    if loaded_continuation_state
        .is_some_and(|continuation| !continuation.has_current_probe_schema(&capability_snapshot))
    {
        return Err(response_history_invalid(
            "cached gateway continuation probe schema has changed",
        ));
    }
    let runtime_capability_hints = state.runtime_capability_hints_snapshot();
    let route_capability_cache = build_request_route_capability_cache_with_hints(
        &capability_snapshot,
        &routing_snapshot.upstreams,
        &normalized_model,
        endpoint,
        &requested_features,
        &runtime_capability_hints,
        inference_strength.as_deref(),
        case_insensitive,
    );
    let eligible_responses_routes = route_capability_cache
        .iter()
        .filter(|((protocol, _, _), route)| *protocol == WireProtocol::Responses && route.eligible)
        .count();
    let eligible_chat_routes = route_capability_cache
        .iter()
        .filter(|((protocol, _, _), route)| {
            *protocol == WireProtocol::ChatCompletions && route.eligible
        })
        .count();
    let responses_strategy = responses_route_strategy(
        requires_responses_tooling,
        eligible_responses_routes,
        eligible_chat_routes,
    );
    let fallback_to_chat = matches!(responses_strategy, ResponsesRouteStrategy::ChatFallback);
    let chat_only_responses_fallback = endpoint == EndpointKind::Responses
        && eligible_responses_routes == 0
        && eligible_chat_routes > 0;
    if requires_responses_tooling {
        let routing_reason = match responses_strategy {
            ResponsesRouteStrategy::Responses => "eligible_responses_route_available",
            ResponsesRouteStrategy::ChatFallback => "responses_routes_ineligible_fallback_to_chat",
            ResponsesRouteStrategy::Unavailable => "no_eligible_responses_or_chat_route",
            ResponsesRouteStrategy::ProtocolAgnostic => unreachable!(),
        };
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %&normalized_model,
            stream = request_stream,
            routing_fallback = fallback_to_chat,
            routing_fallback_reason = routing_reason,
            eligible_responses_routes,
            eligible_chat_routes,
            "evaluated Responses routing strategy"
        );
    }
    let route_capability =
        |upstream: &UpstreamConfig, key_fingerprint: &str, protocol: UpstreamProtocol| {
            route_capability_cache.get(&(
                WireProtocol::from(protocol),
                upstream.id.clone(),
                key_fingerprint.to_string(),
            ))
        };
    let exact_continuation = match loaded_exact_continuation {
        Some(LoadedContinuation::V2(continuation)) => Some(continuation),
        Some(LoadedContinuation::V1NeedsDerivation(continuation)) => {
            let mut derived_contracts = Vec::new();
            for upstream in routing_snapshot.upstreams.iter().filter(|upstream| {
                upstream.active
                    && upstream.id == continuation.profile_key().upstream_id
                    && upstream.supports_model_with(&normalized_model, case_insensitive)
            }) {
                let Some(runtime_model_slug) =
                    upstream.resolved_model_name_with(&normalized_model, case_insensitive)
                else {
                    continue;
                };
                for api_key in route_api_keys(upstream, &runtime_model_slug, case_insensitive) {
                    let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                    for protocol in upstream.supported_protocols() {
                        if !continuation.matches_route(upstream, &key_fingerprint, model, protocol)
                        {
                            continue;
                        }
                        let Some(evaluation) =
                            route_capability(upstream, &key_fingerprint, protocol)
                        else {
                            continue;
                        };
                        let (Some(resolved), Some(profile)) = (
                            evaluation.resolved.as_ref(),
                            capability_snapshot.profiles.get(continuation.profile_key()),
                        ) else {
                            continue;
                        };
                        let Ok(configuration_fingerprint) =
                            AppState::route_configuration_fingerprint_with_snapshot(
                                &capability_snapshot,
                                upstream,
                                &key_fingerprint,
                                model,
                                &runtime_model_slug,
                                protocol,
                            )
                        else {
                            continue;
                        };
                        if configuration_fingerprint != continuation.configuration_fingerprint()
                            || profile.configuration_fingerprint
                                != continuation.configuration_fingerprint()
                            || profile.key != *continuation.profile_key()
                        {
                            continue;
                        }
                        let mut stored_required = continuation.required_capabilities().clone();
                        sanitize_stored_required(&mut stored_required);
                        if let Some(contract) = continuation_contract_for_route(
                            upstream,
                            &runtime_model_slug,
                            WireProtocol::from(endpoint.native_protocol()),
                            WireProtocol::from(protocol),
                            &stored_required,
                            resolved,
                            profile,
                            continuation.tool_registry_version(),
                        ) {
                            derived_contracts.push(contract);
                        }
                    }
                }
            }
            if derived_contracts.len() != 1 {
                return Err(response_history_invalid(
                    "cached legacy continuation does not identify exactly one current contract",
                ));
            }
            Some(
                continuation.with_contract(
                    derived_contracts
                        .pop()
                        .expect("one derived continuation contract"),
                ),
            )
        }
        None => None,
    };
    if let (Some(context), Some(continuation)) = (
        response_history_context.as_mut(),
        exact_continuation.as_ref(),
    ) {
        *context = context.with_selected_route(continuation.clone(), None)?;
    }
    let legacy_continuation_profile =
        if let Some(upstream_id) = legacy_continuation_upstream_id.as_deref() {
            let mut eligible_profiles = Vec::new();
            for upstream in routing_snapshot.upstreams.iter().filter(|upstream| {
                upstream.active
                    && upstream.id == upstream_id
                    && upstream.supports_model_with(&normalized_model, case_insensitive)
            }) {
                let Some(runtime_model_slug) =
                    upstream.resolved_model_name_with(&normalized_model, case_insensitive)
                else {
                    continue;
                };
                for api_key in route_api_keys(upstream, &runtime_model_slug, case_insensitive) {
                    let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                    for protocol in upstream.supported_protocols() {
                        if route_capability(upstream, &key_fingerprint, protocol)
                            .is_some_and(|route| route.eligible)
                        {
                            eligible_profiles.push(DialectProfileKey::for_key(
                                upstream.id.clone(),
                                key_fingerprint.clone(),
                                runtime_model_slug.clone(),
                                WireProtocol::from(protocol),
                            ));
                        }
                    }
                }
            }
            if eligible_profiles.len() != 1 {
                return Err(response_history_invalid(
                "cached legacy gateway continuation does not identify exactly one eligible profile",
            ));
            }
            eligible_profiles.into_iter().next()
        } else {
            None
        };
    let continuation_profile_key = exact_continuation
        .as_ref()
        .map(|continuation| continuation.preferred_profile().clone())
        .or(legacy_continuation_profile);
    let route_profile_constraint_active = continuation_profile_key.is_some();
    // P2 continuation-pin escape: once armed, the profile constraint is
    // relaxed for every remaining routing round of this request and the
    // candidate protocol lock is rebuilt, so a continuation whose pinned
    // route is down can reach other providers.  Each escape fires at most
    // once per request.
    let continuation_constraint_relaxed = AtomicBool::new(false);
    let continuation_pin_escaped = AtomicBool::new(false);
    let route_matches_profile_constraint =
        |upstream: &UpstreamConfig, key_fingerprint: &str, protocol: UpstreamProtocol| {
            if continuation_constraint_relaxed.load(Ordering::Relaxed) {
                // P2 escape pass: the continuation pin is suspended for the
                // rest of this request; every otherwise-eligible route is a
                // candidate (capability / health constraints still apply).
                return true;
            }
            let Some(runtime_model_slug) =
                upstream.resolved_model_name_with(&normalized_model, case_insensitive)
            else {
                return false;
            };
            if let Some(continuation) = exact_continuation.as_ref() {
                let Some(contract) = continuation.contract() else {
                    return false;
                };
                let Some(evaluation) = route_capability(upstream, key_fingerprint, protocol) else {
                    return false;
                };
                let Some(resolved) = evaluation.resolved.as_ref() else {
                    return false;
                };
                let candidate_key = DialectProfileKey::for_key(
                    upstream.id.clone(),
                    key_fingerprint,
                    runtime_model_slug.clone(),
                    WireProtocol::from(protocol),
                );
                let Some(profile) = capability_snapshot.profiles.get(&candidate_key) else {
                    return false;
                };
                let Ok(configuration_fingerprint) =
                    AppState::route_configuration_fingerprint_with_snapshot(
                        &capability_snapshot,
                        upstream,
                        key_fingerprint,
                        model,
                        &runtime_model_slug,
                        protocol,
                    )
                else {
                    return false;
                };
                if profile.configuration_fingerprint != configuration_fingerprint {
                    return false;
                }
                // Both sides of the equality are sanitized: a stored
                // contract created when `DOWNGRADEABLE_STORED_CAPABILITIES`
                // were required still matches a route that now only supports
                // the downgraded set, while routes that support the full set
                // keep matching exactly as before.
                let mut derived_required = continuation.required_capabilities().clone();
                let downgraded = sanitize_stored_required(&mut derived_required);
                let mut stored_contract = contract.clone();
                sanitize_stored_required(&mut stored_contract.required_capabilities);
                if !downgraded.is_empty() {
                    tracing::debug!(
                        request_id = %request_id,
                        downgraded = ?downgraded,
                        upstream_id = %upstream.id,
                        "downgraded stale stored continuation required capabilities"
                    );
                }
                return continuation_contract_for_route(
                    upstream,
                    &runtime_model_slug,
                    WireProtocol::from(endpoint.native_protocol()),
                    WireProtocol::from(protocol),
                    &derived_required,
                    resolved,
                    profile,
                    continuation.tool_registry_version(),
                )
                .as_ref()
                .is_some_and(|derived| {
                    let mut derived_contract = derived.clone();
                    sanitize_stored_required(&mut derived_contract.required_capabilities);
                    derived_contract == stored_contract
                });
            }

            let Some(profile_key) = continuation_profile_key.as_ref() else {
                return true;
            };
            let candidate_key = DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint,
                runtime_model_slug,
                WireProtocol::from(protocol),
            );
            candidate_key == *profile_key
        };
    let claude_replay_route = claude_thinking_replay_route(
        &state,
        &capability_snapshot,
        &routing_snapshot.upstreams,
        model,
        &body,
    );
    if claude_replay_route == ClaudeThinkingReplayRoute::InvalidOrUnavailable {
        let mut error =
            GatewayError::BadRequest("invalid or unavailable Claude thinking replay route".into());
        if should_rollback_downstream_reservation(&error) {
            let rollback = state
                .rollback_downstream_request_reservation(downstream_request_reservation.clone())
                .await;
            error = replace_error_on_runtime_rollback_failure(error, rollback);
        }
        let _ = append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            "",
            None,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            None,
            error.status_code(),
            Some(error.to_string()),
            Some(error.error_category().to_string()),
            0,
            0,
            0,
            started,
        )
        .await;
        downstream_concurrency_guard.release().await;
        active_request_guard.fail_and_finish(error.error_category());
        return Err(error);
    }
    let required_route_available = if route_profile_constraint_active {
        routing_snapshot.upstreams.iter().any(|upstream| {
            if !upstream.active
                || !upstream.supports_model_with(&normalized_model, case_insensitive)
            {
                return false;
            }
            let Some(runtime_model_slug) =
                upstream.resolved_model_name_with(&normalized_model, case_insensitive)
            else {
                return false;
            };
            route_api_keys(upstream, &runtime_model_slug, case_insensitive)
                .into_iter()
                .any(|api_key| {
                    let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                    upstream.supported_protocols().into_iter().any(|protocol| {
                        route_matches_profile_constraint(upstream, &key_fingerprint, protocol)
                            && route_capability(upstream, &key_fingerprint, protocol)
                                .is_some_and(|route| route.eligible)
                    })
                })
        })
    } else {
        match &claude_replay_route {
            ClaudeThinkingReplayRoute::Pinned {
                upstream_id,
                key_fingerprint,
                protocol,
            } => routing_snapshot.upstreams.iter().any(|upstream| {
                upstream.active
                    && upstream.id == *upstream_id
                    && upstream.supports_model_with(&normalized_model, case_insensitive)
                    && upstream.supports_protocol(*protocol)
                    && route_capability(upstream, key_fingerprint, *protocol)
                        .is_some_and(|route| route.eligible)
            }),
            ClaudeThinkingReplayRoute::NoReplay => {
                let has_configured_route = routing_snapshot.upstreams.iter().any(|upstream| {
                    upstream.active
                        && upstream.supports_model_with(&normalized_model, case_insensitive)
                });
                !has_configured_route
                    || routing_snapshot.upstreams.iter().any(|upstream| {
                        if !upstream.active
                            || !upstream.supports_model_with(&normalized_model, case_insensitive)
                        {
                            return false;
                        }
                        let Some(runtime_model_slug) =
                            upstream.resolved_model_name_with(&normalized_model, case_insensitive)
                        else {
                            return false;
                        };
                        route_api_keys(upstream, &runtime_model_slug, case_insensitive)
                            .into_iter()
                            .any(|api_key| {
                                let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                                upstream.supported_protocols().into_iter().any(|protocol| {
                                    route_capability(upstream, &key_fingerprint, protocol)
                                        .is_some_and(|route| route.eligible)
                                })
                            })
                    })
            }
            ClaudeThinkingReplayRoute::InvalidOrUnavailable => unreachable!(),
        }
    };
    if !required_route_available {
        let constrained_failure = continuation_profile_key
            .as_ref()
            .and_then(|profile| {
                route_capability_cache.get(&(
                    profile.protocol,
                    profile.upstream_id.clone(),
                    profile.key_fingerprint.clone(),
                ))
            })
            .and_then(|route| route.failed_capability)
            .or_else(|| match &claude_replay_route {
                ClaudeThinkingReplayRoute::Pinned {
                    upstream_id,
                    key_fingerprint,
                    protocol,
                } => route_capability_cache
                    .get(&(
                        WireProtocol::from(*protocol),
                        upstream_id.clone(),
                        key_fingerprint.clone(),
                    ))
                    .and_then(|route| route.failed_capability),
                ClaudeThinkingReplayRoute::NoReplay
                | ClaudeThinkingReplayRoute::InvalidOrUnavailable => None,
            });
        let capability_name = capability_name_for_failure(
            constrained_failure,
            route_profile_constraint_active,
            &claude_replay_route,
            &route_capability_cache,
            &required_capabilities,
        );
        // A continuation whose pinned routes are temporarily unavailable
        // (cooling / half-open) must not be killed with a terminal 400: the
        // client retries after the advertised wait and keeps the session.
        let temporary_recovery = if has_loaded_continuation {
            // The continuation pins an exact route identity; query the health
            // of that route (and any key sharing the pinned fingerprint)
            // directly so a cooling/half-open route counts as temporary even
            // when the capability contract can no longer be re-derived.
            let mut candidate_route_health = Vec::new();
            if let Some(profile_key) = continuation_profile_key.as_ref() {
                for upstream in routing_snapshot.upstreams.iter().filter(|upstream| {
                    upstream.active
                        && upstream.supports_model_with(&normalized_model, case_insensitive)
                }) {
                    let Some(runtime_model_slug) =
                        upstream.resolved_model_name_with(&normalized_model, case_insensitive)
                    else {
                        continue;
                    };
                    if runtime_model_slug != profile_key.runtime_model_slug {
                        continue;
                    }
                    for api_key in route_api_keys(upstream, &runtime_model_slug, case_insensitive) {
                        let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                        if key_fingerprint != profile_key.key_fingerprint {
                            continue;
                        }
                        for protocol in upstream.supported_protocols() {
                            if WireProtocol::from(protocol) != profile_key.protocol {
                                continue;
                            }
                            let (route_health_key, _) = route_health_keys(
                                upstream,
                                &key_fingerprint,
                                &runtime_model_slug,
                                protocol,
                            );
                            candidate_route_health.push(route_health_key);
                        }
                    }
                }
            }
            if candidate_route_health.is_empty() {
                None
            } else {
                state
                    .earliest_temporary_route_recovery(&candidate_route_health)
                    .await
                    .ok()
                    .flatten()
            }
        } else {
            None
        };
        let error = if let Some(recovery) = temporary_recovery {
            let retry_after_seconds =
                duration_seconds_ceil(recovery.half_open_remaining.unwrap_or(recovery.retry_after));
            GatewayError::classified(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    concat!(
                        "selected routes cannot preserve required capability {}; ",
                        "routes are temporarily unavailable; please try again in {}s"
                    ),
                    capability_name, retry_after_seconds
                ),
                "upstream_error",
                "upstream_routes_temporarily_unavailable",
                "upstream_routes_temporarily_unavailable",
                Some(retry_after_seconds),
                Some(json!({
                    "scope": "gateway",
                    "retry_after_seconds": retry_after_seconds,
                })),
            )
        } else {
            GatewayError::classified(
                StatusCode::BAD_REQUEST,
                format!("selected routes cannot preserve required capability {capability_name}"),
                "invalid_request_error",
                "gateway_protocol_capability_unsupported",
                "gateway_protocol_capability_unsupported",
                None,
                Some(json!({ "scope": "gateway" })),
            )
        };
        let _ = append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            "",
            None,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            None,
            error.status_code(),
            Some(error.to_string()),
            Some(error.error_category().to_string()),
            0,
            0,
            0,
            started,
        )
        .await;
        active_request_guard.fail_and_finish(error.error_category());
        return Err(error);
    }
    let mut last_failure_upstream: Option<(String, Option<String>)> = None;
    let mut candidate_protocols = if let Some(profile_key) = continuation_profile_key.as_ref() {
        match profile_key.protocol {
            WireProtocol::ChatCompletions => vec![UpstreamProtocol::ChatCompletions],
            WireProtocol::Responses => vec![UpstreamProtocol::Responses],
            WireProtocol::Messages => Vec::new(),
        }
    } else {
        match &claude_replay_route {
            ClaudeThinkingReplayRoute::Pinned { protocol, .. } => vec![*protocol],
            ClaudeThinkingReplayRoute::NoReplay => match responses_strategy {
                ResponsesRouteStrategy::ProtocolAgnostic => {
                    vec![endpoint.native_protocol(), endpoint.opposite()]
                }
                ResponsesRouteStrategy::Responses => vec![UpstreamProtocol::Responses],
                ResponsesRouteStrategy::ChatFallback => {
                    vec![UpstreamProtocol::ChatCompletions]
                }
                ResponsesRouteStrategy::Unavailable => Vec::new(),
            },
            ClaudeThinkingReplayRoute::InvalidOrUnavailable => unreachable!(),
        }
    };
    let route_is_candidate =
        |upstream: &UpstreamConfig, key_fingerprint: &str, protocol: UpstreamProtocol| {
            upstream.active
                && upstream.supports_protocol(protocol)
                && upstream.supports_model_with(&normalized_model, case_insensitive)
                && route_matches_profile_constraint(upstream, key_fingerprint, protocol)
                && (matches!(&claude_replay_route, ClaudeThinkingReplayRoute::NoReplay)
                    || matches!(
                        &claude_replay_route,
                        ClaudeThinkingReplayRoute::Pinned {
                            upstream_id,
                            key_fingerprint: replay_key_fingerprint,
                            protocol: replay_protocol,
                        } if upstream.id == *upstream_id
                            && key_fingerprint == replay_key_fingerprint
                            && protocol == *replay_protocol
                    ))
                && route_capability(upstream, key_fingerprint, protocol)
                    .is_some_and(|route| route.eligible)
        };
    let upstream_has_candidate_route = |upstream: &UpstreamConfig, protocol: UpstreamProtocol| {
        let Some(runtime_model_slug) =
            upstream.resolved_model_name_with(&normalized_model, case_insensitive)
        else {
            return false;
        };
        route_api_keys(upstream, &runtime_model_slug, case_insensitive)
            .into_iter()
            .any(|api_key| {
                let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                route_is_candidate(upstream, &key_fingerprint, protocol)
            })
    };
    let compute_candidate_passes = |protocols: &[UpstreamProtocol]| {
        if requested_features.optional.is_empty() {
            protocols
                .iter()
                .copied()
                .map(|protocol| (None, protocol))
                .collect::<Vec<_>>()
        } else {
            let mut miss_tiers = std::collections::BTreeSet::new();
            for protocol in protocols.iter().copied() {
                for upstream in routing_snapshot.upstreams.iter() {
                    let Some(runtime_model_slug) =
                        upstream.resolved_model_name_with(&normalized_model, case_insensitive)
                    else {
                        continue;
                    };
                    for api_key in route_api_keys(upstream, &runtime_model_slug, case_insensitive) {
                        let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                        if route_is_candidate(upstream, &key_fingerprint, protocol) {
                            if let Some(route) =
                                route_capability(upstream, &key_fingerprint, protocol)
                            {
                                miss_tiers.insert(route.optional_misses);
                            }
                        }
                    }
                }
            }
            miss_tiers
                .into_iter()
                .flat_map(|misses| {
                    protocols
                        .iter()
                        .copied()
                        .map(move |protocol| (Some(misses), protocol))
                })
                .collect::<Vec<_>>()
        }
    };
    let mut candidate_passes = compute_candidate_passes(&candidate_protocols);
    // P3: the contract-filtered *pass* count at request start.  A pass is a
    // (optional-capability-miss tier × protocol) channel, NOT a route: many
    // routes can share one pass.  The escape round re-assigns
    // candidate_passes to the relaxed full pool, so the terminal details
    // must read this immutable snapshot, not candidate_passes.
    // (T0.3: renamed from `continuation_candidate_count`, which conflated
    // pass channels with real routes.)
    let candidate_pass_count = candidate_passes.len();
    // T0.3: the number of *real* contract-filtered routes at request start
    // (upstream × key × protocol tuples passing `route_is_candidate`).  This
    // is what "how many candidates do I have" actually means; the pass count
    // above can be much smaller than the route count when many keys of one
    // aggregated gateway share the same pass channel.
    let continuation_route_count = routing_snapshot
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
        .map(|upstream| {
            let Some(runtime_model_slug) =
                upstream.resolved_model_name_with(&normalized_model, case_insensitive)
            else {
                return 0usize;
            };
            candidate_protocols
                .iter()
                .copied()
                .map(|protocol| {
                    route_api_keys(upstream, &runtime_model_slug, case_insensitive)
                        .into_iter()
                        .filter(|api_key| {
                            let key_fingerprint = route_key_fingerprint(upstream, api_key);
                            route_is_candidate(upstream, &key_fingerprint, protocol)
                        })
                        .count()
                })
                .sum::<usize>()
        })
        .sum::<usize>();
    // T1.4: count, per upstream host, how many candidate routes (upstream ×
    // key × protocol tuples passing `route_is_candidate`) resolve to it.  A
    // host with >= 2 candidates is a shared failure domain (single aggregated
    // gateway): transient-family failures on its routes cool on the
    // edge-proxy curve and never escalate the step.  `Credentials`/key-quota
    // classes deliberately ignore this map (per-key cooldown stays per-key).
    let mut shared_host_candidate_counts = std::collections::HashMap::<String, usize>::new();
    for upstream in routing_snapshot
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
    {
        let Some(runtime_model_slug) =
            upstream.resolved_model_name_with(&normalized_model, case_insensitive)
        else {
            continue;
        };
        for protocol in candidate_protocols.iter().copied() {
            for api_key in route_api_keys(upstream, &runtime_model_slug, case_insensitive) {
                let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                if route_is_candidate(upstream, &key_fingerprint, protocol) {
                    if let Some(host) = upstream_host(&upstream.base_url) {
                        *shared_host_candidate_counts.entry(host).or_default() += 1;
                    }
                }
            }
        }
    }
    // P4/R3: when a continuation pin narrows the contract-filtered pool to a
    // single candidate, cross-request failures on that sole route must not
    // escalate its cooldown step (the route would pin itself at max and the
    // session stays stuck). These failures are marked sole_candidate so the
    // health layer applies the repeat_within_request semantics.  The test is
    // on the real route count, not the pass count (T0.3).
    let sole_contract_candidate =
        continuation_profile_key.is_some() && continuation_route_count == 1;
    // E2: number of *real* candidate routes (upstream × key tuples passing
    // `route_is_candidate`) for a protocol — the request's true pool for that
    // (runtime_model_slug, protocol).  When it is 1, a capacity-class failure
    // (upstream 429 family) must never cool that only reachable route, even
    // with the E1 switch turned on: cooling it buys nothing in failover and
    // turns a 1s-granularity client retry loop into a global circuit break
    // (glm5.2 is exactly this shape).  Contrast `sole_contract_candidate`,
    // which is about the continuation pin; this is about the real pool.  The
    // flag is plumbed as `capacity_sole_route` and touches ONLY the
    // capacity-class exemption — the health-class step/cooldown curve stays
    // byte-for-byte identical.
    let candidate_route_count_for = |protocol: UpstreamProtocol| -> usize {
        routing_snapshot
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .filter_map(|upstream| {
                let runtime_model_slug =
                    upstream.resolved_model_name_with(&normalized_model, case_insensitive)?;
                Some(
                    route_api_keys(upstream, &runtime_model_slug, case_insensitive)
                        .into_iter()
                        .filter(|api_key| {
                            let key_fingerprint = route_key_fingerprint(upstream, api_key);
                            route_is_candidate(upstream, &key_fingerprint, protocol)
                        })
                        .count(),
                )
            })
            .sum()
    };
    // E2: a failure on a route is "capacity-sole" when the continuation pin
    // singles it out OR it is the only real candidate of its protocol.
    let capacity_sole_route_for = |protocol: UpstreamProtocol| -> bool {
        sole_contract_candidate || candidate_route_count_for(protocol) == 1
    };
    let route_retry_policy =
        RouteRetryPolicy::from_sources(&state.config, runtime_settings.as_ref());
    let mut route_retry_budget = RouteRetryBudget::default();
    let mut request_route_attempts = RequestRouteAttempts::default();
    tracing::debug!(
        request_id = %request_id,
        downstream_key_id = %downstream.id,
        path = %request_path,
        original_model = %model,
        normalized_model = %&normalized_model,
        stream = request_stream,
        candidate_protocols = ?candidate_protocols,
        "resolved candidate protocols"
    );
    let mut last_error = None;
    let preferred_upstream_id = if let Some(upstream_id) = response_history_context
        .as_ref()
        .and_then(ResponseHistoryContext::continuation_upstream_id)
    {
        routing_snapshot
            .upstreams
            .iter()
            .any(|upstream| {
                upstream.active
                    && upstream.id == upstream_id
                    && upstream.supports_model_with(&normalized_model, case_insensitive)
            })
            .then(|| upstream_id.to_string())
    } else if runtime_settings.routing_affinity_enabled {
        match state.get_affinity_upstream(&downstream.id, &normalized_model) {
            Some(upstream_id)
                if routing_snapshot.upstreams.iter().any(|upstream| {
                    upstream.active
                        && upstream.id == upstream_id
                        && upstream.supports_model_with(&normalized_model, case_insensitive)
                }) =>
            {
                Some(upstream_id)
            }
            Some(_) => {
                state.clear_affinity_upstream(&downstream.id, &normalized_model);
                None
            }
            None => None,
        }
    } else {
        None
    };
    let mut any_same_route_retry = false;
    // B2 common-mode breaker state, scoped to this downstream request. When
    // K consecutive different routes fail with the exact same (class, status)
    // the request shape is the likely culprit: stop replaying, revert the
    // cooldowns this request wrote, and return a request-level error instead
    // of burning the whole pool.  Transient classes first get one delayed
    // replay round (the aggregated-gateway outage case) before the verdict.
    // The streak only grows when a *different route on a different upstream
    // host* fails with the identical signature; the same route or the same
    // host failing again (e.g. across routing rounds or across keys of one
    // aggregated gateway) is a route-local fault, not pool-wide evidence.
    let mut common_mode: Option<CommonModeStreak> = None;
    let mut common_mode_first_message: Option<String> = None;
    let mut common_mode_failed_routes: Vec<RouteHealthKey> = Vec::new();
    let mut common_mode_tripped = false;
    let mut common_mode_verdict: Option<CommonModeVerdict> = None;
    // C4.2: the request fast-failed at the local pre-dispatch concurrency gate
    // (zero physical upstream attempts) and the terminal error is the distinct
    // `gateway_concurrency_saturated` verdict.  When set, the terminal block
    // must NOT aggregate the ledger into an `upstream_routes_exhausted` error
    // — a local-gate verdict is a gateway-side capacity fact, not a route
    // exhaustion story, and the two must stay distinguishable.
    let mut local_gate_fast_failed = false;
    let mut transient_pool_replay_done = false;
    let mut transient_pool_retried = false;

    'routing_rounds: loop {
        let upstream_runtime_snapshots = state
            .upstream_runtime_snapshots()
            .await
            .map_err(|_| runtime_coordination_unavailable_gateway_error())?;
        last_error = None;
        last_failure_upstream = None;
        let mut stream_only_final_attempt = false;
        // C3: the account of the most recent local pre-dispatch concurrency
        // rejection in this round.  When the whole round is a pure local
        // concurrency exhaustion (every candidate hit the gate, no physical
        // attempt), the request queues for a free slot on this account instead
        // of burning the ConcurrencySaturated retry budget.
        let mut last_local_concurrency_account: Option<AccountConcurrencyKey> = None;

        for protocol in candidate_protocols.iter().copied() {
            for upstream in routing_snapshot.upstreams.iter() {
                let Some(runtime_model_slug) =
                    upstream.resolved_model_name_with(&normalized_model, case_insensitive)
                else {
                    continue;
                };
                for api_key in route_api_keys(upstream, &runtime_model_slug, case_insensitive) {
                    let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                    if !route_is_candidate(upstream, &key_fingerprint, protocol) {
                        continue;
                    }
                    let (route_health_key, _) = route_health_keys(
                        upstream,
                        &key_fingerprint,
                        &runtime_model_slug,
                        protocol,
                    );
                    request_route_attempts.register_eligible(
                        route_set_aggregate_key(upstream, &runtime_model_slug, protocol),
                        route_health_key,
                    );
                }
            }
        }

        'candidate_passes: for (optional_miss_tier, protocol) in candidate_passes.iter().copied() {
            let upstream_optional_misses = |upstream: &UpstreamConfig| {
                let runtime_model_slug =
                    upstream.resolved_model_name_with(&normalized_model, case_insensitive)?;
                route_api_keys(upstream, &runtime_model_slug, case_insensitive)
                    .into_iter()
                    .filter_map(|api_key| {
                        let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                        route_is_candidate(upstream, &key_fingerprint, protocol)
                            .then(|| route_capability(upstream, &key_fingerprint, protocol))
                            .flatten()
                            .map(|route| route.optional_misses)
                    })
                    .min()
            };
            let mut upstreams = routing_snapshot
                .upstreams
                .iter()
                .filter(|upstream| upstream_has_candidate_route(upstream, protocol))
                .filter(|upstream| {
                    optional_miss_tier.is_none_or(|misses| {
                        upstream_optional_misses(upstream)
                            .is_some_and(|candidate| candidate == misses)
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            let mut deprioritized_upstreams = Vec::new();
            upstreams.retain(|upstream| {
                let is_non_premium_request =
                    !upstream.is_premium_model_request_with(&normalized_model, case_insensitive);
                let should_deprioritize = upstream.protect_premium_quota
                    && !upstream.premium_models.is_empty()
                    && is_non_premium_request;
                if should_deprioritize {
                    deprioritized_upstreams.push(upstream.clone());
                    false
                } else {
                    true
                }
            });
            let total_candidate_count = upstreams.len() + deprioritized_upstreams.len();
            let history_pinned_upstream = response_history_context
                .as_ref()
                .and_then(ResponseHistoryContext::continuation_upstream_id);
            // Ordinary affinity only helps when there is a single viable upstream; continuation
            // history pinning is stricter and applies even when multiple candidates are available.
            let use_routing_affinity = history_pinned_upstream.is_some()
                || (runtime_settings.routing_affinity_enabled && total_candidate_count == 1);
            let ranking_pressure = |upstream: &UpstreamConfig| {
                let runtime = upstream_runtime_snapshots
                    .get(&upstream.id)
                    .copied()
                    .unwrap_or_default();
                let request_cost = 1.0_f64;
                let minute_pressure = runtime.minute_cost + request_cost;
                let five_hour_pressure = runtime.five_hour_cost + request_cost;
                (
                    false,
                    0,
                    runtime.in_flight,
                    minute_pressure as u64 * 1_000 / upstream.requests_per_minute.max(1) as u64,
                    five_hour_pressure as u64 * 1_000
                        / upstream.request_quota_requests.max(1) as u64,
                )
            };
            let optional_capability_misses_by_upstream = upstreams
                .iter()
                .chain(deprioritized_upstreams.iter())
                .map(|upstream| {
                    (
                        upstream.id.clone(),
                        upstream_optional_misses(upstream)
                            .unwrap_or(requested_features.optional.len()),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let optional_capability_misses = |upstream: &UpstreamConfig| {
                optional_capability_misses_by_upstream
                    .get(&upstream.id)
                    .copied()
                    .unwrap_or_default()
            };
            let ranking_key = |upstream: &UpstreamConfig| {
                let (cooled, cooldown_remaining, in_flight, minute_pressure, five_hour_pressure) =
                    ranking_pressure(upstream);
                (
                    optional_capability_misses(upstream),
                    cooled,
                    cooldown_remaining,
                    Reverse(upstream.priority),
                    in_flight,
                    minute_pressure,
                    five_hour_pressure,
                    upstream.id.clone(),
                )
            };
            upstreams.sort_by_key(&ranking_key);
            deprioritized_upstreams.sort_by_key(ranking_key);
            upstreams.extend(deprioritized_upstreams);
            if !requested_features.optional.is_empty() {
                upstreams.sort_by_key(|upstream| optional_capability_misses(upstream));
            }
            if use_routing_affinity {
                if let Some(preferred_upstream_id) = preferred_upstream_id.as_deref() {
                    if let Some(position) = upstreams
                        .iter()
                        .position(|upstream| upstream.id == preferred_upstream_id)
                    {
                        if history_pinned_upstream == Some(preferred_upstream_id) {
                            let preferred = upstreams.remove(position);
                            upstreams.insert(0, preferred);
                        } else if position > 0 {
                            let escape_ratio = runtime_settings
                                .routing_affinity_escape_pressure_ratio
                                .max(1.0);
                            let (
                                preferred_cooled,
                                preferred_cooldown,
                                preferred_in_flight,
                                preferred_minute_pressure,
                                preferred_five_hour_pressure,
                            ) = ranking_pressure(&upstreams[position]);
                            let (
                                best_cooled,
                                best_cooldown,
                                best_in_flight,
                                best_minute_pressure,
                                best_five_hour_pressure,
                            ) = ranking_pressure(&upstreams[0]);
                            let should_escape = (preferred_cooled && !best_cooled)
                                || metric_exceeds_ratio(
                                    preferred_cooldown as f64,
                                    best_cooldown as f64,
                                    escape_ratio,
                                )
                                || metric_exceeds_ratio(
                                    preferred_in_flight as f64,
                                    best_in_flight as f64,
                                    escape_ratio,
                                )
                                || metric_exceeds_ratio(
                                    preferred_minute_pressure as f64,
                                    best_minute_pressure as f64,
                                    escape_ratio,
                                )
                                || metric_exceeds_ratio(
                                    preferred_five_hour_pressure as f64,
                                    best_five_hour_pressure as f64,
                                    escape_ratio,
                                );
                            if should_escape {
                                tracing::debug!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    protocol = ?protocol,
                                    preferred_upstream_id = %preferred_upstream_id,
                                    escape_ratio,
                                    preferred_minute_pressure,
                                    best_minute_pressure,
                                    preferred_five_hour_pressure,
                                    best_five_hour_pressure,
                                    preferred_in_flight,
                                    best_in_flight,
                                    preferred_cooldown,
                                    best_cooldown,
                                    "routing affinity escaped due upstream pressure"
                                );
                            } else {
                                let preferred = upstreams.remove(position);
                                upstreams.insert(0, preferred);
                                tracing::debug!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    protocol = ?protocol,
                                    preferred_upstream_id = %preferred_upstream_id,
                                    escape_ratio,
                                    "applied routing affinity to candidate order"
                                );
                            }
                        }
                    }
                }
            }
            let ranking_bucket_key = |upstream: &UpstreamConfig| {
                let (cooled, cooldown_remaining, in_flight, minute_pressure, five_hour_pressure) =
                    ranking_pressure(upstream);
                (
                    optional_capability_misses(upstream),
                    cooled,
                    cooldown_remaining,
                    Reverse(upstream.priority),
                    in_flight,
                    minute_pressure,
                    five_hour_pressure,
                )
            };
            if upstreams.len() > 1 {
                let top_bucket_key = ranking_bucket_key(&upstreams[0]);
                let top_bucket_len = upstreams
                    .iter()
                    .take_while(|upstream| ranking_bucket_key(upstream) == top_bucket_key)
                    .count();
                let tie_breaker =
                    state.next_routing_tie_breaker(&downstream.id, &normalized_model, protocol);
                if top_bucket_len > 1 {
                    let rotation = tie_breaker as usize % top_bucket_len;
                    if rotation > 0 {
                        upstreams[..top_bucket_len].rotate_left(rotation);
                    }
                    tracing::debug!(
                        request_id = %request_id,
                        downstream_key_id = %downstream.id,
                        path = %request_path,
                        original_model = %model,
                        normalized_model = %&normalized_model,
                        protocol = ?protocol,
                        tie_bucket_size = top_bucket_len,
                        tie_rotation = rotation,
                        "rotated equal-pressure upstream candidates"
                    );
                }
            }
            let candidate_summary = upstreams
            .iter()
            .map(|upstream| {
                let runtime = upstream_runtime_snapshots
                    .get(&upstream.id)
                    .copied()
                    .unwrap_or_default();
                let request_cost = 1.0_f64;
                let minute_cost = runtime.minute_cost + request_cost;
                let five_hour_cost = runtime.five_hour_cost + request_cost;
                format!(
                    "{}|{}|{:?}|in_flight={}|minute_cost={}/{}|five_hour_cost={}/{}|request_cost={}|protect_premium_quota={}|premium_match={}",
                    upstream.id,
                    upstream.name,
                    protocol,
                    runtime.in_flight,
                    minute_cost,
                    upstream.requests_per_minute,
                    five_hour_cost,
                    upstream.request_quota_requests,
                    request_cost,
                    upstream.protect_premium_quota,
                    upstream.is_premium_model_request_with(&normalized_model, case_insensitive)
                )
            })
            .collect::<Vec<_>>();
            let upstreams_for_retry = upstreams.clone();
            tracing::debug!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %&normalized_model,
                protocol = ?protocol,
                candidates = ?candidate_summary,
                "sorted upstream candidates"
            );

            for (upstream_index, upstream) in upstreams.into_iter().enumerate() {
                let runtime = upstream_runtime_snapshots
                    .get(&upstream.id)
                    .copied()
                    .unwrap_or_default();
                let request_cost = 1.0_f64;
                let minute_cost = runtime.minute_cost + request_cost;
                let five_hour_cost = runtime.five_hour_cost + request_cost;
                let Some(runtime_model_slug) =
                    upstream.resolved_model_name_with(&normalized_model, case_insensitive)
                else {
                    continue;
                };
                let mut candidate_keys =
                    route_api_keys(&upstream, &runtime_model_slug, case_insensitive)
                        .into_iter()
                        .filter(|api_key| {
                            let key_fingerprint = route_key_fingerprint(&upstream, api_key);
                            let (route_health_key, _) = route_health_keys(
                                &upstream,
                                &key_fingerprint,
                                &runtime_model_slug,
                                protocol,
                            );
                            request_route_attempts.should_attempt(&route_health_key)
                                && route_is_candidate(&upstream, &key_fingerprint, protocol)
                                && optional_miss_tier.is_none_or(|misses| {
                                    route_capability(&upstream, &key_fingerprint, protocol)
                                        .is_some_and(|route| route.optional_misses == misses)
                                })
                        })
                        .collect::<Vec<_>>();
                rotate_route_keys_for_request(
                    &mut candidate_keys,
                    &request_id,
                    &upstream.id,
                    &runtime_model_slug,
                    protocol,
                );
                if let Some(preferred_profile) = exact_continuation
                    .as_ref()
                    .map(GatewayContinuationState::preferred_profile)
                    .filter(|profile| profile.upstream_id == upstream.id)
                {
                    promote_preferred_route_key(
                        &upstream,
                        &mut candidate_keys,
                        &preferred_profile.key_fingerprint,
                    );
                }
                if candidate_keys.is_empty() {
                    tracing::debug!(
                        request_id = %request_id,
                        downstream_key_id = %downstream.id,
                        path = %request_path,
                        original_model = %model,
                        normalized_model = %&normalized_model,
                        selected_upstream_id = %upstream.id,
                        selected_upstream_name = %upstream.name,
                        selected_upstream_protocol = ?protocol,
                        api_key_model_count = upstream.api_key_models.len(),
                        "upstream has no eligible mapped key route for requested model; skipping"
                    );
                    continue;
                }
                tracing::info!(
                    request_id = %request_id,
                    downstream_key_id = %downstream.id,
                    path = %request_path,
                    original_model = %model,
                    normalized_model = %&normalized_model,
                    selected_upstream_id = %upstream.id,
                    selected_upstream_name = %upstream.name,
                    selected_upstream_protocol = ?protocol,
                    stream = request_stream,
                    in_flight = runtime.in_flight,
                    request_cost,
                    minute_cost,
                    minute_quota = upstream.requests_per_minute,
                    five_hour_cost,
                    five_hour_quota = upstream.request_quota_requests,
                    candidate_key_count = candidate_keys.len(),
                    "considering upstream candidate"
                );

                let mut stream_only_recovery_leader = None;
                let mut stream_only_recovery_identity = None;
                let mut stream_only_recovery = StreamOnlyRecoveryState::default();
                'key_candidates: for (key_index, api_key) in candidate_keys.iter().enumerate() {
                    let key_fingerprint = route_key_fingerprint(&upstream, api_key);
                    let account_key =
                        AccountConcurrencyKey::new(upstream.id.clone(), key_fingerprint.clone());
                    let (route_health_key, key_health_key) = route_health_keys(
                        &upstream,
                        &key_fingerprint,
                        &runtime_model_slug,
                        protocol,
                    );
                    let route_id = anonymous_route_id(
                        &upstream.id,
                        &key_fingerprint,
                        &runtime_model_slug,
                        WireProtocol::from(protocol),
                    );
                    if !request_route_attempts.should_attempt(&route_health_key) {
                        continue;
                    }
                    if account_recovery
                        .active_probe_account()
                        .is_some_and(|active| active != &account_key)
                    {
                        continue;
                    }
                    // A3 last-resort probe: when the previous round armed this
                    // route, reserve through the early half-open probe API
                    // (ignores the remaining cooldown, single-flight + 1s
                    // per-route interval).  A refusal falls back to the ordinary
                    // reserve semantics below so no in-gateway wait is added.
                    let mut last_resort_probe_armed =
                        request_route_attempts.take_last_resort_probe_for(&route_health_key);
                    let route_health_permit = loop {
                        let availability = if last_resort_probe_armed {
                            state
                                .reserve_route_health_probe(&route_health_key, &key_health_key)
                                .await
                                .map_err(|_| runtime_coordination_unavailable_gateway_error())?
                        } else {
                            state
                                .reserve_route_health(&route_health_key, &key_health_key)
                                .await
                                .map_err(|_| runtime_coordination_unavailable_gateway_error())?
                        };
                        match availability {
                            RouteAvailability::Ready(permit) => {
                                if last_resort_probe_armed {
                                    request_route_attempts.mark_last_resort_probe_granted();
                                }
                                break Some(Arc::new(TokioMutex::new(Some(permit))));
                            }
                            RouteAvailability::Cooling {
                                class,
                                retry_after,
                                upstream_status,
                            } => {
                                if last_resort_probe_armed {
                                    // Probe refused (busy lease / per-route
                                    // interval / nothing left to probe): fall
                                    // back to the ordinary reserve path.
                                    last_resort_probe_armed = false;
                                    continue;
                                }
                                if account_recovery.active_probe_account() == Some(&account_key) {
                                    tokio::select! {
                                        _ = tokio::time::sleep(retry_after) => {}
                                        error = account_recovery.wait_for_probe_interruption() => {
                                            account_recovery
                                                .complete_attempt(
                                                    &account_key,
                                                    AccountProbeOutcome::Cancelled,
                                                )
                                                .await?;
                                            if error.error_category()
                                                == "runtime_coordination_unavailable"
                                            {
                                                return Err(error);
                                            }
                                            last_error = Some(error);
                                            last_failure_upstream = Some((
                                                upstream.id.clone(),
                                                Some(upstream.name.clone()),
                                            ));
                                            break None;
                                        }
                                    }
                                    continue;
                                }
                                record_cooled_route_attempt(
                                    &request_route_attempts,
                                    &upstream,
                                    &key_fingerprint,
                                    &runtime_model_slug,
                                    protocol,
                                    class,
                                    retry_after,
                                    upstream_status,
                                    None,
                                    false,
                                    false,
                                );
                                last_error = Some(GatewayError::TemporaryUpstreamUnavailable(
                                    "all eligible upstream routes are temporarily unavailable"
                                        .into(),
                                ));
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                continue 'key_candidates;
                            }
                            RouteAvailability::HalfOpenBusy {
                                class,
                                retry_after,
                                upstream_status,
                            } => {
                                if last_resort_probe_armed {
                                    // Probe refused (busy lease / per-route
                                    // interval / nothing left to probe): fall
                                    // back to the ordinary reserve path.
                                    last_resort_probe_armed = false;
                                    continue;
                                }
                                if account_recovery.active_probe_account() == Some(&account_key) {
                                    tokio::select! {
                                        _ = tokio::time::sleep(retry_after) => {}
                                        error = account_recovery.wait_for_probe_interruption() => {
                                            account_recovery
                                                .complete_attempt(
                                                    &account_key,
                                                    AccountProbeOutcome::Cancelled,
                                                )
                                                .await?;
                                            if error.error_category()
                                                == "runtime_coordination_unavailable"
                                            {
                                                return Err(error);
                                            }
                                            last_error = Some(error);
                                            last_failure_upstream = Some((
                                                upstream.id.clone(),
                                                Some(upstream.name.clone()),
                                            ));
                                            break None;
                                        }
                                    }
                                    continue;
                                }
                                // Half-open exclusive window: the route is
                                // being probed RIGHT NOW and other requests
                                // may re-enter in ~1s (T3).  Kept separate
                                // from real cooldowns in the ledger so the
                                // busy capability is not counted as a
                                // transient-server failure, and busy waits do
                                // not consume the ordinary retry rounds.
                                record_cooled_route_attempt(
                                    &request_route_attempts,
                                    &upstream,
                                    &key_fingerprint,
                                    &runtime_model_slug,
                                    protocol,
                                    class,
                                    retry_after,
                                    upstream_status,
                                    None,
                                    true,
                                    false,
                                );
                                last_error = Some(GatewayError::TemporaryUpstreamUnavailable(
                                    "all eligible upstream routes are temporarily unavailable"
                                        .into(),
                                ));
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                continue 'key_candidates;
                            }
                        }
                    };
                    let Some(route_health_permit) = route_health_permit else {
                        break 'candidate_passes;
                    };
                    let mut route_failed: Option<(FailureClass, Option<u16>, String)> = None;
                    let mut same_route_retry_attempted = false;
                    let candidate_capability_snapshot = (*capability_snapshot).clone();
                    let resolved_route = route_capability(&upstream, &key_fingerprint, protocol)
                        .and_then(|route| route.resolved.clone());
                    let mut attempt_mode = if stream_only_recovery.consumed {
                        UpstreamAttemptMode::Json
                    } else {
                        select_upstream_attempt_mode(request_stream, resolved_route.as_ref())
                    };
                    loop {
                        let account_admission =
                            match account_recovery.wait_for_account(account_key.clone()).await {
                                Ok(admission) => admission,
                                Err(error) => {
                                    finish_route_health_permit(
                                        &route_health_permit,
                                        RouteOutcome::Cancelled,
                                    )
                                    .await?;
                                    if error.error_category() == "runtime_coordination_unavailable"
                                    {
                                        return Err(error);
                                    }
                                    last_error = Some(error);
                                    last_failure_upstream =
                                        Some((upstream.id.clone(), Some(upstream.name.clone())));
                                    break;
                                }
                            };
                        let account_probe = match account_admission {
                            AccountAdmission::Ordinary => None,
                            AccountAdmission::Deferred { retry_after } => {
                                finish_route_health_permit(
                                    &route_health_permit,
                                    RouteOutcome::Cancelled,
                                )
                                .await?;
                                record_cooled_route_attempt(
                                    &request_route_attempts,
                                    &upstream,
                                    &key_fingerprint,
                                    &runtime_model_slug,
                                    protocol,
                                    FailureClass::ConcurrencySaturated,
                                    retry_after,
                                    None,
                                    None,
                                    false,
                                    true,
                                );
                                last_error = Some(GatewayError::ConcurrencyFull {
                                    message: "upstream account is waiting for recovery".into(),
                                    retry_after: Some(retry_after),
                                    upstream_status: None,
                                });
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                continue 'key_candidates;
                            }
                            AccountAdmission::Probe(lease) => Some(lease),
                        };
                        let upstream_request_lease = {
                            #[cfg(test)]
                            if take_upstream_reservation_failure_test_hook(&upstream.id) {
                                Err(crate::state::UpstreamAdmissionError::new(
                                    crate::state::UpstreamAdmissionRejectionReason::LocalConcurrency,
                                    "upstream request concurrency capacity is full".into(),
                                    1,
                                ))
                            } else {
                                state
                                    .try_reserve_upstream_account_request(
                                        &upstream,
                                        &key_fingerprint,
                                        model,
                                    )
                                    .await
                            }
                            #[cfg(not(test))]
                            state
                                .try_reserve_upstream_account_request(
                                    &upstream,
                                    &key_fingerprint,
                                    model,
                                )
                                .await
                        };
                        let upstream_request_lease = match upstream_request_lease {
                            Ok(lease) => lease,
                            Err(admission_error) => {
                                match admission_error.reason {
                                    crate::state::UpstreamAdmissionRejectionReason::LocalConcurrency => {
                                        finish_route_health_permit(
                                            &route_health_permit,
                                            RouteOutcome::Cancelled,
                                        )
                                        .await?;
                                    }
                                    crate::state::UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable
                                    | crate::state::UpstreamAdmissionRejectionReason::HedgeMinuteQuota
                                    | crate::state::UpstreamAdmissionRejectionReason::HedgeWindowQuota => {
                                        finish_route_health_permit(
                                            &route_health_permit,
                                            RouteOutcome::Cancelled,
                                        )
                                        .await?;
                                        return Err(runtime_coordination_unavailable_gateway_error());
                                    }
                                }
                                if account_probe.is_some() {
                                    account_recovery
                                        .complete_attempt(
                                            &account_key,
                                            AccountProbeOutcome::AttemptFailed,
                                        )
                                        .await?;
                                    continue;
                                }
                                // Local concurrency admission rejection: the
                                // upstream was never called, so this must be
                                // classified as ConcurrencySaturated (429/503
                                // family with retry-after) instead of a
                                // misleading 502 "upstream_invalid_response".
                                last_local_concurrency_account = Some(account_key.clone());
                                let retry_after =
                                    Duration::from_secs(admission_error.retry_after_seconds.max(1))
                                        .min(upstream_retry_after_cap);
                                record_cooled_route_attempt(
                                    &request_route_attempts,
                                    &upstream,
                                    &key_fingerprint,
                                    &runtime_model_slug,
                                    protocol,
                                    FailureClass::ConcurrencySaturated,
                                    retry_after,
                                    None,
                                    None,
                                    false,
                                    true,
                                );
                                last_error = Some(GatewayError::ConcurrencyFull {
                                    message: "upstream request concurrency capacity is full".into(),
                                    retry_after: Some(retry_after),
                                    upstream_status: None,
                                });
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                break;
                            }
                        };
                        let upstream_request_guard = UpstreamRequestReservation::new(
                            UpstreamRequestGuard::new(state.clone(), upstream_request_lease),
                        );
                        tracing::info!(
                            request_id = %request_id,
                            downstream_key_id = %downstream.id,
                            path = %request_path,
                            original_model = %model,
                            normalized_model = %&normalized_model,
                            selected_upstream_id = %upstream.id,
                            selected_upstream_protocol = ?protocol,
                            route_id = %route_id,
                            upstream_attempt_mode = attempt_mode.as_str(),
                            request_cost,
                            "reserved upstream capacity"
                        );
                        state.mark_active_gateway_request_upstream(
                            &request_id,
                            &upstream.id,
                            &upstream.name,
                        );

                        let stream_completion_context = attempt_mode
                            .needs_stream_completion_context()
                            .then(|| StreamCompletionContext {
                                state: state.clone(),
                                route_health_key: route_health_key.clone(),
                                route_attempts: request_route_attempts.clone(),
                                route_health_permit: route_health_permit.clone(),
                                upstream_request_guard: upstream_request_guard.clone(),
                                downstream_concurrency_guard: downstream_concurrency_guard.clone(),
                                hedge_control: None,
                                health_verdict_pending: Arc::new(AtomicBool::new(false)),
                            });
                        if let (Some(cancellation), Some(completion)) = (
                            pre_header_cancellation.as_ref(),
                            stream_completion_context.as_ref(),
                        ) {
                            cancellation.arm(
                                completion.clone(),
                                StreamUsageLogContext {
                                    state: state.clone(),
                                    request_id: request_id.clone(),
                                    downstream_key_id: downstream.id.clone(),
                                    downstream_name: Some(downstream.name.clone()),
                                    upstream_key_id: upstream.id.clone(),
                                    upstream_name: Some(upstream.name.clone()),
                                    upstream_protocol: protocol,
                                    endpoint: request_path.to_string(),
                                    model: model.to_string(),
                                    inference_strength: inference_strength.clone(),
                                    user_agent: user_agent.clone(),
                                    compatibility: None,
                                    normalized_model: normalized_model.to_string(),
                                    status: StatusCode::OK,
                                    wire_status: StatusCode::OK,
                                    transport_committed: false,
                                    error_message: None,
                                    error_category: None,
                                    started,
                                    account_wait_ms: 0,
                                    first_token_latency: FirstTokenLatency::default(),
                                    hedge_control: None,
                                    stream_diagnostics: None,
                                },
                            );
                        }
                        #[cfg(test)]
                        wait_on_pre_header_preparation_test_gate(
                            headers.contains_key(PRE_HEADER_PREPARATION_TEST_GATE_HEADER),
                        )
                        .await;
                        let global_context_profile = state
                            .global_context_profile_for_upstream_base_url(&upstream.base_url)
                            .await;
                        let (
                            mut dispatch_body,
                            dispatch_response_history_context,
                            chat_fallback_stage,
                        ) = if endpoint == EndpointKind::Responses
                            && protocol == UpstreamProtocol::ChatCompletions
                            && chat_only_responses_fallback
                        {
                            let stage = initial_chat_fallback_stage(
                                &state,
                                &downstream.id,
                                client_family,
                                &normalized_model,
                                &upstream.id,
                                original_responses_body
                                    .as_ref()
                                    .expect("responses requests should retain original body"),
                            );
                            tracing::info!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %&normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                client_family,
                                fallback_stage = stage.as_str(),
                                "selected chat-only Responses fallback stage"
                            );
                            match prepare_responses_chat_fallback_request(
                                &state,
                                &downstream.id,
                                original_responses_body
                                    .as_ref()
                                    .expect("responses requests should retain original body"),
                                stage,
                            )
                            .await
                            {
                                Ok((prepared_body, prepared_history_context)) => (
                                    prepared_body,
                                    Some(prepared_history_context.with_fallback_stage(stage)),
                                    Some(stage),
                                ),
                                Err(error) => {
                                    if let Some(cancellation) = pre_header_cancellation.as_ref() {
                                        cancellation.disarm();
                                    }
                                    let _ = append_gateway_usage_log(
                                        &state,
                                        &request_id,
                                        &downstream.id,
                                        &downstream.name,
                                        "",
                                        None,
                                        request_path,
                                        model,
                                        inference_strength.as_deref(),
                                        user_agent.as_deref(),
                                        None,
                                        error.status_code(),
                                        Some(error.to_string()),
                                        Some(error.error_category().to_string()),
                                        0,
                                        0,
                                        0,
                                        started,
                                    )
                                    .await;
                                    active_request_guard.fail_and_finish(error.error_category());
                                    let release = upstream_request_guard.release().await;
                                    return Err(if release.is_err() {
                                        runtime_coordination_unavailable_gateway_error()
                                    } else {
                                        error
                                    });
                                }
                            }
                        } else {
                            (body.clone(), response_history_context.clone(), None)
                        };
                        let mut dispatch_response_history_context =
                            dispatch_response_history_context;
                        if let (Some(context), Some(continuation)) = (
                            dispatch_response_history_context.as_mut(),
                            exact_continuation.as_ref(),
                        ) {
                            *context = context.with_selected_route(continuation.clone(), None)?;
                        }
                        // T3: record the actually-dispatched route's provider
                        // profile on the history context (so the stored
                        // response history carries its source profile), and
                        // when the replayed history was captured from a
                        // different profile, sanitize the replayed items
                        // before they are sent upstream — the same
                        // cross-provider sanitization the continuation-pin
                        // escape channel applies, now enforced on every
                        // ordinary account rotation (T3.2), plus a T2.1
                        // normalization pass for legacy polluted arguments
                        // (T3.3).
                        let selected_source_profile = GatewaySourceProfile::from_route(
                            &upstream,
                            &key_fingerprint,
                            &runtime_model_slug,
                            protocol,
                        );
                        let mut cross_profile_replay = false;
                        if let Some(context) = dispatch_response_history_context.as_mut() {
                            if context
                                .source_profile()
                                .is_some_and(|replayed| replayed != selected_source_profile)
                            {
                                cross_profile_replay = true;
                            }
                            *context = context.with_source_profile(selected_source_profile);
                        }
                        if cross_profile_replay {
                            sanitize_history_for_cross_provider_replay(&mut dispatch_body);
                            normalize_replayed_history_tool_arguments(
                                &mut dispatch_body,
                                Some(model),
                                &request_id,
                            );
                            tracing::info!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                history_cross_profile_replay = true,
                                "replayed response history crossed provider profiles; sanitized before dispatch"
                            );
                        }

                        let route_hedge_candidates = if request_stream
                            && attempt_mode == UpstreamAttemptMode::SsePassThrough
                            && chat_fallback_stage.is_none()
                        {
                            let mut candidates = candidate_keys[key_index + 1..]
                                .iter()
                                .filter_map(|api_key| {
                                    let key_fingerprint = route_key_fingerprint(&upstream, api_key);
                                    let (route_health_key, _) = route_health_keys(
                                        &upstream,
                                        &key_fingerprint,
                                        &runtime_model_slug,
                                        protocol,
                                    );
                                    if !request_route_attempts.should_attempt(&route_health_key) {
                                        return None;
                                    }
                                    let route =
                                        route_capability(&upstream, &key_fingerprint, protocol)?;
                                    Some(RouteHedgeCandidate {
                                        upstream: upstream.clone(),
                                        api_key: api_key.clone(),
                                        key_fingerprint: key_fingerprint.clone(),
                                        route_health_key,
                                        protocol,
                                        resolved_capabilities: route.resolved.clone(),
                                    })
                                })
                                .collect::<Vec<_>>();
                            candidates.extend(
                                upstreams_for_retry
                                    .iter()
                                    .skip(upstream_index + 1)
                                    .filter_map(|candidate| {
                                        let runtime_model_slug = candidate
                                            .resolved_model_name_with(
                                                &normalized_model,
                                                case_insensitive,
                                            )?;
                                        route_api_keys(
                                            candidate,
                                            &runtime_model_slug,
                                            case_insensitive,
                                        )
                                        .into_iter()
                                        .find_map(
                                            |api_key| {
                                                let key_fingerprint =
                                                    route_key_fingerprint(candidate, &api_key);
                                                let (route_health_key, _) = route_health_keys(
                                                    candidate,
                                                    &key_fingerprint,
                                                    &runtime_model_slug,
                                                    protocol,
                                                );
                                                if !route_is_candidate(
                                                    candidate,
                                                    &key_fingerprint,
                                                    protocol,
                                                ) || !request_route_attempts
                                                    .should_attempt(&route_health_key)
                                                    || optional_miss_tier.is_some_and(|misses| {
                                                        route_capability(
                                                            candidate,
                                                            &key_fingerprint,
                                                            protocol,
                                                        )
                                                        .is_none_or(|route| {
                                                            route.optional_misses != misses
                                                        })
                                                    })
                                                {
                                                    return None;
                                                }
                                                let route = route_capability(
                                                    candidate,
                                                    &key_fingerprint,
                                                    protocol,
                                                )?;
                                                Some(RouteHedgeCandidate {
                                                    upstream: candidate.clone(),
                                                    api_key,
                                                    key_fingerprint,
                                                    route_health_key,
                                                    protocol,
                                                    resolved_capabilities: route.resolved.clone(),
                                                })
                                            },
                                        )
                                    }),
                            );
                            candidates
                        } else {
                            Vec::new()
                        };

                        let route_attempt_context = RouteAttemptContext {
                            state: &state,
                            route_attempts: &request_route_attempts,
                            route_health_key: &route_health_key,
                            route: RouteCapabilityRoute::new(
                                &capability_snapshot,
                                &upstream,
                                &key_fingerprint,
                                model,
                                &runtime_model_slug,
                                protocol,
                            ),
                            requested: &requested_features,
                            requested_value: inference_strength.as_deref(),
                            retry_after_cap: upstream_retry_after_cap,
                            retry_after_cooldown_cap: upstream_retry_after_cooldown_cap,
                        };
                        let (account_feedback_sender, account_feedback_receiver) =
                            if account_probe.is_some() {
                                let (sender, receiver) = oneshot::channel();
                                (Some(sender), Some(receiver))
                            } else {
                                (None, None)
                            };
                        let effective_route_hedge_candidates = if account_probe.is_some() {
                            &[][..]
                        } else {
                            route_hedge_candidates.as_slice()
                        };
                        let send_future = send_to_upstream(
                            &state,
                            runtime_settings.clone(),
                            &upstream,
                            api_key,
                            &[],
                            effective_route_hedge_candidates,
                            resolved_route.as_ref(),
                            &candidate_capability_snapshot,
                            &requested_features,
                            protocol,
                            &dispatch_body,
                            endpoint,
                            request_stream,
                            attempt_mode,
                            started,
                            &request_id,
                            model,
                            &normalized_model,
                            &downstream.id,
                            &downstream.name,
                            inference_strength.as_deref(),
                            user_agent.as_deref(),
                            chat_only_responses_fallback,
                            global_context_profile.as_ref(),
                            stream_completion_context.clone(),
                            upstream_request_guard.clone(),
                            request_route_attempts.clone(),
                            route_health_key.clone(),
                            dispatch_response_history_context.clone(),
                            Some(&mut active_request_guard),
                            None,
                            stream_only_recovery_request_safe,
                            account_feedback_sender,
                            &mut stream_only_recovery,
                            &mut stream_only_recovery_leader,
                            &mut stream_only_recovery_identity,
                            account_recovery.waited().as_millis() as u64,
                            first_semantic_deadline,
                        );
                        let mut result =
                            if let Some(mut feedback_receiver) = account_feedback_receiver {
                                let mut send_future = Box::pin(send_future);
                                tokio::select! {
                                    biased;
                                    error = account_recovery.wait_for_probe_interruption() => {
                                        match account_recovery
                                            .complete_attempt(
                                                &account_key,
                                                AccountProbeOutcome::Cancelled,
                                            )
                                            .await
                                        {
                                            Ok(()) => Err(error),
                                            Err(cleanup_error) => Err(cleanup_error),
                                        }
                                    }
                                    feedback = &mut feedback_receiver => {
                                        match feedback {
                                            Ok(outcome) => {
                                                match account_recovery
                                                    .complete_attempt(&account_key, outcome)
                                                    .await
                                                {
                                                    Ok(()) => send_future.await,
                                                    Err(error) => Err(error),
                                                }
                                            }
                                            Err(_) => {
                                                let result = send_future.await;
                                                let outcome = account_attempt_outcome(&result);
                                                match account_recovery
                                                    .complete_attempt(&account_key, outcome)
                                                    .await
                                                {
                                                    Ok(()) => result,
                                                    Err(error) => Err(error),
                                                }
                                            }
                                        }
                                    }
                                    result = &mut send_future => {
                                        let outcome = account_attempt_outcome(&result);
                                        match account_recovery
                                            .complete_attempt(&account_key, outcome)
                                            .await
                                        {
                                            Ok(()) => result,
                                            Err(error) => Err(error),
                                        }
                                    }
                                }
                            } else {
                                let result = send_future.await;
                                let outcome = account_attempt_outcome(&result);
                                match account_recovery
                                    .complete_attempt(&account_key, outcome)
                                    .await
                                {
                                    Ok(()) => result,
                                    Err(error) => Err(error),
                                }
                            };
                        active_request_guard.clear_aggregate_cancellation_log();
                        if let Some(cancellation) = pre_header_cancellation.as_ref() {
                            cancellation.disarm();
                        }

                        // Non-streaming requests and failed streaming attempts should
                        // release upstream capacity immediately because no long-lived
                        // stream body is handed to the caller.
                        if (!request_stream || result.is_err())
                            && upstream_request_guard.release().await.is_err()
                        {
                            result = Err(runtime_coordination_unavailable_gateway_error());
                        }

                        if result
                            .as_ref()
                            .err()
                            .is_some_and(GatewayError::is_stream_only_recovery_candidate)
                            && stream_only_recovery_leader.is_some()
                            && !stream_only_recovery.consumed
                        {
                            stream_only_recovery.consumed = true;
                            same_route_retry_attempted = true;
                            any_same_route_retry = true;
                            attempt_mode = UpstreamAttemptMode::SseAggregate;
                            continue;
                        }

                        match result {
                            Err(error)
                                if error.error_category() == "runtime_coordination_unavailable" =>
                            {
                                finish_route_health_permit(
                                    &route_health_permit,
                                    RouteOutcome::Cancelled,
                                )
                                .await?;
                                return Err(error);
                            }
                            Ok(mut result) => {
                                let selected_upstream_id = result.selected_upstream_id.clone();
                                let selected_upstream_name = result.selected_upstream_name.clone();
                                let selected_upstream_protocol = result.selected_upstream_protocol;
                                let primary_route = selected_upstream_id == upstream.id
                                    && result.selected_upstream_key_fingerprint == key_fingerprint
                                    && selected_upstream_protocol == protocol;
                                if !primary_route {
                                    finish_route_health_permit(
                                        &route_health_permit,
                                        RouteOutcome::Cancelled,
                                    )
                                    .await?;
                                }
                                if selected_upstream_id != upstream.id
                                    && upstream_request_guard.release().await.is_err()
                                {
                                    return Err(runtime_coordination_unavailable_gateway_error());
                                }
                                state.mark_active_gateway_request_upstream(
                                    &request_id,
                                    &selected_upstream_id,
                                    &selected_upstream_name,
                                );
                                if stream_only_recovery.consumed
                                    && attempt_mode == UpstreamAttemptMode::SseAggregate
                                {
                                    if let Some((profile_key, configuration_fingerprint)) =
                                        stream_only_recovery_identity.as_ref()
                                    {
                                        if let Err(error) = state
                                            .learn_stream_only_route(
                                                profile_key,
                                                &normalized_model,
                                                configuration_fingerprint,
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                request_id = %request_id,
                                                selected_upstream_id = %selected_upstream_id,
                                                error = %error,
                                                "failed to persist learned stream-only route evidence"
                                            );
                                        }
                                    }
                                }
                                if let Some(leader) = stream_only_recovery_leader.take() {
                                    leader.complete();
                                }
                                if capture_route_metadata {
                                    let applied_effort_control =
                                        result.applied_effort_control.as_ref().map(|evidence| {
                                            let value = match &evidence.value {
                                                serde_json::Value::String(text) => text.clone(),
                                                other => other.to_string(),
                                            };
                                            (
                                                evidence.requested.clone(),
                                                evidence.field.clone(),
                                                value,
                                            )
                                        });
                                    append_troubleshooting_route_headers(
                                        &mut result.response_headers,
                                        &selected_upstream_id,
                                        &selected_upstream_name,
                                        &result.selected_upstream_key_fingerprint,
                                        selected_upstream_protocol,
                                        protocol_transition_label(
                                            endpoint,
                                            selected_upstream_protocol,
                                        ),
                                        chat_fallback_stage.map(ChatFallbackStage::as_str),
                                        applied_effort_control.as_ref().map(
                                            |(requested, field, value)| {
                                                (requested.as_str(), field.as_str(), value.as_str())
                                            },
                                        ),
                                        result
                                            .compatibility
                                            .as_ref()
                                            .map(|metadata| metadata.adapter_types.as_slice())
                                            .unwrap_or_default(),
                                    );
                                }
                                // stream=true but upstream returned a non-SSE response:
                                // the gateway synthesizes a finite stream body locally,
                                // so release runtime slots right away.
                                if request_stream
                                    && matches!(result.usage_log_timing, UsageLogTiming::Immediate)
                                {
                                    if upstream_request_guard.release().await.is_err() {
                                        return Err(
                                            runtime_coordination_unavailable_gateway_error(),
                                        );
                                    }
                                    downstream_concurrency_guard.release().await;
                                }

                                result.request_id = request_id.clone();
                                if let Some(stage) = chat_fallback_stage {
                                    result
                                        .compatibility
                                        .get_or_insert_with(CompatibilityUsageMetadata::default)
                                        .fallback_stage = Some(stage.as_str().to_string());
                                }
                                let completed_after_stream_fallback =
                                    request_stream && attempt_mode == UpstreamAttemptMode::Json;
                                if chat_fallback_stage.is_some() {
                                    state.clear_fallback_stage_failures(
                                        &downstream.id,
                                        client_family,
                                        &normalized_model,
                                        &selected_upstream_id,
                                    );
                                }
                                if matches!(result.usage_log_timing, UsageLogTiming::Immediate) {
                                    if let Some(selected_upstream) = routing_snapshot
                                        .upstreams
                                        .iter()
                                        .find(|candidate| candidate.id == selected_upstream_id)
                                    {
                                        if let Some(selected_runtime_model) = selected_upstream
                                            .resolved_model_name_with(
                                                &normalized_model,
                                                case_insensitive,
                                            )
                                        {
                                            clear_runtime_capability_hints_for_success(
                                                &state,
                                                &capability_snapshot,
                                                &requested_features,
                                                inference_strength.as_deref(),
                                                model,
                                                selected_upstream,
                                                &result.selected_upstream_key_fingerprint,
                                                &selected_runtime_model,
                                                selected_upstream_protocol,
                                            );
                                        }
                                    }
                                    if primary_route {
                                        finish_route_health_permit(
                                            &route_health_permit,
                                            RouteOutcome::Success,
                                        )
                                        .await?;
                                    }
                                }
                                if use_routing_affinity {
                                    state.set_affinity_upstream(
                                        &downstream.id,
                                        &normalized_model,
                                        &selected_upstream_id,
                                        runtime_settings.routing_affinity_ttl_seconds,
                                    );
                                }
                                tracing::info!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %selected_upstream_id,
                                    selected_upstream_protocol = ?selected_upstream_protocol,
                                    status = result.status.as_u16(),
                                    latency_ms = started.elapsed().as_millis() as u64,
                                    upstream_attempt_mode = attempt_mode.as_str(),
                                    completed_after_stream_fallback,
                                    "upstream request completed"
                                );
                                if matches!(result.usage_log_timing, UsageLogTiming::Immediate) {
                                    let context = GatewayUsageLogContext {
                                        state: state.clone(),
                                        request_id: request_id.clone(),
                                        downstream_id: downstream.id.clone(),
                                        downstream_name: downstream.name.clone(),
                                        upstream_id: selected_upstream_id,
                                        upstream_name: Some(selected_upstream_name),
                                        endpoint: request_path.to_string(),
                                        model: model.to_string(),
                                        inference_strength: inference_strength.clone(),
                                        user_agent: user_agent.clone(),
                                        compatibility: result.compatibility.clone(),
                                        started,
                                    };
                                    if defer_success_usage_log {
                                        result.usage_log_context = Some(context);
                                    } else if let Err(error) = context
                                        .emit_fail_closed(result.status, None, None, result.usage)
                                        .await
                                    {
                                        downstream_concurrency_guard.release().await;
                                        active_request_guard
                                            .fail_and_finish(error.error_category());
                                        return Err(error);
                                    }
                                }
                                if matches!(
                                    result.usage_log_timing,
                                    UsageLogTiming::DeferredUntilStreamEnd
                                ) {
                                    active_request_guard.disarm();
                                } else {
                                    active_request_guard.finish();
                                }
                                if let Err(error) = account_recovery.finish().await {
                                    downstream_concurrency_guard.release().await;
                                    active_request_guard.fail_and_finish(error.error_category());
                                    return Err(error);
                                }
                                return Ok(result);
                            }
                            Err(error)
                                if runtime_settings.upstream_same_route_retry_enabled
                                    && runtime_settings
                                        .upstream_transient_same_route_retry_enabled
                                    && !same_route_retry_attempted
                                    && !stream_only_recovery.final_attempt
                                    && should_retry_same_route_once(&error) =>
                            {
                                same_route_retry_attempted = true;
                                any_same_route_retry = true;
                                let retry_after = error.retry_after().filter(|d| !d.is_zero());
                                let requested_delay = retry_after
                                    .unwrap_or_else(|| Duration::from_millis(300))
                                    .clamp(Duration::from_millis(200), Duration::from_secs(2));
                                let remaining_budget = route_retry_policy
                                    .remaining_wait_budget(route_retry_budget.waited());
                                let retry_delay = requested_delay.min(remaining_budget);
                                tracing::info!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    upstream_status = error.upstream_status().unwrap_or_default(),
                                    downstream_status = error.status_code().as_u16(),
                                    failure_class = %error.route_failure_class().map(FailureClass::as_str).unwrap_or("unclassified"),
                                    route_action = %"same_route_retry",
                                    same_route_retry = true,
                                    cooldown_seconds = 0,
                                    remaining_candidates = candidate_keys.len().saturating_sub(key_index + 1),
                                    retry_delay_ms = retry_delay.as_millis() as u64,
                                    error_category = %error.error_category(),
                                    "retrying transient upstream failure on the same route"
                                );
                                if !retry_delay.is_zero() {
                                    tokio::time::sleep(retry_delay).await;
                                }
                                route_retry_budget.record_wait_time(retry_delay);
                                request_route_attempts.record_retry_waited(retry_delay);
                                continue;
                            }
                            Err(error)
                                if key_index + 1 < candidate_keys.len()
                                    && !stream_only_recovery.final_attempt
                                    && should_try_next_key(&error) =>
                            {
                                finish_route_health_permit(
                                    &route_health_permit,
                                    route_health_outcome_with_cooldown_cap(
                                        &error,
                                        request_route_attempts
                                            .has_transient_failure_for(&route_health_key),
                                        sole_contract_candidate,
                                        capacity_sole_route_for(protocol),
                                        route_attempt_context.retry_after_cap,
                                        route_attempt_context.retry_after_cooldown_cap,
                                        shared_host_failure_domain(
                                            upstream_host(
                                                &route_attempt_context.route.upstream.base_url,
                                            )
                                            .as_deref(),
                                            &shared_host_candidate_counts,
                                            runtime_settings
                                                .upstream_shared_host_failure_domain_enabled,
                                        ),
                                    ),
                                )
                                .await?;
                                record_route_attempt(route_attempt_context, &error).await?;
                                route_failed = error.route_failure_class().map(|class| {
                                    (class, error.upstream_status(), error.to_string())
                                });
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_name = %upstream.name,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    error_category = %error.error_category(),
                                    "upstream key failed; trying next key"
                                );
                                last_error = Some(error);
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                break;
                            }
                            Err(GatewayError::ConcurrencyFull {
                                message,
                                retry_after,
                                upstream_status,
                            }) => {
                                let retry_after = clamp_upstream_retry_after(
                                    retry_after,
                                    upstream_retry_after_cap,
                                );
                                if stream_only_recovery_leader.is_some()
                                    || stream_only_recovery.consumed
                                {
                                    stream_only_recovery.final_attempt = true;
                                }
                                let retry_after_seconds = retry_after.map(duration_seconds_ceil);
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_name = %upstream.name,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    retry_after_seconds,
                                    "upstream concurrency/capacity response; moving to another route"
                                );
                                if runtime_settings.routing_affinity_enabled {
                                    state
                                        .clear_affinity_upstream(&downstream.id, &normalized_model);
                                }
                                if let Some(retry_after) = retry_after {
                                    if state
                                        .mark_upstream_concurrency_full(
                                            &upstream.id,
                                            retry_after.as_millis().min(u128::from(u64::MAX))
                                                as u64,
                                        )
                                        .await
                                        .is_err()
                                    {
                                        finish_route_health_permit(
                                            &route_health_permit,
                                            RouteOutcome::Cancelled,
                                        )
                                        .await?;
                                        return Err(
                                            runtime_coordination_unavailable_gateway_error(),
                                        );
                                    }
                                }
                                last_error = Some(GatewayError::ConcurrencyFull {
                                    message,
                                    retry_after,
                                    upstream_status,
                                });
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));

                                record_route_attempt(
                                    route_attempt_context,
                                    &GatewayError::ConcurrencyFull {
                                        message: String::new(),
                                        retry_after,
                                        upstream_status,
                                    },
                                )
                                .await?;
                                finish_route_health_permit(
                                    &route_health_permit,
                                    if stream_only_recovery.consumed {
                                        // The aggregate stream probe is an internal capability
                                        // recovery attempt.  A provider-side concurrency response
                                        // describes the probe mode, not the JSON route, so do not
                                        // quarantine the exact route for the next request.
                                        RouteOutcome::Cancelled
                                    } else {
                                        retry_after
                                            .map(|retry_after| {
                                                RouteOutcome::RouteFailureWithRetry {
                                                    class: FailureClass::ConcurrencySaturated,
                                                    retry_after,
                                                    upstream_status,
                                                    repeat_within_request: request_route_attempts
                                                        .has_transient_failure_for(
                                                            &route_health_key,
                                                        ),
                                                    sole_candidate: sole_contract_candidate,
                                                    capacity_sole_route: capacity_sole_route_for(
                                                        protocol,
                                                    ),
                                                    shared_host_failure_domain: false,
                                                }
                                            })
                                            .unwrap_or(RouteOutcome::RouteFailure {
                                                class: FailureClass::ConcurrencySaturated,
                                                upstream_status,
                                                repeat_within_request: request_route_attempts
                                                    .has_transient_failure_for(&route_health_key),
                                                sole_candidate: sole_contract_candidate,
                                                capacity_sole_route: capacity_sole_route_for(
                                                    protocol,
                                                ),
                                                shared_host_failure_domain: false,
                                            })
                                    },
                                )
                                .await?;

                                break;
                            }
                            Err(GatewayError::TooManyRequests {
                                message,
                                retry_after,
                            }) => {
                                let retry_after = retry_after
                                    .unwrap_or_else(|| {
                                        Duration::from_secs(
                                            runtime_settings
                                                .upstream_rate_limit_default_retry_seconds
                                                .max(1),
                                        )
                                    })
                                    .min(upstream_retry_after_cap);
                                let retry_after_seconds = duration_seconds_ceil(retry_after);
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_name = %upstream.name,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    retry_after_seconds,
                                    "upstream rate limited; moving to another route"
                                );
                                if runtime_settings.routing_affinity_enabled {
                                    state
                                        .clear_affinity_upstream(&downstream.id, &normalized_model);
                                }
                                if state
                                    .mark_upstream_rate_limited(&upstream.id, retry_after_seconds)
                                    .await
                                    .is_err()
                                {
                                    finish_route_health_permit(
                                        &route_health_permit,
                                        RouteOutcome::Cancelled,
                                    )
                                    .await?;
                                    return Err(runtime_coordination_unavailable_gateway_error());
                                }
                                last_error = Some(GatewayError::TooManyRequests {
                                    message,
                                    retry_after: Some(retry_after),
                                });
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));

                                record_route_attempt(
                                    route_attempt_context,
                                    &GatewayError::TooManyRequests {
                                        message: String::new(),
                                        retry_after: Some(retry_after),
                                    },
                                )
                                .await?;
                                finish_route_health_permit(
                                    &route_health_permit,
                                    if stream_only_recovery.consumed {
                                        RouteOutcome::Cancelled
                                    } else {
                                        RouteOutcome::RouteFailureWithRetry {
                                            class: FailureClass::RateLimited,
                                            retry_after,
                                            upstream_status: None,
                                            repeat_within_request: request_route_attempts
                                                .has_transient_failure_for(&route_health_key),
                                            sole_candidate: sole_contract_candidate,
                                            capacity_sole_route: capacity_sole_route_for(protocol),
                                            shared_host_failure_domain: false,
                                        }
                                    },
                                )
                                .await?;

                                break;
                            }
                            Err(error @ GatewayError::BadRequest(_)) => {
                                finish_route_health_permit(
                                    &route_health_permit,
                                    RouteOutcome::Success,
                                )
                                .await?;
                                maybe_record_chat_fallback_stage_failure(
                                    &state,
                                    &downstream.id,
                                    client_family,
                                    &normalized_model,
                                    &upstream.id,
                                    chat_fallback_stage,
                                    &error,
                                );
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    error_category = %error.error_category(),
                                    "upstream rejected request payload"
                                );
                                last_error = Some(error);
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                break 'candidate_passes;
                            }
                            Err(error)
                                if error.status_code() == StatusCode::BAD_REQUEST
                                    && !(attempt_mode == UpstreamAttemptMode::SsePassThrough
                                        && should_retry_without_stream(&error)) =>
                            {
                                let class = error.route_failure_class();
                                if class == Some(FailureClass::RequestRejected) {
                                    finish_route_health_permit(
                                        &route_health_permit,
                                        RouteOutcome::Success,
                                    )
                                    .await?;
                                    maybe_record_chat_fallback_stage_failure(
                                        &state,
                                        &downstream.id,
                                        client_family,
                                        &normalized_model,
                                        &upstream.id,
                                        chat_fallback_stage,
                                        &error,
                                    );
                                    tracing::warn!(
                                        request_id = %request_id,
                                        downstream_key_id = %downstream.id,
                                        path = %request_path,
                                        original_model = %model,
                                        normalized_model = %&normalized_model,
                                        selected_upstream_id = %upstream.id,
                                        selected_upstream_protocol = ?protocol,
                                        route_id = %route_id,
                                        error_category = %error.error_category(),
                                        "upstream rejected request payload"
                                    );
                                    last_error = Some(error);
                                    last_failure_upstream =
                                        Some((upstream.id.clone(), Some(upstream.name.clone())));
                                    break 'candidate_passes;
                                }
                                if class.is_some() {
                                    finish_route_health_permit(
                                        &route_health_permit,
                                        route_health_outcome_with_cooldown_cap(
                                            &error,
                                            request_route_attempts
                                                .has_transient_failure_for(&route_health_key),
                                            sole_contract_candidate,
                                            capacity_sole_route_for(protocol),
                                            route_attempt_context.retry_after_cap,
                                            route_attempt_context.retry_after_cooldown_cap,
                                            shared_host_failure_domain(
                                                upstream_host(
                                                    &route_attempt_context.route.upstream.base_url,
                                                )
                                                .as_deref(),
                                                &shared_host_candidate_counts,
                                                runtime_settings
                                                    .upstream_shared_host_failure_domain_enabled,
                                            ),
                                        ),
                                    )
                                    .await?;
                                    record_route_attempt(route_attempt_context, &error).await?;
                                }
                                maybe_record_chat_fallback_stage_failure(
                                    &state,
                                    &downstream.id,
                                    client_family,
                                    &normalized_model,
                                    &upstream.id,
                                    chat_fallback_stage,
                                    &error,
                                );
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    error_category = %error.error_category(),
                                    "upstream rejected request payload"
                                );
                                last_error = Some(error);
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                break;
                            }
                            Err(error)
                                if attempt_mode == UpstreamAttemptMode::SsePassThrough
                                    && should_retry_without_stream(&error) =>
                            {
                                finish_route_health_permit(
                                    &route_health_permit,
                                    route_health_outcome_with_cooldown_cap(
                                        &error,
                                        request_route_attempts
                                            .has_transient_failure_for(&route_health_key),
                                        sole_contract_candidate,
                                        capacity_sole_route_for(protocol),
                                        route_attempt_context.retry_after_cap,
                                        route_attempt_context.retry_after_cooldown_cap,
                                        shared_host_failure_domain(
                                            upstream_host(
                                                &route_attempt_context.route.upstream.base_url,
                                            )
                                            .as_deref(),
                                            &shared_host_candidate_counts,
                                            runtime_settings
                                                .upstream_shared_host_failure_domain_enabled,
                                        ),
                                    ),
                                )
                                .await?;
                                tracing::debug!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    upstream_attempt_mode = attempt_mode.as_str(),
                                    error_category = %error.error_category(),
                                    stream_to_json_recovery = true,
                                    "streaming upstream attempt failed; retrying without stream"
                                );
                                same_route_retry_attempted = true;
                                any_same_route_retry = true;
                                attempt_mode = UpstreamAttemptMode::Json;
                                continue;
                            }
                            Err(GatewayError::TemporaryUpstreamUnavailable(message)) => {
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    "upstream temporarily unavailable, trying next candidate"
                                );
                                finish_route_health_permit(
                                    &route_health_permit,
                                    if stream_only_recovery.consumed {
                                        RouteOutcome::Cancelled
                                    } else {
                                        RouteOutcome::RouteFailure {
                                            class: FailureClass::TransientServer,
                                            upstream_status: None,
                                            repeat_within_request: request_route_attempts
                                                .has_transient_failure_for(&route_health_key),
                                            sole_candidate: sole_contract_candidate,
                                            capacity_sole_route: capacity_sole_route_for(protocol),
                                            shared_host_failure_domain: shared_host_failure_domain(
                                                upstream_host(&upstream.base_url).as_deref(),
                                                &shared_host_candidate_counts,
                                                runtime_settings
                                                    .upstream_shared_host_failure_domain_enabled,
                                            ),
                                        }
                                    },
                                )
                                .await?;
                                record_route_attempt(
                                    route_attempt_context,
                                    &GatewayError::TemporaryUpstreamUnavailable(message.clone()),
                                )
                                .await?;
                                route_failed =
                                    Some((FailureClass::TransientServer, None, message.clone()));
                                last_error =
                                    Some(GatewayError::TemporaryUpstreamUnavailable(message));
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                break;
                            }
                            Err(error) => {
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %&normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_name = %upstream.name,
                                    selected_upstream_protocol = ?protocol,
                                    route_id = %route_id,
                                    upstream_status = error.upstream_status(),
                                    error_category = %error.error_category(),
                                    "upstream request failed"
                                );
                                finish_route_health_permit(
                                    &route_health_permit,
                                    route_health_outcome_with_cooldown_cap(
                                        &error,
                                        request_route_attempts
                                            .has_transient_failure_for(&route_health_key),
                                        sole_contract_candidate,
                                        capacity_sole_route_for(protocol),
                                        route_attempt_context.retry_after_cap,
                                        route_attempt_context.retry_after_cooldown_cap,
                                        shared_host_failure_domain(
                                            upstream_host(
                                                &route_attempt_context.route.upstream.base_url,
                                            )
                                            .as_deref(),
                                            &shared_host_candidate_counts,
                                            runtime_settings
                                                .upstream_shared_host_failure_domain_enabled,
                                        ),
                                    ),
                                )
                                .await?;
                                record_route_attempt(route_attempt_context, &error).await?;
                                route_failed = error.route_failure_class().map(|class| {
                                    (class, error.upstream_status(), error.to_string())
                                });
                                last_error = Some(error);
                                last_failure_upstream =
                                    Some((upstream.id.clone(), Some(upstream.name.clone())));
                                break;
                            }
                        }
                    }
                    if let Some((class, upstream_status, message)) = route_failed {
                        let is_transient = is_common_mode_transient_class(class);
                        let threshold = if class == FailureClass::RequestRejected {
                            runtime_settings.upstream_common_mode_breaker_threshold
                        } else if is_transient {
                            runtime_settings.upstream_common_mode_transient_threshold
                        } else {
                            0
                        };
                        if threshold > 0 && is_common_mode_breaker_class(class) {
                            let host = upstream_host(&upstream.base_url);
                            let retry_after =
                                last_error.as_ref().and_then(GatewayError::retry_after);
                            match common_mode {
                                Some(ref mut streak)
                                    if streak.same_signature(class, upstream_status) =>
                                {
                                    let same_host_counts = is_transient
                                        && runtime_settings
                                            .upstream_common_mode_same_host_transient_enabled;
                                    if streak.route_local_fault(
                                        &route_health_key,
                                        &host,
                                        same_host_counts,
                                    ) {
                                        // Identical failure on the *same route*
                                        // or the *same upstream host* as the
                                        // previous one: route-local fault, not a
                                        // pool-wide signature.  Restart the
                                        // streak from this route.
                                        *streak = CommonModeStreak::new(
                                            class,
                                            upstream_status,
                                            route_health_key.clone(),
                                            host,
                                            retry_after,
                                        );
                                        common_mode_first_message = Some(message);
                                        common_mode_failed_routes = vec![route_health_key.clone()];
                                    } else {
                                        streak.count += 1;
                                        streak.last_route = route_health_key.clone();
                                        streak.last_host = host.clone();
                                        if let Some(seen_host) = host.as_ref() {
                                            if !streak.hosts.iter().any(|seen| seen == seen_host) {
                                                streak.hosts.push(seen_host.clone());
                                            }
                                        }
                                        if retry_after.is_some() {
                                            streak.retry_after = retry_after;
                                        }
                                        common_mode_failed_routes.push(route_health_key.clone());
                                        tracing::debug!("common-mode: class={:?} count={} threshold={} same_host_counts={} host={:?} route_local={} tripping={}",
                                            class, streak.count, threshold, same_host_counts, host.as_deref(), streak.route_local_fault(&route_health_key, &host, same_host_counts), streak.count + 1 >= threshold);
                                        if streak.count >= threshold {
                                            common_mode_tripped = true;
                                            let failed_route_count =
                                                common_mode_failed_routes.len();
                                            // P4: snapshot the verdict before the
                                            // replay branch resets `common_mode`, so
                                            // the terminal error can still report the
                                            // common-mode fields.
                                            common_mode_verdict = Some(CommonModeVerdict {
                                                threshold,
                                                failed_route_count,
                                                distinct_hosts: streak.hosts.clone(),
                                                streak_count: streak.count,
                                            });
                                            for failed_route in common_mode_failed_routes.drain(..)
                                            {
                                                state.clear_route_health(&failed_route).await.map_err(
                                                |_| {
                                                    runtime_coordination_unavailable_gateway_error()
                                                },
                                            )?;
                                            }
                                            let first_message = common_mode_first_message
                                                .clone()
                                                .unwrap_or_else(|| message.clone());
                                            let distinct_hosts = streak.hosts.len();
                                            if is_transient && !transient_pool_replay_done {
                                                transient_pool_replay_done = true;
                                                transient_pool_retried = true;
                                                tracing::warn!(
                                                    request_id = %request_id,
                                                    downstream_key_id = %downstream.id,
                                                    path = %request_path,
                                                    original_model = %model,
                                                    normalized_model = %&normalized_model,
                                                    selected_upstream_id = %upstream.id,
                                                    selected_upstream_name = %upstream.name,
                                                    selected_upstream_protocol = ?protocol,
                                                    route_id = %route_id,
                                                    failure_class = class.as_str(),
                                                    upstream_status,
                                                    common_mode_threshold = threshold,
                                                    common_mode_distinct_hosts = distinct_hosts,
                                                    breaker_branch = "transient",
                                                    "transient common-mode failure over distinct hosts: suspected shared upstream gateway outage; replaying one round after a short backoff"
                                                );
                                                let remaining = route_retry_policy
                                                    .remaining_wait_budget(
                                                        route_retry_budget.waited(),
                                                    );
                                                let replay_delay =
                                                    Duration::from_millis(500).min(remaining);
                                                if !replay_delay.is_zero() {
                                                    tokio::time::sleep(replay_delay).await;
                                                }
                                                route_retry_budget
                                                    .record_external_wait(replay_delay);
                                                request_route_attempts
                                                    .record_retry_waited(replay_delay);
                                                // Reset the streak and the attempt
                                                // tracker so the replay round is a
                                                // fresh full pass over the pool; the
                                                // next transient trip with the same
                                                // signature returns the final
                                                // request-level error.
                                                common_mode = None;
                                                common_mode_first_message = None;
                                                common_mode_failed_routes.clear();
                                                request_route_attempts =
                                                    request_route_attempts.next_round();
                                                continue 'routing_rounds;
                                            }
                                            tracing::warn!(
                                                request_id = %request_id,
                                                downstream_key_id = %downstream.id,
                                                path = %request_path,
                                                original_model = %model,
                                                normalized_model = %&normalized_model,
                                                selected_upstream_id = %upstream.id,
                                                selected_upstream_name = %upstream.name,
                                                selected_upstream_protocol = ?protocol,
                                                route_id = %route_id,
                                                failure_class = class.as_str(),
                                                upstream_status,
                                                common_mode_threshold = threshold,
                                                common_mode_distinct_hosts = distinct_hosts,
                                                breaker_branch = if is_transient { "transient-final" } else { "request_shape" },
                                                "common-mode failure detected: stopping route replay and reverting cooldowns"
                                            );
                                            last_error = Some(if is_transient {
                                                common_mode_transient_pool_error(
                                                    &first_message,
                                                    streak,
                                                    threshold,
                                                    failed_route_count,
                                                    transient_pool_retried,
                                                )
                                            } else {
                                                common_mode_breaker_error(
                                                    class,
                                                    upstream_status,
                                                    &first_message,
                                                    streak,
                                                    threshold,
                                                    failed_route_count,
                                                )
                                            });
                                            break 'routing_rounds;
                                        }
                                    }
                                }
                                _ => {
                                    common_mode = Some(CommonModeStreak::new(
                                        class,
                                        upstream_status,
                                        route_health_key.clone(),
                                        host,
                                        retry_after,
                                    ));
                                    common_mode_first_message = Some(message);
                                    common_mode_failed_routes = vec![route_health_key.clone()];
                                }
                            }
                        } else {
                            common_mode = None;
                            common_mode_first_message = None;
                            common_mode_failed_routes.clear();
                        }
                    }
                    if stream_only_recovery.consumed {
                        stream_only_final_attempt = true;
                    }
                    if stream_only_recovery.final_attempt {
                        stream_only_final_attempt = true;
                        break 'candidate_passes;
                    }
                }
            }
        }

        let round_ledger = request_route_attempts.ledger_snapshot();
        let payload_rejected = last_error.as_ref().is_some_and(|error| {
            matches!(error, GatewayError::BadRequest(_))
                || error.route_failure_class() == Some(FailureClass::RequestRejected)
        });
        let round_terminal =
            (!payload_rejected && !stream_only_final_attempt && !round_ledger.is_empty())
                .then(|| round_ledger.terminal_failure());
        let round_recovery = match round_terminal {
            Some(TerminalFailure::Temporary { .. }) => state
                .earliest_temporary_route_recovery(&request_route_attempts.eligible_routes())
                .await
                .map_err(|_| runtime_coordination_unavailable_gateway_error())?,
            _ => None,
        };

        // A3 last-resort probe: a round that made zero physical attempts and
        // skipped every candidate only because of transient-family cooldowns
        // arms the earliest-recovering route, so the next round sends the
        // current request itself as a real half-open probe.  The probe either
        // succeeds (cooldown cleared, request completes), fails through the
        // existing half-open failure path (step capped, request-level repeats
        // suppressed by A1), or is refused by single-flight / the 1s per-route
        // interval, in which case the ordinary decide/terminal flow below
        // applies.  At most one probe is armed per request, and a stale arm
        // that never reached its route is dropped here before re-evaluating.
        // The probe is part of the route-exhaustion retry machinery, so the
        // master `upstream_route_exhaustion_retry_enabled` switch turns it
        // off too: an operator who disables exhaustion retries must not see
        // their request still probed/replayed.
        request_route_attempts.clear_last_resort_probe();
        tracing::debug!(
            "round end: attempts={} all_transient={} avail={} cooled={} all_cooled={}",
            round_ledger.attempt_count(),
            round_ledger.is_all_transient_family_failures(),
            request_route_attempts.available_candidate_count(),
            round_ledger.cooled_candidate_count(),
            round_ledger.is_all_cooled_transient_family(),
        );
        if runtime_settings.upstream_route_exhaustion_retry_enabled
            && runtime_settings.upstream_transient_last_resort_probe_enabled
            && !stream_only_final_attempt
            && round_terminal.is_some()
            && (
                // Original arm: zero physical attempts, every candidate
                // skipped only because of transient-family cooldowns.
                (round_ledger.attempt_count() == 0
                    && round_ledger.is_all_cooled_transient_family())
                // T2.1 arm: the request *did* attempt the whole pool and
                // every physical attempt was a transient-family failure
                // (shared-host aggregated-gateway outage), and by the end of
                // the round no candidate remains available.  The first
                // request to hit a freshly-failing pool gets one real probe
                // instead of waiting for the next request to see the empty
                // pool.  At most one probe per request, as before.
                || (round_ledger.attempt_count() > 0
                    && round_ledger.is_all_transient_family_failures()
                    && request_route_attempts.available_candidate_count() == 0)
            )
            && !request_route_attempts.last_resort_probe_armed()
        {
            if let Some(probe_route) = request_route_attempts.earliest_cooled_route() {
                tracing::info!(
                    request_id = %request_id,
                    downstream_key_id = %downstream.id,
                    path = %request_path,
                    original_model = %model,
                    probe_upstream_id = %probe_route.upstream_id,
                    probe_model_slug = %probe_route.runtime_model_slug,
                    cooled_candidates = round_ledger.cooled_candidate_count(),
                    "arming last-resort half-open probe for the earliest-recovering route"
                );
                request_route_attempts.arm_last_resort_probe(probe_route);
                request_route_attempts = request_route_attempts.next_round();
                continue 'routing_rounds;
            }
        }

        // C3: a round whose only touchpoint was the local pre-dispatch
        // concurrency gate (no physical upstream attempt) is served by
        // queueing for a free slot instead of burning the ConcurrencySaturated
        // budget (32 rounds / 30s).  The account's `max_concurrency` is a hard
        // ceiling on real slots; overflow is *waited out*, not rejected.  The
        // queue is the only wait here: if it gives up (depth limit or wait
        // deadline) the request fast-fails through the terminal flow below
        // rather than falling back to the old reject-and-burn loop.  Gated on
        // the master exhaustion-retry switch too: an operator who disables
        // exhaustion retries wants a quick rejection, not a queued request.
        // The whole round must be local-concurrency-only so the multi-key
        // case (one account full, a sibling account free) keeps its
        // fallback-to-sibling behaviour instead of parking behind the full
        // account.
        if round_terminal.is_some()
            && round_ledger.is_pure_concurrency_exhaustion()
            && runtime_settings.upstream_route_exhaustion_retry_enabled
            && runtime_settings.upstream_account_queue_enabled
            && !payload_rejected
            && !stream_only_final_attempt
        {
            if let Some(account_key) = last_local_concurrency_account.as_ref() {
                let upstream_for_slot = routing_snapshot
                    .upstreams
                    .iter()
                    .find(|candidate| candidate.id == account_key.upstream_id);
                if let Some(upstream_for_slot) = upstream_for_slot {
                    // E4.2: the queue budget is adaptive — clamp(p95_hold ×
                    // 1.5, floor = upstream_account_queue_max_wait_ms,
                    // ceiling) — and the queue is skipped entirely when the
                    // median observed hold already exceeds that budget
                    // ("the first slot would free after the deadline, so
                    // waiting silently is pointless; fast-fail now").
                    let (queue_budget_ms, skip_queue) =
                        if runtime_settings.upstream_account_queue_adaptive_budget_enabled {
                            state.local_slot_queue_plan(account_key)
                        } else {
                            (runtime_settings.upstream_account_queue_max_wait_ms, false)
                        };
                    if skip_queue {
                        tracing::info!(
                            request_id = %request_id,
                            upstream_id = %account_key.upstream_id,
                            queue_budget_ms,
                            "local concurrency queue skipped: median hold exceeds the adaptive budget (E4.2)"
                        );
                    } else if wait_for_local_slot_free(
                        &state,
                        upstream_for_slot,
                        account_key,
                        runtime_settings.upstream_account_queue_max_depth,
                        queue_budget_ms,
                        &request_id,
                    )
                    .await?
                    {
                        tracing::info!(
                            request_id = %request_id,
                            downstream_key_id = %downstream.id,
                            path = %request_path,
                            original_model = %model,
                            normalized_model = %&normalized_model,
                            upstream_id = %account_key.upstream_id,
                            routing_round = request_route_attempts.routing_round(),
                            "local concurrency queue hit: re-running the routing round"
                        );
                        request_route_attempts = request_route_attempts.next_round();
                        continue 'routing_rounds;
                    }
                    // Queue gave up (depth limit / deadline): fast-fail with
                    // the local-gate terminal error instead of burning the
                    // ConcurrencySaturated budget.
                    if runtime_settings.upstream_local_gate_distinct_error_code_enabled {
                        last_error = Some(local_gate_concurrency_saturated_error(
                            "upstream request concurrency capacity is full",
                            state.local_account_lease_count(account_key),
                            upstream_for_slot.max_concurrency,
                            state.local_account_stale_lease_count(
                                account_key,
                                Duration::from_millis(
                                    runtime_settings.upstream_lease_stale_after_ms,
                                ),
                            ),
                            state.local_slot_waiter_count(account_key),
                            0,
                            last_error
                                .as_ref()
                                .and_then(|error| error.retry_after())
                                .map(duration_seconds_ceil)
                                .unwrap_or(1),
                            runtime_settings.upstream_local_gate_max_wait_ms,
                        ));
                        request_route_attempts.set_give_up_reason(GiveUpReason::LocalGateExhausted);
                        local_gate_fast_failed = true;
                    }
                    break 'routing_rounds;
                }
            }
        }

        // C4.1: a round served entirely by the local pre-dispatch concurrency
        // gate (zero physical upstream attempts) fast-fails instead of burning
        // the ConcurrencySaturated budget (32 rounds / 30s).  The C3 queue
        // above already had its chance to park the request behind a real slot;
        // reaching here means the queue is disabled, unavailable, or gave up —
        // there is no evidence a blind retry will help, only the accounting
        // cost of waiting out 30s.  `upstream_local_gate_max_wait_ms` bounds
        // this scenario (the fast-fail realises that bound as an immediate
        // rejection).  Pending account-recovery probes are excluded: they are
        // evidence-backed and waited out by the branch below instead.  A round
        // that only ever saw route-health cooling (no local-gate rejection
        // this round) is NOT a local-gate verdict: the route is in a
        // ConcurrencySaturated recovery with a real probe/cooldown in flight,
        // and the retry-decision budget below is the right place to wait for
        // it — fast-failing there would break the shared-probe recovery.
        if round_terminal.is_some()
            && round_ledger.is_pure_concurrency_exhaustion()
            && request_route_attempts.physical_attempt_count() == 0
            && last_local_concurrency_account.is_some()
            && runtime_settings.upstream_local_gate_fast_fail_enabled
            && !account_recovery.has_pending_recovery()
        {
            tracing::info!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %&normalized_model,
                routing_round = request_route_attempts.routing_round(),
                local_gate_max_wait_ms = runtime_settings.upstream_local_gate_max_wait_ms,
                "local concurrency gate exhausted the round with zero physical attempts: fast-failing"
            );
            if runtime_settings.upstream_local_gate_distinct_error_code_enabled {
                let (in_flight, stale_lease_count, queue_depth, max_concurrency) =
                    match last_local_concurrency_account.as_ref() {
                        Some(account_key) => {
                            let upstream_for_slot = routing_snapshot
                                .upstreams
                                .iter()
                                .find(|candidate| candidate.id == account_key.upstream_id);
                            (
                                state.local_account_lease_count(account_key),
                                state.local_account_stale_lease_count(
                                    account_key,
                                    Duration::from_millis(
                                        runtime_settings.upstream_lease_stale_after_ms,
                                    ),
                                ),
                                state.local_slot_waiter_count(account_key),
                                upstream_for_slot
                                    .map(|upstream| upstream.max_concurrency)
                                    .unwrap_or(0),
                            )
                        }
                        None => (0, 0, 0, 0),
                    };
                last_error = Some(local_gate_concurrency_saturated_error(
                    "upstream request concurrency capacity is full",
                    in_flight,
                    max_concurrency,
                    stale_lease_count,
                    queue_depth,
                    0,
                    last_error
                        .as_ref()
                        .and_then(|error| error.retry_after())
                        .map(duration_seconds_ceil)
                        .unwrap_or(1),
                    runtime_settings.upstream_local_gate_max_wait_ms,
                ));
                request_route_attempts.set_give_up_reason(GiveUpReason::LocalGateExhausted);
                local_gate_fast_failed = true;
            }
            break 'routing_rounds;
        }

        if round_terminal.is_some()
            && round_ledger.is_pure_concurrency_exhaustion()
            && account_recovery.has_pending_recovery()
        {
            match account_recovery.wait_for_pending_account().await {
                Ok(true) => {
                    request_route_attempts = request_route_attempts.next_round();
                    continue 'routing_rounds;
                }
                Ok(false) => {}
                Err(error) if error.error_category() == "runtime_coordination_unavailable" => {
                    return Err(error);
                }
                Err(error) => {
                    last_error = Some(error);
                    break 'routing_rounds;
                }
            }
        }

        let retry_decision = round_terminal.map(|failure| {
            route_retry_policy.decide_with_reason(
                &route_retry_budget,
                failure,
                round_recovery,
                round_ledger.is_pure_client_rate_limit(),
                // A round that attempted nothing and skipped every candidate
                // only because of half-open exclusive windows waits on its own
                // busy budget, not the ordinary round cap (T3).
                round_ledger.attempt_count() == 0 && round_ledger.is_all_half_open_busy(),
                &request_id,
            )
        });
        if let Some((wait, give_up_reason)) = retry_decision {
            if let Some(reason) = give_up_reason {
                // The retry loop stops here: record why, for the terminal
                // error details and stream diagnostics (A5).
                request_route_attempts.set_give_up_reason(reason);
            }
            if let Some(wait) = wait {
                log_route_retry_wait(
                    &request_id,
                    &request_route_attempts,
                    &route_retry_budget,
                    wait,
                    round_recovery,
                );
                tokio::time::sleep(wait.sleep_for).await;
                route_retry_budget.record_wait(wait);
                request_route_attempts.record_retry_waited(wait.sleep_for);
                request_route_attempts = request_route_attempts.next_round();
                continue 'routing_rounds;
            }
        }
        // ── P2 continuation-pin escape ────────────────────────────────
        // The routing rounds are about to give up.  When this request is
        // bound to a continuation pin and the failure is a plain transient /
        // mixed exhaustion (NOT a request-shape rejection and NOT a pure
        // client rate limit, B3), run one extra pass with the pin constraint
        // relaxed and the history sanitized for cross-provider replay.  At
        // most one escape per request; a success re-pins the continuation to
        // the new route via the existing store-back path.
        let escape_eligible = runtime_settings.upstream_continuation_pin_escape_enabled
            && route_profile_constraint_active
            && !chat_only_responses_fallback
            && !continuation_pin_escaped.load(Ordering::Relaxed)
            && !payload_rejected
            && !round_ledger.is_pure_client_rate_limit()
            && matches!(
                round_terminal,
                Some(TerminalFailure::Temporary { .. } | TerminalFailure::MixedRoutesExhausted)
            );
        if escape_eligible {
            let pinned_route_id = continuation_profile_key
                .as_ref()
                .map(|key| key.upstream_id.as_str())
                .unwrap_or("");
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream.id,
                path = %request_path,
                original_model = %model,
                normalized_model = %&normalized_model,
                route_action = "continuation_pin_escape",
                pinned_route_id,
                routing_round = request_route_attempts.routing_round(),
                physical_attempt_count = request_route_attempts.physical_attempt_count(),
                candidate_capacity = candidate_passes.len(),
                "continuation pin escape: relaxing the profile constraint and retrying with a sanitized cross-provider history"
            );
            sanitize_history_for_cross_provider_replay(&mut body);
            continuation_constraint_relaxed.store(true, Ordering::Relaxed);
            continuation_pin_escaped.store(true, Ordering::Relaxed);
            // The continuation lock on candidate protocols is lifted: reuse
            // the unconstrained routing strategy so Messages-pinned
            // continuations can also reach a route on this round.
            candidate_protocols =
                continuation_escape_candidate_protocols(endpoint, responses_strategy);
            candidate_passes = compute_candidate_passes(&candidate_protocols);
            request_route_attempts = request_route_attempts.next_round();
            continue 'routing_rounds;
        }
        break 'routing_rounds;
    }

    if let Some(last_route_error) = last_error {
        let attempt_ledger = request_route_attempts.ledger_snapshot();
        let fallback_upstream_status = last_route_error.upstream_status();
        let fallback_failure_class = last_route_error.route_failure_class();
        let should_aggregate = !local_gate_fast_failed
            && !attempt_ledger.is_empty()
            && (attempt_ledger.distinct_route_count() > 1
                || matches!(
                    last_route_error.route_failure_class(),
                    Some(
                        FailureClass::CapacityUnavailable
                            | FailureClass::TransientServer
                            | FailureClass::RateLimited
                            | FailureClass::KeyQuota
                            | FailureClass::Credentials
                            | FailureClass::ModelUnsupported
                            | FailureClass::FeatureUnsupported
                            | FailureClass::ProtocolUnsupported
                    )
                ));
        let live_recovery = if should_aggregate {
            state
                .earliest_temporary_route_recovery(&request_route_attempts.eligible_routes())
                .await
                .map_err(|_| runtime_coordination_unavailable_gateway_error())?
        } else {
            None
        };
        // P4: the common-mode latch only decides whether the gateway keeps
        // replaying the request — it must never decide how richly the client
        // is told what happened.  Keep the request-level common-mode verdict
        // (its status / code / message are the client contract) and merge both
        // the aggregated T0 routing details and the common-mode fields into
        // its `details` right below.
        let mut error = if common_mode_tripped {
            last_route_error
        } else if should_aggregate {
            terminal_route_failure_error(
                &attempt_ledger,
                request_route_attempts.routing_round(),
                route_retry_budget
                    .waited()
                    .saturating_add(account_recovery.waited()),
                live_recovery,
                request_route_attempts.physical_attempt_count(),
                runtime_settings.upstream_local_gate_distinct_error_code_enabled,
                request_route_attempts.give_up_reason(),
                request_route_attempts.last_resort_probe_granted(),
                upstream_retry_after_cap,
                continuation_pin_escaped.load(Ordering::Relaxed),
                continuation_profile_key.is_some(),
                candidate_pass_count,
                continuation_route_count,
                request_route_attempts.available_candidate_count(),
            )
        } else {
            last_route_error
        };
        // P4: when the common-mode breaker latched, enrich the terminal
        // error's details with the T0 routing details (attempt_count,
        // routing_rounds, give_up_reason, last_resort_probe_attempted,
        // remaining_candidates, ...) plus the common-mode verdict fields.  The
        // two groups are complementary, not mutually exclusive.
        if common_mode_tripped && should_aggregate {
            let t0 = terminal_route_failure_error(
                &attempt_ledger,
                request_route_attempts.routing_round(),
                route_retry_budget
                    .waited()
                    .saturating_add(account_recovery.waited()),
                live_recovery,
                request_route_attempts.physical_attempt_count(),
                runtime_settings.upstream_local_gate_distinct_error_code_enabled,
                request_route_attempts.give_up_reason(),
                request_route_attempts.last_resort_probe_granted(),
                upstream_retry_after_cap,
                continuation_pin_escaped.load(Ordering::Relaxed),
                continuation_profile_key.is_some(),
                candidate_pass_count,
                continuation_route_count,
                request_route_attempts.available_candidate_count(),
            );
            error = merge_common_mode_terminal_details(
                error,
                &t0,
                common_mode_verdict,
                transient_pool_retried,
            );
        }
        if should_rollback_downstream_reservation(&error) {
            let rollback = state
                .rollback_downstream_request_reservation(downstream_request_reservation)
                .await;
            error = replace_error_on_runtime_rollback_failure(error, rollback);
        }
        if let Err(cleanup_error) = account_recovery.finish().await {
            error = cleanup_error;
        }
        let (upstream_id, upstream_name) = last_failure_upstream
            .as_ref()
            .map(|(id, name)| (id.as_str(), name.as_deref()))
            .unwrap_or(("", None));
        let _ = append_gateway_usage_log(
            &state,
            &request_id,
            &downstream.id,
            &downstream.name,
            upstream_id,
            upstream_name,
            request_path,
            model,
            inference_strength.as_deref(),
            user_agent.as_deref(),
            None,
            error.status_code(),
            Some(error.to_string()),
            Some(error.error_category().to_string()),
            0,
            0,
            0,
            started,
        )
        .await;
        downstream_concurrency_guard.release().await;
        active_request_guard.fail_and_finish(error.error_category());
        let terminal_observation = (!attempt_ledger.is_empty())
            .then(|| attempt_ledger.terminal_observation_for(attempt_ledger.terminal_failure()))
            .flatten();
        let upstream_status = terminal_observation
            .as_ref()
            .and_then(|failure| failure.upstream_status)
            .or(fallback_upstream_status)
            .unwrap_or_default();
        let failure_class = terminal_observation
            .as_ref()
            .map(|failure| failure.class.as_str())
            .or_else(|| fallback_failure_class.map(FailureClass::as_str))
            .unwrap_or("unclassified");
        let cooldown_seconds = terminal_observation
            .as_ref()
            .and_then(|failure| failure.retry_after)
            .map(|duration| {
                duration
                    .as_secs()
                    .saturating_add(u64::from(duration.subsec_nanos() > 0))
            })
            .or_else(|| error.retry_after_seconds())
            .unwrap_or_default();
        let route_id = terminal_observation
            .as_ref()
            .map(|failure| failure.route_id.as_str())
            .unwrap_or("route_unknown");
        // T0.1: surface the give-up reason and the budget arithmetic next to
        // `cooldown_seconds` so an operator can see the dimension clash at a
        // glance (28s cooldown > 30s-waited budget = inevitable WaitBudget).
        // The `distinct_upstream_hosts` field is the T1.4
        // fake-diversity tell: if it stays at 1 while `route_count` is
        // large, the "different routes" are all one aggregated gateway and
        // 502s are common-mode, not per-route.
        let live_recovery_seconds = live_recovery.map(|recovery| {
            duration_seconds_ceil(recovery.half_open_remaining.unwrap_or(recovery.retry_after))
        });
        let give_up_reason = request_route_attempts
            .give_up_reason()
            .map(GiveUpReason::as_str)
            .unwrap_or("none");
        let waited_ms = route_retry_budget.waited().as_millis() as u64;
        let mut error_code_counts = attempt_ledger
            .upstream_error_code_counts()
            .into_iter()
            .collect::<Vec<_>>();
        error_code_counts.sort();
        let upstream_error_codes = error_code_counts
            .into_iter()
            .map(|(code, count)| format!("{code}:{count}"))
            .collect::<Vec<_>>()
            .join(",");
        tracing::error!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %&normalized_model,
            endpoint = %request_path,
            route_id = %route_id,
            upstream_status,
            downstream_status = error.status_code().as_u16(),
            failure_class = %failure_class,
            route_action = %"routes_exhausted",
            same_route_retry = any_same_route_retry,
            cooldown_seconds,
            give_up_reason,
            waited_ms,
            retry_max_wait_ms = runtime_settings.upstream_route_exhaustion_retry_max_wait_ms,
            retry_max_rounds = runtime_settings.upstream_route_exhaustion_retry_max_rounds,
            route_count = attempt_ledger.distinct_route_count(),
            cooled_candidate_count = attempt_ledger.cooled_candidate_count(),
            remaining_candidates = request_route_attempts.available_candidate_count(),
            live_recovery_seconds,
            last_resort_probe_attempted = request_route_attempts.last_resort_probe_granted(),
            routing_round = request_route_attempts.routing_round(),
            account_recovery_rounds = account_recovery.rounds(),
            physical_attempt_count = request_route_attempts.physical_attempt_count(),
            half_open_busy_count = attempt_ledger.half_open_busy_count(),
            error_category = %error.error_category(),
            continuation_pinned = continuation_profile_key.is_some(),
            candidate_pass_count,
            continuation_route_count,
            distinct_upstream_hosts = attempt_ledger.distinct_upstream_host_count(),
            upstream_error_codes = %upstream_error_codes,
            continuation_pin_escaped = continuation_pin_escaped.load(Ordering::Relaxed),
            "request failed after exhausting upstream candidates"
        );
        return Err(error);
    }

    let mut error = no_routable_model_error(&routing_snapshot, model);
    if let Err(cleanup_error) = account_recovery.finish().await {
        error = cleanup_error;
    }
    let _ = append_gateway_usage_log(
        &state,
        &request_id,
        &downstream.id,
        &downstream.name,
        "",
        None,
        request_path,
        model,
        inference_strength.as_deref(),
        user_agent.as_deref(),
        None,
        error.status_code(),
        Some(error.to_string()),
        Some(error.error_category().to_string()),
        0,
        0,
        0,
        started,
    )
    .await;
    tracing::warn!(
        request_id = %request_id,
        downstream_key_id = %downstream.id,
        path = %request_path,
        original_model = %model,
        normalized_model = %&normalized_model,
        endpoint = %request_path,
        "no routable upstream found for request"
    );
    downstream_concurrency_guard.release().await;
    active_request_guard.fail_and_finish(error.error_category());
    // Keep the downstream reservation so the portal reflects that the gateway
    // actually received and processed one request attempt, even if no upstream
    // could be routed.
    Err(error)
}

fn synthesize_stream_body(
    endpoint: EndpointKind,
    final_body: &Value,
) -> Result<Body, GatewayError> {
    match endpoint {
        EndpointKind::ChatCompletions => synthesize_chat_stream_body(final_body),
        EndpointKind::Responses => synthesize_responses_stream_body(final_body),
    }
}

fn synthesize_chat_stream_body(final_body: &Value) -> Result<Body, GatewayError> {
    let choices = final_body
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::Upstream("missing chat choices".into()))?;
    let mut stream_choices = Vec::new();

    for (fallback_index, choice) in choices.iter().enumerate() {
        let choice_index = choice
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(fallback_index);
        let message = choice
            .get("message")
            .or_else(|| choice.get("delta"))
            .ok_or_else(|| GatewayError::Upstream("missing chat message".into()))?;
        let mut delta = serde_json::Map::new();
        delta.insert("role".into(), Value::String("assistant".into()));
        if let Some(content) = message.get("content") {
            delta.insert("content".into(), content.clone());
        }
        if let Some(tool_calls) = message.get("tool_calls") {
            delta.insert("tool_calls".into(), tool_calls.clone());
        }
        if let Some(function_call) = message.get("function_call") {
            delta.insert("function_call".into(), function_call.clone());
        }
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .or_else(|| {
                if delta.get("tool_calls").is_some() || delta.get("function_call").is_some() {
                    Some("tool_calls")
                } else {
                    Some("stop")
                }
            });
        stream_choices.push(json!({
            "index": choice_index,
            "delta": Value::Object(delta),
            "finish_reason": finish_reason
                .map(|value| Value::String(value.to_string()))
                .unwrap_or(Value::Null)
        }));
    }
    let response_id = final_body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl");
    let created_at = final_body
        .get("created")
        .and_then(Value::as_u64)
        .unwrap_or_else(unix_seconds);
    let model = final_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let chunk = json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created_at,
        "model": model,
        "choices": stream_choices
    });
    let chunks = vec![
        Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk))),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    Ok(Body::from_stream(futures_stream::iter(chunks)))
}

fn synthesize_responses_stream_body(final_body: &Value) -> Result<Body, GatewayError> {
    let response_id = final_body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp");
    let created_at = final_body
        .get("created")
        .and_then(Value::as_u64)
        .or_else(|| final_body.get("created_at").and_then(Value::as_u64))
        .unwrap_or_else(unix_seconds);
    let model = final_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut events = vec![json!({
        "type": "response.created",
        "sequence_number": 1,
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": created_at,
            "status": "in_progress",
            "model": model,
            "output": []
        }
    })];
    let mut sequence_number = 2u64;

    if let Some(items) = final_body.get("output").and_then(Value::as_array) {
        for (output_index, item) in items.iter().enumerate() {
            let Some(object) = item.as_object() else {
                continue;
            };
            match object.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let item_id = object.get("id").and_then(Value::as_str).unwrap_or("msg");
                    events.push(json!({
                        "type": "response.output_item.added",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": []
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);

                    let text = extract_plain_text_from_content(object.get("content"));
                    if !text.is_empty() {
                        events.push(json!({
                            "type": "response.output_text.delta",
                            "sequence_number": sequence_number,
                            "response_id": response_id,
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "delta": text
                        }));
                        sequence_number = sequence_number.saturating_add(1);
                    }

                    events.push(json!({
                        "type": "response.output_text.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "text": text
                    }));
                    sequence_number = sequence_number.saturating_add(1);

                    events.push(json!({
                        "type": "response.output_item.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "completed",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": text,
                                "annotations": []
                            }]
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                }
                Some("function_call") => {
                    let item_id = object.get("id").and_then(Value::as_str).unwrap_or("call");
                    let call_id = object
                        .get("call_id")
                        .or_else(|| object.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or(item_id);
                    let name = object.get("name").and_then(Value::as_str).unwrap_or("");
                    let arguments = object
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    events.push(json!({
                        "type": "response.output_item.added",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "status": "in_progress",
                            "call_id": call_id,
                            "name": name,
                            "arguments": ""
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                    if !arguments.is_empty() {
                        events.push(json!({
                            "type": "response.function_call_arguments.delta",
                            "sequence_number": sequence_number,
                            "response_id": response_id,
                            "item_id": item_id,
                            "output_index": output_index,
                            "delta": arguments
                        }));
                        sequence_number = sequence_number.saturating_add(1);
                    }
                    events.push(json!({
                        "type": "response.function_call_arguments.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "item_id": item_id,
                        "output_index": output_index,
                        "name": name,
                        "arguments": arguments
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                    events.push(json!({
                        "type": "response.output_item.done",
                        "sequence_number": sequence_number,
                        "response_id": response_id,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "status": "completed",
                            "call_id": call_id,
                            "name": name,
                            "arguments": arguments
                        }
                    }));
                    sequence_number = sequence_number.saturating_add(1);
                }
                _ => {}
            }
        }
    }

    events.push(json!({
        "type": "response.completed",
        "sequence_number": sequence_number,
        "response": final_body
    }));

    let chunks = events
        .into_iter()
        .map(|event| Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", event))))
        .chain(std::iter::once(Ok(Bytes::from_static(b"data: [DONE]\n\n"))))
        .collect::<Vec<_>>();
    Ok(Body::from_stream(futures_stream::iter(chunks)))
}

fn extract_plain_text_from_content(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };

    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if let Some(piece) = part.as_str() {
                    text.push_str(piece);
                    continue;
                }
                if let Some(piece) = part.get("text").and_then(Value::as_str) {
                    text.push_str(piece);
                }
            }
            text
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn usage_from_body(body: &Value) -> (u64, u64, u64) {
    usage_from_usage_value(body.get("usage").unwrap_or(&Value::Null))
}

fn is_empty_success_response(body: &Value) -> bool {
    // Detect upstream 200 responses that carry no usable output:
    // either the choices/output array is missing or empty, or the
    // message content is an empty string/empty array, and no tokens
    // were billed. This matches third-party relay behavior where
    // Claude non-stream responses come back as `content:""` with
    // `completion_tokens:0` — structurally valid but useless.
    let usage = body.get("usage").unwrap_or(&Value::Null);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if completion_tokens != 0 || output_tokens != 0 {
        return false;
    }

    // ChatCompletions shape: choices[].message.content
    if let Some(choices) = body.get("choices").and_then(Value::as_array) {
        if choices.is_empty() {
            return true;
        }
        for choice in choices {
            let message = choice.get("message").or_else(|| choice.get("delta"));
            if let Some(message) = message {
                if chat_message_has_usable_output(message) {
                    return false;
                }
            }
        }
        return true;
    }

    // Responses shape: output[].content[].text
    if let Some(output) = body.get("output").and_then(Value::as_array) {
        if output.is_empty() {
            return true;
        }
        for item in output {
            if responses_output_item_has_usable_output(item) {
                return false;
            }
        }
        return true;
    }

    // A successful OpenAI-compatible response without either recognized
    // output container has no usable agent output. This also catches bare `{}`
    // and usage-only relay responses.
    true
}

fn has_explicit_zero_output_usage(body: &Value, protocol: UpstreamProtocol) -> bool {
    let usage = body.get("usage").and_then(Value::as_object);
    match protocol {
        UpstreamProtocol::ChatCompletions => {
            usage
                .and_then(|usage| usage.get("completion_tokens"))
                .and_then(Value::as_u64)
                == Some(0)
        }
        UpstreamProtocol::Responses => {
            usage
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(Value::as_u64)
                == Some(0)
        }
    }
}

fn chat_message_has_usable_output(message: &Value) -> bool {
    value_has_non_empty_text(message.get("content"))
        || value_has_non_empty_text(message.get("refusal"))
        || value_has_non_empty_text(message.get("reasoning_content"))
        || non_empty_array(message.get("tool_calls"))
        || value_has_payload(message.get("function_call"))
}

fn responses_output_item_has_usable_output(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => {}
        Some("reasoning") => {
            return value_has_non_empty_text(item.get("summary"))
                || value_has_non_empty_text(item.get("content"))
                || item
                    .get("encrypted_content")
                    .is_some_and(typed_field_has_payload);
        }
        Some(_) => return typed_output_item_has_payload(item),
        None => {}
    }

    value_has_non_empty_text(item.get("content"))
        || non_empty_array(item.get("tool_calls"))
        || value_has_payload(item.get("function_call"))
}

fn typed_output_item_has_payload(item: &Value) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    object.iter().any(|(field, value)| {
        !matches!(
            field.as_str(),
            "type"
                | "id"
                | "status"
                | "object"
                | "created_at"
                | "completed_at"
                | "sequence_number"
                | "output_index"
                | "content_index"
        ) && typed_field_has_payload(value)
    })
}

fn typed_field_has_payload(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => items.iter().any(typed_field_has_payload),
        Value::Object(object) => object.values().any(typed_field_has_payload),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn value_has_non_empty_text(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => items
            .iter()
            .any(|item| value_has_non_empty_text(Some(item))),
        Some(Value::Object(object)) => object
            .get("text")
            .or_else(|| object.get("refusal"))
            .or_else(|| object.get("summary_text"))
            .or_else(|| object.get("reasoning_text"))
            .or_else(|| object.get("reasoning_content"))
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        _ => false,
    }
}

fn non_empty_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

fn value_has_payload(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Null) | None => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(object)) => !object.is_empty(),
        Some(_) => true,
    }
}

fn downstream_secret_from_headers(headers: &HeaderMap) -> Result<String, GatewayError> {
    if let Some(api_key) = headers
        .get(header::HeaderName::from_static("x-api-key"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(api_key.to_string());
    }

    if let Some(api_key) = headers
        .get(header::HeaderName::from_static("api-key"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(api_key.to_string());
    }

    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .ok_or_else(|| {
            GatewayError::Unauthorized("missing authorization header or x-api-key".into())
        })?;

    let mut auth_parts = auth_header.split_whitespace();
    let scheme = auth_parts.next().filter(|value| !value.is_empty());
    let token = auth_parts.next().filter(|value| !value.is_empty());
    if auth_parts.next().is_some() {
        return Err(GatewayError::Unauthorized(
            "invalid authorization header".into(),
        ));
    }

    if scheme
        .map(|scheme| scheme.eq_ignore_ascii_case("bearer"))
        .unwrap_or(false)
    {
        token
            .map(str::to_string)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| GatewayError::Unauthorized("invalid authorization header".into()))
    } else {
        Err(GatewayError::Unauthorized(
            "invalid authorization header".into(),
        ))
    }
}

fn client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HeaderName::from_static("x-forwarded-for"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .map(str::to_string)
        .or_else(|| {
            headers
                .get(header::HeaderName::from_static("x-real-ip"))
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
}

// JWT authentication middleware
async fn admin_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::http::Request<Body>,
    next: axum::middleware::Next,
) -> Result<Response, StatusCode> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;

    crate::auth::verify_admin_token(token, &state.config.jwt_secret)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    Ok(next.run(request).await)
}

#[cfg(test)]
#[path = "../../tests/unit/server/gateway.rs"]
mod tests;
