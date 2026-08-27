use super::*;
use axum::response::IntoResponse;

#[tokio::test]
async fn downstream_responses_stream_is_proxied_as_event_stream() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                concat!(
                                    "event: response.created\r\n",
                                    "data: {\"type\":\"response.created\",\"response\":{",
                                    "\"id\":\"resp-stream\",\"object\":\"response\",",
                                    "\"created_at\":1,\"status\":\"in_progress\",",
                                    "\"model\":\"gpt-4.1-mini\",\"output\":[]}}\r\n\r\n",
                                    ": upstream-comment\r\nevent: custom-response-event\r\n",
                                    "id: event-42\r\nretry: 1500\r\n",
                                    "data: {\"id\":\"resp-stream\",\r\n",
                                    "data: \"object\":\"response.chunk\"}\r\n\r\n",
                                    "event: metadata-only\r\nid: event-43\r\nretry: 1600\r\n\r\n",
                                    "event: response.output_text.delta\r\n",
                                    "data: {\"type\":\"response.output_text.delta\",",
                                    "\"response_id\":\"resp-stream\",\"item_id\":\"msg-1\",",
                                    "\"output_index\":0,\"content_index\":0,",
                                    "\"delta\":\"usable output\"}\r\n\r\n"
                                )
                                .as_bytes(),
                            )),
                            Ok(Bytes::from_static(
                                concat!(
                                    "event: response.completed\r\n",
                                    "data: {\"type\":\"response.completed\",\"response\":{",
                                    "\"id\":\"resp-stream\",\"object\":\"response\",",
                                    "\"created_at\":1,\"status\":\"completed\",",
                                    "\"model\":\"gpt-4.1-mini\",\"output\":[]}}\r\n\r\n",
                                    ": done-comment\r\nevent: terminal\r\n",
                                    "id: done-42\r\nretry: 2500\r\ndata: [DONE]\r\n\r"
                                )
                                .as_bytes(),
                            )),
                            Ok(Bytes::from_static(b"\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

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
    let request = Request::builder()
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
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    let events = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|payload| *payload != "[DONE]")
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .collect::<Vec<_>>();
    let response_id = events
        .iter()
        .find(|event| event["type"] == "response.created")
        .and_then(|event| event.pointer("/response/id"))
        .and_then(Value::as_str)
        .expect("gateway response id");
    assert!(response_id.starts_with("resp_"));
    uuid::Uuid::parse_str(response_id.trim_start_matches("resp_")).unwrap();
    for event in events.iter().filter(|event| {
        matches!(
            event["type"].as_str(),
            Some("response.created" | "response.output_text.delta" | "response.completed")
        )
    }) {
        let event_response_id = event
            .get("response_id")
            .or_else(|| event.pointer("/response/id"))
            .and_then(Value::as_str)
            .expect("response event id");
        assert_eq!(event_response_id, response_id, "{}", event["type"]);
    }
    assert!(!text.contains("resp-stream"));
    assert!(text.contains(
        ": upstream-comment\r\nevent: custom-response-event\r\nid: event-42\r\nretry: 1500"
    ));
    assert!(text.contains("event: metadata-only\r\nid: event-43\r\nretry: 1600\r\n\r\n"));
    assert!(text.contains("event: response.output_text.delta\r\n"));
    assert!(text.contains("event: response.completed\r\n"));
    assert!(text.contains(
        ": done-comment\r\nevent: terminal\r\nid: done-42\r\nretry: 2500\r\ndata: [DONE]"
    ));
    assert_eq!(text.matches("event: custom-response-event").count(), 1);
    assert_eq!(text.matches("event: metadata-only").count(), 1);
    assert_eq!(text.matches("event: response.output_text.delta").count(), 1);
    assert_eq!(text.matches("event: response.completed").count(), 1);
    assert_eq!(text.matches("event: terminal").count(), 1);

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(captured.request_body.unwrap()["stream"], true);
}

