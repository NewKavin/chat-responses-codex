use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamAdmissionRejection, DownstreamConfig, ModelRequestCostConfig,
    PersistedState, RuntimeCoordinationBackend, UpstreamConfig, UsageLog,
};
use sha2::{Digest, Sha256};
use std::io;
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use uuid::Uuid;

#[test]
fn app_config_debug_redacts_credentials_and_redis_url() {
    let config = AppConfig {
        admin_password: "admin-debug-secret".into(),
        jwt_secret: "jwt-debug-secret".into(),
        redis_enabled: true,
        redis_url: "redis://redis-user:redis-debug-secret@redis.example:6379/0".into(),
        ..AppConfig::default()
    };

    let debug = format!("{config:?}");

    assert!(!debug.contains("admin-debug-secret"));
    assert!(!debug.contains("jwt-debug-secret"));
    assert!(!debug.contains("redis-user"));
    assert!(!debug.contains("redis-debug-secret"));
    assert!(debug.contains("[REDACTED]"));
}

#[tokio::test]
async fn disabled_redis_does_not_parse_or_connect() {
    let config = AppConfig {
        redis_url: "not a redis url".into(),
        ..AppConfig::default()
    };

    let backend = RuntimeCoordinationBackend::from_config(&config)
        .await
        .unwrap();

    assert!(!backend.is_redis());
}

#[tokio::test]
async fn enabled_redis_requires_a_url() {
    let config = AppConfig {
        redis_enabled: true,
        ..AppConfig::default()
    };

    let error = RuntimeCoordinationBackend::from_config(&config)
        .await
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains("redis://"));
}

#[tokio::test]
async fn enabled_redis_rejects_an_invalid_prefix_before_connecting() {
    let config = AppConfig {
        redis_enabled: true,
        redis_url: "redis://127.0.0.1:1".into(),
        redis_key_prefix: "bad prefix".into(),
        ..AppConfig::default()
    };

    let error = RuntimeCoordinationBackend::from_config(&config)
        .await
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains(&config.redis_url));
}

#[tokio::test]
async fn app_state_load_validates_enabled_redis_before_loading_state() {
    let directory = tempdir().unwrap();
    let state_path = directory.path().join("missing.json");
    let config = AppConfig {
        redis_enabled: true,
        redis_url: "redis://127.0.0.1:1".into(),
        redis_key_prefix: "bad prefix".into(),
        ..AppConfig::default()
    };

    let error = match AppState::load_from_path(&state_path, config).await {
        Ok(_) => panic!("enabled Redis configuration must be validated during state loading"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains("redis://"));
}

#[tokio::test]
async fn local_app_state_runtime_healthcheck_is_a_noop() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );

    state.runtime_coordination_healthcheck().await.unwrap();
}

fn redis_test_config() -> AppConfig {
    AppConfig {
        redis_enabled: true,
        redis_url: std::env::var("TEST_REDIS_URL").expect("TEST_REDIS_URL must be set"),
        redis_key_prefix: format!("chat2responses:test:{}", Uuid::new_v4().simple()),
        ..AppConfig::default()
    }
}

fn redis_test_downstream(id: &str) -> DownstreamConfig {
    DownstreamConfig {
        id: id.into(),
        name: "Redis test downstream".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
        rate_limit_enabled: true,
        per_minute_limit: 1,
        max_concurrency: 1,
        daily_token_limit: None,
        monthly_token_limit: None,
        request_quota_window_hours: None,
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
    }
}

fn redis_test_upstream(id: &str) -> UpstreamConfig {
    UpstreamConfig {
        id: id.into(),
        name: "Redis test upstream".into(),
        active: true,
        max_concurrency: 1,
        requests_per_minute: 100,
        request_quota_window_hours: 1,
        request_quota_requests: 100,
        model_request_costs: vec![ModelRequestCostConfig {
            slug: "model-a".into(),
            cost: 2.5,
        }],
        ..UpstreamConfig::default()
    }
}

async fn redis_test_states(config: &AppConfig) -> (AppState, AppState, tempfile::TempDir) {
    let directory = tempdir().unwrap();
    let first = AppState::load_from_path(directory.path().join("first.json"), config.clone())
        .await
        .unwrap();
    let second = AppState::load_from_path(directory.path().join("second.json"), config.clone())
        .await
        .unwrap();
    (first, second, directory)
}

