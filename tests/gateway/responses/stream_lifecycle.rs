#![allow(clippy::field_reassign_with_default)]

use super::*;

// The gateway's streaming pipeline (dispatch -> hedge -> proxied stream) forms
// a deep asynchronous frame chain whose peak stack usage in a debug build sits
// right at the default 2 MiB test-thread stack, and error-framing changes have
// repeatedly tipped this deepest-path test over the limit. Run the body on a
// dedicated thread with a larger stack so the assertions stay stable across
// rebuilds instead of aborting with a stack overflow.
#[test]
fn slow_first_output_hedge_uses_responses_text_delta_from_next_upstream() {
    let handle = std::thread::Builder::new()
        .name("slow_first_output_hedge_stack_guard".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build test runtime");
            runtime.block_on(
                slow_first_output_hedge_uses_responses_text_delta_from_next_upstream_body(),
            );
        })
        .expect("spawn stack-guard thread");
    handle.join().expect("test body panicked");
}

async fn slow_first_output_hedge_uses_responses_text_delta_from_next_upstream_body() {
    let slow_hits = Arc::new(AtomicUsize::new(0));
    let fast_hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let slow_hits_for_handler = slow_hits.clone();
    let fast_hits_for_handler = fast_hits.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |request: Request<Body>| {
            let slow_hits = slow_hits_for_handler.clone();
            let fast_hits = fast_hits_for_handler.clone();
            async move {
                let authorization = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                if authorization == "Bearer slow-responses-key" {
                    slow_hits.fetch_add(1, Ordering::SeqCst);
                    let lifecycle = Bytes::from_static(
                        concat!(
                            "data: {\"type\":\"response.created\",\"response\":{",
                            "\"id\":\"resp-slow\",\"object\":\"response\",\"created_at\":1,",
                            "\"status\":\"in_progress\",\"model\":\"gpt-4.1-mini\",\"output\":[]}}\n\n"
                        )
                        .as_bytes(),
                    );
                    let stream = stream::once(async {
                        Ok::<Bytes, std::io::Error>(lifecycle)
                    })
                    .chain(stream::pending::<Result<Bytes, std::io::Error>>());
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream),
                    )
                        .into_response();
                }

                fast_hits.fetch_add(1, Ordering::SeqCst);
                let chunks = vec![Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{",
                        "\"id\":\"resp-fast\",\"object\":\"response\",\"created_at\":1,",
                        "\"status\":\"in_progress\",\"model\":\"gpt-4.1-mini\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.output_text.delta\",",
                        "\"response_id\":\"resp-fast\",\"item_id\":\"msg-fast\",",
                        "\"output_index\":0,\"content_index\":0,",
                        "\"delta\":\"hedged Responses winner\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{",
                        "\"id\":\"resp-fast\",\"object\":\"response\",\"created_at\":1,",
                        "\"status\":\"completed\",\"model\":\"gpt-4.1-mini\",",
                        "\"output\":[{\"id\":\"msg-fast\",\"type\":\"message\",",
                        "\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",",
                        "\"text\":\"hedged Responses winner\",\"annotations\":[]}]}]}}\n\n",
                        "data: [DONE]\n\n"
                    )
                    .as_bytes(),
                ))];
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(stream::iter(chunks)),
                )
                    .into_response()
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![
                UpstreamConfig {
                    id: "responses-slow".into(),
                    name: "Responses slow primary".into(),
                    base_url: format!("http://{address}"),
                    api_key: "slow-responses-key".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    priority: 10,
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "responses-fast".into(),
                    name: "Responses fast hedge".into(),
                    base_url: format!("http://{address}"),
                    api_key: "fast-responses-key".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    priority: 0,
                    active: true,
                    ..Default::default()
                },
            ]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),

                model_concurrency_groups: vec![],
            }]),
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: true,
            upstream_hedge_delay_ms: 50,
            upstream_hedge_interval_ms: 50,
            upstream_hedge_max_extra_attempts: 1,
            ..AppConfig::default()
        },
    );
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        build_router(state.clone()).oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(
                    "x-chat2responses-troubleshooting-route",
                    state.troubleshooting_route_capture_token(),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "stream": true,
                        "input": "Compare bounded retries with slow-first-output hedging."
                    })
                    .to_string(),
                ))
                .unwrap(),
        ),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "hedge dispatch timed out: slow_hits={} fast_hits={}",
            slow_hits.load(Ordering::SeqCst),
            fast_hits.load(Ordering::SeqCst)
        )
    })
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-chat2responses-selected-upstream-id"],
        "responses-fast"
    );
    let body = tokio::time::timeout(
        Duration::from_secs(2),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("the Responses hedge should win before the downstream timeout")
    .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("hedged Responses winner"));
    assert!(!body.contains("resp-slow"));
    assert_eq!(body.matches("response.created").count(), 1);
    assert_eq!(slow_hits.load(Ordering::SeqCst), 1);
    assert_eq!(fast_hits.load(Ordering::SeqCst), 1);
    wait_for_upstream_in_flight(&state, "responses-slow", 0).await;
    wait_for_upstream_in_flight(&state, "responses-fast", 0).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].upstream_key_id, "responses-fast");
    assert_eq!(snapshot.usage_logs[0].status_code, 200);
    let first = snapshot.usage_logs[0]
        .first_token_latency_ms
        .expect("the winning physical attempt should carry its first-token timing");
    assert!(first <= snapshot.usage_logs[0].latency_ms);
    assert!(snapshot
        .upstreams
        .iter()
        .all(|upstream| upstream.failure_count == 0));
}

