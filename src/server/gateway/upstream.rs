use super::*;
use crate::capabilities::{
    Capability, CapabilityRuntimeSnapshot, DialectProfileKey, DialectProfileState,
    RequestedFeatures, ResolvedCapabilities, RouteIdentity, WireProtocol,
    DIALECT_PROBE_SCHEMA_VERSION,
};
use crate::keys::{anonymous_route_id, upstream_key_fingerprint};
use crate::protocol::image_adapter::ImageDialect;
use crate::upstream_feedback::{classify_upstream_response, UpstreamFeedbackInput};
use std::collections::BTreeSet;

const GATEWAY_CLAUDE_METADATA_KEY: &str = "_gateway_claude";
const GATEWAY_CLAUDE_THINKING_KEY: &str = "_gateway_claude_thinking";

pub(super) fn is_context_limit_error(error_text: &str) -> bool {
    let normalized = error_text.to_ascii_lowercase();
    normalized.contains("request exceeds limit")
        || normalized.contains("exceeded by")
        || normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("token limit")
}

pub(super) fn parse_u16_code(value: &Value) -> Option<u16> {
    if let Some(code) = value.as_u64().and_then(|code| u16::try_from(code).ok()) {
        return Some(code);
    }
    if let Some(code) = value.as_i64().and_then(|code| u16::try_from(code).ok()) {
        return Some(code);
    }
    if let Some(code) = value.as_str() {
        return code.parse::<u16>().ok();
    }
    None
}

#[derive(Debug, Clone, Default)]
pub(super) struct ParsedUpstreamError {
    code: Option<String>,
    message: Option<String>,
}

pub(super) fn collect_upstream_error_fields(
    value: &Value,
    parsed: &mut ParsedUpstreamError,
    depth: u8,
) {
    if depth == 0 {
        return;
    }

    match value {
        Value::Object(object) => {
            if parsed.code.is_none() {
                if let Some(code) = object.get("code").or_else(|| object.get("error_code")) {
                    parsed.code = code.as_str().map(|value| value.to_string());
                    if parsed.code.is_none() {
                        parsed.code = parse_u16_code(code).map(|code| code.to_string());
                    }
                }
            }

            if parsed.message.is_none() {
                if let Some(message) = object
                    .get("message")
                    .or_else(|| object.get("error_message"))
                    .or_else(|| object.get("error_msg"))
                    .or_else(|| object.get("detail"))
                {
                    collect_upstream_error_fields(message, parsed, depth - 1);
                }
            }

            if let Some(error_value) = object.get("error") {
                collect_upstream_error_fields(error_value, parsed, depth - 1);
            }

            if let Some(errors) = object.get("errors").and_then(Value::as_array) {
                for error_item in errors {
                    if parsed.code.is_some() && parsed.message.is_some() {
                        break;
                    }
                    collect_upstream_error_fields(error_item, parsed, depth - 1);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                if parsed.code.is_some() && parsed.message.is_some() {
                    break;
                }
                collect_upstream_error_fields(value, parsed, depth - 1);
            }
        }
        Value::String(message) => {
            let message = message.trim();
            if !(message.starts_with('{') || message.starts_with('[')) {
                if parsed.message.is_none() && !message.is_empty() {
                    parsed.message = Some(message.to_string());
                }
                return;
            }

            if let Ok(value) = serde_json::from_str::<Value>(message) {
                collect_upstream_error_fields(&value, parsed, depth - 1);
                return;
            }

            let message_with_escaped_quotes = message.replace("\\\"", "\"");
            if let Ok(value) = serde_json::from_str::<Value>(&message_with_escaped_quotes) {
                collect_upstream_error_fields(&value, parsed, depth - 1);
                return;
            }

            if parsed.message.is_none() && !message.is_empty() {
                parsed.message = Some(message.to_string());
            }
        }
        _ => {}
    }
}

pub(super) fn parse_upstream_error_payload(error_text: &str) -> ParsedUpstreamError {
    let mut parsed = ParsedUpstreamError::default();
    let trimmed = error_text.trim();
    if trimmed.is_empty() {
        return parsed;
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        collect_upstream_error_fields(&value, &mut parsed, 8);
        return parsed;
    }

    parsed
}

pub(super) fn extract_upstream_error_code(error_text: &str) -> Option<u16> {
    let payload = parse_upstream_error_payload(error_text);
    if let Some(code) = payload.code {
        if let Ok(code) = code.parse::<u16>() {
            return Some(code);
        }
    }

    if let Ok(value) = serde_json::from_str::<Value>(error_text) {
        if let Some(candidate_code) = value
            .get("code")
            .or_else(|| value.get("error").and_then(|error| error.get("code")))
        {
            if let Some(code) = parse_u16_code(candidate_code) {
                return Some(code);
            }
        }
    }

    None
}

pub(super) fn should_try_next_key(error: &GatewayError) -> bool {
    // Route-attributable failures may differ across Keys and should not pin the
    // request to a failed Key.  A real upstream 429 (including a provider
    // concurrency response) is cooled at the exact route and never retried in
    // place.
    error
        .route_failure_class()
        .is_some_and(|class| class != FailureClass::RequestRejected)
}

pub(super) fn should_retry_without_stream(error: &GatewayError) -> bool {
    match error {
        GatewayError::GatewayTimeout(_)
        | GatewayError::Upstream(_)
        | GatewayError::TemporaryUpstreamUnavailable(_) => true,
        GatewayError::Classified { status, meta, .. } => {
            (meta.category.starts_with("upstream_")
                && status.is_server_error()
                && !matches!(
                    meta.category,
                    "upstream_model_unsupported" | "upstream_protocol_unsupported"
                ))
                || (meta.category == "capability_not_supported"
                    && error.message().to_ascii_lowercase().contains("stream"))
        }
        _ => false,
    }
}

fn append_downgrade_header(headers: &mut HeaderMap, codes: &BTreeSet<String>) {
    if codes.is_empty() {
        return;
    }
    let value = codes.iter().cloned().collect::<Vec<_>>().join(",");
    if let Ok(header_value) = HeaderValue::from_str(&value) {
        headers.insert(
            header::HeaderName::from_static("x-chat2responses-downgrade"),
            header_value,
        );
    }
}

fn unsupported_reasoning_replay_error() -> GatewayError {
    GatewayError::classified(
        StatusCode::BAD_REQUEST,
        "selected route cannot preserve required capability ReasoningReplay",
        "invalid_request_error",
        "gateway_protocol_capability_unsupported",
        "gateway_protocol_capability_unsupported",
        None,
        Some(json!({ "scope": "gateway" })),
    )
}

fn upstream_protocol_label(protocol: UpstreamProtocol) -> &'static str {
    match protocol {
        UpstreamProtocol::ChatCompletions => "chat_completions",
        UpstreamProtocol::Responses => "responses",
    }
}

