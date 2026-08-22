use super::*;

fn assert_class(status: u16, body: &str, expected: FailureClass) {
    let headers = reqwest::header::HeaderMap::new();
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status,
        headers: &headers,
        body: Some(body),
        target_model: Some("glm-5.2"),
    });
    assert_eq!(classified.class, expected);
}

fn assert_semantic(status: u16, body: &str, expected: UpstreamResponseSemantic) {
    let headers = reqwest::header::HeaderMap::new();
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status,
        headers: &headers,
        body: Some(body),
        target_model: Some("glm-5.2"),
    });
    assert_eq!(classified.semantic, expected);
}

#[test]
fn explicit_concurrency_semantics_are_status_independent() {
    for status in [429, 502, 503] {
        for body in [
            r#"{"error":{"code":"concurrency_limit_exceeded"}}"#,
            r#"{"error":{"message":"concurrency limit exceeded"}}"#,
            r#"{"error":{"message":"您当前使用该API的并发数过高"}}"#,
            r#"{"error":{"message":"当前分组上游负载已饱和"}}"#,
        ] {
            assert_semantic(status, body, UpstreamResponseSemantic::ExplicitConcurrency);
        }
    }
}

#[test]
fn explicit_context_overflow_wins_over_outer_5xx() {
    for status in [400, 413, 502, 503] {
        assert_semantic(
            status,
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#,
            UpstreamResponseSemantic::ExplicitContextOverflow,
        );
    }
}

#[test]
fn generic_busy_and_relay_failures_are_not_concurrency_or_context() {
    for body in [
        r#"{"error":{"message":"server busy"}}"#,
        r#"{"error":{"message":"temporarily unavailable"}}"#,
        r#"{"error":{"code":"relay_error","message":"relay failed"}}"#,
    ] {
        assert_semantic(503, body, UpstreamResponseSemantic::Generic);
    }
}

#[test]
fn classifies_route_failures_by_precedence() {
    assert_class(
        500,
        r#"{"error":{"code":"openai_error"}}"#,
        FailureClass::TransientServer,
    );
    assert_class(
        503,
        r#"{"error":{"message":"no available channel for model glm-5.2 under group free"}}"#,
        FailureClass::CapacityUnavailable,
    );
    assert_class(
        400,
        r#"{"error":{"message":"model is not supported"}}"#,
        FailureClass::ModelUnsupported,
    );
    assert_class(
        400,
        r#"{"error":{"message":"level \"xhigh\" not supported"}}"#,
        FailureClass::FeatureUnsupported,
    );
    assert_class(
        404,
        r#"{"error":{"message":"endpoint not found"}}"#,
        FailureClass::ProtocolUnsupported,
    );
    assert_class(
        400,
        r#"{"error":{"message":"invalid request"}}"#,
        FailureClass::RequestRejected,
    );
    assert_class(401, "{}", FailureClass::Credentials);
    assert_class(429, "{}", FailureClass::RateLimited);
}

#[test]
fn no_available_channel_for_another_model_is_not_a_target_capacity_signal() {
    assert_class(
        503,
        r#"{"error":{"message":"no available channel for model other-model"}}"#,
        FailureClass::TransientServer,
    );
}

#[test]
fn outer_server_status_wins_over_nested_client_code() {
    assert_class(
        503,
        r#"{"error":{"inner_code":400,"message":"invalid request"}}"#,
        FailureClass::TransientServer,
    );
}

