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
    chat_response_to_responses_payload_with_tool_registry, responses_response_to_chat_payload,
    tool_adapter::{ToolAdapterRegistry, ToolTarget},
    ChatStreamCanonicalizer, ConversionContext, FirstUsableOutputClassifier,
    FirstUsableOutputResult, ProtocolError, StreamAggregateResult, StreamResponseAggregator,
    StreamTranslator,
};
use crate::routing::UpstreamProtocol;
use crate::state::{
    join_upstream_url, portal_model_is_allowed, unix_seconds, ActiveGatewayRequestStart, AppConfig,
    AppState, CompatibilityUsageMetadata, GlobalContextProfile, KeyHealthKey, RouteAvailability,
    RouteHealthKey, RouteHealthPermit, RouteOutcome, RouteSetAggregateKey, UpstreamConfig,
    UsageLog,
};
use crate::upstream_feedback::UpstreamFeedbackClassification;
use axum::body::{Body, BodyDataStream};
use axum::extract::{rejection::JsonRejection, ConnectInfo, Json, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures_util::{stream as futures_stream, FutureExt, StreamExt};
use mime_guess::from_path;
use rust_embed::RustEmbed;
use serde_json::{json, Map, Value};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Mutex as TokioMutex};
use tokio::time::Instant as TokioInstant;
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

mod capability_admin;
mod capability_probe;
mod capability_routing;
mod claude;
mod compat;
pub(crate) mod compatibility_semantics;
mod context;
mod dialect_retry;
mod errors;
mod responses_fallback;
mod route_attempts;
mod stream;
pub(super) mod thinking_signature;
mod troubleshooting;
mod upstream;

use capability_admin::*;
pub use capability_probe::*;
use capability_routing::*;
use claude::*;
use compat::*;
use context::*;
use errors::*;
use responses_fallback::*;
use route_attempts::*;
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

#[derive(Clone, Debug)]
struct RouteCapabilityEvaluation {
    eligible: bool,
    optional_misses: usize,
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
    )
}

fn build_request_route_capability_cache_with_hints(
    snapshot: &CapabilityRuntimeSnapshot,
    upstreams: &[UpstreamConfig],
    model: &str,
    endpoint: EndpointKind,
    requested: &RequestedFeatures,
    runtime_hints: &RuntimeCapabilityHintSnapshot,
    requested_value: Option<&str>,
) -> BTreeMap<(WireProtocol, String, String), RouteCapabilityEvaluation> {
    let mut cache = BTreeMap::new();
    for upstream in upstreams
        .iter()
        .filter(|upstream| upstream.active && upstream.supports_model(model))
    {
        let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
            continue;
        };
        for api_key in route_api_keys(upstream, &runtime_model_slug) {
            let key_fingerprint = route_key_fingerprint(upstream, &api_key);
            for protocol in upstream.supported_protocols() {
                let resolved = resolve_route_capabilities_with_runtime_hints(
                    snapshot,
                    upstream,
                    &key_fingerprint,
                    model,
                    &runtime_model_slug,
                    protocol,
                    requested,
                    runtime_hints,
                    requested_value,
                );
                let native_file_route_is_valid =
                    !requested.required.contains(&Capability::NativeFileId)
                        || protocol == endpoint.native_protocol();
                let eligible = native_file_route_is_valid && resolved.is_some();
                let optional_misses =
                    resolved
                        .as_ref()
                        .map_or(requested.optional.len(), |resolved| {
                            requested
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
                        resolved,
                    },
                );
            }
        }
    }
    cache
}