#[tokio::test]
async fn stream_disconnect_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            (
                StatusCode::OK,
                headers,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first_frame = body
        .frame()
        .await
        .expect("expected at least one SSE frame before drop")
        .expect("expected a valid SSE frame")
        .into_data()
        .expect("expected an SSE data frame");
    assert!(String::from_utf8_lossy(&first_frame).contains("Hello"));
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
}

#[tokio::test]
async fn stream_interruption_marks_interrupted_not_success() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            (
                StatusCode::OK,
                headers,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body
        .frame()
        .await
        .expect("expected at least one SSE frame before drop")
        .expect("expected a valid SSE frame")
        .into_data()
        .expect("expected an SSE data frame");
    assert!(String::from_utf8_lossy(&first_frame).contains("Hello"));
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 499);
    // A content event reached the downstream before it disconnected. Terminal
    // usage is not required to distinguish a partial delivery from a cancel
    // before output.
    assert_eq!(
        log.error_category.as_deref(),
        Some("stream_incomplete_close")
    );
    assert_eq!(
        log.error_message.as_deref(),
        Some(
            "downstream response body dropped before semantic completion \
             (partial output delivered)"
        ),
        "unexpected interruption message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn post_output_upstream_stream_error_returns_typed_responses_error_not_499() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let first = stream::once(async {
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        "data: {\"id\":\"chatcmpl-valid\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4\",",
                        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"one\"},\"finish_reason\":null}]",
                        "}\n\n",
                    )
                    .as_bytes(),
                ))
            });
            let invalid = stream::once(async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        "data: {\"id\":\"chatcmpl-valid\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4\",",
                        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"two\"},",
                        "\"finish_reason\":\"provider_unknown\"}]}\n\n",
                    )
                    .as_bytes(),
                ))
            });
            (
                StatusCode::OK,
                headers,
                Body::from_stream(first.chain(invalid)),
            )
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "input": "Hello",
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("translation failure should be returned as a structured SSE error");
    let body = String::from_utf8_lossy(&body);
    let failed_position = body
        .find("event: response.failed")
        .expect("Responses stream failures must include response.failed");
    let done_position = body
        .find("data: [DONE]")
        .expect("Responses stream failures must terminate with [DONE]");
    assert!(failed_position < done_position);
    assert!(body.contains("\"type\":\"response.failed\""));
    assert!(body.contains("\"status\":\"failed\""));
    assert!(body.contains("\"object\":\"response\""));
    assert!(body.contains("\"output\":[]"));
    assert!(body.contains("\"usage\":null"));
    // Single terminal event only (2026-08-22): both response.failed and a
    // top-level error event made Responses clients (codex) print the same
    // error twice. The failure diagnosis lives in response.failed's error block.
    assert!(
        !body.contains("event: error"),
        "Responses failure must not emit a duplicate error event: {body}"
    );
    assert!(body.contains("\"code\":\"upstream_stream_error_event\""));
    assert!(body.contains(
        "\"message\":\"[upstream_stream_error_event] upstream SSE stream reported failure; request_id="
    ));
    assert!(body.contains("\"category\":\"upstream_stream_error_event\""));
    assert!(body.contains("\"retry_after_seconds\":null"));
    assert_eq!(
        body.matches("[upstream_stream_error_event]").count(),
        1,
        "single Responses error carrier should contain one stable prefix: {body}"
    );
    assert!(body.contains("\"delta\":\"one\""));
    assert!(!body.contains("\"param\":null"));
    assert!(!body.contains("data: {\"error\":"));
    assert!(body.contains("data: [DONE]"));
    assert!(!body.contains("upstream-secret"));
    assert!(!body.contains("up-1"));

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let mut downstream = state.snapshot().await.downstreams[0].clone();
    downstream.max_concurrency = 1;
    let lease = state
        .try_reserve_downstream_concurrency(&downstream, "test-model")
        .await
        .unwrap();
    state.release_downstream_concurrency(lease).await.unwrap();
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert!(snapshot.usage_logs.iter().all(|log| log.status_code != 499));
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, StatusCode::BAD_GATEWAY.as_u16());
    assert_eq!(
        log.error_category.as_deref(),
        Some("upstream_stream_error_event")
    );
}

