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

use crate::capabilities::{
    apply_probe_outcome, Capability, CompiledCapabilityConfiguration, DeclarativeProbeCase,
    DialectProfileKey, EvidenceState, PredicateOperator, ProbeJob, ProbeJobBatch, ProbeOutcome,
    ProbeQueueState, ReasoningCarrier, ResponsePredicate, RouteIdentity, TokenLimitField,
    UpstreamDialectProfile, WireProtocol,
};
use crate::keys::upstream_key_fingerprint;
use crate::protocol::stream_aggregate::{SseEvent, MAX_STREAM_AGGREGATE_TOTAL_BYTES};
use crate::protocol::{
    ProtocolError, StreamAggregateResult, StreamResponseAggregator, UpstreamStreamErrorKind,
};
use crate::routing::UpstreamProtocol;
use crate::state::{join_upstream_url, unix_seconds, AppState, UpstreamConfig};

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
        value: String,
    },
    FunctionTools,
    FunctionSelection,
    ToolContinuation {
        reasoning_carrier: Option<ReasoningCarrier>,
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
                CoreProbeCase::ToolContinuation {
                    reasoning_carrier: None,
                },
                CoreProbeCase::IndexedToolArguments,
                CoreProbeCase::UsageStream,
            ],
        }
    }

    pub fn reasoning_agent() -> Self {
        let mut plan = Self::agent_core();
        plan.cases.push(CoreProbeCase::ToolContinuation {
            reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
        });
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
    let mut plan = match route.protocol {
        WireProtocol::ChatCompletions => ProbePlan::full(),
        WireProtocol::Responses | WireProtocol::Messages => ProbePlan::agent_core(),
    };
    plan.protocol = route.protocol;
    plan.output_token_cap = configuration.source().probe.output_token_cap.min(64);

    let candidates = configuration.probe_candidates_for(route);
    for field in candidates.token_limit_fields {
        if !plan.cases.iter().any(
            |case| matches!(case, CoreProbeCase::TokenLimit { field: existing } if *existing == field),
        ) {
            plan.cases.push(CoreProbeCase::TokenLimit { field });
        }
    }
    for (field, values) in candidates.reasoning_controls {
        for value in values {
            if !plan.cases.iter().any(|case| {
                matches!(case, CoreProbeCase::ReasoningControl { field: existing_field, value: existing_value }
                    if existing_field == &field && existing_value == &value)
            }) {
                plan.cases.push(CoreProbeCase::ReasoningControl {
                    field: field.clone(),
                    value,
                });
            }
        }
    }
    for reasoning_carrier in candidates.reasoning_carriers {
        if !plan.cases.iter().any(|case| {
            matches!(case, CoreProbeCase::ToolContinuation { reasoning_carrier: Some(existing) }
                if *existing == reasoning_carrier)
        }) {
            plan.cases.push(CoreProbeCase::ToolContinuation {
                reasoning_carrier: Some(reasoning_carrier),
            });
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
        probe_state: None,
        upstream: None,
        runtime_model_slug: key.runtime_model_slug.clone(),
        request_timeout: Duration::from_secs(timeout_seconds.max(1)),
    }
    .run_plan(&key, plan)
    .await
}

impl CapabilityProbeService {
    pub fn spawn(state: AppState) -> Self {
        // Capacity bounds pending submission batches. Each accepted batch is
        // expanded synchronously into ProbeQueueState, which deduplicates jobs
        // by exact route key.
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
            let mut receiver_open = true;
            let mut reconcile_tick = tokio::time::interval_at(
                Instant::now() + Duration::from_secs(1),
                Duration::from_secs(1),
            );
            reconcile_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            if let Ok(initial_jobs) = state.reconcile_dialect_profiles(unix_seconds()).await {
                for job in initial_jobs {
                    let _ = queue.enqueue(job);
                }
            }
            loop {
                let capability_snapshot = state.capability_snapshot();
                let probe = &capability_snapshot.configuration.source().probe;
                queue.set_limits(
                    probe.max_global_concurrency,
                    probe.max_per_upstream_concurrency,
                );
                if probe.enabled {
                    while let Some(next) = queue.start_next() {
                        let state = state.clone();
                        active.push(async move {
                            let key = next.key.clone();
                            let binding = next.configuration.clone();
                            let result = run_probe_job(&state, &next).await;
                            (key, binding, result)
                        });
                    }
                } else {
                    queue.clear_pending();
                }

                if active.is_empty() && !receiver_open {
                    break;
                }

                tokio::select! {
                    _ = reconcile_tick.tick() => {
                        if let Ok(jobs) = state.reconcile_dialect_profiles(unix_seconds()).await {
                            for job in jobs {
                                if !queue.enqueue(job) && queue.is_full() {
                                    tracing::warn!("capability probe queue reached its job capacity");
                                }
                            }
                        }
                    }
                    completed = active.next(), if !active.is_empty() => {
                        if let Some((key, binding, result)) = completed {
                            let _ = result;
                            queue.finish(&key);
                            state.finish_capability_probe_submission(&key, &binding);
                        }
                    }
                    received = receiver.recv(), if receiver_open => {
                        match received {
                            Some(batch) => {
                                if state.capability_snapshot().configuration.source().probe.enabled {
                                    for job in batch.into_jobs() {
                                        if !queue.enqueue(job) && queue.is_full() {
                                            tracing::warn!("capability probe queue reached its job capacity");
                                        }
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

pub async fn maybe_queue_dialect_error_probe(
    state: &AppState,
    upstream_id: &str,
    key_fingerprint: &str,
    exposed_model_slug: &str,
    runtime_model_slug: &str,
    protocol: UpstreamProtocol,
    status: StatusCode,
    error_text: &str,
) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let error_lower = error_text.to_ascii_lowercase();
    let indicates_field_error = [
        "unsupported",
        "not supported",
        "unrecognized",
        "unknown field",
        "invalid field",
        "unexpected field",
    ]
    .iter()
    .any(|pattern| error_lower.contains(pattern));
    if !indicates_field_error {
        return false;
    }
    let mentions_dialect_field = [
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
    .any(|field| error_lower.contains(field));
    if !mentions_dialect_field {
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

async fn run_probe_job(state: &AppState, job: &ProbeJob) -> io::Result<()> {
    let routing = state.routing_snapshot().await;
    let Some(upstream) = routing
        .upstreams
        .iter()
        .find(|upstream| upstream.id == job.key.upstream_id && upstream.active)
        .cloned()
    else {
        return Ok(());
    };

    let capability_snapshot = state.capability_snapshot();
    if !AppState::capability_probe_job_is_current(&capability_snapshot, &upstream, job) {
        return Ok(());
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
        return Ok(());
    };
    let api_key = api_key.clone();
    let outcome = ProbeExecutor {
        client: state.client_for_url(&upstream.base_url),
        base_url: upstream.base_url.clone(),
        api_key,
        protocol: job.key.protocol,
        probe_state: Some(state.clone()),
        upstream: Some(upstream.clone()),
        runtime_model_slug: job.key.runtime_model_slug.clone(),
        request_timeout: Duration::from_secs(
            state.config.capability_probe_request_timeout_seconds.max(1),
        ),
    }
    .run_plan(&job.key, plan)
    .await?;

    let mut profile = state
        .capability_snapshot()
        .profiles
        .get(&job.key)
        .cloned()
        .unwrap_or_else(|| UpstreamDialectProfile::unknown(job.key.clone()));
    profile.configuration_fingerprint = job.configuration.configuration_fingerprint.clone();
    profile.probe_schema_version = job.configuration.probe_schema_version;
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
        }
    }
    let _ = state
        .upsert_dialect_profile_if_probe_current(profile, &job.configuration)
        .await?;
    Ok(())
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
}

impl ProbeExecutor {
    async fn run_plan(&self, key: &DialectProfileKey, plan: ProbePlan) -> io::Result<ProbeOutcome> {
        let mut evidence = ProbeEvidence::new(plan.protocol);
        for case in plan.cases {
            let verdict = match tokio::time::timeout(
                self.request_timeout,
                self.run_case(key, &case, plan.output_token_cap.min(64)),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    return Ok(ProbeOutcome::OperationalFailure {
                        code: "probe_timeout".into(),
                        http_status: None,
                        attempted_at: unix_seconds(),
                    });
                }
            };
            match verdict {
                ProbeCaseVerdict::Unobserved {
                    operational_code,
                    http_status,
                } if matches!(http_status, Some(401 | 403 | 429 | 500..=599) | None) => {
                    return Ok(ProbeOutcome::OperationalFailure {
                        code: operational_code,
                        http_status,
                        attempted_at: unix_seconds(),
                    });
                }
                other => evidence.apply(&case, other),
            }
        }
        Ok(evidence.into_conclusive_outcome(unix_seconds()))
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
                        "input": "compat probe",
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
                    "messages": [{"role": "user", "content": "compat probe"}],
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
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "function_tools_missing_call".into(),
                            http_status: Some(response.status.as_u16()),
                        });
                    };
                    let arguments = call["arguments"].as_str().unwrap_or_default();
                    let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                    return if call["name"] == "gateway_compat_probe"
                        && call["call_id"].is_string()
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
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "function_tools_missing_call".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                };
                let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
                let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                if call["function"]["name"] == "gateway_compat_probe"
                    && call["id"].is_string()
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
            CoreProbeCase::ToolContinuation { reasoning_carrier } => {
                let nonce = "n-17";
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
                    let first = self
                        .post_responses(json!({
                            "model": &self.runtime_model_slug,
                            "input": format!("Call gateway_compat_probe with nonce exactly {nonce}."),
                            "tools": tools.clone(),
                        }))
                        .await?;
                    if first.status != StatusCode::OK {
                        return Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "tool_continuation_failed".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    }
                    let Some(output) = first.body["output"].as_array() else {
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "tool_continuation_missing_call".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    };
                    let Some(call) = output.iter().find(|item| item["type"] == "function_call")
                    else {
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "tool_continuation_missing_call".into(),
                            http_status: Some(first.status.as_u16()),
                        });
                    };
                    let arguments = call["arguments"].as_str().unwrap_or_default();
                    let parsed: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                    let Some(call_id) = call["call_id"].as_str() else {
                        return Ok(ProbeCaseVerdict::Rejected {
                            evidence_code: "tool_continuation_missing_call".into(),
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
                let first = self
                    .post_chat(json!({
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
                        }],
                    }))
                    .await?;
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
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "tool_continuation_missing_call".into(),
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
                let Some(call_id) = call["id"].as_str() else {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "tool_continuation_missing_call".into(),
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
                            "input": "compat probe",
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
                        "messages": [{"role": "user", "content": "compat probe"}],
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
                        "input": "compat probe",
                        "stream": false,
                    })
                } else {
                    json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": "compat probe"}],
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
                let mut body = if self.protocol() == WireProtocol::Responses {
                    json!({
                        "model": &self.runtime_model_slug,
                        "input": "compat probe",
                        "stream": false,
                    })
                } else {
                    json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": "compat probe"}],
                        "stream": false,
                    })
                };
                body[field] = Value::String(value.clone());
                let response = if self.protocol() == WireProtocol::Responses {
                    self.post_responses(body).await?
                } else {
                    self.post_chat(body).await?
                };
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
                        "input": "compat probe",
                        "stream": false,
                    })
                } else {
                    json!({
                        "model": &self.runtime_model_slug,
                        "messages": [{"role": "user", "content": "compat probe"}],
                        "stream": false,
                    })
                };
                merge_json_object(&mut body, &case.request_patch);
                let response = if self.protocol() == WireProtocol::Responses {
                    self.post_responses(body).await?
                } else {
                    self.post_chat(body).await?
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

    async fn post_chat(&self, body: Value) -> io::Result<ProbeHttpResponse> {
        let _reservation = self.reserve_upstream_request().await?;
        let url = join_upstream_url(&self.base_url, "/v1/chat/completions");
        let response = self
            .client
            .post(url)
            .bearer_auth(self.api_key.trim())
            .json(&body)
            .send()
            .await
            .map_err(io::Error::other)?;
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
        let _reservation = self.reserve_upstream_request().await?;
        let url = join_upstream_url(&self.base_url, "/v1/responses");
        let response = self
            .client
            .post(url)
            .bearer_auth(self.api_key.trim())
            .json(&body)
            .send()
            .await
            .map_err(io::Error::other)?;
        let status = response.status();
        let body = response.json::<Value>().await.map_err(io::Error::other)?;
        Ok(ProbeHttpResponse { status, body })
    }

    async fn post_responses_stream(&self, body: Value) -> io::Result<ProbeSseResponse> {
        self.post_stream(body, "/v1/responses", UpstreamProtocol::Responses)
            .await
    }

    fn protocol(&self) -> WireProtocol {
        self.protocol
    }

    async fn post_chat_stream(&self, body: Value) -> io::Result<ProbeSseResponse> {
        self.post_stream(
            body,
            "/v1/chat/completions",
            UpstreamProtocol::ChatCompletions,
        )
        .await
    }

    async fn post_stream(
        &self,
        body: Value,
        path: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<ProbeSseResponse> {
        let _reservation = self.reserve_upstream_request().await?;
        let url = join_upstream_url(&self.base_url, path);
        let response = self
            .client
            .post(url)
            .bearer_auth(self.api_key.trim())
            .json(&body)
            .send()
            .await
            .map_err(io::Error::other)?;
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

    async fn reserve_upstream_request(&self) -> io::Result<Option<ProbeUpstreamRequestGuard>> {
        let (Some(state), Some(upstream)) = (&self.probe_state, &self.upstream) else {
            return Ok(None);
        };
        state
            .try_reserve_upstream_request(upstream, &self.runtime_model_slug)
            .await
            .map_err(|error| io::Error::other(error.message))?;
        Ok(Some(ProbeUpstreamRequestGuard {
            state: state.clone(),
            upstream_id: upstream.id.clone(),
        }))
    }
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
    upstream_id: String,
}

impl Drop for ProbeUpstreamRequestGuard {
    fn drop(&mut self) {
        if let Ok(handle) = Handle::try_current() {
            let state = self.state.clone();
            let upstream_id = self.upstream_id.clone();
            handle.spawn(async move {
                state.release_upstream_request(&upstream_id).await;
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
    reasoning_controls: BTreeMap<String, Vec<String>>,
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
            CoreProbeCase::ToolContinuation { reasoning_carrier } => {
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