#[tokio::test]
async fn downstream_responses_stream_canonicalizes_domestic_chat_provider_eof_variants() {
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let chunks = vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"id\":\"first-id\",\"object\":\"chat.completion.chunk\",\"created\":10,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":null,\"finish_reason\":null}]}\n\n",
                )),
                Ok(Bytes::from_static(
                    b"data: {\"id\":\"later-id\",\"object\":\"chat.completion.chunk\",\"created\":20,\"model\":\"provider-alias\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
                )),
            ];
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(stream::iter(chunks)),
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
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
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
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
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
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("response.failed"), "{body}");
    assert!(!body.contains("upstream_stream_error_event"), "{body}");
    assert!(body.contains("\"type\":\"response.created\""), "{body}");
    let response_id = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|payload| *payload != "[DONE]")
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .find(|event| event["type"] == "response.created")
        .and_then(|event| event["response"]["id"].as_str().map(str::to_owned))
        .expect("gateway response id");
    assert!(response_id.starts_with("resp_"), "{body}");
    assert!(!body.contains("\"id\":\"first-id\""), "{body}");
    assert!(body.contains("\"model\":\"gpt-4.1-mini\""), "{body}");
    assert!(body.contains("\"created_at\":10"), "{body}");
    assert!(body.contains("\"delta\":\"OK\""), "{body}");
    assert_eq!(
        body.matches("\"type\":\"response.completed\"").count(),
        1,
        "{body}"
    );
    assert_eq!(body.matches("data: [DONE]").count(), 1, "{body}");
    let completed_position = body.find("event: response.completed").unwrap();
    let done_position = body.find("data: [DONE]").unwrap();
    assert!(completed_position < done_position, "{body}");

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].status_code, 200);
    assert!(snapshot.usage_logs[0].error_category.is_none());
}

#[tokio::test]
async fn downstream_responses_proxied_stream_drop_after_completed_event_is_logged_as_success() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let initial_chunks =
                            stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                ": usage-comment\r\nevent: custom-completed\r\nid: completed-42\r\nretry: 1750\r\ndata: {}\r\n\r\n",
                                json!({
                                    "type": "response.completed",
                                    "response": {
                                        "id": "resp-stream",
                                        "object": "response",
                                        "output": [{
                                            "id": "msg-1",
                                            "type": "message",
                                            "role": "assistant",
                                            "content": [{
                                                "type": "output_text",
                                                "text": "OK",
                                                "annotations": []
                                            }]
                                        }],
                                        "usage": {
                                            "input_tokens": 4,
                                            "output_tokens": 2,
                                            "total_tokens": 6,
                                            "output_tokens_details": {
                                                "accepted_prediction_tokens": 1
                                            }
                                        }
                                    }
                                })
                            )))]);
                        let delayed_done = stream::once(async {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n"))
                        });

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(initial_chunks.chain(delayed_done)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

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
                        "input": "Hello"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
        .await
        .expect("timed out waiting for proxied SSE frame")
        .expect("expected proxied SSE frame")
        .expect("expected proxied SSE data frame");
    let bytes = frame.into_data().expect("expected data frame");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains(
        ": usage-comment\r\nevent: custom-completed\r\nid: completed-42\r\nretry: 1750\r\ndata: "
    ));
    assert!(text.contains("response.completed"));
    assert!(text.contains("\"cached_tokens\":0"));
    assert!(text.contains("\"reasoning_tokens\":0"));
    assert!(text.contains("\"accepted_prediction_tokens\":1"));
    assert!(!text.contains("[DONE]"));
    drop(body);

    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while state.snapshot().await.usage_logs.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("completed proxied stream should emit one usage log");

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
async fn downstream_responses_stream_preserves_multiple_output_items_when_upstream_returns_json_response(
) {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "resp-json",
                                "object": "response",
                                "created": 1,
                                "model": "gpt-4.1-mini",
                                "output": [
                                    {
                                        "id": "msg-1",
                                        "type": "message",
                                        "status": "completed",
                                        "role": "assistant",
                                        "content": [
                                            {
                                                "type": "output_text",
                                                "text": "Hi",
                                                "annotations": []
                                            }
                                        ]
                                    },
                                    {
                                        "id": "msg-2",
                                        "type": "message",
                                        "status": "completed",
                                        "role": "assistant",
                                        "content": [
                                            {
                                                "type": "output_text",
                                                "text": "Bye",
                                                "annotations": []
                                            }
                                        ]
                                    }
                                ],
                                "usage": {
                                    "input_tokens": 2,
                                    "output_tokens": 3,
                                    "total_tokens": 5
                                }
                            })),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

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
                        "input": "Hello"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"output_index\":0"));
    assert!(text.contains("\"output_index\":1"));
    assert!(text.contains("\"text\":\"Hi\""));
    assert!(text.contains("\"text\":\"Bye\""));
    assert!(text.contains("data: [DONE]"));
    let events = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|payload| *payload != "[DONE]")
        .map(|payload| serde_json::from_str::<Value>(payload).unwrap())
        .collect::<Vec<_>>();
    let response_id = events
        .iter()
        .find(|event| event["type"] == "response.created")
        .and_then(|event| event.pointer("/response/id"))
        .and_then(Value::as_str)
        .expect("gateway response id");
    assert!(response_id.starts_with("resp_"));
    assert_ne!(response_id, "resp-json");
    uuid::Uuid::parse_str(response_id.trim_start_matches("resp_")).unwrap();
    for event in &events {
        if let Some(event_response_id) = event
            .get("response_id")
            .or_else(|| event.pointer("/response/id"))
            .and_then(Value::as_str)
        {
            assert_eq!(event_response_id, response_id, "{}", event["type"]);
        }
    }

    let captured = capture.lock().unwrap().clone();
    let request_body = captured.request_body.unwrap();
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(request_body["stream"], true);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_responses_stream_retries_without_stream_when_upstream_rejects_stream() {
    let capture = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let capture = capture_clone.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let stream = payload
                    .get("stream")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                {
                    let mut lock = capture.lock().unwrap();
                    lock.push(payload.clone());
                }

                if stream {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": {
                                "message": "streaming not supported"
                            }
                        })),
                    );
                }

                let _ = parts;
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-retry",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "Hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 2,
                            "completion_tokens": 3,
                            "total_tokens": 5
                        }
                    })),
                )
            }
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
    let request = Request::builder()
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
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("response.created"));
    assert!(text.contains("response.output_item.added"));
    assert!(text.contains("response.output_text.delta"));
    assert!(text.contains("response.completed"));
    assert!(text.contains("data: [DONE]"));

    {
        let captured = capture.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["stream"], true);
        assert_eq!(captured[1]["stream"], false);
        assert_eq!(captured[0]["messages"][0]["content"], "Hello");
        assert_eq!(captured[1]["messages"][0]["content"], "Hello");
    }

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].prompt_tokens, 2);
    assert_eq!(snapshot.usage_logs[0].completion_tokens, 3);
    assert_eq!(snapshot.usage_logs[0].total_tokens, 5);
}

