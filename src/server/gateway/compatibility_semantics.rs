use crate::capabilities::AgentClientProfile;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct SemanticCheckResult {
    pub id: String,
    pub passed: bool,
    pub codes: Vec<String>,
    pub observed_value: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct SemanticExpectation {
    pub require_text_or_reasoning_or_tool_delta: bool,
    pub forced_function: Option<String>,
    pub expected_namespace: Option<(String, String)>,
    pub expected_reasoning_marker: Option<String>,
    pub expected_image_label: Option<String>,
    pub expected_image_tool_receipt: Option<(String, String)>,
    pub require_usage_if_present: bool,
    pub require_linked_continuation: bool,
}

#[derive(Clone, Debug)]
pub struct SemanticValidation {
    pub passed: bool,
    pub codes: Vec<String>,
    pub error_category: Option<String>,
    pub checks: Vec<SemanticCheckResult>,
    pub first_meaningful_event_ms: Option<u64>,
}

impl SemanticExpectation {
    pub fn text() -> Self {
        Self {
            require_text_or_reasoning_or_tool_delta: true,
            forced_function: None,
            expected_namespace: None,
            expected_reasoning_marker: None,
            expected_image_label: None,
            expected_image_tool_receipt: None,
            require_usage_if_present: true,
            require_linked_continuation: false,
        }
    }

    pub fn forced_function(name: &str) -> Self {
        Self {
            forced_function: Some(name.to_owned()),
            require_linked_continuation: true,
            ..Self::text()
        }
    }

    pub fn codex_namespace_reasoning(namespace: &str, member: &str, marker: &str) -> Self {
        Self {
            expected_namespace: Some((namespace.to_owned(), member.to_owned())),
            expected_reasoning_marker: Some(marker.to_owned()),
            require_linked_continuation: true,
            ..Self::text()
        }
    }
}

pub fn validate_client_json(
    profile: AgentClientProfile,
    body: &[u8],
    expected: &SemanticExpectation,
) -> SemanticValidation {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            return SemanticValidation {
                passed: false,
                codes: vec!["invalid_json_body".into()],
                error_category: Some("gateway_protocol_semantic_invalid".into()),
                checks: vec![SemanticCheckResult {
                    id: "json_parse".into(),
                    passed: false,
                    codes: vec!["invalid_json_body".into()],
                    observed_value: None,
                }],
                first_meaningful_event_ms: None,
            }
        }
    };

    match profile {
        AgentClientProfile::Codex => validate_responses_json(&value, expected),
        AgentClientProfile::Opencode | AgentClientProfile::Hermes => {
            validate_chat_json(&value, expected)
        }
        AgentClientProfile::ClaudeCode => validate_messages_json(&value, expected),
    }
}

pub fn validate_client_stream(
    profile: AgentClientProfile,
    body: &[u8],
    expected: &SemanticExpectation,
) -> SemanticValidation {
    let frames = parse_sse_frames(body);
    let mut validation = match profile {
        AgentClientProfile::Codex => validate_responses_stream(&frames, expected),
        AgentClientProfile::Opencode | AgentClientProfile::Hermes => {
            validate_chat_stream(&frames, expected)
        }
        AgentClientProfile::ClaudeCode => validate_messages_stream(&frames, expected),
    };
    if let Some(category) = structured_gateway_sse_error_category(profile, &frames) {
        validation.passed = false;
        validation.error_category = Some(category.clone());
        if !validation.codes.iter().any(|code| code == &category) {
            validation.codes.push(category.clone());
        }
        validation.checks.push(SemanticCheckResult {
            id: "structured_stream_error".into(),
            passed: false,
            codes: vec![category],
            observed_value: None,
        });
    }
    validation
}

#[derive(Clone, Debug)]
struct SseFrame {
    event: Option<String>,
    data: Option<Value>,
    raw_data: String,
}

#[derive(Clone, Debug)]
pub(super) struct StrictMessagesToolTrace {
    pub thinking: String,
    pub signature: String,
    pub tool_use_id: String,
    pub tool_name: String,
    pub tool_input: Value,
}

struct MessagesStreamAnalysis {
    validation: SemanticValidation,
    signed_tool_trace: Option<StrictMessagesToolTrace>,
}

pub(super) fn validate_and_capture_messages_tool_stream(
    body: &[u8],
    expected_tool: &str,
) -> Result<StrictMessagesToolTrace, &'static str> {
    let frames = parse_sse_frames(body);
    let analysis = analyze_messages_stream(
        &frames,
        &SemanticExpectation::forced_function(expected_tool),
    );
    if !analysis.validation.passed {
        return Err("invalid_messages_stream");
    }
    analysis
        .signed_tool_trace
        .ok_or("missing_signed_thinking_trace")
}

pub(super) struct MeaningfulSseEventDetector {
    profile: AgentClientProfile,
    pending: Vec<u8>,
}

impl MeaningfulSseEventDetector {
    pub(super) fn new(profile: AgentClientProfile) -> Self {
        Self {
            profile,
            pending: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> bool {
        self.pending.extend_from_slice(chunk);
        while let Some((frame_end, delimiter_len)) = sse_frame_delimiter(&self.pending) {
            let frame = self.pending[..frame_end].to_vec();
            self.pending.drain(..frame_end + delimiter_len);
            if parse_sse_frame(&String::from_utf8_lossy(&frame))
                .as_ref()
                .is_some_and(|frame| sse_frame_is_meaningful(self.profile, frame))
            {
                return true;
            }
        }
        false
    }
}

fn parse_sse_frames(body: &[u8]) -> Vec<SseFrame> {
    String::from_utf8_lossy(body)
        .replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(parse_sse_frame)
        .collect()
}

fn parse_sse_frame(frame: &str) -> Option<SseFrame> {
    let frame = frame.trim();
    if frame.is_empty() {
        return None;
    }
    let mut event = None;
    let mut data_lines = Vec::new();
    for line in frame.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim_start().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start().to_string());
        }
    }
    if event.is_none() && data_lines.is_empty() {
        return None;
    }
    let raw_data = data_lines.join("\n");
    let data = serde_json::from_str(&raw_data).ok();
    Some(SseFrame {
        event,
        data,
        raw_data,
    })
}

