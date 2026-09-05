use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{
    unix_seconds, AppConfig, AppState, DownstreamAdmissionRejection, DownstreamConfig,
    ModelConcurrencyGroup, PersistedState, UsageLog,
};
use tempfile::tempdir;

#[tokio::test]
async fn downstream_legacy_token_limit_is_no_longer_enforced() {
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
            model_group_id: None,
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

                model_concurrency_groups: vec![],
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
            runtime_settings: None,
            model_aliases: vec![],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let admission = state.reserve_downstream_request(&downstream).await;

    assert!(
        admission.is_ok(),
        "raw daily token limits are deprecated and must not reject requests"
    );
}

#[tokio::test]
async fn downstream_cost_quota_rejects_with_cost_variant_when_daily_cost_exhausted() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-cost".into(),
                name: "Team Cost".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
            model_group_id: None,
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                // $10 per 1M input tokens: 100 input tokens == 10 cents.
                input_token_price_per_million_cents: Some(1_000_000),
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: Some(10),
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "token".into(),

                model_concurrency_groups: vec![],
            }]),
            usage_logs: vec![UsageLog {
                id: "log-cost-1".into(),
                downstream_key_id: "down-cost".into(),
                upstream_key_id: "up-1".into(),
                downstream_name: None,
                upstream_name: None,
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                inference_strength: None,
                billing_mode: None,
                request_count: None,
                user_agent: None,
                request_id: "REQ-COST-1".into(),
                status_code: 200,
                wire_status_code: 0,
                stream_diagnostics: None,
                error_message: None,
                error_category: None,
                prompt_tokens: 100,
                completion_tokens: 0,
                total_tokens: 100,
                total_cost_cents: Some(10),
                first_token_latency_ms: None,
                latency_ms: 12,
                created_at: now,
                compatibility: None,
            }],
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let admission = state.reserve_downstream_request(&downstream).await;

    let rejection =
        admission.expect_err("daily cost quota should reject exhausted keys as a cost variant");
    assert!(
        matches!(
            rejection,
            DownstreamAdmissionRejection::DailyCostQuotaExceeded {
                limit: 10,
                used: 10,
                ..
            }
        ),
        "cost-mode exhaustion must map to the cost variant, got {rejection:?}"
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
            model_group_id: None,
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

        model_concurrency_groups: vec![],
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
            model_group_id: None,
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

        model_concurrency_groups: vec![],
    };

    let lease = state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    assert!(
        state
            .try_reserve_downstream_concurrency(&downstream, "test-model")
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
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    assert!(
        state
            .try_reserve_downstream_concurrency(&downstream, "test-model")
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
            model_group_id: None,
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

        model_concurrency_groups: vec![],
    };

    let stale = state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    state
        .clear_downstream_runtime(&downstream.id, &[])
        .await
        .unwrap();
    let replacement = state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();

    state.release_downstream_concurrency(stale).await.unwrap();
    assert!(matches!(
        state
            .try_reserve_downstream_concurrency(&downstream, "test-model")
            .await,
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
            model_group_id: None,
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

                model_concurrency_groups: vec![],
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
            runtime_settings: None,
            model_aliases: vec![],
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
            model_group_id: None,
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

                model_concurrency_groups: vec![],
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
            runtime_settings: None,
            model_aliases: vec![],
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
            model_group_id: None,
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

                model_concurrency_groups: vec![],
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
            runtime_settings: None,
            model_aliases: vec![],
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
            model_group_id: None,
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

                model_concurrency_groups: vec![],
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
            runtime_settings: None,
            model_aliases: vec![],
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
async fn downstream_cost_daily_window_slides_after_24h() {
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
            model_group_id: None,
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: Some(1_000_000),
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: Some(10),
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "token".into(),

                model_concurrency_groups: vec![],
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
                total_cost_cents: Some(10),
                first_token_latency_ms: None,
                latency_ms: 12,
                created_at: now.saturating_sub(25 * 3600),
                compatibility: None,
            }],
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
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
async fn downstream_request_window_replay_excludes_rejected_and_rolled_back_logs() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    // Window is rebuilt from usage logs at startup. Rejected (429) and
    // rolled-back (5xx) requests must not consume quota slots; admitted
    // client errors (400) keep theirs, matching the live window.
    let logs: Vec<UsageLog> = [
        (200, "admitted"),
        (429, "rejected"),
        (503, "rolled-back"),
        (400, "client-error"),
    ]
    .iter()
    .enumerate()
    .map(|(index, (status, suffix))| UsageLog {
        id: format!("log-{index}"),
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
        request_id: format!("REQ-{suffix}"),
        status_code: *status,
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
    .collect();

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Replay".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
            model_group_id: None,
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: Some(1),
                request_quota_requests: Some(2),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),

                model_concurrency_groups: vec![],
            }]),
            usage_logs: logs,
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();

    // 2 admitted slots remain in the window after filtering (200 + 400).
    let usage = state
        .compute_request_quota_usage(&downstream)
        .await
        .unwrap();
    assert_eq!(
        usage.used, 2,
        "rejected/rolled-back logs must not count as used"
    );

    // Window full (2/2): a new request is rejected.
    let admission = state.reserve_downstream_request(&downstream).await;
    assert!(
        matches!(
            admission,
            Err(DownstreamAdmissionRejection::RequestQuotaExceeded {
                limit: 2,
                used: 2,
                ..
            })
        ),
        "replayed admitted logs must fill the request window, got {admission:?}"
    );
}