fn apply_claude_thinking_controls_and_replay(
    body: &mut Value,
    resolved: Option<&ResolvedCapabilities>,
    signature_context: &ClaudeThinkingSignatureContext,
    downgrades: &mut BTreeSet<String>,
) -> Result<(), GatewayError> {
    let Some(object) = body.as_object_mut() else {
        return Ok(());
    };

    let metadata = object.remove(GATEWAY_CLAUDE_METADATA_KEY);
    let adaptive = metadata
        .as_ref()
        .and_then(|value| value.pointer("/thinking/type"))
        .and_then(Value::as_str)
        == Some("adaptive");
    let has_reasoning_history = object
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .is_some_and(|thinking| !thinking.is_empty())
                    || message
                        .get(GATEWAY_CLAUDE_THINKING_KEY)
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| !blocks.is_empty())
            })
        });
    if adaptive {
        let Some(resolved) = resolved else {
            if has_reasoning_history {
                return Err(unsupported_reasoning_replay_error());
            }
            downgrades.insert("optional_adaptive_thinking".into());
            return Ok(());
        };
        if !resolved.supports(Capability::ReasoningOutput)
            || !resolved.supports(Capability::ReasoningReplay)
        {
            if has_reasoning_history {
                return Err(unsupported_reasoning_replay_error());
            }
            downgrades.insert("optional_adaptive_thinking".into());
            return Ok(());
        }
        if let Some(requested) = metadata
            .as_ref()
            .and_then(|value| value.pointer("/output_config/effort"))
            .and_then(Value::as_str)
        {
            if resolved.reasoning_control_field.is_none()
                || !resolved.effort_map.contains_key(requested)
            {
                downgrades.insert("optional_reasoning_effort".into());
            }
        }

        if let Some(edits) = metadata
            .as_ref()
            .and_then(|value| value.pointer("/context_management/edits"))
            .and_then(Value::as_array)
        {
            for edit in edits {
                let keep_all = edit.get("keep").and_then(Value::as_str) == Some("all");
                let supported = edit.get("type").and_then(Value::as_str)
                    == Some("clear_thinking_20251015")
                    && keep_all;
                if !supported {
                    downgrades.insert("optional_context_management".into());
                }
            }
        }
    }

    let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for message in messages {
        let Some(message_object) = message.as_object_mut() else {
            continue;
        };
        let Some(thinking_blocks) = message_object
            .remove(GATEWAY_CLAUDE_THINKING_KEY)
            .and_then(|value| value.as_array().cloned())
        else {
            continue;
        };

        let tool_call_ids = message_object
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|tool_calls| {
                tool_calls
                    .iter()
                    .filter_map(|tool_call| {
                        tool_call
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut merged_thinking = String::new();
        for block in thinking_blocks {
            let thinking = block
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if thinking.is_empty() {
                continue;
            }
            let signature = block
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let associated_ids = block
                .get("tool_use_ids")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .filter(|ids| !ids.is_empty())
                .unwrap_or_else(|| tool_call_ids.clone());
            let associated_refs = associated_ids
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>();
            let input = super::thinking_signature::ThinkingSignatureInput {
                thinking,
                model: &signature_context.model,
                upstream_id: &signature_context.upstream_id,
                protocol: &signature_context.protocol,
                profile_fingerprint: &signature_context.profile_fingerprint,
                call_ids: &associated_refs,
            };
            if !super::thinking_signature::verify_thinking(
                signature_context.secret.as_bytes(),
                &input,
                signature,
            ) {
                return Err(GatewayError::BadRequest(
                    "invalid Claude thinking signature".into(),
                ));
            }
            merged_thinking.push_str(thinking);
        }

        if !merged_thinking.is_empty() {
            message_object.insert("reasoning_content".into(), Value::String(merged_thinking));
        }
    }

    Ok(())
}

fn apply_resolved_claude_effort_control(
    body: &mut Value,
    protocol: UpstreamProtocol,
    resolved: Option<&ResolvedCapabilities>,
    requested: Option<&str>,
) -> Result<(), GatewayError> {
    let (Some(resolved), Some(requested)) = (resolved, requested) else {
        return Ok(());
    };
    if !resolved.supports(Capability::ReasoningOutput)
        || !resolved.supports(Capability::ReasoningReplay)
    {
        return Ok(());
    }
    let (Some(field), Some(mapped)) = (
        resolved.reasoning_control_field.as_deref(),
        resolved.effort_map.get(requested),
    ) else {
        return Ok(());
    };
    let Some(object) = body.as_object_mut() else {
        return Ok(());
    };

    let canonical_effort_field = field == "reasoning_effort";
    let reserved_field = matches!(
        field,
        "" | "model"
            | "input"
            | "messages"
            | "instructions"
            | "tools"
            | "tool_choice"
            | "parallel_tool_calls"
            | "stream"
            | "stream_options"
            | "max_tokens"
            | "max_completion_tokens"
            | "max_output_tokens"
            | "reasoning"
            | "metadata"
            | "previous_response_id"
            | "store"
            | "include"
            | "modalities"
            | "response_format"
            | "text"
            | "stop"
            | "stop_sequences"
            | "user"
            | "n"
            | "temperature"
            | "top_p"
            | "frequency_penalty"
            | "presence_penalty"
            | "seed"
            | "logprobs"
            | "top_logprobs"
            | "service_tier"
            | GATEWAY_CLAUDE_METADATA_KEY
            | GATEWAY_CLAUDE_THINKING_KEY
    );
    if !canonical_effort_field && (reserved_field || object.contains_key(field)) {
        return Err(GatewayError::classified(
            StatusCode::BAD_REQUEST,
            format!("reasoning control field \"{field}\" collides with request data"),
            "invalid_request_error",
            "gateway_reasoning_control_field_collision",
            "gateway_reasoning_control_field_collision",
            None,
            Some(json!({ "scope": "gateway", "field": field })),
        ));
    }

    object.remove("reasoning_effort");
    if protocol == UpstreamProtocol::Responses {
        let remove_reasoning = object
            .get_mut("reasoning")
            .and_then(Value::as_object_mut)
            .is_some_and(|reasoning| {
                reasoning.remove("effort");
                reasoning.is_empty()
            });
        if remove_reasoning {
            object.remove("reasoning");
        }
    }
    object.insert(field.to_string(), Value::String(mapped.clone()));
    Ok(())
}

fn applied_claude_effort_control_evidence(
    body: &Value,
    resolved: Option<&ResolvedCapabilities>,
    requested: Option<&str>,
) -> Option<AppliedEffortControl> {
    let resolved = resolved?;
    let requested = requested?;
    let field = resolved.reasoning_control_field.as_deref()?;
    let mapped = resolved.effort_map.get(requested)?;
    (body.get(field).and_then(Value::as_str) == Some(mapped.as_str())).then(|| {
        AppliedEffortControl {
            requested: requested.to_string(),
            field: field.to_string(),
            value: mapped.clone(),
        }
    })
}

fn compatibility_profile_state(state: DialectProfileState) -> &'static str {
    match state {
        DialectProfileState::Verified => "verified",
        DialectProfileState::Partial => "partial",
        DialectProfileState::Unsupported => "unsupported",
        DialectProfileState::Unknown => "unknown",
    }
}

#[allow(clippy::too_many_arguments)]
fn build_compatibility_usage_metadata(
    snapshot: &CapabilityRuntimeSnapshot,
    upstream: &UpstreamConfig,
    key_fingerprint: &str,
    exposed_model_slug: &str,
    runtime_model_slug: &str,
    upstream_protocol: UpstreamProtocol,
    endpoint: EndpointKind,
    resolved: &ResolvedCapabilities,
    downgrade_codes: &BTreeSet<String>,
    dialect_retry_count: u8,
    claude_request: bool,
    attempt_mode: UpstreamAttemptMode,
) -> CompatibilityUsageMetadata {
    let mut route = RouteIdentity {
        upstream_id: upstream.id.clone(),
        key_fingerprint: key_fingerprint.to_string(),
        exposed_model_slug: exposed_model_slug.to_string(),
        runtime_model_slug: runtime_model_slug.to_string(),
        protocol: WireProtocol::from(upstream_protocol),
        tags: BTreeSet::new(),
    };
    snapshot.configuration.apply_route_tags(&mut route);
    let profile_key = DialectProfileKey::from_route(&route);
    let probe_version = snapshot
        .profiles
        .get(&profile_key)
        .map(|profile| profile.probe_schema_version)
        .unwrap_or(DIALECT_PROBE_SCHEMA_VERSION);
    let mut adapter_types = Vec::new();
    if endpoint.native_protocol() != upstream_protocol {
        adapter_types.push(protocol_transition_label(endpoint, upstream_protocol).to_string());
    }
    if claude_request {
        adapter_types.push("messages_to_chat".into());
        adapter_types.push("claude_thinking".into());
    }
    if attempt_mode.aggregates_sse() {
        adapter_types.push("stream_to_json".into());
    }

    CompatibilityUsageMetadata {
        protocol_transition: protocol_transition_label(endpoint, upstream_protocol).to_string(),
        adapter_types,
        optional_downgrades: downgrade_codes.iter().cloned().collect(),
        policy_id: snapshot
            .configuration
            .policy_ids_for(&route)
            .last()
            .map(|value| (*value).to_string()),
        policy_schema_version: snapshot.configuration.source().schema_version,
        policy_digest: snapshot.configuration.digest().to_string(),
        profile_state: compatibility_profile_state(resolved.profile_state).to_string(),
        probe_version,
        dialect_retry_count,
        fallback_stage: None,
    }
}

struct HedgeStreamReady {
    reader: UpstreamStreamReader,
    reservation: Option<UpstreamRequestGuard>,
    route_id: String,
}

struct RouteHedgeReady {
    result: Box<DispatchResult>,
    route_id: String,
}

enum HedgeWinnerReady {
    Stream(Box<HedgeStreamReady>),
    Route(RouteHedgeReady),
}

enum PrefetchedStreamWinner {
    Reader(Box<UpstreamStreamReader>),
    Dispatch(Box<DispatchResult>),
}

struct HedgeStreamAttempt {
    state: AppState,
    upstream: UpstreamConfig,
    api_key: String,
    url: String,
    upstream_body: Value,
    request_model: String,
    upstream_protocol: UpstreamProtocol,
    response_header_timeout: Duration,
    stream_timeouts: StreamTimeouts,
}

#[derive(Clone)]
struct RouteHedgeContext {
    state: AppState,
    capability_snapshot: CapabilityRuntimeSnapshot,
    requested_features: RequestedFeatures,
    body: Value,
    endpoint: EndpointKind,
    started: Instant,
    request_id: String,
    model: String,
    normalized_model: String,
    downstream_key_id: String,
    downstream_name: String,
    inference_strength: Option<String>,
    user_agent: Option<String>,
    downstream_concurrency_guard: DownstreamConcurrencyGuard,
    route_attempts: RequestRouteAttempts,
    response_history_context: Option<ResponseHistoryContext>,
    stream_only_recovery_request_safe: bool,
}

fn hedge_launch_delay(config: &AppConfig, launched_extra_attempts: usize) -> Duration {
    let delay_ms = if launched_extra_attempts == 0 {
        config.upstream_hedge_delay_ms
    } else {
        config.upstream_hedge_interval_ms
    };
    Duration::from_millis(delay_ms.max(1))
}

fn hedge_extra_attempt_limit(config: &AppConfig, available_accounts: usize) -> usize {
    if !config.upstream_hedge_enabled {
        return 0;
    }
    usize::try_from(config.upstream_hedge_max_extra_attempts)
        .unwrap_or(usize::MAX)
        .min(available_accounts)
}

fn send_route_hedge_attempt(
    context: RouteHedgeContext,
    candidate: RouteHedgeCandidate,
    control: HedgeAttemptControl,
) -> futures_util::future::BoxFuture<'static, Result<RouteHedgeReady, GatewayError>> {
    async move {
        let route_health_key = candidate.route_health_key.clone();
        let (_, key_health_key) = super::route_health_keys(
            &candidate.upstream,
            &candidate.key_fingerprint,
            &route_health_key.runtime_model_slug,
            candidate.protocol,
        );
        let route_health_permit = match context
            .state
            .reserve_route_health(&route_health_key, &key_health_key)
            .await
        {
            crate::state::RouteAvailability::Ready(permit) => {
                std::sync::Arc::new(tokio::sync::Mutex::new(Some(permit)))
            }
            crate::state::RouteAvailability::Cooling { retry_after, .. }
            | crate::state::RouteAvailability::HalfOpenBusy { retry_after, .. } => {
                return Err(GatewayError::upstream_temporary_unavailable(
                    format!("hedged route cooling for {}s", retry_after.as_secs()),
                    "upstream_hedge_route_cooling",
                ));
            }
        };
        if let Err(error) = context
            .state
            .try_reserve_upstream_hedge(&candidate.upstream, &context.model)
            .await
        {
            super::finish_route_health_permit(&route_health_permit, RouteOutcome::Cancelled).await;
            return Err(GatewayError::upstream_temporary_unavailable(
                error.message,
                "upstream_hedge_capacity_unavailable",
            ));
        }
        let upstream_request_guard = UpstreamRequestReservation::new(UpstreamRequestGuard::new(
            context.state.clone(),
            candidate.upstream.id.clone(),
        ));
        let completion = StreamCompletionContext {
            state: context.state.clone(),
            route_health_key: route_health_key.clone(),
            route_attempts: context.route_attempts.clone(),
            route_health_permit,
            upstream_request_guard: upstream_request_guard.clone(),
            downstream_concurrency_guard: context.downstream_concurrency_guard.clone(),
            hedge_control: Some(control.clone()),
        };
        let resolved_route = candidate.resolved_capabilities.clone();
        let attempt_mode = select_upstream_attempt_mode(true, resolved_route.as_ref());
        let global_context_profile = context
            .state
            .global_context_profile_for_upstream_base_url(&candidate.upstream.base_url)
            .await;
        let mut stream_only_recovery = StreamOnlyRecoveryState::default();
        let mut stream_only_recovery_leader = None;
        let mut stream_only_recovery_identity = None;
        let result = send_to_upstream(
            &context.state,
            &candidate.upstream,
            &candidate.api_key,
            &[],
            &[],
            resolved_route.as_ref(),
            &context.capability_snapshot,
            &context.requested_features,
            candidate.protocol,
            &context.body,
            context.endpoint,
            true,
            attempt_mode,
            context.started,
            &context.request_id,
            &context.model,
            &context.normalized_model,
            &context.downstream_key_id,
            &context.downstream_name,
            context.inference_strength.as_deref(),
            context.user_agent.as_deref(),
            false,
            global_context_profile.as_ref(),
            Some(completion.clone()),
            upstream_request_guard.clone(),
            context.route_attempts.clone(),
            route_health_key.clone(),
            context.response_history_context,
            None,
            Some(control.clone()),
            context.stream_only_recovery_request_safe,
            &mut stream_only_recovery,
            &mut stream_only_recovery_leader,
            &mut stream_only_recovery_identity,
        )
        .await;
        let result = match result {
            Ok(result) => {
                if matches!(result.usage_log_timing, UsageLogTiming::Immediate) {
                    completion.mark_success().await;
                }
                result
            }
            Err(error) => {
                super::finish_route_health_permit(
                    &completion.route_health_permit,
                    super::route_health_outcome(&error),
                )
                .await;
                if !context.route_attempts.should_attempt(&route_health_key) {
                    super::record_route_attempt(
                        &context.state,
                        &context.route_attempts,
                        &route_health_key,
                        &context.capability_snapshot,
                        &context.requested_features,
                        context.inference_strength.as_deref(),
                        &context.model,
                        &candidate.upstream,
                        &candidate.key_fingerprint,
                        &route_health_key.runtime_model_slug,
                        candidate.protocol,
                        &error,
                    )
                    .await;
                }
                return Err(error);
            }
        };
        drop(upstream_request_guard);
        Ok(RouteHedgeReady {
            result: Box::new(result),
            route_id: anonymous_route_id(
                &route_health_key.upstream_id,
                &route_health_key.key_fingerprint,
                &route_health_key.runtime_model_slug,
                route_health_key.protocol,
            ),
        })
    }
    .boxed()
}