fn structured_gateway_sse_error_category(
    profile: AgentClientProfile,
    frames: &[SseFrame],
) -> Option<String> {
    frames.iter().find_map(|frame| {
        let data = frame.data.as_ref()?.as_object()?;
        let envelope_matches = match profile {
            AgentClientProfile::Codex
            | AgentClientProfile::Opencode
            | AgentClientProfile::Hermes => frame.event.is_none() && data.len() == 1,
            AgentClientProfile::ClaudeCode => {
                frame.event.as_deref() == Some("error")
                    && data.get("type").and_then(Value::as_str) == Some("error")
            }
        };
        if !envelope_matches {
            return None;
        }

        let error = data.get("error")?.as_object()?;
        let category = error.get("category")?.as_str()?;
        let code = error.get("code")?.as_str()?;
        let error_type = error.get("type")?.as_str()?;
        let scope = error.get("details")?.as_object()?.get("scope")?.as_str()?;
        if category.trim().is_empty()
            || code != category
            || !matches!(scope, "gateway" | "upstream")
            || !matches!(
                error_type,
                "api_error"
                    | "authentication_error"
                    | "gateway_access_denied"
                    | "gateway_auth_error"
                    | "invalid_request_error"
                    | "not_found_error"
                    | "permission_error"
                    | "rate_limit_error"
                    | "timeout_error"
                    | "upstream_error"
            )
        {
            return None;
        }
        Some(category.to_string())
    })
}

fn sse_frame_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(lf), Some(crlf)) if lf <= crlf => Some((lf, 2)),
        (Some(_), Some(crlf)) => Some((crlf, 4)),
        (Some(lf), None) => Some((lf, 2)),
        (None, Some(crlf)) => Some((crlf, 4)),
        (None, None) => None,
    }
}

fn sse_frame_is_meaningful(profile: AgentClientProfile, frame: &SseFrame) -> bool {
    let Some(data) = frame.data.as_ref() else {
        return false;
    };
    match profile {
        AgentClientProfile::Codex => {
            let Some(kind) = data.get("type").and_then(Value::as_str) else {
                return false;
            };
            frame.event.as_deref() == Some(kind)
                && match kind {
                    "response.output_text.delta" | "response.reasoning_text.delta" => data
                        .get("delta")
                        .and_then(Value::as_str)
                        .is_some_and(|delta| !delta.is_empty()),
                    "response.function_call_arguments.delta" => data
                        .get("delta")
                        .and_then(Value::as_str)
                        .is_some_and(|delta| !delta.is_empty()),
                    _ => false,
                }
        }
        AgentClientProfile::Opencode | AgentClientProfile::Hermes => data
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .is_some_and(|delta| {
                delta
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
                    || delta
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
                    || delta
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .is_some_and(|calls| calls.iter().any(chat_tool_call_has_delta))
            }),
        AgentClientProfile::ClaudeCode => {
            frame.event.as_deref() == Some("content_block_delta")
                && data.get("type").and_then(Value::as_str) == Some("content_block_delta")
                && match data.pointer("/delta/type").and_then(Value::as_str) {
                    Some("text_delta") => data
                        .pointer("/delta/text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty()),
                    Some("thinking_delta") => data
                        .pointer("/delta/thinking")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty()),
                    Some("input_json_delta") => data
                        .pointer("/delta/partial_json")
                        .and_then(Value::as_str)
                        .is_some_and(|fragment| !fragment.is_empty()),
                    _ => false,
                }
        }
    }
}

fn chat_tool_call_has_delta(call: &Value) -> bool {
    nonempty_string(call.get("id"))
        || call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty())
        || call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .is_some_and(|arguments| !arguments.is_empty())
}

