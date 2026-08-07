use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{
    unix_seconds, AppConfig, AppState, DownstreamAdmissionRejection, DownstreamConfig,
    PersistedState, UsageLog,
};
use tempfile::tempdir;

#[tokio::test]
async fn downstream_token_quota_blocks_when_daily_budget_is_exhausted() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Token".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,

                rate_limit_enabled: true,

                max_concurrency: 10,
                daily_token_limit: Some(10),
                monthly_token_limit: Some(20),
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "token".into(),
            }]),
            usage_logs: vec![UsageLog {
                id: "log-1".into(),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "REQ-1".into(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 4,
                completion_tokens: 6,
                total_tokens: 10,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 12,
                created_at: now,
                compatibility: None,
            }],
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let admission = state.reserve_downstream_request(&downstream).await;

    let rejection = admission.expect_err("daily token quota should reject exhausted keys");
    assert!(
        matches!(
            rejection,
            DownstreamAdmissionRejection::DailyTokenQuotaExceeded {
                limit: 10,
                used: 10,
                ..
            }
        ),
        "unexpected admission rejection: {rejection:?}"
    );
}

#[tokio::test]
async fn downstream_request_rollback_is_exact_and_idempotent() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let downstream = DownstreamConfig {
        id: "down-exact-rollback".into(),
        name: "Exact rollback".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
        rate_limit_enabled: true,
        per_minute_limit: 2,
        max_concurrency: 1,
        daily_token_limit: None,
        monthly_token_limit: None,
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: None,
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
        billing_mode: "request".into(),
    };

    let first = state.reserve_downstream_request(&downstream).await.unwrap();
    let second = state.reserve_downstream_request(&downstream).await.unwrap();

    state
        .rollback_downstream_request_reservation(first.clone())
        .await
        .unwrap();
    state
        .rollback_downstream_request_reservation(first)
        .await
        .unwrap();

    state.reserve_downstream_request(&downstream).await.unwrap();
    let rejection = state
        .reserve_downstream_request(&downstream)
        .await
        .expect_err("the second and third reservations must still consume the limit");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::PerMinuteLimitExceeded {
            limit: 2,
            used: 2,
            ..
        }
    ));

    state
        .rollback_downstream_request_reservation(second)
        .await
        .unwrap();
}

#[tokio::test]
async fn downstream_concurrency_release_is_idempotent_across_clones() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let downstream = DownstreamConfig {
        id: "down-idempotent-release".into(),
        name: "Idempotent release".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
        rate_limit_enabled: true,
        per_minute_limit: 60,
        max_concurrency: 1,
        daily_token_limit: None,
        monthly_token_limit: None,
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: None,
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
        billing_mode: "request".into(),
    };

    let lease = state
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    assert!(
        state
            .try_reserve_downstream_concurrency(&downstream)
            .await
            .is_err(),
        "the first lease must consume all concurrency"
    );

    state
        .release_downstream_concurrency(lease.clone())
        .await
        .unwrap();
    state.release_downstream_concurrency(lease).await.unwrap();

    state
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    assert!(
        state
            .try_reserve_downstream_concurrency(&downstream)
            .await
            .is_err(),
        "releasing a clone twice must not free the replacement lease"
    );
}

#[tokio::test]
async fn stale_downstream_lease_does_not_release_recreated_capacity() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let downstream = DownstreamConfig {
        id: "down-stale-release".into(),
        name: "Stale release".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
        rate_limit_enabled: true,
        per_minute_limit: 60,
        max_concurrency: 1,
        daily_token_limit: None,
        monthly_token_limit: None,
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: None,
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
        billing_mode: "request".into(),
    };

    let stale = state
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    state
        .clear_downstream_runtime(&downstream.id)
        .await
        .unwrap();
    let replacement = state
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();

    state.release_downstream_concurrency(stale).await.unwrap();
    assert!(matches!(
        state.try_reserve_downstream_concurrency(&downstream).await,
        Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded { .. })
    ));

    state
        .release_downstream_concurrency(replacement)
        .await
        .unwrap();
}

#[tokio::test]
async fn request_quota_usage_remaining_calculation() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Token".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: Some(1),
                request_quota_requests: Some(100),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            }]),
            usage_logs: (0..30)
                .map(|i| UsageLog {
                    id: format!("log-{}", i),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    downstream_name: None,
                    upstream_name: None,
                    endpoint: "/v1/chat/completions".into(),
                    model: "gpt-4.1-mini".into(),
                    inference_strength: None,
                    billing_mode: None,
                    request_count: None,
                    user_agent: None,
                    request_id: format!("REQ-{}", i),
                    status_code: 200,
                    wire_status_code: 0,
                    stream_diagnostics: None,
                    error_message: None,
                    error_category: None,
                    prompt_tokens: 10,
                    completion_tokens: 10,
                    total_tokens: 20,
                    total_cost_cents: None,
                    first_token_latency_ms: None,
                    latency_ms: 100,
                    created_at: now,
                    compatibility: None,
                })
                .collect(),
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let quota_usage = state.compute_request_quota_usage(&downstream).await;

    assert!(quota_usage.is_some(), "quota usage should be returned");
    let usage = quota_usage.unwrap();

    assert_eq!(usage.limit, 100, "limit should be 100");
    assert_eq!(usage.used, 30, "used should be 30");
    assert_eq!(usage.remaining, 70, "remaining should be 70 (100 - 30)");
}