#[tokio::test]
async fn translated_truncated_chunked_body_after_usable_output_is_not_retried() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let first_event = concat!(
        "data: {\"id\":\"chatcmpl-truncated\",\"object\":\"chat.completion.chunk\",",
        "\"created\":1,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"translated-before-truncation\"},\"finish_reason\":null}]}\n\n"
    )
    .as_bytes();
    let first_url =
        spawn_truncated_chunked_sse_upstream(Some(first_event), b"data: {", first_hits.clone())
            .await;
    let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_address = second_listener.local_addr().unwrap();
    let second_hits_for_handler = second_hits.clone();
    let second_app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let second_hits = second_hits_for_handler.clone();
            async move {
                second_hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    concat!(
                        "data: {\"id\":\"chatcmpl-replay\",\"object\":\"chat.completion.chunk\",",
                        "\"created\":1,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,",
                        "\"delta\":{\"content\":\"unexpected-replay\"},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n"
                    ),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(second_listener, second_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![
                UpstreamConfig {
                    id: "translated-truncated-first".into(),
                    name: "translated-truncated-first".into(),
                    base_url: first_url,
                    api_key: "first-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],
                    priority: 100,
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "translated-fallback-second".into(),
                    name: "translated-fallback-second".into(),
                    base_url: format!("http://{second_address}"),
                    api_key: "second-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4".into()],
                    priority: 0,
                    active: true,
                    ..Default::default()
                },
            ]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            ..AppConfig::default()
        },
    );

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"model": "gpt-4", "input": "Hello", "stream": true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("translated body failure should become typed Responses SSE")
            .to_vec(),
    )
    .unwrap();
    let output_position = body
        .find("translated-before-truncation")
        .expect("translated output must be preserved");
    let error_position = body
        .find("stream_upstream_body_decode_error")
        .expect("translated stream must expose the stable body-decode category");
    assert!(output_position < error_position, "{body}");
    assert!(body.contains("event: response.failed"), "{body}");
    // Single terminal event only (2026-08-22): no redundant top-level error event.
    assert!(!body.contains("event: error"), "{body}");
    assert!(!body.contains("unexpected-replay"), "{body}");
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);
    wait_for_upstream_in_flight(&state, "translated-truncated-first", 0).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(
        snapshot.usage_logs[0].status_code,
        StatusCode::BAD_GATEWAY.as_u16()
    );
    assert_eq!(
        snapshot.usage_logs[0].error_category.as_deref(),
        Some("stream_upstream_body_decode_error")
    );
}