fn validate_responses_json(value: &Value, expected: &SemanticExpectation) -> SemanticValidation {
    let output = value.get("output").and_then(Value::as_array);
    let mut codes = Vec::new();
    let mut checks = Vec::new();

    let envelope_ok = nonempty_string(value.get("id"))
        && value.get("object").and_then(Value::as_str) == Some("response")
        && value.get("status").and_then(Value::as_str) == Some("completed");
    if !envelope_ok {
        codes.push("invalid_response_envelope".into());
    }
    if output.is_some_and(|items| {
        items.iter().any(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("message" | "reasoning" | "function_call")
            ) && !nonempty_string(item.get("id"))
        })
    }) {
        codes.push("missing_output_item_id".into());
    }
    if output.is_some_and(|items| items.iter().any(responses_item_has_invalid_content)) {
        codes.push("invalid_output_content_type".into());
    }

    let meaningful = output.is_some_and(|items| {
        items
            .iter()
            .any(|item| match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    item.get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|parts| {
                            parts.iter().any(|part| {
                                part.get("type").and_then(Value::as_str) == Some("output_text")
                                    && part
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .is_some_and(|text| !text.is_empty())
                            })
                        })
                }
                Some("reasoning") => reasoning_item_has_text(item),
                Some("function_call") => item.get("name").and_then(Value::as_str).is_some(),
                _ => false,
            })
    });
    if expected.require_text_or_reasoning_or_tool_delta && !meaningful {
        codes.push("missing_meaningful_output".into());
    }
    checks.push(SemanticCheckResult {
        id: "meaningful_output".into(),
        passed: meaningful,
        codes: failure_codes(meaningful, "missing_meaningful_output"),
        observed_value: None,
    });
    if let Some(label) = expected.expected_image_label.as_deref() {
        let passed = output.is_some_and(|items| {
            items.iter().any(|item| {
                item.get("content")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part.get("type").and_then(Value::as_str) == Some("output_text")
                                && part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .is_some_and(|text| text.trim() == label)
                        })
                    })
            })
        });
        if !passed {
            codes.push("missing_expected_image_label".into());
        }
        checks.push(SemanticCheckResult {
            id: "image_label".into(),
            passed,
            codes: failure_codes(passed, "missing_expected_image_label"),
            observed_value: None,
        });
    }
    if let Some((label, receipt)) = expected.expected_image_tool_receipt.as_ref() {
        let passed =
            output.is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|parts| {
                            parts.iter().any(|part| {
                                part.get("type").and_then(Value::as_str) == Some("output_text")
                                    && part.get("text").and_then(Value::as_str).is_some_and(
                                        |text| text_has_image_tool_receipt(text, label, receipt),
                                    )
                            })
                        })
                })
            });
        if !passed {
            codes.push("missing_image_tool_receipt".into());
        }
        checks.push(SemanticCheckResult {
            id: "image_tool_receipt".into(),
            passed,
            codes: failure_codes(passed, "missing_image_tool_receipt"),
            observed_value: None,
        });
    }

    if value.get("status").and_then(Value::as_str) != Some("completed") {
        codes.push("missing_completed_status".into());
    }

    let namespace_ok = expected
        .expected_namespace
        .as_ref()
        .map(|(namespace, member)| {
            output.is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(Value::as_str) == Some("function_call")
                        && item.get("name").and_then(Value::as_str) == Some(member.as_str())
                        && item.get("namespace").and_then(Value::as_str) == Some(namespace.as_str())
                })
            })
        });
    if let Some(passed) = namespace_ok {
        if !passed {
            codes.push("missing_expected_namespace".into());
        }
        checks.push(SemanticCheckResult {
            id: "namespace".into(),
            passed,
            codes: if !passed {
                vec!["missing_expected_namespace".into()]
            } else {
                Vec::new()
            },
            observed_value: None,
        });
    }

    if let Some(name) = expected.forced_function.as_deref() {
        let passed = output.is_some_and(|items| {
            items.iter().any(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call")
                    && item.get("name").and_then(Value::as_str) == Some(name)
                    && item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .is_some_and(|arguments| serde_json::from_str::<Value>(arguments).is_ok())
            })
        });
        if !passed {
            codes.push("missing_forced_function".into());
        }
        checks.push(SemanticCheckResult {
            id: "forced_function".into(),
            passed,
            codes: failure_codes(passed, "missing_forced_function"),
            observed_value: None,
        });
    }

    if expected.require_linked_continuation {
        let passed = output.is_some_and(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                .any(|item| nonempty_string(item.get("call_id")))
        });
        if !passed {
            codes.push("missing_linked_continuation".into());
        }
        checks.push(SemanticCheckResult {
            id: "linked_continuation".into(),
            passed,
            codes: failure_codes(passed, "missing_linked_continuation"),
            observed_value: None,
        });
    }
    validate_json_usage(
        value.get("usage"),
        expected,
        UsageProtocol::Responses,
        &mut codes,
        &mut checks,
    );

    let reasoning_ok = expected.expected_reasoning_marker.as_ref().map(|marker| {
        output.is_some_and(|items| {
            items.iter().any(|item| {
                item.get("type").and_then(Value::as_str) == Some("reasoning")
                    && item
                        .get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|parts| {
                            parts.iter().any(|part| {
                                part.get("text")
                                    .and_then(Value::as_str)
                                    .is_some_and(|text| text.trim() == marker)
                            })
                        })
            })
        })
    });
    if let Some(passed) = reasoning_ok {
        if !passed {
            codes.push("missing_reasoning_marker".into());
        }
        checks.push(SemanticCheckResult {
            id: "reasoning".into(),
            passed,
            codes: if !passed {
                vec!["missing_reasoning_marker".into()]
            } else {
                Vec::new()
            },
            observed_value: None,
        });
    }

    let passed = codes.is_empty();
    let error_category = if !passed
        && expected.forced_function.is_some()
        && codes.iter().any(|code| code == "missing_forced_function")
    {
        Some("gateway_model_semantic_incompatible".into())
    } else {
        semantic_error_category(passed)
    };
    SemanticValidation {
        passed,
        codes,
        error_category,
        checks,
        first_meaningful_event_ms: None,
    }
}

fn validate_chat_json(value: &Value, expected: &SemanticExpectation) -> SemanticValidation {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());
    let message = choice.and_then(|choice| choice.get("message"));
    let mut codes = Vec::new();
    let mut checks = Vec::new();
    let mut error_category = None;

    let meaningful = message.is_some_and(|message| {
        message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
            || message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
            || message
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty())
    });
    if expected.require_text_or_reasoning_or_tool_delta && !meaningful {
        codes.push("missing_meaningful_output".into());
    }
    checks.push(SemanticCheckResult {
        id: "meaningful_output".into(),
        passed: meaningful,
        codes: failure_codes(meaningful, "missing_meaningful_output"),
        observed_value: None,
    });
    if let Some(label) = expected.expected_image_label.as_deref() {
        let passed = message
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|text| text.trim() == label);
        if !passed {
            codes.push("missing_expected_image_label".into());
        }
        checks.push(SemanticCheckResult {
            id: "image_label".into(),
            passed,
            codes: failure_codes(passed, "missing_expected_image_label"),
            observed_value: None,
        });
    }
    if let Some((label, receipt)) = expected.expected_image_tool_receipt.as_ref() {
        let passed = message
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|text| text_has_image_tool_receipt(text, label, receipt));
        if !passed {
            codes.push("missing_image_tool_receipt".into());
        }
        checks.push(SemanticCheckResult {
            id: "image_tool_receipt".into(),
            passed,
            codes: failure_codes(passed, "missing_image_tool_receipt"),
            observed_value: None,
        });
    }
    let expected_finish_reason = if expected.forced_function.is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    if choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        != Some(expected_finish_reason)
    {
        codes.push("invalid_finish_reason".into());
    }

    if let Some(name) = expected.forced_function.as_deref() {
        let passed = message.is_some_and(|message| {
            message
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|tool_calls| {
                    tool_calls.iter().any(|tool_call| {
                        let name_matches = tool_call
                            .get("function")
                            .and_then(Value::as_object)
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                            == Some(name);
                        let arguments_parse = tool_call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .is_some_and(|arguments| {
                                serde_json::from_str::<Value>(arguments).is_ok()
                            });
                        name_matches && arguments_parse
                    })
                })
        });
        if !passed {
            codes.push("missing_forced_function".into());
            error_category = Some("gateway_model_semantic_incompatible".into());
        }
        checks.push(SemanticCheckResult {
            id: "forced_function".into(),
            passed,
            codes: if !passed {
                vec!["missing_forced_function".into()]
            } else {
                Vec::new()
            },
            observed_value: None,
        });
    }

    if expected.require_linked_continuation {
        let passed = message
            .and_then(|message| message.get("tool_calls"))
            .and_then(Value::as_array)
            .is_some_and(|calls| calls.iter().any(|call| nonempty_string(call.get("id"))));
        if !passed {
            codes.push("missing_linked_continuation".into());
        }
        checks.push(SemanticCheckResult {
            id: "linked_continuation".into(),
            passed,
            codes: failure_codes(passed, "missing_linked_continuation"),
            observed_value: None,
        });
    }
    validate_json_usage(
        value.get("usage"),
        expected,
        UsageProtocol::Chat,
        &mut codes,
        &mut checks,
    );

    SemanticValidation {
        passed: codes.is_empty(),
        codes,
        error_category,
        checks,
        first_meaningful_event_ms: None,
    }
}

