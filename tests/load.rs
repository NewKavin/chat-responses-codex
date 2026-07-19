use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use futures_util::stream::{self, StreamExt};
use http_body_util::BodyExt;
use serde_json::json;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;

const INLINE_IMAGE_BASELINE: &str = concat!(
    "data:image/png;base64,",
    "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAMElEQVR42mP4T2PAMGoB",
    "aRYwMFAHjVowasGoBaMWjFowasGoBaMWDHULRpuOA2EBAHmBeOr2sW6XAAAAAElFTkSuQmCC"
);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FirstEventBaseline {
    revision: String,
    direct_p50_ms: u64,
    direct_p95_ms: u64,
    gateway_p50_ms: u64,
    gateway_p95_ms: u64,
    gateway_added_p95_ms: i64,
    image_direct_p95_ms: u64,
    image_gateway_p95_ms: u64,
    image_gateway_added_p95_ms: i64,
    direct_requests: usize,
    gateway_requests: usize,
}

#[derive(Debug)]
struct LatencyComparison {
    direct_ms: Vec<u64>,
    gateway_ms: Vec<u64>,
}

impl LatencyComparison {
    fn gateway_added_p95_ms(&mut self) -> i64 {
        self.direct_ms.sort_unstable();
        self.gateway_ms.sort_unstable();
        let index = (self.gateway_ms.len() * 95 / 100).min(self.gateway_ms.len() - 1);
        self.gateway_ms[index] as i64 - self.direct_ms[index] as i64
    }
}

fn percentile(values: &mut [u64], percentile: usize) -> u64 {
    values.sort_unstable();
    values[(values.len() * percentile / 100).min(values.len() - 1)]
}

const FIRST_EVENT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
enum BaselineRound {
    Text,
    Image,
}

impl BaselineRound {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum BaselinePath {
    Direct,
    Gateway,
}

impl BaselinePath {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Gateway => "gateway",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FirstEventRequestContext {
    round: BaselineRound,
    path: BaselinePath,
    request_index: usize,
}

async fn run_first_event_lifecycle<F, T>(
    context: FirstEventRequestContext,
    timeout: Duration,
    operation: F,
) -> T
where
    F: Future<Output = T>,
{
    tokio::time::timeout(timeout, operation)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for first meaningful SSE event: round={} path={} request_index={} timeout_ms={}",
                context.round.as_str(),
                context.path.as_str(),
                context.request_index,
                timeout.as_millis()
            )
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstSseDataEvent {
    Incomplete,
    Meaningful,
    Terminal,
    Error,
}

fn classify_first_sse_data_event(buffer: &[u8]) -> FirstSseDataEvent {
    let text = String::from_utf8_lossy(buffer);
    let normalized = text.replace("\r\n", "\n");

    for event in normalized.split_inclusive("\n\n") {
        if !event.ends_with("\n\n") {
            continue;
        }

        let payload = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let payload = payload.trim();
        if payload.is_empty() {
            continue;
        }
        if payload == "[DONE]" {
            return FirstSseDataEvent::Terminal;
        }
        if sse_data_payload_is_error(payload) {
            return FirstSseDataEvent::Error;
        }
        return FirstSseDataEvent::Meaningful;
    }

    FirstSseDataEvent::Incomplete
}

fn sse_data_payload_is_error(payload: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };

    object.get("error").is_some_and(|error| !error.is_null())
        || object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|event_type| matches!(event_type, "error" | "response.failed"))
}

async fn direct_first_meaningful_event_latency(
    client: reqwest::Client,
    url: String,
    request_body: String,
    context: FirstEventRequestContext,
) -> u64 {
    let request = client
        .post(url)
        .header(header::AUTHORIZATION.as_str(), "Bearer upstream-secret")
        .header(header::CONTENT_TYPE.as_str(), "application/json")
        .body(request_body)
        .build()
        .unwrap();
    let operation = async move {
        let started = Instant::now();
        let mut response = client.execute(request).await.unwrap();
        assert_eq!(response.status().as_u16(), StatusCode::OK.as_u16());

        let mut buffer = Vec::new();
        loop {
            let chunk = response
                .chunk()
                .await
                .unwrap()
                .expect("direct SSE response ended before a meaningful data frame");
            buffer.extend_from_slice(&chunk);
            match classify_first_sse_data_event(&buffer) {
                FirstSseDataEvent::Meaningful => return started.elapsed().as_millis() as u64,
                FirstSseDataEvent::Terminal => {
                    panic!(
                        "received terminal SSE event before meaningful data: round={} path={} request_index={}",
                        context.round.as_str(),
                        context.path.as_str(),
                        context.request_index
                    )
                }
                FirstSseDataEvent::Error => {
                    panic!(
                        "received SSE error event before meaningful data: round={} path={} request_index={}",
                        context.round.as_str(),
                        context.path.as_str(),
                        context.request_index
                    )
                }
                FirstSseDataEvent::Incomplete => {}
            }
        }
    };

    run_first_event_lifecycle(context, FIRST_EVENT_TIMEOUT, operation).await
}