async fn translated_drop_after_event(event_type: &str) -> (Vec<String>, u16, String, Option<u64>) {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let initial = stream::once(async {
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-delivery\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
                ))
            });
            let stalled = stream::pending::<Result<Bytes, std::io::Error>>();
            (
                StatusCode::OK,
                headers,
                Body::from_stream(initial.chain(stalled)),
            )
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "input": "Hello",
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let marker = format!("\"type\":\"{event_type}\"");
    let mut body = response.into_body();
    let mut delivered = Vec::new();
    for _ in 0..12 {
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("timed out waiting for translated event")
            .expect("expected translated event")
            .expect("expected valid translated event")
            .into_data()
            .expect("expected translated SSE data");
        let text = String::from_utf8_lossy(&frame).into_owned();
        let reached_marker = text.contains(&marker);
        delivered.push(text);
        if reached_marker {
            break;
        }
    }
    assert!(
        delivered.iter().any(|frame| frame.contains(&marker)),
        "translated stream did not emit {event_type}: {delivered:?}"
    );
    drop(body);
    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    tokio::time::timeout(Duration::from_secs(1), async {
        while state.snapshot().await.usage_logs.len() != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("translated disconnect should emit one usage log");
    let snapshot = state.snapshot().await;
    let log = &snapshot.usage_logs[0];
    (
        delivered,
        log.status_code,
        log.error_category.clone().unwrap_or_default(),
        log.first_token_latency_ms,
    )
}

#[tokio::test]
async fn translated_drop_after_response_created_is_cancelled_before_usable_output() {
    let (delivered, status, category, first_token_latency_ms) =
        translated_drop_after_event("response.created").await;

    assert_eq!(delivered.len(), 1);
    assert_eq!(status, 499);
    assert_eq!(category, "stream_client_cancelled");
    assert_eq!(first_token_latency_ms, None);
}

#[tokio::test]
async fn translated_drop_after_text_delta_is_incomplete_close() {
    let (delivered, status, category, first_token_latency_ms) =
        translated_drop_after_event("response.output_text.delta").await;

    assert!(delivered.len() > 1);
    assert_eq!(status, 499);
    assert_eq!(category, "stream_incomplete_close");
    assert!(first_token_latency_ms.is_some());
}

#[tokio::test]
async fn native_drop_after_response_incomplete_is_not_499() {
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/responses",
        post(|| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let output = stream::iter(vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        "event: response.created\n",
                        "data: {\"type\":\"response.created\",\"response\":{",
                        "\"id\":\"resp-incomplete\",\"object\":\"response\",",
                        "\"created_at\":1,\"status\":\"in_progress\",",
                        "\"model\":\"gpt-4\",\"output\":[]}}\n\n",
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",",
                        "\"response_id\":\"resp-incomplete\",\"item_id\":\"msg-1\",",
                        "\"output_index\":0,\"content_index\":0,",
                        "\"delta\":\"partial output\"}\n\n"
                    )
                    .as_bytes(),
                )),
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        "event: response.incomplete\n",
                        "data: {\"type\":\"response.incomplete\",\"response\":{",
                        "\"id\":\"resp-incomplete\",\"object\":\"response\",",
                        "\"created_at\":1,\"status\":\"incomplete\",",
                        "\"incomplete_details\":{\"reason\":\"max_output_tokens\"},",
                        "\"model\":\"gpt-4\",\"output\":[]}}\n\n"
                    )
                    .as_bytes(),
                )),
            ])
            .chain(stream::pending::<Result<Bytes, std::io::Error>>());
            (StatusCode::OK, headers, Body::from_stream(output))
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["gpt-4".into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            ..Default::default()
        },
    );

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"model": "gpt-4", "input": "Hello", "stream": true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let mut saw_incomplete = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("timed out waiting for response.incomplete")
            .expect("expected an SSE frame")
            .expect("expected a valid SSE frame")
            .into_data()
            .expect("expected SSE data");
        if String::from_utf8_lossy(&frame).contains("response.incomplete") {
            saw_incomplete = true;
            break;
        }
    }
    assert!(
        saw_incomplete,
        "upstream terminal must reach the downstream"
    );
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while state.snapshot().await.usage_logs.len() != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("terminal drop should emit one usage log");

    let snapshot = state.snapshot().await;
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, StatusCode::OK.as_u16());
    assert_eq!(log.error_category, None);
    assert_eq!(log.error_message, None);
    assert_eq!(snapshot.upstreams[0].failure_count, 0);
}