fn validate_messages_json(value: &Value, expected: &SemanticExpectation) -> SemanticValidation {
    let content = value.get("content").and_then(Value::as_array);
    let mut codes = Vec::new();
    let mut checks = Vec::new();
    let meaningful = content.is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty()),
                Some("thinking") => block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty()),
                Some("tool_use") => block.get("name").and_then(Value::as_str).is_some(),
                _ => false,
            })
    });
    if expected.require_text_or_reasoning_or_tool_delta && !meaningful {
        codes.push("missing_meaningful_output".into());
    }
    checks.push(SemanticCheckResult {
        id: "meaningful_output".into(),
        passed: meaningful,
        codes: failure_codes(meaningful, "missing_meaningful_output"),
        observed_value: None,
    });
    if let Some(label) = expected.expected_image_label.as_deref() {
        let passed = content.is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.trim() == label)
            })
        });
        if !passed {
            codes.push("missing_expected_image_label".into());
        }
        checks.push(SemanticCheckResult {
            id: "image_label".into(),
            passed,
            codes: failure_codes(passed, "missing_expected_image_label"),
            observed_value: None,
        });
    }
    if let Some((label, receipt)) = expected.expected_image_tool_receipt.as_ref() {
        let passed = content.is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text_has_image_tool_receipt(text, label, receipt))
            })
        });
        if !passed {
            codes.push("missing_image_tool_receipt".into());
        }
        checks.push(SemanticCheckResult {
            id: "image_tool_receipt".into(),
            passed,
            codes: failure_codes(passed, "missing_image_tool_receipt"),
            observed_value: None,
        });
    }

    let expected_stop_reason = if expected.forced_function.is_some() {
        "tool_use"
    } else {
        "end_turn"
    };
    if value.get("stop_reason").and_then(Value::as_str) != Some(expected_stop_reason) {
        codes.push("invalid_stop_reason".into());
    }
    if let Some(name) = expected.forced_function.as_deref() {
        let passed = content.is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && block.get("name").and_then(Value::as_str) == Some(name)
                    && block.get("input").is_some_and(Value::is_object)
            })
        });
        if !passed {
            codes.push("missing_forced_function".into());
        }
        checks.push(SemanticCheckResult {
            id: "forced_function".into(),
            passed,
            codes: failure_codes(passed, "missing_forced_function"),
            observed_value: None,
        });
    }
    if expected.require_linked_continuation {
        let passed = content.is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && nonempty_string(block.get("id"))
            })
        });
        if !passed {
            codes.push("missing_linked_continuation".into());
        }
        checks.push(SemanticCheckResult {
            id: "linked_continuation".into(),
            passed,
            codes: failure_codes(passed, "missing_linked_continuation"),
            observed_value: None,
        });
    }
    validate_json_usage(
        value.get("usage"),
        expected,
        UsageProtocol::Messages,
        &mut codes,
        &mut checks,
    );
    if let Some(marker) = expected.expected_reasoning_marker.as_deref() {
        let thinking = content.and_then(|blocks| {
            blocks.iter().find(|block| {
                block.get("type").and_then(Value::as_str) == Some("thinking")
                    && block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.trim() == marker)
            })
        });
        if thinking.is_none() {
            codes.push("missing_reasoning_marker".into());
        } else if !thinking.is_some_and(|block| {
            block
                .get("signature")
                .and_then(Value::as_str)
                .is_some_and(|signature| !signature.is_empty())
        }) {
            codes.push("missing_thinking_signature".into());
        }
    }

    let passed = codes.is_empty();
    let error_category = if !passed
        && expected.forced_function.is_some()
        && codes.iter().any(|code| code == "missing_forced_function")
    {
        Some("gateway_model_semantic_incompatible".into())
    } else {
        semantic_error_category(passed)
    };
    SemanticValidation {
        passed,
        codes,
        error_category,
        checks,
        first_meaningful_event_ms: None,
    }
}

