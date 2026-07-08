use super::*;

pub(super) fn parse_retry_after_seconds(
    headers: &reqwest::header::HeaderMap,
    default_retry_seconds: u64,
) -> u64 {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_retry_seconds.max(1))
        .max(1)
}

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

pub(super) fn extract_upstream_error_message(error_text: &str) -> String {
    let parsed = parse_upstream_error_payload(error_text);

    // Prefer the human-readable message field from the upstream error body.
    // Only fall back to the code field when no message is present, because
    // non-numeric codes such as "bad_response_status_code" carry no useful
    // diagnostic information for the downstream client.
    if let Some(message) = parsed.message.filter(|message| !message.trim().is_empty()) {
        return message;
    }

    if let Some(code) = parsed.code.as_deref() {
        if !code.is_empty() && code.parse::<u16>().is_err() {
            return code.to_string();
        }
    }

    error_text.to_string()
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
    // Key rotation is only useful for failures that may be credential-specific.
    // Shared upstream concurrency pressure should stay on the same key long
    // enough for the account-level backoff loop to retry first.
    match error {
        GatewayError::Unauthorized(_)
        | GatewayError::TooManyRequests { .. }
        | GatewayError::GatewayTimeout(_)
        | GatewayError::Upstream(_)
        | GatewayError::TemporaryUpstreamUnavailable(_) => true,
        GatewayError::Classified { status, meta, .. } => {
            meta.category == "upstream_auth_error"
                || (meta.category.starts_with("upstream_")
                    && (*status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()))
        }
        _ => false,
    }
}