async fn native_responses_response_for_chunks(
    chunks: Vec<(Duration, Bytes)>,
    stall_after_chunks: bool,
) -> (
    axum::response::Response,
    AppState,
    tempfile::TempDir,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let tempdir = tempdir().unwrap();
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let chunks = Arc::new(chunks);
    let first_hits_for_handler = first_hits.clone();

    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move || {
            let chunks = chunks.clone();
            let first_hits = first_hits_for_handler.clone();
            async move {
                first_hits.fetch_add(1, Ordering::SeqCst);
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                let output =
                    stream::iter(chunks.as_ref().clone()).then(|(delay, chunk)| async move {
                        tokio::time::sleep(delay).await;
                        Ok::<Bytes, std::io::Error>(chunk)
                    });
                let body = if stall_after_chunks {
                    Body::from_stream(
                        output.chain(stream::pending::<Result<Bytes, std::io::Error>>()),
                    )
                } else {
                    Body::from_stream(output)
                };
                (StatusCode::OK, headers, body)
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_address = second_listener.local_addr().unwrap();
    let second_hits_for_handler = second_hits.clone();
    let second_app = Router::new().route(
        "/v1/responses",
        post(move || {
            let second_hits = second_hits_for_handler.clone();
            async move {
                second_hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    concat!(
                        "data: {\"type\":\"response.output_text.delta\",",
                        "\"response_id\":\"resp-replay\",\"item_id\":\"msg-replay\",",
                        "\"output_index\":0,\"content_index\":0,",
                        "\"delta\":\"unexpected-replay\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{",
                        "\"id\":\"resp-replay\",\"object\":\"response\",",
                        "\"created_at\":1,\"status\":\"completed\",",
                        "\"model\":\"gpt-4\",\"output\":[]}}\n\n",
                        "data: [DONE]\n\n"
                    ),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(second_listener, second_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![
                UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: format!("http://{address}"),
                    api_key: "upstream-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["gpt-4".into()],
                    priority: 100,
                    active: true,
                    ..Default::default()
                },
                UpstreamConfig {
                    id: "up-2".into(),
                    name: "fallback".into(),
                    base_url: format!("http://{second_address}"),
                    api_key: "fallback-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["gpt-4".into()],
                    priority: 0,
                    active: true,
                    ..Default::default()
                },
            ]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            ..Default::default()
        },
    );

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"model": "gpt-4", "input": "Hello", "stream": true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    (response, state, tempdir, first_hits, second_hits)
}

#[tokio::test]
async fn native_responses_clean_eof_without_terminal_is_typed_502() {
    let (response, state, _tempdir, first_hits, second_hits) =
        native_responses_response_for_chunks(
            vec![(
                Duration::ZERO,
                Bytes::from_static(
                    concat!(
                        "event: response.created\n",
                        "data: {\"type\":\"response.created\",\"response\":{",
                        "\"id\":\"resp-eof\",\"object\":\"response\",\"created_at\":1,",
                        "\"status\":\"in_progress\",\"model\":\"gpt-4\",\"output\":[]}}\n\n",
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",",
                        "\"response_id\":\"resp-eof\",\"item_id\":\"msg-1\",",
                        "\"output_index\":0,\"content_index\":0,",
                        "\"delta\":\"partial-before-eof\"}\n\n"
                    )
                    .as_bytes(),
                ),
            )],
            false,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("incomplete EOF should become a typed Responses error")
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("partial-before-eof"), "{body}");
    assert!(body.contains("event: response.failed"), "{body}");
    assert!(body.contains("stream_upstream_incomplete_eof"), "{body}");
    assert!(!body.contains("unexpected-replay"), "{body}");
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(
        snapshot.usage_logs[0].status_code,
        StatusCode::BAD_GATEWAY.as_u16()
    );
    assert_eq!(
        snapshot.usage_logs[0].error_category.as_deref(),
        Some("stream_upstream_incomplete_eof")
    );
    assert!(snapshot.usage_logs[0].first_token_latency_ms.is_some());
}

#[tokio::test]
async fn native_responses_done_without_terminal_is_incomplete() {
    let (response, state, _tempdir, first_hits, second_hits) =
        native_responses_response_for_chunks(
            vec![(
                Duration::ZERO,
                Bytes::from_static(
                    concat!(
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",",
                        "\"response_id\":\"resp-done\",\"item_id\":\"msg-1\",",
                        "\"output_index\":0,\"content_index\":0,",
                        "\"delta\":\"partial-before-done\"}\n\n",
                        "data: [DONE]\n\n"
                    )
                    .as_bytes(),
                ),
            )],
            false,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("premature DONE should become a typed Responses error")
            .to_vec(),
    )
    .unwrap();
    let error_position = body
        .find("upstream_stream_incomplete")
        .expect("missing semantic terminal must have a stable category");
    let done_position = body
        .find("data: [DONE]")
        .expect("gateway error sequence must end with DONE");
    assert!(body.contains("partial-before-done"), "{body}");
    assert!(body.contains("event: response.failed"), "{body}");
    assert!(error_position < done_position, "{body}");
    assert_eq!(body.matches("data: [DONE]").count(), 1, "{body}");
    assert!(!body.contains("unexpected-replay"), "{body}");
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(
        snapshot.usage_logs[0].status_code,
        StatusCode::BAD_GATEWAY.as_u16()
    );
    assert_eq!(
        snapshot.usage_logs[0].error_category.as_deref(),
        Some("upstream_stream_incomplete")
    );
}

#[tokio::test]
async fn native_responses_terminal_followed_by_eof_is_valid() {
    for terminal_type in ["response.completed", "response.incomplete"] {
        let terminal_status = if terminal_type == "response.completed" {
            "completed"
        } else {
            "incomplete"
        };
        let terminal = format!(
            concat!(
                "data: {{\"type\":\"response.output_text.delta\",",
                "\"response_id\":\"resp-terminal\",\"item_id\":\"msg-1\",",
                "\"output_index\":0,\"content_index\":0,\"delta\":\"valid\"}}\n\n",
                "data: {{\"type\":\"{}\",\"response\":{{",
                "\"id\":\"resp-terminal\",\"object\":\"response\",",
                "\"created_at\":1,\"status\":\"{}\",",
                "\"model\":\"gpt-4\",\"output\":[]}}}}\n\n"
            ),
            terminal_type, terminal_status
        );
        let (response, state, _tempdir, first_hits, second_hits) =
            native_responses_response_for_chunks(
                vec![(Duration::ZERO, Bytes::from(terminal))],
                false,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("explicit Responses terminal followed by EOF must remain valid")
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains(terminal_type), "{body}");
        assert!(!body.contains("event: response.failed"), "{body}");
        assert_eq!(first_hits.load(Ordering::SeqCst), 1);
        assert_eq!(second_hits.load(Ordering::SeqCst), 0);

        wait_for_upstream_in_flight(&state, "up-1", 0).await;
        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.usage_logs.len(), 1);
        assert_eq!(snapshot.usage_logs[0].status_code, StatusCode::OK.as_u16());
        assert_eq!(snapshot.usage_logs[0].error_category, None);
        assert_eq!(snapshot.upstreams[0].failure_count, 0);
    }
}

#[tokio::test]
async fn native_responses_records_first_token_latency_on_first_usable_output() {
    let metadata = Bytes::from_static(
        concat!(
            ": upstream keepalive\n\n",
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{",
            "\"id\":\"resp-latency\",\"object\":\"response\",\"created_at\":1,",
            "\"status\":\"in_progress\",\"model\":\"gpt-4\",\"output\":[]}}\n\n"
        )
        .as_bytes(),
    );
    let usable_output = Bytes::from_static(
        concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",",
            "\"response_id\":\"resp-latency\",\"item_id\":\"msg-1\",",
            "\"output_index\":0,\"content_index\":0,\"delta\":\"delayed\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{",
            "\"id\":\"resp-latency\",\"object\":\"response\",\"created_at\":1,",
            "\"status\":\"completed\",\"model\":\"gpt-4\",\"output\":[]}}\n\n",
            "data: [DONE]\n\n"
        )
        .as_bytes(),
    );
    let (response, state, _tempdir, first_hits, second_hits) =
        native_responses_response_for_chunks(
            vec![
                (Duration::ZERO, metadata),
                (Duration::from_millis(80), usable_output),
            ],
            false,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("delayed"), "{body}");
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    let log = &snapshot.usage_logs[0];
    let first = log
        .first_token_latency_ms
        .expect("stream should record first usable output latency");
    assert!(
        first >= 40,
        "first usable output was recorded too early: {first}"
    );
    assert!(first <= log.latency_ms);
}

#[tokio::test]
async fn translated_stream_disconnect_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(|_body: String| async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                (
                    StatusCode::OK,
                    headers,
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0,\"text\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\",\"annotations\":[]}]}]}}\n\n",
                        "data: [DONE]\n\n",
                    ),
                )
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["claude-3-5-sonnet".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["claude-3-5-sonnet".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-3-5-sonnet",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = body.frame().await.unwrap();
    first_frame.expect("expected at least one translated SSE frame before drop");
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
}

#[tokio::test]
async fn translated_stream_drop_after_done_is_logged_as_success() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new()
        .route(
            "/v1/responses",
            post(|_body: String| async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                (
                    StatusCode::OK,
                    headers,
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0,\"text\":\"Hello\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"claude-3-5-sonnet\",\"output\":[{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\",\"annotations\":[]}]}]}}\n\n",
                        "data: [DONE]\n\n",
                    ),
                )
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["claude-3-5-sonnet".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["claude-3-5-sonnet".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "claude-3-5-sonnet",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let mut saw_done = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("timed out waiting for translated SSE frame")
            .expect("expected translated SSE frame")
            .expect("expected translated SSE data frame");
        let bytes = frame.into_data().expect("expected data frame");
        if bytes
            .windows(b"[DONE]".len())
            .any(|window| window == b"[DONE]")
        {
            saw_done = true;
            break;
        }
    }
    assert!(
        saw_done,
        "translated stream should emit a terminal [DONE] frame"
    );
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(
        log.status_code, 200,
        "unexpected translated stream log error: {:?} / {:?}",
        log.error_category, log.error_message
    );
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
}

#[tokio::test]
async fn translated_chat_to_responses_drop_after_completed_event_is_logged_as_success() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );

            let initial_chunks = stream::iter(vec![
                Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                    "data: {}\n\n",
                    json!({
                        "id": "chatcmpl-tool",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "role": "assistant",
                                "tool_calls": [{
                                    "index": 0,
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "exec_command",
                                        "arguments": "{\"cmd\":\"pwd\"}"
                                    }
                                }]
                            },
                            "finish_reason": null
                        }]
                    })
                ))),
                Ok(Bytes::from(format!(
                    "data: {}\n\n",
                    json!({
                        "id": "chatcmpl-tool",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "tool_calls"
                        }]
                    })
                ))),
            ]);
            let delayed_done = stream::once(async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n"))
            });

            (
                StatusCode::OK,
                headers,
                Body::from_stream(initial_chunks.chain(delayed_done)),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),

                model_concurrency_groups: vec![],
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "stream": true,
                        "input": "Use pwd",
                        "tools": [{
                            "type": "function",
                            "function": {
                                "name": "exec_command",
                                "description": "Run a shell command",
                                "parameters": {
                                    "type": "object",
                                    "properties": {
                                        "cmd": {"type": "string"}
                                    },
                                    "required": ["cmd"],
                                    "additionalProperties": false
                                }
                            }
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let mut saw_completed = false;
    let mut saw_done = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("timed out waiting for translated SSE frame")
            .expect("expected translated SSE frame")
            .expect("expected translated SSE data frame");
        let bytes = frame.into_data().expect("expected data frame");
        let text = String::from_utf8_lossy(&bytes);
        if text.contains("response.completed") {
            saw_completed = true;
            break;
        }
        if text.contains("[DONE]") {
            saw_done = true;
            break;
        }
    }

    assert!(
        saw_completed,
        "expected translated stream to emit response.completed"
    );
    assert!(
        !saw_done,
        "test expected to drop before the translated [DONE] frame arrived"
    );
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 200);
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
}