fn validate_responses_stream(
    frames: &[SseFrame],
    expected: &SemanticExpectation,
) -> SemanticValidation {
    let mut codes = Vec::new();
    let has_meaningful = frames
        .iter()
        .any(|frame| sse_frame_is_meaningful(AgentClientProfile::Codex, frame));
    if expected.require_text_or_reasoning_or_tool_delta && !has_meaningful {
        codes.push("missing_meaningful_event".into());
    }
    if !frames.iter().any(|frame| {
        frame.event.as_deref() == Some("response.completed")
            && frame
                .data
                .as_ref()
                .and_then(|data| data.get("type"))
                .and_then(Value::as_str)
                == Some("response.completed")
            && frame
                .data
                .as_ref()
                .and_then(|data| data.pointer("/response/status"))
                .and_then(Value::as_str)
                == Some("completed")
    }) {
        codes.push("missing_response_completed".into());
    }

    let mut output_items = frames
        .iter()
        .filter_map(|frame| frame.data.as_ref())
        .filter_map(|data| data.get("item"))
        .collect::<Vec<_>>();
    output_items.extend(
        frames
            .iter()
            .filter_map(|frame| frame.data.as_ref())
            .filter(|data| data.get("type").and_then(Value::as_str) == Some("response.completed"))
            .filter_map(|data| data.pointer("/response/output").and_then(Value::as_array))
            .flatten(),
    );
    if has_meaningful
        && (output_items.is_empty()
            || output_items.iter().any(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("message" | "reasoning" | "function_call")
                ) && !nonempty_string(item.get("id"))
            }))
    {
        codes.push("missing_output_item_id".into());
    }
    if let Some(name) = expected.forced_function.as_deref() {
        let found_name = output_items.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some(name)
        });
        let arguments_in_item = output_items.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some(name)
                && item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .is_some_and(|arguments| serde_json::from_str::<Value>(arguments).is_ok())
        });
        let arguments_done = frames
            .iter()
            .filter_map(|frame| frame.data.as_ref())
            .any(|data| {
                data.get("type").and_then(Value::as_str)
                    == Some("response.function_call_arguments.done")
                    && data
                        .get("arguments")
                        .and_then(Value::as_str)
                        .is_some_and(|arguments| serde_json::from_str::<Value>(arguments).is_ok())
            });
        if !found_name || (!arguments_in_item && !arguments_done) {
            codes.push("missing_forced_function".into());
        }
        let argument_fragment_count = frames
            .iter()
            .filter_map(|frame| frame.data.as_ref())
            .filter(|data| {
                data.get("type").and_then(Value::as_str)
                    == Some("response.function_call_arguments.delta")
                    && data
                        .get("delta")
                        .and_then(Value::as_str)
                        .is_some_and(|fragment| !fragment.is_empty())
            })
            .count();
        if argument_fragment_count < 2 {
            codes.push("missing_fragmented_arguments".into());
        }
    }
    if expected.require_linked_continuation
        && !output_items.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && nonempty_string(item.get("call_id"))
        })
    {
        codes.push("missing_linked_continuation".into());
    }
    if expected.require_usage_if_present {
        let usages = frames
            .iter()
            .filter_map(|frame| frame.data.as_ref())
            .filter_map(|data| {
                (data.get("type").and_then(Value::as_str) == Some("response.completed"))
                    .then(|| data.pointer("/response/usage"))
                    .flatten()
            })
            .collect::<Vec<_>>();
        if usages
            .iter()
            .any(|usage| !usage_is_valid(usage, UsageProtocol::Responses))
        {
            codes.push("invalid_usage".into());
        }
    }
    if let Some((namespace, member)) = expected.expected_namespace.as_ref() {
        let found = output_items.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some(member)
                && item.get("namespace").and_then(Value::as_str) == Some(namespace)
        });
        if !found {
            codes.push("missing_expected_namespace".into());
        }
    }
    if let Some(marker) = expected.expected_reasoning_marker.as_deref() {
        let streamed_reasoning = frames
            .iter()
            .filter_map(|frame| frame.data.as_ref())
            .filter_map(|data| {
                (data.get("type").and_then(Value::as_str) == Some("response.reasoning_text.delta"))
                    .then(|| data.get("delta").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<String>();
        let found = streamed_reasoning.trim() == marker
            || output_items
                .iter()
                .any(|item| reasoning_item_contains(item, marker));
        if !found {
            codes.push("missing_reasoning_marker".into());
        }
    }
    finalize_stream_validation_with_expectations(codes, "responses_stream", expected)
}

fn chat_delta_is_valid(delta: &Value) -> bool {
    let Some(delta) = delta.as_object() else {
        return false;
    };
    for field in ["role", "content", "reasoning_content", "refusal"] {
        if delta
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return false;
        }
    }
    delta
        .get("tool_calls")
        .map(|tool_calls| {
            tool_calls.as_array().is_some_and(|tool_calls| {
                tool_calls.iter().all(|tool_call| {
                    tool_call.get("index").and_then(Value::as_u64).is_some()
                        && tool_call.get("id").is_none_or(|value| value.is_string())
                        && tool_call.get("type").is_none_or(|value| value.is_string())
                        && tool_call.get("function").is_none_or(|function| {
                            function.as_object().is_some_and(|function| {
                                function.get("name").is_none_or(|value| value.is_string())
                                    && function
                                        .get("arguments")
                                        .is_none_or(|value| value.is_string())
                            })
                        })
                })
            })
        })
        .unwrap_or(true)
}

fn chat_stream_chunk_is_valid(data: &Value) -> bool {
    let Some(object) = data.as_object() else {
        return false;
    };
    let envelope_valid = nonempty_string(object.get("id"))
        && object.get("object").and_then(Value::as_str) == Some("chat.completion.chunk")
        && object.get("created").and_then(Value::as_u64).is_some()
        && nonempty_string(object.get("model"));
    if !envelope_valid {
        return false;
    }
    let Some(choices) = object.get("choices").and_then(Value::as_array) else {
        return false;
    };
    if choices.is_empty() {
        return object
            .get("usage")
            .is_some_and(|usage| usage_is_valid(usage, UsageProtocol::Chat));
    }
    if object.get("usage").is_some_and(|usage| !usage.is_null()) {
        return false;
    }
    choices.iter().all(|choice| {
        choice.get("index").and_then(Value::as_u64).is_some()
            && choice.get("delta").is_some_and(chat_delta_is_valid)
            && choice
                .get("finish_reason")
                .is_some_and(|reason| reason.is_null() || reason.is_string())
    })
}

fn chat_stream_chunk_is_usage_only(data: &Value) -> bool {
    data.get("choices")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
        && data.get("usage").is_some_and(|usage| !usage.is_null())
}

fn validate_chat_stream(frames: &[SseFrame], expected: &SemanticExpectation) -> SemanticValidation {
    let mut codes = Vec::new();
    let mut calls = BTreeMap::<u64, (String, String, String)>::new();
    let mut argument_fragment_counts = BTreeMap::<u64, usize>::new();
    let mut reasoning = String::new();
    let has_meaningful = frames
        .iter()
        .any(|frame| sse_frame_is_meaningful(AgentClientProfile::Opencode, frame));
    if expected.require_text_or_reasoning_or_tool_delta && !has_meaningful {
        codes.push("missing_meaningful_event".into());
    }
    if frames
        .iter()
        .any(|frame| frame.data.is_none() && frame.raw_data.trim() != "[DONE]")
    {
        codes.push("invalid_sse_data".into());
    }
    if frames
        .iter()
        .filter_map(|frame| frame.data.as_ref())
        .any(|data| !chat_stream_chunk_is_valid(data))
    {
        codes.push("invalid_chat_chunk_envelope".into());
    }
    let done_indices = frames
        .iter()
        .enumerate()
        .filter_map(|(index, frame)| (frame.raw_data.trim() == "[DONE]").then_some(index))
        .collect::<Vec<_>>();
    if done_indices.is_empty() {
        codes.push("missing_done".into());
    }
    if done_indices.len() > 1 {
        codes.push("invalid_done_count".into());
    }
    if done_indices
        .first()
        .is_some_and(|done_index| *done_index + 1 != frames.len())
    {
        codes.push("invalid_done_order".into());
    }
    let expected_finish_reason = if expected.forced_function.is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    let finish_events = frames
        .iter()
        .enumerate()
        .filter_map(|(index, frame)| frame.data.as_ref().map(|data| (index, data)))
        .flat_map(|data| {
            data.1
                .get("choices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |choice| {
                    choice
                        .get("finish_reason")
                        .and_then(Value::as_str)
                        .map(|reason| (data.0, reason))
                })
        })
        .collect::<Vec<_>>();
    if finish_events.len() != 1 || finish_events[0].1 != expected_finish_reason {
        codes.push("invalid_finish_reason".into());
    }
    if let (Some(done_index), Some((finish_index, _))) =
        (done_indices.first(), finish_events.first())
    {
        if finish_index >= done_index {
            codes.push("invalid_done_order".into());
        }
    }
    if let [(finish_index, _)] = finish_events.as_slice() {
        let terminal_suffix = &frames[finish_index + 1..];
        let terminal_order_valid = match terminal_suffix {
            [done] => done.raw_data.trim() == "[DONE]",
            [usage, done] => {
                usage.data.as_ref().is_some_and(|data| {
                    chat_stream_chunk_is_usage_only(data) && chat_stream_chunk_is_valid(data)
                }) && done.raw_data.trim() == "[DONE]"
            }
            _ => false,
        };
        if !terminal_order_valid {
            codes.push("invalid_terminal_order".into());
        }
    }
    let usage_only_indices = frames
        .iter()
        .enumerate()
        .filter_map(|(index, frame)| {
            frame
                .data
                .as_ref()
                .is_some_and(chat_stream_chunk_is_usage_only)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if usage_only_indices.len() > 1
        || usage_only_indices.first().is_some_and(|usage_index| {
            done_indices.first() != Some(&(*usage_index + 1))
                || finish_events
                    .first()
                    .is_none_or(|(finish_index, _)| finish_index >= usage_index)
        })
    {
        codes.push("invalid_chat_usage_chunk_order".into());
    }
    for data in frames.iter().filter_map(|frame| frame.data.as_ref()) {
        let Some(delta) = data
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
        else {
            continue;
        };
        if let Some(fragment) = delta.get("reasoning_content").and_then(Value::as_str) {
            reasoning.push_str(fragment);
        }
        let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for tool_call in tool_calls {
            let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0);
            let entry = calls.entry(index).or_default();
            if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                entry.0.push_str(id);
            }
            if let Some(name) = tool_call.pointer("/function/name").and_then(Value::as_str) {
                entry.1.push_str(name);
            }
            if let Some(arguments) = tool_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
            {
                entry.2.push_str(arguments);
                if !arguments.is_empty() {
                    *argument_fragment_counts.entry(index).or_default() += 1;
                }
            }
        }
    }
    if let Some(name) = expected.forced_function.as_deref() {
        let found = calls.values().any(|(_, observed_name, arguments)| {
            observed_name == name && serde_json::from_str::<Value>(arguments).is_ok()
        });
        if !found {
            codes.push("missing_forced_function".into());
        }
        if !argument_fragment_counts.values().any(|count| *count >= 2) {
            codes.push("missing_fragmented_arguments".into());
        }
    }
    if calls
        .values()
        .any(|(_, _, arguments)| serde_json::from_str::<Value>(arguments).is_err())
    {
        codes.push("invalid_tool_arguments".into());
    }
    if expected.require_linked_continuation && !calls.values().any(|(id, _, _)| !id.is_empty()) {
        codes.push("missing_linked_continuation".into());
    }
    if expected.require_usage_if_present
        && frames
            .iter()
            .filter_map(|frame| frame.data.as_ref())
            .filter_map(|data| data.get("usage").filter(|usage| !usage.is_null()))
            .any(|usage| !usage_is_valid(usage, UsageProtocol::Chat))
    {
        codes.push("invalid_usage".into());
    }
    if let Some(marker) = expected.expected_reasoning_marker.as_deref() {
        if reasoning.trim() != marker {
            codes.push("missing_reasoning_marker".into());
        }
    }
    finalize_stream_validation_with_expectations(codes, "chat_stream", expected)
}