fn route_api_keys(upstream: &UpstreamConfig, model: &str) -> Vec<String> {
    let keys = upstream.keys_for_model(model);
    if keys.is_empty() && upstream.api_key_models.is_empty() {
        vec![upstream.api_key.clone()]
    } else {
        keys
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

async fn record_route_attempt(
    state: &AppState,
    route_attempts: &RequestRouteAttempts,
    route_health_key: &RouteHealthKey,
    capability_snapshot: &CapabilityRuntimeSnapshot,
    requested: &RequestedFeatures,
    requested_value: Option<&str>,
    exposed_model_slug: &str,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    error: &GatewayError,
) {
    let Some(class) = error.route_failure_class() else {
        return;
    };
    if class == FailureClass::RequestRejected {
        return;
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
    let retry_after = error.retry_after_seconds().map(Duration::from_secs);
    route_attempts.record_failure_with_status(
        route_health_key,
        class,
        retry_after,
        error.upstream_status(),
    );
    for observation in route_attempts.take_newly_exhausted() {
        state
            .observe_route_set_failure(&observation.key, observation.class, observation.retry_after)
            .await;
    }
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

fn route_health_outcome(error: &GatewayError) -> RouteOutcome {
    let retry_after = error.retry_after_seconds().map(Duration::from_secs);
    match error.route_failure_class() {
        Some(class @ (FailureClass::Credentials | FailureClass::KeyQuota)) => retry_after
            .map(|retry_after| RouteOutcome::KeyFailureWithRetry { class, retry_after })
            .unwrap_or(RouteOutcome::KeyFailure(class)),
        Some(FailureClass::RequestRejected) => RouteOutcome::Success,
        Some(class) => retry_after
            .map(|retry_after| RouteOutcome::RouteFailureWithRetry { class, retry_after })
            .unwrap_or(RouteOutcome::RouteFailure(class)),
        None => RouteOutcome::Cancelled,
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
) {
    let permit = permit.lock().await.take();
    if let Some(permit) = permit {
        permit.finish(outcome).await;
    }
}

fn record_cooled_route_attempt(
    route_attempts: &RequestRouteAttempts,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    class: FailureClass,
    retry_after: Duration,
) {
    route_attempts.record_cooled(AttemptFailure {
        route_id: anonymous_route_id(
            &upstream.id,
            key_fingerprint,
            runtime_model_slug,
            WireProtocol::from(protocol),
        ),
        upstream_status: Some(StatusCode::SERVICE_UNAVAILABLE.as_u16()),
        class,
        retry_after: Some(retry_after.max(Duration::from_secs(1))),
    });
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
    value: String,
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
    ) {
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
        .await;
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
}

impl ActiveGatewayRequestGuard {
    fn new(state: AppState, request_id: String) -> Self {
        Self {
            state,
            request_id,
            active: true,
            aggregate_cancellation_log: None,
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
                    context
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
    fn from_config(config: &AppConfig) -> Self {
        Self {
            keepalive_interval: Duration::from_secs(
                config.upstream_stream_keepalive_interval_seconds.max(1),
            ),
            idle_timeout: Duration::from_secs(config.upstream_stream_idle_timeout_seconds.max(1)),
            max_duration: Duration::from_secs(config.upstream_stream_max_duration_seconds.max(1)),
        }
    }
}

fn key_prefix(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 8 {
        key.to_string()
    } else {
        format!("{}...", &key[..8])
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
    error_message: Option<String>,
    error_category: Option<String>,
    started: Instant,
    hedge_control: Option<HedgeAttemptControl>,
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
            .field("error_category", &self.error_category)
            .finish()
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

    async fn emit(self, usage: (u64, u64, u64)) {
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
            error_message,
            error_category,
            started,
            hedge_control: _,
        } = self;

        let log = UsageLog {
            id: request_id.clone(),
            downstream_key_id: downstream_key_id.clone(),
            upstream_key_id: upstream_key_id.clone(),
            downstream_name,
            upstream_name,
            endpoint: endpoint.clone(),
            model: model.clone(),
            inference_strength,
            billing_mode: Some(if usage.2 > 0 {
                "Token 计费".to_string()
            } else {
                "请求计费".to_string()
            }),
            request_count: Some(1),
            user_agent,
            request_id: request_id.clone(),
            status_code: status.as_u16(),
            error_message,
            error_category,
            prompt_tokens: usage.0,
            completion_tokens: usage.1,
            total_tokens: usage.2,
            latency_ms: started.elapsed().as_millis() as u64,
            created_at: unix_seconds(),
            compatibility,
        };

        if let Err(error) = state.append_usage_log(log).await {
            tracing::error!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint,
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream_key_id,
                selected_upstream_protocol = ?upstream_protocol,
                error = %error,
                "failed to save usage log"
            );
        }
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
) {
    let log = UsageLog {
        id: request_id.to_string(),
        downstream_key_id: downstream_id.to_string(),
        upstream_key_id: upstream_id.to_string(),
        downstream_name: Some(downstream_name.to_string()),
        upstream_name: upstream_name.map(str::to_string),
        endpoint: endpoint.to_string(),
        model: model.to_string(),
        inference_strength: inference_strength.map(str::to_string),
        billing_mode: Some(if total_tokens > 0 {
            "Token 计费".to_string()
        } else {
            "请求计费".to_string()
        }),
        request_count: Some(1),
        user_agent: user_agent.map(str::to_string),
        request_id: request_id.to_string(),
        status_code: status_code.as_u16(),
        error_message,
        error_category,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        latency_ms: started.elapsed().as_millis() as u64,
        created_at: unix_seconds(),
        compatibility,
    };

    if let Err(error) = state.append_usage_log(log).await {
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
            "/api/admin/capabilities/profiles",
            get(admin_capability_profiles).route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                admin_auth_middleware,
            )),
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
        // Portal API
        .route("/api/portal/login", post(portal_login))
        .route("/api/portal/overview", get(portal_overview))
        .route("/api/portal/quota", get(portal_quota))
        .route("/api/portal/usage-history", get(portal_usage_history))
        .route("/api/portal/models", get(portal_models))
        .route("/api/portal/model-probe", get(portal_model_probe))
        .route("/api/portal/announcement", get(portal_announcement))
        .route("/api/portal/key", get(portal_get_key))
        .route("/api/portal/key/rotate", post(portal_rotate_key))
        // Frontend assets and SPA fallback
        .fallback(serve_frontend)
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

async fn serve_frontend(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(asset) = FrontendAssets::get(path) {
        let mime_type = from_path(path).first_or_octet_stream().as_ref().to_string();
        return (
            [(header::CONTENT_TYPE, mime_type)],
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
            [(header::CONTENT_TYPE, mime_type)],
            asset.data.into_response(),
        )
            .into_response();
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

async fn healthz() -> impl IntoResponse {
    "ok"
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

    // Codex sends `?client_version=x.y.z` when fetching its model catalog.
    // Return the Codex-compatible `{"models": [ModelInfo]}` shape so Codex
    // can display context-window usage and reasoning levels for custom
    // models served through the gateway.
    if query.client_version.is_some() {
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
}

struct CodexReasoningMetadata {
    supported_levels: Vec<Value>,
    default_level: Value,
    supports_summaries: bool,
}

const CODEX_REASONING_EFFORT_ORDER: [&str; 6] =
    ["minimal", "low", "medium", "high", "xhigh", "max"];

fn codex_reasoning_effort_rank(effort: &str) -> usize {
    CODEX_REASONING_EFFORT_ORDER
        .iter()
        .position(|candidate| *candidate == effort)
        .unwrap_or(CODEX_REASONING_EFFORT_ORDER.len())
}

fn codex_reasoning_description(effort: &str) -> String {
    format!("Use {effort} reasoning effort")
}

fn codex_reasoning_metadata(resolved: &ResolvedCapabilities) -> CodexReasoningMetadata {
    let verified_control = resolved.supports(Capability::ReasoningOutput)
        && resolved.reasoning_control_field.is_some()
        && !resolved.effort_map.is_empty();

    if !verified_control {
        return CodexReasoningMetadata {
            supported_levels: vec![json!({
                "effort": "none",
                "description": "Do not request a configurable reasoning effort"
            })],
            default_level: Value::String("none".into()),
            supports_summaries: false,
        };
    }

    let mut efforts = resolved
        .effort_map
        .keys()
        .filter(|effort| !effort.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    efforts.sort_by(|left, right| {
        codex_reasoning_effort_rank(left)
            .cmp(&codex_reasoning_effort_rank(right))
            .then_with(|| left.cmp(right))
    });

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
        .find(|effort| effort.as_str() == "medium")
        .cloned()
        .unwrap_or_else(|| efforts[0].clone());
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
        supports_summaries: true,
    }
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

    let mut exposed_models = std::collections::BTreeSet::new();
    for upstream in snapshot.upstreams.iter().filter(|u| u.active) {
        for model in upstream.route_models() {
            if downstream.model_allowlist.is_empty()
                || portal_model_is_allowed(&downstream.model_allowlist, &model)
            {
                exposed_models.insert(model);
            }
        }
    }

    let model_infos = exposed_models
        .into_iter()
        .filter_map(|slug| {
            let witness = select_catalog_witness_entry(state, &snapshot.upstreams, &slug)?;
            let context_window = witness
                .capabilities
                .context_window
                .and_then(|limit| i64::try_from(limit).ok());
            let reasoning = codex_reasoning_metadata(&witness.capabilities);
            Some(json!({
                "slug": slug,
                "display_name": slug,
                "description": null,
                "supported_reasoning_levels": reasoning.supported_levels,
                "default_reasoning_level": reasoning.default_level,
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
                "apply_patch_tool_type": witness.capabilities.supports(Capability::CustomTools).then_some("freeform"),
                "supports_parallel_tool_calls": witness.capabilities.supports(Capability::ParallelToolCalls),
                "supports_image_detail_original": false,
                "context_window": context_window,
                "max_context_window": context_window,
                "effective_context_window_percent": 95,
                "additional_speed_tiers": [],
                "service_tiers": [],
                "experimental_supported_tools": [],
                "input_modalities": if witness.capabilities.supports(Capability::ImageHttps) && witness.capabilities.supports(Capability::ImageDataUrl) { json!(["text", "image"]) } else { json!(["text"]) },
                "gateway_catalog_witness": witness.diagnostic(),
            }))
        })
        .collect::<Vec<_>>();

    Json(json!({ "models": model_infos })).into_response()
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => {
            return GatewayError::BadRequest("invalid json request body".into()).into_response();
        }
    };
    let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if is_stream {
        return dispatch_streaming_request(state, headers, body, EndpointKind::ChatCompletions)
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
        Err(_) => {
            return GatewayError::BadRequest("invalid json request body".into()).into_response();
        }
    };
    let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if is_stream {
        return dispatch_streaming_request(state, headers, body, EndpointKind::Responses).await;
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
        Err(_) => {
            return GatewayError::BadRequest("invalid json request body".into())
                .into_anthropic_response();
        }
    };
    let claude_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let chat_payload = match claude_messages_to_chat_payload(&body) {
        Ok(payload) => payload,
        Err(message) => return GatewayError::BadRequest(message).into_anthropic_response(),
    };

    match process_gateway_request_inner(
        state,
        headers,
        chat_payload,
        EndpointKind::ChatCompletions,
        true,
        None,
        None,
    )
    .await
    {
        Ok(result) => dispatch_claude_success(result, claude_stream).await,
        Err(error) => error.into_anthropic_response(),
    }
}

async fn claude_count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => {
            return GatewayError::BadRequest("invalid json request body".into())
                .into_anthropic_response();
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

struct DownstreamConcurrencyGuardInner {
    state: AppState,
    downstream_id: String,
    released: AtomicBool,
}

impl DownstreamConcurrencyGuardInner {
    fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            self.state
                .release_downstream_concurrency(&self.downstream_id);
        }
    }
}

impl Drop for DownstreamConcurrencyGuardInner {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Clone)]
struct DownstreamConcurrencyGuard {
    inner: Arc<DownstreamConcurrencyGuardInner>,
}

impl DownstreamConcurrencyGuard {
    fn new(state: AppState, downstream_id: String) -> Self {
        Self {
            inner: Arc::new(DownstreamConcurrencyGuardInner {
                state,
                downstream_id,
                released: AtomicBool::new(false),
            }),
        }
    }

    fn release(&self) {
        self.inner.release();
    }
}

struct UpstreamRequestGuardInner {
    state: AppState,
    upstream_id: String,
    released: AtomicBool,
}

impl UpstreamRequestGuardInner {
    fn spawn_release(&self) -> Option<tokio::task::JoinHandle<()>> {
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(
                    upstream_id = %self.upstream_id,
                    error = %error,
                    "upstream request guard dropped outside Tokio runtime"
                );
                return None;
            }
        };
        if self.released.swap(true, Ordering::AcqRel) {
            return None;
        }
        let state = self.state.clone();
        let upstream_id = self.upstream_id.clone();
        Some(runtime.spawn(async move {
            state.release_upstream_request(&upstream_id).await;
        }))
    }
}

impl Drop for UpstreamRequestGuardInner {
    fn drop(&mut self) {
        drop(self.spawn_release());
    }
}

#[derive(Clone)]
struct UpstreamRequestGuard {
    inner: Arc<UpstreamRequestGuardInner>,
}

impl UpstreamRequestGuard {
    fn new(state: AppState, upstream_id: String) -> Self {
        Self {
            inner: Arc::new(UpstreamRequestGuardInner {
                state,
                upstream_id,
                released: AtomicBool::new(false),
            }),
        }
    }

    async fn release(&self) {
        if let Some(task) = self.inner.spawn_release() {
            if let Err(error) = task.await {
                tracing::error!(
                    upstream_id = %self.inner.upstream_id,
                    error = %error,
                    "upstream request release task failed"
                );
            }
        }
    }
}

#[derive(Clone)]
struct UpstreamRequestReservation {
    guard: Arc<TokioMutex<Option<UpstreamRequestGuard>>>,
}

impl UpstreamRequestReservation {
    fn new(guard: UpstreamRequestGuard) -> Self {
        Self {
            guard: Arc::new(TokioMutex::new(Some(guard))),
        }
    }

    async fn release(&self) {
        let guard = self.guard.lock().await.take();
        if let Some(guard) = guard {
            guard.release().await;
        }
    }

    async fn reserve_next(
        &self,
        state: &AppState,
        upstream: &UpstreamConfig,
        model: &str,
    ) -> Result<(), GatewayError> {
        self.release().await;
        state
            .try_reserve_upstream_request(upstream, model)
            .await
            .map_err(|_| {
                GatewayError::Upstream(
                    "failed to reserve capacity for an internal upstream retry".into(),
                )
            })?;
        *self.guard.lock().await = Some(UpstreamRequestGuard::new(
            state.clone(),
            upstream.id.clone(),
        ));
        Ok(())
    }
}

#[derive(Clone)]
struct StreamCompletionContext {
    state: AppState,
    upstream_id: String,
    route_health_key: RouteHealthKey,
    route_attempts: RequestRouteAttempts,
    route_health_permit: Arc<TokioMutex<Option<RouteHealthPermit>>>,
    upstream_request_guard: UpstreamRequestReservation,
    downstream_concurrency_guard: DownstreamConcurrencyGuard,
    hedge_control: Option<HedgeAttemptControl>,
}

impl StreamCompletionContext {
    async fn release_all(&self) {
        if !self.is_hedge_loser() {
            self.downstream_concurrency_guard.release();
        }
        self.upstream_request_guard.release().await;
    }

    fn is_hedge_loser(&self) -> bool {
        self.hedge_control
            .as_ref()
            .is_some_and(HedgeAttemptControl::is_loser)
    }

    async fn mark_success(&self) {
        finish_route_health_permit(&self.route_health_permit, RouteOutcome::Success).await;
        if let Err(error) = self.state.mark_upstream_success(&self.upstream_id).await {
            tracing::warn!(
                selected_upstream_id = %self.upstream_id,
                error = %error,
                "failed to reset legacy upstream failure count after stream success"
            );
        }
    }

    async fn mark_failure(&self) {
        finish_route_health_permit(
            &self.route_health_permit,
            RouteOutcome::UncertainRouteFailure(FailureClass::Transport),
        )
        .await;
        self.route_attempts
            .record_failure(&self.route_health_key, FailureClass::Transport, None);
        for observation in self.route_attempts.take_newly_exhausted() {
            self.state
                .observe_route_set_failure(
                    &observation.key,
                    observation.class,
                    observation.retry_after,
                )
                .await;
        }
        if let Err(error) = self.state.mark_upstream_failure(&self.upstream_id).await {
            tracing::warn!(
                selected_upstream_id = %self.upstream_id,
                error = %error,
                "failed to update legacy upstream failure count after stream failure"
            );
        }
    }

    async fn mark_cancelled(&self) {
        finish_route_health_permit(&self.route_health_permit, RouteOutcome::Cancelled).await;
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
            stream_drop_interruption_message(false),
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
static PRE_HEADER_PREPARATION_TEST_GATE: Mutex<Option<PreHeaderPreparationTestGate>> =
    Mutex::new(None);

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
async fn wait_on_pre_header_preparation_test_gate() {
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
    history_input_items: Vec<Value>,
    history_request_state: Map<String, Value>,
    tool_registry: Option<ToolAdapterRegistry>,
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
            history_input_items: self.history_input_items.clone(),
            history_request_state,
            tool_registry: self.tool_registry.clone(),
        })
    }

    fn tool_registry(&self) -> Option<&ToolAdapterRegistry> {
        self.tool_registry.as_ref()
    }

    fn set_tool_registry(&mut self, registry: ToolAdapterRegistry) {
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
                        .and_then(Value::as_object)
                        .and_then(|profile| profile.get("upstream_id"))
                })
            })
            .and_then(Value::as_str)
    }

    fn exact_continuation_state(&self) -> Result<Option<GatewayContinuationState>, GatewayError> {
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
        if !continuation.validate_version() {
            return Err(response_history_invalid(
                "cached gateway continuation version is unsupported",
            ));
        }
        Ok(Some(continuation))
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

    fn has_continuation_state(&self) -> bool {
        self.history_request_state
            .contains_key("_gateway_continuation")
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
        self.state
            .store_response_history(response_id.to_string(), items, request_state);
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
                Some("function_call" | "function_call_output")
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
    body: &mut Value,
) -> Result<ResponseHistoryContext, GatewayError> {
    prepare_response_history_context_with_replay(state, body, true).await
}

async fn prepare_response_history_context_with_replay(
    state: &AppState,
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
            .response_history(previous_response_id)
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
        history_input_items: effective_input_items,
        history_request_state,
        tool_registry,
    })
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
                Some("function_call" | "function_call_output")
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

/// Build a discriminative interruption message for the Drop path based on
/// how far the stream progressed before the downstream client closed.
/// Splits the catch-all `stream_interrupted` bucket into
/// `stream_client_cancelled` (no output yet) and `stream_incomplete_close`
/// (some output received but not completed) for actionable 499 triage.
fn stream_drop_interruption_message(usable_output_seen: bool) -> String {
    if usable_output_seen {
        "client disconnected during stream (partial output received)".to_string()
    } else {
        "client disconnected before any upstream output".to_string()
    }
}

fn classify_upstream_stream_error(
    error_message: &str,
    is_timeout: bool,
    is_decode: bool,
) -> (StatusCode, &'static str) {
    let normalized = error_message.to_ascii_lowercase();
    if is_timeout || normalized.contains("timed out") || normalized.contains("timeout") {
        (StatusCode::GATEWAY_TIMEOUT, "stream_upstream_timeout")
    } else if is_decode || normalized.contains("error decoding response body") {
        (StatusCode::BAD_GATEWAY, "stream_upstream_body_decode_error")
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
    mark_upstream_failure: bool,
) {
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
        if mark_upstream_failure {
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
        log_context.status = status;
        log_context.error_message = Some(error_message);
        log_context.error_category = Some(error_category.to_string());
        log_context.emit(usage.unwrap_or((0, 0, 0))).await;
    }
}

async fn finalize_stream_interruption(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    error_message: String,
) {
    let (status, error_category) = classify_stream_failure(&error_message);
    let mark_upstream_failure = status != StatusCode::from_u16(499).expect("valid status code");
    finalize_stream_error(
        completion_context,
        log_context,
        usage,
        status,
        error_category,
        error_message,
        mark_upstream_failure,
    )
    .await;
}

fn spawn_stream_interruption_cleanup(
    completion_context: Option<StreamCompletionContext>,
    log_context: Option<StreamUsageLogContext>,
    usage: Option<(u64, u64, u64)>,
    error_message: String,
) {
    if completion_context.is_none() && log_context.is_none() {
        return;
    }

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            finalize_stream_interruption(completion_context, log_context, usage, error_message)
                .await;
        });
    } else {
        tracing::warn!("stream cleanup dropped outside runtime; cleanup skipped");
    }
}

