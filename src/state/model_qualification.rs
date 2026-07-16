use crate::capabilities::{
    Capability, DialectProfileState, EvidenceState, UpstreamDialectProfile,
};
use crate::routing::UpstreamProtocol;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use serde::Serialize;
use std::time::{Duration, Instant};

const QUALIFICATION_RESPONSE_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelQualificationLevel {
    Full,
    Adapted,
    Unusable,
    OperationalFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelQualificationCategory {
    Passed,
    Authentication,
    RateLimit,
    UpstreamUnavailable,
    RequestRejected,
    ModelNotFound,
    MalformedResponse,
    EmptyResponse,
    Timeout,
    Network,
}

impl ModelQualificationCategory {
    pub fn is_operational(self) -> bool {
        matches!(
            self,
            Self::Authentication
                | Self::RateLimit
                | Self::UpstreamUnavailable
                | Self::Timeout
                | Self::Network
        )
    }

    pub fn requires_confirmation(self) -> bool {
        matches!(
            self,
            Self::RequestRejected
                | Self::ModelNotFound
                | Self::MalformedResponse
                | Self::EmptyResponse
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelQualificationEvidence {
    pub upstream_id: String,
    pub key_prefix: String,
    pub model: String,
    pub protocol: UpstreamProtocol,
    pub level: ModelQualificationLevel,
    pub category: ModelQualificationCategory,
    pub latency_ms: u64,
    pub attempted_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DirectQualificationResult {
    pub category: ModelQualificationCategory,
    pub latency_ms: u64,
}

pub async fn qualify_model_on_upstream(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    protocol: UpstreamProtocol,
    timeout_seconds: u64,
) -> DirectQualificationResult {
    let started = Instant::now();
    let endpoint = match protocol {
        UpstreamProtocol::ChatCompletions => "/v1/chat/completions",
        UpstreamProtocol::Responses => "/v1/responses",
    };
    let body = match protocol {
        UpstreamProtocol::ChatCompletions => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "Reply with one short word."}],
            "stream": false
        }),
        UpstreamProtocol::Responses => serde_json::json!({
            "model": model,
            "input": "Reply with one short word.",
            "stream": false
        }),
    };
    let url = crate::util::join_upstream_url(base_url, endpoint);
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(timeout_seconds.max(1)))
        .json(&body)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return qualification_result(
                if error.is_timeout() {
                    ModelQualificationCategory::Timeout
                } else {
                    ModelQualificationCategory::Network
                },
                started,
            );
        }
    };

    let status = response.status();
    let bytes = match read_bounded_response(response).await {
        Ok(bytes) => bytes,
        Err(category) => return qualification_result(category, started),
    };
    let parsed = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let error_code = parsed
        .as_ref()
        .and_then(|value| value.pointer("/error/code"))
        .and_then(serde_json::Value::as_str);
    let category = if matches!(status.as_u16(), 401 | 403) {
        ModelQualificationCategory::Authentication
    } else if status.as_u16() == 429 {
        ModelQualificationCategory::RateLimit
    } else if status.is_server_error() {
        ModelQualificationCategory::UpstreamUnavailable
    } else if status.as_u16() == 404 || error_code == Some("model_not_found") {
        ModelQualificationCategory::ModelNotFound
    } else if !status.is_success() {
        ModelQualificationCategory::RequestRejected
    } else if let Some(payload) = parsed {
        if meaningful_output(protocol, &payload) {
            ModelQualificationCategory::Passed
        } else {
            ModelQualificationCategory::EmptyResponse
        }
    } else {
        ModelQualificationCategory::MalformedResponse
    };

    qualification_result(category, started)
}

fn qualification_result(
    category: ModelQualificationCategory,
    started: Instant,
) -> DirectQualificationResult {
    DirectQualificationResult {
        category,
        latency_ms: started.elapsed().as_millis().max(1) as u64,
    }
}

async fn read_bounded_response(
    response: reqwest::Response,
) -> Result<Bytes, ModelQualificationCategory> {
    let mut stream = response.bytes_stream();
    let mut buffer = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                ModelQualificationCategory::Timeout
            } else {
                ModelQualificationCategory::Network
            }
        })?;
        if buffer.len().saturating_add(chunk.len()) > QUALIFICATION_RESPONSE_BODY_LIMIT {
            return Err(ModelQualificationCategory::MalformedResponse);
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer.freeze())
}

fn non_empty(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(serde_json::Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

fn meaningful_output(protocol: UpstreamProtocol, value: &serde_json::Value) -> bool {
    match protocol {
        UpstreamProtocol::ChatCompletions => value
            .get("choices")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|choices| {
                choices.iter().any(|choice| {
                    let message = choice.get("message");
                    non_empty(message.and_then(|item| item.get("content")))
                        || non_empty(message.and_then(|item| item.get("reasoning_content")))
                        || message
                            .and_then(|item| item.get("tool_calls"))
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|calls| {
                                calls.iter().any(|call| {
                                    non_empty(call.get("id"))
                                        && non_empty(call.pointer("/function/name"))
                                })
                            })
                })
            }),
        UpstreamProtocol::Responses => {
            non_empty(value.get("output_text"))
                || value
                    .get("output")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|items| {
                        items.iter().any(|item| {
                            (matches!(
                                item.get("type").and_then(serde_json::Value::as_str),
                                Some("function_call" | "custom_tool_call")
                            ) && (non_empty(item.get("call_id")) || non_empty(item.get("name"))))
                                || item
                                    .get("content")
                                    .and_then(serde_json::Value::as_array)
                                    .is_some_and(|parts| {
                                        parts.iter().any(|part| {
                                            non_empty(part.get("text"))
                                                || non_empty(part.get("reasoning_text"))
                                        })
                                    })
                        })
                    })
        }
    }
}

pub fn classify_qualification_level(
    category: ModelQualificationCategory,
    profile: Option<&UpstreamDialectProfile>,
) -> ModelQualificationLevel {
    if category.is_operational() {
        return ModelQualificationLevel::OperationalFailure;
    }
    if category != ModelQualificationCategory::Passed {
        return ModelQualificationLevel::Unusable;
    }

    let full = profile.is_some_and(|profile| {
        profile.state == DialectProfileState::Verified
            && [
                Capability::TextInput,
                Capability::TextStream,
                Capability::FunctionTools,
                Capability::ToolContinuation,
            ]
            .into_iter()
            .all(|capability| {
                profile.capabilities.get(&capability) == Some(&EvidenceState::Supported)
            })
    });

    if full {
        ModelQualificationLevel::Full
    } else {
        ModelQualificationLevel::Adapted
    }
}
