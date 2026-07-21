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