#[tokio::test]
async fn downstream_request_window_replay_ignores_collapsed_duplicates() {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let now = unix_seconds();

    // The same log id (same request) may appear in both usage_logs and
    // archived logs after a snapshot: replay must dedupe by id.
    let mut logs: Vec<UsageLog> = (0..3)
        .map(|index| UsageLog {
            id: format!("log-{index}"),
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
        .collect();
    logs.push(logs[0].clone());

    let state = AppState::new(
        PersistedState {
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Replay Dedupe".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![],
            model_group_id: None,
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

                model_concurrency_groups: vec![],
            }]),
            usage_logs: logs,
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );

    let downstream = state.snapshot().await.downstreams[0].clone();
    let usage = state
        .compute_request_quota_usage(&downstream)
        .await
        .unwrap();
    assert_eq!(
        usage.used, 3,
        "duplicate log ids must be collapsed on replay"
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

#[tokio::test]
async fn downstream_admission_rolls_back_request_when_concurrency_exhausted() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let downstream = DownstreamConfig {
        id: "down-admission-atomic".into(),
        name: "Atomic admission".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
            model_group_id: None,
        rate_limit_enabled: true,
        per_minute_limit: 100,
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

        model_concurrency_groups: vec![],
    };

    let (first_reservation, first_lease) = state
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect("first admission must succeed");
    assert!(
        first_lease.lease_id().is_some(),
        "rate-limited admission must hold a concurrency lease"
    );

    let rejection = state
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect_err("second admission must be rejected while the lease is held");
    assert!(
        matches!(
            rejection,
            DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit: 1, .. }
        ),
        "unexpected rejection: {rejection:?}"
    );

    state
        .release_downstream_concurrency(first_lease)
        .await
        .unwrap();

    // The rejected admission must not have consumed a request-quota slot:
    // after the lease is released, a fresh admission succeeds even though the
    // per-minute counter saw two attempts.
    let (second_reservation, second_lease) = state
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect("rollback of the rejected admission must free the request slot");
    state
        .rollback_downstream_request_reservation(second_reservation)
        .await
        .unwrap();
    state
        .release_downstream_concurrency(second_lease)
        .await
        .unwrap();
    state
        .rollback_downstream_request_reservation(first_reservation)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// C7: per-model downstream concurrency groups
// ---------------------------------------------------------------------------

fn c7_downstream(id: &str, max_concurrency: u32) -> DownstreamConfig {
    DownstreamConfig {
        id: id.into(),
        name: "C7 downstream".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
            model_group_id: None,
        rate_limit_enabled: true,
        per_minute_limit: 1000,
        max_concurrency,
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
        model_concurrency_groups: vec![],
    }
}

fn c7_group(name: &str, patterns: &[&str], max_concurrency: u32) -> ModelConcurrencyGroup {
    ModelConcurrencyGroup {
        name: name.into(),
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        max_concurrency,
    }
}

/// C7 head-of-line blocking regression: a small-capacity model group that is
/// full must NOT block a large-capacity model group on the same downstream
/// key. Before C7 this test fails because the gate counted only
/// `(downstream_id)` and the glm leases ate the whole budget.
#[tokio::test]
async fn c7_full_glm_group_does_not_block_deepseek_into_its_own_group() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut downstream = c7_downstream("down-c7-hol", 32);
    downstream.model_concurrency_groups = vec![
        c7_group("glm", &["glm-5.1", "glm-5.2"], 2),
        c7_group("deepseek", &["deepseek-*"], 4),
    ];

    // Fill the glm group to its cap (2).
    let mut glm_leases = Vec::new();
    for _ in 0..2 {
        let lease = state
            .try_reserve_downstream_concurrency(&downstream, "glm-5.2")
            .await
            .expect("glm lease within its own group cap must succeed");
        glm_leases.push(lease);
    }
    // A third glm request must be rejected by the glm group cap.
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.1")
        .await
        .expect_err("glm group cap must reject the third glm request");
    let (limit, group) = match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit, group, .. } => {
            (limit, group)
        }
        other => panic!("unexpected rejection: {other:?}"),
    };
    assert_eq!(limit, 2);
    assert_eq!(
        group.as_deref(),
        Some("glm"),
        "rejection must name the glm group"
    );

    // deepseek must still be able to take its own 4 slots: the glm burst must
    // not block it (global backstop is 32, far from full).
    for _ in 0..4 {
        // spell-checker:disable-next-line
        state
            .try_reserve_downstream_concurrency(&downstream, "deepseek-v4-flash-0731")
            .await
            .expect("deepseek lease must not be blocked by the full glm group");
    }

    // But the global backstop still bounds the total: one more deepseek is now
    // over the group cap and must be rejected (naming deepseek).
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4")
        .await
        .expect_err("deepseek group cap must reject the fifth deepseek request");
    let (limit, group) = match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit, group, .. } => {
            (limit, group)
        }
        other => panic!("unexpected rejection: {other:?}"),
    };
    assert_eq!(limit, 4);
    assert_eq!(group.as_deref(), Some("deepseek"));

    for lease in glm_leases {
        state.release_downstream_concurrency(lease).await.unwrap();
    }
}