#[test]
fn key_quota_requires_structured_key_scope() {
    assert_class(
        429,
        r#"{"error":{"code":"quota_exhausted","scope":"key"}}"#,
        FailureClass::KeyQuota,
    );
    assert_class(
        429,
        r#"{"error":{"message":"quota exceeded for this key"}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn chinese_concurrency_429_is_capacity_unavailable() {
    // GLM/Zhipu concurrency saturation (error code 1302 family) reports in
    // Chinese; it must ride the fast concurrency recovery path instead of the
    // 30s+ exponential rate-limit cooldown.
    assert_class(
        429,
        r#"{"error":{"code":"1302","message":"您当前使用该API的并发数过高，请降低并发，或联系客服增加限额"}}"#,
        FailureClass::CapacityUnavailable,
    );
    assert_class(
        429,
        r#"{"error":{"message":"当前分组上游负载已饱和，请稍后再试"}}"#,
        FailureClass::CapacityUnavailable,
    );
}

#[test]
fn numeric_429_status_code_without_concurrency_semantics_stays_rate_limited() {
    assert_class(
        429,
        r#"{"error":{"code":"429","message":"relay unavailable"}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn generic_busy_429_without_concurrency_semantics_stays_rate_limited() {
    assert_class(
        429,
        r#"{"error":{"message":"server is busy, please retry later"}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn explicit_concurrency_semantics_win_over_rate_limit_wording() {
    assert_class(
        429,
        r#"{"error":{"message":"concurrency limit exceeded; rate limit reached"}}"#,
        FailureClass::CapacityUnavailable,
    );
}

#[test]
fn structured_concurrency_code_is_capacity_unavailable_without_message() {
    assert_class(
        429,
        r#"{"error":{"code":"concurrency_limit_exceeded","message":"relay unavailable"}}"#,
        FailureClass::CapacityUnavailable,
    );
}

#[test]
fn concurrency_code_only_429_is_capacity_unavailable() {
    assert_class(
        429,
        r#"{"error":{"code":"concurrency_limit_exceeded"}}"#,
        FailureClass::CapacityUnavailable,
    );
}

#[test]
fn structured_429_preserves_model_feature_and_protocol_rejections() {
    assert_class(
        429,
        r#"{"error":{"code":"model_not_found","message":"model not found"}}"#,
        FailureClass::ModelUnsupported,
    );
    assert_class(
        429,
        r#"{"error":{"code":"feature_unsupported","message":"stream not supported"}}"#,
        FailureClass::FeatureUnsupported,
    );
    assert_class(
        429,
        r#"{"error":{"code":"protocol_unsupported","message":"responses not supported"}}"#,
        FailureClass::ProtocolUnsupported,
    );
}

#[test]
fn unknown_structured_429_without_status_signal_stays_rate_limited() {
    assert_class(
        429,
        r#"{"error":{"code":"relay_error","message":"relay unavailable"}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn request_rejection_429_does_not_enter_account_recovery() {
    assert_class(
        429,
        r#"{"error":{"code":"request_rejected","message":"request rejected"}}"#,
        FailureClass::RequestRejected,
    );
}

#[test]
fn quota_429_without_key_scope_does_not_enter_account_recovery() {
    assert_class(
        429,
        r#"{"error":{"type":"insufficient_quota","message":"check your plan and billing details"}}"#,
        FailureClass::RateLimited,
    );
    assert_class(
        429,
        r#"{"error":{"code":"quota_exhausted","message":"billing limit reached"}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn explicit_rate_limit_429_remains_rate_limited() {
    assert_class(
        429,
        r#"{"error":{"code":"rate_limit_error","message":"try again later"}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn later_structured_message_preserves_quota_semantics() {
    assert_class(
        429,
        r#"{"error":{"code":"429","details":[{"message":"relay unavailable"},{"message":"quota exceeded"}]}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn later_structured_message_preserves_rejection_and_capacity_semantics() {
    assert_class(
        429,
        r#"{"error":{"code":"429","details":[{"message":"relay unavailable"},{"message":"request rejected"}]}}"#,
        FailureClass::RequestRejected,
    );
    assert_class(
        429,
        r#"{"error":{"code":"429","details":[{"message":"relay unavailable"},{"message":"concurrency limit exceeded"}]}}"#,
        FailureClass::CapacityUnavailable,
    );
}

#[test]
fn explicit_later_rejection_wins_over_wrapper_busy_message() {
    assert_class(
        429,
        r#"{"error":{"code":"429","details":[{"message":"server is busy"},{"message":"request rejected"}]}}"#,
        FailureClass::RequestRejected,
    );
    assert_class(
        429,
        r#"{"error":{"code":"429","details":[{"message":"server is busy"},{"message":"quota exceeded"}]}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn explicit_rejection_wins_over_busy_wrapper_for_non_429_responses() {
    assert_class(
        400,
        r#"{"error":{"details":[{"message":"server is busy"},{"message":"request rejected"}]}}"#,
        FailureClass::RequestRejected,
    );
}

#[test]
fn explicit_credential_message_does_not_enter_account_recovery() {
    assert_class(
        429,
        r#"{"error":{"code":"429","message":"invalid API key"}}"#,
        FailureClass::Credentials,
    );
}

#[test]
fn chinese_rate_limit_bodies_classify_as_rate_limited() {
    assert_class(
        429,
        r#"{"error":{"message":"您当前使用该API的调用频率过高，请稍后重试"}}"#,
        FailureClass::RateLimited,
    );
    // Relay wrappers sometimes hide the 429 behind another status; the
    // Chinese rate-limit phrasing still identifies the failure.
    assert_class(
        400,
        r#"{"error":{"message":"触发限流策略，请稍后重试"}}"#,
        FailureClass::RateLimited,
    );
}

#[test]
fn retry_after_is_preserved_without_legacy_clipping() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "86400".parse().unwrap());
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status: 503,
        headers: &headers,
        body: None,
        target_model: Some("glm-5.2"),
    });
    assert_eq!(
        classified.retry_after,
        Some(std::time::Duration::from_secs(86400))
    );
}

#[test]
fn retry_after_http_date_preserves_future_subsecond_deadline() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-27T12:00:00.250Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let future = chrono::DateTime::parse_from_rfc3339("2026-07-27T12:00:01Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    assert_eq!(
        retry_after_deadline_duration(future, now),
        Some(std::time::Duration::from_millis(750))
    );
}

#[test]
fn test_429_is_rate_limited() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(429, &headers, None);
    assert_eq!(classification, UpstreamFeedbackClassification::RateLimited);
}

#[test]
fn test_429_with_concurrency_body_is_concurrency_full() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(
        429,
        &headers,
        Some(r#"{"error": {"message": "concurrency limit exceeded"}}"#),
    );
    assert_eq!(
        classification,
        UpstreamFeedbackClassification::ConcurrencyFull
    );
}

#[test]
fn test_429_with_chinese_concurrency_body_is_concurrency_full() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(
        429,
        &headers,
        Some(
            r#"{"error": {"code": "1302", "message": "您当前使用该API的并发数过高，请降低并发"}}"#,
        ),
    );
    assert_eq!(
        classification,
        UpstreamFeedbackClassification::ConcurrencyFull
    );
}

#[test]
fn test_retry_after_indicates_temporary() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", "60".parse().unwrap());
    let classification = UpstreamFeedbackClassification::from_response(503, &headers, None);
    assert_eq!(
        classification,
        UpstreamFeedbackClassification::TemporaryUnavailable
    );
}