fn redis_test_usage_log(id: &str, downstream_id: &str, total_tokens: u64) -> UsageLog {
    UsageLog {
        id: id.into(),
        downstream_key_id: downstream_id.into(),
        upstream_key_id: "upstream".into(),
        downstream_name: None,
        upstream_name: None,
        endpoint: "/v1/responses".into(),
        model: "model-a".into(),
        inference_strength: None,
        billing_mode: None,
        request_count: Some(1),
        user_agent: None,
        request_id: id.into(),
        status_code: 200,
        error_message: None,
        error_category: None,
        prompt_tokens: total_tokens,
        completion_tokens: 0,
        total_tokens,
        latency_ms: 1,
        created_at: 0,
        compatibility: None,
    }
}

async fn redis_test_command(config: &AppConfig, arguments: &[String]) -> String {
    let address = config
        .redis_url
        .strip_prefix("redis://")
        .expect("TEST_REDIS_URL must use redis://")
        .split('/')
        .next()
        .expect("TEST_REDIS_URL must include an address");
    assert!(
        !address.contains('@'),
        "Redis integration tests require a credential-free TEST_REDIS_URL"
    );
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut request = format!("*{}\r\n", arguments.len()).into_bytes();
    for argument in arguments {
        request.extend_from_slice(format!("${}\r\n", argument.len()).as_bytes());
        request.extend_from_slice(argument.as_bytes());
        request.extend_from_slice(b"\r\n");
    }
    stream.write_all(&request).await.unwrap();
    let mut response = vec![0_u8; 1_024];
    let length = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut response))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(response[..length].to_vec()).unwrap()
}

