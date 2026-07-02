use super::*;

#[test]
fn sse_keepalive_frame_is_a_data_event_not_a_comment() {
    // SSE comment frames (": keepalive\n\n") are silently dropped by
    // client SSE parsers and do NOT reset client-side idle timers such as
    // Codex's `stream_idle_timeout_ms`. The keepalive must carry a real
    // `data:` field so downstream clients count it as stream activity.
    let frame = sse_keepalive_frame();
    let text = std::str::from_utf8(&frame).unwrap();
    assert!(
        !text.starts_with(':'),
        "keepalive frame must not be a comment, got: {text:?}"
    );
    assert!(
        text.contains("data:"),
        "keepalive frame must include a data field, got: {text:?}"
    );
    assert!(
        text.ends_with("\n\n"),
        "keepalive frame must be terminated with a blank line, got: {text:?}"
    );
}

#[test]
fn chat_keepalive_frame_is_a_comment_not_a_data_event() {
    let frame = sse_keepalive_frame_for_endpoint(EndpointKind::ChatCompletions);
    let text = std::str::from_utf8(&frame).unwrap();
    assert!(
        text.starts_with(':'),
        "chat keepalive frame must be a comment, got: {text:?}"
    );
    assert!(
        text.ends_with("\n\n"),
        "chat keepalive frame must be terminated with a blank line, got: {text:?}"
    );
}

#[test]
fn downstream_disconnect_stays_499() {
    let (status, category) = classify_stream_failure("stream disconnected before completion");
    assert_eq!(status, StatusCode::from_u16(499).unwrap());
    assert_eq!(category, "stream_interrupted");
}

#[test]
fn drop_message_no_usage_means_cancelled_before_output() {
    assert_eq!(
        stream_drop_interruption_message(None),
        "client disconnected before any upstream output"
    );
    assert_eq!(
        stream_drop_interruption_message(Some((0, 0, 0))),
        "client disconnected before any upstream output"
    );
}

#[test]
fn drop_message_with_usage_means_partial_output() {
    assert_eq!(
        stream_drop_interruption_message(Some((100, 5, 105))),
        "client disconnected during stream (partial output received)"
    );
}

#[test]
fn client_cancelled_before_output_is_categorized() {
    // Codex/user cancelled the turn before any upstream output arrived.
    let (status, category) =
        classify_stream_failure("client disconnected before any upstream output");
    assert_eq!(status, StatusCode::from_u16(499).unwrap());
    assert_eq!(category, "stream_client_cancelled");
}

#[test]
fn official_openai_chat_upstream_detection_is_limited_to_official_hosts() {
    assert!(is_likely_official_openai_chat_upstream(
        "https://api.openai.com/v1"
    ));
    assert!(is_likely_official_openai_chat_upstream(
        "https://example.openai.azure.com/openai/deployments/test"
    ));
    assert!(!is_likely_official_openai_chat_upstream(
        "https://api.openai.com.proxy.local/v1"
    ));
    assert!(!is_likely_official_openai_chat_upstream(
        "https://example.openai.azure.com.evil/openai/deployments/test"
    ));
    assert!(!is_likely_official_openai_chat_upstream(
        "https://api.chatanywhere.tech"
    ));
    assert!(!is_likely_official_openai_chat_upstream(
        "https://huazi.de5.net"
    ));
}

#[test]
fn safe_upstream_body_diagnostics_do_not_include_payload_values() {
    let diagnostics = safe_upstream_body_diagnostics(&json!({
        "model": "gpt-5.1-ca",
        "messages": [{
            "role": "user",
            "content": "secret prompt that must not enter logs"
        }],
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup_secret",
                "arguments": "{\"token\":\"tool-secret\"}"
            }
        }],
        "api_key": "request-secret",
        "max_tokens": 1000,
        "stream": true
    }));

    let rendered = format!("{diagnostics:?}");
    assert!(rendered.contains("json_bytes"));
    assert!(rendered.contains("message_count"));
    assert!(rendered.contains("tool_count"));
    for sensitive in [
        "secret prompt",
        "tool-secret",
        "request-secret",
        "lookup_secret",
        "gpt-5.1-ca",
    ] {
        assert!(
            !rendered.contains(sensitive),
            "safe diagnostics must not include payload value {sensitive:?}: {rendered}"
        );
    }
}

#[test]
fn safe_upstream_error_summary_does_not_include_upstream_message_text() {
    let upstream_message = "expecting , delimiter near SECRET_PROMPT_BODY_SHOULD_NOT_LEAK";
    let summary = safe_upstream_error_summary(
        StatusCode::BAD_REQUEST,
        Some(400),
        UpstreamFeedbackClassification::Unknown,
    );

    assert!(summary.contains("status 400"));
    assert!(summary.contains("upstream code 400"));
    assert!(
        !summary.contains(upstream_message),
        "safe summary must not include raw upstream error text: {summary}"
    );
    assert!(
        !summary.contains("SECRET_PROMPT_BODY"),
        "safe summary must not include echoed request content: {summary}"
    );
}

#[test]
fn upstream_error_code_extraction_ignores_numbers_from_freeform_echoed_message() {
    let error_text = json!({
        "error": {
            "message": "parse failed near {\"code\":\"1234\",\"token\":\"secret\"}",
            "type": "badrequesterror"
        }
    })
    .to_string();

    assert_eq!(extract_upstream_error_code(&error_text), None);
}

#[test]
fn client_disconnected_during_partial_output_is_categorized() {
    // Downstream closed mid-stream after some (incomplete) output but
    // before the completion signal. Distinct from a clean cancel.
    let (status, category) =
        classify_stream_failure("client disconnected during stream (partial output received)");
    assert_eq!(status, StatusCode::from_u16(499).unwrap());
    assert_eq!(category, "stream_incomplete_close");
}

#[test]
fn upstream_stream_read_error_is_bad_gateway() {
    let (status, category) =
        classify_upstream_stream_error("error decoding response body: unexpected eof", false, true);
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(category, "stream_upstream_body_decode_error");
}

#[test]
fn upstream_stream_timeout_is_gateway_timeout() {
    let (status, category) = classify_upstream_stream_error(
        "request timed out while reading upstream response",
        true,
        false,
    );
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(category, "stream_upstream_timeout");
}
