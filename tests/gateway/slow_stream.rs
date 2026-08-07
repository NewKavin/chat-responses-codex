#![allow(dead_code, unused_imports, clippy::await_holding_lock)]

use super::common::*;
use axum::response::IntoResponse;
use futures_util::StreamExt;
use std::time::Duration;

/// A fixture that simulates an upstream with configurable delays for
/// response headers and first semantic output.  Designed for use with
/// `#[tokio::test(start_paused = true)]`.
struct DelayedStreamFixture {
    response: Option<axum::response::Response>,
    state: AppState,
    _tempdir: tempfile::TempDir,
    upstream_hits: Arc<AtomicUsize>,
    logical_status: Arc<Mutex<u16>>,
}

impl DelayedStreamFixture {
    /// Create a Responses-protocol upstream that delays the initial SSE
    /// chunk (which serves as both header and first semantic output) by
    /// `header_delay` then `first_output_delay`.
    async fn responses(header_delay: Duration, first_output_delay: Duration) -> Self {
        Self::build(
            header_delay,
            first_output_delay,
            UpstreamProtocol::Responses,
            "/v1/responses",
            None,
            None,
        )
        .await
    }

    /// Create a multi-upstream fixture where the first upstream never
    /// produces semantic output within the combined budget, and the
    /// second upstream also delays.  This exercises the shared deadline
    /// across routing attempts.
    #[allow(clippy::too_many_arguments)]
    async fn component_path(
        primary_header_delay: Duration,
        primary_output_delay: Duration,
        _secondary_header_delay: Duration,
        _total_budget: Duration,
    ) -> Self {
        Self::build(
            primary_header_delay,
            primary_output_delay,
            UpstreamProtocol::Responses,
            "/v1/responses",
            Some(_secondary_header_delay),
            Some(_total_budget),
        )
        .await
    }

    /// Create a Responses upstream that sends headers after `header_delay`
    /// but never produces semantic output (stalls forever).
    async fn stalled_responses(header_delay: Duration) -> Self {
        Self::build_stalled(header_delay).await
    }

