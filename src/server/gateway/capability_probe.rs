use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use axum::http::StatusCode;
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};

use super::compat::normalize_chat_payload_for_upstream_compatibility;
use crate::capabilities::{
    apply_probe_outcome, apply_probe_outcome_partial, Capability, CompiledCapabilityConfiguration,
    DeclarativeProbeCase, DialectProfileKey, EvidenceState, PredicateOperator, ProbeJob,
    ProbeJobBatch, ProbeMode, ProbeOutcome, ProbeQueueEnqueueOutcome, ProbeQueueState,
    ReasoningCarrier, ResponsePredicate, RouteIdentity, TokenLimitField, UpstreamDialectProfile,
    WireProtocol,
};
use crate::keys::upstream_key_fingerprint;
use crate::protocol::stream_aggregate::{SseEvent, MAX_STREAM_AGGREGATE_TOTAL_BYTES};
use crate::protocol::{
    ProtocolError, StreamAggregateResult, StreamResponseAggregator, UpstreamStreamErrorKind,
};
use crate::routing::UpstreamProtocol;
use crate::state::{
    join_upstream_url, unix_seconds, AppState, ProbeJobExecution, RouteHealthKey,
    RuntimeCoordinationError, UpstreamConfig, UpstreamRequestLease,
};

/// User-turn prompt sent by core capability probes (minimal text, token
/// limit, reasoning control, declarative cases). A concrete arithmetic
/// question yields a deterministic text answer and engages real
/// inference/reasoning paths, unlike a greeting or placeholder.
pub const PROBE_INPUT_PROMPT: &str = "请计算 17 乘以 23，并给出最终答案。";

fn enqueue_probe_job(queue: &mut ProbeQueueState, state: &AppState, job: ProbeJob) -> bool {
    match queue.enqueue_with_outcome(job) {
        Ok(ProbeQueueEnqueueOutcome::Enqueued) => true,
        Ok(ProbeQueueEnqueueOutcome::Unchanged) => false,
        Ok(ProbeQueueEnqueueOutcome::Replaced(discarded)) => {
            state.discard_capability_probe_submission(&discarded);
            false
        }
        Err(_) => false,
    }
}

