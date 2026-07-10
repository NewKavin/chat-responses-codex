use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use axum::http::StatusCode;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use crate::capabilities::{
    apply_probe_outcome, Capability, DeclarativeProbeCase, DialectProfileKey, EvidenceState,
    ProbeJob, ProbeOutcome, ProbeQueueState, ReasoningCarrier, TokenLimitField,
    UpstreamDialectProfile, WireProtocol,
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
                CoreProbeCase::FunctionSelection,
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
    sender: mpsc::Sender<ProbeJob>,
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
    let key = DialectProfileKey {
        upstream_id: "probe-upstream".to_owned(),
        runtime_model_slug: "probe-model".to_owned(),
        protocol: plan.protocol,
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_seconds.max(1)))
        .build()
        .expect("probe test client");
    ProbeExecutor {
        client,
        base_url: base_url.to_owned(),
        api_key: api_key.to_owned(),
        probe_state: None,
        upstream: None,
        runtime_model_slug: key.runtime_model_slug.clone(),
    }
    .run_plan(&key, plan)
    .await
}

impl CapabilityProbeService {
    pub fn spawn(state: AppState) -> Self {
        let (sender, mut receiver) =
            mpsc::channel::<ProbeJob>(state.config.capability_probe_queue_capacity.max(1));
        state.set_capability_probe_sender(sender.clone());
        let service = Self {
            sender: sender.clone(),
        };
        tokio::spawn(async move {
            let mut queue = ProbeQueueState::new(1, 1);
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
                while let Some(next) = queue.start_next() {
                    let _ = run_probe_job(&state, &next).await;
                    queue.finish(&next.key);
                }
                let Some(job) = receiver.recv().await else {
                    break;
                };
                let _ = queue.enqueue(job);
            }
        });
        service
    }

    pub fn sender(&self) -> &mpsc::Sender<ProbeJob> {
        &self.sender
    }
}