#[test]
fn test_5xx_is_temporary() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(503, &headers, None);
    assert_eq!(
        classification,
        UpstreamFeedbackClassification::TemporaryUnavailable
    );
}

#[test]
fn test_404_is_protocol_unsupported() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(404, &headers, None);
    assert_eq!(
        classification,
        UpstreamFeedbackClassification::ProtocolUnsupported
    );
}

#[test]
fn test_model_not_supported_is_protocol_unsupported() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(
        400,
        &headers,
        Some(r#"{"error": {"message": "model not supported"}}"#),
    );
    assert_eq!(
        classification,
        UpstreamFeedbackClassification::ProtocolUnsupported
    );
}

#[test]
fn test_model_is_not_supported_is_protocol_unsupported() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(
        400,
        &headers,
        Some(
            r#"{"error": {"message": "The 'glm-5.2' model is not supported when using Codex with a ChatGPT account."}}"#,
        ),
    );
    assert_eq!(
        classification,
        UpstreamFeedbackClassification::ProtocolUnsupported
    );
}

#[test]
fn test_generic_400_is_unknown() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(400, &headers, None);
    assert_eq!(classification, UpstreamFeedbackClassification::Unknown);
}

#[test]
fn test_body_with_rate_limit_text() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(
        400,
        &headers,
        Some(r#"{"error": "rate limit exceeded"}"#),
    );
    assert_eq!(classification, UpstreamFeedbackClassification::RateLimited);
}