#[tokio::test]
async fn downstream_responses_stream_recovers_when_chat_upstream_first_event_is_error() {
    let captured_stream_modes = Arc::new(Mutex::new(Vec::<bool>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured_for_handler = captured_stream_modes.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let captured = captured_for_handler.clone();
            async move {
                let payload: Value = serde_json::from_slice(
                    &to_bytes(request.into_body(), usize::MAX).await.unwrap(),
                )
                .unwrap();
                let request_stream = payload["stream"].as_bool().unwrap_or(false);
                captured.lock().unwrap().push(request_stream);

                if request_stream {
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream::iter([Ok::<Bytes, std::io::Error>(
                            Bytes::from_static(
                                b"event: error\ndata: {\"error\":{\"message\":\"temporary stream failure\"}}\n\n",
                            ),
                        )])),
                    )
                        .into_response();
                }

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-recovered",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "recovered"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 2,
                            "completion_tokens": 1,
                            "total_tokens": 3
                        }
                    })),
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
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{address}"),
                api_key: "fixture-key".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
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
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());
    let response = app
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
                    json!({
                        "model": "gpt-4.1-mini",
                        "stream": true,
                        "input": "Explain one protocol compatibility invariant."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(*captured_stream_modes.lock().unwrap(), vec![true, false]);
    assert!(body.contains("response.created"));
    assert!(body.contains("response.completed"));
    assert!(body.contains("data: [DONE]"));
    assert!(!body.contains("upstream_stream_error_event"));
    wait_for_upstream_in_flight(&state, "up-1", 0).await;

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].status_code, 200);
}