async fn gateway_first_meaningful_event_latency(
    app: Router,
    secret: String,
    request_body: String,
    context: FirstEventRequestContext,
) -> u64 {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(request_body))
        .unwrap();
    let operation = async move {
        let started = Instant::now();
        let response = app.oneshot(request).await.unwrap();
        if response.status() != StatusCode::OK {
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            panic!(
                "gateway first-event request failed: round={} path={} request_index={} status={} body={}",
                context.round.as_str(),
                context.path.as_str(),
                context.request_index,
                status,
                String::from_utf8_lossy(&body),
            );
        }

        let mut body = response.into_body();
        let mut buffer = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Ok(data) = frame.into_data() {
                buffer.extend_from_slice(&data);
                match classify_first_sse_data_event(&buffer) {
                    FirstSseDataEvent::Meaningful => return started.elapsed().as_millis() as u64,
                    FirstSseDataEvent::Terminal => {
                        panic!(
                            "received terminal SSE event before meaningful data: round={} path={} request_index={}",
                            context.round.as_str(),
                            context.path.as_str(),
                            context.request_index
                        )
                    }
                    FirstSseDataEvent::Error => {
                        panic!(
                            "received SSE error event before meaningful data: round={} path={} request_index={}",
                            context.round.as_str(),
                            context.path.as_str(),
                            context.request_index
                        )
                    }
                    FirstSseDataEvent::Incomplete => {}
                }
            }
        }

        panic!("gateway SSE response ended before a meaningful data frame");
    };

    run_first_event_lifecycle(context, FIRST_EVENT_TIMEOUT, operation).await
}

async fn run_direct_first_event_round(
    client: reqwest::Client,
    url: String,
    request_body: String,
    round: BaselineRound,
    total_requests: usize,
    concurrency: usize,
) -> Vec<u64> {
    stream::iter(0..total_requests)
        .map(|request_index| {
            direct_first_meaningful_event_latency(
                client.clone(),
                url.clone(),
                request_body.clone(),
                FirstEventRequestContext {
                    round,
                    path: BaselinePath::Direct,
                    request_index,
                },
            )
        })
        .buffer_unordered(concurrency)
        .collect()
        .await
}

async fn run_gateway_first_event_round(
    app: Router,
    secret: String,
    request_body: String,
    round: BaselineRound,
    total_requests: usize,
    concurrency: usize,
) -> Vec<u64> {
    stream::iter(0..total_requests)
        .map(|request_index| {
            gateway_first_meaningful_event_latency(
                app.clone(),
                secret.clone(),
                request_body.clone(),
                FirstEventRequestContext {
                    round,
                    path: BaselinePath::Gateway,
                    request_index,
                },
            )
        })
        .buffer_unordered(concurrency)
        .collect()
        .await
}

fn responses_request_body(image_url: Option<&str>) -> String {
    match image_url {
        Some(image_url) => json!({
            "model": "gpt-4.1-mini",
            "stream": true,
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "Describe this image."},
                    {"type": "input_image", "image_url": image_url}
                ]
            }]
        })
        .to_string(),
        None => json!({
            "model": "gpt-4.1-mini",
            "stream": true,
            "input": "Hello"
        })
        .to_string(),
    }
}

async fn stamp_load_profile(state: &AppState, profile: &mut UpstreamDialectProfile) {
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.id == profile.key.upstream_id)
        .expect("load-test upstream must exist");
    let protocol = match profile.key.protocol {
        WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
        WireProtocol::Responses => UpstreamProtocol::Responses,
        WireProtocol::Messages => panic!("Messages is not an upstream protocol"),
    };
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            &profile.key.runtime_model_slug,
            &profile.key.runtime_model_slug,
            protocol,
        )
        .unwrap();
}

#[test]
fn first_sse_event_rejects_terminal_before_meaningful() {
    assert_eq!(
        classify_first_sse_data_event(b"data: [DONE]\n\n"),
        FirstSseDataEvent::Terminal
    );
}

#[test]
fn first_sse_event_rejects_gateway_error_before_meaningful() {
    assert_eq!(
        classify_first_sse_data_event(
            b"data: {\"error\":{\"message\":\"upstream failed\",\"type\":\"upstream_error\"}}\n\n"
        ),
        FirstSseDataEvent::Error
    );
}