async fn send_hedge_stream_attempt(
    attempt: HedgeStreamAttempt,
) -> Result<HedgeStreamReady, GatewayError> {
    let HedgeStreamAttempt {
        state,
        upstream,
        api_key,
        url,
        upstream_body,
        request_model,
        upstream_protocol,
        response_header_timeout,
        stream_timeouts,
    } = attempt;
    state
        .try_reserve_upstream_hedge(&upstream, &request_model)
        .await
        .map_err(|error| {
            GatewayError::upstream_temporary_unavailable(
                error.message,
                "upstream_hedge_capacity_unavailable",
            )
        })?;
    let reservation = UpstreamRequestGuard::new(state.clone(), upstream.id.clone());
    let response = tokio::time::timeout(
        response_header_timeout,
        state
            .client_for_url(&url)
            .post(url)
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .json(&upstream_body)
            .send(),
    )
    .await
    .map_err(|_| GatewayError::upstream_timeout("hedged upstream response header timeout"))?
    .map_err(|error| {
        GatewayError::upstream_network_error(format!("hedged upstream request failed: {error}"))
    })?;

    if !response.status().is_success() {
        return Err(GatewayError::upstream_temporary_unavailable(
            format!("hedged upstream returned HTTP {}", response.status()),
            "upstream_hedge_rejected",
        ));
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.contains("text/event-stream") {
        return Err(GatewayError::upstream_invalid_response(
            "hedged streaming request did not return SSE",
            "upstream_hedge_non_sse_response",
        ));
    }

    let reader = prefetch_first_usable_output(
        UpstreamStreamReader::new(response, stream_timeouts),
        upstream_protocol,
    )
    .await?;
    Ok(HedgeStreamReady {
        reader,
        reservation: Some(reservation),
        route_id: anonymous_route_id(
            &upstream.id,
            &upstream_key_fingerprint(&upstream.id, &api_key),
            &request_model,
            WireProtocol::from(upstream_protocol),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
async fn prefetch_stream_with_hedges(
    state: &AppState,
    upstream: &UpstreamConfig,
    primary_reader: UpstreamStreamReader,
    primary_api_key: &str,
    hedge_api_keys: &[String],
    route_hedge_candidates: &[RouteHedgeCandidate],
    route_hedge_context: Option<RouteHedgeContext>,
    url: &str,
    upstream_body: &Value,
    request_model: &str,
    upstream_protocol: UpstreamProtocol,
    response_header_timeout: Duration,
    stream_timeouts: StreamTimeouts,
    request_id: &str,
    started: Instant,
) -> Result<PrefetchedStreamWinner, GatewayError> {
    let route_hedge_count = route_hedge_context
        .as_ref()
        .map(|_| route_hedge_candidates.len())
        .unwrap_or(0);
    let extra_candidate_count = route_hedge_count.saturating_add(hedge_api_keys.len());
    let max_extra_attempts = hedge_extra_attempt_limit(&state.config, extra_candidate_count);
    if max_extra_attempts == 0 {
        return prefetch_first_usable_output(primary_reader, upstream_protocol)
            .await
            .map(|reader| PrefetchedStreamWinner::Reader(Box::new(reader)));
    }

    type HedgeFuture =
        futures_util::future::BoxFuture<'static, (u32, Result<HedgeWinnerReady, GatewayError>)>;
    let mut attempts = futures_stream::FuturesUnordered::<HedgeFuture>::new();
    let primary_route_id = anonymous_route_id(
        &upstream.id,
        &upstream_key_fingerprint(&upstream.id, primary_api_key),
        request_model,
        WireProtocol::from(upstream_protocol),
    );
    attempts.push(
        async move {
            (
                0,
                prefetch_first_usable_output(primary_reader, upstream_protocol)
                    .await
                    .map(|reader| {
                        HedgeWinnerReady::Stream(Box::new(HedgeStreamReady {
                            reader,
                            reservation: None,
                            route_id: primary_route_id,
                        }))
                    }),
            )
        }
        .boxed(),
    );

    let mut launched_extra_attempts = 0usize;
    let mut next_candidate_index = 0usize;
    let mut next_attempt_index = 1u32;
    let mut next_launch_at = TokioInstant::now() + hedge_launch_delay(&state.config, 0);
    let mut primary_error = None;
    let mut last_error = None;
    let mut route_controls = Vec::<(u32, HedgeAttemptControl)>::new();

    loop {
        if attempts.is_empty()
            && (launched_extra_attempts >= max_extra_attempts
                || next_candidate_index >= extra_candidate_count)
        {
            return Err(primary_error.or(last_error).unwrap_or_else(|| {
                GatewayError::upstream_temporary_unavailable(
                    "all hedged upstream attempts ended before usable output",
                    "upstream_hedge_exhausted",
                )
            }));
        }

        tokio::select! {
            outcome = attempts.next(), if !attempts.is_empty() => {
                let Some((attempt_index, outcome)) = outcome else {
                    continue;
                };
                match outcome {
                    Ok(ready) => {
                        for (control_index, control) in &route_controls {
                            if *control_index != attempt_index {
                                control.cancel_as_loser();
                            }
                        }
                        let winner = match ready {
                            HedgeWinnerReady::Stream(mut ready) => {
                                if let Some(reservation) = ready.reservation.take() {
                                    reservation.release().await;
                                }
                                tracing::info!(
                                    request_id,
                                    hedge_enabled = state.config.upstream_hedge_enabled,
                                    hedge_winner_upstream_id = %upstream.id,
                                    selected_upstream_id = %upstream.id,
                                    hedge_winner_attempt = attempt_index,
                                    route_id = %ready.route_id,
                                    hedge_extra_attempts_launched = launched_extra_attempts,
                                    hedge_losers_cancelled = attempts.len(),
                                    first_usable_output_latency_ms = started.elapsed().as_millis() as u64,
                                    "selected first usable upstream stream"
                                );
                                PrefetchedStreamWinner::Reader(Box::new(ready.reader))
                            }
                            HedgeWinnerReady::Route(ready) => {
                                tracing::info!(
                                    request_id,
                                    hedge_enabled = state.config.upstream_hedge_enabled,
                                    hedge_winner_upstream_id = %ready.result.selected_upstream_id,
                                    selected_upstream_id = %ready.result.selected_upstream_id,
                                    route_id = %ready.route_id,
                                    hedge_winner_attempt = attempt_index,
                                    hedge_extra_attempts_launched = launched_extra_attempts,
                                    hedge_losers_cancelled = attempts.len(),
                                    first_usable_output_latency_ms = started.elapsed().as_millis() as u64,
                                    "selected first usable upstream route"
                                );
                                PrefetchedStreamWinner::Dispatch(ready.result)
                            }
                        };
                        drop(attempts);
                        return Ok(winner);
                    }
                    Err(error) => {
                        if attempt_index == 0
                            && (error.is_stream_only_recovery_candidate()
                                || should_retry_without_stream(&error))
                        {
                            for (_, control) in &route_controls {
                                control.cancel_as_loser();
                            }
                            drop(attempts);
                            return Err(error);
                        }
                        route_controls.retain(|(control_index, _)| *control_index != attempt_index);
                        let admission_skipped = attempt_index != 0
                            && error.error_category() == "upstream_hedge_capacity_unavailable";
                        if admission_skipped {
                            launched_extra_attempts = launched_extra_attempts.saturating_sub(1);
                        }
                        if attempt_index == 0 {
                            primary_error = Some(error);
                        } else {
                            last_error = Some(error);
                        }
                        if launched_extra_attempts < max_extra_attempts
                            && next_candidate_index < extra_candidate_count
                        {
                            next_launch_at = TokioInstant::now();
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(next_launch_at), if launched_extra_attempts < max_extra_attempts && next_candidate_index < extra_candidate_count => {
                let candidate_index = next_candidate_index;
                next_candidate_index += 1;
                launched_extra_attempts += 1;
                let attempt_index = next_attempt_index;
                next_attempt_index = next_attempt_index.saturating_add(1);
                if candidate_index < route_hedge_count {
                    let candidate = route_hedge_candidates[candidate_index].clone();
                    let context = route_hedge_context
                        .as_ref()
                        .expect("route hedge count requires context")
                        .clone();
                    let control = HedgeAttemptControl::default();
                    route_controls.push((attempt_index, control.clone()));
                    tracing::info!(
                        request_id,
                        selected_upstream_id = %candidate.upstream.id,
                        hedge_attempt = attempt_index,
                        route_id = %anonymous_route_id(
                            &candidate.route_health_key.upstream_id,
                            &candidate.route_health_key.key_fingerprint,
                            &candidate.route_health_key.runtime_model_slug,
                            candidate.route_health_key.protocol,
                        ),
                        "launching slow first-output route hedge"
                    );
                    let future = send_route_hedge_attempt(context, candidate, control);
                    attempts.push(
                        async move {
                            (
                                attempt_index,
                                future.await.map(HedgeWinnerReady::Route),
                            )
                        }
                        .boxed(),
                    );
                } else {
                    let api_key = hedge_api_keys[candidate_index - route_hedge_count].clone();
                    tracing::info!(
                        request_id,
                        selected_upstream_id = %upstream.id,
                        hedge_attempt = attempt_index,
                        route_id = %anonymous_route_id(
                            &upstream.id,
                            &upstream_key_fingerprint(&upstream.id, &api_key),
                            request_model,
                            WireProtocol::from(upstream_protocol),
                        ),
                        "launching slow first-output key hedge"
                    );
                    let future = send_hedge_stream_attempt(HedgeStreamAttempt {
                        state: state.clone(),
                        upstream: upstream.clone(),
                        api_key,
                        url: url.to_string(),
                        upstream_body: upstream_body.clone(),
                        request_model: request_model.to_string(),
                        upstream_protocol,
                        response_header_timeout,
                        stream_timeouts,
                    });
                    attempts.push(
                        async move {
                            (
                                attempt_index,
                                future
                                    .await
                                    .map(|ready| HedgeWinnerReady::Stream(Box::new(ready))),
                            )
                        }
                        .boxed(),
                    );
                }
                next_launch_at = TokioInstant::now()
                    + hedge_launch_delay(&state.config, launched_extra_attempts);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_to_upstream(
    state: &AppState,
    upstream: &UpstreamConfig,
    api_key: &str,
    hedge_api_keys: &[String],
    route_hedge_candidates: &[RouteHedgeCandidate],
    resolved_capabilities: Option<&ResolvedCapabilities>,
    capability_snapshot: &CapabilityRuntimeSnapshot,
    requested_features: &RequestedFeatures,
    upstream_protocol: UpstreamProtocol,
    body: &Value,
    endpoint: EndpointKind,
    request_stream: bool,
    attempt_mode: UpstreamAttemptMode,
    started: Instant,
    request_id: &str,
    model: &str,
    normalized_model: &str,
    downstream_key_id: &str,
    downstream_name: &str,
    inference_strength: Option<&str>,
    user_agent: Option<&str>,
    chat_fallback_requested: bool,
    global_context_profile: Option<&GlobalContextProfile>,
    stream_completion_context: Option<StreamCompletionContext>,
    upstream_request_guard: UpstreamRequestReservation,
    route_attempts: RequestRouteAttempts,
    route_health_key: RouteHealthKey,
    mut response_history_context: Option<ResponseHistoryContext>,
    active_request_guard: Option<&mut ActiveGatewayRequestGuard>,
    hedge_control: Option<HedgeAttemptControl>,
    stream_only_recovery_request_safe: bool,
    stream_only_recovery: &mut StreamOnlyRecoveryState,
    stream_only_recovery_leader: &mut Option<StreamOnlyRecoveryLeader>,
    stream_only_recovery_identity: &mut Option<(DialectProfileKey, String)>,
) -> Result<DispatchResult, GatewayError> {
    let key_fingerprint = upstream_key_fingerprint(&upstream.id, api_key);
    let runtime_capability_hints = state.runtime_capability_hints_snapshot();
    let mut active_capability_snapshot = capability_snapshot.clone();
    let mut resolved_capabilities = resolved_capabilities.cloned();
    let mut attempt_mode = attempt_mode;
    let mut downgrade_codes = BTreeSet::new();
    let mut dialect_retry_count = 0u8;
    let request_model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::BadRequest("missing model".into()))?;
    let mut final_upstream_model =
        upstream.resolved_model_name(request_model).ok_or_else(|| {
            GatewayError::BadRequest(format!(
                "model \"{request_model}\" is not configured for upstream \"{}\"",
                upstream.name
            ))
        })?;
    let selected_upstream_model = final_upstream_model.clone();
    let model_rewritten = final_upstream_model != request_model;
    let claude_request = body.get(GATEWAY_CLAUDE_METADATA_KEY).is_some()
        || body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message
                        .get(GATEWAY_CLAUDE_THINKING_KEY)
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| !blocks.is_empty())
                })
            });
    let claude_requested_effort = body
        .get(GATEWAY_CLAUDE_METADATA_KEY)
        .filter(|metadata| {
            metadata.pointer("/thinking/type").and_then(Value::as_str) == Some("adaptive")
        })
        .and_then(|metadata| metadata.pointer("/output_config/effort"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let preconversion_signature_context = if claude_request
        && endpoint == EndpointKind::ChatCompletions
        && upstream_protocol == UpstreamProtocol::Responses
    {
        Some(ClaudeThinkingSignatureContext {
            secret: state.config.jwt_secret.clone(),
            model: final_upstream_model.clone(),
            upstream_id: upstream.id.clone(),
            protocol: upstream_protocol_label(upstream_protocol).to_string(),
            profile_fingerprint: AppState::route_configuration_fingerprint_with_snapshot(
                &active_capability_snapshot,
                upstream,
                &key_fingerprint,
                request_model,
                &final_upstream_model,
                upstream_protocol,
            )
            .map_err(|error| {
                GatewayError::upstream_invalid_response(
                    format!("failed to compute route configuration fingerprint: {error}"),
                    "upstream_invalid_response",
                )
            })?,
        })
    } else {
        None
    };
    let mut canonical_body = body.clone();
    if let Some(signature_context) = preconversion_signature_context.as_ref() {
        apply_claude_thinking_controls_and_replay(
            &mut canonical_body,
            resolved_capabilities.as_ref(),
            signature_context,
            &mut downgrade_codes,
        )?;
    }
    let upstream_body = match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => canonical_body,
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
            let conversion_context = resolved_capabilities
                .as_ref()
                .map(|resolved| ConversionContext::new(resolved, ToolAdapterRegistry::empty()))
                .unwrap_or_default();
            chat_request_to_responses_payload_with_context(&canonical_body, &conversion_context)
                .map_err(protocol_error_to_gateway)?
        }
        (EndpointKind::Responses, UpstreamProtocol::Responses) => {
            let (adapted, downgrades) = apply_responses_hosted_tool_policy(
                body,
                resolved_capabilities
                    .as_ref()
                    .is_some_and(|resolved| resolved.supports(Capability::HostedTools)),
            )
            .map_err(protocol_error_to_gateway)?;
            downgrade_codes.extend(downgrades);
            adapted
        }
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
            let fallback_report = responses_request_chat_fallback_report(body);
            let mut fallback_reasons = Vec::new();
            if chat_fallback_requested {
                fallback_reasons.push("no_responses_upstream_supports_model");
            }
            if fallback_report.stripped_tool_count > 0 {
                fallback_reasons.push("unsupported_tools");
            }
            if fallback_report.tool_choice_dropped {
                fallback_reasons.push("tool_choice_dropped");
            }
            if !fallback_reasons.is_empty() {
                tracing::warn!(
                    request_id = %request_id,
                    downstream_key_id = %downstream_key_id,
                    path = %endpoint.path(),
                    original_model = %model,
                    normalized_model = %normalized_model,
                    retained_tool_count = fallback_report.retained_tool_count,
                    stripped_tool_count = fallback_report.stripped_tool_count,
                    has_tool_choice = fallback_report.has_tool_choice,
                    tool_choice_dropped = fallback_report.tool_choice_dropped,
                    fallback_reasons = ?fallback_reasons,
                    "responses request downgraded to ChatCompletions"
                );
            }
            responses_request_to_chat_payload_with_fallback(
                body,
                resolved_capabilities.as_ref(),
                &mut downgrade_codes,
            )
            .map_err(protocol_error_to_gateway)?
        }
    };
    let mut upstream_body = upstream_body;
    let protocol_path = protocol_transition_label(endpoint, upstream_protocol);
    if let Some(object) = upstream_body.as_object_mut() {
        object.insert("model".into(), Value::String(final_upstream_model.clone()));
    }
    strip_response_usage_fields_from_upstream_request(&mut upstream_body);
    tracing::info!(
        request_id = %request_id,
        downstream_key_id = %downstream_key_id,
        path = %endpoint.path(),
        original_model = %model,
        normalized_model = %normalized_model,
        selected_upstream_id = %upstream.id,
        selected_upstream_name = %upstream.name,
        selected_upstream_protocol = ?upstream_protocol,
        upstream_model = %request_model,
        final_upstream_model = %final_upstream_model,
        model_rewritten = model_rewritten,
        protocol_transition = %protocol_path,
        request_stream,
        upstream_attempt_mode = attempt_mode.as_str(),
        "prepared upstream request body"
    );
    if model_rewritten {
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream_protocol,
            upstream_model = %request_model,
            final_upstream_model = %final_upstream_model,
            "upstream model alias rewrote request model"
        );
    }
    let context_budget_report = apply_context_budget_controls(
        upstream,
        global_context_profile,
        &mut upstream_body,
        &final_upstream_model,
    );
    if let Some(report) = context_budget_report.as_ref() {
        if let Some(switched_model) = report.fallback_model.as_ref() {
            if requested_features.continuation_profile.is_some() {
                return Err(response_history_invalid(
                    "exact gateway continuation cannot change runtime model during context fallback",
                ));
            }
            final_upstream_model = switched_model.clone();
        }
        tracing::info!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream_protocol,
            final_upstream_model = %final_upstream_model,
            context_limit = report.context_limit,
            output_reserve = report.output_reserve,
            estimated_input_tokens = report.estimated_input_tokens,
            estimated_input_tokens_after_trim = report.estimated_input_tokens_after_trim,
            requested_output_tokens = report.requested_output_tokens,
            allowed_input_tokens = report.allowed_input_tokens,
            trimmed_blocks = report.trim_stats.truncated_blocks,
            compacted_entries = report.trim_stats.compacted_entries,
            tool_result_blocks = report.trim_stats.tool_result_blocks,
            max_output_tokens_cap = report.max_output_tokens_cap,
            max_output_tokens_clamped = report.max_output_tokens_clamped,
            fallback_model = ?report.fallback_model,
            "applied upstream context budgeting"
        );
        if report.max_output_tokens_clamped {
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint.path(),
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream.id,
                selected_upstream_name = %upstream.name,
                selected_upstream_protocol = ?upstream_protocol,
                final_upstream_model = %final_upstream_model,
                requested_output_tokens = report.requested_output_tokens,
                max_output_tokens_cap = report.max_output_tokens_cap,
                "clamped max_tokens to upstream max_output_tokens cap"
            );
        }
    }

    if final_upstream_model != selected_upstream_model {
        let fallback_profile_key = DialectProfileKey::for_key(
            upstream.id.clone(),
            key_fingerprint.clone(),
            final_upstream_model.clone(),
            WireProtocol::from(upstream_protocol),
        );
        if !active_capability_snapshot
            .profiles
            .contains_key(&fallback_profile_key)
        {
            return Err(GatewayError::classified(
                StatusCode::BAD_REQUEST,
                format!(
                    "context fallback model \"{final_upstream_model}\" has no exact capability profile"
                ),
                "invalid_request_error",
                "gateway_protocol_capability_unsupported",
                "gateway_protocol_capability_unsupported",
                None,
                Some(json!({
                    "scope": "gateway",
                    "upstream_id": upstream.id,
                    "runtime_model": final_upstream_model,
                    "protocol": upstream_protocol_label(upstream_protocol),
                })),
            ));
        }
        resolved_capabilities = resolve_route_capabilities_with_runtime_hints(
            &active_capability_snapshot,
            upstream,
            &key_fingerprint,
            request_model,
            &final_upstream_model,
            upstream_protocol,
            requested_features,
            &runtime_capability_hints,
            inference_strength,
        );
        if resolved_capabilities.is_none() {
            return Err(GatewayError::classified(
                StatusCode::BAD_REQUEST,
                format!(
                    "context fallback model \"{final_upstream_model}\" cannot preserve requested capabilities"
                ),
                "invalid_request_error",
                "gateway_protocol_capability_unsupported",
                "gateway_protocol_capability_unsupported",
                None,
                Some(json!({
                    "scope": "gateway",
                    "upstream_id": upstream.id,
                    "runtime_model": final_upstream_model,
                    "protocol": upstream_protocol_label(upstream_protocol),
                })),
            ));
        }
    }

    // A context fallback has its own exact capability profile. Non-stream
    // downstream requests must use that final route's evidence, while a
    // downstream SSE retry explicitly forced to JSON stays forced to JSON.
    if !request_stream
        && final_upstream_model != selected_upstream_model
        && !stream_only_recovery.consumed
    {
        attempt_mode = select_upstream_attempt_mode(false, resolved_capabilities.as_ref());
    }
    if stream_only_recovery_request_safe
        && !stream_only_recovery.consumed
        && stream_only_recovery_leader.is_none()
        && stream_only_recovery_identity.is_none()
        && route_has_raw_stream_delivery_evidence(resolved_capabilities.as_ref())
    {
        let configuration_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
            &active_capability_snapshot,
            upstream,
            &key_fingerprint,
            request_model,
            &final_upstream_model,
            upstream_protocol,
        )
        .map_err(|error| {
            GatewayError::upstream_invalid_response(
                format!("failed to compute route configuration fingerprint: {error}"),
                "upstream_invalid_response",
            )
        })?;
        let profile_key = DialectProfileKey::for_key(
            upstream.id.clone(),
            key_fingerprint.clone(),
            final_upstream_model.clone(),
            WireProtocol::from(upstream_protocol),
        );
        match begin_stream_only_recovery(
            state,
            profile_key.clone(),
            configuration_fingerprint.clone(),
        ) {
            StreamOnlyRecoveryRole::Leader(leader) => {
                *stream_only_recovery_leader = Some(leader);
                *stream_only_recovery_identity = Some((profile_key, configuration_fingerprint));
            }
            StreamOnlyRecoveryRole::Follower(follower) => {
                follower.wait().await;
                stream_only_recovery.consumed = true;
                stream_only_recovery.final_attempt = true;
                let fresh = state.capability_snapshot();
                active_capability_snapshot = (*fresh).clone();
                resolved_capabilities = resolve_route_capabilities_with_runtime_hints(
                    &active_capability_snapshot,
                    upstream,
                    &key_fingerprint,
                    request_model,
                    &final_upstream_model,
                    upstream_protocol,
                    requested_features,
                    &runtime_capability_hints,
                    inference_strength,
                );
                attempt_mode = select_upstream_attempt_mode(false, resolved_capabilities.as_ref());
            }
            StreamOnlyRecoveryRole::AtCapacity => {
                stream_only_recovery.consumed = true;
            }
        }
    }
    if let Some(context) = response_history_context.take() {
        let configuration_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
            &active_capability_snapshot,
            upstream,
            &key_fingerprint,
            request_model,
            &final_upstream_model,
            upstream_protocol,
        )
        .map_err(|error| {
            GatewayError::upstream_invalid_response(
                format!("failed to compute continuation route fingerprint: {error}"),
                "gateway_response_history_invalid",
            )
        })?;
        let profile_key = DialectProfileKey::for_key(
            upstream.id.clone(),
            key_fingerprint.clone(),
            final_upstream_model.clone(),
            WireProtocol::from(upstream_protocol),
        );
        let profile_reasoning_carrier = active_capability_snapshot
            .profiles
            .get(&profile_key)
            .filter(|profile| {
                profile.configuration_fingerprint == configuration_fingerprint
                    && profile.probe_schema_version
                        == crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION
            })
            .and_then(|profile| profile.reasoning_carrier);
        let continuation = GatewayContinuationState::new(
            profile_key,
            configuration_fingerprint,
            profile_reasoning_carrier,
            requested_features.required.clone(),
            WireProtocol::from(endpoint.native_protocol()),
            WireProtocol::from(upstream_protocol),
            context.tool_registry_version(),
        );
        response_history_context = Some(context.with_selected_route(continuation, None)?);
    }
    if let Some(object) = upstream_body.as_object_mut() {
        object.insert(
            "stream".into(),
            Value::Bool(attempt_mode.uses_upstream_sse()),
        );
        if attempt_mode.uses_upstream_sse()
            && upstream_protocol == UpstreamProtocol::ChatCompletions
        {
            if attempt_mode.requests_usage_stream(resolved_capabilities.as_ref()) {
                let stream_options = object
                    .entry("stream_options".to_string())
                    .or_insert_with(|| json!({}));
                if !stream_options.is_object() {
                    *stream_options = json!({});
                }
                if let Some(stream_options) = stream_options.as_object_mut() {
                    stream_options.insert("include_usage".into(), Value::Bool(true));
                }
            } else {
                object.remove("stream_options");
            }
        }
    }

    let claude_thinking_signature = if claude_request {
        Some(ClaudeThinkingSignatureContext {
            secret: state.config.jwt_secret.clone(),
            model: final_upstream_model.clone(),
            upstream_id: upstream.id.clone(),
            protocol: upstream_protocol_label(upstream_protocol).to_string(),
            profile_fingerprint: AppState::route_configuration_fingerprint_with_snapshot(
                &active_capability_snapshot,
                upstream,
                &key_fingerprint,
                request_model,
                &final_upstream_model,
                upstream_protocol,
            )
            .map_err(|error| {
                GatewayError::upstream_invalid_response(
                    format!("failed to compute route configuration fingerprint: {error}"),
                    "upstream_invalid_response",
                )
            })?,
        })
    } else {
        None
    };

    if preconversion_signature_context.is_none() {
        if let Some(signature_context) = claude_thinking_signature.as_ref() {
            apply_claude_thinking_controls_and_replay(
                &mut upstream_body,
                resolved_capabilities.as_ref(),
                signature_context,
                &mut downgrade_codes,
            )?;
        }
    }

    if upstream_protocol == UpstreamProtocol::ChatCompletions {
        if let Some(object) = upstream_body.as_object_mut() {
            if let Some(requested_reasoning_effort) =
                object.get("reasoning_effort").and_then(Value::as_str)
            {
                if let Some(normalized_reasoning_effort) = normalize_reasoning_effort_for_model(
                    &final_upstream_model,
                    requested_reasoning_effort,
                ) {
                    if normalized_reasoning_effort != requested_reasoning_effort {
                        tracing::warn!(
                            request_id = %request_id,
                            downstream_key_id = %downstream_key_id,
                            path = %endpoint.path(),
                            original_model = %model,
                            normalized_model = %normalized_model,
                            selected_upstream_id = %upstream.id,
                            selected_upstream_name = %upstream.name,
                            selected_upstream_protocol = ?upstream_protocol,
                            upstream_model = %request_model,
                            final_upstream_model = %final_upstream_model,
                            requested_reasoning_effort = %requested_reasoning_effort,
                            normalized_reasoning_effort = %normalized_reasoning_effort,
                            "downgraded reasoning effort for upstream compatibility"
                        );
                        object.insert(
                            "reasoning_effort".into(),
                            Value::String(normalized_reasoning_effort.to_string()),
                        );
                    }
                }
            }
        }
    }

    if upstream_protocol == UpstreamProtocol::ChatCompletions {
        normalize_chat_tool_required_arrays(&mut upstream_body);
    }

    if upstream_protocol == UpstreamProtocol::ChatCompletions {
        normalize_chat_payload_for_upstream_compatibility(
            &mut upstream_body,
            &final_upstream_model,
            &upstream.base_url,
            upstream.strip_nonstandard_chat_fields,
        );
        tracing::debug!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            final_upstream_model = %final_upstream_model,
            strip_unknown_nonstandard_fields = upstream.strip_nonstandard_chat_fields,
            "normalized chat payload for upstream compatibility"
        );
        if let Some(resolved) = resolved_capabilities.as_ref() {
            normalize_chat_payload_for_capabilities(&mut upstream_body, resolved);
        }
    }

    if let Some(resolved) = resolved_capabilities.as_ref() {
        if let Some(object) = upstream_body.as_object_mut() {
            if let Some(code) = normalize_image_payload_for_capabilities(
                object,
                &ImageDialect::from_resolved(resolved),
            ) {
                downgrade_codes.insert(code);
            }
        }
    }
    apply_resolved_claude_effort_control(
        &mut upstream_body,
        upstream_protocol,
        resolved_capabilities.as_ref(),
        claude_requested_effort.as_deref(),
    )?;

    let url = join_upstream_url(&upstream.base_url, endpoint_for_upstream(upstream_protocol));
    tracing::info!(
        request_id = %request_id,
        downstream_key_id = %downstream_key_id,
        path = %endpoint.path(),
        original_model = %model,
        normalized_model = %normalized_model,
        selected_upstream_id = %upstream.id,
        selected_upstream_name = %upstream.name,
        selected_upstream_protocol = ?upstream_protocol,
        final_upstream_model = %final_upstream_model,
        url = %url,
        request_stream,
        upstream_attempt_mode = attempt_mode.as_str(),
        "dispatching request to upstream service"
    );
    let mut context_retry_attempted = false;
    let mut dialect_retry_attempted = false;
    let dialect_retry_source_body = upstream_body.clone();
    let response_header_timeout =
        Duration::from_secs(state.config.upstream_response_header_timeout_seconds.max(1));
    if attempt_mode.aggregates_sse() {
        if let Some(active_request_guard) = active_request_guard {
            active_request_guard.arm_aggregate_cancellation_log(GatewayUsageLogContext {
                state: state.clone(),
                request_id: request_id.to_string(),
                downstream_id: downstream_key_id.to_string(),
                downstream_name: downstream_name.to_string(),
                upstream_id: upstream.id.clone(),
                upstream_name: Some(upstream.name.clone()),
                endpoint: endpoint.path().to_string(),
                model: model.to_string(),
                inference_strength: inference_strength.map(str::to_string),
                user_agent: user_agent.map(str::to_string),
                compatibility: None,
                started,
            });
        }
    }
    let response = loop {
        route_attempts.record_physical_attempt(route_health_key.clone());
        let send_future = state
            .client_for_url(&url)
            .post(url.clone())
            .header(header::AUTHORIZATION, format!("Bearer {}", api_key))
            .json(&upstream_body)
            .send();

        let response = match tokio::time::timeout(response_header_timeout, send_future).await {
            Ok(result) => result.map_err(|error| {
                tracing::warn!(
                    request_id = %request_id,
                    downstream_key_id = %downstream_key_id,
                    path = %endpoint.path(),
                    original_model = %model,
                    normalized_model = %normalized_model,
                    selected_upstream_id = %upstream.id,
                    selected_upstream_name = %upstream.name,
                    selected_upstream_protocol = ?upstream_protocol,
                    url = %url,
                    error = %error,
                    "upstream request failed"
                );
                GatewayError::upstream_network_error(format!(
                    "upstream request failed (upstream {}: {}): {error}",
                    upstream.name, url
                ))
            })?,
            Err(_) => {
                tracing::warn!(
                    request_id = %request_id,
                    downstream_key_id = %downstream_key_id,
                    path = %endpoint.path(),
                    original_model = %model,
                    normalized_model = %normalized_model,
                    selected_upstream_id = %upstream.id,
                    selected_upstream_name = %upstream.name,
                    selected_upstream_protocol = ?upstream_protocol,
                    url = %url,
                    header_timeout_seconds = response_header_timeout.as_secs(),
                    "upstream response header timeout"
                );
                return Err(GatewayError::upstream_timeout(format!(
                    "upstream response header timeout after {}s (upstream {}: {})",
                    response_header_timeout.as_secs(),
                    upstream.name,
                    url
                )));
            }
        };

        let status = response.status();
        if status.is_success() {
            break response;
        }

        // Get headers before consuming response with .text()
        let headers = response.headers().clone();
        let error_body = response.bytes().await.unwrap_or_default();
        let error_text = String::from_utf8_lossy(&error_body).to_string();
        let upstream_error_code = extract_upstream_error_code(&error_text);

        if !stream_only_recovery.consumed && !dialect_retry_attempted {
            if let Some(rule) = super::dialect_retry::correction_for_response(
                status,
                &error_body,
                false,
                resolved_capabilities
                    .as_ref()
                    .map(|resolved| resolved.correction_rules.as_slice())
                    .unwrap_or(&[]),
            ) {
                let mut corrected_body = dialect_retry_source_body.clone();
                if super::dialect_retry::apply_correction_rule(&mut corrected_body, &rule) {
                    upstream_body = corrected_body;
                    dialect_retry_attempted = true;
                    dialect_retry_count = 1;
                    upstream_request_guard
                        .reserve_next(state, upstream, model)
                        .await?;
                    continue;
                }
            }
        }

        let classified_feedback = classify_upstream_response(UpstreamFeedbackInput {
            status: status.as_u16(),
            headers: &headers,
            body: Some(&error_text),
            target_model: Some(&final_upstream_model),
        });
        // Retain the legacy summary label for log compatibility while routing
        // decisions use the precise Key-route classification above.
        let feedback = UpstreamFeedbackClassification::from_response(
            status.as_u16(),
            &headers,
            Some(&error_text),
        );
        // Log-facing excerpt: full diagnostic context (status, classification,
        // upstream code, message) for operators reading the server log.
        let error_excerpt = safe_upstream_error_summary(status, upstream_error_code, feedback);
        let upstream_error_message = upstream_client_message(status);

        tracing::warn!(
            request_id = %request_id,
            downstream_key_id = %downstream_key_id,
            path = %endpoint.path(),
            original_model = %model,
            normalized_model = %normalized_model,
            selected_upstream_id = %upstream.id,
            selected_upstream_name = %upstream.name,
            selected_upstream_protocol = ?upstream_protocol,
            url = %url,
            status = status.as_u16(),
            error_excerpt = %error_excerpt,
            feedback_classification = ?feedback,
            context_retry_attempted,
            estimated_input_tokens = ?context_budget_report
                .as_ref()
                .map(|report| report.estimated_input_tokens_after_trim),
            requested_output_tokens = ?context_budget_report
                .as_ref()
                .map(|report| report.requested_output_tokens),
            "upstream responded with a non-success status"
        );

        // serde_json always produces syntactically valid JSON, so a 400 with a
        // JSON-syntax error message from the upstream usually means an
        // intermediate proxy or the upstream rejected a field. Keep this
        // diagnostic structural only; prompts, tool arguments, and tool results
        // must not be written to runtime logs.
        if status.is_client_error() {
            let diagnostics = safe_upstream_body_diagnostics(&upstream_body);
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint.path(),
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream.id,
                selected_upstream_name = %upstream.name,
                selected_upstream_protocol = ?upstream_protocol,
                url = %url,
                status = status.as_u16(),
                upstream_body_json_bytes = diagnostics.json_bytes,
                upstream_body_top_level_field_count = diagnostics.top_level_field_count,
                upstream_body_message_count = ?diagnostics.message_count,
                upstream_body_tool_count = ?diagnostics.tool_count,
                upstream_body_has_stream = diagnostics.has_stream,
                upstream_body_has_reasoning_effort = diagnostics.has_reasoning_effort,
                upstream_body_has_max_output_tokens = diagnostics.has_max_output_tokens,
                upstream_body_has_max_tokens = diagnostics.has_max_tokens,
                upstream_body_has_max_completion_tokens = diagnostics.has_max_completion_tokens,
                upstream_body_has_usage = diagnostics.has_usage,
                upstream_body_has_input_tokens = diagnostics.has_input_tokens,
                upstream_body_has_output_tokens = diagnostics.has_output_tokens,
                upstream_body_has_prompt_tokens = diagnostics.has_prompt_tokens,
                upstream_body_has_completion_tokens = diagnostics.has_completion_tokens,
                "upstream rejected request body; payload values withheld"
            );
            let _ = maybe_queue_dialect_error_probe(
                state,
                &upstream.id,
                &key_fingerprint,
                normalized_model,
                &final_upstream_model,
                upstream_protocol,
                status,
                &error_text,
            )
            .await;
        }

        // Only a bad-request response can enter the legacy context-cap retry.
        // Auth, quota, conflict, and server failures keep their original category.
        if status == StatusCode::BAD_REQUEST && is_context_limit_error(&error_text) {
            if !stream_only_recovery.consumed && !context_retry_attempted {
                if let Some((cap_field, current_cap, reduced_cap)) =
                    halve_generation_cap_for_context_retry(&mut upstream_body)
                {
                    context_retry_attempted = true;
                    tracing::warn!(
                        request_id = %request_id,
                        downstream_key_id = %downstream_key_id,
                        path = %endpoint.path(),
                        original_model = %model,
                        normalized_model = %normalized_model,
                        selected_upstream_id = %upstream.id,
                        selected_upstream_name = %upstream.name,
                        selected_upstream_protocol = ?upstream_protocol,
                        cap_field,
                        current_cap,
                        reduced_cap,
                        "context limit hit; retrying once with reduced output token cap"
                    );
                    upstream_request_guard
                        .reserve_next(state, upstream, model)
                        .await?;
                    continue;
                }
            }
            return Err(GatewayError::upstream_context_limit(format!(
                "upstream request exceeded the model context window; reduce prompt size or use a model with a larger context window (model={final_upstream_model}, upstream={}, status={}, detail={})",
                upstream.name,
                status.as_u16(),
                error_excerpt
            ), status));
        }

        return Err(GatewayError::from_classified_upstream_failure(
            classified_feedback,
            upstream_error_message,
        ));
    };

    let applied_effort_control = applied_claude_effort_control_evidence(
        &upstream_body,
        resolved_capabilities.as_ref(),
        claude_requested_effort.as_deref(),
    );
    let status = response.status();

    if attempt_mode.aggregates_sse() {
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !content_type.contains("text/event-stream") {
            return Err(GatewayError::upstream_invalid_response(
                "upstream returned a non-SSE response for stream aggregation",
                "upstream_stream_content_type_invalid",
            ));
        }
        let upstream_json = aggregate_upstream_sse_response(
            response,
            upstream_protocol,
            StreamTimeouts::from_config(&state.config),
        )
        .await?;
        let body = match (endpoint, upstream_protocol) {
            (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => upstream_json,
            (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
                responses_response_to_chat_payload(&upstream_json)
                    .map_err(protocol_error_to_gateway)?
            }
            (EndpointKind::Responses, UpstreamProtocol::Responses) => upstream_json,
            (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
                chat_response_to_responses_payload_with_tool_registry(
                    &upstream_json,
                    response_history_context
                        .as_ref()
                        .and_then(ResponseHistoryContext::tool_registry),
                )
                .map_err(protocol_error_to_gateway)?
            }
        };

        if status == StatusCode::OK && is_empty_success_response(&body) {
            return Err(GatewayError::upstream_invalid_response(
                "upstream returned an empty response body (no content, zero tokens)",
                "upstream_empty_response",
            ));
        }
        if let Some(context) = response_history_context.as_ref() {
            context.store_from_response_body(&body);
        }
        let usage = usage_from_body(&body);
        let compatibility = resolved_capabilities.as_ref().map(|resolved| {
            build_compatibility_usage_metadata(
                &active_capability_snapshot,
                upstream,
                &key_fingerprint,
                request_model,
                &final_upstream_model,
                upstream_protocol,
                endpoint,
                resolved,
                &downgrade_codes,
                dialect_retry_count,
                claude_request,
                attempt_mode,
            )
        });

        return Ok(DispatchResult {
            status,
            body: DispatchBody::Json(body),
            request_id: String::new(),
            response_headers: {
                let mut headers = HeaderMap::new();
                append_downgrade_header(&mut headers, &downgrade_codes);
                if dialect_retry_count > 0 {
                    headers.insert(
                        header::HeaderName::from_static("x-chat2responses-dialect-retry"),
                        HeaderValue::from_static("1"),
                    );
                }
                headers
            },
            applied_effort_control: applied_effort_control.clone(),
            claude_thinking_signature,
            compatibility,
            usage,
            usage_log_timing: UsageLogTiming::Immediate,
            usage_log_context: None,
            selected_upstream_id: upstream.id.clone(),
            selected_upstream_name: upstream.name.clone(),
            selected_upstream_key_fingerprint: key_fingerprint.clone(),
            selected_upstream_protocol: upstream_protocol,
        });
    }

    if request_stream {
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let stream_timeouts = StreamTimeouts::from_config(&state.config);

        let mut usage_body = None;
        let body = if content_type.contains("text/event-stream") {
            let reader = UpstreamStreamReader::new(response, stream_timeouts);
            let reader = if attempt_mode == UpstreamAttemptMode::SsePassThrough {
                let route_hedge_context =
                    stream_completion_context
                        .as_ref()
                        .map(|completion| RouteHedgeContext {
                            state: state.clone(),
                            capability_snapshot: capability_snapshot.clone(),
                            requested_features: requested_features.clone(),
                            body: body.clone(),
                            endpoint,
                            started,
                            request_id: request_id.to_string(),
                            model: model.to_string(),
                            normalized_model: normalized_model.to_string(),
                            downstream_key_id: downstream_key_id.to_string(),
                            downstream_name: downstream_name.to_string(),
                            inference_strength: inference_strength.map(str::to_string),
                            user_agent: user_agent.map(str::to_string),
                            downstream_concurrency_guard: completion
                                .downstream_concurrency_guard
                                .clone(),
                            route_attempts: route_attempts.clone(),
                            response_history_context: response_history_context.clone(),
                            stream_only_recovery_request_safe,
                        });
                match prefetch_stream_with_hedges(
                    state,
                    upstream,
                    reader,
                    api_key,
                    hedge_api_keys,
                    route_hedge_candidates,
                    route_hedge_context,
                    &url,
                    &upstream_body,
                    request_model,
                    upstream_protocol,
                    response_header_timeout,
                    stream_timeouts,
                    request_id,
                    started,
                )
                .await?
                {
                    PrefetchedStreamWinner::Reader(reader) => *reader,
                    PrefetchedStreamWinner::Dispatch(result) => return Ok(*result),
                }
            } else {
                reader
            };
            let stream_log_context = StreamUsageLogContext {
                state: state.clone(),
                request_id: request_id.to_string(),
                downstream_key_id: downstream_key_id.to_string(),
                downstream_name: Some(downstream_name.to_string()),
                upstream_key_id: upstream.id.clone(),
                upstream_name: Some(upstream.name.clone()),
                upstream_protocol,
                endpoint: endpoint.path().to_string(),
                model: model.to_string(),
                inference_strength: inference_strength.map(str::to_string),
                user_agent: user_agent.map(str::to_string),
                compatibility: None,
                normalized_model: normalized_model.to_string(),
                status,
                error_message: None,
                error_category: None,
                started,
                hedge_control: hedge_control.clone(),
            };
            if upstream_protocol == endpoint.native_protocol() {
                proxied_stream_body(
                    reader,
                    endpoint,
                    stream_log_context,
                    stream_completion_context,
                    response_history_context,
                )?
            } else {
                translated_stream_body(
                    reader,
                    upstream_protocol,
                    endpoint.native_protocol(),
                    endpoint,
                    stream_log_context,
                    stream_completion_context,
                    response_history_context,
                )?
            }
        } else {
            let bytes = response.bytes().await.map_err(|error| {
                GatewayError::upstream_network_error(format!(
                    "failed to read upstream response: {error}"
                ))
            })?;
            let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
                GatewayError::upstream_invalid_response(
                    format!("upstream returned invalid json: {error}"),
                    "upstream_invalid_response",
                )
            })?;

            let final_body = match (endpoint, upstream_protocol) {
                (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => upstream_json,
                (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
                    responses_response_to_chat_payload(&upstream_json)
                        .map_err(protocol_error_to_gateway)?
                }
                (EndpointKind::Responses, UpstreamProtocol::Responses) => upstream_json,
                (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
                    chat_response_to_responses_payload_with_tool_registry(
                        &upstream_json,
                        response_history_context
                            .as_ref()
                            .and_then(ResponseHistoryContext::tool_registry),
                    )
                    .map_err(protocol_error_to_gateway)?
                }
            };

            if status == StatusCode::OK && is_empty_success_response(&final_body) {
                return Err(GatewayError::upstream_invalid_response(
                    "upstream returned an empty response body (no content, zero tokens)",
                    "upstream_empty_response",
                ));
            }

            if let Some(context) = response_history_context.as_ref() {
                context.store_from_response_body(&final_body);
            }

            usage_body = Some(final_body.clone());
            synthesize_stream_body(endpoint, &final_body)?
        };

        let compatibility = resolved_capabilities.as_ref().map(|resolved| {
            build_compatibility_usage_metadata(
                &active_capability_snapshot,
                upstream,
                &key_fingerprint,
                request_model,
                &final_upstream_model,
                upstream_protocol,
                endpoint,
                resolved,
                &downgrade_codes,
                dialect_retry_count,
                claude_request,
                attempt_mode,
            )
        });

        return Ok(DispatchResult {
            status,
            body: DispatchBody::Stream(body),
            request_id: String::new(),
            response_headers: {
                let mut headers = HeaderMap::new();
                append_downgrade_header(&mut headers, &downgrade_codes);
                if dialect_retry_count > 0 {
                    headers.insert(
                        header::HeaderName::from_static("x-chat2responses-dialect-retry"),
                        HeaderValue::from_static("1"),
                    );
                }
                headers
            },
            applied_effort_control: applied_effort_control.clone(),
            claude_thinking_signature,
            compatibility,
            usage_log_timing: if usage_body.is_some() {
                UsageLogTiming::Immediate
            } else {
                UsageLogTiming::DeferredUntilStreamEnd
            },
            usage: usage_body
                .as_ref()
                .map(usage_from_body)
                .unwrap_or((0, 0, 0)),
            usage_log_context: None,
            selected_upstream_id: upstream.id.clone(),
            selected_upstream_name: upstream.name.clone(),
            selected_upstream_key_fingerprint: key_fingerprint.clone(),
            selected_upstream_protocol: upstream_protocol,
        });
    }

    let bytes = response.bytes().await.map_err(|error| {
        GatewayError::upstream_network_error(format!("failed to read upstream response: {error}"))
    })?;
    let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
        GatewayError::upstream_invalid_response(
            format!("upstream returned invalid json: {error}"),
            "upstream_invalid_response",
        )
    })?;

    let stream_only_recovery_candidate =
        has_explicit_zero_output_usage(&upstream_json, upstream_protocol)
            && is_empty_success_response(&upstream_json);

    let body = match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => upstream_json,
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
            responses_response_to_chat_payload(&upstream_json).map_err(protocol_error_to_gateway)?
        }
        (EndpointKind::Responses, UpstreamProtocol::Responses) => upstream_json,
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
            chat_response_to_responses_payload_with_tool_registry(
                &upstream_json,
                response_history_context
                    .as_ref()
                    .and_then(ResponseHistoryContext::tool_registry),
            )
            .map_err(protocol_error_to_gateway)?
        }
    };

    let usage = usage_from_body(&body);

    if status == StatusCode::OK && is_empty_success_response(&body) {
        return Err(if stream_only_recovery_candidate {
            recoverable_upstream_empty_response_error()
        } else {
            upstream_empty_response_error()
        });
    }

    if let Some(context) = response_history_context.as_ref() {
        context.store_from_response_body(&body);
    }

    let compatibility = resolved_capabilities.as_ref().map(|resolved| {
        build_compatibility_usage_metadata(
            &active_capability_snapshot,
            upstream,
            &key_fingerprint,
            request_model,
            &final_upstream_model,
            upstream_protocol,
            endpoint,
            resolved,
            &downgrade_codes,
            dialect_retry_count,
            claude_request,
            attempt_mode,
        )
    });

    Ok(DispatchResult {
        status,
        body: DispatchBody::Json(body),
        request_id: String::new(),
        response_headers: {
            let mut headers = HeaderMap::new();
            append_downgrade_header(&mut headers, &downgrade_codes);
            if dialect_retry_count > 0 {
                headers.insert(
                    header::HeaderName::from_static("x-chat2responses-dialect-retry"),
                    HeaderValue::from_static("1"),
                );
            }
            headers
        },
        applied_effort_control,
        claude_thinking_signature,
        compatibility,
        usage,
        usage_log_timing: UsageLogTiming::Immediate,
        usage_log_context: None,
        selected_upstream_id: upstream.id.clone(),
        selected_upstream_name: upstream.name.clone(),
        selected_upstream_key_fingerprint: key_fingerprint,
        selected_upstream_protocol: upstream_protocol,
    })
}

