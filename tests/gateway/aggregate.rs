use super::common::*;
use axum::response::{IntoResponse, Response};
use chat_responses_codex::capabilities::{
    Capability, DialectProfileKey, DialectProfileState, EvidenceState, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::protocol::stream_aggregate::{
    MAX_STREAM_AGGREGATE_FRAME_BYTES, MAX_STREAM_AGGREGATE_TOTAL_BYTES,
};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::task::Poll;
use tokio::sync::Notify;

const MODEL: &str = "opaque/aggregate";
const FALLBACK_MODEL: &str = "opaque/aggregate-long";
const UPSTREAM_ID: &str = "up-aggregate";
const DOWNSTREAM_ID: &str = "down-aggregate";
const PARTIAL_RESPONSE_ID: &str = "resp-aggregate-partial";

#[derive(Clone)]
enum AggregateScript {
    Pending,
    Fixed(Vec<Bytes>),
    RetryThenPending,
    RecoverThenPending,
}

struct AggregateHarness {
    app: Router,
    state: AppState,
    downstream: DownstreamConfig,
    downstream_key: String,
    pending_polled: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    _tempdir: tempfile::TempDir,
}

struct ContextFallbackHarness {
    app: Router,
    state: AppState,
    downstream: DownstreamConfig,
    downstream_key: String,
    pending_polled: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<(String, bool)>>>,
    _tempdir: tempfile::TempDir,
}

impl ContextFallbackHarness {
    async fn new(source_nonstream: EvidenceState, fallback_nonstream: EvidenceState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pending_polled = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let upstream_app = Router::new().route(
            "/v1/responses",
            post({
                let pending_polled = pending_polled.clone();
                let requests = requests.clone();
                move |request: Request<Body>| {
                    let pending_polled = pending_polled.clone();
                    let requests = requests.clone();
                    async move {
                        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&bytes).unwrap();
                        let runtime_model = payload["model"].as_str().unwrap().to_string();
                        let stream = payload["stream"] == true;
                        requests.lock().unwrap().push((runtime_model, stream));
                        if stream {
                            pending_aggregate_response(pending_polled)
                        } else {
                            pending_json_response(pending_polled)
                        }
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let upstream = UpstreamConfig {
            id: UPSTREAM_ID.into(),
            name: "aggregate-upstream".into(),
            base_url: format!("http://{address}"),
            api_key: "aggregate-secret".into(),
            protocol: UpstreamProtocol::Responses,
            protocols: vec![UpstreamProtocol::Responses],
            supported_models: vec![MODEL.into(), FALLBACK_MODEL.into()],
            model_contexts: vec![
                ModelContextConfig {
                    slug: MODEL.into(),
                    context_limit: 220,
                    output_reserve: 80,
                    max_output_tokens: 0,
                    context_group: "aggregate-group".into(),
                },
                ModelContextConfig {
                    slug: FALLBACK_MODEL.into(),
                    context_limit: 1_200,
                    output_reserve: 80,
                    max_output_tokens: 0,
                    context_group: "aggregate-group".into(),
                },
            ],
            request_quota_window_hours: 24,
            request_quota_requests: 1_000,
            requests_per_minute: 60,
            max_concurrency: 1,
            active: true,
            failure_count: 3,
            ..Default::default()
        };
        let downstream = downstream_config(&downstream_key, 1);
        let tempdir = tempdir().unwrap();
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                downstreams: vec![downstream.clone()],
                usage_logs: vec![],
                announcement: None,
                global_context_profiles: HashMap::new(),
            },
            tempdir.path().join("state.json"),
            AppConfig::default(),
        );
        for (runtime_model, nonstream) in [
            (MODEL, source_nonstream),
            (FALLBACK_MODEL, fallback_nonstream),
        ] {
            let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
                key_fingerprint: upstream_model_key_fingerprint(&upstream, runtime_model),
                upstream_id: UPSTREAM_ID.into(),
                runtime_model_slug: runtime_model.into(),
                protocol: WireProtocol::Responses,
            });
            profile.state = DialectProfileState::Verified;
            profile.configuration_fingerprint = state
                .route_configuration_fingerprint(
                    &upstream,
                    &profile.key.key_fingerprint,
                    MODEL,
                    runtime_model,
                    UpstreamProtocol::Responses,
                )
                .unwrap();
            profile
                .capabilities
                .insert(Capability::NonStreamingResponse, nonstream);
            profile
                .capabilities
                .insert(Capability::TextStream, EvidenceState::Supported);
            state.upsert_dialect_profile(profile).await.unwrap();
        }

        Self {
            app: build_router(state.clone()),
            state,
            downstream,
            downstream_key: downstream_key.plaintext,
            pending_polled,
            requests,
            _tempdir: tempdir,
        }
    }

    fn request(&self) -> Request<Body> {
        responses_request(&self.downstream_key, "A".repeat(1_800))
    }
}

impl AggregateHarness {
    async fn pending() -> Self {
        Self::new(
            AggregateScript::Pending,
            &["aggregate-secret"],
            AppConfig::default(),
        )
        .await
    }

    async fn new(script: AggregateScript, api_keys: &[&str], config: AppConfig) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pending_polled = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let upstream_app = Router::new().route(
            "/v1/responses",
            post({
                let pending_polled = pending_polled.clone();
                let requests = requests.clone();
                let script = script.clone();
                move |request: Request<Body>| {
                    let pending_polled = pending_polled.clone();
                    let requests = requests.clone();
                    let script = script.clone();
                    async move {
                        let authorization = request
                            .headers()
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                        let payload: Value = serde_json::from_slice(&bytes).unwrap();
                        let request_stream = payload["stream"] == true;
                        requests
                            .lock()
                            .unwrap()
                            .push((authorization.clone(), payload));
                        match script {
                            AggregateScript::Pending => pending_aggregate_response(pending_polled),
                            AggregateScript::Fixed(chunks) => fixed_aggregate_response(chunks),
                            AggregateScript::RetryThenPending
                                if authorization == "Bearer key-first" =>
                            {
                                fixed_aggregate_response(vec![
                                    Bytes::from(nonterminal_responses_sse()),
                                    Bytes::from_static(b"data: {not-json}\n\n"),
                                ])
                            }
                            AggregateScript::RetryThenPending => {
                                pending_aggregate_response(pending_polled)
                            }
                            AggregateScript::RecoverThenPending if !request_stream => (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "id": "resp-empty-recovery",
                                    "object": "response",
                                    "created_at": 1,
                                    "status": "completed",
                                    "model": MODEL,
                                    "output": [],
                                    "usage": {
                                        "input_tokens": 0,
                                        "output_tokens": 0,
                                        "total_tokens": 0
                                    }
                                })),
                            )
                                .into_response(),
                            AggregateScript::RecoverThenPending => {
                                pending_aggregate_response(pending_polled)
                            }
                        }
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let downstream_key = generate_downstream_key("gw");
        let upstream = UpstreamConfig {
            id: UPSTREAM_ID.into(),
            name: "aggregate-upstream".into(),
            base_url: format!("http://{address}"),
            api_key: api_keys
                .first()
                .filter(|_| api_keys.len() == 1)
                .copied()
                .unwrap_or_default()
                .into(),
            api_keys: if api_keys.len() > 1 {
                api_keys.iter().map(|key| (*key).to_string()).collect()
            } else {
                Vec::new()
            },
            protocol: UpstreamProtocol::Responses,
            protocols: vec![UpstreamProtocol::Responses],
            supported_models: vec![MODEL.into()],
            request_quota_window_hours: 24,
            request_quota_requests: 1_000,
            requests_per_minute: 60,
            max_concurrency: 1,
            active: true,
            failure_count: 3,
            ..Default::default()
        };
        let downstream = DownstreamConfig {
            id: DOWNSTREAM_ID.into(),
            name: "aggregate-client".into(),
            hash: downstream_key.hash.clone(),
            plaintext_key: Some(downstream_key.plaintext.clone()),
            plaintext_key_prefix: None,
            model_allowlist: vec![MODEL.into()],
            rate_limit_enabled: true,
            per_minute_limit: 60,
            max_concurrency: 1,
            daily_token_limit: None,
            monthly_token_limit: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
        };
        let tempdir = tempdir().unwrap();
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                downstreams: vec![downstream.clone()],
                usage_logs: vec![],
                announcement: None,
                global_context_profiles: HashMap::new(),
            },
            tempdir.path().join("state.json"),
            config,
        );
        for api_key in upstream.keys_for_model(MODEL) {
            let key_fingerprint =
                chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, &api_key);
            let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
                key_fingerprint,
                upstream_id: UPSTREAM_ID.into(),
                runtime_model_slug: MODEL.into(),
                protocol: WireProtocol::Responses,
            });
            profile.state = DialectProfileState::Verified;
            profile.configuration_fingerprint = state
                .route_configuration_fingerprint(
                    &upstream,
                    &profile.key.key_fingerprint,
                    MODEL,
                    MODEL,
                    UpstreamProtocol::Responses,
                )
                .unwrap();
            if matches!(&script, AggregateScript::RecoverThenPending) {
                assert!(profile.capabilities.is_empty());
            } else {
                profile
                    .capabilities
                    .insert(Capability::NonStreamingResponse, EvidenceState::Rejected);
                profile
                    .capabilities
                    .insert(Capability::TextStream, EvidenceState::Supported);
                assert_eq!(profile.capabilities.len(), 2);
            }
            state.upsert_dialect_profile(profile).await.unwrap();
        }

        Self {
            app: build_router(state.clone()),
            state,
            downstream,
            downstream_key: downstream_key.plaintext,
            pending_polled,
            requests,
            _tempdir: tempdir,
        }
    }

    fn request(&self) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.downstream_key),
            )
            .body(Body::from(
                json!({
                    "model": MODEL,
                    "input": "wait for the complete response",
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap()
    }

    async fn wait_until_pending(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !self.pending_polled.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aggregate upstream body should reach its pending tail");
    }

    async fn assert_cleanup(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let upstream_released = self
                    .state
                    .upstream_runtime_snapshots()
                    .await
                    .get(UPSTREAM_ID)
                    .is_some_and(|runtime| runtime.in_flight == 0);
                if upstream_released && self.state.active_gateway_requests(None).is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aggregate request should release runtime state");

        assert_eq!(
            self.state
                .upstream_runtime_snapshots()
                .await
                .get(UPSTREAM_ID)
                .unwrap()
                .in_flight,
            0
        );
        assert!(self.state.active_gateway_requests(None).is_empty());
        self.state
            .try_reserve_downstream_concurrency(&self.downstream)
            .expect("downstream concurrency should return to zero");
        assert!(
            self.state
                .try_reserve_downstream_concurrency(&self.downstream)
                .is_err(),
            "max_concurrency=1 must make the zero-state probe exact"
        );
        self.state
            .release_downstream_concurrency(&self.downstream.id);
        assert!(
            self.state
                .response_history(PARTIAL_RESPONSE_ID)
                .await
                .is_none(),
            "nonterminal aggregate output must not be stored"
        );
    }

    async fn wait_for_one_log(&self) {
        let result = tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let count = self.state.snapshot().await.usage_logs.len();
                if count == 1 {
                    return;
                }
                assert!(count < 1, "aggregate request emitted duplicate logs");
                tokio::task::yield_now().await;
            }
        })
        .await;
        if result.is_err() {
            let count = self.state.snapshot().await.usage_logs.len();
            panic!("aggregate request should emit one usage log; saw {count}");
        }
    }
}