#[tokio::test]
async fn stream_idle_timeout_interrupts_hung_stream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::pending::<Result<Bytes, std::io::Error>>();
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_idle_timeout_seconds = 1;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = tokio::time::timeout(
        Duration::from_secs(3),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("stream did not time out in time")
    .expect("stream timeout should be emitted as a structured SSE frame");
    let body_text = String::from_utf8_lossy(&body);
    assert!(
        body_text.contains("\"code\":\"stream_idle_timeout\""),
        "stream idle timeout should include a machine-readable code, got: {body_text}"
    );
    assert!(
        body_text.contains("\"category\":\"stream_idle_timeout\""),
        "stream idle timeout should include a searchable category, got: {body_text}"
    );
    assert!(
        body_text.contains("data: [DONE]"),
        "stream idle timeout should terminate the SSE stream, got: {body_text}"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 504);
    assert_eq!(log.error_category.as_deref(), Some("stream_idle_timeout"));
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("idle timeout waiting for SSE"),
        "unexpected idle timeout message: {:?}",
        log.error_message
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stream_keepalive_heartbeats_extend_stream_until_completion() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::once(async {
                tokio::time::sleep(Duration::from_millis(2_200)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                ))
            });
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 1;
    config.upstream_stream_idle_timeout_seconds = 2;
    config.upstream_stream_max_duration_seconds = 10;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "input": "Hello",
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_request_id = response
        .headers()
        .get("x-gateway-request-id")
        .expect("early Responses SSE response must include a gateway request ID")
        .to_str()
        .expect("gateway request ID must be a valid header value")
        .to_string();
    assert!(!response_request_id.is_empty());

    let mut body = response.into_body();
    let keepalive_bytes = Bytes::from_static(b": keepalive\n\n");

    let first_frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("expected the first keepalive frame before the idle timeout")
        .expect("expected first keepalive frame")
        .expect("expected data frame");
    let first_bytes = first_frame.into_data().expect("expected keepalive bytes");
    assert_eq!(first_bytes, keepalive_bytes);

    let mut saw_real_chunk = false;
    let mut saw_stream_end = false;
    for _ in 0..16 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("timed out waiting for the upstream chunk or a keepalive");

        match frame {
            Some(Ok(frame)) => {
                let bytes = frame.into_data().expect("expected stream bytes");
                if bytes != keepalive_bytes {
                    saw_real_chunk = true;
                }
            }
            Some(Err(error)) => panic!("unexpected stream error: {error}"),
            None => {
                saw_stream_end = true;
                break;
            }
        }
    }

    assert!(
        saw_real_chunk,
        "expected the delayed upstream chunk to complete the stream"
    );
    assert!(
        saw_stream_end,
        "expected the stream to close cleanly after the upstream chunk"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 200);
    assert_eq!(log.error_category.as_deref(), None);
    assert_eq!(log.error_message.as_deref(), None);
    assert_eq!(log.request_id, response_request_id);
}

