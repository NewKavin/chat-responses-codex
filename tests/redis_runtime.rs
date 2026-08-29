use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chat_responses_codex::capabilities::WireProtocol;
use chat_responses_codex::keys::{generate_downstream_key, upstream_key_fingerprint};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AccountConcurrencyKey, AccountProbeOutcome, ApiKeyModelConfig, AppConfig, AppState,
    CoordinationTestFault, DownstreamAdmissionRejection, DownstreamConfig, KeyHealthKey,
    ModelConcurrencyGroup, ModelKeySyncService, PersistedState, ProbeDecision, RouteAvailability,
    RouteFailureClass, RouteHealthKey, RouteOutcome, RouteSetAggregateKey,
    RuntimeCoordinationBackend, UpstreamConfig, UsageLog,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
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

fn redis_test_upstream(id: &str) -> UpstreamConfig {
    UpstreamConfig {
        id: id.into(),
        name: "Redis test upstream".into(),
        active: true,
        max_concurrency: 1,
        requests_per_minute: 100,
        request_quota_window_hours: 1,
        request_quota_requests: 100,
        ..UpstreamConfig::default()
    }
}

fn redis_test_health_key(upstream_id: &str, fingerprint: &str) -> KeyHealthKey {
    KeyHealthKey {
        upstream_id: upstream_id.into(),
        key_fingerprint: fingerprint.into(),
    }
}

fn redis_test_health_route(upstream_id: &str, fingerprint: &str, model: &str) -> RouteHealthKey {
    RouteHealthKey {
        upstream_id: upstream_id.into(),
        key_fingerprint: fingerprint.into(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    }
}

fn redis_bulk_string(response: &str) -> &str {
    response
        .split("\r\n")
        .nth(1)
        .expect("bulk Redis response must include a value")
}

fn redis_bulk_u64(response: &str) -> u64 {
    redis_bulk_string(response)
        .parse()
        .expect("bulk Redis value must be an integer")
}

fn redis_integer(response: &str) -> i64 {
    response
        .strip_prefix(':')
        .and_then(|value| value.strip_suffix("\r\n"))
        .expect("Redis response must be an integer")
        .parse()
        .expect("Redis integer response must contain a number")
}

fn redis_integer_array(response: &str) -> Vec<i64> {
    response
        .split("\r\n")
        .filter_map(|line| line.strip_prefix(':'))
        .map(|value| value.parse().expect("Redis array integer must be valid"))
        .collect()
}

async fn redis_route_health_state_key(config: &AppConfig) -> String {
    let response = redis_test_command(
        config,
        &[
            "KEYS".into(),
            format!("{}:v1:route-health:*:route:*", config.redis_key_prefix),
        ],
    )
    .await;
    let mut lines = response.split("\r\n");
    assert_eq!(lines.next(), Some("*1"));
    assert!(lines.next().is_some_and(|line| line.starts_with('$')));
    lines
        .next()
        .expect("route-health key response must contain one key")
        .to_string()
}

fn redis_route_health_key(config: &AppConfig, suffix: &str) -> String {
    format!(
        "{}:v1:route-health:{{route-health}}:{suffix}",
        config.redis_key_prefix
    )
}

fn redis_route_health_route_state_key(config: &AppConfig, route: &RouteHealthKey) -> String {
    assert_eq!(route.protocol, WireProtocol::Responses);
    let identity = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}\0{}\0{}\0responses",
                route.upstream_id, route.key_fingerprint, route.runtime_model_slug
            )
            .as_bytes()
        )
    );
    redis_route_health_key(config, &format!("route:{identity}"))
}

fn redis_account_key(config: &AppConfig, account: &AccountConcurrencyKey, suffix: &str) -> String {
    let identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    format!(
        "{}:v1:account:{{{identity}}}:{suffix}",
        config.redis_key_prefix
    )
}

fn redis_downstream_key(config: &AppConfig, downstream_id: &str, suffix: &str) -> String {
    let identity = format!("{:x}", Sha256::digest(downstream_id.as_bytes()));
    format!(
        "{}:v1:downstream:{{{identity}}}:{suffix}",
        config.redis_key_prefix
    )
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

fn coordination_fault(state: &AppState) -> std::sync::Arc<CoordinationTestFault> {
    state
        .coordination_test_fault()
        .expect("test states must use the Redis runtime backend")
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
        wire_status_code: 0,
        stream_diagnostics: None,
        error_message: None,
        error_category: None,
        prompt_tokens: total_tokens,
        completion_tokens: 0,
        total_tokens,
        total_cost_cents: None,
        first_token_latency_ms: None,
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
    let mut response = vec![0_u8; 64 * 1_024];
    let length = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut response))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(response[..length].to_vec()).unwrap()
}