fn validate_messages_stream(
    frames: &[SseFrame],
    expected: &SemanticExpectation,
) -> SemanticValidation {
    analyze_messages_stream(frames, expected).validation
}

fn analyze_messages_stream(
    frames: &[SseFrame],
    expected: &SemanticExpectation,
) -> MessagesStreamAnalysis {
    let mut codes = Vec::new();
    let mut started = false;
    let mut message_delta_seen = false;
    let mut stopped = false;
    let mut open_blocks = BTreeMap::<u64, (String, Vec<String>)>::new();
    let mut next_block_index = 0_u64;
    let mut tool_names = Vec::new();
    let mut tool_ids = Vec::new();
    let mut tool_blocks = BTreeMap::<u64, (String, String)>::new();
    let mut tool_arguments = BTreeMap::<u64, String>::new();
    let mut tool_argument_fragment_counts = BTreeMap::<u64, usize>::new();
    let mut thinking = String::new();
    let mut thinking_signatures = Vec::new();
    for frame in frames {
        let event = frame.event.as_deref();
        let data = frame.data.as_ref();
        let Some(event_name) = event else {
            codes.push("missing_messages_event_name".into());
            continue;
        };
        if data
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            != Some(event_name)
        {
            codes.push("invalid_messages_event_type".into());
            continue;
        }
        match event {
            Some("message_start") if !started && !stopped => started = true,
            Some("content_block_start") if started && !message_delta_seen && !stopped => {
                let index = data
                    .and_then(|value| value.get("index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                let kind = data
                    .and_then(|value| value.pointer("/content_block/type"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if index == u64::MAX
                    || index != next_block_index
                    || !open_blocks.is_empty()
                    || !matches!(kind.as_str(), "text" | "thinking" | "tool_use")
                {
                    codes.push("invalid_content_block_order".into());
                    continue;
                }
                open_blocks.insert(index, (kind.clone(), Vec::new()));
                next_block_index += 1;
                if kind == "tool_use" {
                    let id = data
                        .and_then(|value| value.pointer("/content_block/id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = data
                        .and_then(|value| value.pointer("/content_block/name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !id.is_empty() {
                        tool_ids.push(id.to_string());
                    }
                    if !name.is_empty() {
                        tool_names.push(name.to_string());
                    }
                    tool_blocks.insert(index, (id.to_string(), name.to_string()));
                }
            }
            Some("content_block_delta") if started && !message_delta_seen && !stopped => {
                let index = data
                    .and_then(|value| value.get("index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                let Some((block_kind, signatures)) = open_blocks.get_mut(&index) else {
                    codes.push("invalid_content_block_order".into());
                    continue;
                };
                match data
                    .and_then(|value| value.pointer("/delta/type"))
                    .and_then(Value::as_str)
                {
                    Some("input_json_delta") if block_kind == "tool_use" => {
                        if let Some(fragment) = data
                            .and_then(|value| value.pointer("/delta/partial_json"))
                            .and_then(Value::as_str)
                        {
                            tool_arguments.entry(index).or_default().push_str(fragment);
                            if !fragment.is_empty() {
                                *tool_argument_fragment_counts.entry(index).or_default() += 1;
                            }
                        }
                    }
                    Some("thinking_delta") if block_kind == "thinking" => {
                        if let Some(fragment) = data
                            .and_then(|value| value.pointer("/delta/thinking"))
                            .and_then(Value::as_str)
                        {
                            thinking.push_str(fragment);
                        }
                    }
                    Some("signature_delta") if block_kind == "thinking" => {
                        let signature = data
                            .and_then(|value| value.pointer("/delta/signature"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if signature.is_empty() {
                            codes.push("invalid_thinking_signature".into());
                        } else {
                            signatures.push(signature.to_string());
                        }
                        if signatures.len() > 1 {
                            codes.push("invalid_thinking_signature".into());
                        }
                    }
                    Some("text_delta") if block_kind == "text" => {}
                    _ => codes.push("invalid_content_block_delta".into()),
                }
            }
            Some("content_block_stop") if started && !message_delta_seen && !stopped => {
                let index = data
                    .and_then(|value| value.get("index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                match open_blocks.remove(&index) {
                    Some((kind, signatures)) if kind == "thinking" => {
                        if signatures.is_empty() {
                            codes.push("missing_thinking_signature".into());
                        } else if signatures.len() == 1 {
                            thinking_signatures.push(signatures[0].clone());
                        } else {
                            codes.push("invalid_thinking_signature".into());
                        }
                    }
                    Some((_, signatures)) if signatures.is_empty() => {}
                    Some(_) => codes.push("invalid_thinking_signature".into()),
                    None => codes.push("invalid_content_block_order".into()),
                }
            }
            Some("message_delta") if started && open_blocks.is_empty() && !stopped => {
                message_delta_seen = true;
            }
            Some("message_stop") if started && message_delta_seen && !stopped => stopped = true,
            Some("ping") => {}
            Some("error") => codes.push("messages_error_event".into()),
            Some(_) => codes.push("invalid_messages_event_order".into()),
            None => unreachable!("missing event names are handled before the state machine"),
        }
    }
    let has_delta = frames
        .iter()
        .any(|frame| sse_frame_is_meaningful(AgentClientProfile::ClaudeCode, frame));
    if expected.require_text_or_reasoning_or_tool_delta && !has_delta {
        codes.push("missing_content_block_delta".into());
    }
    if !frames
        .iter()
        .any(|frame| frame.event.as_deref() == Some("message_delta"))
    {
        codes.push("missing_message_delta".into());
    }
    if !frames
        .iter()
        .any(|frame| frame.event.as_deref() == Some("message_stop"))
    {
        codes.push("missing_message_stop".into());
    }
    if !started || !stopped || !open_blocks.is_empty() {
        codes.push("invalid_messages_event_order".into());
    }
    let expected_stop_reason = if expected.forced_function.is_some() {
        "tool_use"
    } else {
        "end_turn"
    };
    let stop_reasons = frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("message_delta"))
        .filter_map(|frame| frame.data.as_ref())
        .filter_map(|data| data.pointer("/delta/stop_reason").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if stop_reasons.as_slice() != [expected_stop_reason] {
        codes.push("invalid_stop_reason".into());
    }
    if let Some(name) = expected.forced_function.as_deref() {
        if !tool_names.iter().any(|observed| observed == name) {
            codes.push("missing_forced_function".into());
        }
        if tool_arguments.is_empty()
            || tool_arguments
                .values()
                .any(|arguments| serde_json::from_str::<Value>(arguments).is_err())
        {
            codes.push("invalid_tool_arguments".into());
        }
        if !tool_argument_fragment_counts
            .values()
            .any(|count| *count >= 2)
        {
            codes.push("missing_fragmented_arguments".into());
        }
    }
    if expected.require_linked_continuation && !tool_ids.iter().any(|id| !id.is_empty()) {
        codes.push("missing_linked_continuation".into());
    }
    if expected.require_usage_if_present
        && frames
            .iter()
            .filter_map(|frame| frame.data.as_ref())
            .filter_map(|data| data.pointer("/message/usage").or_else(|| data.get("usage")))
            .any(|usage| !usage_is_valid(usage, UsageProtocol::Messages))
    {
        codes.push("invalid_usage".into());
    }
    if let Some(marker) = expected.expected_reasoning_marker.as_deref() {
        if thinking.trim() != marker {
            codes.push("missing_reasoning_marker".into());
        }
        if thinking_signatures.is_empty() {
            codes.push("missing_thinking_signature".into());
        }
    }
    codes.sort();
    codes.dedup();
    let validation =
        finalize_stream_validation_with_expectations(codes, "messages_stream", expected);
    let signed_tool_trace = validation
        .passed
        .then(|| {
            let expected_tool = expected.forced_function.as_deref()?;
            if thinking.is_empty() || thinking_signatures.len() != 1 {
                return None;
            }
            let (index, (tool_use_id, tool_name)) = tool_blocks
                .iter()
                .find(|(_, (_, name))| name == expected_tool)?;
            if tool_use_id.is_empty() {
                return None;
            }
            let tool_input = serde_json::from_str(tool_arguments.get(index)?).ok()?;
            Some(StrictMessagesToolTrace {
                thinking: thinking.clone(),
                signature: thinking_signatures[0].clone(),
                tool_use_id: tool_use_id.clone(),
                tool_name: tool_name.clone(),
                tool_input,
            })
        })
        .flatten();
    MessagesStreamAnalysis {
        validation,
        signed_tool_trace,
    }
}

fn reasoning_item_contains(item: &Value, marker: &str) -> bool {
    item.get("type").and_then(Value::as_str) == Some("reasoning")
        && item
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                        && part
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.trim() == marker)
                })
            })
}

fn reasoning_item_has_text(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("reasoning")
        && item
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                        && part
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.is_empty())
                })
            })
}

fn responses_item_has_invalid_content(item: &Value) -> bool {
    let expected_part_type = match item.get("type").and_then(Value::as_str) {
        Some("message") => "output_text",
        Some("reasoning") => "reasoning_text",
        _ => return false,
    };
    !item
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            !parts.is_empty()
                && parts.iter().all(|part| {
                    part.get("type").and_then(Value::as_str) == Some(expected_part_type)
                        && part.get("text").and_then(Value::as_str).is_some()
                })
        })
}

