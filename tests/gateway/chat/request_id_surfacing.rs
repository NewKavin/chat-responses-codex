//! E4: gateway-generated request_id must reach the client on every error
//! exit - the x-gateway-request-id response header, `details.request_id` in
//! the JSON error body, and a `request_id=<rid>` tail on the message (the
//! only carrier codex sees on the SSE path).  Success responses already
//! carry the header; this suite covers the error paths that did not.
//!
//! Red line asserted throughout: the upstream response body never leaks into
//! the client-visible message.

use super::*;
use axum::body::Body;
use axum::http::Request;
use axum::routing::post;
use axum::Router;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppState, DownstreamConfig, PersistedState, UpstreamConfig};

async fn request_id_error_state(
    upstream_status: StatusCode,
    delay_ms: Option<u64>,
) -> (AppState, GeneratedDownstreamKey) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request<Body>| {
            let (delay_ms, upstream_status) = (delay_ms, upstream_status);
            async move {
                if let Some(delay) = delay_ms {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                (
                    upstream_status,
                    axum::Json(json!({
                        "error": {
                            "message": "UPSTREAM_SECRET_BODY_MUST_NOT_LEAK",
                            "type": "server_error",
                            "code": "origin_unreachable"
                        }
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let model = "gpt-4";
    let upstream = UpstreamConfig {
        id: "reqid-upstream".into(),
        name: "reqid upstream".into(),
        base_url: format!("http://{address}"),
        api_key: "reqid-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    };
    let downstream_key = generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream]),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "down-reqid".into(),
                name: "reqid client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec![model.into()],
                rate_limit_enabled: false,
                per_minute_limit: 60,
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
            }]),
            ..PersistedState::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_route_exhaustion_retry_enabled: false,
            upstream_transient_last_resort_probe_enabled: false,
            ..AppConfig::default()
        },
    );
    let _ = directory;
    (state, downstream_key)
}

fn chat_request(downstream_key: &GeneratedDownstreamKey, stream: bool) -> Request<Body> {
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
                "stream": stream
            })
            .to_string(),
        ))
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn error_response_carries_gateway_request_id() {
    let (state, downstream_key) =
        request_id_error_state(StatusCode::SERVICE_UNAVAILABLE, None).await;
    let app = build_router(state.clone());

    let response = app
        .oneshot(chat_request(&downstream_key, false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let rid = response
        .headers()
        .get("x-gateway-request-id")
        .expect("error response must carry x-gateway-request-id")
        .to_str()
        .unwrap()
        .to_string();
    assert!(!rid.is_empty(), "request id must be non-empty");

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"]["details"]["request_id"],
        json!(rid),
        "JSON error body details must carry the gateway request id"
    );
    let message = payload["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("request_id={rid}")),
        "message must carry request_id tail: {message}"
    );
    assert!(
        !message.contains("UPSTREAM_SECRET_BODY_MUST_NOT_LEAK"),
        "upstream body must never leak into the client message: {message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stream_error_frame_carries_gateway_request_id() {
    // Delay the 503 past the 10ms early-failure window so the request lands
    // on the SSE keepalive path and the terminal error is emitted as a stream
    // error frame (the path where message is the only carrier).
    let (state, downstream_key) =
        request_id_error_state(StatusCode::SERVICE_UNAVAILABLE, Some(80)).await;
    let app = build_router(state.clone());

    let response = app
        .oneshot(chat_request(&downstream_key, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rid = response
        .headers()
        .get("x-gateway-request-id")
        .expect("SSE response must carry x-gateway-request-id")
        .to_str()
        .unwrap()
        .to_string();

    let mut body = response.into_body();
    let mut text = String::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
            .await
            .expect("timed out reading SSE frames");
        match frame {
            Some(Ok(frame)) => {
                if let Ok(bytes) = frame.into_data() {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            Some(Err(error)) => panic!("unexpected stream error: {error}"),
            None => break,
        }
        if text.contains("[DONE]") {
            break;
        }
    }

    assert!(
        text.contains(&format!("request_id={rid}")),
        "SSE error frame message must carry request_id tail; got: {text}"
    );
    assert!(
        text.contains(&format!("\"request_id\":\"{rid}\"")),
        "SSE error frame details must carry request_id; got: {text}"
    );
    assert!(
        !text.contains("UPSTREAM_SECRET_BODY_MUST_NOT_LEAK"),
        "upstream body must never leak into the SSE frame: {text}"
    );
}