#[test]
fn test_body_with_busy_text() {
    let headers = reqwest::header::HeaderMap::new();
    let classification = UpstreamFeedbackClassification::from_response(
        400,
        &headers,
        Some(r#"{"error": "server is busy"}"#),
    );
    assert_eq!(classification, UpstreamFeedbackClassification::ProviderBusy);
}

#[test]
fn html_502_is_edge_proxy_error_with_short_cooldown_class() {
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status: 502,
        headers: &reqwest::header::HeaderMap::new(),
        body: Some(
            "<html>\r\n<head><title>502 Bad Gateway</title></head>\r\n<body>\r\n<center><h1>502 Bad Gateway</h1></center>\r\n</body>\r\n</html>",
        ),
        target_model: Some("glm-5.2"),
    });
    assert_eq!(classified.class, FailureClass::EdgeProxyError);
    assert_eq!(
        classified.semantic,
        UpstreamResponseSemantic::EdgeProxyError
    );
}

#[test]
fn empty_503_is_edge_proxy_error() {
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status: 503,
        headers: &reqwest::header::HeaderMap::new(),
        body: None,
        target_model: Some("glm-5.2"),
    });
    assert_eq!(classified.class, FailureClass::EdgeProxyError);
    assert_eq!(
        classified.semantic,
        UpstreamResponseSemantic::EdgeProxyError
    );
}

#[test]
fn json_502_with_request_semantics_is_request_rejected() {
    assert_class(
        502,
        r#"{"error":{"message":"invalid parameter: stream_options"}}"#,
        FailureClass::RequestRejected,
    );
}

#[test]
fn json_500_internal_server_error_stays_transient_server() {
    assert_class(
        500,
        r#"{"error":{"message":"internal server error"}}"#,
        FailureClass::TransientServer,
    );
}

#[test]
fn json_503_service_busy_stays_transient_server() {
    assert_class(
        503,
        r#"{"error":{"message":"server busy"}}"#,
        FailureClass::TransientServer,
    );
}

// ---- E1: upstream error-code token survives classification ----

#[test]
fn string_error_code_token_survives_classification() {
    let headers = reqwest::header::HeaderMap::new();
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status: 502,
        headers: &headers,
        body: Some(r#"{"error":{"code":"channel_not_found","message":"channel gone"}}"#),
        target_model: Some("glm-5.2"),
    });
    assert_eq!(
        classified.upstream_error_code.as_deref(),
        Some("channel_not_found"),
        "string code must survive as a sanitized token"
    );
    // classification semantics unchanged
    assert_eq!(classified.class, FailureClass::TransientServer);
    assert_eq!(classified.upstream_status, Some(502));
}

#[test]
fn numeric_error_code_stays_out_of_token_but_preserves_status() {
    let headers = reqwest::header::HeaderMap::new();
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status: 429,
        headers: &headers,
        body: Some(r#"{"error":{"code":429,"message":"rate limited"}}"#),
        target_model: Some("glm-5.2"),
    });
    assert_eq!(
        classified.upstream_error_code, None,
        "pure-numeric codes must not become client tokens"
    );
    assert_eq!(classified.upstream_status, Some(429));
    assert_eq!(classified.class, FailureClass::RateLimited);
}