#[tokio::test]
async fn downstream_responses_stream_is_translated_from_chat_stream_with_tool_calls() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
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
                                                    "name": "get_weather",
                                                    "arguments": "{\"location\":\"Paris\"}"
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
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

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
    let request = Request::builder()
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
                "input": "Need weather",
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get the weather",
                            "parameters": {
                                "type": "object"
                            }
                        }
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("response.created"));
    assert!(text.contains("response.output_item.added"));
    assert!(text.contains("response.function_call_arguments.delta"));
    assert!(text.contains("response.function_call_arguments.done"));
    assert!(text.contains("response.completed"));
    assert!(text.contains("get_weather"));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(
        captured.request_body.unwrap()["messages"][0]["content"],
        "Need weather"
    );
}

#[tokio::test]
async fn downstream_responses_stream_is_translated_from_chat_stream_with_flat_tool_calls() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        let chunks = vec![
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
                                                "name": "get_weather",
                                                "arguments": "{\"location\":\"Paris\"}"
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
                            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

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
    let request = Request::builder()
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
                "input": [
                    {"role": "user", "content": "Need weather"}
                ],
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get the weather",
                            "parameters": {
                                "type": "object"
                            }
                        }
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("response.output_item.added"));
    assert!(text.contains("response.function_call_arguments.delta"));
    assert!(text.contains("response.completed"));
    assert!(text.contains("data: [DONE]"));

    let captured = capture.lock().unwrap().clone();
    assert_eq!(captured.path, "/v1/chat/completions");
    let request_body = captured.request_body.unwrap();
    assert_eq!(request_body["messages"][0]["content"], "Need weather");
    assert_eq!(request_body["tools"][0]["function"]["name"], "get_weather");
}

#[tokio::test]
async fn downstream_responses_stream_tolerates_empty_data_keepalive_frames() {
    // Domestic "OpenAI compatible" proxies emit `: ping` comments, empty
    // `data:` events, and comment-style `data: : ping` padding as keepalives.
    // The proxied Responses stream must skip them instead of failing JSON
    // decode mid-stream.
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(|| async {
            let chunks = vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        "event: response.created\n",
                        "data: {\"type\":\"response.created\",\"response\":{",
                        "\"id\":\"resp-keepalive\",\"object\":\"response\",",
                        "\"created_at\":1,\"status\":\"in_progress\",",
                        "\"model\":\"gpt-4.1-mini\",\"output\":[]}}\n\n",
                        ": ping\n\n",
                        "data:\n\n",
                        "data: : ping\n\n",
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",",
                        "\"response_id\":\"resp-keepalive\",\"item_id\":\"msg-1\",",
                        "\"output_index\":0,\"content_index\":0,",
                        "\"delta\":\"usable output\"}\n\n"
                    )
                    .as_bytes(),
                )),
                Ok(Bytes::from_static(
                    concat!(
                        ": ping\n\n",
                        "data:\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{",
                        "\"id\":\"resp-keepalive\",\"object\":\"response\",",
                        "\"created_at\":1,\"status\":\"completed\",",
                        "\"model\":\"gpt-4.1-mini\",\"output\":[]}}\n\n",
                        "data: [DONE]\n\n"
                    )
                    .as_bytes(),
                )),
            ];
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(stream::iter(chunks)),
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
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
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
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
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
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("response.failed"), "{body}");
    assert!(
        !body.contains("stream_upstream_body_decode_error"),
        "{body}"
    );
    assert!(
        body.contains("\"type\":\"response.output_text.delta\""),
        "{body}"
    );
    assert!(body.contains("\"delta\":\"usable output\""), "{body}");
    assert!(body.contains("\"type\":\"response.completed\""), "{body}");
    assert!(body.contains("data: [DONE]"), "{body}");
}