fn enqueue_probe_batch(
    queue: &mut ProbeQueueState,
    state: &AppState,
    batch: ProbeJobBatch,
) -> ProbeJobBatch {
    let outcome = queue.enqueue_batch_with_outcome(batch);
    let (remaining, replaced) = outcome.into_parts();
    for discarded in replaced {
        state.discard_capability_probe_submission(&discarded);
    }
    remaining
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningTrigger {
    pub field: String,
    pub value: Value,
}

impl ReasoningTrigger {
    pub fn new(field: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            field: field.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum CoreProbeCase {
    MinimalText {
        stream: bool,
    },
    TokenLimit {
        field: TokenLimitField,
    },
    ReasoningControl {
        field: String,
        value: Value,
    },
    FunctionTools,
    FunctionSelection,
    ToolContinuation {
        reasoning_carrier: Option<ReasoningCarrier>,
        reasoning_trigger: Option<ReasoningTrigger>,
    },
    ParallelTools,
    IndexedToolArguments,
    UsageStream,
    ImageDataUrl,
    ImageHttps {
        url: String,
        expected_label: String,
    },
    RestrictedResponses,
    Declarative(DeclarativeProbeCase),
}

impl CoreProbeCase {
    pub fn tool_continuation(reasoning_carrier: Option<ReasoningCarrier>) -> Self {
        CoreProbeCase::ToolContinuation {
            reasoning_carrier,
            reasoning_trigger: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageProbeContract {
    DominantColor,
    GenericLabel,
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

pub type CapabilityProbePlan = ProbePlan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbePlanCompleteness {
    Full,
    CapacitySkipped,
}

impl ProbePlan {
    pub fn agent_core() -> Self {
        Self {
            protocol: WireProtocol::ChatCompletions,
            output_token_cap: 64,
            cases: vec![
                CoreProbeCase::MinimalText { stream: false },
                CoreProbeCase::MinimalText { stream: true },
                CoreProbeCase::FunctionTools,
                CoreProbeCase::FunctionSelection,
                CoreProbeCase::tool_continuation(None),
                CoreProbeCase::IndexedToolArguments,
                CoreProbeCase::UsageStream,
            ],
        }
    }

    pub fn reasoning_agent() -> Self {
        let mut plan = Self::agent_core();
        plan.cases.push(CoreProbeCase::tool_continuation(Some(
            ReasoningCarrier::ReasoningContent,
        )));
        plan
    }

    pub fn full() -> Self {
        let mut plan = Self::reasoning_agent();
        plan.cases.extend([
            CoreProbeCase::ParallelTools,
            CoreProbeCase::ImageDataUrl,
            CoreProbeCase::RestrictedResponses,
        ]);
        plan
    }
}

pub fn probe_plan_for_route(
    configuration: &CompiledCapabilityConfiguration,
    route: &RouteIdentity,
) -> ProbePlan {
    let output_token_cap = configuration.source().probe.output_token_cap.min(64);
    if route.protocol == WireProtocol::Messages {
        return ProbePlan {
            protocol: WireProtocol::Messages,
            cases: Vec::new(),
            output_token_cap,
        };
    }

    let mut plan = match route.protocol {
        WireProtocol::ChatCompletions => ProbePlan::full(),
        WireProtocol::Responses => ProbePlan::agent_core(),
        WireProtocol::Messages => unreachable!("Messages returned before probe plan selection"),
    };
    plan.protocol = route.protocol;
    plan.output_token_cap = output_token_cap;

    let candidates = configuration.probe_candidates_for(route);
    for field in candidates.token_limit_fields {
        if !plan.cases.iter().any(
            |case| matches!(case, CoreProbeCase::TokenLimit { field: existing } if *existing == field),
        ) {
            plan.cases.push(CoreProbeCase::TokenLimit { field });
        }
    }
    for (field, values) in &candidates.reasoning_controls {
        for value in values {
            if !plan.cases.iter().any(|case| {
                matches!(case, CoreProbeCase::ReasoningControl { field: existing_field, value: existing_value }
                    if existing_field == field && existing_value == value)
            }) {
                plan.cases.push(CoreProbeCase::ReasoningControl {
                    field: field.clone(),
                    value: value.clone(),
                });
            }
        }
    }
    let reasoning_trigger =
        candidates
            .reasoning_controls
            .iter()
            .next()
            .and_then(|(field, values)| {
                values
                    .first()
                    .map(|value| ReasoningTrigger::new(field.clone(), value.clone()))
            });
    for reasoning_carrier in candidates
        .reasoning_carriers
        .iter()
        .copied()
        .filter(|carrier| match route.protocol {
            WireProtocol::ChatCompletions => *carrier == ReasoningCarrier::ReasoningContent,
            WireProtocol::Responses => *carrier == ReasoningCarrier::ResponsesReasoningItem,
            WireProtocol::Messages => false,
        })
    {
        match plan.cases.iter_mut().find(|case| {
            matches!(case, CoreProbeCase::ToolContinuation { reasoning_carrier: Some(existing), .. }
                if *existing == reasoning_carrier)
        }) {
            Some(CoreProbeCase::ToolContinuation {
                reasoning_trigger: slot,
                ..
            }) => {
                if reasoning_trigger.is_some() {
                    *slot = reasoning_trigger.clone();
                }
            }
            Some(_) => {}
            None => {
                plan.cases.push(CoreProbeCase::ToolContinuation {
                    reasoning_carrier: Some(reasoning_carrier),
                    reasoning_trigger: reasoning_trigger.clone(),
                });
            }
        }
    }
    plan.cases.extend(
        configuration
            .extensions_for(route)
            .into_iter()
            .filter(|case| case.protocol == route.protocol)
            .cloned()
            .map(CoreProbeCase::Declarative),
    );

    let fixture = configuration
        .expectations_for(route)
        .into_iter()
        .find_map(|expectation| expectation.https_image_fixture.as_ref())
        .or(configuration.source().probe.https_image_fixture.as_ref());
    if let Some(fixture) = fixture {
        if !plan
            .cases
            .iter()
            .any(|case| matches!(case, CoreProbeCase::ImageDataUrl))
        {
            plan.cases.push(CoreProbeCase::ImageDataUrl);
        }
        plan.cases.push(CoreProbeCase::ImageHttps {
            url: fixture.url.clone(),
            expected_label: fixture.expected_label.clone(),
        });
    }

    plan
}

pub fn probe_plan_for_job(job: &ProbeJob) -> ProbePlan {
    let configuration = &job.plan_configuration;
    let primary_exposed_model = job
        .exposed_model_slugs
        .iter()
        .next()
        .cloned()
        .unwrap_or_else(|| job.key.runtime_model_slug.clone());
    let mut route = RouteIdentity {
        upstream_id: job.key.upstream_id.clone(),
        key_fingerprint: job.key.key_fingerprint.clone(),
        exposed_model_slug: primary_exposed_model,
        runtime_model_slug: job.key.runtime_model_slug.clone(),
        protocol: job.key.protocol,
        tags: BTreeSet::new(),
    };
    configuration.apply_route_tags(&mut route);
    if job.mode == ProbeMode::Reasoning {
        // Reasoning-scoped batches probe only the connectivity gate and the
        // declared reasoning-control candidates. Everything else (tools,
        // images, streaming, token limits) is deliberately excluded: the
        // button exists to discover thinking levels, and a minimal plan keeps
        // the batch short enough to survive internal gateway rate limits.
        let mut plan = ProbePlan {
            protocol: job.key.protocol,
            cases: vec![CoreProbeCase::MinimalText { stream: false }],
            output_token_cap: configuration.source().probe.output_token_cap.min(64),
        };
        let candidates = configuration.probe_candidates_for(&route);
        for (field, values) in &candidates.reasoning_controls {
            for value in values {
                if !plan.cases.iter().any(|case| {
                    matches!(case, CoreProbeCase::ReasoningControl { field: existing_field, value: existing_value }
                        if existing_field == field && existing_value == value)
                }) {
                    plan.cases.push(CoreProbeCase::ReasoningControl {
                        field: field.clone(),
                        value: value.clone(),
                    });
                }
            }
        }
        return plan;
    }
    let mut plan = probe_plan_for_route(configuration, &route);
    plan.cases
        .retain(|case| !matches!(case, CoreProbeCase::ImageHttps { .. }));

    let fixture = job
        .exposed_model_slugs
        .iter()
        .find_map(|exposed_model_slug| {
            let mut alias_route = RouteIdentity {
                upstream_id: job.key.upstream_id.clone(),
                key_fingerprint: job.key.key_fingerprint.clone(),
                exposed_model_slug: exposed_model_slug.clone(),
                runtime_model_slug: job.key.runtime_model_slug.clone(),
                protocol: job.key.protocol,
                tags: BTreeSet::new(),
            };
            configuration.apply_route_tags(&mut alias_route);
            configuration
                .expectations_for(&alias_route)
                .into_iter()
                .find_map(|expectation| expectation.https_image_fixture.as_ref())
        })
        .or(configuration.source().probe.https_image_fixture.as_ref());
    if let Some(fixture) = fixture {
        plan.cases.push(CoreProbeCase::ImageHttps {
            url: fixture.url.clone(),
            expected_label: fixture.expected_label.clone(),
        });
    }
    plan
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeCaseVerdict {
    Supported {
        evidence_code: String,
    },
    Rejected {
        evidence_code: String,
        http_status: Option<u16>,
    },
    Unobserved {
        operational_code: String,
        http_status: Option<u16>,
    },
}

#[derive(Clone)]
pub struct CapabilityProbeService {
    sender: mpsc::Sender<ProbeJobBatch>,
}

#[derive(Clone, Debug)]
pub enum CapabilityProbeMockReply {
    ChatJson(Value),
    ChatSse(Vec<String>),
}

pub async fn run_probe_plan_for_test(
    base_url: &str,
    api_key: &str,
    plan: CapabilityProbePlan,
    timeout_seconds: u64,
) -> io::Result<ProbeOutcome> {
    run_probe_plan_for_model_for_test(base_url, api_key, "probe-model", plan, timeout_seconds).await
}

pub async fn run_probe_plan_for_model_for_test(
    base_url: &str,
    api_key: &str,
    runtime_model_slug: &str,
    plan: CapabilityProbePlan,
    timeout_seconds: u64,
) -> io::Result<ProbeOutcome> {
    let (outcome, _completeness) = run_probe_plan_with_coordination_for_test(
        base_url,
        api_key,
        runtime_model_slug,
        plan,
        timeout_seconds,
        None,
        None,
    )
    .await?;
    Ok(outcome)
}

/// Test-only: runs a probe plan against an optional coordinated AppState, so
/// tests can exercise the upstream capacity reservation guard (Redis or
/// in-memory coordination). Returns the plan completeness so tests can assert
/// that a capacity-skipped probe still finishes without aborting.
pub async fn run_probe_plan_with_coordination_for_test(
    base_url: &str,
    api_key: &str,
    runtime_model_slug: &str,
    plan: CapabilityProbePlan,
    timeout_seconds: u64,
    probe_state: Option<AppState>,
    upstream: Option<UpstreamConfig>,
) -> io::Result<(ProbeOutcome, ProbePlanCompleteness)> {
    if plan.protocol == WireProtocol::Messages {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Messages is a downstream compatibility protocol, not an upstream probe protocol",
        ));
    }
    let key = DialectProfileKey::for_key(
        "probe-upstream",
        upstream_key_fingerprint("probe-upstream", api_key),
        runtime_model_slug,
        plan.protocol,
    );
    let client = Client::builder().build().expect("probe test client");
    ProbeExecutor {
        client,
        base_url: base_url.to_owned(),
        api_key: api_key.to_owned(),
        protocol: key.protocol,
        probe_state,
        upstream,
        runtime_model_slug: key.runtime_model_slug.clone(),
        request_timeout: Duration::from_secs(timeout_seconds.max(1)),
        reasoning_timeout: Duration::from_secs(timeout_seconds.max(1)),
    }
    .run_plan(&key, plan)
    .await
}

impl CapabilityProbeService {
    pub fn spawn(state: AppState) -> Self {
        // Capacity bounds pending submission batches. The worker retains any
        // batch remainder until ProbeQueueState has room for every exact route.
        let (sender, mut receiver) =
            mpsc::channel::<ProbeJobBatch>(state.config.capability_probe_queue_capacity.max(1));
        state.set_capability_probe_sender(sender.clone());
        let service = Self {
            sender: sender.clone(),
        };
        tokio::spawn(async move {
            let mut queue =
                ProbeQueueState::new(1, 1, state.config.capability_probe_queue_capacity);
            let mut active = FuturesUnordered::new();
            let mut deferred_batch = None;
            let mut receiver_open = true;
            let mut reconcile_tick = tokio::time::interval_at(
                Instant::now() + Duration::from_secs(1),
                Duration::from_secs(1),
            );
            reconcile_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            if let Ok(initial_jobs) = state.reconcile_dialect_profiles(unix_seconds()).await {
                for job in initial_jobs {
                    let _ = enqueue_probe_job(&mut queue, &state, job);
                }
            }
            loop {
                let capability_snapshot = state.capability_snapshot();
                let probe = &capability_snapshot.configuration.source().probe;
                let runtime_settings = state.runtime_settings();
                queue.set_limits(
                    (runtime_settings.capability_probe_concurrency as usize)
                        .min(probe.max_global_concurrency),
                    probe.max_per_upstream_concurrency,
                );
                if probe.enabled {
                    if let Some(batch) = deferred_batch.take() {
                        let remaining = enqueue_probe_batch(&mut queue, &state, batch);
                        if !remaining.is_empty() {
                            deferred_batch = Some(remaining);
                        }
                    }
                    while let Some(next) = queue.start_next() {
                        let state = state.clone();
                        active.push(async move {
                            let key = next.key.clone();
                            let binding = next.configuration.clone();
                            state.mark_capability_probe_running(&key, &binding);
                            let execution = run_probe_job(&state, &next).await;
                            (key, binding, execution)
                        });
                    }
                } else {
                    for job in queue.clear_pending() {
                        state.discard_capability_probe_submission(&job);
                    }
                    if let Some(batch) = deferred_batch.take() {
                        for job in batch.into_jobs() {
                            state.discard_capability_probe_submission(&job);
                        }
                    }
                }

                if active.is_empty() && !receiver_open && deferred_batch.is_none() {
                    break;
                }

                tokio::select! {
                    _ = reconcile_tick.tick() => {
                        if let Ok(jobs) = state.reconcile_dialect_profiles(unix_seconds()).await {
                            for job in jobs {
                                if !enqueue_probe_job(&mut queue, &state, job) && queue.is_full() {
                                    tracing::warn!("capability probe queue reached its job capacity");
                                }
                            }
                        }
                    }
                    completed = active.next(), if !active.is_empty() => {
                        if let Some((key, binding, execution)) = completed {
                            queue.finish(&key);
                            state.finish_capability_probe_job(&key, &binding, &execution);
                        }
                    }
                    received = receiver.recv(), if receiver_open && deferred_batch.is_none() => {
                        match received {
                            Some(batch) => {
                                if state.capability_snapshot().configuration.source().probe.enabled {
                                    let remaining = enqueue_probe_batch(&mut queue, &state, batch);
                                    if !remaining.is_empty() {
                                        tracing::info!(
                                            jobs = remaining.jobs().len(),
                                            "capability probe batch is waiting for queue capacity"
                                        );
                                        deferred_batch = Some(remaining);
                                    }
                                } else {
                                    for job in batch.into_jobs() {
                                        state.discard_capability_probe_submission(&job);
                                    }
                                }
                            }
                            None => receiver_open = false,
                        }
                    }
                }
            }
        });
        service
    }

    pub fn sender(&self) -> &mpsc::Sender<ProbeJobBatch> {
        &self.sender
    }
}

pub(super) struct DialectErrorProbe<'a> {
    pub(super) upstream_id: &'a str,
    pub(super) key_fingerprint: &'a str,
    pub(super) exposed_model_slug: &'a str,
    pub(super) runtime_model_slug: &'a str,
    pub(super) protocol: UpstreamProtocol,
    pub(super) status: StatusCode,
    pub(super) class: crate::state::RouteFailureClass,
    pub(super) error_text: &'a str,
}

/// Dialect fields that mention of an error text as rejected by the upstream.
/// The first match is returned so callers can strip exactly that field.
pub(super) fn dialect_field_error_hint(error_text: &str) -> Option<&'static str> {
    let error_lower = error_text.to_ascii_lowercase();
    let indicates_field_error = [
        "unsupported",
        "not supported",
        "unrecognized",
        "unknown field",
        "invalid field",
        "invalid parameter",
        "unexpected field",
    ]
    .iter()
    .any(|pattern| error_lower.contains(pattern));
    if !indicates_field_error {
        return None;
    }
    [
        "parallel_tool_calls",
        "service_tier",
        "reasoning_effort",
        "max_output_tokens",
        "max_completion_tokens",
        "stream_options",
        "reasoning_content",
        "tool_choice",
        "verbosity",
        "prompt_cache_key",
    ]
    .iter()
    .copied()
    .find(|field| error_lower.contains(field))
}

/// Fields that are safe to strip for a same-route downgrade retry: they are
/// optional sampling/extensions, never semantic state. `tool_choice` and
/// `reasoning_content` are excluded because removing them changes behavior.
pub(super) fn is_safe_dialect_strip_field(field: &str) -> bool {
    matches!(
        field,
        "parallel_tool_calls"
            | "service_tier"
            | "reasoning_effort"
            | "max_output_tokens"
            | "max_completion_tokens"
            | "stream_options"
            | "verbosity"
            | "prompt_cache_key"
    )
}

/// Map a stripped dialect field to the capability a runtime hint should
/// reject once the downgrade retry succeeds. Fields without a capability
/// mapping (token limit fields, service_tier, ...) are still stripped for the
/// request but do not produce a learned hint.
pub(super) fn dialect_field_capability(field: &str) -> Option<Capability> {
    match field {
        "parallel_tool_calls" => Some(Capability::ParallelToolCalls),
        "stream_options" => Some(Capability::UsageStream),
        "reasoning_effort" => Some(Capability::ReasoningOutput),
        _ => None,
    }
}

pub(super) async fn maybe_queue_dialect_error_probe(
    state: &AppState,
    input: DialectErrorProbe<'_>,
) -> bool {
    let DialectErrorProbe {
        upstream_id,
        key_fingerprint,
        exposed_model_slug,
        runtime_model_slug,
        protocol,
        status,
        class,
        error_text,
    } = input;
    // 400s are always request-shape rejections; 5xx with request-shape
    // evidence are classified RequestRejected/FeatureUnsupported by B1 and
    // equally represent a dialect field problem rather than a service fault.
    if status != StatusCode::BAD_REQUEST
        && !matches!(
            class,
            crate::state::RouteFailureClass::RequestRejected
                | crate::state::RouteFailureClass::FeatureUnsupported
        )
    {
        return false;
    }
    if dialect_field_error_hint(error_text).is_none() {
        return false;
    }
    state
        .build_capability_probe_job(
            upstream_id,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
            crate::capabilities::ProbeReason::DialectError,
        )
        .await
        .ok()
        .flatten()
        .is_some_and(|job| state.queue_capability_probe(job))
}

pub async fn run_probe_job(state: &AppState, job: &ProbeJob) -> ProbeJobExecution {
    let routing = state.routing_snapshot().await;
    let Some(upstream) = routing
        .upstreams
        .iter()
        .find(|upstream| upstream.id == job.key.upstream_id && upstream.active)
        .cloned()
    else {
        // The upstream was deactivated or removed after the job was queued;
        // the newer configuration supersedes this job.
        return ProbeJobExecution::Superseded;
    };

    let capability_snapshot = state.capability_snapshot();
    if !AppState::capability_probe_job_is_current(&capability_snapshot, &upstream, job) {
        // The configuration fingerprint changed since the job was queued;
        // record the superseded outcome instead of silently discarding it.
        return ProbeJobExecution::Superseded;
    }
    let route_key = RouteHealthKey {
        upstream_id: job.key.upstream_id.clone(),
        key_fingerprint: job.key.key_fingerprint.clone(),
        runtime_model_slug: job.key.runtime_model_slug.clone(),
        protocol: job.key.protocol,
    };
    if let Ok(Some(health)) = state.route_health_snapshot(&route_key).await {
        if health.cooldown_remaining > Duration::ZERO {
            return ProbeJobExecution::CooldownSkipped {
                cooldown_remaining: health.cooldown_remaining,
            };
        }
    }
    let plan = probe_plan_for_job(job);
    let mapped_keys = upstream.keys_for_model(&job.key.runtime_model_slug);
    let matching_keys = upstream
        .available_keys()
        .into_iter()
        .filter(|api_key| {
            mapped_keys.iter().any(|mapped| mapped == api_key)
                && upstream_key_fingerprint(&upstream.id, api_key) == job.key.key_fingerprint
        })
        .collect::<Vec<_>>();
    let [api_key] = matching_keys.as_slice() else {
        // The key mapping that produced this job no longer matches; the
        // current configuration supersedes it.
        return ProbeJobExecution::Superseded;
    };
    let api_key = api_key.clone();
    let runtime_settings = state.runtime_settings();
    let result = ProbeExecutor {
        client: state.client_for_url(&upstream.base_url),
        base_url: upstream.base_url.clone(),
        api_key,
        protocol: job.key.protocol,
        probe_state: Some(state.clone()),
        upstream: Some(upstream.clone()),
        runtime_model_slug: job.key.runtime_model_slug.clone(),
        request_timeout: Duration::from_secs(
            runtime_settings
                .capability_probe_request_timeout_seconds
                .max(1),
        ),
        reasoning_timeout: Duration::from_secs(
            runtime_settings
                .capability_probe_request_timeout_seconds
                .max(1),
        ),
    }
    .run_plan(&job.key, plan)
    .await;
    let (outcome, completeness) = match result {
        Ok(pair) => pair,
        Err(error) => return ProbeJobExecution::Failed(error),
    };

    let mut profile = state
        .capability_snapshot()
        .profiles
        .get(&job.key)
        .cloned()
        .unwrap_or_else(|| UpstreamDialectProfile::unknown(job.key.clone()));
    profile.configuration_fingerprint = job.configuration.configuration_fingerprint.clone();
    profile.probe_schema_version = job.configuration.probe_schema_version;
    let mut conclusive_capabilities = None;
    match outcome {
        ProbeOutcome::OperationalFailure {
            code,
            http_status,
            attempted_at,
        } => {
            apply_probe_outcome(
                &mut profile,
                ProbeOutcome::OperationalFailure {
                    code,
                    http_status,
                    attempted_at,
                },
            );
        }
        ProbeOutcome::Conclusive {
            capabilities,
            token_limit_field,
            reasoning_carrier,
            reasoning_controls,
            correction_rules,
            extension_evidence,
            evidence_codes,
            event_types,
            http_status,
            attempted_at,
        } => {
            conclusive_capabilities = Some(capabilities.keys().copied().collect::<BTreeSet<_>>());
            if completeness == ProbePlanCompleteness::Full && job.mode == ProbeMode::Full {
                apply_probe_outcome(
                    &mut profile,
                    ProbeOutcome::Conclusive {
                        capabilities,
                        token_limit_field,
                        reasoning_carrier,
                        reasoning_controls,
                        correction_rules,
                        extension_evidence,
                        evidence_codes,
                        event_types,
                        http_status,
                        attempted_at,
                    },
                );
            } else {
                // A capacity-skipped probe or a reasoning-scoped batch is
                // partial: merge instead of replace so previously-known
                // evidence (reasoning levels, carriers, supported
                // capabilities) is never erased by the cases that could not
                // run or were deliberately excluded from the minimal plan.
                apply_probe_outcome_partial(
                    &mut profile,
                    ProbeOutcome::Conclusive {
                        capabilities,
                        token_limit_field,
                        reasoning_carrier,
                        reasoning_controls,
                        correction_rules,
                        extension_evidence,
                        evidence_codes,
                        event_types,
                        http_status,
                        attempted_at,
                    },
                );
            }
        }
    }
    let applied = match state
        .upsert_dialect_profile_if_probe_current(profile, &job.configuration)
        .await
    {
        Ok(applied) => applied,
        Err(error) => return ProbeJobExecution::Failed(error),
    };
    if applied {
        if let Some(capabilities) = conclusive_capabilities {
            state.clear_runtime_capability_hints_after_probe(
                &job.key,
                &job.configuration.configuration_fingerprint,
                &capabilities,
            );
        }
    }
    ProbeJobExecution::Completed
}

struct ProbeExecutor {
    client: Client,
    base_url: String,
    api_key: String,
    protocol: WireProtocol,
    probe_state: Option<AppState>,
    upstream: Option<UpstreamConfig>,
    runtime_model_slug: String,
    request_timeout: Duration,
    reasoning_timeout: Duration,
}

impl ProbeExecutor {
    async fn run_plan(
        &self,
        key: &DialectProfileKey,
        plan: ProbePlan,
    ) -> io::Result<(ProbeOutcome, ProbePlanCompleteness)> {
        let mut evidence = ProbeEvidence::new(plan.protocol);
        let mut completeness = ProbePlanCompleteness::Full;
        let mut saw_conclusive_evidence = false;
        let mut saw_case_timeout = false;
        let mut first_http_status: Option<u16> = None;
        // A plan-level budget caps the whole run so per-case retries and
        // slow reasoning requests cannot drag a batch out indefinitely.
        let total_budget = plan_total_budget(&plan, self.request_timeout, self.reasoning_timeout);
        let deadline = Instant::now() + total_budget;
        let mut first_case = true;
        for case in plan.cases {
            let case_timeout = self.case_timeout_for(&case);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                saw_case_timeout = true;
                evidence.apply(
                    &case,
                    ProbeCaseVerdict::Unobserved {
                        operational_code: "probe_case_timeout".into(),
                        http_status: None,
                    },
                );
                continue;
            }
            let verdict = match tokio::time::timeout(
                case_timeout.min(remaining),
                self.run_case(key, &case, plan.output_token_cap.min(64)),
            )
            .await
            {
                Ok(result) => match result {
                    Ok(verdict) => verdict,
                    // A transient upstream capacity rejection (see
                    // reserve_upstream_request) skips only this case; the
                    // rest of the plan still runs and records.
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        completeness = ProbePlanCompleteness::CapacitySkipped;
                        continue;
                    }
                    Err(error) => return Err(error),
                },
                Err(_) => {
                    // A per-case timeout records the case as unobserved and
                    // keeps the plan running: later cases (e.g. the
                    // reasoning controls that actually carry the levels the
                    // button is looking for) still execute.
                    saw_case_timeout = true;
                    evidence.apply(
                        &case,
                        ProbeCaseVerdict::Unobserved {
                            operational_code: "probe_case_timeout".into(),
                            http_status: None,
                        },
                    );
                    continue;
                }
            };
            match verdict {
                ProbeCaseVerdict::Unobserved {
                    operational_code,
                    http_status,
                } if matches!(http_status, Some(401 | 403)) => {
                    // Credential errors are not transient: every later case
                    // would hit the same wall, so abort the plan.
                    return Ok((
                        ProbeOutcome::OperationalFailure {
                            code: operational_code,
                            http_status,
                            attempted_at: unix_seconds(),
                        },
                        completeness,
                    ));
                }
                ProbeCaseVerdict::Unobserved {
                    operational_code,
                    http_status,
                } if first_case => {
                    // The first case is the connectivity gate: when it fails
                    // (timeout, 5xx, 429, network error) the whole route is
                    // operationally blocked, so fail fast.
                    return Ok((
                        ProbeOutcome::OperationalFailure {
                            code: operational_code,
                            http_status,
                            attempted_at: unix_seconds(),
                        },
                        completeness,
                    ));
                }
                ProbeCaseVerdict::Unobserved { http_status, .. } => {
                    if first_http_status.is_none() {
                        first_http_status = http_status;
                    }
                    evidence.apply(&case, verdict);
                }
                other => {
                    saw_conclusive_evidence = true;
                    evidence.apply(&case, other);
                }
            }
            first_case = false;
        }
        if saw_conclusive_evidence || completeness == ProbePlanCompleteness::CapacitySkipped {
            // Capacity-skipped plans keep the conclusive shape so the caller
            // merges them partially (Unobserved entries carry no
            // information); only a plan whose cases all failed for real
            // reasons becomes an operational failure.
            Ok((
                evidence.into_conclusive_outcome(unix_seconds()),
                completeness,
            ))
        } else {
            Ok((
                ProbeOutcome::OperationalFailure {
                    code: if saw_case_timeout {
                        "probe_timeout".into()
                    } else {
                        "probe_all_unobserved".into()
                    },
                    http_status: first_http_status,
                    attempted_at: unix_seconds(),
                },
                completeness,
            ))
        }
    }

    fn case_timeout_for(&self, case: &CoreProbeCase) -> Duration {
        let reasoning_case = matches!(case, CoreProbeCase::ReasoningControl { .. })
            || matches!(
                case,
                CoreProbeCase::ToolContinuation {
                    reasoning_trigger: Some(_),
                    ..
                }
            );
        if reasoning_case {
            self.reasoning_timeout
        } else {
            self.request_timeout
        }
    }

    async fn run_case(
        &self,
        _key: &DialectProfileKey,
        case: &CoreProbeCase,
        output_token_cap: u32,
    ) -> io::Result<ProbeCaseVerdict> {
        match case {
            CoreProbeCase::MinimalText { stream } => {
                if self.protocol() == WireProtocol::Responses {
                    let body = json!({
                        "model": &self.runtime_model_slug,
                        "input": PROBE_INPUT_PROMPT,
                        "stream": stream,
                    });
                    if *stream {
                        let response = self.post_responses_stream(body).await?;
                        if let Some(verdict) = response.operational_verdict() {
                            return Ok(verdict);
                        }
                        if response.status != StatusCode::OK {
                            return Ok(verdict_for_status(
                                response.status,
                                "minimal_text_stream",
                                "minimal_text_stream_rejected",
                                "minimal_text_stream_failed",
                            ));
                        }
                        return if response.saw_done && response.saw_text_delta {
                            Ok(ProbeCaseVerdict::Supported {
                                evidence_code: "minimal_text_stream".into(),
                            })
                        } else {
                            Ok(ProbeCaseVerdict::Rejected {
                                evidence_code: "minimal_text_stream_incomplete".into(),
                                http_status: Some(response.status.as_u16()),
                            })
                        };
                    }
                    let response = self.post_responses(body).await?;
                    if response.status != StatusCode::OK {
                        return Ok(verdict_for_status(
                            response.status,
                            "minimal_text",
                            "minimal_text_rejected",
                            "minimal_text_failed",
                        ));
                    }
                    return if responses_has_usable_output(&response.body) {
                        Ok(ProbeCaseVerdict::Supported {
                            evidence_code: "minimal_text".into(),
                        })
                    } else if has_explicit_zero_output_tokens(&response.body) {
                        Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "minimal_text_nonstream_empty".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    } else {
                        Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "minimal_text_nonstream_empty_unobserved".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    };
                }
                let mut body = json!({
                    "model": &self.runtime_model_slug,
                    "messages": [{"role": "user", "content": PROBE_INPUT_PROMPT}],
                    "stream": stream,
                });
                if *stream {
                    body["stream_options"] = json!({"include_usage": false});
                    let response = self.post_chat_stream(body).await?;
                    if let Some(verdict) = response.operational_verdict() {
                        return Ok(verdict);
                    }
                    if response.status != StatusCode::OK {
                        return Ok(verdict_for_status(
                            response.status,
                            "minimal_text_stream",
                            "minimal_text_stream_rejected",
                            "minimal_text_stream_failed",
                        ));
                    }
                    if response.saw_done && response.saw_text_delta {
                        Ok(ProbeCaseVerdict::Supported {
                            evidence_code: "minimal_text_stream".into(),
                        })
                    } else {
                        Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "minimal_text_stream_incomplete".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    }
                } else {
                    let response = self.post_chat(body).await?;
                    if response.status != StatusCode::OK {
                        return Ok(verdict_for_status(
                            response.status,
                            "minimal_text",
                            "minimal_text_rejected",
                            "minimal_text_failed",
                        ));
                    }
                    if chat_has_usable_output(&response.body) {
                        Ok(ProbeCaseVerdict::Supported {
                            evidence_code: "minimal_text".into(),
                        })
                    } else if has_explicit_zero_output_tokens(&response.body) {
                        Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "minimal_text_nonstream_empty".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    } else {
                        Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "minimal_text_nonstream_empty_unobserved".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    }
                }
            }
            CoreProbeCase::FunctionTools => {
                let nonce = "n-17";
                if self.protocol() == WireProtocol::Responses {
                    let response = self
                        .post_responses(json!({
                            "model": &self.runtime_model_slug,
                            "input": format!("Call gateway_compat_probe with nonce exactly {nonce}."),
                            "tools": [{
                                "type": "function",
                                "name": "gateway_compat_probe",
                                "description": "compat probe",
                                "parameters": {
                                    "type": "object",
                                    "properties": {"nonce": {"type": "string"}},
                                    "required": ["nonce"]
                                }
                            }]
                        }))
                        .await?;
                    if response.status != StatusCode::OK {
                        return Ok(verdict_for_status(
                            response.status,
                            "function_tools",
                            "function_tools_rejected",
                            "function_tools_failed",
                        ));
                    }
                    let Some(call) = response.body["output"].as_array().and_then(|output| {
                        output.iter().find(|item| item["type"] == "function_call")
                    }) else {
                        return Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "function_tools_missing_call".into(),
                            http_status: Some(response.status.as_u16()),
                        });
                    };
                    let arguments = call["arguments"].as_str().unwrap_or_default();
                    let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                    return if call["name"] == "gateway_compat_probe"
                        && call["call_id"]
                            .as_str()
                            .is_some_and(|call_id| !call_id.is_empty())
                        && parsed["nonce"] == nonce
                    {
                        Ok(ProbeCaseVerdict::Supported {
                            evidence_code: "function_tools".into(),
                        })
                    } else {
                        Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "function_tools_invalid_call".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    };
                }
                let body = json!({
                    "model": &self.runtime_model_slug,
                    "messages": [{
                        "role": "user",
                        "content": format!("Call gateway_compat_probe with nonce exactly {nonce}.")
                    }],
                    "tools": [{
                        "type": "function",
                        "function": {
                            "name": "gateway_compat_probe",
                            "description": "compat probe",
                            "parameters": {
                                "type": "object",
                                "properties": {"nonce": {"type": "string"}},
                                "required": ["nonce"]
                            }
                        }
                    }]
                });
                let response = self.post_chat(body).await?;
                if response.status != StatusCode::OK {
                    return Ok(verdict_for_status(
                        response.status,
                        "function_tools",
                        "function_tools_rejected",
                        "function_tools_failed",
                    ));
                }
                let Some(call) = response.body["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .and_then(|calls| calls.first())
                else {
                    return Ok(ProbeCaseVerdict::Unobserved {
                        operational_code: "function_tools_missing_call".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                };
                let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
                let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                if call["function"]["name"] == "gateway_compat_probe"
                    && call["id"]
                        .as_str()
                        .is_some_and(|call_id| !call_id.is_empty())
                    && parsed["nonce"] == nonce
                {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "function_tools".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "function_tools_invalid_call".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::FunctionSelection => {
                let nonce = "n-17";
                let response = if self.protocol() == WireProtocol::Responses {
                    self.post_responses(json!({
                        "model": &self.runtime_model_slug,
                        "input": format!("Call gateway_compat_probe with nonce exactly {nonce}."),
                        "tool_choice": {"type": "function", "name": "gateway_compat_probe"},
                        "tools": [{
                            "type": "function",
                            "name": "gateway_compat_probe",
                            "description": "compat probe",
                            "parameters": {
                                "type": "object",
                                "properties": {"nonce": {"type": "string"}},
                                "required": ["nonce"]
                            }
                        }]
                    }))
                    .await?
                } else {
                    self.post_chat(json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{
                            "role": "user",
                            "content": format!("Call gateway_compat_probe with nonce exactly {nonce}.")
                        }],
                        "tool_choice": {
                            "type": "function",
                            "function": {"name": "gateway_compat_probe"}
                        },
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "gateway_compat_probe",
                                "description": "compat probe",
                                "parameters": {
                                    "type": "object",
                                    "properties": {"nonce": {"type": "string"}},
                                    "required": ["nonce"]
                                }
                            }
                        }]
                    }))
                    .await?
                };
                if response.status != StatusCode::OK {
                    return Ok(verdict_for_status(
                        response.status,
                        "forced_tool_selected",
                        "forced_tool_choice_rejected",
                        "function_selection_failed",
                    ));
                }
                let call = if self.protocol() == WireProtocol::Responses {
                    response.body["output"]
                        .as_array()
                        .and_then(|output| {
                            output.iter().find(|item| item["type"] == "function_call")
                        })
                        .map(|call| {
                            (
                                call["name"].as_str(),
                                call["call_id"].as_str(),
                                call["arguments"].as_str(),
                            )
                        })
                } else {
                    response.body["choices"][0]["message"]["tool_calls"]
                        .as_array()
                        .and_then(|calls| calls.first())
                        .map(|call| {
                            (
                                call["function"]["name"].as_str(),
                                call["id"].as_str(),
                                call["function"]["arguments"].as_str(),
                            )
                        })
                };
                let Some((name, call_id, arguments)) = call else {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "forced_tool_not_selected".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                };
                let parsed = arguments
                    .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                    .unwrap_or(Value::Null);
                if name == Some("gateway_compat_probe")
                    && call_id.is_some_and(|id| !id.is_empty())
                    && parsed["nonce"] == nonce
                {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "forced_tool_selected".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "forced_tool_not_selected".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::ToolContinuation {
                reasoning_carrier,
                reasoning_trigger,
            } => {
                // Arithmetic-gated probe: the model must compute the nonce
                // (94 * 7 = 658) before calling the tool. Reasoning-capable
                // models emit a reasoning channel while computing, which is
                // exactly what the reasoning-carrier gate below checks for;
                // models without a reasoning channel may still compute and
                // call the tool directly and are rejected by that gate.
                let nonce = "658";
                let prompt = "First compute 94 * 7 exactly, then call gateway_compat_probe \
with the exact result as the nonce string.";
                if self.protocol() == WireProtocol::Responses {
                    let tools = json!([{
                        "type": "function",
                        "name": "gateway_compat_probe",
                        "description": "compat probe",
                        "parameters": {
                            "type": "object",
                            "properties": {"nonce": {"type": "string"}},
                            "required": ["nonce"]
                        }
                    }]);
                    let mut first_body = json!({
                        "model": &self.runtime_model_slug,
                        "input": prompt,
                        "tools": tools.clone(),
                    });
                    if let Some(trigger) = reasoning_trigger {
                        if trigger.field == "reasoning_effort" {
                            first_body["reasoning"] = json!({"effort": trigger.value});
                        } else {
                            first_body[&trigger.field] = trigger.value.clone();
                        }
                    }
                    let first = self.post_responses(first_body).await?;
                    if first.status != StatusCode::OK {
                        return Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "tool_continuation_failed".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    }
                    let Some(output) = first.body["output"].as_array() else {
                        return Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "tool_continuation_missing_call".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    };
                    let Some(call) = output.iter().find(|item| item["type"] == "function_call")
                    else {
                        return Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "tool_continuation_missing_call".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    };
                    let arguments = call["arguments"].as_str().unwrap_or_default();
                    let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                    let Some(call_id) = call["call_id"].as_str().filter(|id| !id.is_empty()) else {
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "tool_continuation_invalid_call".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    };
                    if call["name"] != "gateway_compat_probe" || parsed["nonce"] != nonce {
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "tool_continuation_invalid_call".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    }
                    if reasoning_carrier.is_some()
                        && !matches!(
                            reasoning_carrier,
                            Some(ReasoningCarrier::ResponsesReasoningItem)
                        )
                    {
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "reasoning_replay_carrier_mismatch".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    }
                    if matches!(
                        reasoning_carrier,
                        Some(ReasoningCarrier::ResponsesReasoningItem)
                    ) && !output.iter().any(|item| item["type"] == "reasoning")
                    {
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "reasoning_replay_missing".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    }
                    let mut input = output
                        .iter()
                        .filter(|item| {
                            matches!(item["type"].as_str(), Some("reasoning" | "function_call"))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": format!(r#"{{"nonce":"{nonce}","ok":true}}"#)
                    }));
                    let second = self
                        .post_responses(json!({
                            "model": &self.runtime_model_slug,
                            "input": input,
                            "tools": tools,
                        }))
                        .await?;
                    return if second.status == StatusCode::OK
                        && responses_has_output_text(&second.body)
                    {
                        Ok(ProbeCaseVerdict::Supported {
                            evidence_code: "tool_continuation_ok".into(),
                        })
                    } else {
                        Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "tool_continuation_failed".into(),
                            http_status: Some(second.status.as_u16()),
                        })
                    };
                }
                let mut first_body = json!({
                    "model": &self.runtime_model_slug,
                    "messages": [{
                        "role": "user",
                        "content": prompt
                    }],
                    "tools": [{
                        "type": "function",
                        "function": {
                            "name": "gateway_compat_probe",
                            "description": "compat probe",
                            "parameters": {
                                "type": "object",
                                "properties": {"nonce": {"type": "string"}},
                                "required": ["nonce"]
                            }
                        }
                    }],
                });
                if let Some(trigger) = reasoning_trigger {
                    first_body[&trigger.field] = trigger.value.clone();
                }
                let first = self.post_chat(first_body).await?;
                if first.status != StatusCode::OK {
                    return Ok(ProbeCaseVerdict::Unobserved {
                        operational_code: "tool_continuation_failed".into(),
                        http_status: Some(first.status.as_u16()),
                    });
                }
                let Some(call) = first.body["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .and_then(|calls| calls.first())
                else {
                    return Ok(ProbeCaseVerdict::Unobserved {
                        operational_code: "tool_continuation_missing_call".into(),
                        http_status: Some(first.status.as_u16()),
                    });
                };
                let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
                let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                if call["function"]["name"] != "gateway_compat_probe" || parsed["nonce"] != nonce {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "tool_continuation_invalid_call".into(),
                        http_status: Some(first.status.as_u16()),
                    });
                }
                let Some(call_id) = call["id"].as_str().filter(|id| !id.is_empty()) else {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "tool_continuation_invalid_call".into(),
                        http_status: Some(first.status.as_u16()),
                    });
                };
                let reasoning_content = first.body["choices"][0]["message"]["reasoning_content"]
                    .as_str()
                    .unwrap_or_default();
                if matches!(reasoning_carrier, Some(ReasoningCarrier::ReasoningContent))
                    && reasoning_content.is_empty()
                {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "reasoning_replay_missing".into(),
                        http_status: Some(first.status.as_u16()),
                    });
                }
                let mut assistant_message = json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [call.clone()]
                });
                if matches!(reasoning_carrier, Some(ReasoningCarrier::ReasoningContent))
                    && !reasoning_content.is_empty()
                {
                    assistant_message["reasoning_content"] =
                        Value::String(reasoning_content.to_string());
                }
                let second = self
                    .post_chat(json!({
                        "model": &self.runtime_model_slug,
                        "messages": [
                            {"role": "user", "content": "compat probe"},
                            assistant_message,
                            {
                                "role": "tool",
                                "tool_call_id": call_id,
                                "content": format!(r#"{{"nonce":"{nonce}","ok":true}}"#)
                            }
                        ],
                    }))
                    .await?;
                if second.status == StatusCode::OK {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "tool_continuation_ok".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "tool_continuation_failed".into(),
                        http_status: Some(second.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::IndexedToolArguments => {
                let nonce = "n-17";
                let response = if self.protocol() == WireProtocol::Responses {
                    self.post_responses_stream(json!({
                        "model": &self.runtime_model_slug,
                        "input": format!("Call gateway_compat_probe with nonce exactly {nonce}."),
                        "stream": true,
                        "tools": [{
                            "type": "function",
                            "name": "gateway_compat_probe",
                            "description": "compat probe",
                            "parameters": {
                                "type": "object",
                                "properties": {"nonce": {"type": "string"}},
                                "required": ["nonce"]
                            }
                        }]
                    }))
                    .await?
                } else {
                    self.post_chat_stream(json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{
                            "role": "user",
                            "content": format!("Call gateway_compat_probe with nonce exactly {nonce}.")
                        }],
                        "stream": true,
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "gateway_compat_probe",
                                "description": "compat probe",
                                "parameters": {
                                    "type": "object",
                                    "properties": {"nonce": {"type": "string"}},
                                    "required": ["nonce"]
                                }
                            }
                        }]
                    }))
                    .await?
                };
                if let Some(verdict) = response.operational_verdict() {
                    return Ok(verdict);
                }
                let valid = response.has_indexed_tool_arguments(nonce);
                if response.saw_done && valid {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "indexed_tool_arguments".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "indexed_tool_arguments_missing".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::UsageStream => {
                if self.protocol() == WireProtocol::Responses {
                    let response = self
                        .post_responses_stream(json!({
                            "model": &self.runtime_model_slug,
                            "input": PROBE_INPUT_PROMPT,
                            "stream": true,
                        }))
                        .await?;
                    if let Some(verdict) = response.operational_verdict() {
                        return Ok(verdict);
                    }
                    return if response.saw_done && response.saw_usage {
                        Ok(ProbeCaseVerdict::Supported {
                            evidence_code: "usage_stream".into(),
                        })
                    } else {
                        Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "usage_stream_missing_usage".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    };
                }
                let response = self
                    .post_chat_stream(json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": PROBE_INPUT_PROMPT}],
                        "stream": true,
                        "stream_options": {"include_usage": true},
                    }))
                    .await?;
                if let Some(verdict) = response.operational_verdict() {
                    return Ok(verdict);
                }
                if response.saw_done && response.saw_usage {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "usage_stream".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "usage_stream_missing_usage".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::ParallelTools => {
                let response = self
                    .post_chat(json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": "Call both tools in one turn."}],
                        "parallel_tool_calls": true,
                        "tools": [
                            {
                                "type": "function",
                                "function": {
                                    "name": "gateway_compat_probe",
                                    "description": "compat probe 1",
                                    "parameters": {"type": "object"}
                                }
                            },
                            {
                                "type": "function",
                                "function": {
                                    "name": "gateway_compat_probe_2",
                                    "description": "compat probe 2",
                                    "parameters": {"type": "object"}
                                }
                            }
                        ],
                    }))
                    .await?;
                if response.status != StatusCode::OK {
                    return Ok(ProbeCaseVerdict::Unobserved {
                        operational_code: "parallel_tools_failed".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                }
                let tool_calls = response.body["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if tool_calls.len() >= 2 {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "parallel_tools".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "parallel_tools_single_call".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::ImageDataUrl => {
                if self.protocol() == WireProtocol::Responses {
                    return self
                        .probe_responses_image(
                            DATA_URL_IMAGE_FIXTURE,
                            DATA_URL_IMAGE_EXPECTED_LABEL,
                            "image_data_url",
                            ImageProbeContract::DominantColor,
                        )
                        .await;
                }
                let response = self
                    .post_chat(json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{
                            "role": "user",
                            "content": [
                                {"type": "text", "text": "Inspect the actual image and report its dominant color via the probe tool. Set label to one of the lowercase values allowed by the tool schema."},
                                {"type": "image_url", "image_url": {"url": DATA_URL_IMAGE_FIXTURE}}
                            ]
                        }],
                        "tool_choice": {
                            "type": "function",
                            "function": {"name": "gateway_compat_probe"}
                        },
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "gateway_compat_probe",
                                "description": "Report the dominant color observed in the actual image.",
                                "parameters": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "The dominant color visible in the actual image, expressed as a lowercase label.",
                                            "enum": ["red", "green", "blue", "black", "white"]
                                        }
                                    },
                                    "required": ["label"],
                                    "additionalProperties": false
                                }
                            }
                        }],
                    }))
                    .await?;
                if response.status != StatusCode::OK {
                    return Ok(ProbeCaseVerdict::Unobserved {
                        operational_code: "image_data_url_failed".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                }
                let Some(call) = response.body["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .and_then(|calls| calls.first())
                else {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "image_data_url_missing_tool".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                };
                let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
                let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                if call["function"]["name"] == "gateway_compat_probe"
                    && parsed["label"] == DATA_URL_IMAGE_EXPECTED_LABEL
                {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "image_data_url".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "image_data_url_unrecognized".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::RestrictedResponses => Ok(ProbeCaseVerdict::Rejected {
                evidence_code: "restricted_responses_unverified".into(),
                http_status: None,
            }),
            CoreProbeCase::ImageHttps {
                url,
                expected_label,
            } => {
                if self.protocol() == WireProtocol::Responses {
                    return self
                        .probe_responses_image(
                            url,
                            expected_label,
                            "image_https",
                            ImageProbeContract::GenericLabel,
                        )
                        .await;
                }
                let response = self
                    .post_chat(json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{
                            "role": "user",
                            "content": [
                                {"type": "text", "text": "Inspect the actual image and report one concise label that best describes its visible content via the probe tool."},
                                {"type": "image_url", "image_url": {"url": url}}
                            ]
                        }],
                        "tool_choice": {
                            "type": "function",
                            "function": {"name": "gateway_compat_probe"}
                        },
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "gateway_compat_probe",
                                "description": "Report a concise label derived from the actual image content.",
                                "parameters": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "A concise label derived only from the actual image content."
                                        }
                                    },
                                    "required": ["label"],
                                    "additionalProperties": false
                                }
                            }
                        }],
                    }))
                    .await?;
                if response.status != StatusCode::OK {
                    return Ok(ProbeCaseVerdict::Unobserved {
                        operational_code: "image_https_failed".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                }
                let Some(call) = response.body["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .and_then(|calls| calls.first())
                else {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "image_https_missing_tool".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                };
                let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
                let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                if call["function"]["name"] == "gateway_compat_probe"
                    && parsed["label"] == expected_label.as_str()
                {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "image_https".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "image_https_unrecognized".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
            CoreProbeCase::TokenLimit { field } => {
                let mut body = if self.protocol() == WireProtocol::Responses {
                    json!({
                        "model": &self.runtime_model_slug,
                        "input": PROBE_INPUT_PROMPT,
                        "stream": false,
                    })
                } else {
                    json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": PROBE_INPUT_PROMPT}],
                        "stream": false,
                    })
                };
                if let Some(request_field) = field.request_field() {
                    body[request_field] = json!(output_token_cap);
                }
                let response = if self.protocol() == WireProtocol::Responses {
                    self.post_responses(body).await?
                } else {
                    self.post_chat(body).await?
                };
                Ok(verdict_for_status(
                    response.status,
                    "token_limit_accepted",
                    "token_limit_rejected",
                    "token_limit_failed",
                ))
            }
            CoreProbeCase::ReasoningControl { field, value } => {
                // Streaming first: thinking evidence can be observed in the
                // stream (delta.reasoning_content / reasoning item / usage
                // reasoning_tokens) and the probe stops as soon as the first
                // reasoning increment arrives, so a slow thinking model costs
                // only its first-token latency instead of a full completion.
                let mut stream_body = if self.protocol() == WireProtocol::Responses {
                    json!({
                        "model": &self.runtime_model_slug,
                        "input": PROBE_INPUT_PROMPT,
                        "stream": true,
                    })
                } else {
                    json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": PROBE_INPUT_PROMPT}],
                        "stream": true,
                        "stream_options": {"include_usage": true},
                    })
                };
                if self.protocol() == WireProtocol::Responses && field == "reasoning_effort" {
                    stream_body["reasoning"] = json!({"effort": value});
                } else {
                    stream_body[field] = value.clone();
                }
                let mut stop_on_reasoning =
                    |summary: &ProbeStreamSummary| summary.saw_reasoning_delta;
                let response = if self.protocol() == WireProtocol::Responses {
                    self.post_stream(
                        stream_body,
                        "/v1/responses",
                        UpstreamProtocol::Responses,
                        &mut stop_on_reasoning,
                    )
                    .await?
                } else {
                    let body = self.normalize_probe_chat_body(stream_body);
                    self.post_stream(
                        body,
                        "/v1/chat/completions",
                        UpstreamProtocol::ChatCompletions,
                        &mut stop_on_reasoning,
                    )
                    .await?
                };
                if response.saw_reasoning_delta {
                    return Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "reasoning_control_accepted".into(),
                    });
                }
                if response.status == StatusCode::BAD_REQUEST
                    || (response.status == StatusCode::OK && !response.saw_done)
                {
                    // The upstream rejected streaming (400) or answered the
                    // stream request with a non-stream body (a JSON response
                    // or a broken/incomplete stream): fall back to the legacy
                    // non-streaming judgment instead of dropping the case.
                    let mut body = if self.protocol() == WireProtocol::Responses {
                        json!({
                            "model": &self.runtime_model_slug,
                            "input": PROBE_INPUT_PROMPT,
                            "stream": false,
                        })
                    } else {
                        json!({
                            "model": &self.runtime_model_slug,
                            "messages": [{"role": "user", "content": PROBE_INPUT_PROMPT}],
                            "stream": false,
                        })
                    };
                    if self.protocol() == WireProtocol::Responses && field == "reasoning_effort" {
                        body["reasoning"] = json!({"effort": value});
                    } else {
                        body[field] = value.clone();
                    }
                    let response = if self.protocol() == WireProtocol::Responses {
                        self.post_responses(body).await?
                    } else {
                        self.post_chat(body).await?
                    };
                    if response.status == StatusCode::OK {
                        if reasoning_response_has_evidence(&response.body) {
                            return Ok(ProbeCaseVerdict::Supported {
                                evidence_code: "reasoning_control_accepted".into(),
                            });
                        }
                        // Most domestic gateways silently ignore unknown fields and
                        // return a plain 200. Without reasoning evidence that is
                        // not an acceptance: the bin must stay unverified or the
                        // catalog would advertise fake thinking levels.
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "reasoning_control_ignored".into(),
                            http_status: Some(200),
                        });
                    }
                    return Ok(verdict_for_status(
                        response.status,
                        "reasoning_control_accepted",
                        "reasoning_control_rejected",
                        "reasoning_control_failed",
                    ));
                }
                if response.status == StatusCode::OK {
                    // A complete stream without any reasoning evidence is an
                    // ignored field, exactly like the non-streaming case.
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "reasoning_control_ignored".into(),
                        http_status: Some(200),
                    });
                }
                Ok(verdict_for_status(
                    response.status,
                    "reasoning_control_accepted",
                    "reasoning_control_rejected",
                    "reasoning_control_failed",
                ))
            }
            CoreProbeCase::Declarative(case) => {
                let mut body = if self.protocol() == WireProtocol::Responses {
                    json!({
                        "model": &self.runtime_model_slug,
                        "input": PROBE_INPUT_PROMPT,
                        "stream": false,
                    })
                } else {
                    json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": PROBE_INPUT_PROMPT}],
                        "stream": false,
                    })
                };
                merge_json_object(&mut body, &case.request_patch);
                let response = if self.protocol() == WireProtocol::Responses {
                    self.post_responses(body).await?
                } else {
                    self.post_chat_unmodified(body).await?
                };
                if response.status != StatusCode::OK {
                    return Ok(verdict_for_status(
                        response.status,
                        "extension_probe_supported",
                        "extension_probe_rejected",
                        "extension_probe_failed",
                    ));
                }
                if response_predicate_matches(&response.body, &case.response_predicate) {
                    Ok(ProbeCaseVerdict::Supported {
                        evidence_code: "extension_probe_supported".into(),
                    })
                } else {
                    Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "extension_probe_predicate_failed".into(),
                        http_status: Some(response.status.as_u16()),
                    })
                }
            }
        }
    }

    /// Route every chat-protocol probe body through the same upstream
    /// compatibility normalization as real requests, so a probe that passes
    /// implies the same request shape in production (WS-E4).
    ///
    /// The reasoning-effort candidate is restored verbatim afterwards: real
    /// requests cap xhigh/max down to high, but a probe must send the exact
    /// candidate value or it could never distinguish an upstream that rejects
    /// xhigh from one that silently downgrades it.
    fn normalize_probe_chat_body(&self, body: Value) -> Value {
        let mut body = body;
        let original_reasoning_effort = body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let strip = self
            .upstream
            .as_ref()
            .map(|upstream| {
                upstream
                    .strip_nonstandard_chat_fields
                    .strips_on_unprobed_route()
            })
            .unwrap_or(false);
        normalize_chat_payload_for_upstream_compatibility(
            &mut body,
            &self.runtime_model_slug,
            &self.base_url,
            strip,
        );
        if let Some(effort) = original_reasoning_effort {
            body["reasoning_effort"] = Value::String(effort);
        }
        body
    }

    /// Sends a probe request, retrying exactly once when the upstream answers
    /// 429 with a Retry-After header. The retry sleeps at most the header
    /// value (capped) and is bounded by the outer per-case timeout, so a
    /// rate-limited probe degrades to a case skip instead of aborting the
    /// whole plan.
    async fn send_with_retry_after(
        &self,
        request: reqwest::RequestBuilder,
    ) -> io::Result<reqwest::Response> {
        let mut response = request
            .try_clone()
            .ok_or_else(|| io::Error::other("probe request body is not cloneable"))?
            .send()
            .await
            .map_err(io::Error::other)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            if let Some(retry_after) = retry_after_seconds(response.headers()) {
                tokio::time::sleep(retry_after).await;
                if let Some(retried) = request.try_clone() {
                    response = retried.send().await.map_err(io::Error::other)?;
                }
            }
        }
        Ok(response)
    }

    async fn post_chat(&self, body: Value) -> io::Result<ProbeHttpResponse> {
        self.post_chat_inner(body, true).await
    }

    /// Declarative extension probes send their request patch verbatim: the
    /// whole point is to observe whether the upstream accepts the patched
    /// field, so it must not be stripped by the compatibility pass.
    async fn post_chat_unmodified(&self, body: Value) -> io::Result<ProbeHttpResponse> {
        self.post_chat_inner(body, false).await
    }

    async fn post_chat_inner(&self, body: Value, normalize: bool) -> io::Result<ProbeHttpResponse> {
        let _held = self.reserve_upstream_request().await?;
        let body = if normalize {
            self.normalize_probe_chat_body(body)
        } else {
            body
        };
        let url = join_upstream_url(&self.base_url, "/v1/chat/completions");
        let request = self
            .client
            .post(url)
            .bearer_auth(self.api_key.trim())
            .json(&body);
        let response = self.send_with_retry_after(request).await?;
        let status = response.status();
        let body = response.json::<Value>().await.map_err(io::Error::other)?;
        Ok(ProbeHttpResponse { status, body })
    }

    async fn probe_responses_image(
        &self,
        image_url: &str,
        expected_label: &str,
        evidence_prefix: &str,
        contract: ImageProbeContract,
    ) -> io::Result<ProbeCaseVerdict> {
        let (prompt, tool_description, label_schema) = match contract {
            ImageProbeContract::DominantColor => (
                "Inspect the actual image and report its dominant color via the probe tool. Set label to one of the lowercase values allowed by the tool schema.",
                "Report the dominant color observed in the actual image.",
                json!({
                    "type": "string",
                    "description": "The dominant color visible in the actual image, expressed as a lowercase label.",
                    "enum": ["red", "green", "blue", "black", "white"]
                }),
            ),
            ImageProbeContract::GenericLabel => (
                "Inspect the actual image and report one concise label that best describes its visible content via the probe tool.",
                "Report a concise label derived from the actual image content.",
                json!({
                    "type": "string",
                    "description": "A concise label derived only from the actual image content."
                }),
            ),
        };
        let response = self
            .post_responses(json!({
                "model": &self.runtime_model_slug,
                "input": [{
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": prompt},
                        {"type": "input_image", "image_url": image_url}
                    ]
                }],
                "tool_choice": {"type": "function", "name": "gateway_compat_probe"},
                "tools": [{
                    "type": "function",
                    "name": "gateway_compat_probe",
                    "description": tool_description,
                    "parameters": {
                        "type": "object",
                        "properties": {"label": label_schema},
                        "required": ["label"],
                        "additionalProperties": false
                    }
                }]
            }))
            .await?;
        if response.status != StatusCode::OK {
            return Ok(ProbeCaseVerdict::Unobserved {
                operational_code: format!("{evidence_prefix}_failed"),
                http_status: Some(response.status.as_u16()),
            });
        }
        let Some(call) = response.body["output"]
            .as_array()
            .and_then(|output| output.iter().find(|item| item["type"] == "function_call"))
        else {
            return Ok(ProbeCaseVerdict::Rejected {
                evidence_code: format!("{evidence_prefix}_missing_tool"),
                http_status: Some(response.status.as_u16()),
            });
        };
        let arguments = call["arguments"].as_str().unwrap_or_default();
        let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
        if call["name"] == "gateway_compat_probe"
            && call["call_id"].is_string()
            && parsed["label"] == expected_label
        {
            Ok(ProbeCaseVerdict::Supported {
                evidence_code: evidence_prefix.into(),
            })
        } else {
            Ok(ProbeCaseVerdict::Rejected {
                evidence_code: format!("{evidence_prefix}_unrecognized"),
                http_status: Some(response.status.as_u16()),
            })
        }
    }

    async fn post_responses(&self, body: Value) -> io::Result<ProbeHttpResponse> {
        let _held = self.reserve_upstream_request().await?;
        let url = join_upstream_url(&self.base_url, "/v1/responses");
        let request = self
            .client
            .post(url)
            .bearer_auth(self.api_key.trim())
            .json(&body);
        let response = self.send_with_retry_after(request).await?;
        let status = response.status();
        let body = response.json::<Value>().await.map_err(io::Error::other)?;
        Ok(ProbeHttpResponse { status, body })
    }

    async fn post_responses_stream(&self, body: Value) -> io::Result<ProbeSseResponse> {
        self.post_stream(
            body,
            "/v1/responses",
            UpstreamProtocol::Responses,
            &mut |_| false,
        )
        .await
    }

    fn protocol(&self) -> WireProtocol {
        self.protocol
    }

    async fn post_chat_stream(&self, body: Value) -> io::Result<ProbeSseResponse> {
        let body = self.normalize_probe_chat_body(body);
        self.post_stream(
            body,
            "/v1/chat/completions",
            UpstreamProtocol::ChatCompletions,
            &mut |_| false,
        )
        .await
    }

    async fn post_stream(
        &self,
        body: Value,
        path: &str,
        protocol: UpstreamProtocol,
        early_stop: &mut impl FnMut(&ProbeStreamSummary) -> bool,
    ) -> io::Result<ProbeSseResponse> {
        let _held = self.reserve_upstream_request().await?;
        let url = join_upstream_url(&self.base_url, path);
        let request = self
            .client
            .post(url)
            .bearer_auth(self.api_key.trim())
            .json(&body);
        let response = self.send_with_retry_after(request).await?;
        let status = response.status();
        if status != StatusCode::OK {
            return Ok(ProbeSseResponse::empty(status));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_STREAM_AGGREGATE_TOTAL_BYTES as u64)
        {
            return Ok(ProbeSseResponse::operational(
                status,
                "probe_stream_byte_limit_exceeded",
            ));
        }

        let mut aggregator = StreamResponseAggregator::new(protocol);
        let mut summary = ProbeStreamSummary::default();
        let mut complete = false;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => {
                    return Ok(ProbeSseResponse::operational(
                        status,
                        "probe_stream_transport_failed",
                    ));
                }
            };
            let result = aggregator.push_observing(&chunk, |event| {
                summary.observe(protocol, event);
            });
            if early_stop(&summary) {
                return Ok(ProbeSseResponse::early_stopped(status, summary));
            }
            match result {
                Ok(StreamAggregateResult::Complete(_)) => {
                    complete = true;
                    break;
                }
                Ok(StreamAggregateResult::Pending) => {}
                Err(error) if stream_error_is_incomplete(&error) => {
                    return Ok(ProbeSseResponse::incomplete(status, summary));
                }
                Err(_) => {
                    return Ok(ProbeSseResponse::operational(
                        status,
                        "probe_stream_invalid",
                    ));
                }
            }
        }

        if !complete {
            match aggregator.finish_observing(|event| summary.observe(protocol, event)) {
                Ok(_) => {}
                Err(error) if stream_error_is_incomplete(&error) => {
                    return Ok(ProbeSseResponse::incomplete(status, summary));
                }
                Err(_) => {
                    return Ok(ProbeSseResponse::operational(
                        status,
                        "probe_stream_invalid",
                    ));
                }
            }
        }

        Ok(ProbeSseResponse::complete(status, protocol, summary))
    }

    async fn reserve_upstream_request(&self) -> io::Result<Option<Box<ProbeUpstreamRequestGuard>>> {
        let (Some(state), Some(upstream)) = (&self.probe_state, &self.upstream) else {
            // No probe coordination configured (e.g. the offline test
            // harness): nothing to reserve, proceed without a guard.
            return Ok(None);
        };
        // A transient capacity/quota rejection (e.g. the upstream is
        // momentarily saturated) must not abort the whole probe: retry up
        // to 3 attempts (2 retries with 100ms/200ms backoff), then signal
        // the caller to skip just that one case (io::ErrorKind::WouldBlock)
        // so the rest of the plan still runs and records. Only coordination
        // failures are fatal.
        let mut delay = Duration::from_millis(100);
        for attempt in 0..3 {
            match state
                .try_reserve_upstream_account_request(
                    upstream,
                    &upstream_key_fingerprint(&upstream.id, &self.api_key),
                    &self.runtime_model_slug,
                )
                .await
            {
                Ok(lease) => {
                    return Ok(Some(Box::new(ProbeUpstreamRequestGuard {
                        state: state.clone(),
                        lease,
                    })));
                }
                Err(error) => {
                    match error.reason {
                        crate::state::UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable => {
                            return Err(io::Error::other(RuntimeCoordinationError));
                        }
                        crate::state::UpstreamAdmissionRejectionReason::LocalConcurrency
                        | crate::state::UpstreamAdmissionRejectionReason::HedgeMinuteQuota
                        | crate::state::UpstreamAdmissionRejectionReason::HedgeWindowQuota => {
                            if attempt < 2 {
                                tracing::warn!(
                                    upstream_id = %upstream.id,
                                    error = %error.message,
                                    "capability probe request reservation rejected, retrying"
                                );
                                tokio::time::sleep(delay).await;
                                delay *= 2;
                            } else {
                                tracing::warn!(
                                    upstream_id = %upstream.id,
                                    error = %error.message,
                                    "capability probe request reservation still rejected, skipping case"
                                );
                                return Err(io::Error::new(
                                    io::ErrorKind::WouldBlock,
                                    "upstream capacity reservation rejected",
                                ));
                            }
                        }
                    }
                }
            }
        }
        unreachable!("reservation retry loop always returns")
    }
}