pub fn maybe_queue_dialect_error_probe(
    state: &AppState,
    upstream_id: &str,
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
    state.queue_capability_probe(ProbeJob {
        key: DialectProfileKey {
            upstream_id: upstream_id.to_owned(),
            runtime_model_slug: runtime_model_slug.to_owned(),
            protocol: protocol.into(),
        },
        reason: crate::capabilities::ProbeReason::DialectError,
    })
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

    let plan = match job.key.protocol {
        WireProtocol::ChatCompletions => ProbePlan::full(),
        WireProtocol::Responses | WireProtocol::Messages => ProbePlan::agent_core(),
    };
    let api_key = upstream
        .keys_for_model(&job.key.runtime_model_slug)
        .into_iter()
        .next()
        .unwrap_or_else(|| upstream.api_key.clone());
    let outcome = ProbeExecutor {
        client: state.client_for_url(&upstream.base_url),
        base_url: upstream.base_url.clone(),
        api_key,
        probe_state: Some(state.clone()),
        upstream: Some(upstream.clone()),
        runtime_model_slug: job.key.runtime_model_slug.clone(),
    }
    .run_plan(&job.key, plan)
    .await?;

    let mut profile = state
        .capability_snapshot()
        .profiles
        .get(&job.key)
        .cloned()
        .unwrap_or_else(|| UpstreamDialectProfile::unknown(job.key.clone()));
    let fingerprint = state.route_configuration_fingerprint(
        &upstream,
        &job.key.runtime_model_slug,
        &job.key.runtime_model_slug,
        match job.key.protocol {
            WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
            WireProtocol::Responses => UpstreamProtocol::Responses,
            WireProtocol::Messages => UpstreamProtocol::ChatCompletions,
        },
    )?;
    profile.configuration_fingerprint = fingerprint;
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
            correction_rules,
            extension_evidence,
            evidence_codes,
            event_types,
            http_status,
            attempted_at,
        } => {
            profile.probe_schema_version = crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION;
            apply_probe_outcome(
                &mut profile,
                ProbeOutcome::Conclusive {
                    capabilities,
                    token_limit_field,
                    reasoning_carrier,
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
    state.upsert_dialect_profile(profile).await
}

struct ProbeExecutor {
    client: Client,
    base_url: String,
    api_key: String,
    probe_state: Option<AppState>,
    upstream: Option<UpstreamConfig>,
    runtime_model_slug: String,
}

impl ProbeExecutor {
    async fn run_plan(&self, key: &DialectProfileKey, plan: ProbePlan) -> io::Result<ProbeOutcome> {
        let mut evidence = ProbeEvidence::new(plan.protocol);
        for case in plan.cases {
            let verdict = self
                .run_case(key, &case, plan.output_token_cap.min(64))
                .await?;
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
                let mut body = json!({
                    "model": "probe-model",
                    "messages": [{"role": "user", "content": "compat probe"}],
                    "stream": stream,
                    "max_tokens": output_token_cap,
                });
                if *stream {
                    body["stream_options"] = json!({"include_usage": false});
                    let response = self.post_chat_stream(body).await?;
                    let saw_text_delta = response.events.iter().any(chat_stream_has_text_delta);
                    if response.saw_done && saw_text_delta {
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
                    if response.status == StatusCode::OK {
                        Ok(ProbeCaseVerdict::Supported {
                            evidence_code: "minimal_text".into(),
                        })
                    } else {
                        Ok(ProbeCaseVerdict::Unobserved {
                            operational_code: "minimal_text_failed".into(),
                            http_status: Some(response.status.as_u16()),
                        })
                    }
                }
            }
            CoreProbeCase::FunctionSelection => {
                let nonce = "n-17";
                let body = json!({
                    "model": "probe-model",
                    "messages": [{"role": "user", "content": "compat probe"}],
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
                    }],
                    "max_tokens": output_token_cap,
                    "metadata": {"nonce": nonce}
                });
                let response = self.post_chat(body).await?;
                if response.status != StatusCode::OK {
                    return Ok(ProbeCaseVerdict::Unobserved {
                        operational_code: "function_selection_failed".into(),
                        http_status: Some(response.status.as_u16()),
                    });
                }
                let Some(call) = response.body["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .and_then(|calls| calls.first())
                else {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "forced_tool_not_selected".into(),
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
                let first = self
                    .post_chat(json!({
                        "model": "probe-model",
                        "messages": [{"role": "user", "content": "compat probe"}],
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
                        }],
                        "max_tokens": output_token_cap,
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
                let Some(call_id) = call["id"].as_str() else {
                    return Ok(ProbeCaseVerdict::Rejected {
                        evidence_code: "tool_continuation_missing_call".into(),
                        http_status: Some(first.status.as_u16()),
                    });
                };
                let reasoning_content = first.body["choices"][0]["message"]["reasoning_content"]
                    .as_str()
                    .unwrap_or_default();
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
                        "model": "probe-model",
                        "messages": [
                            {"role": "user", "content": "compat probe"},
                            assistant_message,
                            {
                                "role": "tool",
                                "tool_call_id": call_id,
                                "content": format!(r#"{{"nonce":"{nonce}","ok":true}}"#)
                            }
                        ],
                        "max_tokens": output_token_cap,
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
            CoreProbeCase::IndexedToolArguments => Ok(ProbeCaseVerdict::Supported {
                evidence_code: "indexed_tool_arguments_unverified".into(),
            }),
            CoreProbeCase::UsageStream => {
                let response = self
                    .post_chat_stream(json!({
                        "model": "probe-model",
                        "messages": [{"role": "user", "content": "compat probe"}],
                        "stream": true,
                        "stream_options": {"include_usage": true},
                        "max_tokens": output_token_cap,
                    }))
                    .await?;
                let saw_usage = response.events.iter().any(chat_stream_has_usage);
                if response.saw_done && saw_usage {
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
                        "model": "probe-model",
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
                        "max_tokens": output_token_cap,
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
                let response = self
                    .post_chat(json!({
                        "model": "probe-model",
                        "messages": [{
                            "role": "user",
                            "content": [
                                {"type": "text", "text": "Report the dominant color via the probe tool."},
                                {"type": "image_url", "image_url": DATA_URL_IMAGE_FIXTURE}
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
                                "description": "compat probe",
                                "parameters": {
                                    "type": "object",
                                    "properties": {"label": {"type": "string"}},
                                    "required": ["label"]
                                }
                            }
                        }],
                        "max_tokens": output_token_cap,
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
                let response = self
                    .post_chat(json!({
                        "model": "probe-model",
                        "messages": [{
                            "role": "user",
                            "content": [
                                {"type": "text", "text": "Report the dominant color via the probe tool."},
                                {"type": "image_url", "image_url": url}
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
                                "description": "compat probe",
                                "parameters": {
                                    "type": "object",
                                    "properties": {"label": {"type": "string"}},
                                    "required": ["label"]
                                }
                            }
                        }],
                        "max_tokens": output_token_cap,
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
            CoreProbeCase::TokenLimit { .. }
            | CoreProbeCase::ReasoningControl { .. }
            | CoreProbeCase::Declarative(_) => Ok(ProbeCaseVerdict::Rejected {
                evidence_code: "probe_case_unimplemented".into(),
                http_status: None,
            }),
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

    async fn post_chat_stream(&self, body: Value) -> io::Result<ProbeSseResponse> {
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
        let text = response.text().await.map_err(io::Error::other)?;
        let mut events = Vec::new();
        let mut saw_done = false;
        for chunk in text.split("\n\n") {
            let mut payload = None;
            for line in chunk.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    payload = Some(data);
                }
            }
            let Some(payload) = payload else {
                continue;
            };
            if payload.trim() == "[DONE]" {
                saw_done = true;
                continue;
            }
            events.push(serde_json::from_str::<Value>(payload).map_err(io::Error::other)?);
        }
        Ok(ProbeSseResponse {
            status,
            events,
            saw_done,
        })
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
    events: Vec<Value>,
    saw_done: bool,
}

struct ProbeEvidence {
    protocol: WireProtocol,
    capabilities: BTreeMap<Capability, EvidenceState>,
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
                self.capabilities
                    .insert(Capability::TextInput, supported_or_rejected(&verdict));
                if *stream {
                    self.capabilities
                        .insert(Capability::TextStream, supported_or_rejected(&verdict));
                }
            }
            CoreProbeCase::FunctionSelection => {
                let state = supported_or_rejected(&verdict);
                self.capabilities.insert(Capability::FunctionTools, state);
                self.capabilities
                    .insert(Capability::ForcedToolChoice, state);
            }
            CoreProbeCase::ToolContinuation { reasoning_carrier } => {
                let state = supported_or_rejected(&verdict);
                self.capabilities.insert(Capability::FunctionTools, state);
                self.capabilities
                    .insert(Capability::ToolContinuation, state);
                self.capabilities.insert(Capability::ReasoningReplay, state);
                if reasoning_carrier.is_some() && state == EvidenceState::Supported {
                    self.capabilities
                        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
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
            CoreProbeCase::TokenLimit { .. } | CoreProbeCase::ReasoningControl { .. } => {}
        }
    }

    fn into_conclusive_outcome(self, attempted_at: u64) -> ProbeOutcome {
        let rejected_http_status = None;
        let reasoning_carrier = match self.protocol {
            WireProtocol::ChatCompletions
                if self.capabilities.get(&Capability::ReasoningReplay).copied()
                    == Some(EvidenceState::Supported) =>
            {
                Some(ReasoningCarrier::ReasoningContent)
            }
            _ => None,
        };
        ProbeOutcome::Conclusive {
            capabilities: self.capabilities,
            token_limit_field: None,
            reasoning_carrier,
            correction_rules: Vec::new(),
            extension_evidence: self.extension_evidence,
            evidence_codes: self.evidence_codes,
            event_types: self.event_types,
            http_status: rejected_http_status.unwrap_or(200),
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

fn chat_stream_has_text_delta(event: &Value) -> bool {
    event["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|choice| choice["delta"]["content"].as_str().is_some_and(|text| !text.is_empty()))
}

fn chat_stream_has_usage(event: &Value) -> bool {
    event.get("usage").and_then(Value::as_object).is_some_and(|usage| {
        usage.contains_key("prompt_tokens")
            || usage.contains_key("completion_tokens")
            || usage.contains_key("total_tokens")
    })
}