#[derive(Clone, Copy)]
enum UsageProtocol {
    Responses,
    Chat,
    Messages,
}

fn nonempty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

fn text_has_image_tool_receipt(text: &str, expected_label: &str, expected_receipt: &str) -> bool {
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|object| {
            object.len() == 2
                && object.get("label").and_then(Value::as_str) == Some(expected_label)
                && object.get("receipt_nonce").and_then(Value::as_str) == Some(expected_receipt)
        })
}

fn usage_is_valid(usage: &Value, protocol: UsageProtocol) -> bool {
    let Some(object) = usage.as_object() else {
        return false;
    };
    let token = |key: &str| object.get(key).and_then(Value::as_u64);
    match protocol {
        UsageProtocol::Responses => {
            let (Some(input), Some(output), Some(total)) = (
                token("input_tokens"),
                token("output_tokens"),
                token("total_tokens"),
            ) else {
                return false;
            };
            total >= input.saturating_add(output)
        }
        UsageProtocol::Chat => {
            let (Some(input), Some(output), Some(total)) = (
                token("prompt_tokens"),
                token("completion_tokens"),
                token("total_tokens"),
            ) else {
                return false;
            };
            total >= input.saturating_add(output)
        }
        UsageProtocol::Messages => {
            let input = object.get("input_tokens");
            let output = object.get("output_tokens");
            (input.is_some() || output.is_some())
                && input.is_none_or(|value| value.as_u64().is_some())
                && output.is_none_or(|value| value.as_u64().is_some())
        }
    }
}