pub(super) fn should_retry_without_stream(error: &GatewayError) -> bool {
    match error {
        GatewayError::GatewayTimeout(_)
        | GatewayError::Upstream(_)
        | GatewayError::TemporaryUpstreamUnavailable(_) => true,
        GatewayError::Classified { status, meta, .. } => {
            meta.category.starts_with("upstream_") && status.is_server_error()
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_to_upstream(
    state: &AppState,
    upstream: &UpstreamConfig,
    api_key: &str,
    upstream_protocol: UpstreamProtocol,
    body: &Value,
    endpoint: EndpointKind,
    request_stream: bool,
    try_upstream_stream: bool,
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
    response_history_context: Option<ResponseHistoryContext>,
) -> Result<DispatchResult, GatewayError> {
    let upstream_body = match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => body.clone(),
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
            chat_request_to_responses_payload(body).map_err(protocol_error_to_gateway)?
        }
        (EndpointKind::Responses, UpstreamProtocol::Responses) => body.clone(),
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
            responses_request_to_chat_payload_with_fallback(body)
                .map_err(protocol_error_to_gateway)?
        }
    };
    let mut upstream_body = upstream_body;
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
    let model_rewritten = final_upstream_model != request_model;
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
        try_upstream_stream,
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
    if !try_upstream_stream {
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert("stream".into(), Value::Bool(false));
        }
    } else if upstream_protocol == UpstreamProtocol::ChatCompletions {
        if let Some(object) = upstream_body.as_object_mut() {
            object.insert(
                "stream_options".into(),
                json!({
                    "include_usage": true
                }),
            );
        }
    }

    let context_budget_report = apply_context_budget_controls(
        upstream,
        global_context_profile,
        &mut upstream_body,
        &final_upstream_model,
    );
    if let Some(report) = context_budget_report.as_ref() {
        if let Some(switched_model) = report.fallback_model.as_ref() {
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
    }

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
        try_upstream_stream,
        "dispatching request to upstream service"
    );
    let mut context_retry_attempted = false;
    let mut tool_choice_tool_retry_attempted = false;
    let response_header_timeout =
        Duration::from_secs(state.config.upstream_response_header_timeout_seconds.max(1));
    let response = loop {
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
                GatewayError::upstream_network_error(format!("upstream request failed (upstream {}: {}): {error}", upstream.name, url))
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
        let error_text = response.text().await.unwrap_or_default();
        let raw_upstream_error_message = extract_upstream_error_message(&error_text);
        // Some upstreams (e.g. huazi) wrap the real error in a body whose
        // `code`/`type` fields are the literal string "bad_response_status_code".
        // That signals a request-format problem that may be caused by tools /
        // tool_choice, so we detect it from the raw body rather than the
        // extracted message (which now prefers the human-readable `message`
        // field over the non-numeric `code`).
        let upstream_error_is_bad_response_status_code =
            error_text.contains("bad_response_status_code");
        let upstream_error_code = extract_upstream_error_code(&error_text);

        // Classify the upstream response to determine how to handle it
        let feedback = UpstreamFeedbackClassification::from_response(
            status.as_u16(),
            &headers,
            Some(&error_text),
        );
        // Log-facing excerpt: full diagnostic context (status, classification,
        // upstream code, message) for operators reading the server log.
        let error_excerpt = safe_upstream_error_summary(status, upstream_error_code, feedback, &raw_upstream_error_message);
        // Client-facing message: the upstream's real error text (e.g.
        // "This token has no access to model deepseek-v4-pro"). Falls back to
        // a status-based hint when the upstream body had no parseable message.
        let upstream_error_message = upstream_client_message(status, &raw_upstream_error_message);

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
        }

        // Handle context limit errors first (before feedback classification)
        if is_context_limit_error(&error_text) {
            if !context_retry_attempted {
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

        if !tool_choice_tool_retry_attempted
            && protocol_path == "responses_to_chat"
            && upstream_error_is_bad_response_status_code
            && (upstream_body.get("tools").is_some() || upstream_body.get("tool_choice").is_some())
        {
            if let Some(object) = upstream_body.as_object_mut() {
                object.remove("tools");
                object.remove("tool_choice");
            }
            tool_choice_tool_retry_attempted = true;
            tracing::warn!(
                request_id = %request_id,
                downstream_key_id = %downstream_key_id,
                path = %endpoint.path(),
                original_model = %model,
                normalized_model = %normalized_model,
                selected_upstream_id = %upstream.id,
                selected_upstream_name = %upstream.name,
                selected_upstream_protocol = ?upstream_protocol,
                protocol_transition = %protocol_path,
                status = status.as_u16(),
                "responses_to_chat retrying without tools/tool_choice after bad_response_status_code (status={})",
                status.as_u16()
            );
            continue;
        }

        // If we already retried without tools/tool_choice and still get bad_response_status_code,
        // the upstream simply doesn't support this model/request. Try next upstream.
        //
        // However, if the persistent status is 401/403, the upstream is refusing
        // authorization (bad API key or model not permitted for this key), not
        // signalling a protocol/feature gap. Classifying that as
        // upstream_protocol_unsupported (503) masks the real problem and prevents
        // the outer loop from trying the next upstream/key. Surface it as an auth
        // error instead.
        if tool_choice_tool_retry_attempted && upstream_error_is_bad_response_status_code {
            if matches!(status.as_u16(), 401 | 403) {
                return Err(GatewayError::upstream_auth_error(upstream_error_message.clone(), status));
            }
            return Err(GatewayError::upstream_temporary_unavailable(
                upstream_error_message.clone(),
                "upstream_protocol_unsupported",
            ));
        }

        if matches!(status.as_u16(), 401 | 403) {
            return Err(GatewayError::upstream_auth_error(upstream_error_message.clone(), status));
        }

        if matches!(
            feedback,
            UpstreamFeedbackClassification::ProtocolUnsupported
        ) {
            return Err(GatewayError::upstream_temporary_unavailable(
                upstream_error_message.clone(),
                "upstream_protocol_unsupported",
            ));
        }

        // When the upstream HTTP status is 5xx, the upstream itself is failing.
        // A nested 4xx inner_code in the body (e.g. 400 "bad request") does not mean
        // the *gateway* client request was bad; treat as temporary so we try another upstream.
        let upstream_is_server_error = status.is_server_error();

        if let Some(inner_code) = upstream_error_code {
            if (400..=499).contains(&inner_code) {
                if upstream_is_server_error {
                    return Err(GatewayError::upstream_temporary_unavailable(
                        upstream_error_message.clone(),
                        "upstream_temporary_unavailable",
                    ));
                }

                if inner_code == 429 {
                    let retry_after_seconds = parse_retry_after_seconds(
                        &headers,
                        state.config.upstream_rate_limit_default_retry_seconds,
                    );
                    return Err(GatewayError::TooManyRequests {
                        message: upstream_error_message.clone(),
                        retry_after_seconds: Some(retry_after_seconds),
                    });
                }
                return Err(GatewayError::upstream_bad_request(
                    if upstream_error_message.is_empty() {
                        format!("upstream rejected request with status {inner_code}")
                    } else {
                        upstream_error_message
                    },
                    status,
                ));
            }
        }

        // Handle feedback-based decisions
        match feedback {
            UpstreamFeedbackClassification::RateLimited => {
                let retry_after_seconds = parse_retry_after_seconds(
                    &headers,
                    state.config.upstream_rate_limit_default_retry_seconds,
                );
                return Err(GatewayError::TooManyRequests {
                    message: upstream_error_message.clone(),
                    retry_after_seconds: Some(retry_after_seconds),
                });
            }
            UpstreamFeedbackClassification::ConcurrencyFull => {
                return Err(GatewayError::ConcurrencyFull {
                    message: upstream_error_message.clone(),
                    retry_after_seconds: None,
                });
            }
            UpstreamFeedbackClassification::ProviderBusy
            | UpstreamFeedbackClassification::TemporaryUnavailable => {
                // Return error to allow outer loop to try next upstream
                return Err(GatewayError::upstream_temporary_unavailable(
                    upstream_error_message.clone(),
                    "upstream_temporary_unavailable",
                ));
            }
            UpstreamFeedbackClassification::ProtocolUnsupported => {
                // Protocol not supported, return error to try next upstream
                return Err(GatewayError::upstream_temporary_unavailable(
                    upstream_error_message.clone(),
                    "upstream_protocol_unsupported",
                ));
            }
            UpstreamFeedbackClassification::Unknown => {
                // Unknown error - pass through client errors (4xx) as BadRequest,
                // server errors (5xx) as Upstream. The upstream_error_message
                // already contains a clear, client-facing description.
                if status.is_client_error() {
                    return Err(GatewayError::upstream_bad_request(
                        upstream_error_message.clone(),
                        status,
                    ));
                } else {
                    return Err(GatewayError::Upstream(upstream_error_message.clone()));
                }
            }
        }
    };

    let status = response.status();

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
                normalized_model: normalized_model.to_string(),
                status,
                error_message: None,
                error_category: None,
                started,
            };
            if upstream_protocol == endpoint.native_protocol() {
                proxied_stream_body(
                    response,
                    endpoint,
                    stream_log_context,
                    stream_completion_context,
                    response_history_context,
                    stream_timeouts,
                )?
            } else {
                translated_stream_body(
                    response,
                    upstream_protocol,
                    endpoint.native_protocol(),
                    endpoint,
                    stream_log_context,
                    stream_completion_context,
                    response_history_context,
                    stream_timeouts,
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
                    chat_response_to_responses_payload(&upstream_json)
                        .map_err(protocol_error_to_gateway)?
                }
            };

            if let Some(context) = response_history_context.as_ref() {
                context.store_from_response_body(&final_body);
            }

            if status == StatusCode::OK && is_empty_success_response(&final_body) {
                return Err(GatewayError::upstream_invalid_response(
                    "upstream returned an empty response body (no content, zero tokens)",
                    "upstream_empty_response",
                ));
            }

            usage_body = Some(final_body.clone());
            synthesize_stream_body(endpoint, &final_body)?
        };

        return Ok(DispatchResult {
            status,
            body: DispatchBody::Stream(body),
            request_id: String::new(),
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

    let body = match (endpoint, upstream_protocol) {
        (EndpointKind::ChatCompletions, UpstreamProtocol::ChatCompletions) => upstream_json,
        (EndpointKind::ChatCompletions, UpstreamProtocol::Responses) => {
            responses_response_to_chat_payload(&upstream_json).map_err(protocol_error_to_gateway)?
        }
        (EndpointKind::Responses, UpstreamProtocol::Responses) => upstream_json,
        (EndpointKind::Responses, UpstreamProtocol::ChatCompletions) => {
            chat_response_to_responses_payload(&upstream_json).map_err(protocol_error_to_gateway)?
        }
    };

    if let Some(context) = response_history_context.as_ref() {
        context.store_from_response_body(&body);
    }

    let usage = usage_from_body(&body);

    if status == StatusCode::OK && is_empty_success_response(&body) {
        return Err(GatewayError::upstream_invalid_response(
            "upstream returned an empty response body (no content, zero tokens)",
            "upstream_empty_response",
        ));
    }

    Ok(DispatchResult {
        status,
        body: DispatchBody::Json(body),
        request_id: String::new(),
        usage,
        usage_log_timing: UsageLogTiming::Immediate,
        usage_log_context: None,
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