/// Returns true when a probe response carries concrete evidence that the
/// upstream actually ran a reasoning path: a non-empty chat
/// `reasoning_content` / GLM `message.reasoning` field, a positive
/// `reasoning_tokens` usage counter (chat or Responses), or a Responses
/// `output` reasoning item. A bare 200 with none of these is treated as an
/// ignored field, not an acceptance.
fn reasoning_response_has_evidence(body: &Value) -> bool {
    if let Some(message) = body.pointer("/choices/0/message") {
        if let Some(content) = message.get("reasoning_content").and_then(Value::as_str) {
            if !content.is_empty() {
                return true;
            }
        }
        if message
            .get("reasoning")
            .is_some_and(|value| !value.is_null())
        {
            return true;
        }
    }
    if body
        .pointer("/usage/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
        .is_some_and(|tokens| tokens > 0)
    {
        return true;
    }
    if let Some(output) = body.get("output").and_then(Value::as_array) {
        if output.iter().any(|item| item["type"] == "reasoning") {
            return true;
        }
    }
    if body
        .pointer("/usage/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
        .is_some_and(|tokens| tokens > 0)
    {
        return true;
    }
    false
}

/// Plan-level time budget: the sum of every case's timeout, capped at ten
/// minutes. Retries inside a case are bounded by the case timeout, and the
/// whole plan by this budget, so a pathological route cannot hold a batch
/// forever.
fn plan_total_budget(
    plan: &ProbePlan,
    request_timeout: Duration,
    reasoning_timeout: Duration,
) -> Duration {
    let per_case_seconds = plan
        .cases
        .iter()
        .map(|case| {
            let timeout = match case {
                CoreProbeCase::ReasoningControl { .. } => reasoning_timeout,
                CoreProbeCase::ToolContinuation {
                    reasoning_trigger: Some(_),
                    ..
                } => reasoning_timeout,
                _ => request_timeout,
            };
            timeout.as_secs().max(1)
        })
        .sum::<u64>();
    Duration::from_secs(per_case_seconds.clamp(1, 600))
}

/// Parses a Retry-After header (integer seconds form), capped so a stale
/// or hostile upstream cannot stall the probe worker.
fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let seconds = value.parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds.min(60)))
}