/// When a stream finished normally (received [DONE]) but the downstream client
/// disconnected before all pending frames were delivered, finalize as success
/// rather than recording a spurious "stream disconnected" error.
fn spawn_stream_normal_completion_cleanup(
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
                ctx.emit(usage.unwrap_or((0, 0, 0))).await;
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
    process_gateway_request_inner(state, headers, body, endpoint, false, None, None).await
}

async fn process_gateway_request_with_pre_header_cancellation(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    endpoint: EndpointKind,
    request_id: String,
    cancellation: PreHeaderStreamCancellation,
) -> Result<DispatchResult, GatewayError> {
    process_gateway_request_inner(
        state,
        headers,
        body,
        endpoint,
        false,
        Some(cancellation),
        Some(request_id),
    )
    .await
}

#[allow(unused_assignments)]
async fn process_gateway_request_inner(
    state: AppState,
    headers: HeaderMap,
    mut body: Value,
    endpoint: EndpointKind,
    defer_success_usage_log: bool,
    pre_header_cancellation: Option<PreHeaderStreamCancellation>,
    request_id: Option<String>,
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
            append_gateway_usage_log(
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
    let normalized_model = model;
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
        normalized_model = %normalized_model,
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
                normalized_model = %normalized_model,
                expires_at,
                "downstream key expired"
            );
            let error =
                GatewayError::gateway_forbidden("downstream key expired", "gateway_key_expired");
            append_gateway_usage_log(
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
                normalized_model = %normalized_model,
                client_ip = %client_ip,
                "client IP not allowed"
            );
            let error = GatewayError::gateway_forbidden("ip not allowed", "gateway_ip_not_allowed");
            append_gateway_usage_log(
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
            normalized_model = %normalized_model,
            "model not allowed"
        );
        let error =
            GatewayError::gateway_forbidden("model not allowed", "gateway_model_not_allowed");
        append_gateway_usage_log(
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
        append_gateway_usage_log(
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

    if let Err(rejection) = state.reserve_downstream_request(&downstream).await {
        let retry_after_seconds = rejection.retry_after_seconds();
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            retry_after_seconds,
            "downstream request admission rejected"
        );
        let error = GatewayError::downstream_admission_rejection(rejection);
        append_gateway_usage_log(
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

    if let Err(retry_after_seconds) = state.try_reserve_downstream_concurrency(&downstream) {
        state
            .rollback_downstream_request_reservation(&downstream.id)
            .await;
        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            retry_after_seconds,
            max_concurrency = downstream.max_concurrency,
            "downstream concurrency limit exceeded"
        );
        let error = GatewayError::classified(
            StatusCode::TOO_MANY_REQUESTS,
            "downstream concurrency limit exceeded",
            "gateway_quota_exceeded",
            "gateway_concurrency_full",
            "gateway_concurrency_full",
            Some(retry_after_seconds),
            Some(json!({
                "scope": "gateway",
                "quota": "concurrent_requests",
                "limit": downstream.max_concurrency.max(1),
                "retry_after_seconds": retry_after_seconds,
            })),
        );
        append_gateway_usage_log(
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
    let downstream_concurrency_guard =
        DownstreamConcurrencyGuard::new(state.clone(), downstream.id.clone());

    let original_responses_body = (endpoint == EndpointKind::Responses).then(|| body.clone());
    let mut response_history_context = if endpoint == EndpointKind::Responses {
        match prepare_response_history_context(&state, &mut body).await {
            Ok(context) => Some(context),
            Err(error) => {
                append_gateway_usage_log(
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
            if context.tool_registry().is_none() && !context.has_continuation_state() {
                if let Some(tools) = body.get("tools").and_then(Value::as_array) {
                    if let Ok(adaptation) = ToolAdapterRegistry::build(
                        &Value::Array(tools.clone()),
                        ToolTarget::FunctionsOnly,
                    ) {
                        context.set_tool_registry(adaptation.registry);
                    }
                }
            }
        }
    }

    let responses_upstream_available = endpoint == EndpointKind::Responses
        && routing_snapshot.upstreams.iter().any(|upstream| {
            upstream.active
                && upstream.supports_protocol(UpstreamProtocol::Responses)
                && upstream.supports_model(model)
        });
    let chat_only_responses_fallback =
        endpoint == EndpointKind::Responses && !responses_upstream_available;
    let requires_responses_tooling =
        endpoint == EndpointKind::Responses && responses_request_requires_responses_upstream(&body);
    let fallback_to_chat = requires_responses_tooling && chat_only_responses_fallback;
    let client_family = infer_client_family(user_agent.as_deref(), endpoint);
    if requires_responses_tooling {
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            stream = request_stream,
            routing_fallback = fallback_to_chat,
            routing_fallback_reason = if fallback_to_chat {
                "no_responses_upstream_supports_model"
            } else {
                "responses_upstream_available"
            },
            "evaluated Responses routing strategy"
        );
    }

    let upstream_runtime_snapshots = state.upstream_runtime_snapshots().await;
    let exact_continuation = response_history_context
        .as_ref()
        .map(ResponseHistoryContext::exact_continuation_state)
        .transpose()?
        .flatten();
    if exact_continuation.as_ref().is_some_and(|continuation| {
        !continuation.has_protocol_transition(
            WireProtocol::from(endpoint.native_protocol()),
            continuation.profile_key().protocol,
        )
    }) {
        return Err(response_history_invalid(
            "cached gateway continuation adapter identity is incompatible",
        ));
    }
    if exact_continuation.as_ref().is_some_and(|continuation| {
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
    if let Some(continuation) = exact_continuation.as_ref() {
        continuation.apply_to_requested(&mut requested_features);
    }
    let required_capabilities = requested_features.required.clone();
    let capability_snapshot = state.capability_snapshot();
    if exact_continuation.as_ref().is_some_and(|continuation| {
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
    if exact_continuation
        .as_ref()
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
        model,
        endpoint,
        &requested_features,
        &runtime_capability_hints,
        inference_strength.as_deref(),
    );
    let route_capability =
        |upstream: &UpstreamConfig, key_fingerprint: &str, protocol: UpstreamProtocol| {
            route_capability_cache.get(&(
                WireProtocol::from(protocol),
                upstream.id.clone(),
                key_fingerprint.to_string(),
            ))
        };
    let legacy_continuation_profile =
        if let Some(upstream_id) = legacy_continuation_upstream_id.as_deref() {
            let mut eligible_profiles = Vec::new();
            for upstream in routing_snapshot.upstreams.iter().filter(|upstream| {
                upstream.active && upstream.id == upstream_id && upstream.supports_model(model)
            }) {
                let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                    continue;
                };
                for api_key in route_api_keys(upstream, &runtime_model_slug) {
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
    let codex_catalog_allowed_profiles = (endpoint == EndpointKind::Responses
        && client_family == "codex"
        && exact_continuation.is_none()
        && legacy_continuation_upstream_id.is_none()
        && (!requested_features.required.is_empty() || inference_strength.is_some()))
    .then(|| {
        let witness = select_catalog_witness_entry(&state, &routing_snapshot.upstreams, model)?;
        let mut allowed = BTreeSet::from([witness.profile_key.clone()]);
        let witness_transition =
            ProtocolTransitionIdentity::new(WireProtocol::Responses, witness.profile_key.protocol);
        for upstream in routing_snapshot
            .upstreams
            .iter()
            .filter(|upstream| upstream.active && upstream.supports_model(model))
        {
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                continue;
            };
            for api_key in route_api_keys(upstream, &runtime_model_slug) {
                let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                for protocol in upstream.supported_protocols() {
                    let Some(candidate) = route_capability(upstream, &key_fingerprint, protocol)
                        .and_then(|route| route.resolved.as_ref())
                    else {
                        continue;
                    };
                    let candidate_transition = ProtocolTransitionIdentity::new(
                        WireProtocol::Responses,
                        WireProtocol::from(protocol),
                    );
                    if is_compatible_catalog_superset(
                        candidate,
                        &witness.capabilities,
                        candidate_transition,
                        witness_transition,
                    ) {
                        allowed.insert(DialectProfileKey::for_key(
                            upstream.id.clone(),
                            key_fingerprint.clone(),
                            runtime_model_slug.clone(),
                            WireProtocol::from(protocol),
                        ));
                    }
                }
            }
        }
        Some(allowed)
    })
    .flatten();
    let continuation_profile_key = exact_continuation
        .as_ref()
        .map(|continuation| continuation.profile_key().clone())
        .or(legacy_continuation_profile);
    let route_profile_constraint_active =
        continuation_profile_key.is_some() || codex_catalog_allowed_profiles.is_some();
    let route_matches_profile_constraint =
        |upstream: &UpstreamConfig, key_fingerprint: &str, protocol: UpstreamProtocol| {
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                return false;
            };
            let candidate_key = DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint,
                runtime_model_slug,
                WireProtocol::from(protocol),
            );
            if let Some(profile_key) = continuation_profile_key.as_ref() {
                return candidate_key == *profile_key;
            }
            codex_catalog_allowed_profiles
                .as_ref()
                .is_none_or(|allowed| allowed.contains(&candidate_key))
        };
    let claude_replay_route = claude_thinking_replay_route(
        &state,
        &capability_snapshot,
        &routing_snapshot.upstreams,
        model,
        &body,
    );
    if claude_replay_route == ClaudeThinkingReplayRoute::InvalidOrUnavailable {
        let error =
            GatewayError::BadRequest("invalid or unavailable Claude thinking replay route".into());
        append_gateway_usage_log(
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
        if should_rollback_downstream_reservation(&error) {
            state
                .rollback_downstream_request_reservation(&downstream.id)
                .await;
        }
        downstream_concurrency_guard.release();
        active_request_guard.fail_and_finish(error.error_category());
        return Err(error);
    }
    let required_route_available = if route_profile_constraint_active {
        routing_snapshot.upstreams.iter().any(|upstream| {
            if !upstream.active || !upstream.supports_model(model) {
                return false;
            }
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                return false;
            };
            route_api_keys(upstream, &runtime_model_slug)
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
                    && upstream.supports_model(model)
                    && upstream.supports_protocol(*protocol)
                    && route_capability(upstream, key_fingerprint, *protocol)
                        .is_some_and(|route| route.eligible)
            }),
            ClaudeThinkingReplayRoute::NoReplay => {
                let has_configured_route = routing_snapshot
                    .upstreams
                    .iter()
                    .any(|upstream| upstream.active && upstream.supports_model(model));
                !has_configured_route
                    || routing_snapshot.upstreams.iter().any(|upstream| {
                        if !upstream.active || !upstream.supports_model(model) {
                            return false;
                        }
                        let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                            return false;
                        };
                        route_api_keys(upstream, &runtime_model_slug)
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
        let capability_name = required_capabilities
            .iter()
            .next()
            .map(|capability| format!("{capability:?}"))
            .unwrap_or_else(|| "Unknown".to_string());
        let error = GatewayError::classified(
            StatusCode::BAD_REQUEST,
            format!("selected routes cannot preserve required capability {capability_name}"),
            "invalid_request_error",
            "gateway_protocol_capability_unsupported",
            "gateway_protocol_capability_unsupported",
            None,
            Some(json!({ "scope": "gateway" })),
        );
        append_gateway_usage_log(
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
    let candidate_protocols = if let Some(profile_key) = continuation_profile_key.as_ref() {
        match profile_key.protocol {
            WireProtocol::ChatCompletions => vec![UpstreamProtocol::ChatCompletions],
            WireProtocol::Responses => vec![UpstreamProtocol::Responses],
            WireProtocol::Messages => Vec::new(),
        }
    } else if let Some(allowed) = codex_catalog_allowed_profiles.as_ref() {
        allowed
            .iter()
            .next()
            .and_then(|profile_key| match profile_key.protocol {
                WireProtocol::ChatCompletions => Some(UpstreamProtocol::ChatCompletions),
                WireProtocol::Responses => Some(UpstreamProtocol::Responses),
                WireProtocol::Messages => None,
            })
            .into_iter()
            .collect()
    } else {
        match &claude_replay_route {
            ClaudeThinkingReplayRoute::Pinned { protocol, .. } => vec![*protocol],
            ClaudeThinkingReplayRoute::NoReplay => {
                if requires_responses_tooling {
                    if fallback_to_chat {
                        vec![UpstreamProtocol::ChatCompletions]
                    } else {
                        vec![UpstreamProtocol::Responses]
                    }
                } else {
                    vec![endpoint.native_protocol(), endpoint.opposite()]
                }
            }
            ClaudeThinkingReplayRoute::InvalidOrUnavailable => unreachable!(),
        }
    };
    let route_is_candidate =
        |upstream: &UpstreamConfig, key_fingerprint: &str, protocol: UpstreamProtocol| {
            upstream.active
                && upstream.supports_protocol(protocol)
                && upstream.supports_model(model)
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
        let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
            return false;
        };
        route_api_keys(upstream, &runtime_model_slug)
            .into_iter()
            .any(|api_key| {
                let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                route_is_candidate(upstream, &key_fingerprint, protocol)
            })
    };
    let candidate_passes = if requested_features.optional.is_empty() {
        candidate_protocols
            .iter()
            .copied()
            .map(|protocol| (None, protocol))
            .collect::<Vec<_>>()
    } else {
        let mut miss_tiers = std::collections::BTreeSet::new();
        for protocol in candidate_protocols.iter().copied() {
            for upstream in &routing_snapshot.upstreams {
                let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                    continue;
                };
                for api_key in route_api_keys(upstream, &runtime_model_slug) {
                    let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                    if route_is_candidate(upstream, &key_fingerprint, protocol) {
                        if let Some(route) = route_capability(upstream, &key_fingerprint, protocol)
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
                candidate_protocols
                    .iter()
                    .copied()
                    .map(move |protocol| (Some(misses), protocol))
            })
            .collect::<Vec<_>>()
    };
    let request_route_attempts = RequestRouteAttempts::default();
    for protocol in candidate_protocols.iter().copied() {
        for upstream in &routing_snapshot.upstreams {
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                continue;
            };
            for api_key in route_api_keys(upstream, &runtime_model_slug) {
                let key_fingerprint = route_key_fingerprint(upstream, &api_key);
                if !route_is_candidate(upstream, &key_fingerprint, protocol) {
                    continue;
                }
                let (route_health_key, _) =
                    route_health_keys(upstream, &key_fingerprint, &runtime_model_slug, protocol);
                request_route_attempts.register_eligible(
                    route_set_aggregate_key(upstream, &runtime_model_slug, protocol),
                    route_health_key,
                );
            }
        }
    }
    tracing::debug!(
        request_id = %request_id,
        downstream_key_id = %downstream.id,
        path = %request_path,
        original_model = %model,
        normalized_model = %normalized_model,
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
                upstream.active && upstream.id == upstream_id && upstream.supports_model(model)
            })
            .then(|| upstream_id.to_string())
    } else if state.config.routing_affinity_enabled {
        match state.get_affinity_upstream(&downstream.id, normalized_model) {
            Some(upstream_id)
                if routing_snapshot.upstreams.iter().any(|upstream| {
                    upstream.active && upstream.id == upstream_id && upstream.supports_model(model)
                }) =>
            {
                Some(upstream_id)
            }
            Some(_) => {
                state.clear_affinity_upstream(&downstream.id, normalized_model);
                None
            }
            None => None,
        }
    } else {
        None
    };

    'candidate_passes: for (optional_miss_tier, protocol) in candidate_passes {
        let upstream_optional_misses = |upstream: &UpstreamConfig| {
            let runtime_model_slug = upstream.resolved_model_name(model)?;
            route_api_keys(upstream, &runtime_model_slug)
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
                    upstream_optional_misses(upstream).is_some_and(|candidate| candidate == misses)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut deprioritized_upstreams = Vec::new();
        upstreams.retain(|upstream| {
            let is_non_premium_request = !upstream.is_premium_model_request(model);
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
            || (state.config.routing_affinity_enabled && total_candidate_count == 1);
        let ranking_pressure = |upstream: &UpstreamConfig| {
            let runtime = upstream_runtime_snapshots
                .get(&upstream.id)
                .copied()
                .unwrap_or_default();
            let request_cost = upstream.request_cost_for_model(model);
            let minute_pressure = runtime.minute_cost + request_cost;
            let five_hour_pressure = runtime.five_hour_cost + request_cost;
            (
                false,
                0,
                runtime.in_flight,
                minute_pressure as u64 * 1_000 / upstream.requests_per_minute.max(1) as u64,
                five_hour_pressure as u64 * 1_000 / upstream.request_quota_requests.max(1) as u64,
            )
        };
        let optional_capability_misses_by_upstream = upstreams
            .iter()
            .chain(deprioritized_upstreams.iter())
            .map(|upstream| {
                (
                    upstream.id.clone(),
                    upstream_optional_misses(upstream).unwrap_or(requested_features.optional.len()),
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
                in_flight,
                minute_pressure,
                five_hour_pressure,
                Reverse(upstream.priority),
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
                        let escape_ratio =
                            state.config.routing_affinity_escape_pressure_ratio.max(1.0);
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
                                normalized_model = %normalized_model,
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
                                normalized_model = %normalized_model,
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
                state.next_routing_tie_breaker(&downstream.id, normalized_model, protocol);
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
                    normalized_model = %normalized_model,
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
                let request_cost = upstream.request_cost_for_model(model);
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
                    upstream.is_premium_model_request(model)
                )
            })
            .collect::<Vec<_>>();
        let upstreams_for_retry = upstreams.clone();
        tracing::debug!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            protocol = ?protocol,
            candidates = ?candidate_summary,
            "sorted upstream candidates"
        );

        for (upstream_index, upstream) in upstreams.into_iter().enumerate() {
            let runtime = upstream_runtime_snapshots
                .get(&upstream.id)
                .copied()
                .unwrap_or_default();
            let request_cost = upstream.request_cost_for_model(model);
            let minute_cost = runtime.minute_cost + request_cost;
            let five_hour_cost = runtime.five_hour_cost + request_cost;
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                continue;
            };
            let candidate_keys = route_api_keys(&upstream, &runtime_model_slug)
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
            if candidate_keys.is_empty() {
                tracing::debug!(
                    request_id = %request_id,
                    downstream_key_id = %downstream.id,
                    path = %request_path,
                    original_model = %model,
                    normalized_model = %normalized_model,
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
                normalized_model = %normalized_model,
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
            for (key_index, api_key) in candidate_keys.iter().enumerate() {
                let key_fingerprint = route_key_fingerprint(&upstream, api_key);
                let (route_health_key, key_health_key) =
                    route_health_keys(&upstream, &key_fingerprint, &runtime_model_slug, protocol);
                if !request_route_attempts.should_attempt(&route_health_key) {
                    continue;
                }
                let route_health_permit = match state
                    .reserve_route_health(&route_health_key, &key_health_key)
                    .await
                {
                    RouteAvailability::Ready(permit) => Arc::new(TokioMutex::new(Some(permit))),
                    RouteAvailability::Cooling { class, retry_after }
                    | RouteAvailability::HalfOpenBusy { class, retry_after } => {
                        record_cooled_route_attempt(
                            &request_route_attempts,
                            &upstream,
                            &key_fingerprint,
                            &runtime_model_slug,
                            protocol,
                            class,
                            retry_after,
                        );
                        last_error = Some(GatewayError::TemporaryUpstreamUnavailable(
                            "all eligible upstream routes are temporarily unavailable".into(),
                        ));
                        last_failure_upstream =
                            Some((upstream.id.clone(), Some(upstream.name.clone())));
                        continue;
                    }
                };
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
                    if state
                        .try_reserve_upstream_request(&upstream, model)
                        .await
                        .is_err()
                    {
                        finish_route_health_permit(&route_health_permit, RouteOutcome::Cancelled)
                            .await;
                        last_error = Some(GatewayError::Upstream(
                            "failed to reserve upstream request capacity".into(),
                        ));
                        break;
                    }
                    let upstream_request_guard = UpstreamRequestReservation::new(
                        UpstreamRequestGuard::new(state.clone(), upstream.id.clone()),
                    );
                    tracing::info!(
                        request_id = %request_id,
                        downstream_key_id = %downstream.id,
                        path = %request_path,
                        original_model = %model,
                        normalized_model = %normalized_model,
                        selected_upstream_id = %upstream.id,
                        selected_upstream_protocol = ?protocol,
                        selected_upstream_key_prefix = %key_prefix(api_key),
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
                            upstream_id: upstream.id.clone(),
                            route_health_key: route_health_key.clone(),
                            route_attempts: request_route_attempts.clone(),
                            route_health_permit: route_health_permit.clone(),
                            upstream_request_guard: upstream_request_guard.clone(),
                            downstream_concurrency_guard: downstream_concurrency_guard.clone(),
                            hedge_control: None,
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
                                error_message: None,
                                error_category: None,
                                started,
                                hedge_control: None,
                            },
                        );
                    }
                    #[cfg(test)]
                    wait_on_pre_header_preparation_test_gate().await;
                    let global_context_profile = state
                        .global_context_profile_for_upstream_base_url(&upstream.base_url)
                        .await;
                    let (dispatch_body, dispatch_response_history_context, chat_fallback_stage) =
                        if endpoint == EndpointKind::Responses
                            && protocol == UpstreamProtocol::ChatCompletions
                            && chat_only_responses_fallback
                        {
                            let stage = initial_chat_fallback_stage(
                                &state,
                                &downstream.id,
                                client_family,
                                normalized_model,
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
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                client_family,
                                fallback_stage = stage.as_str(),
                                "selected chat-only Responses fallback stage"
                            );
                            match prepare_responses_chat_fallback_request(
                                &state,
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
                                    append_gateway_usage_log(
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
                                    upstream_request_guard.release().await;
                                    return Err(error);
                                }
                            }
                        } else {
                            (body.clone(), response_history_context.clone(), None)
                        };

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
                                    let runtime_model_slug =
                                        candidate.resolved_model_name(model)?;
                                    route_api_keys(candidate, &runtime_model_slug)
                                        .into_iter()
                                        .find_map(|api_key| {
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
                                        })
                                }),
                        );
                        candidates
                    } else {
                        Vec::new()
                    };

                    let result = send_to_upstream(
                        &state,
                        &upstream,
                        api_key,
                        &[],
                        &route_hedge_candidates,
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
                        normalized_model,
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
                        &mut stream_only_recovery,
                        &mut stream_only_recovery_leader,
                        &mut stream_only_recovery_identity,
                    )
                    .await;
                    active_request_guard.clear_aggregate_cancellation_log();
                    if let Some(cancellation) = pre_header_cancellation.as_ref() {
                        cancellation.disarm();
                    }

                    // Non-streaming requests and failed streaming attempts should
                    // release upstream capacity immediately because no long-lived
                    // stream body is handed to the caller.
                    if !request_stream || result.is_err() {
                        upstream_request_guard.release().await;
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
                        attempt_mode = UpstreamAttemptMode::SseAggregate;
                        continue;
                    }

                    match result {
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
                                .await;
                            }
                            if selected_upstream_id != upstream.id {
                                upstream_request_guard.release().await;
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
                                            model,
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
                                        (
                                            evidence.requested.as_str(),
                                            evidence.field.as_str(),
                                            evidence.value.as_str(),
                                        )
                                    });
                                append_troubleshooting_route_headers(
                                    &mut result.response_headers,
                                    &selected_upstream_id,
                                    &selected_upstream_name,
                                    &result.selected_upstream_key_fingerprint,
                                    selected_upstream_protocol,
                                    protocol_transition_label(endpoint, selected_upstream_protocol),
                                    chat_fallback_stage.map(ChatFallbackStage::as_str),
                                    applied_effort_control,
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
                                upstream_request_guard.release().await;
                                downstream_concurrency_guard.release();
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
                                    normalized_model,
                                    &selected_upstream_id,
                                );
                            }
                            if matches!(result.usage_log_timing, UsageLogTiming::Immediate) {
                                if let Some(selected_upstream) = routing_snapshot
                                    .upstreams
                                    .iter()
                                    .find(|candidate| candidate.id == selected_upstream_id)
                                {
                                    if let Some(selected_runtime_model) =
                                        selected_upstream.resolved_model_name(model)
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
                                    .await;
                                }
                                if let Err(error) =
                                    state.mark_upstream_success(&selected_upstream_id).await
                                {
                                    tracing::warn!(
                                        selected_upstream_id = %selected_upstream_id,
                                        error = %error,
                                        "failed to reset legacy upstream failure count after immediate success"
                                    );
                                }
                            }
                            if use_routing_affinity {
                                state.set_affinity_upstream(
                                    &downstream.id,
                                    normalized_model,
                                    &selected_upstream_id,
                                );
                            }
                            tracing::info!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
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
                                } else {
                                    context.emit(result.status, None, None, result.usage).await;
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
                            return Ok(result);
                        }
                        Err(error)
                            if !same_route_retry_attempted
                                && !stream_only_recovery.final_attempt
                                && should_retry_same_route_once(&error) =>
                        {
                            same_route_retry_attempted = true;
                            tracing::info!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                retry_delay_ms = 300,
                                error_category = %error.error_category(),
                                "retrying transient upstream failure on the same route"
                            );
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            continue;
                        }
                        Err(error)
                            if key_index + 1 < candidate_keys.len()
                                && !stream_only_recovery.final_attempt
                                && should_try_next_key(&error) =>
                        {
                            finish_route_health_permit(
                                &route_health_permit,
                                route_health_outcome(&error),
                            )
                            .await;
                            record_route_attempt(
                                &state,
                                &request_route_attempts,
                                &route_health_key,
                                &capability_snapshot,
                                &requested_features,
                                inference_strength.as_deref(),
                                model,
                                &upstream,
                                &key_fingerprint,
                                &runtime_model_slug,
                                protocol,
                                &error,
                            )
                            .await;
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_name = %upstream.name,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %error,
                                "upstream key failed; trying next key"
                            );
                            last_error = Some(error);
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));
                            break;
                        }
                        Err(GatewayError::ConcurrencyFull {
                            message,
                            retry_after_seconds,
                        }) => {
                            let retry_after_seconds = retry_after_seconds.unwrap_or(15).max(1);
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_name = %upstream.name,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %message,
                                retry_after_seconds,
                                "upstream concurrency/capacity response; moving to another route"
                            );
                            if state.config.routing_affinity_enabled {
                                state.clear_affinity_upstream(&downstream.id, normalized_model);
                            }
                            state
                                .mark_upstream_concurrency_full(
                                    &upstream.id,
                                    retry_after_seconds.saturating_mul(1_000),
                                )
                                .await;
                            last_error = Some(GatewayError::ConcurrencyFull {
                                message,
                                retry_after_seconds: Some(retry_after_seconds),
                            });
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));

                            record_route_attempt(
                                &state,
                                &request_route_attempts,
                                &route_health_key,
                                &capability_snapshot,
                                &requested_features,
                                inference_strength.as_deref(),
                                model,
                                &upstream,
                                &key_fingerprint,
                                &runtime_model_slug,
                                protocol,
                                &GatewayError::ConcurrencyFull {
                                    message: String::new(),
                                    retry_after_seconds: Some(retry_after_seconds),
                                },
                            )
                            .await;
                            finish_route_health_permit(
                                &route_health_permit,
                                if stream_only_recovery.consumed {
                                    // The aggregate stream probe is an internal capability
                                    // recovery attempt.  A provider-side concurrency response
                                    // describes the probe mode, not the JSON route, so do not
                                    // quarantine the exact route for the next request.
                                    RouteOutcome::Cancelled
                                } else {
                                    RouteOutcome::RouteFailureWithRetry {
                                        class: FailureClass::CapacityUnavailable,
                                        retry_after: Duration::from_secs(retry_after_seconds),
                                    }
                                },
                            )
                            .await;

                            break;
                        }
                        Err(GatewayError::TooManyRequests {
                            message,
                            retry_after_seconds,
                        }) => {
                            let retry_after_seconds = retry_after_seconds.unwrap_or(
                                state
                                    .config
                                    .upstream_rate_limit_default_retry_seconds
                                    .max(1),
                            );
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_name = %upstream.name,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %message,
                                retry_after_seconds,
                                "upstream rate limited; moving to another route"
                            );
                            if state.config.routing_affinity_enabled {
                                state.clear_affinity_upstream(&downstream.id, normalized_model);
                            }
                            state
                                .mark_upstream_rate_limited(&upstream.id, retry_after_seconds)
                                .await;
                            last_error = Some(GatewayError::TooManyRequests {
                                message,
                                retry_after_seconds: Some(retry_after_seconds),
                            });
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));

                            record_route_attempt(
                                &state,
                                &request_route_attempts,
                                &route_health_key,
                                &capability_snapshot,
                                &requested_features,
                                inference_strength.as_deref(),
                                model,
                                &upstream,
                                &key_fingerprint,
                                &runtime_model_slug,
                                protocol,
                                &GatewayError::TooManyRequests {
                                    message: String::new(),
                                    retry_after_seconds: Some(retry_after_seconds),
                                },
                            )
                            .await;
                            finish_route_health_permit(
                                &route_health_permit,
                                if stream_only_recovery.consumed {
                                    RouteOutcome::Cancelled
                                } else {
                                    RouteOutcome::RouteFailureWithRetry {
                                        class: FailureClass::RateLimited,
                                        retry_after: Duration::from_secs(retry_after_seconds),
                                    }
                                },
                            )
                            .await;

                            break;
                        }
                        Err(error @ GatewayError::BadRequest(_)) => {
                            finish_route_health_permit(&route_health_permit, RouteOutcome::Success)
                                .await;
                            maybe_record_chat_fallback_stage_failure(
                                &state,
                                &downstream.id,
                                client_family,
                                normalized_model,
                                &upstream.id,
                                chat_fallback_stage,
                                &error,
                            );
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %error,
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
                                .await;
                                maybe_record_chat_fallback_stage_failure(
                                    &state,
                                    &downstream.id,
                                    client_family,
                                    normalized_model,
                                    &upstream.id,
                                    chat_fallback_stage,
                                    &error,
                                );
                                tracing::warn!(
                                    request_id = %request_id,
                                    downstream_key_id = %downstream.id,
                                    path = %request_path,
                                    original_model = %model,
                                    normalized_model = %normalized_model,
                                    selected_upstream_id = %upstream.id,
                                    selected_upstream_protocol = ?protocol,
                                    selected_upstream_key_prefix = %key_prefix(api_key),
                                    error = %error,
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
                                    route_health_outcome(&error),
                                )
                                .await;
                                record_route_attempt(
                                    &state,
                                    &request_route_attempts,
                                    &route_health_key,
                                    &capability_snapshot,
                                    &requested_features,
                                    inference_strength.as_deref(),
                                    model,
                                    &upstream,
                                    &key_fingerprint,
                                    &runtime_model_slug,
                                    protocol,
                                    &error,
                                )
                                .await;
                            }
                            maybe_record_chat_fallback_stage_failure(
                                &state,
                                &downstream.id,
                                client_family,
                                normalized_model,
                                &upstream.id,
                                chat_fallback_stage,
                                &error,
                            );
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %error,
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
                                route_health_outcome(&error),
                            )
                            .await;
                            tracing::debug!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                upstream_attempt_mode = attempt_mode.as_str(),
                                error = %error,
                                error_category = %error.error_category(),
                                stream_to_json_recovery = true,
                                "streaming upstream attempt failed; retrying without stream"
                            );
                            same_route_retry_attempted = true;
                            attempt_mode = UpstreamAttemptMode::Json;
                            continue;
                        }
                        Err(GatewayError::TemporaryUpstreamUnavailable(message)) => {
                            tracing::warn!(
                                request_id = %request_id,
                                downstream_key_id = %downstream.id,
                                path = %request_path,
                                original_model = %model,
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %message,
                                "upstream temporarily unavailable, trying next candidate"
                            );
                            finish_route_health_permit(
                                &route_health_permit,
                                if stream_only_recovery.consumed {
                                    RouteOutcome::Cancelled
                                } else {
                                    RouteOutcome::RouteFailure(FailureClass::TransientServer)
                                },
                            )
                            .await;
                            record_route_attempt(
                                &state,
                                &request_route_attempts,
                                &route_health_key,
                                &capability_snapshot,
                                &requested_features,
                                inference_strength.as_deref(),
                                model,
                                &upstream,
                                &key_fingerprint,
                                &runtime_model_slug,
                                protocol,
                                &GatewayError::TemporaryUpstreamUnavailable(message.clone()),
                            )
                            .await;
                            last_error = Some(GatewayError::TemporaryUpstreamUnavailable(message));
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
                                normalized_model = %normalized_model,
                                selected_upstream_id = %upstream.id,
                                selected_upstream_protocol = ?protocol,
                                selected_upstream_key_prefix = %key_prefix(api_key),
                                error = %error,
                                "upstream request failed"
                            );
                            finish_route_health_permit(
                                &route_health_permit,
                                route_health_outcome(&error),
                            )
                            .await;
                            record_route_attempt(
                                &state,
                                &request_route_attempts,
                                &route_health_key,
                                &capability_snapshot,
                                &requested_features,
                                inference_strength.as_deref(),
                                model,
                                &upstream,
                                &key_fingerprint,
                                &runtime_model_slug,
                                protocol,
                                &error,
                            )
                            .await;
                            last_error = Some(error);
                            last_failure_upstream =
                                Some((upstream.id.clone(), Some(upstream.name.clone())));
                            break;
                        }
                    }
                }
                if stream_only_recovery.final_attempt {
                    break 'candidate_passes;
                }
            }
        }
    }

    if let Some(last_route_error) = last_error {
        let attempt_ledger = request_route_attempts.ledger_snapshot();
        let should_aggregate = !attempt_ledger.is_empty()
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
        let error = if should_aggregate {
            terminal_route_failure_error(&attempt_ledger)
        } else {
            last_route_error
        };
        let (upstream_id, upstream_name) = last_failure_upstream
            .as_ref()
            .map(|(id, name)| (id.as_str(), name.as_deref()))
            .unwrap_or(("", None));
        append_gateway_usage_log(
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
        if should_rollback_downstream_reservation(&error) {
            state
                .rollback_downstream_request_reservation(&downstream.id)
                .await;
        }
        downstream_concurrency_guard.release();
        active_request_guard.fail_and_finish(error.error_category());
        tracing::error!(
            request_id = %request_id,
            downstream_key_id = %downstream.id,
            path = %request_path,
            original_model = %model,
            normalized_model = %normalized_model,
            endpoint = %request_path,
            error = %error,
            "request failed after exhausting upstream candidates"
        );
        return Err(error);
    }

    let error = no_routable_model_error(&routing_snapshot, model);
    append_gateway_usage_log(
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
        normalized_model = %normalized_model,
        endpoint = %request_path,
        "no routable upstream found for request"
    );
    downstream_concurrency_guard.release();
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