#[tokio::test]
async fn stale_upstream_lease_does_not_release_recreated_capacity() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let upstream = redis_test_upstream("local-stale-upstream-release");
    state.insert_upstream(upstream.clone()).await.unwrap();

    let stale = state
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    assert!(state.remove_upstream(&upstream.id).await.unwrap());
    state.insert_upstream(upstream.clone()).await.unwrap();
    let replacement = state
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();

    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .unwrap()
            .in_flight,
        1,
        "removing an upstream must clear the old runtime generation"
    );
    state.release_upstream_request(stale).await.unwrap();
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .unwrap()
            .in_flight,
        1,
        "a stale lease must not release replacement capacity"
    );

    state.release_upstream_request(replacement).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_reserve_replays_preserve_original_scores_and_costs() {
    let config = redis_test_config();
    let request_key = format!("{}:replay:requests", config.redis_key_prefix);
    let token_key = format!("{}:replay:tokens", config.redis_key_prefix);
    let token_values_key = format!("{}:replay:token-values", config.redis_key_prefix);
    let downstream_lease_key = format!("{}:replay:downstream-leases", config.redis_key_prefix);
    let downstream_aggregate_lease_key = format!(
        "{}:replay:downstream-aggregate-leases",
        config.redis_key_prefix
    );
    let upstream_lease_key = format!("{}:replay:upstream-leases", config.redis_key_prefix);
    let upstream_aggregate_lease_key = format!(
        "{}:replay:upstream-aggregate-leases",
        config.redis_key_prefix
    );
    let upstream_event_key = format!("{}:replay:upstream-events", config.redis_key_prefix);
    let upstream_cost_key = format!("{}:replay:upstream-costs", config.redis_key_prefix);
    let upstream_counters_key = format!("{}:replay:upstream-counters", config.redis_key_prefix);
    let upstream_reclaim_markers_key = format!(
        "{}:replay:upstream-reclaim-markers",
        config.redis_key_prefix
    );

    let downstream_args = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/downstream_reserve.lua").into(),
        "3".into(),
        request_key.clone(),
        token_key,
        token_values_key,
        "event-id".into(),
        "10".into(),
        "0".into(),
        "0".into(),
        "0".into(),
        "0".into(),
    ];
    redis_test_command(&config, &downstream_args).await;
    let request_score = redis_bulk_u64(
        &redis_test_command(
            &config,
            &["ZSCORE".into(), request_key.clone(), "event-id".into()],
        )
        .await,
    );

    let lease_args = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/lease_reserve.lua").into(),
        "2".into(),
        downstream_lease_key.clone(),
        downstream_aggregate_lease_key,
        "lease-id".into(),
        "0".into(),
        "10".into(),
        "120000".into(),
    ];
    redis_test_command(&config, &lease_args).await;
    let downstream_lease_score = redis_bulk_u64(
        &redis_test_command(
            &config,
            &[
                "ZSCORE".into(),
                downstream_lease_key.clone(),
                "lease-id".into(),
            ],
        )
        .await,
    );

    let upstream_args = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/upstream_reserve.lua").into(),
        "6".into(),
        upstream_lease_key.clone(),
        upstream_aggregate_lease_key,
        upstream_event_key.clone(),
        upstream_cost_key.clone(),
        upstream_counters_key,
        upstream_reclaim_markers_key,
        "upstream-event-id".into(),
        "upstream-lease-id".into(),
        "2.5".into(),
        "0".into(),
        "10".into(),
        "100".into(),
        "3600".into(),
        "100".into(),
        "120000".into(),
        "200000".into(),
    ];
    redis_test_command(&config, &upstream_args).await;
    let upstream_lease_score = redis_bulk_u64(
        &redis_test_command(
            &config,
            &[
                "ZSCORE".into(),
                upstream_lease_key.clone(),
                "upstream-lease-id".into(),
            ],
        )
        .await,
    );
    let upstream_event_score = redis_bulk_u64(
        &redis_test_command(
            &config,
            &[
                "ZSCORE".into(),
                upstream_event_key.clone(),
                "upstream-event-id".into(),
            ],
        )
        .await,
    );
    let upstream_cost = redis_bulk_string(
        &redis_test_command(
            &config,
            &[
                "HGET".into(),
                upstream_cost_key.clone(),
                "upstream-event-id".into(),
            ],
        )
        .await,
    )
    .to_string();

    tokio::time::sleep(Duration::from_millis(5)).await;
    redis_test_command(&config, &downstream_args).await;
    redis_test_command(&config, &lease_args).await;
    redis_test_command(&config, &upstream_args).await;

    assert_eq!(
        request_score,
        redis_bulk_u64(
            &redis_test_command(&config, &["ZSCORE".into(), request_key, "event-id".into()],).await,
        )
    );
    assert_eq!(
        downstream_lease_score,
        redis_bulk_u64(
            &redis_test_command(
                &config,
                &["ZSCORE".into(), downstream_lease_key, "lease-id".into(),],
            )
            .await,
        )
    );
    assert_eq!(
        upstream_lease_score,
        redis_bulk_u64(
            &redis_test_command(
                &config,
                &[
                    "ZSCORE".into(),
                    upstream_lease_key,
                    "upstream-lease-id".into(),
                ],
            )
            .await,
        )
    );
    assert_eq!(
        upstream_event_score,
        redis_bulk_u64(
            &redis_test_command(
                &config,
                &[
                    "ZSCORE".into(),
                    upstream_event_key,
                    "upstream-event-id".into(),
                ],
            )
            .await,
        )
    );
    assert_eq!(
        upstream_cost,
        redis_bulk_string(
            &redis_test_command(
                &config,
                &["HGET".into(), upstream_cost_key, "upstream-event-id".into(),],
            )
            .await,
        )
    );
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
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    assert!(
        second
            .try_reserve_downstream_concurrency(&downstream, "test-model")
            .await
            .is_err(),
        "the second coordinator must observe the first lease"
    );

    first
        .release_downstream_concurrency(first_lease.clone())
        .await
        .unwrap();
    let second_lease = second
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    first
        .release_downstream_concurrency(first_lease)
        .await
        .unwrap();
    assert!(
        first
            .try_reserve_downstream_concurrency(&downstream, "test-model")
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
async fn redis_downstream_lease_renewal_extends_lease_ttl() {
    let config = redis_test_config();
    let (state, _directory) = {
        let (first, _second, directory) = redis_test_states(&config).await;
        (first, directory)
    };
    let downstream = redis_test_downstream("renewal-extends-lease");
    let lease = state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    let lease_id = lease.lease_id().expect("redis lease id").to_string();

    let identity = format!("{:x}", Sha256::digest(downstream.id.as_bytes()));
    let lease_key = format!(
        "{}:v1:downstream:{{{identity}}}:leases",
        config.redis_key_prefix
    );

    // Push the lease to the brink of expiry, then renew it.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    redis_test_command(
        &config,
        &[
            "ZADD".into(),
            lease_key.clone(),
            (now_ms + 1_000).to_string(),
            lease_id.clone(),
        ],
    )
    .await;
    state.renew_downstream_concurrency(&lease).await.unwrap();
    let renewed_score = redis_bulk_u64(
        &redis_test_command(
            &config,
            &["ZSCORE".into(), lease_key.clone(), lease_id.clone()],
        )
        .await,
    );
    assert!(
        renewed_score > now_ms + 60_000,
        "renewal must push the lease score at least one TTL into the future"
    );

    let counts = state
        .downstream_runtime_snapshot(&downstream)
        .await
        .unwrap();
    assert_eq!(counts.admitted, 1, "renewed lease must still be counted");

    state.release_downstream_concurrency(lease).await.unwrap();
    let counts = state
        .downstream_runtime_snapshot(&downstream)
        .await
        .unwrap();
    assert_eq!(counts.admitted, 0, "released lease must be gone");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_lease_renewal_extends_lease_ttl() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("renewal-extends-upstream-lease");
    let fingerprint = "fingerprint-upstream-renewal";
    let account = AccountConcurrencyKey::new(upstream.id.clone(), fingerprint);
    state.insert_upstream(upstream.clone()).await.unwrap();
    let lease = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let account_identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    let lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:leases",
        config.redis_key_prefix
    );
    let aggregate_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:leases",
        config.redis_key_prefix
    );
    let members = redis_test_command(
        &config,
        &["ZRANGE".into(), lease_key.clone(), "0".into(), "-1".into()],
    )
    .await;
    let lease_id = members
        .split("\r\n")
        .nth(2)
        .expect("ZRANGE must return the reserved upstream lease id")
        .to_string();

    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1_000 + micros / 1_000;
    redis_test_command(
        &config,
        &[
            "ZADD".into(),
            lease_key.clone(),
            (now_ms + 1_000).to_string(),
            lease_id.clone(),
        ],
    )
    .await;
    redis_test_command(
        &config,
        &[
            "ZADD".into(),
            aggregate_key.clone(),
            (now_ms + 1_000).to_string(),
            lease_id.clone(),
        ],
    )
    .await;

    state
        .renew_upstream_request(&lease)
        .await
        .expect("Redis upstream lease renewal must succeed");
    let renewed_score = redis_bulk_u64(
        &redis_test_command(&config, &["ZSCORE".into(), lease_key, lease_id.clone()]).await,
    );
    assert!(
        renewed_score > now_ms + 60_000,
        "renewal must push the Redis upstream lease into the future, got {renewed_score}"
    );
    let aggregate_renewed_score = redis_bulk_u64(
        &redis_test_command(
            &config,
            &["ZSCORE".into(), aggregate_key.clone(), lease_id.clone()],
        )
        .await,
    );
    assert!(
        aggregate_renewed_score > now_ms + 60_000,
        "renewal must push the aggregate Redis upstream lease into the future, got {aggregate_renewed_score}"
    );

    state.release_upstream_request(lease).await.unwrap();
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .unwrap()
            .in_flight,
        0,
        "released Redis upstream lease must no longer count as in flight"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_gateway_heartbeat_adopts_hot_updated_lease_ttl() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let release_rx = release_rx.clone();
            async move {
                let receiver = release_rx.lock().await.take();
                if let Some(receiver) = receiver {
                    let _ = receiver.await;
                }
                Json(json!({
                "id": "chatcmpl-hot-updated-ttl",
                "object": "chat.completion",
                "created": 1,
                "model": "model-a",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }]
                }))
            }
        }),
    );
    let upstream_server = tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let mut config = redis_test_config();
    config.upstream_local_lease_ttl_seconds = 120;
    config.upstream_lease_stale_after_ms = 80_000;
    config.upstream_response_header_timeout_seconds = 300;
    config.upstream_concurrency_recovery_max_wait_ms = 30_000;
    config.upstream_route_exhaustion_retry_max_wait_ms = 30_000;
    let directory = tempdir().unwrap();
    let state = AppState::load_from_path(directory.path().join("gateway.json"), config.clone())
        .await
        .unwrap();
    let api_key = "hot-updated-ttl-account";
    let upstream = UpstreamConfig {
        id: "redis-hot-updated-ttl".into(),
        name: "Redis hot-updated TTL".into(),
        base_url: format!("http://{address}"),
        api_key: api_key.into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["model-a".into()],
        max_concurrency: 1,
        active: true,
        ..UpstreamConfig::default()
    };
    state.insert_upstream(upstream.clone()).await.unwrap();

    let downstream_key = generate_downstream_key("redis-hot-updated-ttl");
    let mut downstream = redis_test_downstream("redis-hot-updated-ttl-downstream");
    downstream.hash = downstream_key.hash;
    downstream.model_allowlist = vec!["model-a".into()];
    downstream.rate_limit_enabled = false;
    downstream.max_concurrency = 10;
    state.insert_downstream(downstream).await.unwrap();

    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "model": "model-a",
                    "messages": [{"role": "user", "content": "hello"}]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let request_task = tokio::spawn(build_router(state.clone()).oneshot(request()));
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let account = AccountConcurrencyKey::new(
        upstream.id.clone(),
        upstream_key_fingerprint(&upstream.id, api_key),
    );
    let account_identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    let account_lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:leases",
        config.redis_key_prefix
    );
    let lease_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let members = redis_test_command(
                &config,
                &[
                    "ZRANGE".into(),
                    account_lease_key.clone(),
                    "0".into(),
                    "-1".into(),
                ],
            )
            .await;
            if let Some(lease_id) = members.split("\r\n").nth(2) {
                if !lease_id.is_empty() {
                    break lease_id.to_string();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the gateway request must reserve a Redis upstream lease");
    let aggregate_lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:leases",
        config.redis_key_prefix
    );

    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let near_expiry_ms = seconds * 1_000 + micros / 1_000 + 1_000;
    for key in [account_lease_key.clone(), aggregate_lease_key.clone()] {
        redis_test_command(
            &config,
            &[
                "ZADD".into(),
                key,
                near_expiry_ms.to_string(),
                lease_id.clone(),
            ],
        )
        .await;
    }

    let mut settings = state.runtime_settings().as_ref().clone();
    settings.upstream_local_lease_ttl_seconds = 60;
    settings.upstream_lease_stale_after_ms = 40_000;
    state
        .update_runtime_settings(0, settings)
        .await
        .expect("hot-updating the Redis lease TTL must succeed");

    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1_000 + micros / 1_000;
    for key in [account_lease_key.clone(), aggregate_lease_key] {
        let renewed_score = redis_bulk_u64(
            &redis_test_command(&config, &["ZSCORE".into(), key, lease_id.clone()]).await,
        );
        assert!(
            renewed_score >= now_ms + 45_000,
            "a Redis heartbeat must renew immediately after a hot TTL update, got {renewed_score}"
        );
        assert!(
            renewed_score <= now_ms + 75_000,
            "the hot-updated Redis lease must use the new 60s TTL, got {renewed_score}"
        );
    }

    release_tx.send(()).unwrap();
    let response = request_task.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    upstream_server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_queue_grants_one_fifo_probe_across_instances() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let older = first
        .register_account_waiter(&account, "req-1", "down-a", "lease-1")
        .await
        .unwrap();
    let newer = second
        .register_account_waiter(&account, "req-2", "down-a", "lease-2")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;

    let (older_result, newer_result) = tokio::join!(
        first.try_acquire_account_probe(&older),
        second.try_acquire_account_probe(&newer),
    );
    assert!(matches!(older_result.unwrap(), ProbeDecision::Granted(_)));
    assert!(matches!(newer_result.unwrap(), ProbeDecision::Wait { .. }));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_healthy_accounts_do_not_register_recovery_waiters() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-healthy");

    assert!(first
        .register_account_waiter_if_saturated(&account, "req-healthy", "down-a", "lease-healthy",)
        .await
        .unwrap()
        .is_none());

    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    assert!(first
        .register_account_waiter_if_saturated(
            &account,
            "req-saturated",
            "down-a",
            "lease-saturated",
        )
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_probe_grant_atomically_requires_and_clears_downstream_waiting() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("down-atomic-probe-grant");
    let lease = first
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    let account = AccountConcurrencyKey::new("up-atomic-probe-grant", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let ticket = first
        .register_account_waiter_for_downstream_lease_if_saturated(
            &account,
            "req-atomic-probe-grant",
            &lease,
        )
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;

    assert!(second
        .try_acquire_account_probe_for_downstream_lease(&ticket, &lease)
        .await
        .is_err());
    let before = second
        .downstream_runtime_snapshot(&downstream)
        .await
        .unwrap();
    assert_eq!((before.waiting_upstream, before.running), (0, 1));

    first.mark_downstream_waiting(&lease).await.unwrap();
    assert!(matches!(
        second
            .try_acquire_account_probe_for_downstream_lease(&ticket, &lease)
            .await
            .unwrap(),
        ProbeDecision::Granted(_)
    ));
    let after = first
        .downstream_runtime_snapshot(&downstream)
        .await
        .unwrap();
    assert_eq!((after.waiting_upstream, after.running), (0, 1));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_snapshot_counts_admitted_and_waiting_without_false_zero() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("down-runtime");
    let lease = first
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    first.mark_downstream_waiting(&lease).await.unwrap();
    let snapshot = second
        .downstream_runtime_snapshot(&downstream)
        .await
        .unwrap();
    assert_eq!(
        (
            snapshot.admitted,
            snapshot.waiting_upstream,
            snapshot.running
        ),
        (1, 1, 0)
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_admin_downstream_snapshot_failure_returns_typed_503() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    state
        .add_downstream(redis_test_downstream("down-runtime-admin-outage"))
        .await
        .unwrap();
    let app = build_router(state.clone());
    let token = chat_responses_codex::auth::generate_admin_token(
        &config.admin_username,
        &config.jwt_secret,
    )
    .unwrap();
    let fault = coordination_fault(&state);
    fault.arm_outage(true);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/downstreams/runtime")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"]["code"], "runtime_state_unavailable");
    fault.arm_outage(false);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_portal_downstream_snapshot_failure_preserves_quota() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let generated = generate_downstream_key("sk");
    let mut downstream = redis_test_downstream("down-runtime-portal-outage");
    downstream.hash = generated.hash;
    downstream.plaintext_key = Some(generated.plaintext.clone());
    downstream.request_quota_window_hours = Some(24);
    downstream.request_quota_requests = Some(1_000);
    state.add_downstream(downstream).await.unwrap();
    let app = build_router(state.clone());
    let fault = coordination_fault(&state);
    fault.arm_outage(true);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/overview")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", generated.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["quota_summary"]["request_quota"]["limit"], 1_000);
    assert_eq!(payload["concurrency"]["available"], false);
    assert_eq!(payload["concurrency"]["limit"], 1);
    assert!(payload["concurrency"].get("running").is_none());
    assert!(payload["concurrency"].get("waiting_upstream").is_none());
    assert!(payload["concurrency"].get("admitted").is_none());
    fault.arm_outage(false);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_short_waiting_lease_does_not_shorten_shared_waiting_ttl() {
    let mut long_config = redis_test_config();
    long_config.upstream_stream_max_duration_seconds = 300;
    long_config.upstream_concurrency_recovery_max_wait_ms = 300_000;
    let mut short_config = long_config.clone();
    short_config.upstream_stream_max_duration_seconds = 0;
    short_config.upstream_concurrency_recovery_max_wait_ms = 0;

    let directory = tempdir().unwrap();
    let long_state =
        AppState::load_from_path(directory.path().join("long.json"), long_config.clone())
            .await
            .unwrap();
    let short_state = AppState::load_from_path(directory.path().join("short.json"), short_config)
        .await
        .unwrap();
    let mut downstream = redis_test_downstream("down-runtime-shared-ttl");
    downstream.max_concurrency = 2;

    let long_lease = long_state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    long_state
        .mark_downstream_waiting(&long_lease)
        .await
        .unwrap();
    let waiting_key = redis_downstream_key(&long_config, &downstream.id, "waiting");
    let before = redis_integer(
        &redis_test_command(&long_config, &["PTTL".into(), waiting_key.clone()]).await,
    );

    let observed_at = Instant::now();
    let short_lease = short_state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    short_state
        .mark_downstream_waiting(&short_lease)
        .await
        .unwrap();
    let elapsed_ms = i64::try_from(observed_at.elapsed().as_millis()).unwrap_or(i64::MAX);
    let after =
        redis_integer(&redis_test_command(&long_config, &["PTTL".into(), waiting_key]).await);

    assert!(before > 0, "long waiting key must have a positive TTL");
    assert!(after > 0, "shared waiting key must retain a positive TTL");
    assert!(
        after.saturating_add(elapsed_ms).saturating_add(2_000) >= before,
        "short lease reduced shared waiting TTL from {before} ms to {after} ms"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_waiter_cancellation_advances_fifo_head() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-cancel", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let cancelled = first
        .register_account_waiter(&account, "req-cancel", "down-a", "lease-cancel")
        .await
        .unwrap();
    let retained = second
        .register_account_waiter(&account, "req-retain", "down-a", "lease-retain")
        .await
        .unwrap();
    first.cancel_account_waiter(&cancelled).await.unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;

    assert!(matches!(
        second.try_acquire_account_probe(&retained).await.unwrap(),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_stale_ticket_cannot_mutate_re_registration() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-ticket-fence", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let stale = first
        .register_account_waiter(&account, "req-ticket", "down-a", "lease-ticket")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let current = second
        .register_account_waiter(&account, "req-ticket", "down-a", "lease-ticket")
        .await
        .unwrap();

    assert!(first.cancel_account_waiter(&stale).await.is_err());
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert!(matches!(
        second.try_acquire_account_probe(&current).await.unwrap(),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_registration_token_fences_same_millisecond_replacement() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-ticket-token", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let stale = first
        .register_account_waiter(&account, "req-ticket", "down-a", "lease-ticket")
        .await
        .unwrap();
    let mut current = second
        .register_account_waiter(&account, "req-ticket", "down-a", "lease-ticket")
        .await
        .unwrap();
    assert_ne!(stale.registration_token, current.registration_token);

    let tickets = redis_account_key(&config, &account, "tickets");
    let force_timestamp_collision = vec![
        "EVAL".into(),
        "local ticket=cjson.decode(redis.call('HGET',KEYS[1],ARGV[1])); ticket.registered_at_ms=tonumber(ARGV[2]); redis.call('HSET',KEYS[1],ARGV[1],cjson.encode(ticket)); return 1".into(),
        "1".into(),
        tickets,
        current.request_id.clone(),
        stale.registered_at_ms.to_string(),
    ];
    assert_eq!(
        redis_test_command(&config, &force_timestamp_collision).await,
        ":1\r\n"
    );
    current.registered_at_ms = stale.registered_at_ms;

    assert!(first.cancel_account_waiter(&stale).await.is_err());
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert!(matches!(
        second.try_acquire_account_probe(&current).await.unwrap(),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_probe_fences_stale_owner_completion() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-stale", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let ticket = first
        .register_account_waiter(&account, "req-stale", "down-a", "lease-stale")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;
    let probe = match first.try_acquire_account_probe(&ticket).await.unwrap() {
        ProbeDecision::Granted(probe) => probe,
        other => panic!("expected a probe grant, got {other:?}"),
    };
    first.renew_account_probe(&probe).await.unwrap();
    second
        .finish_account_probe(&probe, AccountProbeOutcome::Accepted)
        .await
        .unwrap();

    assert!(first
        .finish_account_probe(&probe, AccountProbeOutcome::Accepted)
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_probe_script_renewal_extends_ownership_past_initial_ttl() {
    let config = redis_test_config();
    let account = AccountConcurrencyKey::new("up-renew", "fingerprint-a");
    let queue = redis_account_key(&config, &account, "waiters");
    let tickets = redis_account_key(&config, &account, "tickets");
    let sequence = redis_account_key(&config, &account, "sequence");
    let state = redis_account_key(&config, &account, "state");
    let probe = redis_account_key(&config, &account, "probe");
    let account_identity = queue
        .split('{')
        .nth(1)
        .and_then(|value| value.split('}').next())
        .unwrap();

    let reject = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/account_probe.lua").into(),
        "5".into(),
        queue.clone(),
        tickets.clone(),
        state.clone(),
        probe.clone(),
        redis_account_key(&config, &account, "mutation-reject-renew"),
        "reject".into(),
        account_identity.into(),
        "-1".into(),
        "0".into(),
        "1".into(),
        "reject-renew-token".into(),
        "100".into(),
    ];
    assert!(redis_test_command(&config, &reject)
        .await
        .contains(":0\r\n"));

    let register = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/account_waiter.lua").into(),
        "4".into(),
        queue.clone(),
        tickets.clone(),
        sequence,
        state.clone(),
        "register".into(),
        "req-renew".into(),
        "down-a".into(),
        "lease-renew".into(),
        "600000".into(),
        "660000".into(),
        "registration-renew".into(),
    ];
    let register_response = redis_test_command(&config, &register).await;
    let register_values = redis_integer_array(&register_response);
    assert_eq!(register_values.first(), Some(&0));
    let registered_at_ms = register_values[2].to_string();
    tokio::time::sleep(Duration::from_millis(120)).await;

    let grant = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/account_probe.lua").into(),
        "4".into(),
        queue.clone(),
        tickets.clone(),
        state.clone(),
        probe.clone(),
        "grant".into(),
        "req-renew".into(),
        "1".into(),
        registered_at_ms,
        "registration-renew".into(),
        "owner-renew".into(),
        "36000".into(),
    ];
    assert!(redis_test_command(&config, &grant).await.contains(":0\r\n"));
    tokio::time::sleep(Duration::from_millis(30_100)).await;

    let renew = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/account_probe.lua").into(),
        "4".into(),
        queue.clone(),
        tickets.clone(),
        state.clone(),
        probe.clone(),
        "renew".into(),
        "req-renew".into(),
        "1".into(),
        "owner-renew".into(),
        "36000".into(),
    ];
    let renew_response = redis_test_command(&config, &renew).await;
    assert!(
        renew_response.contains(":0\r\n"),
        "unexpected renew response: {renew_response:?}"
    );
    tokio::time::sleep(Duration::from_millis(6_100)).await;

    let finish = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/account_probe.lua").into(),
        "5".into(),
        queue,
        tickets,
        state,
        probe,
        redis_account_key(&config, &account, "mutation-finish-renew"),
        "finish".into(),
        "req-renew".into(),
        "1".into(),
        "owner-renew".into(),
        "accepted".into(),
        "finish-renew-token".into(),
    ];
    assert_eq!(redis_test_command(&config, &finish).await, "*1\r\n:0\r\n");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_keys_include_upstream_identity() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let first_account = AccountConcurrencyKey::new("up-one", "same-fingerprint");
    let second_account = AccountConcurrencyKey::new("up-two", "same-fingerprint");
    first
        .observe_account_concurrency(&first_account, None)
        .await
        .unwrap();
    second
        .observe_account_concurrency(&second_account, None)
        .await
        .unwrap();
    let first_ticket = first
        .register_account_waiter(&first_account, "req-one", "down-a", "lease-one")
        .await
        .unwrap();
    let second_ticket = second
        .register_account_waiter(&second_account, "req-two", "down-a", "lease-two")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;

    let (first_probe, second_probe) = tokio::join!(
        first.try_acquire_account_probe(&first_ticket),
        second.try_acquire_account_probe(&second_ticket),
    );
    assert!(matches!(first_probe.unwrap(), ProbeDecision::Granted(_)));
    assert!(matches!(second_probe.unwrap(), ProbeDecision::Granted(_)));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_release_removes_wait_state_atomically() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("down-release-waiting");
    let lease = first
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    first.mark_downstream_waiting(&lease).await.unwrap();
    first.release_downstream_concurrency(lease).await.unwrap();

    assert_eq!(
        second
            .downstream_runtime_snapshot(&downstream)
            .await
            .unwrap(),
        Default::default()
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_mutation_fails_closed_during_outage() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-outage", "fingerprint-a");
    let fault = coordination_fault(&first);
    fault.arm_outage(true);

    let error = first
        .register_account_waiter(&account, "req-outage", "down-a", "lease-outage")
        .await
        .expect_err("account mutation must not fall back to local state");
    assert_eq!(error.to_string(), "runtime coordination unavailable");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_waiter_registration_retry_is_idempotent() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-register-retry", "fingerprint-a");
    let fault = coordination_fault(&first);
    fault.lose_next_responses(1);
    first
        .register_account_waiter(&account, "req-retry", "down-a", "lease-retry")
        .await
        .unwrap();

    let sequence_key = redis_account_key(&config, &account, "sequence");
    let response = redis_test_command(&config, &["GET".into(), sequence_key]).await;
    assert_eq!(redis_bulk_u64(&response), 1);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_rejection_retry_advances_one_generation() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-reject-retry", "fingerprint-a");
    let fault = coordination_fault(&first);
    fault.lose_next_responses(1);
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let ticket = first
        .register_account_waiter(&account, "req-reject", "down-a", "lease-reject")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(320)).await;
    let probe = match first.try_acquire_account_probe(&ticket).await.unwrap() {
        ProbeDecision::Granted(probe) => probe,
        other => panic!("expected a probe grant, got {other:?}"),
    };
    assert_eq!(probe.generation, 1);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_concurrent_rejection_does_not_invalidate_an_active_probe() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-active-reject", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let ticket = first
        .register_account_waiter(&account, "req-probe", "down-a", "lease-probe")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;
    let probe = match first.try_acquire_account_probe(&ticket).await.unwrap() {
        ProbeDecision::Granted(probe) => probe,
        other => panic!("expected a probe grant, got {other:?}"),
    };

    second
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();

    first.renew_account_probe(&probe).await.unwrap();
    first
        .finish_account_probe(&probe, AccountProbeOutcome::Accepted)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_rejection_replay_survives_an_interleaved_mutation() {
    let config = redis_test_config();
    let account = AccountConcurrencyKey::new("up-reject-interleaved", "fingerprint-a");
    let queue = redis_account_key(&config, &account, "waiters");
    let tickets = redis_account_key(&config, &account, "tickets");
    let state = redis_account_key(&config, &account, "state");
    let probe = redis_account_key(&config, &account, "probe");
    let identity = queue
        .split('{')
        .nth(1)
        .and_then(|value| value.split('}').next())
        .unwrap()
        .to_string();

    for (token, marker) in [
        ("reject-token-a", "mutation-a"),
        ("reject-token-b", "mutation-b"),
        ("reject-token-a", "mutation-a"),
    ] {
        let reject = vec![
            "EVAL".into(),
            include_str!("../src/state/redis_runtime/account_probe.lua").into(),
            "5".into(),
            queue.clone(),
            tickets.clone(),
            state.clone(),
            probe.clone(),
            redis_account_key(&config, &account, marker),
            "reject".into(),
            identity.clone(),
            "-1".into(),
            "0".into(),
            "1".into(),
            token.into(),
            "100".into(),
        ];
        assert!(redis_test_command(&config, &reject)
            .await
            .contains(":0\r\n"));
    }

    let generation =
        redis_test_command(&config, &["HGET".into(), state, "generation".into()]).await;
    assert_eq!(redis_bulk_u64(&generation), 2);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_probe_grant_retry_returns_the_original_lease() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-grant-retry", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let ticket = first
        .register_account_waiter(&account, "req-grant", "down-a", "lease-grant")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;
    let fault = coordination_fault(&first);
    fault.lose_next_responses(1);

    assert!(matches!(
        first.try_acquire_account_probe(&ticket).await.unwrap(),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_probe_finish_retry_is_idempotent() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-finish-retry", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let ticket = first
        .register_account_waiter(&account, "req-finish", "down-a", "lease-finish")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;
    let probe = match first.try_acquire_account_probe(&ticket).await.unwrap() {
        ProbeDecision::Granted(probe) => probe,
        other => panic!("expected a probe grant, got {other:?}"),
    };
    let fault = coordination_fault(&first);
    fault.lose_next_responses(1);

    first
        .finish_account_probe(&probe, AccountProbeOutcome::Accepted)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_concurrency_rejection_requeues_at_the_tail() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-requeue", "fingerprint-a");
    first
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let oldest = first
        .register_account_waiter(&account, "req-oldest", "down-a", "lease-oldest")
        .await
        .unwrap();
    let next = second
        .register_account_waiter(&account, "req-next", "down-a", "lease-next")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;
    let probe = match first.try_acquire_account_probe(&oldest).await.unwrap() {
        ProbeDecision::Granted(probe) => probe,
        other => panic!("expected a probe grant, got {other:?}"),
    };
    first
        .finish_account_probe(
            &probe,
            AccountProbeOutcome::ConcurrencyRejected { retry_after: None },
        )
        .await
        .unwrap();
    let retried = first
        .register_account_waiter(&account, "req-oldest", "down-a", "lease-oldest")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(320)).await;

    assert!(matches!(
        first.try_acquire_account_probe(&retried).await.unwrap(),
        ProbeDecision::Wait { .. }
    ));
    assert!(matches!(
        second.try_acquire_account_probe(&next).await.unwrap(),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_recovery_retry_after_is_queryable_across_instances() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-recovery-deadline", "fingerprint-a");
    first
        .observe_account_concurrency(&account, Some(Duration::from_secs(30)))
        .await
        .unwrap();

    let retry_after = second.account_recovery_retry_after(&account).await.unwrap();

    assert!(retry_after <= Duration::from_secs(30));
    assert!(retry_after >= Duration::from_secs(29));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_account_state_ttl_covers_long_explicit_retry_after() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let account = AccountConcurrencyKey::new("up-long-retry", "fingerprint-a");
    first
        .observe_account_concurrency(&account, Some(Duration::from_secs(1_800)))
        .await
        .unwrap();

    let state_key = redis_account_key(&config, &account, "state");
    let response = redis_test_command(&config, &["TTL".into(), state_key]).await;
    let ttl = redis_integer(&response);
    assert!(ttl >= 2_399, "long retry-after was truncated to TTL {ttl}");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_cost_usage_is_shared() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("shared-cost-limit");
    downstream.per_minute_limit = 60;
    downstream.billing_mode = "token".into();
    downstream.input_token_price_per_million_cents = Some(1_000_000);
    downstream.output_token_price_per_million_cents = Some(1_000_000);
    downstream.daily_cost_limit_cents = Some(10);
    // Legacy raw token limit fields are ignored; only cost billing enforces
    // the daily rolling window.
    downstream.daily_token_limit = Some(1);
    first.insert_downstream(downstream.clone()).await.unwrap();

    let mut log = redis_test_usage_log("redis-cost-event", &downstream.id, 10);
    log.total_cost_cents = Some(10);
    first.append_usage_log(log).await.unwrap();

    let rejection = second
        .reserve_downstream_request(&downstream)
        .await
        .expect_err("the second coordinator must observe shared cost usage");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::DailyCostQuotaExceeded {
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
async fn redis_cost_retry_after_waits_until_window_expires() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("cost-retry-after");
    downstream.per_minute_limit = 60;
    downstream.billing_mode = "token".into();
    downstream.input_token_price_per_million_cents = Some(1_000_000);
    downstream.output_token_price_per_million_cents = Some(1_000_000);
    downstream.daily_cost_limit_cents = Some(100);
    first.insert_downstream(downstream.clone()).await.unwrap();

    let mut small = redis_test_usage_log("small-old-event", &downstream.id, 1);
    small.total_cost_cents = Some(1);
    first.append_usage_log(small).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let mut large = redis_test_usage_log("large-new-event", &downstream.id, 100);
    large.total_cost_cents = Some(100);
    first.append_usage_log(large).await.unwrap();

    let rejection = second
        .reserve_downstream_request(&downstream)
        .await
        .expect_err("the daily cost quota must be exhausted");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::DailyCostQuotaExceeded {
            retry_after_seconds,
            limit: 100,
            used: 101,
        } if retry_after_seconds >= 86_000
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_legacy_token_limit_without_prices_writes_no_window() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("legacy-token-no-window");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(100);
    downstream.billing_mode = "token".into();
    // No prices: not cost billing, so no daily window key is written at all.
    first.insert_downstream(downstream.clone()).await.unwrap();
    first
        .append_usage_log(redis_test_usage_log("legacy-no-window", &downstream.id, 1))
        .await
        .unwrap();

    let identity = format!("{:x}", Sha256::digest(downstream.id.as_bytes()));
    let token_key = format!(
        "{}:v1:downstream:{{{identity}}}:tokens",
        config.redis_key_prefix
    );
    let response = redis_test_command(&config, &["EXISTS".into(), token_key]).await;
    assert_eq!(
        redis_integer(&response),
        0,
        "legacy token limit without prices must not write a cost window"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_cost_billing_token_keys_use_daily_retention() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("cost-billing-retention");
    downstream.per_minute_limit = 60;
    downstream.billing_mode = "token".into();
    downstream.input_token_price_per_million_cents = Some(1000);
    downstream.output_token_price_per_million_cents = Some(1000);
    downstream.daily_cost_limit_cents = Some(3000);
    // 按金额计费不依赖每日 token 数；留空验证金额窗口依然按 24h 滚动。
    downstream.daily_token_limit = None;
    first.insert_downstream(downstream.clone()).await.unwrap();
    first
        .append_usage_log(redis_test_usage_log(
            "cost-billing-retention",
            &downstream.id,
            1,
        ))
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
    assert!(
        ttl > 86_000 && ttl <= 86_460,
        "cost-billed downstream without a token limit must still keep a 24h rolling window, got TTL {ttl}"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_release_and_rollback_retry_once_after_timeout() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("release-retry");
    downstream.per_minute_limit = 1;

    let lease = first
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    let fault = coordination_fault(&first);
    fault.lose_next_responses(1);
    first
        .release_downstream_concurrency(lease)
        .await
        .expect("lease release must retry once after the first timeout");
    let replacement = second
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    second
        .release_downstream_concurrency(replacement)
        .await
        .unwrap();

    let reservation = first.reserve_downstream_request(&downstream).await.unwrap();
    fault.lose_next_responses(1);
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
async fn redis_reserves_retry_commit_after_response_loss_without_double_counting() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;

    let downstream = redis_test_downstream("reserve-replay-request");
    let fault = coordination_fault(&first);
    fault.lose_next_responses(1);
    let reservation = first
        .reserve_downstream_request(&downstream)
        .await
        .expect("request reserve must replay the same event after a lost response");
    let rejection = second
        .reserve_downstream_request(&downstream)
        .await
        .expect_err("a replayed request event must count exactly once");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::PerMinuteLimitExceeded { used: 1, .. }
    ));
    first
        .rollback_downstream_request_reservation(reservation)
        .await
        .unwrap();

    let downstream = redis_test_downstream("reserve-replay-lease");
    fault.lose_next_responses(1);
    let downstream_lease = first
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .expect("downstream lease reserve must replay the same lease after a lost response");
    assert!(matches!(
        second
            .try_reserve_downstream_concurrency(&downstream, "test-model")
            .await,
        Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded { .. })
    ));
    first
        .release_downstream_concurrency(downstream_lease)
        .await
        .unwrap();

    let upstream = redis_test_upstream("reserve-replay-upstream");
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();
    fault.lose_next_responses(1);
    let upstream_lease = first
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .expect("upstream reserve must replay the same event and lease after a lost response");
    let snapshot = second
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .remove(&upstream.id)
        .unwrap();
    assert_eq!(snapshot.in_flight, 1);
    assert_eq!(snapshot.minute_cost, 1.0);
    assert_eq!(snapshot.five_hour_cost, 1.0);
    first
        .release_upstream_request(upstream_lease)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn failed_redis_releases_can_be_retried_by_a_clone() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("release-clone-retry");
    let upstream = redis_test_upstream("upstream-release-clone-retry");
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();

    let downstream_lease = first
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    let downstream_retry_during_outage = downstream_lease.clone();
    let downstream_retry_after_recovery = downstream_lease.clone();
    let upstream_lease = first
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let upstream_retry_during_outage = upstream_lease.clone();
    let upstream_retry_after_recovery = upstream_lease.clone();

    let fault = coordination_fault(&first);
    fault.arm_outage(true);
    let (downstream_result, upstream_result) = tokio::join!(
        first.release_downstream_concurrency(downstream_lease),
        first.release_upstream_request(upstream_lease),
    );
    assert!(downstream_result.is_err());
    assert!(upstream_result.is_err());

    fault.arm_outage(false);
    fault.arm_outage(true);
    let (downstream_result, upstream_result) = tokio::join!(
        first.release_downstream_concurrency(downstream_retry_during_outage),
        first.release_upstream_request(upstream_retry_during_outage),
    );
    assert!(
        downstream_result.is_err(),
        "a retained downstream clone must retry Redis instead of returning false success"
    );
    assert!(
        upstream_result.is_err(),
        "a retained upstream clone must retry Redis instead of returning false success"
    );
    assert_eq!(
        first.redis_upstream_release_failure_count(),
        2,
        "failed Redis upstream releases must be counted"
    );

    fault.arm_outage(false);
    let (downstream_result, upstream_result) = tokio::join!(
        first.release_downstream_concurrency(downstream_retry_after_recovery),
        first.release_upstream_request(upstream_retry_after_recovery),
    );
    downstream_result.unwrap();
    upstream_result.unwrap();

    let replacement = second
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    second
        .release_downstream_concurrency(replacement)
        .await
        .unwrap();
    assert_eq!(
        second
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .unwrap()
            .in_flight,
        0
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn failed_redis_token_recording_does_not_queue_a_duplicate_usage_log() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("token-record-retry");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(100);
    downstream.billing_mode = "token".into();
    // Cost-billing fields: without them `cost_billing_mode()` is false and the
    // usage-log write never reaches the Redis token-recording path, so these
    // tests would silently exercise nothing (the cost-billing refactor in
    // 44ab6bee tightened this predicate and the ignored suite never ran).
    downstream.input_token_price_per_million_cents = Some(1_000_000);
    downstream.output_token_price_per_million_cents = Some(1_000_000);
    downstream.daily_cost_limit_cents = Some(100);
    first.insert_downstream(downstream.clone()).await.unwrap();
    let log = redis_test_usage_log("retryable-token-log", &downstream.id, 10);

    let fault = coordination_fault(&first);
    fault.arm_outage(true);
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

    tokio::time::sleep(Duration::from_millis(200)).await;
    fault.arm_outage(false);
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
async fn redis_token_recording_retries_commit_after_response_loss() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("token-record-response-loss");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(100);
    downstream.billing_mode = "token".into();
    // Cost-billing fields: without them `cost_billing_mode()` is false and the
    // usage-log write never reaches the Redis token-recording path, so these
    // tests would silently exercise nothing (the cost-billing refactor in
    // 44ab6bee tightened this predicate and the ignored suite never ran).
    downstream.input_token_price_per_million_cents = Some(1_000_000);
    downstream.output_token_price_per_million_cents = Some(1_000_000);
    downstream.daily_cost_limit_cents = Some(100);
    first.insert_downstream(downstream.clone()).await.unwrap();
    let log = redis_test_usage_log("token-record-response-loss", &downstream.id, 10);

    first
        .append_usage_log(redis_test_usage_log(
            "token-record-warmup",
            &downstream.id,
            1,
        ))
        .await
        .unwrap();

    let fault = coordination_fault(&first);
    fault.lose_next_responses(1);
    first
        .append_usage_log(log.clone())
        .await
        .expect("token recording must replay the same event after a lost response");

    assert_eq!(
        fault.lost_response_commits(),
        1,
        "the lost attempt must commit its token write before the coordinator replays it"
    );

    assert_eq!(
        first
            .snapshot()
            .await
            .usage_logs
            .iter()
            .filter(|entry| entry.id == log.id)
            .count(),
        1
    );
    let identity = format!("{:x}", Sha256::digest(downstream.id.as_bytes()));
    let token_key = format!(
        "{}:v1:downstream:{{{identity}}}:tokens",
        config.redis_key_prefix
    );
    let token_values_key = format!(
        "{}:v1:downstream:{{{identity}}}:token_values",
        config.redis_key_prefix
    );
    let member = format!("history:{}", log.id);
    assert_eq!(
        redis_integer(&redis_test_command(&config, &["ZCARD".into(), token_key]).await),
        2
    );
    assert_eq!(
        redis_bulk_u64(
            &redis_test_command(&config, &["HGET".into(), token_values_key, member],).await,
        ),
        10
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_token_replay_preserves_original_score_and_value() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("token-replay-first-write-wins");
    downstream.per_minute_limit = 60;
    downstream.daily_token_limit = Some(1_000);
    downstream.billing_mode = "token".into();
    // Cost-billing fields: without them `cost_billing_mode()` is false and the
    // usage-log write never reaches the Redis token-recording path, so these
    // tests would silently exercise nothing (the cost-billing refactor in
    // 44ab6bee tightened this predicate and the ignored suite never ran).
    downstream.input_token_price_per_million_cents = Some(1_000_000);
    downstream.output_token_price_per_million_cents = Some(1_000_000);
    downstream.daily_cost_limit_cents = Some(100);
    first.insert_downstream(downstream.clone()).await.unwrap();

    let log_id = "replayed-token-event";
    first
        .append_usage_log(redis_test_usage_log(log_id, &downstream.id, 10))
        .await
        .unwrap();
    let identity = format!("{:x}", Sha256::digest(downstream.id.as_bytes()));
    let token_key = format!(
        "{}:v1:downstream:{{{identity}}}:tokens",
        config.redis_key_prefix
    );
    let token_values_key = format!(
        "{}:v1:downstream:{{{identity}}}:token_values",
        config.redis_key_prefix
    );
    let member = format!("history:{log_id}");
    let original_score = redis_bulk_u64(
        &redis_test_command(
            &config,
            &["ZSCORE".into(), token_key.clone(), member.clone()],
        )
        .await,
    );
    let original_value = redis_bulk_u64(
        &redis_test_command(
            &config,
            &["HGET".into(), token_values_key.clone(), member.clone()],
        )
        .await,
    );
    tokio::time::sleep(Duration::from_millis(5)).await;

    first
        .append_usage_log(redis_test_usage_log(log_id, &downstream.id, 99))
        .await
        .unwrap();

    let replayed_score = redis_bulk_u64(
        &redis_test_command(&config, &["ZSCORE".into(), token_key, member.clone()]).await,
    );
    let replayed_value = redis_bulk_u64(
        &redis_test_command(&config, &["HGET".into(), token_values_key, member]).await,
    );
    assert_eq!(original_score, replayed_score);
    assert_eq!(original_value, 10);
    assert_eq!(original_value, replayed_value);
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

    let fault = coordination_fault(&first);
    fault.arm_outage(true);
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

    fault.arm_outage(false);
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
    // Flat 1.0 request cost: a limit of 1 makes the second (hedge) request
    // exceed the retained minute quota event after the lease is released.
    upstream.requests_per_minute = 1;
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();

    let first_lease = first
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let snapshots = second.upstream_runtime_snapshots().await.unwrap();
    let snapshot = snapshots.get(&upstream.id).unwrap();
    assert_eq!(snapshot.in_flight, 1);
    assert_eq!(snapshot.minute_cost, 1.0);
    assert_eq!(snapshot.five_hour_cost, 1.0);

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
    assert_eq!(snapshot.minute_cost, 1.0);
    assert_eq!(snapshot.five_hour_cost, 1.0);

    let minute_rejection = second
        .try_reserve_upstream_hedge(&upstream, "model-a")
        .await
        .expect_err("releasing a lease must not erase its quota event");
    assert!(!minute_rejection.is_runtime_coordination_unavailable());

    upstream.requests_per_minute = 100;
    upstream.request_quota_requests = 1;
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
async fn local_main_upstream_request_respects_max_concurrency() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let mut upstream = redis_test_upstream("local-main-request-concurrency");
    upstream.max_concurrency = 1;
    state.insert_upstream(upstream.clone()).await.unwrap();

    let first = state
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let rejection = state
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .expect_err("the main request must respect max_concurrency");
    assert!(
        !rejection.is_runtime_coordination_unavailable(),
        "a full concurrency slot is an admission rejection, not a coordination failure"
    );

    state.release_upstream_request(first).await.unwrap();
    let retry = state
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    state.release_upstream_request(retry).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_main_upstream_request_respects_max_concurrency() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("redis-main-request-concurrency");
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();

    let first_lease = first
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let rejection = second
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .expect_err("the main request must observe the shared lease");
    assert!(!rejection.is_runtime_coordination_unavailable());

    first.release_upstream_request(first_lease).await.unwrap();
    let retry = second
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    second.release_upstream_request(retry).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_concurrency_is_scoped_per_account() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut upstream = redis_test_upstream("redis-per-account-concurrency");
    upstream.max_concurrency = 1;
    first.insert_upstream(upstream.clone()).await.unwrap();
    second.insert_upstream(upstream.clone()).await.unwrap();
    let fingerprint_a = upstream_key_fingerprint(&upstream.id, "account-a");
    let fingerprint_b = upstream_key_fingerprint(&upstream.id, "account-b");

    let lease_a = first
        .try_reserve_upstream_account_request(&upstream, &fingerprint_a, "model-a")
        .await
        .expect("account A should reserve its first Redis slot");
    let lease_b = second
        .try_reserve_upstream_account_request(&upstream, &fingerprint_b, "model-a")
        .await
        .expect("account B should have an independent Redis slot");
    let same_account_rejection = second
        .try_reserve_upstream_account_request(&upstream, &fingerprint_a, "model-a")
        .await
        .expect_err("a second request on account A must exceed its Redis limit");

    assert_eq!(same_account_rejection.retry_after_seconds, 1);
    assert_eq!(
        first
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .expect("shared Redis upstream runtime snapshot")
            .in_flight,
        2,
    );

    first.release_upstream_request(lease_a).await.unwrap();
    second.release_upstream_request(lease_b).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_gateway_local_capacity_release_is_immediately_schedulable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_hits = Arc::new(AtomicUsize::new(0));
    let hits = upstream_hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Json(json!({
                    "id": "chatcmpl-capacity-release",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "model-a",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }]
                }))
            }
        }),
    );
    let upstream_server = tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let mut config = redis_test_config();
    config.upstream_stream_max_duration_seconds = 86_400;
    // Runtime settings require a positive recovery budget. Disable the
    // route-exhaustion retry path explicitly instead of using an invalid zero
    // budget, so the first request observes the held Redis slot immediately.
    config.upstream_concurrency_recovery_max_wait_ms = 1;
    config.upstream_route_exhaustion_retry_enabled = false;
    let (state_a, state_b, _directory) = redis_test_states(&config).await;
    let api_key = "capacity-release-account";
    let upstream = UpstreamConfig {
        id: "redis-gateway-capacity-release".into(),
        name: "Redis gateway capacity release".into(),
        base_url: format!("http://{address}"),
        api_key: api_key.into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["model-a".into()],
        max_concurrency: 1,
        active: true,
        ..UpstreamConfig::default()
    };
    state_b.insert_upstream(upstream.clone()).await.unwrap();

    let downstream_key = generate_downstream_key("redis-capacity-release");
    let mut downstream = redis_test_downstream("redis-capacity-release-downstream");
    downstream.hash = downstream_key.hash;
    downstream.model_allowlist = vec!["model-a".into()];
    downstream.rate_limit_enabled = false;
    downstream.max_concurrency = 10;
    state_b.insert_downstream(downstream).await.unwrap();

    let key_fingerprint = upstream_key_fingerprint(&upstream.id, api_key);
    let held = state_a
        .try_reserve_upstream_account_request(&upstream, &key_fingerprint, "model-a")
        .await
        .unwrap();
    let route = redis_test_health_route(&upstream.id, &key_fingerprint, "model-a");
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "model": "model-a",
                    "messages": [{"role": "user", "content": "hello"}]
                })
                .to_string(),
            ))
            .unwrap()
    };
    let app = build_router(state_b.clone());

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(first.headers()[header::RETRY_AFTER], "1");
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);
    assert!(state_a
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .is_none());
    assert!(state_b
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .is_none());

    state_a.release_upstream_request(held).await.unwrap();
    let second = app.oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);

    upstream_server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_snapshot_counts_flat_request_cost() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut upstream = redis_test_upstream("precise-upstream-costs");
    // Upstream model-weighted request costs were removed; every request
    // consumes a flat 1.0 quota unit.
    upstream.max_concurrency = 2;
    let request_cost = 1.0_f64;
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

    let fault = coordination_fault(&second);
    fault.arm_outage(true);
    let coordination = second
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .expect_err("Redis timeout must fail closed");
    assert!(coordination.is_runtime_coordination_unavailable());
    fault.arm_outage(false);
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

    coordination_fault(&state).arm_outage(true);
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
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

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_cooldown_and_half_open_owner_are_shared() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("health-upstream", "fingerprint-a");
    let route = redis_test_health_route("health-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(50)),
            false,
        )
        .await
        .unwrap();
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Cooling {
            class: RouteFailureClass::ConcurrencySaturated,
            ..
        }
    ));

    tokio::time::sleep(Duration::from_millis(60)).await;
    let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    permit.finish(RouteOutcome::Success).await.unwrap();
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Ready(permit) if !permit.is_half_open()
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_exclusive_window_allows_admission_after_window() {
    // T1 Redis backend: the half-open exclusivity window (150ms here) bounds
    // how long a probe may exclusively occupy a recovering route. After the
    // window elapses the route is admitted without a lease while the original
    // lease is still alive, and a successful no-lease request clears the
    // route state through the same-observation path.
    let mut config = redis_test_config();
    config.upstream_route_half_open_exclusive_window_ms = 150;
    // T1.2: the upstream Retry-After hint is only a *floor* under the local
    // backoff curve (`cooldown = max(local, explicit)`), so the 50ms hint
    // below no longer yields a 50ms cooldown.  State the curve explicitly
    // (base 1s, jittered 80-120% -> at most 1.2s) and sleep past it.
    config.upstream_transient_route_cooldown_base_seconds = 1;
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("exclusive-window", "fingerprint-a");
    let route = redis_test_health_route("exclusive-window", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::TransientServer,
            Some(Duration::from_millis(50)),
            false,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    // Window (150ms) elapses while the lease is still alive.
    tokio::time::sleep(Duration::from_millis(160)).await;
    let admission = match second.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if !permit.is_half_open() => permit,
        other => panic!("expected no-lease admission after window, got {other:?}"),
    };
    // The no-lease success clears the route (same-observation); further
    // reserves are ready without a lease.
    admission.finish(RouteOutcome::Success).await.unwrap();
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Ready(permit) if !permit.is_half_open()
    ));
    permit.finish(RouteOutcome::Success).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_settle_healthy_releases_exclusive_window_for_other_state() {
    // T2 Redis backend: settling the half-open lease healthy on the first
    // semantic output releases the exclusive window immediately, so another
    // AppState gets a fresh lease (or an admission) while the probe stream is
    // still running; the route state is cleared entirely.
    let mut config = redis_test_config();
    config.upstream_route_half_open_exclusive_window_ms = 150;
    // T1.2: the upstream Retry-After hint is only a floor under the local
    // backoff curve, so the 50ms hint below no longer yields a 50ms cooldown.
    // State the curve explicitly (base 1s, jittered 80-120% -> at most 1.2s)
    // and sleep past it.
    config.upstream_transient_route_cooldown_base_seconds = 1;
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("settle-healthy-upstream", "fingerprint-a");
    let route = redis_test_health_route("settle-healthy-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::TransientServer,
            Some(Duration::from_millis(50)),
            false,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    // While the probe lease is live, a second state sees the route as busy.
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    // T2: settle healthy (as if the first semantic output arrived). The
    // exclusivity is released well before the 150ms window or the lease TTL,
    // and the route state is cleared.
    permit.settle_healthy().await.unwrap();
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Ready(permit) if !permit.is_half_open()
    ));
    assert!(first.route_health_snapshot(&route).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_settled_permit_failure_observes_route_without_lease() {
    // T2 Redis backend: after a healthy settle, a stream failure is recorded
    // as a fresh no-lease observation (step 1) rather than a half-open probe
    // failure (which would escalate the streak and needs the lease).
    let mut config = redis_test_config();
    // T1.2: the local backoff curve dominates the (absent) upstream hint, so
    // the seed cooldown is the local step-1 duration.  State the curve
    // explicitly (base 1s, jittered 80-120% -> at most 1.2s) and sleep past it.
    config.upstream_transient_route_cooldown_base_seconds = 1;
    let (first, _second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("settled-observe-upstream", "fingerprint-a");
    let route = redis_test_health_route("settled-observe-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::Transport, None, false)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    permit.settle_healthy().await.unwrap();
    assert!(first.route_health_snapshot(&route).await.unwrap().is_none());

    // The settled permit must not call the finish script again (committed
    // marker); the failure goes through the no-lease observe script.
    permit
        .finish(RouteOutcome::RouteFailure {
            class: RouteFailureClass::Transport,
            upstream_status: Some(502),
            repeat_within_request: false,
            sole_candidate: false,
            capacity_sole_route: false,
            shared_host_failure_domain: false,
        })
        .await
        .unwrap();
    let snapshot = first
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("post-settle failure must be observed");
    assert_eq!(
        snapshot.consecutive_failures, 1,
        "post-settle failure must start a fresh streak, got {snapshot:?}"
    );
    assert_eq!(
        snapshot.last_failure_class,
        Some(RouteFailureClass::Transport)
    );
    assert!(!snapshot.half_open, "{snapshot:?}");
    assert!(snapshot.cooldown_remaining > Duration::ZERO);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_probe_ignores_cooldown_and_is_single_flight() {
    // A3 Redis backend: while the route is cooling, the last-resort probe API
    // ignores the remaining cooldown and grants a single-flight half-open
    // lease; a second caller is busy until the first finishes, and a
    // successful probe clears the cooldown entirely.
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("early-probe-upstream", "fingerprint-a");
    let route = redis_test_health_route("early-probe-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    // Default tuning observes a long cooldown; a normal reserve must refuse.
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Cooling { .. }
    ));

    let permit = match first
        .reserve_route_health_probe(&route, &key)
        .await
        .unwrap()
    {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected early half-open lease, got {other:?}"),
    };
    // Single-flight: the second early probe on the same route is busy while
    // the lease is active, even though the cooldown has not elapsed.
    assert!(matches!(
        second
            .reserve_route_health_probe(&route, &key)
            .await
            .unwrap(),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    // A successful probe clears the cooldown and the route is healthy again:
    // the Redis backend drops the route hash entirely, so the snapshot is
    // gone and a normal reserve is ready immediately.
    permit.finish(RouteOutcome::Success).await.unwrap();
    assert!(first.route_health_snapshot(&route).await.unwrap().is_none());
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Ready(permit) if !permit.is_half_open()
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_early_probe_exclusivity_ends_with_the_cooldown_not_the_lease() {
    // T9/F2 Redis backend: mirror of the local registry test. The probe
    // lease's exclusivity ends with the route's remaining cooldown (the new
    // ARGV[5] exclusive_window_ms in route_health_probe.lua), so a regular
    // reserve is admitted as a plain non-half-open lease afterwards although
    // the probe lease itself is still alive (TTL 300s). While cooling, the
    // route still refuses regular reserves (order invariant).
    let config = AppConfig {
        upstream_transient_route_cooldown_base_seconds: 1,
        upstream_transient_route_cooldown_max_seconds: 2,
        upstream_route_half_open_exclusive_window_ms: 500,
        ..redis_test_config()
    };
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("early-probe-exclusivity-upstream", "fingerprint-a");
    let route = redis_test_health_route(
        "early-probe-exclusivity-upstream",
        "fingerprint-a",
        "model-a",
    );

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    let cooldown_remaining = first
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("route failure must be visible")
        .cooldown_remaining;

    // The early probe ignores the remaining cooldown and takes the
    // single-flight half-open lease.
    let probe = match first
        .reserve_route_health_probe(&route, &key)
        .await
        .unwrap()
    {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected early half-open lease, got {other:?}"),
    };
    // Ordering invariant: while still cooling, a regular reserve is Cooling.
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Cooling { .. }
    ));

    // Wait out the cooldown plus the exclusive window (500ms, shorter than
    // the ~1s cooldown so exclusivity ends exactly at the cooldown end). The
    // probe lease is still alive, but the route admits a plain ready lease.
    tokio::time::sleep(cooldown_remaining + Duration::from_millis(700)).await;
    assert!(matches!(
        second.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Ready(permit) if !permit.is_half_open()
    ));

    // The probe still owns its lease and clears the route health on success.
    probe.finish(RouteOutcome::Success).await.unwrap();
    assert!(first.route_health_snapshot(&route).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_probe_enforces_one_second_interval_per_route() {
    // A3 Redis backend: after an early probe (even a cancelled one) the same
    // route refuses another early probe for ~1s; normal reserves stay cooling
    // during the window and a fresh probe is granted after it.
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("early-probe-interval-upstream", "fingerprint-a");
    let route =
        redis_test_health_route("early-probe-interval-upstream", "fingerprint-a", "model-a");

    state
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();

    let first = match state
        .reserve_route_health_probe(&route, &key)
        .await
        .unwrap()
    {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected early half-open lease, got {other:?}"),
    };
    // Released without a physical attempt: the interval still applies.
    first.finish(RouteOutcome::Cancelled).await.unwrap();

    let busy = match state
        .reserve_route_health_probe(&route, &key)
        .await
        .unwrap()
    {
        RouteAvailability::HalfOpenBusy { retry_after, .. } => retry_after,
        other => panic!("expected interval-busy, got {other:?}"),
    };
    assert!(
        busy <= Duration::from_secs(1),
        "interval refusal must report the remaining 1s window, got {busy:?}"
    );
    assert!(matches!(
        state.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Cooling { .. }
    ));

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(matches!(
        state
            .reserve_route_health_probe(&route, &key)
            .await
            .unwrap(),
        RouteAvailability::Ready(permit) if permit.is_half_open()
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_probe_failure_stays_capped_and_keeps_interval() {
    // A3 Redis backend: a failing early probe follows the half-open failure
    // path: the step stays capped and the 1s probe window stays armed for the
    // next caller.
    let mut config = redis_test_config();
    // T1.3/P0: the shipped default max step is 2, which would cap the seeded
    // step at 2 and make this test indistinguishable from the cap behavior it
    // pins.  State the curve explicitly so the independent seed reaches 5.
    config.upstream_transient_route_cooldown_max_step = 5;
    let (state, _second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("early-probe-capped-upstream", "fingerprint-a");
    let route = redis_test_health_route("early-probe-capped-upstream", "fingerprint-a", "model-a");

    // Seed a step of 5 through independent failures; a probe failure must not
    // push it to 6.
    for _ in 0..5 {
        state
            .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
            .await
            .unwrap();
    }
    let seeded = state
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("seeded route health must exist");
    assert_eq!(seeded.consecutive_failures, 5);

    let permit = match state
        .reserve_route_health_probe(&route, &key)
        .await
        .unwrap()
    {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected early half-open lease, got {other:?}"),
    };
    permit
        .finish(RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: Some(502),
            repeat_within_request: false,
            sole_candidate: false,
            capacity_sole_route: false,
            shared_host_failure_domain: false,
        })
        .await
        .unwrap();
    let snapshot = state
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("route health must survive a failed probe");
    assert_eq!(
        snapshot.consecutive_failures, 5,
        "an early probe failure must stay capped at the half-open step"
    );
    assert!(snapshot.cooldown_remaining > Duration::ZERO);

    // The interval persists after the failed probe: no immediate re-probe.
    assert!(matches!(
        state
            .reserve_route_health_probe(&route, &key)
            .await
            .unwrap(),
        RouteAvailability::HalfOpenBusy { .. }
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_probe_refuses_when_key_cooling_or_route_healthy() {
    // A3 Redis backend guards: a cooling key (credentials/quota quarantine)
    // must not be probed, and a route without health state has nothing to
    // probe.
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("early-probe-guards-upstream", "fingerprint-a");
    let route = redis_test_health_route("early-probe-guards-upstream", "fingerprint-a", "model-a");

    // No route health state: nothing to probe.
    let refusal = match state
        .reserve_route_health_probe(&route, &key)
        .await
        .unwrap()
    {
        RouteAvailability::Cooling { retry_after, .. } => retry_after,
        other => panic!("expected refusal for a route without health state, got {other:?}"),
    };
    assert!(refusal.is_zero());

    state
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    state
        .observe_key_failure(&key, RouteFailureClass::Credentials, None)
        .await
        .unwrap();
    assert!(matches!(
        state
            .reserve_route_health_probe(&route, &key)
            .await
            .unwrap(),
        RouteAvailability::Cooling { .. }
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn route_reservation_self_heals_only_legacy_local_admission_cooldown() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let upstream_id = "legacy-local-admission-upstream";
    let fingerprint = "fingerprint-a";
    let key = redis_test_health_key(upstream_id, fingerprint);
    let legacy_route = redis_test_health_route(upstream_id, fingerprint, "legacy-local");

    state
        .observe_route_failure(
            &legacy_route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(86_000_000)),
            false,
        )
        .await
        .unwrap();

    let controls = [
        (
            "provider-concurrency",
            RouteFailureClass::ConcurrencySaturated,
            Some("503"),
            86_000_000_u64,
        ),
        (
            "short-concurrency",
            RouteFailureClass::ConcurrencySaturated,
            None,
            2_000_u64,
        ),
        (
            "transient",
            RouteFailureClass::TransientServer,
            None,
            86_000_000_u64,
        ),
    ];
    let mut control_routes = Vec::new();
    for (model, class, status, cooldown_ms) in controls {
        let route = redis_test_health_route(upstream_id, fingerprint, model);
        state
            .observe_route_failure(
                &route,
                class,
                Some(Duration::from_millis(cooldown_ms)),
                false,
            )
            .await
            .unwrap();
        if let Some(status) = status {
            let response = redis_test_command(
                &config,
                &[
                    "HSET".into(),
                    redis_route_health_route_state_key(&config, &route),
                    "failure_status".into(),
                    status.into(),
                ],
            )
            .await;
            assert!(response.starts_with(':'));
        }
        control_routes.push((route, class));
    }

    let permit = match state
        .reserve_route_health(&legacy_route, &key)
        .await
        .unwrap()
    {
        RouteAvailability::Ready(permit) => permit,
        other => panic!("legacy local-admission cooldown must self-heal, got {other:?}"),
    };
    assert!(!permit.is_half_open());
    assert_eq!(
        redis_integer(
            &redis_test_command(
                &config,
                &[
                    "EXISTS".into(),
                    redis_route_health_route_state_key(&config, &legacy_route),
                ],
            )
            .await,
        ),
        0
    );

    for (route, expected_class) in control_routes {
        assert!(matches!(
            state.reserve_route_health(&route, &key).await.unwrap(),
            RouteAvailability::Cooling { class, .. } if class == expected_class
        ));
        assert_eq!(
            redis_integer(
                &redis_test_command(
                    &config,
                    &[
                        "EXISTS".into(),
                        redis_route_health_route_state_key(&config, &route),
                    ],
                )
                .await,
            ),
            1
        );
    }
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn legacy_local_admission_route_health_is_repaired_selectively() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let upstream_id = "startup-legacy-repair-upstream";
    let api_key = "startup-legacy-repair-secret";
    let fingerprint = upstream_key_fingerprint(upstream_id, api_key);
    let legacy_route = redis_test_health_route(upstream_id, &fingerprint, "legacy-local");

    state
        .observe_route_failure(
            &legacy_route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(86_000_000)),
            false,
        )
        .await
        .unwrap();

    let controls = [
        (
            "provider-concurrency",
            RouteFailureClass::ConcurrencySaturated,
            Some("503"),
            86_000_000_u64,
        ),
        (
            "short-concurrency",
            RouteFailureClass::ConcurrencySaturated,
            None,
            2_000_u64,
        ),
        (
            "transient",
            RouteFailureClass::TransientServer,
            None,
            86_000_000_u64,
        ),
    ];
    let mut control_state_keys = Vec::new();
    for (model, class, status, cooldown_ms) in controls {
        let route = redis_test_health_route(upstream_id, &fingerprint, model);
        state
            .observe_route_failure(
                &route,
                class,
                Some(Duration::from_millis(cooldown_ms)),
                false,
            )
            .await
            .unwrap();
        let state_key = redis_route_health_route_state_key(&config, &route);
        if let Some(status) = status {
            assert!(redis_test_command(
                &config,
                &[
                    "HSET".into(),
                    state_key.clone(),
                    "failure_status".into(),
                    status.into(),
                ],
            )
            .await
            .starts_with(':'));
        }
        control_state_keys.push(state_key);
    }

    let unrelated_route_health_key = redis_route_health_key(&config, "key:unrelated");
    assert!(redis_test_command(
        &config,
        &[
            "HSET".into(),
            unrelated_route_health_key.clone(),
            "failure_class".into(),
            "transient_server".into(),
        ],
    )
    .await
    .starts_with(':'));
    assert_eq!(
        redis_integer(
            &redis_test_command(
                &config,
                &[
                    "ZADD".into(),
                    redis_route_health_key(&config, "index:keys"),
                    "1".into(),
                    unrelated_route_health_key.clone(),
                ],
            )
            .await,
        ),
        1
    );

    let unrelated = [
        (
            format!(
                "{}:v1:upstream:{{unrelated}}:leases",
                config.redis_key_prefix
            ),
            "lease-marker",
        ),
        (
            format!(
                "{}:v1:account:{{unrelated}}:waiter",
                config.redis_key_prefix
            ),
            "waiter-marker",
        ),
        (
            format!(
                "{}:v1:upstream:{{unrelated}}:quota",
                config.redis_key_prefix
            ),
            "quota-marker",
        ),
    ];
    for (key, value) in &unrelated {
        assert_eq!(
            redis_test_command(&config, &["SET".into(), key.clone(), (*value).into()],).await,
            "+OK\r\n"
        );
    }

    let upstream = UpstreamConfig {
        id: upstream_id.into(),
        name: "Startup legacy repair".into(),
        api_key: api_key.into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        active: true,
        ..UpstreamConfig::default()
    };
    let before_repair = state
        .route_health_snapshots(std::slice::from_ref(&upstream))
        .await
        .unwrap();
    let before_repair = &before_repair[upstream_id];
    assert_eq!(before_repair.legacy_local_admission_poisoned_routes, 1);
    let serialized = serde_json::to_string(before_repair).unwrap();
    assert!(!serialized.contains(api_key));
    assert!(!serialized.contains(&fingerprint));

    let report = state
        .repair_legacy_local_admission_route_health()
        .await
        .unwrap();

    assert_eq!(report.scanned_routes, 4);
    assert_eq!(report.repaired_routes, 1);
    assert_eq!(
        state
            .route_health_snapshots(std::slice::from_ref(&upstream))
            .await
            .unwrap()[upstream_id]
            .legacy_local_admission_poisoned_routes,
        0
    );
    assert_eq!(
        redis_integer(
            &redis_test_command(
                &config,
                &[
                    "EXISTS".into(),
                    redis_route_health_route_state_key(&config, &legacy_route),
                ],
            )
            .await,
        ),
        0
    );
    for state_key in control_state_keys {
        assert_eq!(
            redis_integer(&redis_test_command(&config, &["EXISTS".into(), state_key]).await,),
            1
        );
    }
    assert_eq!(
        redis_integer(
            &redis_test_command(&config, &["EXISTS".into(), unrelated_route_health_key],).await,
        ),
        1
    );
    for (key, expected) in unrelated {
        assert_eq!(
            redis_bulk_string(&redis_test_command(&config, &["GET".into(), key]).await),
            expected
        );
    }
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn app_state_load_repairs_legacy_local_admission_route_health() {
    let config = redis_test_config();
    let (state, _second, directory) = redis_test_states(&config).await;
    let upstream_id = "startup-load-legacy-repair-upstream";
    let fingerprint = "fingerprint-a";
    let legacy_route = redis_test_health_route(upstream_id, fingerprint, "legacy-local");
    let provider_route = redis_test_health_route(upstream_id, fingerprint, "provider-503");

    state
        .observe_route_failure(
            &legacy_route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(86_000_000)),
            false,
        )
        .await
        .unwrap();
    state
        .observe_route_failure(
            &provider_route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(86_000_000)),
            false,
        )
        .await
        .unwrap();
    let provider_state_key = redis_route_health_route_state_key(&config, &provider_route);
    assert!(redis_test_command(
        &config,
        &[
            "HSET".into(),
            provider_state_key.clone(),
            "failure_status".into(),
            "503".into(),
        ],
    )
    .await
    .starts_with(':'));

    AppState::load_from_path(directory.path().join("startup.json"), config.clone())
        .await
        .unwrap();

    assert_eq!(
        redis_integer(
            &redis_test_command(
                &config,
                &[
                    "EXISTS".into(),
                    redis_route_health_route_state_key(&config, &legacy_route),
                ],
            )
            .await,
        ),
        0
    );
    assert_eq!(
        redis_integer(&redis_test_command(&config, &["EXISTS".into(), provider_state_key]).await,),
        1
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_earliest_route_recovery_uses_shared_health() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let route = redis_test_health_route("shared-recovery-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::TransientServer,
            Some(Duration::from_secs(5)),
            false,
        )
        .await
        .unwrap();

    let recovery = second
        .earliest_temporary_route_recovery(std::slice::from_ref(&route))
        .await
        .unwrap()
        .expect("the second coordinator must see the shared recovery");
    assert_eq!(recovery.class, RouteFailureClass::TransientServer);
    assert!(recovery.retry_after > Duration::ZERO);
    assert!(recovery.retry_after <= Duration::from_secs(10));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_transient_route_cooldown_uses_configured_base_and_max() {
    let mut config = redis_test_config();
    config.upstream_transient_route_cooldown_base_seconds = 1;
    config.upstream_transient_route_cooldown_max_seconds = 1;
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("configured-cooldown-upstream", "fingerprint-a");
    let route = redis_test_health_route("configured-cooldown-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    let recovery = second
        .earliest_temporary_route_recovery(std::slice::from_ref(&route))
        .await
        .unwrap()
        .expect("the configured Redis cooldown must be shared");
    assert!(recovery.retry_after > Duration::ZERO);
    assert!(recovery.retry_after <= Duration::from_secs(1));

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit after configured cooldown, got {other:?}"),
    };
    permit
        .finish(RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: None,
            repeat_within_request: false,
            sole_candidate: false,
            capacity_sole_route: false,
            shared_host_failure_domain: false,
        })
        .await
        .unwrap();
    let recovery = second
        .earliest_temporary_route_recovery(std::slice::from_ref(&route))
        .await
        .unwrap()
        .expect("permit completion must reuse the configured Redis cooldown");
    assert!(recovery.retry_after > Duration::ZERO);
    assert!(recovery.retry_after <= Duration::from_secs(1));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_ttl_outlives_long_configured_cooldown() {
    let mut config = redis_test_config();
    config.upstream_transient_route_cooldown_base_seconds = 7_201;
    config.upstream_transient_route_cooldown_max_seconds = 7_201;
    let (state, _second, _directory) = redis_test_states(&config).await;
    let route = redis_test_health_route("long-cooldown-upstream", "fingerprint-a", "model-a");

    state
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();

    let state_key = redis_route_health_state_key(&config).await;
    let ttl_seconds = redis_integer(&redis_test_command(&config, &["TTL".into(), state_key]).await);
    assert!(
        ttl_seconds >= 7_260,
        "Redis route-health TTL {ttl_seconds}s must outlive the 7201s cooldown"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_admin_route_health_snapshots_use_shared_health() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let upstream = UpstreamConfig {
        id: "shared-admin-health-upstream".into(),
        name: "Shared admin health upstream".into(),
        api_key: "snapshot-secret".into(),
        api_key_models: vec![ApiKeyModelConfig {
            api_key: "snapshot-secret".into(),
            supported_models: vec!["model-a".into()],
        }],
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["model-a".into()],
        active: true,
        ..UpstreamConfig::default()
    };
    let route = redis_test_health_route(
        &upstream.id,
        &upstream_key_fingerprint(&upstream.id, "snapshot-secret"),
        "model-a",
    );

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();

    let snapshots = second
        .route_health_snapshots(std::slice::from_ref(&upstream))
        .await
        .unwrap();
    let snapshot = &snapshots[&upstream.id];
    assert_eq!(snapshot.healthy_routes, 0);
    assert_eq!(snapshot.cooldown_routes, 1);
    assert_eq!(snapshot.half_open_routes, 0);
    assert_eq!(snapshot.failure_classes["transient_server"], 1);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_reconcile_removes_unconfigured_state() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let route = redis_test_health_route("removed-health-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    assert!(second
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .is_some());

    second.reconcile_route_health(&[]).await.unwrap();

    assert!(first.route_health_snapshot(&route).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_reconcile_defers_active_lease_then_removes_on_finish() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("removed-active-upstream", "fingerprint-a");
    let route = redis_test_health_route("removed-active-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(20)),
            false,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };

    second.reconcile_route_health(&[]).await.unwrap();
    assert!(second
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .is_some_and(|snapshot| snapshot.half_open));

    permit
        .finish(RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: None,
            repeat_within_request: false,
            sole_candidate: false,
            capacity_sole_route: false,
            shared_host_failure_domain: false,
        })
        .await
        .unwrap();
    assert!(first.route_health_snapshot(&route).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_repeated_transient_failure_within_same_request_keeps_step_flat() {
    // A1 Redis backend: the Lua observe() must mirror failure_step and keep
    // the step flat for repeat failures of the same downstream request.
    let mut config = redis_test_config();
    config.upstream_transient_route_cooldown_base_seconds = 1;
    config.upstream_transient_route_cooldown_max_seconds = 1;
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("request-suppressed-upstream", "fingerprint-a");
    let route = redis_test_health_route("request-suppressed-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    let first_step = first
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("first failure must create route health state");
    assert_eq!(first_step.consecutive_failures, 1);

    for round in 2..=3 {
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
            RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
            other => panic!("expected half-open permit in round {round}, got {other:?}"),
        };
        permit
            .finish(RouteOutcome::RouteFailure {
                class: RouteFailureClass::TransientServer,
                upstream_status: None,
                repeat_within_request: true,
                sole_candidate: false,
                capacity_sole_route: false,
                shared_host_failure_domain: false,
            })
            .await
            .unwrap();
        let snapshot = second
            .route_health_snapshot(&route)
            .await
            .unwrap()
            .expect("repeat failure must keep route health state");
        assert_eq!(
            snapshot.consecutive_failures, 1,
            "round {round} of the same request must not escalate the failure step"
        );
    }
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_independent_request_failures_still_escalate_the_step() {
    let mut config = redis_test_config();
    config.upstream_transient_route_cooldown_base_seconds = 1;
    config.upstream_transient_route_cooldown_max_seconds = 1;
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("independent-escalation-upstream", "fingerprint-a");
    let route = redis_test_health_route(
        "independent-escalation-upstream",
        "fingerprint-a",
        "model-a",
    );

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    permit
        .finish(RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: None,
            repeat_within_request: false,
            sole_candidate: false,
            capacity_sole_route: false,
            shared_host_failure_domain: false,
        })
        .await
        .unwrap();
    let snapshot = second
        .route_health_snapshot(&route)
        .await
        .unwrap()
        .expect("second failure must keep route health state");
    assert_eq!(
        snapshot.consecutive_failures, 2,
        "an independent request failure must escalate the step"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_reconcile_removes_expired_half_open_lease() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("removed-expired-upstream", "fingerprint-a");
    let route = redis_test_health_route("removed-expired-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(20)),
            false,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let abandoned = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    std::mem::forget(abandoned);
    let state_key = redis_route_health_state_key(&config).await;
    assert!(redis_test_command(
        &config,
        &[
            "HSET".into(),
            state_key,
            "half_open_expires_at_ms".into(),
            "1".into(),
        ],
    )
    .await
    .starts_with(':'));

    second.reconcile_route_health(&[]).await.unwrap();
    assert!(first.route_health_snapshot(&route).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_global_eviction_removes_the_owners_upstream_index() {
    let config = redis_test_config();
    let old_state = redis_route_health_key(&config, "route:old");
    let new_state = redis_route_health_key(&config, "route:new");
    let old_upstream_index = redis_route_health_key(&config, "upstream:old:index:routes");
    let new_upstream_index = redis_route_health_key(&config, "upstream:new:index:routes");
    let global_index = redis_route_health_key(&config, "index:routes");
    let generation = redis_route_health_key(&config, "generation");

    assert!(redis_test_command(
        &config,
        &[
            "HSET".into(),
            old_state.clone(),
            "failure_class".into(),
            "transient_server".into(),
            "upstream_index_key".into(),
            old_upstream_index.clone(),
        ],
    )
    .await
    .starts_with(':'));
    for index in [&old_upstream_index, &global_index] {
        assert_eq!(
            redis_integer(
                &redis_test_command(
                    &config,
                    &[
                        "ZADD".into(),
                        index.to_string(),
                        "1".into(),
                        old_state.clone()
                    ],
                )
                .await,
            ),
            1
        );
    }

    let response = redis_test_command(
        &config,
        &[
            "EVAL".into(),
            include_str!("../src/state/redis_runtime/route_health_observe.lua").into(),
            "4".into(),
            new_state,
            new_upstream_index,
            global_index,
            generation,
            "observe".into(),
            "route".into(),
            "transient_server".into(),
            "-1".into(),
            "600000".into(),
            "7200".into(),
            "1".into(),
            "1".into(),
            "new-upstream".into(),
            "fingerprint".into(),
            "model-a".into(),
            "responses".into(),
            "0".into(),
            "".into(),
            "1".into(),
            "1000".into(),
            // T1.3: max_step appended after the variable-length probe schedule.
            "3".into(),
        ],
    )
    .await;
    assert_eq!(redis_integer(&response), 1);
    assert_eq!(
        redis_integer(&redis_test_command(&config, &["ZCARD".into(), old_upstream_index]).await,),
        0
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_finish_rejects_capacity_exhaustion_and_corrupt_markers() {
    let config = redis_test_config();
    let key_state = redis_route_health_key(&config, "key:new");
    let route_state = redis_route_health_key(&config, "route:new");
    let key_upstream_index = redis_route_health_key(&config, "upstream:new:index:keys");
    let route_upstream_index = redis_route_health_key(&config, "upstream:new:index:routes");
    let key_global_index = redis_route_health_key(&config, "index:keys");
    let route_global_index = redis_route_health_key(&config, "index:routes");
    let generation = redis_route_health_key(&config, "generation");
    let marker = redis_route_health_key(&config, "finish:lease");
    let active_state = redis_route_health_key(&config, "route:active");
    let active_upstream_index = redis_route_health_key(&config, "upstream:active:index:routes");
    assert!(redis_test_command(
        &config,
        &[
            "HSET".into(),
            active_state.clone(),
            "failure_class".into(),
            "transient_server".into(),
            "half_open_lease".into(),
            "active-owner".into(),
            "upstream_index_key".into(),
            active_upstream_index,
        ],
    )
    .await
    .starts_with(':'));
    assert_eq!(
        redis_integer(
            &redis_test_command(
                &config,
                &[
                    "ZADD".into(),
                    route_global_index.clone(),
                    "1".into(),
                    active_state,
                ],
            )
            .await,
        ),
        1
    );

    let finish_arguments = vec![
        "EVAL".into(),
        include_str!("../src/state/redis_runtime/route_health_finish.lua").into(),
        "8".into(),
        key_state,
        route_state,
        key_upstream_index,
        route_upstream_index,
        key_global_index,
        route_global_index,
        generation,
        marker.clone(),
        "lease".into(),
        "".into(),
        "".into(),
        "".into(),
        "route_failure".into(),
        "transient_server".into(),
        "-1".into(),
        "600000".into(),
        "7200".into(),
        "1".into(),
        "1".into(),
        "new-upstream".into(),
        "fingerprint".into(),
        "model-a".into(),
        "responses".into(),
        "".into(),
        "0".into(),
        "1".into(),
        "1000".into(),
        "0".into(),
        "0".into(),
        // T1.3: max_step appended after the variable-length schedules.
        "3".into(),
    ];
    assert_eq!(
        redis_integer(&redis_test_command(&config, &finish_arguments).await),
        -1
    );

    assert_eq!(
        redis_test_command(&config, &["SET".into(), marker, "2".into()]).await,
        "+OK\r\n"
    );
    let response = redis_test_command(&config, &finish_arguments).await;
    assert!(response.starts_with('-'), "unexpected response: {response}");
    assert!(response.contains("invalid route health finish marker"));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_half_open_reserve_refreshes_route_index_ttls() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let upstream_id = "route-index-ttl-upstream";
    let route = redis_test_health_route(upstream_id, "fingerprint-a", "model-a");
    let key = redis_test_health_key(upstream_id, "fingerprint-a");
    let upstream_identity = format!("{:x}", Sha256::digest(upstream_id.as_bytes()));
    let upstream_index = format!(
        "{}:v1:route-health:{{route-health}}:upstream:{upstream_identity}:index:routes",
        config.redis_key_prefix
    );
    let global_index = format!(
        "{}:v1:route-health:{{route-health}}:index:routes",
        config.redis_key_prefix
    );

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(20)),
            false,
        )
        .await
        .unwrap();
    for index in [&upstream_index, &global_index] {
        assert_eq!(
            redis_integer(
                &redis_test_command(&config, &["EXPIRE".into(), index.to_string(), "1".into()],)
                    .await,
            ),
            1
        );
    }
    tokio::time::sleep(Duration::from_millis(30)).await;

    let permit = match second.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };

    for index in [&upstream_index, &global_index] {
        let ttl =
            redis_integer(&redis_test_command(&config, &["TTL".into(), index.to_string()]).await);
        assert!(ttl > 60, "route index TTL was not refreshed: {ttl}");
    }
    permit.finish(RouteOutcome::Cancelled).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_stale_finish_cannot_clear_a_newer_failure() {
    let mut config = redis_test_config();
    // E1 defaults capacity failures to observation-only. This test needs a
    // real RateLimited cooldown so it can verify a stale half-open success
    // does not clear a newer failure generation.
    config.upstream_capacity_failure_cooldown_enabled = true;
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("stale-health-upstream", "fingerprint-a");
    let route = redis_test_health_route("stale-health-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(20)),
            false,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let stale = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };

    second
        .observe_route_failure(
            &route,
            RouteFailureClass::RateLimited,
            Some(Duration::from_secs(30)),
            false,
        )
        .await
        .unwrap();
    stale.finish(RouteOutcome::Success).await.unwrap();

    assert!(matches!(
        first.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Cooling {
            class: RouteFailureClass::RateLimited,
            ..
        }
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_key_and_aggregate_health_are_shared_without_overblocking_routes() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let blocked_key = redis_test_health_key("scoped-health-upstream", "fingerprint-a");
    let blocked_route =
        redis_test_health_route("scoped-health-upstream", "fingerprint-a", "model-a");
    let blocked_other_model =
        redis_test_health_route("scoped-health-upstream", "fingerprint-a", "model-b");
    let healthy_key = redis_test_health_key("scoped-health-upstream", "fingerprint-b");
    let healthy_route =
        redis_test_health_route("scoped-health-upstream", "fingerprint-b", "model-a");
    let aggregate = RouteSetAggregateKey {
        upstream_id: "scoped-health-upstream".into(),
        runtime_model_slug: "model-a".into(),
        protocol: WireProtocol::Responses,
    };

    first
        .observe_key_failure(
            &blocked_key,
            RouteFailureClass::Credentials,
            Some(Duration::from_secs(1)),
        )
        .await
        .unwrap();
    assert!(matches!(
        second
            .reserve_route_health(&blocked_route, &blocked_key)
            .await
            .unwrap(),
        RouteAvailability::Cooling {
            class: RouteFailureClass::Credentials,
            ..
        }
    ));
    assert!(matches!(
        second
            .reserve_route_health(&blocked_other_model, &blocked_key)
            .await
            .unwrap(),
        RouteAvailability::Cooling { .. }
    ));

    first
        .observe_route_set_failure(
            &aggregate,
            RouteFailureClass::RateLimited,
            Some(Duration::from_secs(7)),
        )
        .await
        .unwrap();
    let aggregate_snapshot = second
        .route_set_health_snapshot(&aggregate)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(aggregate_snapshot.consecutive_failures, 1);
    assert_eq!(
        aggregate_snapshot.last_failure_class,
        Some(RouteFailureClass::RateLimited)
    );
    assert!(matches!(
        second
            .reserve_route_health(&healthy_route, &healthy_key)
            .await
            .unwrap(),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_credentials_first_strike_cools_short_then_escalates() {
    // T5 mirror of the local registry test: the key cooldown schedule is
    // precomputed Rust-side, so the first Credentials strike uses the
    // ~60s first-strike window and the second escalates to the 15min curve.
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("first-strike-upstream", "fingerprint-a");
    let route = redis_test_health_route("first-strike-upstream", "fingerprint-a", "model-a");

    state
        .observe_key_failure(&key, RouteFailureClass::Credentials, None)
        .await
        .unwrap();
    let first = match state.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Cooling { retry_after, .. } => retry_after,
        other => panic!("expected key cooling after first strike, got {other:?}"),
    };
    assert!(
        first >= Duration::from_secs(48) && first <= Duration::from_secs(72),
        "first credential strike should cool ~60s on Redis too, got {first:?}"
    );

    state
        .observe_key_failure(&key, RouteFailureClass::Credentials, None)
        .await
        .unwrap();
    let second = match state.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Cooling { retry_after, .. } => retry_after,
        other => panic!("expected key cooling after second strike, got {other:?}"),
    };
    assert!(
        second >= Duration::from_secs(24 * 60) && second <= Duration::from_secs(36 * 60),
        "second credential strike should escalate to ~30min on Redis too, got {second:?}"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_cancelled_half_open_reapplies_concurrency_probe_delay() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("cancelled-health-upstream", "fingerprint-a");
    let route = redis_test_health_route("cancelled-health-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::ConcurrencySaturated, None, false)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(110)).await;
    let permit = match second.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    permit.finish(RouteOutcome::Cancelled).await.unwrap();

    let snapshot = first.route_health_snapshot(&route).await.unwrap().unwrap();
    assert_eq!(
        snapshot.last_failure_class,
        Some(RouteFailureClass::ConcurrencySaturated)
    );
    assert!(snapshot.cooldown_remaining > Duration::ZERO);
    assert!(snapshot.cooldown_remaining <= Duration::from_millis(100));
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_expired_half_open_owner_can_be_reclaimed() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("expired-owner-upstream", "fingerprint-a");
    let route = redis_test_health_route("expired-owner-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(
            &route,
            RouteFailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(20)),
            false,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let abandoned = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    std::mem::forget(abandoned);

    let response = redis_test_command(
        &config,
        &[
            "HSET".into(),
            redis_route_health_state_key(&config).await,
            "half_open_expires_at_ms".into(),
            "1".into(),
        ],
    )
    .await;
    assert!(response.starts_with(':'));

    let replacement = match second.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected expired owner to be reclaimed, got {other:?}"),
    };
    replacement.finish(RouteOutcome::Success).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_generation_does_not_reset_after_clear() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let route = redis_test_health_route("generation-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    let state_key = redis_route_health_state_key(&config).await;
    let first_generation = redis_bulk_u64(
        &redis_test_command(
            &config,
            &["HGET".into(), state_key.clone(), "state_generation".into()],
        )
        .await,
    );

    first.clear_route_health(&route).await.unwrap();
    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    let second_generation = redis_bulk_u64(
        &redis_test_command(
            &config,
            &["HGET".into(), state_key, "state_generation".into()],
        )
        .await,
    );

    assert!(second_generation > first_generation);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_route_health_finish_retry_is_idempotent() {
    let config = redis_test_config();
    let (first, _second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("finish-retry-upstream", "fingerprint-a");
    let route = redis_test_health_route("finish-retry-upstream", "fingerprint-a", "model-a");
    let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) => permit,
        other => panic!("expected healthy permit, got {other:?}"),
    };

    coordination_fault(&first).lose_next_responses(1);
    permit
        .finish(RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: None,
            repeat_within_request: false,
            sole_candidate: false,
            capacity_sole_route: false,
            shared_host_failure_domain: false,
        })
        .await
        .unwrap();

    let snapshot = first.route_health_snapshot(&route).await.unwrap().unwrap();
    assert_eq!(snapshot.consecutive_failures, 1);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_targeted_model_sync_retains_pending_cleanup_until_coordination_recovers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let discovery_server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/models",
                get(|| async {
                    Json(serde_json::json!({
                        "object": "list",
                        "data": [{"id": "model-a", "object": "model"}]
                    }))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let mut config = redis_test_config();
    config.upstream_model_auto_discovery_enabled = true;
    config.upstream_model_key_sync_interval_seconds = 900;
    let directory = tempdir().unwrap();
    let state = AppState::load_from_path(directory.path().join("targeted.json"), config.clone())
        .await
        .unwrap();
    let api_key = "targeted-secret";
    let upstream_id = "targeted-redis-upstream";
    state
        .insert_upstream(UpstreamConfig {
            id: upstream_id.into(),
            name: "Targeted Redis upstream".into(),
            base_url: format!("http://{address}"),
            api_key: api_key.into(),
            api_key_models: vec![ApiKeyModelConfig {
                api_key: api_key.into(),
                supported_models: vec!["model-a".into()],
            }],
            supported_models: vec!["model-a".into()],
            protocol: UpstreamProtocol::Responses,
            protocols: vec![UpstreamProtocol::Responses],
            active: true,
            ..UpstreamConfig::default()
        })
        .await
        .unwrap();
    let fingerprint = upstream_key_fingerprint(upstream_id, api_key);
    let route = redis_test_health_route(upstream_id, &fingerprint, "model-a");
    state
        .observe_route_failure(&route, RouteFailureClass::ModelUnsupported, None, false)
        .await
        .unwrap();
    let worker = ModelKeySyncService::spawn(state.clone()).expect("model sync enabled");

    coordination_fault(&state).arm_outage(true);
    assert!(state.submit_targeted_model_discovery(upstream_id, &fingerprint, "model-a"));
    tokio::time::sleep(Duration::from_millis(4_500)).await;
    assert_eq!(state.targeted_model_discovery_pending_count(), 1);

    coordination_fault(&state).arm_outage(false);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if state.targeted_model_discovery_pending_count() == 0
                && state
                    .route_health_snapshot(&route)
                    .await
                    .is_ok_and(|snapshot| snapshot.is_none())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("pending route-health cleanup should finish after Redis recovers");

    worker.abort();
    discovery_server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_coordinated_probe_plan_reserves_and_releases_upstream_capacity() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = hits.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let hits = hits_clone.clone();
            async move {
                let (_, body) = request.into_parts();
                let _payload: Value =
                    serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                hits.fetch_add(1, Ordering::SeqCst);
                Json(json!({
                    "id": "chatcmpl-probe",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "probe-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }]
                }))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("redis-coordinated-probe");
    state.insert_upstream(upstream.clone()).await.unwrap();

    let plan = chat_responses_codex::server::CapabilityProbePlan {
        protocol: WireProtocol::ChatCompletions,
        cases: vec![
            chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false },
            chat_responses_codex::server::CoreProbeCase::ReasoningControl {
                field: "reasoning_effort".into(),
                value: "high".into(),
            },
        ],
        output_token_cap: 16,
    };

    let (outcome, completeness) =
        chat_responses_codex::server::run_probe_plan_with_coordination_for_test(
            &format!("http://{address}"),
            "probe-secret",
            "model-a",
            plan,
            5,
            Some(state.clone()),
            Some(upstream.clone()),
        )
        .await
        .unwrap();

    assert_eq!(
        completeness,
        chat_responses_codex::server::ProbePlanCompleteness::Full
    );
    assert!(matches!(
        outcome,
        chat_responses_codex::capabilities::ProbeOutcome::Conclusive { .. }
    ));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        3,
        "both cases must reach the upstream under coordination; the reasoning          case streams first and, against this non-SSE JSON fake upstream,          falls back to one non-stream request (see capability_probe.rs          ReasoningControl), so MinimalText (1) + reasoning stream + fallback (2)          = 3 hits"
    );

    // Every probe case reserves a Redis lease for its request and releases it
    // when the request finishes; nothing may leak in-flight capacity.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .remove("redis-coordinated-probe")
        .expect("upstream snapshot must exist");
    assert_eq!(snapshot.in_flight, 0, "probe leases must all be released");
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_lease_uses_short_ttl_not_upstream_stream_duration() {
    let config = AppConfig {
        downstream_lease_ttl_seconds: 300,
        ..redis_test_config()
    };
    let (state, _second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("down-lease-ttl");
    let lease = state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();

    let key = redis_downstream_key(&config, "down-lease-ttl", "leases");
    let members = redis_test_command(
        &config,
        &["ZRANGE".into(), key.clone(), "0".into(), "-1".into()],
    )
    .await;
    // RESP array of bulk strings: *1\r\n$36\r\n<uuid>\r\n
    let lease_id = members
        .split("\r\n")
        .nth(2)
        .expect("ZRANGE must return the reserved lease id")
        .to_string();
    let score_raw = redis_test_command(&config, &["ZSCORE".into(), key, lease_id.clone()]).await;
    // RESP bulk string: $13\r\n1786070635555\r\n
    let score_ms: i64 = score_raw
        .split("\r\n")
        .nth(1)
        .expect("ZSCORE must return a bulk string")
        .parse()
        .expect("ZSCORE must return an integer");

    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    // RESP array of bulk strings: *2\r\n$10\r\n1786069805\r\n$6\r\n499055\r\n
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: i64 = time_parts[2].parse().expect("TIME seconds");
    let micros: i64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1000 + micros / 1000;

    let remaining_ms = score_ms - now_ms;
    // Downstream lease must use the short configured TTL (~300s), NOT the
    // upstream stream max duration (24h + 60s) that leaks ghost leases after
    // a gateway restart.
    assert!(
        (290_000..=320_000).contains(&remaining_ms),
        "downstream lease should expire with the short TTL (~300s), got {remaining_ms}ms"
    );
    // Keep the lease alive until after the assertions; dropping it releases
    // the Redis lease and would make the ZSET empty.
    drop(lease);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_lease_uses_local_ttl_not_stream_duration() {
    let config = AppConfig {
        upstream_local_lease_ttl_seconds: 300,
        upstream_stream_max_duration_seconds: 86_400,
        ..redis_test_config()
    };
    let (state, _second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("upstream-lease-ttl");
    state.insert_upstream(upstream.clone()).await.unwrap();
    let fingerprint = "fingerprint-upstream-ttl";
    let lease = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();
    let account = AccountConcurrencyKey::new(upstream.id.clone(), fingerprint);
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let account_identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    let lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:leases",
        config.redis_key_prefix
    );
    let lease_id = redis_test_command(
        &config,
        &["ZRANGE".into(), lease_key.clone(), "0".into(), "0".into()],
    )
    .await
    .split("\r\n")
    .nth(2)
    .expect("ZRANGE must return the reserved upstream lease id")
    .to_string();
    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: i64 = time_parts[2].parse().expect("TIME seconds");
    let micros: i64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1_000 + micros / 1_000;
    let score_ms: i64 = redis_test_command(&config, &["ZSCORE".into(), lease_key, lease_id])
        .await
        .split("\r\n")
        .nth(1)
        .expect("ZSCORE must return a bulk string")
        .parse()
        .expect("ZSCORE must return an integer");
    let remaining_ms = score_ms - now_ms;
    assert!(
        (290_000..=320_000).contains(&remaining_ms),
        "upstream lease must use local TTL (~300s), got {remaining_ms}ms"
    );
    state.release_upstream_request(lease).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_snapshot_reports_real_stale_and_oldest_lease_state() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("upstream-snapshot-stale");
    let fingerprint = "fingerprint-snapshot-stale";
    state.insert_upstream(upstream.clone()).await.unwrap();
    let lease = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();
    let account = AccountConcurrencyKey::new(upstream.id.clone(), fingerprint);
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let account_identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    let lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:leases",
        config.redis_key_prefix
    );
    let lease_id = redis_test_command(
        &config,
        &["ZRANGE".into(), lease_key.clone(), "0".into(), "0".into()],
    )
    .await
    .split("\r\n")
    .nth(2)
    .expect("ZRANGE must return the reserved upstream lease id")
    .to_string();
    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1_000 + micros / 1_000;
    let aggregate_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:leases",
        config.redis_key_prefix
    );
    // Simulate a lease whose last heartbeat was 250s ago with a 300s TTL:
    // expiry is now + 50s (still live), last renewal 250s > stale_after 200s.
    for key in [lease_key, aggregate_key] {
        redis_test_command(
            &config,
            &[
                "ZADD".into(),
                key,
                (now_ms + 50_000).to_string(),
                lease_id.clone(),
            ],
        )
        .await;
    }

    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get(&upstream.id)
        .cloned()
        .expect("upstream snapshot must exist");
    assert_eq!(
        snapshot.stale_lease_count, 1,
        "a lease heartbeated 250s ago must be counted as stale"
    );
    assert!(
        (250..=251).contains(&snapshot.oldest_lease_age_seconds),
        "oldest lease age must reflect the 250s heartbeat gap, got {}",
        snapshot.oldest_lease_age_seconds
    );
    assert_eq!(
        snapshot.stale_reclaimed_total, 1,
        "the snapshot must reclaim (and count) the stale lease in the same pass"
    );
    assert_eq!(
        snapshot.in_flight, 0,
        "the reclaimed stale lease must no longer count as in flight"
    );
    assert_eq!(
        snapshot.route_cooldown_skipped_total, None,
        "Redis backend must report cooldown skips as unsupported, not 0"
    );
    assert_eq!(snapshot.hold_p50_ms, None);
    assert_eq!(snapshot.hold_p95_ms, None);
    state.release_upstream_request(lease).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_stale_lease_is_reclaimed_before_ttl() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let mut upstream = redis_test_upstream("upstream-stale-reclaim");
    upstream.max_concurrency = 1;
    let fingerprint = "fingerprint-stale-reclaim";
    state.insert_upstream(upstream.clone()).await.unwrap();
    let account = AccountConcurrencyKey::new(upstream.id.clone(), fingerprint);
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let account_identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    let lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:leases",
        config.redis_key_prefix
    );
    let aggregate_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:leases",
        config.redis_key_prefix
    );
    let _held = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();
    let held_id = redis_test_command(
        &config,
        &["ZRANGE".into(), lease_key.clone(), "0".into(), "-1".into()],
    )
    .await
    .split("\r\n")
    .nth(2)
    .expect("ZRANGE must return the reserved upstream lease id")
    .to_string();
    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1_000 + micros / 1_000;
    // Age the lease 250s (300s TTL, stale after 200s): still live for 50s,
    // but already past the stale window.  Admission must reclaim it instead
    // of waiting for the TTL.
    for key in [lease_key.clone(), aggregate_key] {
        redis_test_command(
            &config,
            &[
                "ZADD".into(),
                key,
                (now_ms + 50_000).to_string(),
                held_id.clone(),
            ],
        )
        .await;
    }

    let replacement = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .expect("the stale lease must be reclaimed before the TTL expires");
    let gone = redis_test_command(&config, &["ZSCORE".into(), lease_key, held_id]).await;
    assert!(
        gone.starts_with("$-1"),
        "the stale lease must be removed from the ZSET"
    );
    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get(&upstream.id)
        .cloned()
        .expect("upstream snapshot must exist");
    assert_eq!(snapshot.stale_reclaimed_total, 1);
    assert_eq!(snapshot.in_flight, 1);
    state.release_upstream_request(replacement).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_snapshot_counts_capacity_rejections_and_leaked_reclaims() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let mut upstream = redis_test_upstream("upstream-snapshot-counters");
    upstream.max_concurrency = 1;
    let fingerprint = "fingerprint-snapshot-counters";
    state.insert_upstream(upstream.clone()).await.unwrap();
    let account = AccountConcurrencyKey::new(upstream.id.clone(), fingerprint);
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let account_identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    let lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:leases",
        config.redis_key_prefix
    );
    let aggregate_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:leases",
        config.redis_key_prefix
    );

    let first = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();
    let rejection = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .expect_err("the second request must be rejected by the Redis gate");
    assert!(!rejection.is_runtime_coordination_unavailable());
    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get(&upstream.id)
        .cloned()
        .expect("upstream snapshot must exist");
    assert_eq!(
        snapshot.capacity_reject_total, 1,
        "the Lua admission-gate rejection must be counted"
    );
    state.release_upstream_request(first).await.unwrap();

    // Park an expired ghost lease in the account + aggregate ZSETs; the next
    // admission must lazily prune it and count it as a leaked reclaim.
    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1_000 + micros / 1_000;
    for key in [lease_key.clone(), aggregate_key] {
        redis_test_command(
            &config,
            &[
                "ZADD".into(),
                key,
                (now_ms - 1_000).to_string(),
                "ghost-lease".into(),
            ],
        )
        .await;
    }
    let replacement = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .expect("the expired ghost lease must be pruned before admission");
    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get(&upstream.id)
        .cloned()
        .expect("upstream snapshot must exist");
    assert_eq!(
        snapshot.leaked_reclaimed_total, 1,
        "the expired ghost lease must be counted as a leaked reclaim"
    );
    state.release_upstream_request(replacement).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_upstream_snapshot_counts_expired_aggregate_reclaims() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let upstream = redis_test_upstream("upstream-snapshot-expired-reclaim");
    state.insert_upstream(upstream.clone()).await.unwrap();
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let aggregate_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:leases",
        config.redis_key_prefix
    );
    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let now_ms = seconds * 1_000 + micros / 1_000;
    redis_test_command(
        &config,
        &[
            "ZADD".into(),
            aggregate_key.clone(),
            (now_ms - 1_000).to_string(),
            "snapshot-expired-lease".into(),
        ],
    )
    .await;

    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get(&upstream.id)
        .cloned()
        .expect("upstream snapshot must exist");
    assert_eq!(snapshot.in_flight, 0);
    assert_eq!(
        snapshot.leaked_reclaimed_total, 1,
        "snapshot expiry sweep must count the reclaimed aggregate lease"
    );
    assert!(
        redis_test_command(
            &config,
            &[
                "ZSCORE".into(),
                aggregate_key,
                "snapshot-expired-lease".into(),
            ],
        )
        .await
        .starts_with("$-1"),
        "snapshot expiry sweep must remove the expired aggregate lease"
    );
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_reclaiming_one_lease_in_snapshot_and_admission_counts_once() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let mut upstream = redis_test_upstream("upstream-reclaim-count-idempotence");
    upstream.max_concurrency = 1;
    let fingerprint = "fingerprint-reclaim-count-idempotence";
    state.insert_upstream(upstream.clone()).await.unwrap();
    let account = AccountConcurrencyKey::new(upstream.id.clone(), fingerprint);
    let upstream_identity = format!("{:x}", Sha256::digest(upstream.id.as_bytes()));
    let account_identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", account.upstream_id, account.key_fingerprint).as_bytes())
    );
    let account_lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:account:{account_identity}:leases",
        config.redis_key_prefix
    );
    let aggregate_lease_key = format!(
        "{}:v1:upstream:{{{upstream_identity}}}:leases",
        config.redis_key_prefix
    );

    let held = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();
    let lease_id = redis_test_command(
        &config,
        &[
            "ZRANGE".into(),
            account_lease_key.clone(),
            "0".into(),
            "0".into(),
        ],
    )
    .await
    .split("\r\n")
    .nth(2)
    .expect("ZRANGE must return the reserved upstream lease id")
    .to_string();
    let time_raw = redis_test_command(&config, &["TIME".into()]).await;
    let time_parts: Vec<&str> = time_raw.split("\r\n").collect();
    let seconds: u64 = time_parts[2].parse().expect("TIME seconds");
    let micros: u64 = time_parts[4].parse().expect("TIME micros");
    let expired_at_ms = seconds * 1_000 + micros / 1_000 - 1_000;
    for key in [account_lease_key, aggregate_lease_key] {
        redis_test_command(
            &config,
            &[
                "ZADD".into(),
                key,
                expired_at_ms.to_string(),
                lease_id.clone(),
            ],
        )
        .await;
    }

    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get(&upstream.id)
        .cloned()
        .expect("upstream snapshot must exist");
    assert_eq!(snapshot.leaked_reclaimed_total, 1);

    let replacement = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .expect("the expired account lease must be reclaimed before admission");
    let snapshot = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get(&upstream.id)
        .cloned()
        .expect("upstream snapshot must exist");
    assert_eq!(
        snapshot.leaked_reclaimed_total, 1,
        "the same lease must not be counted again when its account ZSET is swept"
    );

    state.release_upstream_request(replacement).await.unwrap();
    drop(held);
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_admission_reserves_request_and_lease_atomically() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let downstream = redis_test_downstream("shared-admission-atomic");

    let (reservation, lease) = first
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect("first combined admission must succeed");
    assert!(
        lease.lease_id().is_some(),
        "combined admission must hold a concurrency lease"
    );

    // The second instance must observe both the request slot and the lease.
    let rejection = second
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect_err("second admission must be rejected while lease is held");
    assert!(
        matches!(
            rejection,
            DownstreamAdmissionRejection::PerMinuteLimitExceeded {
                limit: 1,
                used: 1,
                ..
            }
        ),
        "unexpected rejection: {rejection:?}"
    );

    first
        .rollback_downstream_request_reservation(reservation)
        .await
        .unwrap();
    first.release_downstream_concurrency(lease).await.unwrap();
    let (reservation, lease) = second
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect("admission must succeed after rollback and release");
    second
        .rollback_downstream_request_reservation(reservation)
        .await
        .unwrap();
    second.release_downstream_concurrency(lease).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_admission_concurrency_rejection_records_nothing() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("admission-reject-records-nothing");
    downstream.per_minute_limit = 100;
    downstream.request_quota_window_hours = Some(1);
    downstream.request_quota_requests = Some(2);

    let (first_reservation, first_lease) = first
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect("first admission must succeed");

    // With quota 2 and one request recorded, a second admission passes the
    // request checks but must be rejected on the concurrency limit.
    let rejection = second
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect_err("second admission must hit the concurrency limit");
    assert!(
        matches!(
            rejection,
            DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit: 1, .. }
        ),
        "unexpected rejection: {rejection:?}"
    );

    // Atomicity: the rejected admission must NOT have recorded its request
    // event. After the first lease is released, a fresh admission succeeds
    // (quota used is still 1, not 2).
    first
        .release_downstream_concurrency(first_lease)
        .await
        .unwrap();
    let (second_reservation, second_lease) = second
        .reserve_downstream_admission(&downstream, "test-model")
        .await
        .expect("rejected admission must not consume a request-quota slot");
    second
        .rollback_downstream_request_reservation(second_reservation)
        .await
        .unwrap();
    second
        .release_downstream_concurrency(second_lease)
        .await
        .unwrap();
    first
        .rollback_downstream_request_reservation(first_reservation)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_half_open_busy_reports_remaining_dedicated_lease() {
    let mut config = redis_test_config();
    config.upstream_transient_route_cooldown_base_seconds = 1;
    config.upstream_transient_route_cooldown_max_seconds = 1;
    config.upstream_route_health_half_open_ttl_seconds = 2;
    config.upstream_stream_max_duration_seconds = 86_400; // 24h stream lease must NOT apply to half-open
    let (first, second, _directory) = redis_test_states(&config).await;
    let key = redis_test_health_key("half-open-ttl-upstream", "fingerprint-a");
    let route = redis_test_health_route("half-open-ttl-upstream", "fingerprint-a", "model-a");

    first
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let permit = match first.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };

    match second.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::HalfOpenBusy { retry_after, .. } => {
            // 调度语义：busy 时乐观轮询 1s（探针通常数秒内完成）
            assert_eq!(retry_after, Duration::from_secs(1));
        }
        other => panic!("expected half-open busy, got {other:?}"),
    }

    let recovery = second
        .earliest_temporary_route_recovery(std::slice::from_ref(&route))
        .await
        .unwrap()
        .expect("shared half-open recovery must be visible");
    // 调度用乐观 1s 轮询；真实剩余租约在 half_open_remaining 中诚实上报
    assert_eq!(recovery.retry_after, Duration::from_secs(1));
    assert!(
        recovery.half_open_remaining.is_some_and(|remaining| {
            remaining > Duration::from_secs(1) && remaining <= Duration::from_secs(2)
        }),
        "shared recovery must report remaining half-open lease, got {:?}",
        recovery.half_open_remaining
    );

    permit.finish(RouteOutcome::Success).await.unwrap();
}

// ============================================================================
// P2 (2026-08-26 T11): no-Redis drift guard — the three T1.x cooldown
// parameters must be threaded into the Redis backend's Lua scripts/argument
// lists.  The live-Redis suite is `#[ignore]`d and needs TEST_REDIS_URL; this
// plain test runs everywhere and statically locks the threading so a change
// to the local backend can never silently leave the Redis backend behind.
//
// Threading recap (redis_runtime.rs):
//   - T1.2 upstream_retry_after_cooldown_cap_seconds: the gateway clamps the
//     upstream Retry-After BEFORE it reaches the coordinator; the clamped
//     value arrives as `optional_duration_ms(retry_after)` (ARGV explicit
//     retry), and the Lua side does `cooldown_ms = max(cooldown_ms,
//     explicit_retry_ms)` — so a 28s hint can only ever *raise* the schedule,
//     never blow the wait budget.
//   - T1.3 upstream_transient_route_cooldown_max_step: read in
//     `update_runtime_tuning` into `RedisRuntimeTuning`, appended to the
//     finish/observe Lua invocations after the variable-length schedules, and
//     used as `min(step, max_step)` in Lua.
//   - T1.4 upstream_shared_host_failure_domain_enabled: the per-request
//     `shared_host_failure_domain` flag folds into the `repeat_within_request`
//     bit (`repeat_within_request || shared_host_failure_domain`), which the
//     Lua step escalator honors as "do not escalate".
// ============================================================================
#[test]
fn redis_lua_scripts_thread_the_t1_cooldown_parameters() {
    let finish = include_str!("../src/state/redis_runtime/route_health_finish.lua");
    let observe = include_str!("../src/state/redis_runtime/route_health_observe.lua");
    let coordinator_source =
        std::fs::read_to_string("src/state/redis_runtime.rs").expect("redis_runtime.rs");

    // --- T1.3: max step must be threaded on BOTH Lua paths and read into the
    // tuning in Rust. -----------------------------------------------------
    assert!(
        coordinator_source.contains("settings.upstream_transient_route_cooldown_max_step"),
        "update_runtime_tuning must read upstream_transient_route_cooldown_max_step \
         from RuntimeSettings (T1.3)"
    );
    assert!(
        coordinator_source.contains("self.tuning_snapshot().transient_route_cooldown_max_step"),
        "the coordinator must append transient_route_cooldown_max_step to the Lua \
         invocations (T1.3)"
    );
    assert!(
        finish.contains("local max_step = tonumber(ARGV[cursor])"),
        "route_health_finish.lua must read max_step after the variable-length schedules (T1.3)"
    );
    assert!(
        observe.contains(
            "local max_step = schedule_count and tonumber(ARGV[16 + schedule_count]) or nil"
        ),
        "route_health_observe.lua must read max_step after the schedules (T1.3)"
    );
    for (name, script) in [("finish", finish), ("observe", observe)] {
        assert!(
            script.contains("math.min(step, max_step)"),
            "route_health_{name}.lua must cap the non-half-open step with max_step (T1.3)"
        );
    }

    // --- T1.4: the shared-host failure domain flag must reach the step
    // escalator via the repeat bit. ---------------------------------------
    assert!(
        coordinator_source.contains("shared_host_failure_domain"),
        "observe_route_failure / finish_route_health_once must accept the shared-host \
         failure-domain flag (T1.4)"
    );
    assert!(
        coordinator_source.contains("repeat_within_request || shared_host_failure_domain"),
        "the shared-host flag must fold into the repeat bit so Lua does not escalate (T1.4)"
    );
    assert!(
        finish.contains("local repeat_within_request = ARGV[17] == '1'"),
        "route_health_finish.lua must read the repeat (incl. shared-host) bit (T1.4)"
    );

    // --- T1.2: the (already-clamped) upstream Retry-After must reach the Lua
    // cooldown as an explicit-retry floor, never a replacement. ------------
    assert!(
        coordinator_source.contains("optional_duration_ms(retry_after)"),
        "the coordinator must forward the (gateway-clamped) retry_after into the Lua \
         invocation (T1.2)"
    );
    assert!(
        finish.contains("local explicit_retry_ms = tonumber(ARGV[7])"),
        "route_health_finish.lua must read the explicit retry hint (T1.2)"
    );
    assert!(
        observe.contains("local explicit_retry_ms = tonumber(ARGV[4])"),
        "route_health_observe.lua must read the explicit retry hint (T1.2)"
    );
    for (name, script) in [("finish", finish), ("observe", observe)] {
        assert!(
            script.contains("cooldown_ms = math.max(cooldown_ms, explicit_retry_ms)"),
            "route_health_{name}.lua must apply the explicit retry hint as a floor, \
             preserving the local backoff curve as the primary cooldown (T1.2)"
        );
    }
}

// ============================================================================
// C7 (2026-08-27): no-Redis drift guard -- the downstream per-model group caps
// must be threaded into the Redis backend's Lua scripts, mirroring the local
// backend (group cap first, downstream-wide global backstop second).  This
// plain test runs everywhere and statically locks the threading so a change
// to the local backend can never silently leave the Redis backend behind.
// ============================================================================
#[test]
fn redis_lua_scripts_thread_the_c7_downstream_group_limits() {
    let reserve = include_str!("../src/state/redis_runtime/lease_reserve.lua");
    let admission = include_str!("../src/state/redis_runtime/downstream_admission.lua");
    let release = include_str!("../src/state/redis_runtime/lease_release.lua");
    let renew = include_str!("../src/state/redis_runtime/lease_renew.lua");
    let coordinator_source =
        std::fs::read_to_string("src/state/redis_runtime.rs").expect("redis_runtime.rs");

    // lease_reserve.lua (the try_reserve_downstream_concurrency path) must
    // check the group cap on the group bucket first, then the global
    // backstop on the aggregate zset.
    assert!(
        reserve.contains("local group_limit = tonumber(ARGV[2])"),
        "lease_reserve.lua must read the group cap (C7)"
    );
    assert!(
        reserve.contains("local global_limit = tonumber(ARGV[3])"),
        "lease_reserve.lua must read the global backstop (C7)"
    );
    assert!(
        reserve.contains("ZCARD', KEYS[1]) >= group_limit"),
        "lease_reserve.lua must enforce the group cap on the group bucket (C7)"
    );
    assert!(
        reserve.contains("ZCARD', KEYS[2]) >= global_limit"),
        "lease_reserve.lua must enforce the global backstop on the aggregate zset (C7)"
    );

    // downstream_admission.lua (the merged request+lease path) must enforce
    // the same group-first / global-backstop ordering.
    assert!(
        admission.contains("local group_limit = tonumber(ARGV[10])"),
        "downstream_admission.lua must read the group cap (C7)"
    );
    assert!(
        admission.contains("ZCARD', KEYS[4]) >= group_limit"),
        "downstream_admission.lua must enforce the group cap on the group bucket (C7)"
    );
    assert!(
        admission.contains("ZCARD', KEYS[5]) >= concurrency_limit"),
        "downstream_admission.lua must enforce the global backstop on the aggregate zset (C7)"
    );

    // lease_release.lua / lease_renew.lua must keep the aggregate zset in
    // sync so the global backstop never leaks slots or under-counts renewals.
    assert!(
        release.contains("if KEYS[3] then"),
        "lease_release.lua must drop the aggregate member too (C7)"
    );
    assert!(
        renew.contains("if KEYS[2] then"),
        "lease_renew.lua must renew the aggregate member too (C7)"
    );

    // The coordinator must pass the aggregate key and the group cap on both
    // reservation paths.
    assert!(
        coordinator_source.contains("\"leases_all\""),
        "the coordinator must build the downstream aggregate lease key (C7)"
    );
    assert!(
        coordinator_source.contains("group_cap"),
        "coordinate must accept the group cap and forward it into the Lua invocation (C7)"
    );
    assert!(
        coordinator_source.contains(".key(aggregate_lease_key)"),
        "the coordinator must pass the aggregate key into the Lua invocation (C7)"
    );
}

// ============================================================================
// C7 live-Redis suite: group caps behave per group on the Redis backend the
// same way they do on the local backend.  `#[ignore]`d; set TEST_REDIS_URL.
// ============================================================================
#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_group_budget_is_per_group_not_global() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("down-group-budgets");
    downstream.per_minute_limit = 100;
    // Global backstop is deliberately NOT simply the sum of the group caps:
    // the group caps sum to 2, so set the global to 3 to prove the global
    // backstop is a separate bound.
    downstream.max_concurrency = 3;
    downstream.model_concurrency_groups = vec![
        ModelConcurrencyGroup {
            name: "glm".into(),
            patterns: vec!["glm-*".into()],
            max_concurrency: 1,
        },
        ModelConcurrencyGroup {
            name: "deepseek".into(),
            patterns: vec!["deepseek-*".into()],
            max_concurrency: 3,
        },
    ];

    // glm group cap = 1: a second glm must be rejected by the GROUP cap while
    // the deepseek group still has plenty of capacity (C7 HOL regression).
    let glm_lease = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.2")
        .await
        .expect("first glm lease must succeed");
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "glm-5.1")
        .await
        .expect_err("second glm must hit the glm group cap");
    match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit, group, .. } => {
            assert_eq!(limit, 1, "group rejection must report the glm cap");
            assert_eq!(
                group.as_deref(),
                Some("glm"),
                "group rejection must name the glm group"
            );
        }
        other => panic!("unexpected rejection: {other:?}"),
    }

    // deepseek is a different bucket: it must still be admitted even though
    // the glm group is full.
    let deepseek_lease = state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4")
        .await
        .expect("deepseek must not be blocked by the full glm group");

    // Both groups now hold 1 lease each = 2 admitted total, under the global
    // 3. A second deepseek is fine.
    let deepseek_lease_2 = state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4-flash")
        .await
        .expect("second deepseek must succeed (group 3, global 3 not reached)");

    state
        .release_downstream_concurrency(glm_lease)
        .await
        .unwrap();
    state
        .release_downstream_concurrency(deepseek_lease)
        .await
        .unwrap();
    state
        .release_downstream_concurrency(deepseek_lease_2)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_admission_global_backstop_bounds_group_sum() {
    let config = redis_test_config();
    let (state, _second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("down-group-backstop");
    downstream.per_minute_limit = 100;
    // Legal overbooking on purpose: group caps sum to 4 but the global
    // backstop is 3. The global bound must still win across the aggregate.
    downstream.max_concurrency = 3;
    downstream.model_concurrency_groups = vec![
        ModelConcurrencyGroup {
            name: "glm".into(),
            patterns: vec!["glm-*".into()],
            max_concurrency: 2,
        },
        ModelConcurrencyGroup {
            name: "deepseek".into(),
            patterns: vec!["deepseek-*".into()],
            max_concurrency: 2,
        },
    ];

    // Fill glm (2) and deepseek (1) = 3 total = global backstop.
    let mut leases = Vec::new();
    for _ in 0..2 {
        leases.push(
            state
                .try_reserve_downstream_concurrency(&downstream, "glm-5.2")
                .await
                .expect("glm lease must succeed"),
        );
    }
    leases.push(
        state
            .try_reserve_downstream_concurrency(&downstream, "deepseek-v4")
            .await
            .expect("deepseek lease must succeed"),
    );

    // A third deepseek is under its group cap (2) but over the global
    // backstop (3): the global limit must be the one that rejects it.
    let rejection = state
        .try_reserve_downstream_concurrency(&downstream, "deepseek-v4-flash")
        .await
        .expect_err("global backstop must reject the third deepseek");
    match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit, group, .. } => {
            assert_eq!(
                limit, 3,
                "global backstop must report downstream.max_concurrency"
            );
            assert_eq!(
                group.as_deref(),
                Some("deepseek"),
                "global rejection still names the matched group"
            );
        }
        other => panic!("unexpected rejection: {other:?}"),
    }

    for lease in leases {
        state.release_downstream_concurrency(lease).await.unwrap();
    }
}

// The merged request+lease path (reserve_downstream_admission) must reject on
// the group cap through the same Lua limits and must NOT record the request
// slot when it does (atomicity preserved).
#[tokio::test]
#[ignore = "requires TEST_REDIS_URL"]
async fn redis_downstream_admission_group_rejection_names_the_group() {
    let config = redis_test_config();
    let (first, second, _directory) = redis_test_states(&config).await;
    let mut downstream = redis_test_downstream("down-admission-group");
    downstream.per_minute_limit = 100;
    downstream.max_concurrency = 4;
    downstream.model_concurrency_groups = vec![ModelConcurrencyGroup {
        name: "glm".into(),
        patterns: vec!["glm-*".into()],
        max_concurrency: 1,
    }];

    let (first_reservation, first_lease) = first
        .reserve_downstream_admission(&downstream, "glm-5.2")
        .await
        .expect("first combined admission must succeed");
    let rejection = first
        .reserve_downstream_admission(&downstream, "glm-5.1")
        .await
        .expect_err("second combined admission must hit the glm group cap");
    match rejection {
        DownstreamAdmissionRejection::ConcurrencyLimitExceeded { limit, group, .. } => {
            assert_eq!(limit, 1, "group rejection must report the glm cap");
            assert_eq!(
                group.as_deref(),
                Some("glm"),
                "group rejection must name the glm group"
            );
        }
        other => panic!("unexpected rejection: {other:?}"),
    }

    // Atomicity: the rejected admission must not have recorded a request
    // event. After release, a fresh glm admission succeeds again.
    first
        .release_downstream_concurrency(first_lease)
        .await
        .unwrap();
    first
        .rollback_downstream_request_reservation(first_reservation)
        .await
        .unwrap();
    let (second_reservation, second_lease) = first
        .reserve_downstream_admission(&downstream, "glm-5.1")
        .await
        .expect("admission must succeed after release");
    second
        .rollback_downstream_request_reservation(second_reservation)
        .await
        .unwrap();
    second
        .release_downstream_concurrency(second_lease)
        .await
        .unwrap();
}