#[tokio::test]
async fn downstream_responses_stream_tolerates_chat_keepalive_and_empty_data_frames() {
    // Chat-completions upstream (deepseek/GLM/minimax style) emits `: ping`
    // comment frames, empty `data:` events and `data: : ping` padding between
    // real chunks; the chat->Responses translation must skip them and complete
    // normally instead of aborting with a decode error.
    let tempdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let chunks = vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    concat!(
                        ": ping\n\n",
                        "data:\n\n",
                        "data: : ping\n\n",
                        "data: {\"id\":\"chatcmpl-keepalive\",\"object\":\"chat.completion.chunk\",\"created\":7,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n"
                    )
                    .as_bytes(),
                )),
                Ok(Bytes::from_static(
                    concat!(
                        ": ping\n\n",
                        "data: {\"id\":\"chatcmpl-keepalive\",\"object\":\"chat.completion.chunk\",\"created\":7,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                        "data:\n\n",
                        "data: : ping\n\n",
                        "data: [DONE]\n\n"
                    )
                    .as_bytes(),
                )),
            ];
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(stream::iter(chunks)),
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
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
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
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
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
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("response.failed"), "{body}");
    assert!(
        !body.contains("stream_upstream_body_decode_error"),
        "{body}"
    );
    assert!(body.contains("\"delta\":\"OK\""), "{body}");
    assert_eq!(
        body.matches("\"type\":\"response.completed\"").count(),
        1,
        "{body}"
    );
    assert_eq!(body.matches("data: [DONE]").count(), 1, "{body}");
}

// ============================================================================
// Integration guard: account-B-style fragmented tool calls produce a valid
// `function_call_arguments.done` downstream (T1.1 + T1.2 end-to-end).
// The mock chat upstream emits the first fragment with `id` and `index`, then
// a continuation fragment that omits BOTH `index` and `id` (the "account B"
// fragmentation style from the extra-data diagnosis).  Without T1 the
// accumulator would append and yield `{}{"command":["ls"]}`; with T1 the
// downstream done event must carry parseable `{"command":["ls"]}`.
#[tokio::test]
async fn fragmented_tool_call_without_index_id_yields_valid_done_arguments() {
    let capture = Arc::new(Mutex::new(RequestCapture::default()));
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture_clone = capture.clone();

    let upstream_app =
        Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    move |State(capture): State<Arc<Mutex<RequestCapture>>>,
                          request: Request<Body>| async move {
                        let (parts, body) = request.into_parts();
                        let body = to_bytes(body, usize::MAX).await.unwrap();
                        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let mut lock = capture.lock().unwrap();
                        lock.path = parts.uri.path().to_string();
                        lock.authorization = parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        lock.request_body = Some(payload);

                        // "Account B" style: first fragment carries id+index
                        // and a `{}` placeholder, the continuation fragment
                        // carries only `arguments` (no index, no id).
                        let chunks = vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-frag",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "arbitrary/fragmented",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "role": "assistant",
                                            "tool_calls": [{
                                                "index": 0,
                                                "id": "call_frag",
                                                "function": {
                                                    "name": "shell",
                                                    "arguments": "{}"
                                                }
                                            }]
                                        },
                                        "finish_reason": null
                                    }]
                                })
                            ))),
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-frag",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "arbitrary/fragmented",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "tool_calls": [{
                                                "function": {
                                                    "arguments": "{\"command\":[\"ls\"]}"
                                                }
                                            }]
                                        },
                                        "finish_reason": null
                                    }]
                                })
                            ))),
                            Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                                "data: {}\n\n",
                                json!({
                                    "id": "chatcmpl-frag",
                                    "object": "chat.completion.chunk",
                                    "created": 1,
                                    "model": "arbitrary/fragmented",
                                    "choices": [{
                                        "index": 0,
                                        "delta": {},
                                        "finish_reason": "tool_calls"
                                    }]
                                })
                            ))),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n")),
                        ];

                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(chunks)),
                        )
                    },
                ),
            )
            .with_state(capture_clone);

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/fragmented";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-fragmented".into(),
                name: "fragmented".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
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
            ..PersistedState::default()
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": model,
                "stream": true,
                "input": "Run ls",
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "shell",
                            "description": "Run a command",
                            "parameters": {
                                "type": "object"
                            }
                        }
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("response.function_call_arguments.done"),
        "{text}"
    );
    assert!(text.contains("response.completed"), "{text}");

    // Extract every `response.function_call_arguments.done` data payload and
    // assert its `arguments` is a complete, parseable JSON object with no
    // `{}` placeholder prefix.
    let mut done_count = 0;
    for event in text.split("\n\n") {
        if event.contains("response.function_call_arguments.done") {
            let data_line = event
                .lines()
                .find(|line| line.starts_with("data: "))
                .expect("done event must carry a data line");
            let data = data_line.trim_start_matches("data: ");
            let value: Value = serde_json::from_str(data).expect("done event data must be JSON");
            let arguments = value
                .get("arguments")
                .and_then(Value::as_str)
                .expect("done event must carry arguments");
            let parsed: Value =
                serde_json::from_str(arguments).expect("done arguments must be valid JSON");
            assert_eq!(
                parsed,
                json!({"command": ["ls"]}),
                "done arguments must equal the repaired object, got {arguments}"
            );
            assert!(
                !arguments.starts_with("{}"),
                "done arguments must not carry a placeholder prefix"
            );
            done_count += 1;
        }
    }
    assert_eq!(
        done_count, 1,
        "expected exactly one done event, got {done_count}"
    );
}