async fn pause_test_redis(config: &AppConfig, milliseconds: u64) {
    let response = redis_test_command(
        config,
        &[
            "CLIENT".into(),
            "PAUSE".into(),
            milliseconds.to_string(),
            "ALL".into(),
        ],
    )
    .await;
    assert_eq!(response, "+OK\r\n");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_request_reservations_are_shared_and_exact() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("shared-request-limit");

    let reservation = first.reserve_downstream_request(&downstream).await.unwrap();
    let rejection = second
        .reserve_downstream_request(&downstream)
        .await
        .expect_err("the second coordinator must observe the first reservation");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::PerMinuteLimitExceeded {
            limit: 1,
            used: 1,
            ..
        }
    ));

    first
        .rollback_downstream_request_reservation(reservation)
        .await
        .unwrap();
    second
        .reserve_downstream_request(&downstream)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_concurrency_leases_are_shared_and_idempotent() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("shared-concurrency-limit");

    let first_lease = first
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    assert!(
        second
            .try_reserve_downstream_concurrency(&downstream)
            .await
            .is_err(),
        "the second coordinator must observe the first lease"
    );

    first
        .release_downstream_concurrency(first_lease.clone())
        .await
        .unwrap();
    let second_lease = second
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    first
        .release_downstream_concurrency(first_lease)
        .await
        .unwrap();
    assert!(
        first
            .try_reserve_downstream_concurrency(&downstream)
            .await
            .is_err(),
        "releasing a stale clone must not remove the replacement lease"
    );
    second
        .release_downstream_concurrency(second_lease)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_token_usage_is_shared() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("shared-token-limit");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(10);

    first.insert_downstream(downstream.clone()).await.unwrap();

    first
        .append_usage_log(redis_test_usage_log(
            "redis-token-event",
            &downstream.id,
            10,
        ))
        .await
        .unwrap();

    let rejection = second
        .reserve_downstream_request(&downstream)
        .await
        .expect_err("the second coordinator must observe shared token usage");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::DailyTokenQuotaExceeded {
            limit: 10,
            used: 10,
            ..
        }
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_request_quota_mode_does_not_apply_token_limits() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("request-mode-only");
    downstream.per_minute_limit = 60;
    downstream.request_quota_window_hours = Some(1);
    downstream.request_quota_requests = Some(100);
    downstream.daily_token_limit = Some(1);
    downstream.monthly_token_limit = Some(1);
    first.insert_downstream(downstream.clone()).await.unwrap();

    first
        .append_usage_log(redis_test_usage_log(
            "request-mode-token-event",
            &downstream.id,
            10,
        ))
        .await
        .unwrap();

    second
        .reserve_downstream_request(&downstream)
        .await
        .expect("request quota mode must ignore stale token limit fields");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_token_retry_after_waits_until_enough_tokens_expire() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("token-retry-after");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(100);
    first.insert_downstream(downstream.clone()).await.unwrap();

    first
        .append_usage_log(redis_test_usage_log("small-old-event", &downstream.id, 1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    first
        .append_usage_log(redis_test_usage_log("large-new-event", &downstream.id, 100))
        .await
        .unwrap();

    let rejection = second
        .reserve_downstream_request(&downstream)
        .await
        .expect_err("the daily token quota must be exhausted");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::DailyTokenQuotaExceeded {
            retry_after_seconds,
            limit: 100,
            used: 101,
        } if retry_after_seconds >= 86_400
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_daily_token_keys_use_daily_retention() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("daily-token-retention");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(100);
    first.insert_downstream(downstream.clone()).await.unwrap();
    first
        .append_usage_log(redis_test_usage_log("daily-retention", &downstream.id, 1))
        .await
        .unwrap();

    let identity = format!("{:x}", Sha256::digest(downstream.id.as_bytes()));
    let token_key = format!(
        "{}:v1:downstream:{{{identity}}}:tokens",
        config.redis_key_prefix
    );
    let response = redis_test_command(&config, &["TTL".into(), token_key]).await;
    let ttl = response
        .strip_prefix(':')
        .and_then(|value| value.trim().parse::<i64>().ok())
        .expect("TTL must return an integer");
    assert!(ttl > 86_000 && ttl <= 86_460, "unexpected daily TTL: {ttl}");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_release_and_rollback_retry_once_after_timeout() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("release-retry");
    downstream.per_minute_limit = 1;

    let lease = first
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    pause_test_redis(&config, 2_100).await;
    first
        .release_downstream_concurrency(lease)
        .await
        .expect("lease release must retry once after the first timeout");
    let replacement = second
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    second
        .release_downstream_concurrency(replacement)
        .await
        .unwrap();

    let reservation = first.reserve_downstream_request(&downstream).await.unwrap();
    pause_test_redis(&config, 2_100).await;
    first
        .rollback_downstream_request_reservation(reservation)
        .await
        .expect("request rollback must retry once after the first timeout");
    second
        .reserve_downstream_request(&downstream)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn failed_redis_token_recording_does_not_queue_a_duplicate_usage_log() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("token-record-retry");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(100);
    first.insert_downstream(downstream.clone()).await.unwrap();
    let log = redis_test_usage_log("retryable-token-log", &downstream.id, 10);

    pause_test_redis(&config, 2_500).await;
    let error = first
        .append_usage_log(log.clone())
        .await
        .expect_err("token recording must fail closed while Redis is paused");
    assert_eq!(error.to_string(), "runtime coordination unavailable");
    assert!(
        first
            .snapshot()
            .await
            .usage_logs
            .iter()
            .all(|entry| entry.id != log.id),
        "a failed Redis write must not leave a pending durable log"
    );

    tokio::time::sleep(Duration::from_millis(1_500)).await;
    first.append_usage_log(log.clone()).await.unwrap();
    let matching_logs = first
        .snapshot()
        .await
        .usage_logs
        .into_iter()
        .filter(|entry| entry.id == log.id)
        .count();
    assert_eq!(matching_logs, 1);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn failed_redis_cleanup_leaves_downstream_available_for_retry() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("cleanup-retry");
    downstream.per_minute_limit = 1;
    first.insert_downstream(downstream.clone()).await.unwrap();
    first.reserve_downstream_request(&downstream).await.unwrap();

    pause_test_redis(&config, 2_500).await;
    first
        .remove_downstream(&downstream.id)
        .await
        .expect_err("cleanup must report Redis unavailability");
    assert!(
        first
            .snapshot()
            .await
            .downstreams
            .iter()
            .any(|entry| entry.id == downstream.id),
        "failed cleanup must not persist the removal"
    );

    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(first.remove_downstream(&downstream.id).await.unwrap());
    second
        .reserve_downstream_request(&downstream)
        .await
        .expect("retrying removal must clear the old request reservation");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_reservations_snapshots_and_exact_release_are_shared() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut upstream = redis_test_upstream("shared-upstream-admission");
    upstream.requests_per_minute = 2;
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();

    let first_lease = first
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let snapshots = second.upstream_runtime_snapshots().await.unwrap();
    let snapshot = snapshots.get(&upstream.id).unwrap();
    assert_eq!(snapshot.in_flight, 1);
    assert_eq!(snapshot.minute_cost, 2.5);
    assert_eq!(snapshot.five_hour_cost, 2.5);

    let concurrency_rejection = second
        .try_reserve_upstream_hedge(&upstream, "model-a")
        .await
        .expect_err("the second coordinator must observe the shared lease");
    assert!(!concurrency_rejection.is_runtime_coordination_unavailable());

    first
        .release_upstream_request(first_lease.clone())
        .await
        .unwrap();
    let snapshots = second.upstream_runtime_snapshots().await.unwrap();
    let snapshot = snapshots.get(&upstream.id).unwrap();
    assert_eq!(snapshot.in_flight, 0);
    assert_eq!(snapshot.minute_cost, 2.5);
    assert_eq!(snapshot.five_hour_cost, 2.5);

    let minute_rejection = second
        .try_reserve_upstream_hedge(&upstream, "model-a")
        .await
        .expect_err("releasing a lease must not erase its quota event");
    assert!(!minute_rejection.is_runtime_coordination_unavailable());

    upstream.requests_per_minute = 100;
    upstream.request_quota_requests = 2;
    let window_rejection = second
        .try_reserve_upstream_hedge(&upstream, "model-a")
        .await
        .expect_err("the configured request window must be shared");
    assert!(!window_rejection.is_runtime_coordination_unavailable());

    let replacement = second
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    first.release_upstream_request(first_lease).await.unwrap();
    assert_eq!(
        first
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .unwrap()
            .in_flight,
        1,
        "releasing a stale clone must not remove the replacement lease"
    );
    second.release_upstream_request(replacement).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_snapshot_round_trips_precise_fractional_costs() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut upstream = redis_test_upstream("precise-upstream-costs");
    let request_cost = 1.234_567_890_123_456_7;
    upstream.model_request_costs[0].cost = request_cost;
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();

    let first_lease = first
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let second_lease = second
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();

    let snapshot = first
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .remove(&upstream.id)
        .unwrap();
    assert_eq!(snapshot.minute_cost, request_cost + request_cost);
    assert_eq!(snapshot.five_hour_cost, request_cost + request_cost);

    first.release_upstream_request(first_lease).await.unwrap();
    second.release_upstream_request(second_lease).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_cooldown_is_shared_and_success_clears_it() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("shared-upstream-cooldown");
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();

    first
        .mark_upstream_rate_limited(&upstream.id, 30)
        .await
        .unwrap();
    let snapshots = second.upstream_runtime_snapshots().await.unwrap();
    let cooldown_until = snapshots.get(&upstream.id).unwrap().cooldown_until;
    assert!(cooldown_until > 0);

    second
        .mark_upstream_concurrency_full(&upstream.id, 1_000)
        .await
        .unwrap();
    let feedback = first
        .upstream_runtime_snapshots_with_feedback()
        .await
        .unwrap();
    let feedback = feedback.get(&upstream.id).unwrap();
    assert_eq!(feedback.cooldown_until, cooldown_until);
    assert_eq!(
        feedback.last_feedback_type.as_deref(),
        Some("concurrency_full")
    );
    assert_eq!(feedback.last_retry_after_seconds, Some(1));

    second.mark_upstream_success(&upstream.id).await.unwrap();
    let snapshots = first.upstream_runtime_snapshots().await.unwrap();
    assert_eq!(snapshots.get(&upstream.id).unwrap().cooldown_until, 0);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_admission_distinguishes_capacity_from_coordination_failure() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("upstream-coordination-failure");

    let lease = first
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let capacity = second
        .try_reserve_upstream_hedge(&upstream, "model-a")
        .await
        .expect_err("the shared concurrency limit must reject the hedge");
    assert!(!capacity.is_runtime_coordination_unavailable());
    first.release_upstream_request(lease).await.unwrap();

    pause_test_redis(&config, 2_500).await;
    let coordination = second
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .expect_err("Redis timeout must fail closed");
    assert!(coordination.is_runtime_coordination_unavailable());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_snapshot_failure_returns_stable_gateway_503() {
    let config = redis_test_config();
    let directory = tempdir().unwrap();
    let state = AppState::load_from_path(directory.path().join("gateway.json"), config.clone())
        .await
        .unwrap();
    let mut upstream = redis_test_upstream("upstream-snapshot-503");
    upstream.base_url = "http://127.0.0.1:1".into();
    upstream.api_key = "unused-upstream-key".into();
    upstream.supported_models = vec!["model-a".into()];
    state.insert_upstream(upstream).await.unwrap();

    let downstream_key = generate_downstream_key("redis-test");
    let mut downstream = redis_test_downstream("snapshot-503-downstream");
    downstream.hash = downstream_key.hash;
    downstream.model_allowlist = vec!["model-a".into()];
    downstream.rate_limit_enabled = false;
    state.insert_downstream(downstream).await.unwrap();

    pause_test_redis(&config, 2_500).await;
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {}", downstream_key.plaintext))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "model": "model-a",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("\"code\":\"runtime_coordination_unavailable\""));
}