fn verdict_for_status(
    status: StatusCode,
    accepted_code: &str,
    rejected_code: &str,
    operational_code: &str,
) -> ProbeCaseVerdict {
    if status == StatusCode::OK {
        ProbeCaseVerdict::Supported {
            evidence_code: accepted_code.into(),
        }
    } else if matches!(status.as_u16(), 401 | 403 | 429 | 500..=599) {
        ProbeCaseVerdict::Unobserved {
            operational_code: operational_code.into(),
            http_status: Some(status.as_u16()),
        }
    } else {
        ProbeCaseVerdict::Rejected {
            evidence_code: rejected_code.into(),
            http_status: Some(status.as_u16()),
        }
    }
}

fn merge_json_object(target: &mut Value, patch: &Value) {
    let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) else {
        return;
    };
    merge_json_maps(target, patch);
}

fn merge_json_maps(
    target: &mut serde_json::Map<String, Value>,
    patch: &serde_json::Map<String, Value>,
) {
    for (key, value) in patch {
        match (target.get_mut(key), value) {
            (Some(Value::Object(target)), Value::Object(patch)) => {
                merge_json_maps(target, patch);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn response_predicate_matches(body: &Value, predicate: &ResponsePredicate) -> bool {
    let actual = body.pointer(&predicate.path);
    match predicate.operator {
        PredicateOperator::Exists => actual.is_some(),
        PredicateOperator::Equals => actual
            .zip(predicate.value.as_ref())
            .is_some_and(|(actual, expected)| actual == expected),
        PredicateOperator::Contains => {
            actual
                .zip(predicate.value.as_ref())
                .is_some_and(|(actual, expected)| match (actual, expected) {
                    (Value::String(actual), Value::String(expected)) => actual.contains(expected),
                    (Value::Array(actual), expected) => actual.contains(expected),
                    (Value::Object(actual), Value::String(expected)) => {
                        actual.contains_key(expected)
                    }
                    _ => false,
                })
        }
        PredicateOperator::EventSequence => actual
            .and_then(Value::as_array)
            .zip(predicate.value.as_ref().and_then(Value::as_array))
            .is_some_and(|(actual, expected)| {
                let mut next = 0;
                for item in actual {
                    if expected.get(next) == Some(item) {
                        next += 1;
                    }
                }
                next == expected.len()
            }),
    }
}

struct ProbeUpstreamRequestGuard {
    state: AppState,
    lease: UpstreamRequestLease,
}

impl Drop for ProbeUpstreamRequestGuard {
    fn drop(&mut self) {
        if let Ok(handle) = Handle::try_current() {
            let state = self.state.clone();
            let lease = self.lease.clone();
            let upstream_id = lease.upstream_id().to_string();
            handle.spawn(async move {
                if let Err(error) = state.release_upstream_request(lease).await {
                    tracing::error!(
                        upstream_id = %upstream_id,
                        error = %error,
                        "failed to release capability probe upstream lease"
                    );
                }
            });
        }
    }
}

struct ProbeHttpResponse {
    status: StatusCode,
    body: Value,
}

struct ProbeSseResponse {
    status: StatusCode,
    saw_done: bool,
    saw_text_delta: bool,
    saw_usage: bool,
    saw_reasoning_delta: bool,
    tool_calls: BTreeMap<u64, ToolArgumentProbe>,
    operational_code: Option<&'static str>,
}

impl ProbeSseResponse {
    fn empty(status: StatusCode) -> Self {
        Self {
            status,
            saw_done: false,
            saw_text_delta: false,
            saw_usage: false,
            saw_reasoning_delta: false,
            tool_calls: BTreeMap::new(),
            operational_code: stream_http_status_is_operational(status)
                .then_some("probe_stream_http_failed"),
        }
    }

    fn operational(status: StatusCode, operational_code: &'static str) -> Self {
        Self {
            operational_code: Some(operational_code),
            ..Self::empty(status)
        }
    }

    fn incomplete(status: StatusCode, summary: ProbeStreamSummary) -> Self {
        Self {
            status,
            saw_done: false,
            saw_text_delta: summary.saw_text_delta,
            saw_usage: summary.saw_usage,
            saw_reasoning_delta: summary.saw_reasoning_delta,
            tool_calls: summary.tool_calls,
            operational_code: None,
        }
    }

    fn complete(
        status: StatusCode,
        protocol: UpstreamProtocol,
        summary: ProbeStreamSummary,
    ) -> Self {
        Self {
            status,
            saw_done: match protocol {
                UpstreamProtocol::ChatCompletions => summary.saw_chat_done,
                UpstreamProtocol::Responses => true,
            },
            saw_text_delta: summary.saw_text_delta,
            saw_usage: summary.saw_usage,
            saw_reasoning_delta: summary.saw_reasoning_delta,
            tool_calls: summary.tool_calls,
            operational_code: None,
        }
    }

    /// The probe observer asked to stop as soon as it saw the evidence it
    /// needed (e.g. the first reasoning delta). The stream is dropped at
    /// that point, so no further chunks are consumed.
    fn early_stopped(status: StatusCode, summary: ProbeStreamSummary) -> Self {
        Self {
            status,
            saw_done: false,
            saw_text_delta: summary.saw_text_delta,
            saw_usage: summary.saw_usage,
            saw_reasoning_delta: summary.saw_reasoning_delta,
            tool_calls: summary.tool_calls,
            operational_code: None,
        }
    }

    fn operational_verdict(&self) -> Option<ProbeCaseVerdict> {
        self.operational_code
            .map(|operational_code| ProbeCaseVerdict::Unobserved {
                operational_code: operational_code.into(),
                http_status: stream_http_status_is_operational(self.status)
                    .then_some(self.status.as_u16()),
            })
    }

    fn has_indexed_tool_arguments(&self, nonce: &str) -> bool {
        has_valid_tool_argument_probe(self.tool_calls.values(), nonce)
    }
}

fn stream_http_status_is_operational(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403 | 429 | 500..=599)
}

fn stream_error_is_incomplete(error: &ProtocolError) -> bool {
    matches!(
        error,
        ProtocolError::InvalidUpstreamStream {
            kind: UpstreamStreamErrorKind::Incomplete,
            ..
        }
    )
}

#[derive(Default)]
struct ProbeStreamSummary {
    saw_chat_done: bool,
    saw_text_delta: bool,
    saw_usage: bool,
    saw_reasoning_delta: bool,
    tool_calls: BTreeMap<u64, ToolArgumentProbe>,
}

impl ProbeStreamSummary {
    fn observe(&mut self, protocol: UpstreamProtocol, event: &SseEvent) {
        let payload = event.data().trim();
        if payload.is_empty() {
            return;
        }
        if payload == "[DONE]" {
            self.saw_chat_done |= protocol == UpstreamProtocol::ChatCompletions;
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        match protocol {
            UpstreamProtocol::ChatCompletions => {
                self.saw_text_delta |= chat_stream_has_text_delta(&value);
                self.saw_usage |= chat_stream_has_usage(&value);
                self.saw_reasoning_delta |= chat_stream_has_reasoning_delta(&value);
                observe_chat_tool_arguments(&mut self.tool_calls, &value);
            }
            UpstreamProtocol::Responses => {
                let event_type = value["type"].as_str().or(event.event_type());
                self.saw_text_delta |= event_type == Some("response.output_text.delta")
                    && value["delta"]
                        .as_str()
                        .is_some_and(|delta| !delta.is_empty());
                self.saw_usage |= event_type == Some("response.completed")
                    && value["response"]["usage"]
                        .as_object()
                        .is_some_and(responses_usage_has_token_field);
                self.saw_reasoning_delta |=
                    responses_stream_has_reasoning_delta(&value, event_type);
                observe_responses_tool_arguments(&mut self.tool_calls, &value, event_type);
            }
        }
    }
}

struct ProbeEvidence {
    protocol: WireProtocol,
    capabilities: BTreeMap<Capability, EvidenceState>,
    token_limit_field: Option<TokenLimitField>,
    reasoning_carrier: Option<ReasoningCarrier>,
    reasoning_controls: BTreeMap<String, Vec<Value>>,
    evidence_codes: BTreeSet<String>,
    extension_evidence: BTreeMap<String, EvidenceState>,
    event_types: BTreeSet<String>,
}

impl ProbeEvidence {
    fn new(protocol: WireProtocol) -> Self {
        let capabilities = Capability::ALL
            .into_iter()
            .map(|capability| (capability, EvidenceState::Unobserved))
            .collect();
        Self {
            protocol,
            capabilities,
            token_limit_field: None,
            reasoning_carrier: None,
            reasoning_controls: BTreeMap::new(),
            evidence_codes: BTreeSet::new(),
            extension_evidence: BTreeMap::new(),
            event_types: BTreeSet::new(),
        }
    }

    fn apply(&mut self, case: &CoreProbeCase, verdict: ProbeCaseVerdict) {
        match &verdict {
            ProbeCaseVerdict::Supported { evidence_code } => {
                self.evidence_codes.insert(evidence_code.clone());
            }
            ProbeCaseVerdict::Rejected { evidence_code, .. } => {
                self.evidence_codes.insert(evidence_code.clone());
            }
            ProbeCaseVerdict::Unobserved {
                operational_code, ..
            } => {
                self.evidence_codes.insert(operational_code.clone());
            }
        }

        match case {
            CoreProbeCase::MinimalText { stream } => {
                if *stream {
                    let state = supported_or_rejected(&verdict);
                    self.capabilities.insert(Capability::TextStream, state);
                    if state == EvidenceState::Supported {
                        self.capabilities
                            .insert(Capability::TextInput, EvidenceState::Supported);
                    }
                } else {
                    let state = supported_or_rejected(&verdict);
                    self.capabilities
                        .insert(Capability::NonStreamingResponse, state);
                    if state == EvidenceState::Supported {
                        self.capabilities
                            .insert(Capability::TextInput, EvidenceState::Supported);
                    }
                }
            }
            CoreProbeCase::FunctionTools => {
                self.capabilities
                    .insert(Capability::FunctionTools, supported_or_rejected(&verdict));
            }
            CoreProbeCase::FunctionSelection => {
                let state = supported_or_rejected(&verdict);
                self.capabilities
                    .insert(Capability::ForcedToolChoice, state);
                if state == EvidenceState::Supported {
                    self.capabilities
                        .insert(Capability::FunctionTools, EvidenceState::Supported);
                }
            }
            CoreProbeCase::ToolContinuation {
                reasoning_carrier, ..
            } => {
                let state = supported_or_rejected(&verdict);
                if reasoning_carrier.is_none() {
                    self.capabilities
                        .insert(Capability::ToolContinuation, state);
                    if state == EvidenceState::Supported {
                        self.capabilities
                            .insert(Capability::FunctionTools, EvidenceState::Supported);
                    }
                } else {
                    self.capabilities.insert(Capability::ReasoningReplay, state);
                    self.capabilities.insert(Capability::ReasoningOutput, state);
                    if state == EvidenceState::Supported {
                        self.capabilities
                            .insert(Capability::FunctionTools, EvidenceState::Supported);
                        self.capabilities
                            .insert(Capability::ToolContinuation, EvidenceState::Supported);
                        self.reasoning_carrier = *reasoning_carrier;
                    }
                }
            }
            CoreProbeCase::IndexedToolArguments => {
                self.capabilities.insert(
                    Capability::IndexedToolArgumentStream,
                    supported_or_rejected(&verdict),
                );
            }
            CoreProbeCase::UsageStream => {
                self.capabilities
                    .insert(Capability::UsageStream, supported_or_rejected(&verdict));
            }
            CoreProbeCase::ParallelTools => {
                self.capabilities.insert(
                    Capability::ParallelToolCalls,
                    supported_or_rejected(&verdict),
                );
            }
            CoreProbeCase::ImageDataUrl => {
                self.capabilities
                    .insert(Capability::ImageDataUrl, supported_or_rejected(&verdict));
            }
            CoreProbeCase::ImageHttps { .. } => {
                self.capabilities
                    .insert(Capability::ImageHttps, supported_or_rejected(&verdict));
            }
            CoreProbeCase::RestrictedResponses => {}
            CoreProbeCase::Declarative(case) => {
                self.extension_evidence
                    .insert(case.id.clone(), supported_or_rejected(&verdict));
            }
            CoreProbeCase::TokenLimit { field } => {
                if supported_or_rejected(&verdict) == EvidenceState::Supported
                    && self.token_limit_field.is_none()
                {
                    self.token_limit_field = Some(*field);
                }
            }
            CoreProbeCase::ReasoningControl { field, value } => {
                if supported_or_rejected(&verdict) == EvidenceState::Supported {
                    let accepted = self.reasoning_controls.entry(field.clone()).or_default();
                    if !accepted.contains(value) {
                        accepted.push(value.clone());
                    }
                }
            }
        }
    }

    fn into_conclusive_outcome(self, attempted_at: u64) -> ProbeOutcome {
        let reasoning_carrier = self.reasoning_carrier.or_else(|| match self.protocol {
            WireProtocol::ChatCompletions
                if self.capabilities.get(&Capability::ReasoningReplay).copied()
                    == Some(EvidenceState::Supported) =>
            {
                Some(ReasoningCarrier::ReasoningContent)
            }
            _ => None,
        });
        ProbeOutcome::Conclusive {
            capabilities: self.capabilities,
            token_limit_field: self.token_limit_field,
            reasoning_carrier,
            reasoning_controls: self.reasoning_controls,
            correction_rules: Vec::new(),
            extension_evidence: self.extension_evidence,
            evidence_codes: self.evidence_codes,
            event_types: self.event_types,
            http_status: StatusCode::OK.as_u16(),
            attempted_at,
        }
    }
}

fn supported_or_rejected(verdict: &ProbeCaseVerdict) -> EvidenceState {
    match verdict {
        ProbeCaseVerdict::Supported { .. } => EvidenceState::Supported,
        ProbeCaseVerdict::Rejected { .. } => EvidenceState::Rejected,
        ProbeCaseVerdict::Unobserved { .. } => EvidenceState::Unobserved,
    }
}

fn has_explicit_zero_output_tokens(body: &Value) -> bool {
    let Some(usage) = body.get("usage").and_then(Value::as_object) else {
        return false;
    };
    let mut saw_output_field = false;
    for field in ["completion_tokens", "output_tokens"] {
        let Some(value) = usage.get(field) else {
            continue;
        };
        saw_output_field = true;
        let parsed = value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()));
        if parsed != Some(0) {
            return false;
        }
    }
    saw_output_field
}

fn chat_has_usable_output(body: &Value) -> bool {
    body["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("message").or_else(|| choice.get("delta")))
        .any(super::chat_message_has_usable_output)
}

fn responses_has_usable_output(body: &Value) -> bool {
    body["output"]
        .as_array()
        .into_iter()
        .flatten()
        .any(super::responses_output_item_has_usable_output)
}

fn responses_has_output_text(body: &Value) -> bool {
    body["status"] == "completed"
        && body["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == "message")
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .any(|part| {
                part["type"] == "output_text"
                    && part["text"].as_str().is_some_and(|text| !text.is_empty())
            })
}

#[derive(Default)]
struct ToolArgumentProbe {
    name: String,
    arguments: String,
    has_call_id: bool,
}

fn observe_chat_tool_arguments(calls: &mut BTreeMap<u64, ToolArgumentProbe>, event: &Value) {
    for call in event["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|choice| {
            choice["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
        })
    {
        let Some(index) = call["index"].as_u64() else {
            continue;
        };
        let current = calls.entry(index).or_default();
        if let Some(name) = call["function"]["name"].as_str() {
            current.name = name.to_owned();
        }
        if let Some(arguments) = call["function"]["arguments"].as_str() {
            current.arguments.push_str(arguments);
        }
        current.has_call_id |= call["id"].as_str().is_some_and(|id| !id.is_empty());
    }
}

fn observe_responses_tool_arguments(
    calls: &mut BTreeMap<u64, ToolArgumentProbe>,
    event: &Value,
    event_type: Option<&str>,
) {
    let Some(index) = event["output_index"].as_u64() else {
        return;
    };
    let current = calls.entry(index).or_default();
    match event_type {
        Some("response.output_item.added") => {
            if let Some(name) = event["item"]["name"].as_str() {
                current.name = name.to_owned();
            }
            if let Some(arguments) = event["item"]["arguments"].as_str() {
                current.arguments.push_str(arguments);
            }
            current.has_call_id |= event["item"]["call_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty());
        }
        Some("response.function_call_arguments.delta") => {
            if let Some(delta) = event["delta"].as_str() {
                current.arguments.push_str(delta);
            }
        }
        Some("response.function_call_arguments.done") => {
            if let Some(arguments) = event["arguments"].as_str() {
                current.arguments = arguments.to_owned();
            }
        }
        _ => {}
    }
}

fn has_valid_tool_argument_probe<'a>(
    calls: impl Iterator<Item = &'a ToolArgumentProbe>,
    nonce: &str,
) -> bool {
    calls.into_iter().any(|call| {
        let arguments = serde_json::from_str::<Value>(&call.arguments).unwrap_or(Value::Null);
        call.name == "gateway_compat_probe" && call.has_call_id && arguments["nonce"] == nonce
    })
}

fn chat_stream_has_text_delta(event: &Value) -> bool {
    event["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|choice| {
            choice["delta"]["content"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        })
}

fn chat_stream_has_usage(event: &Value) -> bool {
    event
        .get("usage")
        .and_then(Value::as_object)
        .is_some_and(|usage| {
            usage.contains_key("prompt_tokens")
                || usage.contains_key("completion_tokens")
                || usage.contains_key("total_tokens")
        })
}

fn responses_usage_has_token_field(usage: &serde_json::Map<String, Value>) -> bool {
    usage.contains_key("input_tokens")
        || usage.contains_key("output_tokens")
        || usage.contains_key("total_tokens")
}

/// Streaming chat evidence of an active reasoning path: a non-empty
/// `delta.reasoning_content` (deepseek-style), a non-null `delta.reasoning`
/// (GLM-style thinking objects), or a positive reasoning token counter in a
/// usage chunk.
fn chat_stream_has_reasoning_delta(event: &Value) -> bool {
    let delta_has_reasoning = event["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|choice| {
            let delta = &choice["delta"];
            delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.is_empty())
                || delta
                    .get("reasoning")
                    .is_some_and(|reasoning| !reasoning.is_null())
        });
    delta_has_reasoning || chat_stream_has_reasoning_tokens(event)
}

fn chat_stream_has_reasoning_tokens(event: &Value) -> bool {
    event
        .get("usage")
        .and_then(Value::as_object)
        .is_some_and(|usage| {
            usage
                .get("completion_tokens_details")
                .and_then(Value::as_object)
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(Value::as_u64)
                .is_some_and(|tokens| tokens > 0)
                || usage
                    .get("output_tokens_details")
                    .and_then(Value::as_object)
                    .and_then(|details| details.get("reasoning_tokens"))
                    .and_then(Value::as_u64)
                    .is_some_and(|tokens| tokens > 0)
        })
}

/// Streaming Responses evidence of an active reasoning path: a
/// `response.reasoning_summary_text.delta` event with a non-empty delta, a
/// `response.output_item.added` event whose item is a reasoning item, or a
/// positive `usage.output_tokens_details.reasoning_tokens` counter on
/// `response.completed`.
fn responses_stream_has_reasoning_delta(event: &Value, event_type: Option<&str>) -> bool {
    if event_type == Some("response.reasoning_summary_text.delta")
        && event
            .get("delta")
            .and_then(Value::as_str)
            .is_some_and(|delta| !delta.is_empty())
    {
        return true;
    }
    if event_type == Some("response.output_item.added")
        && event["item"]["type"].as_str() == Some("reasoning")
    {
        return true;
    }
    if event_type == Some("response.completed")
        && event["response"]["usage"]["output_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    {
        return true;
    }
    false
}