/// Shared harness for the P1 tool-call identity guard tests: spins up a
/// chat-completions upstream that streams the given `delta.tool_calls`
/// fragments (each is a full tool_call object), then performs a streaming
/// `/v1/responses` request and returns the downstream SSE text.  A final
/// `finish_reason: "tool_calls"` chunk and `[DONE]` terminate the stream.
async fn run_fragmented_tool_call_stream(fragments: Vec<Value>) -> String {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| async move {
            let (_, body) = request.into_parts();
            let body = to_bytes(body, usize::MAX).await.unwrap();
            let _payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let mut chunks = Vec::new();
            for fragment in fragments {
                let chunk = format!(
                    "data: {}\n\n",
                    json!({
                        "id": "chatcmpl-frag",
                        "object": "chat.completion.chunk",
                        "created": 1,
                        "model": "arbitrary/fragmented",
                        "choices": [{
                            "index": 0,
                            "delta": { "tool_calls": [fragment] },
                            "finish_reason": null
                        }]
                    })
                );
                chunks.push(Ok::<Bytes, std::io::Error>(Bytes::from(chunk)));
            }
            chunks.push(Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                json!({
                    "id": "chatcmpl-frag",
                    "object": "chat.completion.chunk",
                    "created": 1,
                    "model": "arbitrary/fragmented",
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": "tool_calls"
                    }]
                })
            ))));
            chunks.push(Ok::<Bytes, std::io::Error>(Bytes::from_static(
                b"data: [DONE]\n\n",
            )));
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(stream::iter(chunks)),
            )
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let model = "arbitrary/fragmented";
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-fragmented".into(),
                name: "fragmented".into(),
                base_url: format!("http://{}", address),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec![model.into()],
                active: true,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
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
            ..PersistedState::default()
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "Authorization",
            format!("Bearer {}", downstream_key.plaintext),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "model": model,
                "stream": true,
                "input": "Run ls",
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "shell",
                            "description": "Run a command",
                            "parameters": { "type": "object" }
                        }
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

/// Parse every `response.function_call_arguments.done` event from the SSE
/// text into (name, arguments) pairs, in stream order.
fn parse_done_events(text: &str) -> Vec<(String, String)> {
    let mut done = Vec::new();
    for event in text.split("\n\n") {
        if event.contains("response.function_call_arguments.done") {
            let data_line = event
                .lines()
                .find(|line| line.starts_with("data: "))
                .expect("done event must carry a data line");
            let value: Value =
                serde_json::from_str(data_line.trim_start_matches("data: ")).expect("JSON");
            done.push((
                value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                value
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ));
        }
    }
    done
}

/// P1.1 guard: when the open (index/id-less) continuation fragment carries an
/// explicitly different non-empty name, it is a genuinely NEW tool call and
/// must be split into its own entry — not merged onto the open one (which
/// would yield name from call A + arguments from call B).
#[tokio::test]
async fn tool_call_name_mismatch_without_index_splits_into_two_calls() {
    let text = run_fragmented_tool_call_stream(vec![
        json!({
            "index": 0,
            "id": "call_a",
            "function": {
                "name": "shell",
                "arguments": "{\"command\":[\"ls\"]}"
            }
        }),
        json!({
            "function": {
                "name": "apply_patch",
                "arguments": "{\"patch\":\"x\"}"
            }
        }),
    ])
    .await;
    assert!(
        text.contains("response.function_call_arguments.done"),
        "{text}"
    );
    let done = parse_done_events(&text);
    assert_eq!(done.len(), 2, "expected two done events, got {done:?}");
    let shell = done
        .iter()
        .find(|(name, _)| name == "shell")
        .expect("shell call must be present");
    assert_eq!(shell.1, "{\"command\":[\"ls\"]}");
    let patch = done
        .iter()
        .find(|(name, _)| name == "apply_patch")
        .expect("apply_patch call must be present");
    assert_eq!(patch.1, "{\"patch\":\"x\"}");
}