fn downstream_config(
    downstream_key: &chat_responses_codex::keys::GeneratedDownstreamKey,
    max_concurrency: u32,
) -> DownstreamConfig {
    DownstreamConfig {
        id: DOWNSTREAM_ID.into(),
        name: "aggregate-client".into(),
        hash: downstream_key.hash.clone(),
        plaintext_key: Some(downstream_key.plaintext.clone()),
        plaintext_key_prefix: None,
        model_allowlist: vec![MODEL.into()],
        rate_limit_enabled: true,
        per_minute_limit: 60,
        max_concurrency,
        daily_token_limit: None,
        monthly_token_limit: None,
        request_quota_window_hours: None,
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
    }
}

fn responses_request(downstream_key: &str, input: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {downstream_key}"))
        .body(Body::from(
            json!({
                "model": MODEL,
                "max_output_tokens": 80,
                "input": input,
                "stream": false
            })
            .to_string(),
        ))
        .unwrap()
}

async fn wait_until_pending(pending_polled: &AtomicBool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !pending_polled.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual upstream response should reach its pending tail");
}

async fn assert_cancelled_request_cleanup(
    state: &AppState,
    downstream: &DownstreamConfig,
    expected_downstream_capacity: u32,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let upstream_released = state
                .upstream_runtime_snapshots()
                .await
                .get(UPSTREAM_ID)
                .is_some_and(|runtime| runtime.in_flight == 0);
            if upstream_released && state.active_gateway_requests(None).is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual-mode cancellation should release runtime state");

    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .in_flight,
        0
    );
    assert!(state.active_gateway_requests(None).is_empty());
    for _ in 0..expected_downstream_capacity {
        state
            .try_reserve_downstream_concurrency(downstream)
            .expect("downstream concurrency should return to zero");
    }
    assert!(
        state
            .try_reserve_downstream_concurrency(downstream)
            .is_err(),
        "downstream capacity probe must reach the configured limit"
    );
    for _ in 0..expected_downstream_capacity {
        state.release_downstream_concurrency(&downstream.id);
    }
    assert!(state.response_history(PARTIAL_RESPONSE_ID).await.is_none());
}