#[tokio::test(flavor = "current_thread")]
async fn native_responses_stream_keepalive_is_sse_comment_for_codex_clients() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/responses",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::once(async {
                tokio::time::sleep(Duration::from_millis(2_200)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"gpt-4.1-mini\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg-1\",\"type\":\"message\",\"status\":\"in_progress\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\n",
                        "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0,\"text\":\"OK\"}\n\n",
                        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"msg-1\",\"type\":\"message\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"OK\",\"annotations\":[]}]}}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"object\":\"response\",\"created_at\":1,\"model\":\"gpt-4.1-mini\",\"output\":[{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"OK\",\"annotations\":[]}]}]}}\n\n",
                        "data: [DONE]\n\n",
                    )
                    .as_bytes(),
                ))
            });
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 1;
    config.upstream_stream_idle_timeout_seconds = 2;
    config.upstream_stream_max_duration_seconds = 10;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["gpt-4.1-mini".into()],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),

                model_concurrency_groups: vec![],
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "stream": true,
                        "input": "Hello"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let keepalive_bytes = Bytes::from_static(b": keepalive\n\n");

    let first_frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("expected the first keepalive frame before the delayed Responses chunk")
        .expect("expected first keepalive frame")
        .expect("expected data frame");
    let first_bytes = first_frame.into_data().expect("expected keepalive bytes");
    assert_eq!(first_bytes, keepalive_bytes);
    assert!(
        !first_bytes.starts_with(b"data:"),
        "Codex Responses keepalive must be an SSE comment, not a fake data event"
    );

    let mut saw_real_chunk = false;
    let mut saw_stream_end = false;
    for _ in 0..4 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("timed out waiting for the upstream Responses chunk or a keepalive");

        match frame {
            Some(Ok(frame)) => {
                let bytes = frame.into_data().expect("expected stream bytes");
                if bytes != keepalive_bytes {
                    saw_real_chunk = true;
                }
            }
            Some(Err(error)) => panic!("unexpected stream error: {error}"),
            None => {
                saw_stream_end = true;
                break;
            }
        }
    }

    assert!(
        saw_real_chunk,
        "expected the delayed upstream Responses chunk to complete the stream"
    );
    assert!(
        saw_stream_end,
        "expected the Responses stream to close cleanly after the upstream chunk"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
}