#[test]
fn sanitize_upstream_error_token_rejects_whitespace_and_foreign_chars() {
    assert_eq!(
        sanitize_upstream_error_token("  channel_not_found  "),
        Some("channel_not_found".to_string())
    );
    assert_eq!(
        sanitize_upstream_error_token("insufficient_user_quota"),
        Some("insufficient_user_quota".to_string())
    );
    // space inside the token => drop
    assert_eq!(sanitize_upstream_error_token("channel not found"), None);
    // foreign characters => drop
    assert_eq!(sanitize_upstream_error_token("渠道不可用"), None);
    assert_eq!(sanitize_upstream_error_token("code$with#symbols"), None);
    assert_eq!(
        sanitize_upstream_error_token("UPPER_case"),
        Some("upper_case".to_string())
    );
}

#[test]
fn sanitize_upstream_error_token_drops_overlong_and_empty() {
    let long = "x".repeat(65);
    assert_eq!(sanitize_upstream_error_token(&long), None);
    assert_eq!(
        sanitize_upstream_error_token(&"x".repeat(64)).map(|token| token.len()),
        Some(64)
    );
    assert_eq!(sanitize_upstream_error_token(""), None);
    assert_eq!(sanitize_upstream_error_token("   "), None);
}

#[test]
fn sanitized_token_preserves_hyphen_and_colon_whitelist() {
    assert_eq!(
        sanitize_upstream_error_token("model-not-found:gpt-4"),
        Some("model-not-found:gpt-4".to_string())
    );
}

#[test]
fn sanitize_body_excerpt_strips_secret_shapes_and_collapses_whitespace() {
    // `sk-` key material and `Bearer` tokens must never reach the client.
    let raw = r#"{"error":{"message":"bad request sk-live-abcdefghijklmnopqrst"}}"#;
    let excerpt = sanitize_upstream_body_excerpt(raw, 200).unwrap();
    assert!(
        !excerpt.contains("sk-live"),
        "key material must be redacted"
    );
    assert!(excerpt.contains("[redacted]"));
    assert!(excerpt.contains("bad request"));

    let raw = "authentication failed: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0";
    let excerpt = sanitize_upstream_body_excerpt(raw, 200).unwrap();
    assert!(
        !excerpt.contains("eyJhbGci"),
        "bearer token must be redacted"
    );
    assert!(excerpt.contains("[redacted]"));
}

#[test]
fn sanitize_body_excerpt_redacts_json_secret_pairs() {
    let raw = r#"{"error":{"secret_key":"s3cr3t-value-123","message":"quota exceeded"}}"#;
    let excerpt = sanitize_upstream_body_excerpt(raw, 200).unwrap();
    assert!(
        !excerpt.contains("s3cr3t-value-123"),
        "json secret pair value must be redacted"
    );
    assert!(excerpt.contains("\"secret_key\":\"[redacted]\""));
    assert!(excerpt.contains("quota exceeded"));
}

#[test]
fn sanitize_body_excerpt_truncates_to_max_chars_with_ellipsis() {
    let raw = "x".repeat(500);
    let excerpt = sanitize_upstream_body_excerpt(&raw, 200).unwrap();
    assert_eq!(
        excerpt.chars().count(),
        201,
        "200 chars + trailing ellipsis"
    );
    assert!(excerpt.ends_with('\u{2026}'));
    // No truncation when within the bound.
    let short = "short error text";
    assert_eq!(sanitize_upstream_body_excerpt(short, 200).unwrap(), short);
}

#[test]
fn sanitize_body_excerpt_returns_none_for_empty_or_whitespace_body() {
    assert_eq!(sanitize_upstream_body_excerpt("", 200), None);
    assert_eq!(sanitize_upstream_body_excerpt("   \n\t  ", 200), None);
    assert_eq!(sanitize_upstream_body_excerpt("", 0), None);
    // A body that is *only* secret material still produces a redacted marker.
    let raw = "sk-live-abcdefghijklmnopqrst";
    let excerpt = sanitize_upstream_body_excerpt(raw, 200).unwrap();
    assert!(!excerpt.contains("sk-live"));
}

#[test]
fn sanitize_body_excerpt_passes_through_plain_text_unchanged() {
    let raw = "upstream channel temporarily unavailable, please retry later";
    assert_eq!(sanitize_upstream_body_excerpt(raw, 200).unwrap(), raw);
}