async fn preset_and_capture_upstream_health(state: &AppState) -> u64 {
    let failure_count = state.snapshot().await.upstreams[0].failure_count;
    assert!(failure_count <= 3);
    for _ in failure_count..3 {
        state.mark_upstream_failure(UPSTREAM_ID).await.unwrap();
    }
    state.mark_upstream_rate_limited(UPSTREAM_ID, 60).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].failure_count, 3);
    state
        .upstream_runtime_snapshots()
        .await
        .get(UPSTREAM_ID)
        .unwrap()
        .cooldown_until
}

async fn assert_upstream_health_unchanged(state: &AppState, cooldown_until: u64) {
    assert_eq!(state.snapshot().await.upstreams[0].failure_count, 3);
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .cooldown_until,
        cooldown_until
    );
}

async fn wait_for_one_aggregate_cancellation_log(state: &AppState) {
    let result = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let snapshot = state.snapshot().await;
            let count = snapshot
                .usage_logs
                .iter()
                .filter(|log| log.error_category.as_deref() == Some("stream_client_cancelled"))
                .count();
            if count == 1 {
                break;
            }
            assert!(count < 1, "aggregate cancellation emitted duplicate logs");
            tokio::task::yield_now().await;
        }
    })
    .await;
    if result.is_err() {
        let snapshot = state.snapshot().await;
        let count = snapshot
            .usage_logs
            .iter()
            .filter(|log| log.error_category.as_deref() == Some("stream_client_cancelled"))
            .count();
        panic!("expected one aggregate cancellation log; saw {count}");
    }

    let snapshot = state.snapshot().await;
    let logs = snapshot
        .usage_logs
        .iter()
        .filter(|log| log.error_category.as_deref() == Some("stream_client_cancelled"))
        .collect::<Vec<_>>();
    assert_eq!(logs.len(), 1);
    let log = logs[0];
    assert_eq!(log.status_code, 499);
    assert_eq!(log.upstream_key_id, UPSTREAM_ID);
    assert_eq!(
        (log.prompt_tokens, log.completion_tokens, log.total_tokens),
        (0, 0, 0)
    );
}