#[tokio::test]
async fn stream_max_duration_interrupts_hung_stream() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            let stream = stream::pending::<Result<Bytes, std::io::Error>>();
            (StatusCode::OK, headers, Body::from_stream(stream))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let mut config = AppConfig::default();
    config.upstream_stream_keepalive_interval_seconds = 10;
    config.upstream_stream_idle_timeout_seconds = 60;
    config.upstream_stream_max_duration_seconds = 1;
    config.upstream_response_header_timeout_seconds = 1;
    config.upstream_connect_timeout_seconds = 1;
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        config,
    );

    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "Authorization",
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = tokio::time::timeout(
        Duration::from_secs(3),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("stream did not time out in time")
    .expect("stream max duration should be emitted as a structured SSE frame");
    let body_text = String::from_utf8_lossy(&body);
    assert!(
        body_text.contains("\"code\":\"stream_max_duration\""),
        "stream max duration should include a machine-readable code, got: {body_text}"
    );
    assert!(
        body_text.contains("\"category\":\"stream_max_duration\""),
        "stream max duration should include a searchable category, got: {body_text}"
    );
    assert!(
        body_text.contains("data: [DONE]"),
        "stream max duration should terminate the SSE stream, got: {body_text}"
    );

    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    let log = snapshot
        .usage_logs
        .last()
        .expect("expected usage log entry");
    assert_eq!(log.status_code, 504);
    assert_eq!(log.error_category.as_deref(), Some("stream_max_duration"));
    assert!(
        log.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("stream max duration exceeded before completion"),
        "unexpected max duration message: {:?}",
        log.error_message
    );
}

#[tokio::test]
async fn synthesized_stream_response_releases_runtime_state() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: String| async move {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                headers,
                axum::Json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2
                    }
                })),
            )
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 10,
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
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
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases: vec![],
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                "Authorization",
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "model": "gpt-4",
                    "stream": true,
                    "messages": [{"role": "user", "content": "Hello"}]
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let _ = to_bytes(first.into_body(), usize::MAX).await.unwrap();

    let snapshots = state.upstream_runtime_snapshots().await.unwrap();
    let up1_snapshot = snapshots.get("up-1").unwrap();
    assert_eq!(
        up1_snapshot.in_flight, 0,
        "in_flight should be 0 after synthesized stream"
    );

    let second = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
}

/// A committed SSE stream that returns HTTP 200 on the wire, delivers semantic
/// output, then truncates (incomplete EOF) must record wire_status_code 200 and
/// logical status_code 502 with structured stream diagnostics.
#[tokio::test]
async fn committed_stream_failure_records_wire_200_and_logical_502() {
    let (response, state, _tempdir, _first_hits, _second_hits) =
        native_responses_response_for_chunks(
            vec![(
                Duration::ZERO,
                Bytes::from_static(
                    concat!(
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",",
                        "\"response_id\":\"resp-wire\",\"item_id\":\"msg-wire\",",
                        "\"output_index\":0,\"content_index\":0,",
                        "\"delta\":\"semantic-output-before-failure\"}\n\n"
                    )
                    .as_bytes(),
                ),
            )],
            false,
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("incomplete EOF should become a typed Responses error")
            .to_vec(),
    )
    .unwrap();
    assert!(
        body.contains("semantic-output-before-failure"),
        "body must preserve semantic output: {body}"
    );
    assert!(body.contains("event: response.failed"), "{body}");

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.wire_status_code, 200, "wire status must be 200");
    assert_eq!(log.status_code, 502, "logical status must be 502");
    let diagnostic = log
        .stream_diagnostics
        .as_ref()
        .expect("stream_diagnostics must be populated");
    assert!(
        diagnostic.semantic_output_observed,
        "semantic output must be observed"
    );
    assert!(
        !diagnostic.semantic_terminal_observed,
        "terminal must not be observed for incomplete EOF"
    );
    assert!(
        diagnostic.physical_attempt_count >= 1,
        "at least one physical attempt"
    );
}