#[tokio::test]
async fn request_quota_usage_remaining_when_exhausted() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Token".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: Some(1),
                request_quota_requests: Some(10),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            }]),
            usage_logs: (0..15)
                .map(|i| UsageLog {
                    id: format!("log-{}", i),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    downstream_name: None,
                    upstream_name: None,
                    endpoint: "/v1/chat/completions".into(),
                    model: "gpt-4.1-mini".into(),
                    inference_strength: None,
                    billing_mode: None,
                    request_count: None,
                    user_agent: None,
                    request_id: format!("REQ-{}", i),
                    status_code: 200,
                    wire_status_code: 0,
                    stream_diagnostics: None,
                    error_message: None,
                    error_category: None,
                    prompt_tokens: 10,
                    completion_tokens: 10,
                    total_tokens: 20,
                    total_cost_cents: None,
                    first_token_latency_ms: None,
                    latency_ms: 100,
                    created_at: now,
                    compatibility: None,
                })
                .collect(),
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let quota_usage = state.compute_request_quota_usage(&downstream).await;

    let usage = quota_usage.unwrap();

    assert_eq!(usage.limit, 10);
    assert_eq!(usage.used, 15);
    assert_eq!(
        usage.remaining, 0,
        "remaining should be 0 when used exceeds limit (saturating_sub)"
    );
}

#[tokio::test]
async fn downstream_request_mode_ignores_token_limits() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-request".into(),
                name: "Request Mode".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: Some(10),
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            }]),
            usage_logs: vec![UsageLog {
                id: "log-1".into(),
                downstream_key_id: "down-request".into(),
                upstream_key_id: "up-1".into(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "REQ-1".into(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 4,
                completion_tokens: 6,
                total_tokens: 10,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 12,
                created_at: now,
                compatibility: None,
            }],
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let admission = state.reserve_downstream_request(&downstream).await;

    assert!(
        admission.is_ok(),
        "request billing mode must ignore token limits even when daily_token_limit is set"
    );
}

#[tokio::test]
async fn downstream_token_mode_ignores_request_window_quota() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-token".into(),
                name: "Token Mode".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: Some(10_000),
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: Some(5),
                request_quota_requests: Some(1),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "token".into(),
            }]),
            // Two requests already sit inside the request window; token mode must
            // not apply the request-window quota (limit is 1).
            usage_logs: (0..2)
                .map(|index| UsageLog {
                    id: format!("log-{index}"),
                    downstream_key_id: "down-token".into(),
                    upstream_key_id: "up-1".into(),
                    downstream_name: None,
                    upstream_name: None,
                    endpoint: "/v1/chat/completions".into(),
                    model: "gpt-4.1-mini".into(),
                    inference_strength: None,
                    billing_mode: None,
                    request_count: None,
                    user_agent: None,
                    request_id: format!("REQ-{index}"),
                    status_code: 200,
                    wire_status_code: 0,
                    stream_diagnostics: None,
                    error_message: None,
                    error_category: None,
                    prompt_tokens: 4,
                    completion_tokens: 6,
                    total_tokens: 10,
                    total_cost_cents: None,
                    first_token_latency_ms: None,
                    latency_ms: 12,
                    created_at: now,
                    compatibility: None,
                })
                .collect(),
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let admission = state.reserve_downstream_request(&downstream).await;

    assert!(
        admission.is_ok(),
        "token billing mode must ignore the request-window quota"
    );
}

#[tokio::test]
async fn downstream_token_daily_window_slides_after_24h() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-slide".into(),
                name: "Sliding Window".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: Some(10),
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "token".into(),
            }]),
            // Consumption 25h ago has slid out of the 24h rolling window.
            usage_logs: vec![UsageLog {
                id: "log-old".into(),
                downstream_key_id: "down-slide".into(),
                upstream_key_id: "up-1".into(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "REQ-OLD".into(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 4,
                completion_tokens: 6,
                total_tokens: 10,
                total_cost_cents: None,
                first_token_latency_ms: None,
                latency_ms: 12,
                created_at: now.saturating_sub(25 * 3600),
                compatibility: None,
            }],
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let admission = state.reserve_downstream_request(&downstream).await;

    assert!(
        admission.is_ok(),
        "consumption older than 24h must slide out of the rolling daily window"
    );
}

#[tokio::test]
async fn downstream_billing_mode_defaults_to_request_when_absent() {
    // Legacy configs without a billing_mode field must deserialize as "request".
    let json = serde_json::json!({
        "id": "legacy-1",
        "name": "Legacy",
        "hash": "h",
        "rate_limit_enabled": true,
        "per_minute_limit": 60,
        "max_concurrency": 10,
        "active": true
    });
    let downstream: DownstreamConfig = serde_json::from_value(json).unwrap();
    assert_eq!(downstream.billing_mode(), "request");
    assert!(!downstream.token_billing_mode());
}