async fn assert_no_aggregate_cancellation_log(state: &AppState) {
    tokio::task::yield_now().await;
    let snapshot = state.snapshot().await;
    assert!(snapshot
        .usage_logs
        .iter()
        .all(|log| log.error_category.as_deref() != Some("stream_client_cancelled")));
}

fn pending_aggregate_response(pending_polled: Arc<AtomicBool>) -> Response {
    let first = stream::once(async {
        Ok::<Bytes, std::io::Error>(Bytes::from(nonterminal_responses_sse()))
    });
    let pending = stream::poll_fn(move |_cx| {
        pending_polled.store(true, Ordering::SeqCst);
        Poll::<Option<Result<Bytes, std::io::Error>>>::Pending
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(first.chain(pending)),
    )
        .into_response()
}

fn pending_json_response(pending_polled: Arc<AtomicBool>) -> Response {
    let pending = stream::poll_fn(move |_cx| {
        pending_polled.store(true, Ordering::SeqCst);
        Poll::<Option<Result<Bytes, std::io::Error>>>::Pending
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from_stream(pending),
    )
        .into_response()
}

fn fixed_aggregate_response(chunks: Vec<Bytes>) -> Response {
    let chunks = stream::iter(chunks.into_iter().map(Ok::<Bytes, std::io::Error>));
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(chunks),
    )
        .into_response()
}

fn nonterminal_responses_sse() -> String {
    let created = json!({
        "type": "response.created",
        "response": {
            "id": PARTIAL_RESPONSE_ID,
            "object": "response",
            "created_at": 1,
            "status": "in_progress",
            "model": MODEL,
            "output": []
        }
    });
    let partial = json!({
        "type": "response.output_text.delta",
        "output_index": 0,
        "content_index": 0,
        "delta": "partial"
    });
    format!(
        "event: response.created\ndata: {created}\n\nevent: response.output_text.delta\ndata: {partial}\n\n"
    )
}

fn oversized_frame_fixture() -> Bytes {
    let target_len = MAX_STREAM_AGGREGATE_FRAME_BYTES + 1;
    let mut frame = Vec::with_capacity(target_len);
    frame.extend_from_slice(b"data: ");
    frame.resize(target_len - 2, b'x');
    frame.extend_from_slice(b"\n\n");
    Bytes::from(frame)
}

fn maximum_sized_nonterminal_frame_fixture() -> Bytes {
    let created = json!({
        "type": "response.created",
        "response": {
            "id": PARTIAL_RESPONSE_ID,
            "object": "response",
            "created_at": 1,
            "status": "in_progress",
            "model": MODEL,
            "output": []
        }
    });
    let prefix = format!("data: {created}\n:");
    assert!(prefix.len() + 2 <= MAX_STREAM_AGGREGATE_FRAME_BYTES);
    let mut frame = Vec::with_capacity(MAX_STREAM_AGGREGATE_FRAME_BYTES);
    frame.extend_from_slice(prefix.as_bytes());
    frame.resize(MAX_STREAM_AGGREGATE_FRAME_BYTES - 2, b'x');
    frame.extend_from_slice(b"\n\n");
    Bytes::from(frame)
}

fn empty_responses_json_for_learning() -> Response {
    (
        StatusCode::OK,
        axum::Json(json!({
            "id": "resp-empty-learning",
            "object": "response",
            "created_at": 1,
            "status": "completed",
            "model": MODEL,
            "output": [],
            "usage": {"input_tokens": 1, "output_tokens": 0, "total_tokens": 1}
        })),
    )
        .into_response()
}

fn completed_responses_sse_for_learning() -> Response {
    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": "resp-singleflight-leader",
            "object": "response",
            "created_at": 1,
            "status": "completed",
            "model": MODEL,
            "output": [{
                "id": "msg-singleflight-leader",
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "learned",
                    "annotations": []
                }]
            }],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from(format!(
            "event: response.completed\ndata: {completed}\n\ndata: [DONE]\n\n"
        )),
    )
        .into_response()
}