#[test]
fn first_sse_event_accepts_meaningful_lf_frame_after_comment() {
    assert_eq!(
        classify_first_sse_data_event(
            b": keepalive\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n"
        ),
        FirstSseDataEvent::Meaningful
    );
}

#[test]
fn first_sse_event_accepts_meaningful_crlf_frame() {
    assert_eq!(
        classify_first_sse_data_event(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\r\n\r\n"
        ),
        FirstSseDataEvent::Meaningful
    );
}

#[test]
fn first_sse_event_waits_for_cross_chunk_frame_completion() {
    let first_chunk = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n";
    assert_eq!(
        classify_first_sse_data_event(first_chunk),
        FirstSseDataEvent::Incomplete
    );

    let mut complete = first_chunk.to_vec();
    complete.extend_from_slice(b"\n");
    assert_eq!(
        classify_first_sse_data_event(&complete),
        FirstSseDataEvent::Meaningful
    );
}

#[tokio::test]
async fn first_event_lifecycle_timeout_reports_request_context() {
    let contexts = [
        FirstEventRequestContext {
            round: BaselineRound::Text,
            path: BaselinePath::Direct,
            request_index: 3,
        },
        FirstEventRequestContext {
            round: BaselineRound::Image,
            path: BaselinePath::Gateway,
            request_index: 17,
        },
    ];

    for context in contexts {
        let task = tokio::spawn(run_first_event_lifecycle(
            context,
            Duration::from_millis(10),
            std::future::pending::<u64>(),
        ));
        let join_result = tokio::time::timeout(Duration::from_millis(250), task)
            .await
            .expect("first-event lifecycle did not enforce its internal timeout");
        let join_error = join_result.expect_err("pending lifecycle must panic on timeout");
        assert!(join_error.is_panic());

        let panic = join_error.into_panic();
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("timeout panic must contain a string diagnostic");
        assert!(message.contains(&format!("round={}", context.round.as_str())));
        assert!(message.contains(&format!("path={}", context.path.as_str())));
        assert!(message.contains(&format!("request_index={}", context.request_index)));
    }
}

#[test]
fn app_config_exposes_postgres_pool_and_admin_query_limits() {
    let config = AppConfig::default();
    assert!(config.postgres_pool_max_size >= 4);
    assert!(config.admin_logs_page_size_max >= 200);
    assert!(config.upstream_http_pool_max_idle_per_host >= 8);
}