/// P1.1 guard: an empty `"name": ""` on the continuation fragment is treated
/// as missing (some upstreams send empty names), so it still continues the
/// open call instead of splitting.
#[tokio::test]
async fn tool_call_empty_name_without_index_continues_open_call() {
    let text = run_fragmented_tool_call_stream(vec![
        json!({
            "index": 0,
            "id": "call_a",
            "function": {
                "name": "shell",
                "arguments": "{}"
            }
        }),
        json!({
            "function": {
                "name": "",
                "arguments": "{\"command\":[\"ls\"]}"
            }
        }),
    ])
    .await;
    assert!(
        text.contains("response.function_call_arguments.done"),
        "{text}"
    );
    let done = parse_done_events(&text);
    assert_eq!(
        done.len(),
        1,
        "expected exactly one done event, got {done:?}"
    );
    assert_eq!(done[0].0, "shell");
    assert_eq!(done[0].1, "{\"command\":[\"ls\"]}");
}

/// §3.3 guard: on a NORMAL (non-anomalous) multi-fragment tool call, the
/// `response.function_call_arguments.delta` fragments for a given item must
/// concatenate byte-for-byte to the `response.function_call_arguments.done`
/// `arguments` for the same item.  This locks in that the `client_delta_desynced`
/// suppression introduced with the T1.2 complete-then-new guard does not leak
/// into the ordinary incremental-split path.
#[tokio::test]
async fn normal_fragmented_tool_call_deltas_concatenate_bytewise_to_done_arguments() {
    // True incremental splits (index carried, no placeholder): the normal
    // upstream shape that must keep appending.
    let text = run_fragmented_tool_call_stream(vec![
        json!({
            "index": 0,
            "id": "call_incr",
            "function": {
                "name": "shell",
                "arguments": "{\"comm"
            }
        }),
        json!({
            "index": 0,
            "function": {
                "arguments": "and\":[\"ls\"]}"
            }
        }),
    ])
    .await;

    assert!(
        text.contains("response.function_call_arguments.done"),
        "{text}"
    );
    assert!(
        text.contains("response.function_call_arguments.delta"),
        "{text}"
    );

    // Collect (item_id -> concatenated deltas) and (item_id -> done arguments).
    let mut deltas: std::collections::BTreeMap<String, String> = Default::default();
    let mut done: std::collections::BTreeMap<String, String> = Default::default();
    for event in text.split("\n\n") {
        let is_delta = event.contains("response.function_call_arguments.delta");
        let is_done = event.contains("response.function_call_arguments.done");
        if !is_delta && !is_done {
            continue;
        }
        let data_line = event
            .lines()
            .find(|line| line.starts_with("data: "))
            .expect("event must carry a data line");
        let value: Value =
            serde_json::from_str(data_line.trim_start_matches("data: ")).expect("JSON");
        let item_id = value
            .get("item_id")
            .and_then(Value::as_str)
            .expect("events must carry item_id")
            .to_string();
        if is_delta {
            let delta = value
                .get("delta")
                .and_then(Value::as_str)
                .expect("delta event must carry delta");
            deltas.entry(item_id).or_default().push_str(delta);
        } else {
            let arguments = value
                .get("arguments")
                .and_then(Value::as_str)
                .expect("done event must carry arguments")
                .to_string();
            done.insert(item_id, arguments);
        }
    }

    assert_eq!(
        done.len(),
        1,
        "expected exactly one done event, got {done:?}"
    );
    let item_id = done.keys().next().unwrap().clone();
    let concatenated = deltas
        .get(&item_id)
        .unwrap_or_else(|| panic!("missing deltas for {item_id}"));
    assert_eq!(
        concatenated, &done[&item_id],
        "concatenated deltas must equal the done arguments byte-for-byte"
    );
    assert_eq!(
        done[&item_id], "{\"command\":[\"ls\"]}",
        "expected the incrementally accumulated arguments"
    );
}