pub(super) fn no_routable_model_error(
    snapshot: &crate::state::PersistedState,
    model: &str,
) -> GatewayError {
    let mut visible_models = snapshot
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
        .flat_map(|upstream| upstream.route_models())
        .collect::<Vec<_>>();
    visible_models.sort();
    visible_models.dedup();

    let message = if visible_models.is_empty() {
        format!(
            "model \"{model}\" is not configured on any active upstream; check supported_models"
        )
    } else {
        format!(
            "model \"{model}\" is not configured on any active upstream; available models: {}; check supported_models",
            visible_models.join(", ")
        )
    };
    GatewayError::classified(
        StatusCode::BAD_REQUEST,
        message,
        "invalid_request_error",
        "gateway_no_routable_upstream",
        "gateway_no_routable_upstream",
        None,
        Some(json!({ "scope": "gateway" })),
    )
}

pub(super) fn endpoint_for_upstream(protocol: UpstreamProtocol) -> &'static str {
    match protocol {
        UpstreamProtocol::ChatCompletions => "/v1/chat/completions",
        UpstreamProtocol::Responses => "/v1/responses",
    }
}

pub(super) fn protocol_transition_label(
    endpoint: EndpointKind,
    upstream_protocol: UpstreamProtocol,
) -> &'static str {
    match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => "native",
        (EndpointKind::Responses, UpstreamProtocol::Responses) => "native",
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => "chat_to_responses",
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => "responses_to_chat",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{
        CapabilitySource, EvidenceState, ReasoningCarrier, ReasoningMode, ResolvedCapability,
        TokenLimitField,
    };
    use std::collections::BTreeMap;

    #[test]
    fn hedge_schedule_uses_configured_initial_delay_and_interval() {
        let config = AppConfig {
            upstream_hedge_delay_ms: 25,
            upstream_hedge_interval_ms: 80,
            ..AppConfig::default()
        };

        assert_eq!(hedge_launch_delay(&config, 0), Duration::from_millis(25));
        assert_eq!(hedge_launch_delay(&config, 1), Duration::from_millis(80));
        assert_eq!(hedge_launch_delay(&config, 3), Duration::from_millis(80));
    }

    #[test]
    fn hedge_attempt_limit_respects_toggle_budget_and_available_accounts() {
        let mut config = AppConfig {
            upstream_hedge_enabled: true,
            upstream_hedge_max_extra_attempts: 3,
            ..AppConfig::default()
        };

        assert_eq!(hedge_extra_attempt_limit(&config, 5), 3);
        assert_eq!(hedge_extra_attempt_limit(&config, 2), 2);
        config.upstream_hedge_enabled = false;
        assert_eq!(hedge_extra_attempt_limit(&config, 5), 0);
    }

    fn reasoning_control(field: &str, mapped: &str) -> ResolvedCapabilities {
        ResolvedCapabilities {
            values: BTreeMap::from([
                (
                    Capability::ReasoningOutput,
                    ResolvedCapability {
                        state: EvidenceState::Supported,
                        source: CapabilitySource::Probe,
                    },
                ),
                (
                    Capability::ReasoningReplay,
                    ResolvedCapability {
                        state: EvidenceState::Supported,
                        source: CapabilitySource::Probe,
                    },
                ),
            ]),
            token_limit_field: TokenLimitField::Omit,
            reasoning_mode: ReasoningMode::Optional,
            reasoning_carrier: ReasoningCarrier::ResponsesReasoningItem,
            correction_rules: Vec::new(),
            reasoning_control_field: Some(field.into()),
            effort_map: BTreeMap::from([("high".into(), mapped.into())]),
            omit_sampling_fields: BTreeSet::new(),
            context_window: None,
            max_output_tokens: None,
            omit_optional_extensions: false,
            profile_state: DialectProfileState::Verified,
            provisional: false,
            native_preferred: false,
            adapters: BTreeSet::new(),
            request_extensions: Vec::new(),
            field_sources: BTreeMap::new(),
        }
    }

    #[test]
    fn claude_effort_collision_preserves_responses_reasoning_object() {
        let mut body = json!({
            "model": "opaque-runtime",
            "input": "hello",
            "reasoning": {
                "effort": "high",
                "summary": "auto"
            }
        });
        let before = body.clone();

        let _result = apply_resolved_claude_effort_control(
            &mut body,
            UpstreamProtocol::Responses,
            Some(&reasoning_control("reasoning", "maximum")),
            Some("high"),
        );

        assert_eq!(body, before);
    }
}