async fn run_context_fallback_cancellation(
    source_nonstream: EvidenceState,
    fallback_nonstream: EvidenceState,
    expected_actual_stream: bool,
    expect_cancellation_log: bool,
) {
    let harness = ContextFallbackHarness::new(source_nonstream, fallback_nonstream).await;
    let request = tokio::spawn(harness.app.clone().oneshot(harness.request()));

    wait_until_pending(&harness.pending_polled).await;
    assert_eq!(
        harness.requests.lock().unwrap().as_slice(),
        [(FALLBACK_MODEL.to_string(), expected_actual_stream)]
    );
    assert_eq!(harness.state.active_gateway_requests(None).len(), 1);
    assert_eq!(
        harness
            .state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .in_flight,
        1
    );
    let cooldown_until = preset_and_capture_upstream_health(&harness.state).await;

    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert_cancelled_request_cleanup(&harness.state, &harness.downstream, 1).await;
    assert_upstream_health_unchanged(&harness.state, cooldown_until).await;
    if expect_cancellation_log {
        wait_for_one_aggregate_cancellation_log(&harness.state).await;
    } else {
        assert_no_aggregate_cancellation_log(&harness.state).await;
    }
}

#[tokio::test]
async fn aggregate_learned_singleflight_follower_uses_actual_mode_for_cancellation_log() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let first_json_started = Arc::new(Notify::new());
    let first_json_release = Arc::new(Notify::new());
    let pending_polled = Arc::new(AtomicBool::new(false));
    let json_attempts = Arc::new(AtomicUsize::new(0));
    let sse_attempts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let upstream_app = Router::new().route(
        "/v1/responses",
        post({
            let first_json_started = first_json_started.clone();
            let first_json_release = first_json_release.clone();
            let pending_polled = pending_polled.clone();
            let json_attempts = json_attempts.clone();
            let sse_attempts = sse_attempts.clone();
            let requests = requests.clone();
            move |request: Request<Body>| {
                let first_json_started = first_json_started.clone();
                let first_json_release = first_json_release.clone();
                let pending_polled = pending_polled.clone();
                let json_attempts = json_attempts.clone();
                let sse_attempts = sse_attempts.clone();
                let requests = requests.clone();
                async move {
                    let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    let payload: Value = serde_json::from_slice(&bytes).unwrap();
                    let runtime_model = payload["model"].as_str().unwrap().to_string();
                    let stream = payload["stream"] == true;
                    requests.lock().unwrap().push((runtime_model, stream));
                    if !stream {
                        assert_eq!(json_attempts.fetch_add(1, Ordering::SeqCst), 0);
                        first_json_started.notify_one();
                        first_json_release.notified().await;
                        return empty_responses_json_for_learning();
                    }
                    if sse_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        completed_responses_sse_for_learning()
                    } else {
                        pending_aggregate_response(pending_polled)
                    }
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: UPSTREAM_ID.into(),
        name: "aggregate-upstream".into(),
        base_url: format!("http://{address}"),
        api_key: "aggregate-secret".into(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec![MODEL.into()],
        request_quota_window_hours: 24,
        request_quota_requests: 1_000,
        requests_per_minute: 60,
        max_concurrency: 2,
        active: true,
        failure_count: 3,
        ..Default::default()
    };
    let downstream = downstream_config(&downstream_key, 2);
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            downstreams: vec![downstream.clone()],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: HashMap::new(),
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: upstream_model_key_fingerprint(&upstream, MODEL),
        upstream_id: UPSTREAM_ID.into(),
        runtime_model_slug: MODEL.into(),
        protocol: WireProtocol::Responses,
    });
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            &profile.key.key_fingerprint,
            MODEL,
            MODEL,
            UpstreamProtocol::Responses,
        )
        .unwrap();
    state.upsert_dialect_profile(profile).await.unwrap();

    let app = build_router(state.clone());
    let leader = tokio::spawn(app.clone().oneshot(responses_request(
        &downstream_key.plaintext,
        "leader".into(),
    )));
    tokio::time::timeout(Duration::from_secs(2), first_json_started.notified())
        .await
        .expect("singleflight leader should reach its JSON request");
    let follower = tokio::spawn(app.oneshot(responses_request(
        &downstream_key.plaintext,
        "follower".into(),
    )));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let both_reserved = state
                .upstream_runtime_snapshots()
                .await
                .get(UPSTREAM_ID)
                .is_some_and(|runtime| runtime.in_flight == 2);
            if both_reserved
                && state.active_gateway_requests(Some(DOWNSTREAM_ID)).len() == 2
                && requests.lock().unwrap().len() == 1
            {
                tokio::task::yield_now().await;
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("singleflight follower should wait before dispatch");
    first_json_release.notify_waiters();

    wait_until_pending(&pending_polled).await;
    let leader_response = tokio::time::timeout(Duration::from_secs(2), leader)
        .await
        .expect("singleflight leader should finish learning")
        .unwrap()
        .unwrap();
    assert_eq!(leader_response.status(), StatusCode::OK);
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        [
            (MODEL.to_string(), false),
            (MODEL.to_string(), true),
            (MODEL.to_string(), true),
        ]
    );
    let learned = state.capability_snapshot();
    let learned_profile = learned
        .profiles
        .get(&DialectProfileKey {
            key_fingerprint: upstream_model_key_fingerprint(&upstream, MODEL),
            upstream_id: UPSTREAM_ID.into(),
            runtime_model_slug: MODEL.into(),
            protocol: WireProtocol::Responses,
        })
        .unwrap();
    assert_eq!(
        learned_profile
            .capabilities
            .get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        learned_profile.capabilities.get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );
    assert_eq!(state.active_gateway_requests(None).len(), 1);
    let cooldown_until = preset_and_capture_upstream_health(&state).await;

    follower.abort();
    assert!(follower.await.unwrap_err().is_cancelled());
    assert_cancelled_request_cleanup(&state, &downstream, 2).await;
    assert_upstream_health_unchanged(&state, cooldown_until).await;
    wait_for_one_aggregate_cancellation_log(&state).await;
}