/// Empty `model_concurrency_groups` must keep the byte-identical legacy
/// behaviour: the gate counts only `(downstream_id, "")` and the global
/// `max_concurrency` is the only bound.
#[tokio::test]
async fn c7_empty_groups_keep_legacy_single_budget_behaviour() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let downstream = c7_downstream("down-c7-empty", 1);

    let first = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.2")
        .await
        .expect("first lease must succeed");
    assert_eq!(
        first.group_name(),
        "",
        "no group matched => legacy empty bucket"
    );
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4")
        .await
        .expect_err("no groups => single global budget must reject the second lease");
    match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
            limit: 1,
            group: None,
            ..
        } => {}
        other => panic!("unexpected rejection: {other:?}"),
    }
    state.release_downstream_concurrency(first).await.unwrap();
    state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4")
        .await
        .expect("release must free the only slot");
}

/// Group caps that sum above the global `max_concurrency` are legal overbooking;
/// the global backstop must still bound the total across groups.
#[tokio::test]
async fn c7_global_backstop_bounds_groups_that_sum_above_global() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut downstream = c7_downstream("down-c7-backstop", 3);
    downstream.model_concurrency_groups = vec![
        c7_group("glm", &["glm-*"], 2),
        c7_group("deepseek", &["deepseek-*"], 2),
    ];

    // Fill glm (2) and take 1 deepseek: total 3 = global backstop.
    let glm_lease = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.2")
        .await
        .unwrap();
    let glm_lease2 = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.1")
        .await
        .unwrap();
    let ds_lease = state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4")
        .await
        .unwrap();

    // The second deepseek is over its own group cap (2) AND the global cap (3).
    // Both rejections are valid; we assert the strictest signal: the global
    // backstop must reject before the group cap would admit.
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4-flash-0731")
        .await
        .expect_err("global backstop must bound the sum");
    match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit: 3, .. } => {}
        other => panic!("unexpected rejection: {other:?}"),
    }

    for lease in [glm_lease, glm_lease2, ds_lease] {
        state.release_downstream_concurrency(lease).await.unwrap();
    }
}

/// A model that matched no group falls back to the global `max_concurrency`
/// bucket keyed `(downstream_id, "")` — the legacy path stays live even when
/// other models are grouped, and the global backstop still bounds the sum.
#[tokio::test]
async fn c7_unmatched_model_uses_global_budget_independently_of_groups() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut downstream = c7_downstream("down-c7-unmatched", 2);
    downstream.model_concurrency_groups = vec![c7_group("glm", &["glm-*"], 100)];

    // glm group cap is huge; an unmatched model must not be admitted into the
    // glm bucket. Global cap is 2: glm takes one slot, the unmatched model a
    // second — both succeed, proving the unmatched model lives in its own
    // empty bucket and is not limited by the glm group cap.
    let a = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.2")
        .await
        .unwrap();
    assert_eq!(a.group_name(), "glm");
    let b = state
        .try_reserve_downstream_concurrency(&downstream, "some-other-model")
        .await
        .expect("unmatched model uses its own empty bucket, not the glm bucket");
    assert_eq!(
        b.group_name(),
        "",
        "unmatched model must land in the legacy empty bucket"
    );

    // The global backstop (2) is now full: a third lease is rejected even
    // though the glm group cap (100) is nowhere near full.
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "another-unknown")
        .await
        .expect_err("global backstop must reject the third lease");
    match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
            limit: 2,
            group: None,
            ..
        } => {}
        other => panic!("unexpected rejection: {other:?}"),
    }
    state.release_downstream_concurrency(a).await.unwrap();
    state.release_downstream_concurrency(b).await.unwrap();
}