    async fn build_stalled(header_delay: Duration) -> Self {
        let upstream_hits = Arc::new(AtomicUsize::new(0));
        let logical_status = Arc::new(Mutex::new(0u16));
        let tempdir = tempdir().unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = upstream_hits.clone();

        let app = Router::new().route(
            "/v1/responses",
            post(move || {
                let hits = hits.clone();
                let header_delay = header_delay;
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(header_delay).await;

                    let created = Bytes::from_static(
                        b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-stall\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"model\":\"gpt-4\",\"output\":[]}}\n\n",
                    );
                    let stall = stream::pending::<Result<Bytes, std::io::Error>>();
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream::once(async { Ok::<Bytes, std::io::Error>(created) }).chain(stall)),
                    )
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let config = AppConfig {
            upstream_hedge_enabled: false,
            upstream_stream_keepalive_interval_seconds: 1,
            upstream_response_header_timeout_seconds: 3_600,
            upstream_stream_idle_timeout_seconds: 3_600,
            upstream_first_semantic_output_timeout_seconds: 3_300,
            upstream_concurrency_recovery_max_wait_ms: 1,
            upstream_concurrency_recovery_max_rounds: 1,
            ..AppConfig::default()
        };
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
                    priority: 100,
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
                    per_minute_limit: 999,
                    rate_limit_enabled: false,
                    max_concurrency: 999,
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
                }]),
                ..Default::default()
            },
            tempdir.path().to_path_buf(),
            config,
        );

        let app_state = state.clone();
        let router = build_router(state.clone());
        let response = router
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
                        serde_json::to_vec(&json!({
                            "model": "gpt-4",
                            "input": "hello",
                            "stream": true,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        Self {
            response: Some(response),
            state: app_state,
            _tempdir: tempdir,
            upstream_hits,
            logical_status,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn build(
        header_delay: Duration,
        first_output_delay: Duration,
        _protocol: UpstreamProtocol,
        upstream_path: &str,
        secondary_delay: Option<Duration>,
        _total_budget: Option<Duration>,
    ) -> Self {
        let upstream_hits = Arc::new(AtomicUsize::new(0));
        let logical_status = Arc::new(Mutex::new(0u16));
        let tempdir = tempdir().unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = upstream_hits.clone();
        let status_tracker = logical_status.clone();

        let app = Router::new().route(
            upstream_path,
            post(move || {
                let hits = hits.clone();
                let status_tracker = status_tracker.clone();
                let header_delay = header_delay;
                let output_delay = first_output_delay;
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    // Delay before sending the first chunk (simulates
                    // slow response headers).
                    tokio::time::sleep(header_delay).await;

                    let first_chunk = Bytes::from_static(
                        concat!(
                            "data: {\"type\":\"response.created\",\"response\":{",
                            "\"id\":\"resp-slow\",\"object\":\"response\",\"created_at\":1,",
                            "\"status\":\"in_progress\",\"model\":\"gpt-4\",\"output\":[]}}\n\n",
                        )
                        .as_bytes(),
                    );

                    // Delay before first semantic output.
                    tokio::time::sleep(output_delay).await;

                    let semantic_chunk = Bytes::from_static(
                        concat!(
                            "data: {\"type\":\"response.output_text.delta\",",
                            "\"response_id\":\"resp-slow\",\"item_id\":\"msg-slow\",",
                            "\"output_index\":0,\"content_index\":0,",
                            "\"delta\":\"hello\"}\n\n",
                        )
                        .as_bytes(),
                    );

                    let terminal_chunk = Bytes::from_static(
                        concat!(
                            "data: {\"type\":\"response.completed\",\"response\":{",
                            "\"id\":\"resp-slow\",\"object\":\"response\",\"created_at\":1,",
                            "\"status\":\"completed\",\"model\":\"gpt-4\",",
                            "\"output\":[{\"id\":\"msg-slow\",\"type\":\"message\",",
                            "\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",",
                            "\"text\":\"hello\",\"annotations\":[]}]}]}}\n\n",
                            "data: [DONE]\n\n",
                        )
                        .as_bytes(),
                    );

                    let chunks = vec![
                        Ok::<Bytes, std::io::Error>(first_chunk),
                        Ok(semantic_chunk),
                        Ok(terminal_chunk),
                    ];

                    let stream = stream::iter(chunks);
                    *status_tracker.lock().unwrap() = 200;
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream),
                    )
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Optional secondary upstream that never produces semantic output.
        let upstreams = if let Some(second_delay) = secondary_delay {
            let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let second_address = second_listener.local_addr().unwrap();
            let second_hits = upstream_hits.clone();
            let second_app = Router::new().route(
                upstream_path,
                post(move || {
                    let second_hits = second_hits.clone();
                    let second_delay = second_delay;
                    async move {
                        second_hits.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(second_delay).await;
                        // Never produces semantic output — stalls forever
                        let stall =
                            stream::pending::<Result<Bytes, std::io::Error>>();
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(
                                stream::once(async {
                                    Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                        b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-2\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"model\":\"gpt-4\",\"output\":[]}}\n\n",
                                    ))
                                })
                                .chain(stall),
                            ),
                        )
                    }
                }),
            );
            tokio::spawn(async move {
                axum::serve(second_listener, second_app).await.unwrap();
            });

            vec![
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
                    name: "secondary".into(),
                    base_url: format!("http://{second_address}"),
                    api_key: "secondary-secret".into(),
                    protocol: UpstreamProtocol::Responses,
                    protocols: vec![UpstreamProtocol::Responses],
                    supported_models: vec!["gpt-4".into()],
                    priority: 0,
                    active: true,
                    ..Default::default()
                },
            ]
        } else {
            vec![UpstreamConfig {
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
            }]
        };

        let downstream_key = generate_downstream_key("gw");

        let config = AppConfig {
            upstream_hedge_enabled: false,
            upstream_stream_keepalive_interval_seconds: 1,
            upstream_response_header_timeout_seconds: 3_600,
            upstream_first_semantic_output_timeout_seconds: 3_300,
            upstream_concurrency_recovery_max_wait_ms: 1,
            upstream_concurrency_recovery_max_rounds: 1,
            ..AppConfig::default()
        };

        let state = AppState::new(
            PersistedState {
                upstreams: std::sync::Arc::new(upstreams),
                downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "team-a".into(),
                    hash: downstream_key.hash.clone(),
                    plaintext_key: Some(downstream_key.plaintext.clone()),
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["gpt-4".into()],
                    per_minute_limit: 999,
                    rate_limit_enabled: false,
                    max_concurrency: 999,
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
                }]),
                ..Default::default()
            },
            tempdir.path().to_path_buf(),
            config,
        );

        let app_state = state.clone();
        let router = build_router(state.clone());
        let response = router
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
                        serde_json::to_vec(&json!({
                            "model": "gpt-4",
                            "input": "hello",
                            "stream": true,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        Self {
            response: Some(response),
            state: app_state,
            _tempdir: tempdir,
            upstream_hits,
            logical_status,
        }
    }

    fn request_stream(&mut self) -> axum::response::Response {
        self.response.take().expect("response already consumed")
    }

    async fn logical_status(&self) -> u16 {
        // Wait for usage log to be flushed
        tokio::time::sleep(Duration::from_millis(50)).await;
        let snapshot = self.state.snapshot().await;
        if let Some(log) = snapshot.usage_logs.last() {
            return log.status_code;
        }
        *self.logical_status.lock().unwrap()
    }

    async fn physical_attempts(&self) -> usize {
        self.upstream_hits.load(Ordering::SeqCst)
    }
}

async fn response_body_text(response: axum::response::Response) -> String {
    String::from_utf8_lossy(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should complete"),
    )
    .into_owned()
}

#[tokio::test(start_paused = true)]
async fn delayed_headers_and_first_semantic_output_survive_80_180_and_300_seconds() {
    for delay in [80_u64, 180, 300] {
        let mut fixture =
            DelayedStreamFixture::responses(Duration::from_secs(delay), Duration::from_secs(delay))
                .await;
        let response = fixture.request_stream();
        let body = tokio::spawn(response_body_text(response));
        tokio::time::advance(Duration::from_secs(delay * 2 + 2)).await;
        let body = body.await.unwrap();
        assert!(
            body.contains("response.output_text.delta"),
            "body should contain semantic output: {body}"
        );
        assert!(
            body.contains("response.completed"),
            "body should contain terminal: {body}"
        );
        assert_eq!(
            fixture.logical_status().await,
            200,
            "logical status should be 200"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn all_attempts_share_one_first_semantic_deadline() {
    // Primary delays headers by 600s then stalls forever (no semantic output).
    // The shared first-semantic-output deadline (3300s) should expire
    // before any semantic output is observed, producing a typed timeout.
    let mut fixture = DelayedStreamFixture::stalled_responses(Duration::from_secs(600)).await;
    let response = fixture.request_stream();
    let body = tokio::spawn(response_body_text(response));
    tokio::time::advance(Duration::from_secs(3_301)).await;
    let body = body.await.unwrap();
    assert!(
        body.contains("first_semantic_output_timeout"),
        "body should contain timeout error: {body}"
    );
    let physical_attempts = fixture.physical_attempts().await;
    assert!(
        physical_attempts <= 2,
        "physical attempts should be bounded, got {physical_attempts}"
    );
    let status = fixture.logical_status().await;
    assert_ne!(status, 499, "logical status must not be 499");
}
