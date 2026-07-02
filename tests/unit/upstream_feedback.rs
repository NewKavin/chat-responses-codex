use super::*;

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