#[tokio::test]
async fn aggregate_context_fallback_outer_aggregate_actual_json_does_not_log_cancellation() {
    run_context_fallback_cancellation(
        EvidenceState::Rejected,
        EvidenceState::Supported,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn aggregate_context_fallback_outer_json_actual_aggregate_logs_cancellation() {
    run_context_fallback_cancellation(
        EvidenceState::Supported,
        EvidenceState::Rejected,
        true,
        true,
    )
    .await;
}

async fn assert_aggregate_fault(
    harness: &AggregateHarness,
    expected_status: StatusCode,
    expected_category: &str,
) {
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        harness.app.clone().oneshot(harness.request()),
    )
    .await
    .expect("aggregate fault request should finish")
    .unwrap();
    assert_eq!(response.status(), expected_status);
    let payload: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(payload["error"]["code"], expected_category);
    assert_eq!(payload["error"]["category"], expected_category);
    assert_eq!(harness.requests.lock().unwrap().len(), 1);
    assert_eq!(harness.requests.lock().unwrap()[0].1["stream"], true);

    harness.assert_cleanup().await;
    harness.wait_for_one_log().await;
    let snapshot = harness.state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, expected_status.as_u16());
    assert_eq!(log.error_category.as_deref(), Some(expected_category));
    assert_eq!(log.upstream_key_id, UPSTREAM_ID);
    assert_eq!(
        (log.prompt_tokens, log.completion_tokens, log.total_tokens),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn aggregate_malformed_sse_cleans_up_and_logs_one_decode_error() {
    let harness = AggregateHarness::new(
        AggregateScript::Fixed(vec![
            Bytes::from(nonterminal_responses_sse()),
            Bytes::from_static(b"data: {not-json}\n\n"),
        ]),
        &["aggregate-secret"],
        AppConfig::default(),
    )
    .await;

    assert_aggregate_fault(
        &harness,
        StatusCode::BAD_GATEWAY,
        "upstream_stream_decode_error",
    )
    .await;
}

#[tokio::test]
async fn aggregate_oversized_frame_cleans_up_and_logs_one_limit_error() {
    let oversized_frame = oversized_frame_fixture();
    assert_eq!(oversized_frame.len(), MAX_STREAM_AGGREGATE_FRAME_BYTES + 1);
    assert!(oversized_frame.len() > MAX_STREAM_AGGREGATE_FRAME_BYTES);
    assert!(oversized_frame.ends_with(b"\n\n"));
    let harness = AggregateHarness::new(
        AggregateScript::Fixed(vec![
            Bytes::from(nonterminal_responses_sse()),
            oversized_frame,
        ]),
        &["aggregate-secret"],
        AppConfig::default(),
    )
    .await;

    assert_aggregate_fault(
        &harness,
        StatusCode::BAD_GATEWAY,
        "upstream_stream_limit_exceeded",
    )
    .await;
}

#[tokio::test]
async fn aggregate_oversized_total_cleans_up_and_logs_one_limit_error() {
    let frame = maximum_sized_nonterminal_frame_fixture();
    let frames = vec![frame; 17];
    assert_eq!(frames.len(), 17);
    assert!(frames
        .iter()
        .all(|frame| frame.len() == MAX_STREAM_AGGREGATE_FRAME_BYTES));
    let total_fixture_bytes = frames.iter().map(Bytes::len).sum::<usize>();
    assert_eq!(total_fixture_bytes, 17 * MAX_STREAM_AGGREGATE_FRAME_BYTES);
    assert!(total_fixture_bytes > MAX_STREAM_AGGREGATE_TOTAL_BYTES);
    let harness = AggregateHarness::new(
        AggregateScript::Fixed(frames),
        &["aggregate-secret"],
        AppConfig::default(),
    )
    .await;

    assert_aggregate_fault(
        &harness,
        StatusCode::BAD_GATEWAY,
        "upstream_stream_limit_exceeded",
    )
    .await;
}

#[tokio::test]
async fn aggregate_idle_timeout_cleans_up_and_logs_one_timeout_error() {
    let config = AppConfig {
        upstream_stream_keepalive_interval_seconds: 60,
        upstream_stream_idle_timeout_seconds: 1,
        upstream_stream_max_duration_seconds: 60,
        ..AppConfig::default()
    };
    let harness =
        AggregateHarness::new(AggregateScript::Pending, &["aggregate-secret"], config).await;

    assert_aggregate_fault(&harness, StatusCode::GATEWAY_TIMEOUT, "stream_idle_timeout").await;
}

#[tokio::test]
async fn aggregate_cancellation_rearms_after_retryable_first_key_error() {
    let harness = AggregateHarness::new(
        AggregateScript::RetryThenPending,
        &["key-first", "key-second"],
        AppConfig::default(),
    )
    .await;
    let request = tokio::spawn(harness.app.clone().oneshot(harness.request()));

    harness.wait_until_pending().await;
    let captured = harness.requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].0, "Bearer key-first");
    assert_eq!(captured[1].0, "Bearer key-second");
    assert!(captured
        .iter()
        .all(|(_, payload)| payload["stream"] == true));
    assert_eq!(harness.state.active_gateway_requests(None).len(), 1);
    assert_eq!(
        harness
            .state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .in_flight,
        1
    );

    harness
        .state
        .mark_upstream_rate_limited(UPSTREAM_ID, 60)
        .await;
    let cooldown_before_cancel = harness
        .state
        .upstream_runtime_snapshots()
        .await
        .get(UPSTREAM_ID)
        .unwrap()
        .cooldown_until;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());

    harness.assert_cleanup().await;
    let snapshot = harness.state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].failure_count, 0);
    assert_eq!(
        harness
            .state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .cooldown_until,
        cooldown_before_cancel
    );
    harness.wait_for_one_log().await;
    let snapshot = harness.state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, 499);
    assert_eq!(
        log.error_category.as_deref(),
        Some("stream_client_cancelled")
    );
    assert_eq!(log.upstream_key_id, UPSTREAM_ID);
    assert_eq!(
        (log.prompt_tokens, log.completion_tokens, log.total_tokens),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn aggregate_cancellation_arms_after_json_to_sse_recovery() {
    let harness = AggregateHarness::new(
        AggregateScript::RecoverThenPending,
        &["aggregate-secret"],
        AppConfig::default(),
    )
    .await;
    let request = tokio::spawn(harness.app.clone().oneshot(harness.request()));

    harness.wait_until_pending().await;
    let captured = harness.requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].1["stream"], false);
    assert_eq!(captured[1].1["stream"], true);
    assert_eq!(harness.state.active_gateway_requests(None).len(), 1);

    harness
        .state
        .mark_upstream_rate_limited(UPSTREAM_ID, 60)
        .await;
    let cooldown_before_cancel = harness
        .state
        .upstream_runtime_snapshots()
        .await
        .get(UPSTREAM_ID)
        .unwrap()
        .cooldown_until;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());

    harness.assert_cleanup().await;
    assert!(
        harness
            .state
            .response_history(PARTIAL_RESPONSE_ID)
            .await
            .is_none(),
        "nonterminal aggregate output must not be stored"
    );
    assert!(
        harness
            .state
            .response_history("resp-empty-recovery")
            .await
            .is_none(),
        "failed empty-response recovery must not be stored"
    );
    let snapshot = harness.state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].failure_count, 0);
    assert_eq!(
        harness
            .state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .cooldown_until,
        cooldown_before_cancel
    );
    harness.wait_for_one_log().await;
    let snapshot = harness.state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, 499);
    assert_eq!(
        log.error_category.as_deref(),
        Some("stream_client_cancelled")
    );
    assert_eq!(log.upstream_key_id, UPSTREAM_ID);
    assert_eq!(
        (log.prompt_tokens, log.completion_tokens, log.total_tokens),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn aggregate_future_cancellation_cleans_up_and_logs_once_without_changing_health() {
    let harness = AggregateHarness::pending().await;
    let request = tokio::spawn(harness.app.clone().oneshot(harness.request()));

    harness.wait_until_pending().await;
    assert_eq!(harness.requests.lock().unwrap().len(), 1);
    assert_eq!(harness.requests.lock().unwrap()[0].1["stream"], true);
    assert_eq!(harness.state.active_gateway_requests(None).len(), 1);
    assert_eq!(
        harness
            .state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .in_flight,
        1
    );

    harness
        .state
        .mark_upstream_rate_limited(UPSTREAM_ID, 60)
        .await;
    let cooldown_before_cancel = harness
        .state
        .upstream_runtime_snapshots()
        .await
        .get(UPSTREAM_ID)
        .unwrap()
        .cooldown_until;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());

    harness.assert_cleanup().await;
    let snapshot = harness.state.snapshot().await;
    assert_eq!(snapshot.upstreams[0].failure_count, 0);
    assert_eq!(
        harness
            .state
            .upstream_runtime_snapshots()
            .await
            .get(UPSTREAM_ID)
            .unwrap()
            .cooldown_until,
        cooldown_before_cancel
    );

    harness.wait_for_one_log().await;
    let snapshot = harness.state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    let log = &snapshot.usage_logs[0];
    assert_eq!(log.status_code, 499);
    assert_eq!(
        log.error_category.as_deref(),
        Some("stream_client_cancelled")
    );
    assert_eq!(log.upstream_key_id, UPSTREAM_ID);
    assert_eq!(
        (log.prompt_tokens, log.completion_tokens, log.total_tokens),
        (0, 0, 0)
    );
}