fn validate_json_usage(
    usage: Option<&Value>,
    expected: &SemanticExpectation,
    protocol: UsageProtocol,
    codes: &mut Vec<String>,
    checks: &mut Vec<SemanticCheckResult>,
) {
    if !expected.require_usage_if_present {
        return;
    }
    let Some(usage) = usage else {
        return;
    };
    let passed = usage_is_valid(usage, protocol);
    if !passed {
        codes.push("invalid_usage".into());
    }
    checks.push(SemanticCheckResult {
        id: "usage".into(),
        passed,
        codes: failure_codes(passed, "invalid_usage"),
        observed_value: None,
    });
}

fn finalize_stream_validation(codes: Vec<String>, id: &str) -> SemanticValidation {
    let passed = codes.is_empty();
    SemanticValidation {
        passed,
        error_category: semantic_error_category(passed),
        checks: vec![SemanticCheckResult {
            id: id.to_string(),
            passed,
            codes: codes.clone(),
            observed_value: None,
        }],
        codes,
        first_meaningful_event_ms: None,
    }
}

fn finalize_stream_validation_with_expectations(
    codes: Vec<String>,
    id: &str,
    expected: &SemanticExpectation,
) -> SemanticValidation {
    let mut validation = finalize_stream_validation(codes, id);
    if expected.forced_function.is_some() {
        push_code_check(
            &mut validation,
            "forced_function",
            "missing_forced_function",
        );
        push_code_check(&mut validation, "tool_arguments", "invalid_tool_arguments");
        push_code_check(
            &mut validation,
            "fragmented_arguments",
            "missing_fragmented_arguments",
        );
    }
    if expected.expected_namespace.is_some() {
        push_code_check(&mut validation, "namespace", "missing_expected_namespace");
    }
    if expected.expected_reasoning_marker.is_some() {
        push_code_check(&mut validation, "reasoning", "missing_reasoning_marker");
        if id == "messages_stream" {
            push_code_check(
                &mut validation,
                "thinking_signature",
                "missing_thinking_signature",
            );
        }
    }
    if expected.require_linked_continuation {
        push_code_check(
            &mut validation,
            "linked_continuation",
            "missing_linked_continuation",
        );
    }
    if expected.require_usage_if_present {
        push_code_check(&mut validation, "usage", "invalid_usage");
    }
    let terminal_failure_codes = [
        "missing_response_completed",
        "missing_done",
        "invalid_finish_reason",
        "missing_message_delta",
        "missing_message_stop",
        "invalid_stop_reason",
        "invalid_usage",
    ];
    let terminal_passed = !validation
        .codes
        .iter()
        .any(|code| terminal_failure_codes.contains(&code.as_str()));
    validation.checks.push(SemanticCheckResult {
        id: "usage_and_terminal".into(),
        passed: terminal_passed,
        codes: if terminal_passed {
            Vec::new()
        } else {
            validation
                .codes
                .iter()
                .filter(|code| terminal_failure_codes.contains(&code.as_str()))
                .cloned()
                .collect()
        },
        observed_value: None,
    });
    if id == "messages_stream" {
        push_code_check(&mut validation, "stream_error", "messages_error_event");
    }
    if expected.forced_function.is_some()
        && validation
            .codes
            .iter()
            .any(|code| code == "missing_forced_function")
    {
        validation.error_category = Some("gateway_model_semantic_incompatible".into());
    }
    validation
}

fn failure_codes(passed: bool, failure_code: &str) -> Vec<String> {
    if passed {
        Vec::new()
    } else {
        vec![failure_code.to_string()]
    }
}

fn semantic_error_category(passed: bool) -> Option<String> {
    if passed {
        None
    } else {
        Some("gateway_protocol_semantic_invalid".into())
    }
}

fn push_code_check(validation: &mut SemanticValidation, id: &str, failure_code: &str) {
    let passed = !validation.codes.iter().any(|code| code == failure_code);
    validation.checks.push(SemanticCheckResult {
        id: id.to_string(),
        passed,
        codes: failure_codes(passed, failure_code),
        observed_value: None,
    });
}