#[tokio::test]
#[ignore]
async fn load_gateway_chat_path_with_twenty_way_concurrency() {
    const TOTAL_REQUESTS: usize = 100;
    const CONCURRENCY: usize = 20;

    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_hits = Arc::new(AtomicUsize::new(0));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let upstream_hits_clone = upstream_hits.clone();

    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let upstream_hits_clone = upstream_hits_clone.clone();
            async move {
                upstream_hits_clone.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-load",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
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
            }
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{}", upstream_addr),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],

                default_model_context: None,

                model_contexts: vec![],
                request_quota_window_hours: 5,

                request_quota_requests: 10_000,
                requests_per_minute: 10_000,
                max_concurrency: 20,
                model_request_costs: vec![],
                priority: 0,
                premium_models: vec![],
                premium_only: false,
                protect_premium_quota: false,
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 10_000,

                rate_limit_enabled: true,

                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig::default(),
    );

    let app = build_router(state);
    let request_body = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string();

    let started = Instant::now();
    let mut latencies = stream::iter(0..TOTAL_REQUESTS)
        .map(|_| {
            let app = app.clone();
            let request_body = request_body.clone();
            let secret = downstream_key.plaintext.clone();
            async move {
                let request_started = Instant::now();
                let response = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/v1/chat/completions")
                            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(request_body))
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                assert_eq!(response.status(), StatusCode::OK);
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                assert!(!body.is_empty());
                request_started.elapsed().as_millis() as u64
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    latencies.sort_unstable();
    let total_elapsed = started.elapsed();
    let min = latencies.first().copied().unwrap_or_default();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    let max = latencies.last().copied().unwrap_or_default();
    let average = if latencies.is_empty() {
        0
    } else {
        latencies.iter().sum::<u64>() / latencies.len() as u64
    };

    println!(
        "load test baseline: requests={} concurrency={} elapsed_ms={} min_ms={} avg_ms={} p50_ms={} p95_ms={} max_ms={} upstream_hits={}",
        TOTAL_REQUESTS,
        CONCURRENCY,
        total_elapsed.as_millis(),
        min,
        average,
        p50,
        p95,
        max,
        upstream_hits.load(Ordering::SeqCst)
    );
}

#[tokio::test]
#[ignore]
async fn load_gateway_first_meaningful_event_baseline() {
    const TOTAL_REQUESTS: usize = 100;
    const CONCURRENCY: usize = 20;

    let revision = std::env::var("PROTOCOL_BASELINE_REVISION")
        .expect("PROTOCOL_BASELINE_REVISION must be set for the baseline run");
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_hits = Arc::new(AtomicUsize::new(0));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let upstream_hits_clone = upstream_hits.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move || {
            let upstream_hits_clone = upstream_hits_clone.clone();
            async move {
                upstream_hits_clone.fetch_add(1, Ordering::SeqCst);
                let first_event = stream::once(async {
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
                    ))
                });
                let terminal_event = stream::once(async {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n"))
                });

                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(first_event.chain(terminal_event)),
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
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{upstream_addr}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["gpt-4.1-mini".into()],
                request_quota_window_hours: 5,
                request_quota_requests: 10_000,
                requests_per_minute: 10_000,
                max_concurrency: TOTAL_REQUESTS as u32,
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 10_000,
                rate_limit_enabled: true,
                max_concurrency: TOTAL_REQUESTS as u32,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-1".into(),
        runtime_model_slug: "gpt-4.1-mini".into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::ImageDataUrl,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_load_profile(&state, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
    let app = build_router(state);
    let direct_client = reqwest::Client::builder()
        .pool_max_idle_per_host(CONCURRENCY)
        .build()
        .unwrap();
    let direct_url = format!("http://{upstream_addr}/v1/responses");
    let text_request_body = responses_request_body(None);
    let image_request_body = responses_request_body(Some(INLINE_IMAGE_BASELINE));

    let mut direct_latencies = run_direct_first_event_round(
        direct_client.clone(),
        direct_url.clone(),
        text_request_body.clone(),
        BaselineRound::Text,
        TOTAL_REQUESTS,
        CONCURRENCY,
    )
    .await;
    let mut gateway_latencies = run_gateway_first_event_round(
        app.clone(),
        downstream_key.plaintext.clone(),
        text_request_body,
        BaselineRound::Text,
        TOTAL_REQUESTS,
        CONCURRENCY,
    )
    .await;
    let mut image_direct_latencies = run_direct_first_event_round(
        direct_client,
        direct_url,
        image_request_body.clone(),
        BaselineRound::Image,
        TOTAL_REQUESTS,
        CONCURRENCY,
    )
    .await;
    let mut image_gateway_latencies = run_gateway_first_event_round(
        app,
        downstream_key.plaintext,
        image_request_body,
        BaselineRound::Image,
        TOTAL_REQUESTS,
        CONCURRENCY,
    )
    .await;

    assert_eq!(direct_latencies.len(), TOTAL_REQUESTS);
    assert_eq!(gateway_latencies.len(), TOTAL_REQUESTS);
    assert_eq!(image_direct_latencies.len(), TOTAL_REQUESTS);
    assert_eq!(image_gateway_latencies.len(), TOTAL_REQUESTS);
    assert_eq!(
        upstream_hits.load(Ordering::SeqCst),
        TOTAL_REQUESTS * 4,
        "each direct and gateway request must hit the mock exactly once"
    );

    let direct_p50_ms = percentile(&mut direct_latencies, 50);
    let direct_p95_ms = percentile(&mut direct_latencies, 95);
    let gateway_p50_ms = percentile(&mut gateway_latencies, 50);
    let gateway_p95_ms = percentile(&mut gateway_latencies, 95);
    let image_direct_p95_ms = percentile(&mut image_direct_latencies, 95);
    let image_gateway_p95_ms = percentile(&mut image_gateway_latencies, 95);
    let baseline = FirstEventBaseline {
        revision,
        direct_p50_ms,
        direct_p95_ms,
        gateway_p50_ms,
        gateway_p95_ms,
        gateway_added_p95_ms: gateway_p95_ms as i64 - direct_p95_ms as i64,
        image_direct_p95_ms,
        image_gateway_p95_ms,
        image_gateway_added_p95_ms: image_gateway_p95_ms as i64 - image_direct_p95_ms as i64,
        direct_requests: direct_latencies.len() + image_direct_latencies.len(),
        gateway_requests: gateway_latencies.len() + image_gateway_latencies.len(),
    };

    println!("{}", serde_json::to_string(&baseline).unwrap());
    if let Ok(path) = std::env::var("PROTOCOL_BASELINE_OUTPUT") {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, serde_json::to_vec_pretty(&baseline).unwrap()).unwrap();
    }
}

#[tokio::test]
#[ignore]
async fn load_gateway_first_meaningful_event() {
    const TOTAL_REQUESTS: usize = 100;
    const CONCURRENCY: usize = 20;

    let baseline: FirstEventBaseline = serde_json::from_slice(include_bytes!(
        "../docs/verification/2026-07-10-agent-protocol-baseline.json"
    ))
    .unwrap();
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let upstream_hits = Arc::new(AtomicUsize::new(0));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let upstream_hits_clone = upstream_hits.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move || {
            let upstream_hits_clone = upstream_hits_clone.clone();
            async move {
                upstream_hits_clone.fetch_add(1, Ordering::SeqCst);
                let first_event = stream::once(async {
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
                    ))
                });
                let terminal_event = stream::once(async {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n"))
                });

                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(first_event.chain(terminal_event)),
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
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{upstream_addr}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                protocols: vec![UpstreamProtocol::Responses],
                supported_models: vec!["gpt-4.1-mini".into()],
                request_quota_window_hours: 5,
                request_quota_requests: 10_000,
                requests_per_minute: 10_000,
                max_concurrency: TOTAL_REQUESTS as u32,
                active: true,
                failure_count: 0,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 10_000,
                rate_limit_enabled: true,
                max_concurrency: TOTAL_REQUESTS as u32,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        state_path,
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-1".into(),
        runtime_model_slug: "gpt-4.1-mini".into(),
        protocol: WireProtocol::Responses,
    });
    profile.state = DialectProfileState::Verified;
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::ImageDataUrl,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    stamp_load_profile(&state, &mut profile).await;
    state.upsert_dialect_profile(profile).await.unwrap();
    let app = build_router(state);
    let direct_client = reqwest::Client::builder()
        .pool_max_idle_per_host(CONCURRENCY)
        .build()
        .unwrap();
    let direct_url = format!("http://{upstream_addr}/v1/responses");

    let text_request_body = responses_request_body(None);
    let image_request_body = responses_request_body(Some(INLINE_IMAGE_BASELINE));

    let mut comparison = LatencyComparison {
        direct_ms: run_direct_first_event_round(
            direct_client.clone(),
            direct_url.clone(),
            text_request_body.clone(),
            BaselineRound::Text,
            TOTAL_REQUESTS,
            CONCURRENCY,
        )
        .await,
        gateway_ms: run_gateway_first_event_round(
            app.clone(),
            downstream_key.plaintext.clone(),
            text_request_body,
            BaselineRound::Text,
            TOTAL_REQUESTS,
            CONCURRENCY,
        )
        .await,
    };
    println!(
        "text first-event comparison: direct_p95_ms={} gateway_p95_ms={} gateway_added_p95_ms={}",
        percentile(&mut comparison.direct_ms.clone(), 95),
        percentile(&mut comparison.gateway_ms.clone(), 95),
        comparison.gateway_added_p95_ms(),
    );
    assert!(
        comparison.gateway_added_p95_ms() < 50,
        "gateway-added first meaningful event P95 must remain below 50 ms"
    );
    assert!(
        comparison.gateway_added_p95_ms() <= baseline.gateway_added_p95_ms + 10,
        "gateway-added P95 regressed by more than the 10 ms measurement allowance"
    );

    let mut image_comparison = LatencyComparison {
        direct_ms: run_direct_first_event_round(
            direct_client,
            direct_url,
            image_request_body.clone(),
            BaselineRound::Image,
            TOTAL_REQUESTS,
            CONCURRENCY,
        )
        .await,
        gateway_ms: run_gateway_first_event_round(
            app,
            downstream_key.plaintext,
            image_request_body,
            BaselineRound::Image,
            TOTAL_REQUESTS,
            CONCURRENCY,
        )
        .await,
    };
    println!(
        "image first-event comparison: direct_p95_ms={} gateway_p95_ms={} gateway_added_p95_ms={}",
        percentile(&mut image_comparison.direct_ms.clone(), 95),
        percentile(&mut image_comparison.gateway_ms.clone(), 95),
        image_comparison.gateway_added_p95_ms(),
    );
    assert!(image_comparison.gateway_added_p95_ms() < 50);
    assert!(image_comparison.gateway_added_p95_ms() <= baseline.image_gateway_added_p95_ms + 10);
    assert_eq!(
        upstream_hits.load(Ordering::SeqCst),
        TOTAL_REQUESTS * 4,
        "direct and gateway rounds must each make one healthy attempt"
    );
}
