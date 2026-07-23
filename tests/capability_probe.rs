#[path = "gateway/common.rs"]
mod common;

use axum::response::{IntoResponse, Response};
use common::*;
use futures_util::stream;
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

use chat_responses_codex::capabilities::{
    apply_probe_outcome, AgentClientProfile, Capability, CapabilityConfiguration, CapabilityPolicy,
    CapabilitySelector, CompatibilityExpectation, DeclarativeProbeCase, DialectProfileKey,
    EvidenceState, HttpsImageFixture, PredicateOperator, ProbeCandidates, ProbeJob, ProbeOutcome,
    ProbeQueueState, ProbeReason, ReasoningCarrier, ResponsePredicate, RouteIdentity,
    TokenLimitField, UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::protocol::stream_aggregate::MAX_STREAM_AGGREGATE_TOTAL_BYTES;
use chat_responses_codex::server::{
    probe_plan_for_job, probe_plan_for_route, run_probe_plan_for_model_for_test,
    run_probe_plan_for_test, CapabilityProbeMockReply, CapabilityProbePlan, CapabilityProbeService,
};

#[test]
fn matching_expectation_adds_https_image_case_to_probe_plan() {
    let fixture = HttpsImageFixture {
        url: "https://fixtures.example.test/red.png".into(),
        expected_label: "red".into(),
    };
    let mut configuration = CapabilityConfiguration::default();
    configuration
        .compatibility_expectations
        .push(CompatibilityExpectation {
            id: "vision-route".into(),
            selector: CapabilitySelector {
                upstream_id: Some("up-vision".into()),
                runtime_model: Some("opaque/vision-model".into()),
                protocol: Some(WireProtocol::ChatCompletions),
                ..CapabilitySelector::default()
            },
            bundles: Default::default(),
            client_profiles: std::collections::BTreeSet::from([AgentClientProfile::Codex]),
            permitted_optional_downgrades: Default::default(),
            https_image_fixture: Some(fixture.clone()),
        });
    let compiled = Arc::new(configuration.compile().unwrap());
    let route = RouteIdentity {
        key_fingerprint: String::new(),
        upstream_id: "up-vision".into(),
        exposed_model_slug: "public-vision".into(),
        runtime_model_slug: "opaque/vision-model".into(),
        protocol: WireProtocol::ChatCompletions,
        tags: Default::default(),
    };

    let plan = probe_plan_for_route(&compiled, &route);

    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::ImageHttps {
            url,
            expected_label
        } if url.as_str() == fixture.url && expected_label.as_str() == fixture.expected_label
    )));
}

#[test]
fn responses_image_expectation_probes_https_and_data_url_inputs() {
    let mut configuration = CapabilityConfiguration::default();
    configuration
        .compatibility_expectations
        .push(CompatibilityExpectation {
            id: "responses-vision".into(),
            selector: CapabilitySelector {
                exposed_model: Some("public-vision".into()),
                protocol: Some(WireProtocol::Responses),
                ..Default::default()
            },
            bundles: Default::default(),
            client_profiles: Default::default(),
            permitted_optional_downgrades: Default::default(),
            https_image_fixture: Some(HttpsImageFixture {
                url: "https://fixtures.example.test/image.png".into(),
                expected_label: "red".into(),
            }),
        });
    let compiled = configuration.compile().unwrap();
    let route = RouteIdentity {
        key_fingerprint: String::new(),
        upstream_id: "up-vision".into(),
        exposed_model_slug: "public-vision".into(),
        runtime_model_slug: "opaque/vision".into(),
        protocol: WireProtocol::Responses,
        tags: Default::default(),
    };

    let plan = probe_plan_for_route(&compiled, &route);

    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::ImageDataUrl
    )));
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::ImageHttps { .. }
    )));
}

#[test]
fn matching_policy_adds_declared_candidates_and_extensions_to_probe_plan() {
    let extension = DeclarativeProbeCase {
        id: "service-tier".into(),
        protocol: WireProtocol::ChatCompletions,
        prerequisites: Default::default(),
        request_patch: json!({"service_tier": "auto"}),
        response_predicate: ResponsePredicate {
            path: "/accepted".into(),
            operator: PredicateOperator::Equals,
            value: Some(json!(true)),
        },
    };
    let configuration = CapabilityConfiguration {
        policies: vec![CapabilityPolicy {
            id: "synthetic-dialect".into(),
            selector: CapabilitySelector {
                runtime_model_glob: Some("lab/*".into()),
                protocol: Some(WireProtocol::ChatCompletions),
                ..Default::default()
            },
            probe_candidates: ProbeCandidates {
                token_limit_fields: vec![TokenLimitField::MaxCompletionTokens],
                reasoning_controls: std::collections::BTreeMap::from([(
                    "reasoning_effort".into(),
                    vec!["high".into()],
                )]),
                reasoning_carriers: vec![ReasoningCarrier::ReasoningContent],
            },
            extension_probes: vec![extension.clone()],
            ..Default::default()
        }],
        ..Default::default()
    }
    .compile()
    .unwrap();
    let route = RouteIdentity {
        key_fingerprint: String::new(),
        upstream_id: "up-1".into(),
        exposed_model_slug: "public".into(),
        runtime_model_slug: "lab/opaque".into(),
        protocol: WireProtocol::ChatCompletions,
        tags: Default::default(),
    };

    let plan = probe_plan_for_route(&configuration, &route);

    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::TokenLimit {
            field: TokenLimitField::MaxCompletionTokens
        }
    )));
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::ReasoningControl { field, value }
            if field == "reasoning_effort" && value == "high"
    )));
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::ToolContinuation {
            reasoning_carrier: Some(ReasoningCarrier::ReasoningContent)
        }
    )));
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::Declarative(case)
            if case == &extension
    )));
}

#[test]
fn agent_core_plan_probes_basic_tool_continuation() {
    let plan = CapabilityProbePlan::agent_core();
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::ToolContinuation {
            reasoning_carrier: None
        }
    )));
}

#[test]
fn queued_probe_preserves_exposed_model_for_fixture_selection() {
    let fixture = HttpsImageFixture {
        url: "https://fixtures.example.test/blue.png".into(),
        expected_label: "blue".into(),
    };
    let mut configuration = CapabilityConfiguration::default();
    configuration
        .compatibility_expectations
        .push(CompatibilityExpectation {
            id: "public-vision-route".into(),
            selector: CapabilitySelector {
                exposed_model: Some("public-vision-alias".into()),
                ..CapabilitySelector::default()
            },
            bundles: Default::default(),
            client_profiles: Default::default(),
            permitted_optional_downgrades: Default::default(),
            https_image_fixture: Some(fixture.clone()),
        });
    let compiled = Arc::new(configuration.compile().unwrap());
    let key = DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up-vision".into(),
        runtime_model_slug: "opaque/runtime-vision".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let first_job = ProbeJob {
        key: key.clone(),
        exposed_model_slugs: std::collections::BTreeSet::from(["plain-alias".into()]),
        reason: ProbeReason::Manual,
        configuration: chat_responses_codex::capabilities::ProbeConfigurationBinding {
            configuration_fingerprint: "test-fingerprint".into(),
            configuration_digest: "test-digest".into(),
            configuration_schema_version: 1,
            configuration_revision: 1,
            probe_schema_version: chat_responses_codex::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
        },
        plan_configuration: compiled.clone(),
    };
    let matching_job = ProbeJob {
        key,
        exposed_model_slugs: std::collections::BTreeSet::from(["public-vision-alias".into()]),
        reason: ProbeReason::Manual,
        configuration: chat_responses_codex::capabilities::ProbeConfigurationBinding {
            configuration_fingerprint: "test-fingerprint".into(),
            configuration_digest: "test-digest".into(),
            configuration_schema_version: 1,
            configuration_revision: 1,
            probe_schema_version: chat_responses_codex::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
        },
        plan_configuration: compiled.clone(),
    };
    let mut queue = ProbeQueueState::new(1, 1, usize::MAX);
    assert!(queue.enqueue(first_job));
    assert!(!queue.enqueue(matching_job));
    let merged_job = queue.start_next().unwrap();

    let plan = probe_plan_for_job(&merged_job);

    assert!(plan.cases.iter().any(|case| matches!(
        case,
        chat_responses_codex::server::CoreProbeCase::ImageHttps {
            url,
            expected_label
        } if url.as_str() == fixture.url && expected_label.as_str() == fixture.expected_label
    )));
}

#[derive(Clone)]
struct ProbeMock {
    base_url: String,
    capture: Arc<Mutex<Vec<Value>>>,
    request_count: Arc<AtomicUsize>,
}

impl ProbeMock {
    async fn chat(handler: impl Fn(Value) -> Value + Send + Sync + 'static) -> Self {
        Self::responding(move |payload| {
            (StatusCode::OK, axum::Json(handler(payload))).into_response()
        })
        .await
    }

    async fn responding(handler: impl Fn(Value) -> Response + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let capture_clone = capture.clone();
        let request_count_clone = request_count.clone();
        let handler = Arc::new(handler);
        let handler_clone = handler.clone();

        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                let request_count = request_count_clone.clone();
                let handler = handler_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    request_count.fetch_add(1, Ordering::SeqCst);
                    capture.lock().unwrap().push(payload.clone());
                    (handler)(payload)
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            base_url: format!("http://{}", address),
            capture,
            request_count,
        }
    }

    async fn status(status: StatusCode) -> Self {
        Self::status_with_body(status, json!({"error": {"message": "denied"}})).await
    }

    async fn status_with_body(status: StatusCode, body: Value) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let capture_clone = capture.clone();
        let request_count_clone = request_count.clone();
        let response_body = Arc::new(body);
        let response_body_clone = response_body.clone();

        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                let request_count = request_count_clone.clone();
                let response_body = response_body_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    request_count.fetch_add(1, Ordering::SeqCst);
                    capture.lock().unwrap().push(payload);
                    (status, axum::Json((*response_body).clone()))
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            base_url: format!("http://{}", address),
            capture,
            request_count,
        }
    }

    async fn scripted(replies: Vec<CapabilityProbeMockReply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let capture_clone = capture.clone();
        let request_count_clone = request_count.clone();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let replies_clone = replies.clone();

        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let capture = capture_clone.clone();
                let request_count = request_count_clone.clone();
                let replies = replies_clone.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    request_count.fetch_add(1, Ordering::SeqCst);
                    capture.lock().unwrap().push(payload);
                    let reply = replies.lock().unwrap().pop_front().unwrap();
                    match reply {
                        CapabilityProbeMockReply::ChatJson(body) => {
                            (StatusCode::OK, axum::Json(body)).into_response()
                        }
                        CapabilityProbeMockReply::ChatSse(events) => (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(stream::iter(
                                events
                                    .into_iter()
                                    .map(|event| Ok::<Bytes, std::io::Error>(Bytes::from(event))),
                            )),
                        )
                            .into_response(),
                    }
                }
            }),
        );

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            base_url: format!("http://{}", address),
            capture,
            request_count,
        }
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Value> {
        self.capture.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn probe_service_honors_global_concurrency_across_upstreams() {
    with_proxy_env_cleared(|| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/v1/chat/completions",
            post({
                let active = active.clone();
                let max_active = max_active.clone();
                let request_count = request_count.clone();
                move || {
                    let active = active.clone();
                    let max_active = max_active.clone();
                    let request_count = request_count.clone();
                    async move {
                        request_count.fetch_add(1, Ordering::SeqCst);
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(current, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        (
                            StatusCode::FORBIDDEN,
                            axum::Json(json!({"error": {"message": "denied"}})),
                        )
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let base_url = format!("http://{address}");
        let upstream = |id: &str, model: &str| UpstreamConfig {
            id: id.into(),
            name: id.into(),
            base_url: base_url.clone(),
            api_key: "probe-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![model.into()],
            active: true,
            ..Default::default()
        };
        let tempdir = tempdir().unwrap();
        let config = AppConfig {
            capability_probe_request_timeout_seconds: 2,
            automatic_capability_probes_enabled: true,
            ..AppConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream("up-1", "model-a"), upstream("up-2", "model-b")],
                downstreams: vec![DownstreamConfig {
                    id: "down-1".into(),
                    name: "probe-consumer".into(),
                    hash: "unused".into(),
                    plaintext_key: None,
                    plaintext_key_prefix: None,
                    model_allowlist: Vec::new(),
                    rate_limit_enabled: false,
                    per_minute_limit: 60,
                    max_concurrency: 10,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: Vec::new(),
                    expires_at: None,
                    active: true,
                }],
                ..PersistedState::default()
            },
            tempdir.path().join("state.json"),
            config,
        );
        state
            .replace_capability_configuration(CapabilityConfiguration::default())
            .await
            .unwrap();

        let _service = CapabilityProbeService::spawn(state);
        tokio::time::timeout(Duration::from_secs(2), async {
            while request_count.load(Ordering::SeqCst) < 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(max_active.load(Ordering::SeqCst), 2);
    })
    .await;
}

#[tokio::test]
async fn probe_service_periodically_reconciles_expired_verified_profiles() {
    with_proxy_env_cleared(|| async move {
        let mock = ProbeMock::chat(|_| json!({"choices": [{"message": {"content": "ok"}}]})).await;
        let upstream = UpstreamConfig {
            id: "periodic-upstream".into(),
            name: "periodic-upstream".into(),
            base_url: mock.base_url.clone(),
            api_key: "probe-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["periodic-model".into()],
            active: true,
            ..UpstreamConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                downstreams: vec![DownstreamConfig {
                    id: "periodic-downstream".into(),
                    name: "periodic-downstream".into(),
                    hash: "unused".into(),
                    plaintext_key: None,
                    plaintext_key_prefix: None,
                    model_allowlist: vec!["periodic-model".into()],
                    rate_limit_enabled: false,
                    per_minute_limit: 60,
                    max_concurrency: 10,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: Vec::new(),
                    expires_at: None,
                    active: true,
                }],
                ..PersistedState::default()
            },
            tempdir().unwrap().path().join("state.json"),
            AppConfig {
                automatic_capability_probes_enabled: true,
                ..AppConfig::default()
            },
        );
        state
            .replace_capability_configuration(CapabilityConfiguration {
                probe: chat_responses_codex::capabilities::ProbeConfiguration {
                    refresh_interval_seconds: 1,
                    ..Default::default()
                },
                ..CapabilityConfiguration::default()
            })
            .await
            .unwrap();
        let fingerprint = state
            .route_configuration_fingerprint(
                &upstream,
                &chat_responses_codex::keys::upstream_key_fingerprint(
                    &upstream.id,
                    &upstream.api_key,
                ),
                "periodic-model",
                "periodic-model",
                UpstreamProtocol::ChatCompletions,
            )
            .unwrap();
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            key_fingerprint: chat_responses_codex::keys::upstream_key_fingerprint(
                &upstream.id,
                &upstream.api_key,
            ),
            upstream_id: upstream.id.clone(),
            runtime_model_slug: "periodic-model".into(),
            protocol: WireProtocol::ChatCompletions,
        });
        profile.state = chat_responses_codex::capabilities::DialectProfileState::Verified;
        profile.configuration_fingerprint = fingerprint;
        profile.last_success_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        state.upsert_dialect_profile(profile).await.unwrap();

        let _service = CapabilityProbeService::spawn(state);
        tokio::time::timeout(Duration::from_secs(3), async {
            while mock.request_count() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("expired profile should be re-probed by the periodic reconciler");
    })
    .await;
}

#[tokio::test]
async fn per_key_probe_profiles_keep_independent_reasoning_controls() {
    with_proxy_env_cleared(|| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let requests_clone = requests.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |request: Request<Body>| {
                let requests = requests_clone.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let authorization = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    requests
                        .lock()
                        .unwrap()
                        .push((authorization.clone(), payload.clone()));
                    if authorization == "Bearer key-a" && payload["reasoning_effort"] == "xhigh" {
                        (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": {"message": "reasoning_effort xhigh is unsupported"}
                            })),
                        )
                            .into_response()
                    } else {
                        (StatusCode::OK, axum::Json(text_response("ok"))).into_response()
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let model = "glm-5.2";
        let upstream = UpstreamConfig {
            id: "per-key-probe".into(),
            name: "per-key-probe".into(),
            base_url: format!("http://{address}"),
            api_key: "key-a".into(),
            api_keys: vec!["key-b".into()],
            api_key_models: vec![
                chat_responses_codex::state::ApiKeyModelConfig {
                    api_key: "key-a".into(),
                    supported_models: vec![model.into()],
                },
                chat_responses_codex::state::ApiKeyModelConfig {
                    api_key: "key-b".into(),
                    supported_models: vec![model.into()],
                },
            ],
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![model.into()],
            active: true,
            ..UpstreamConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                downstreams: vec![DownstreamConfig {
                    id: "per-key-downstream".into(),
                    name: "per-key-downstream".into(),
                    hash: "unused".into(),
                    plaintext_key: None,
                    plaintext_key_prefix: None,
                    model_allowlist: vec![model.into()],
                    rate_limit_enabled: false,
                    per_minute_limit: 60,
                    max_concurrency: 10,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: Vec::new(),
                    expires_at: None,
                    active: true,
                }],
                ..PersistedState::default()
            },
            tempdir().unwrap().path().join("state.json"),
            AppConfig {
                automatic_capability_probes_enabled: true,
                ..AppConfig::default()
            },
        );
        state
            .replace_capability_configuration(CapabilityConfiguration {
                policies: vec![CapabilityPolicy {
                    id: "per-key-reasoning-controls".into(),
                    probe_candidates: ProbeCandidates {
                        reasoning_controls: std::collections::BTreeMap::from([(
                            "reasoning_effort".into(),
                            vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
                        )]),
                        ..ProbeCandidates::default()
                    },
                    ..CapabilityPolicy::default()
                }],
                ..CapabilityConfiguration::default()
            })
            .await
            .unwrap();

        let jobs = state
            .reconcile_dialect_profiles(1_700_000_000)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 2);
        assert_ne!(jobs[0].key.key_fingerprint, jobs[1].key.key_fingerprint);
        let _service = CapabilityProbeService::spawn(state.clone());

        let key_a = DialectProfileKey::for_key(
            upstream.id.clone(),
            chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, "key-a"),
            model,
            WireProtocol::ChatCompletions,
        );
        let key_b = DialectProfileKey::for_key(
            upstream.id.clone(),
            chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, "key-b"),
            model,
            WireProtocol::ChatCompletions,
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot = state.capability_snapshot();
                if snapshot.profiles.contains_key(&key_a) && snapshot.profiles.contains_key(&key_b)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("both key routes should finish probing");

        let snapshot = state.capability_snapshot();
        assert_eq!(
            snapshot.profiles[&key_a].reasoning_controls["reasoning_effort"],
            vec!["low", "medium", "high"]
        );
        assert!(
            snapshot.profiles[&key_b].reasoning_controls["reasoning_effort"]
                .contains(&"xhigh".to_string())
        );
        let requests = requests.lock().unwrap();
        assert!(requests
            .iter()
            .any(|(authorization, _)| authorization == "Bearer key-a"));
        assert!(requests
            .iter()
            .any(|(authorization, _)| authorization == "Bearer key-b"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stale_per_key_probe_job_redaction_hides_keyed_identity_in_tracing() {
    with_proxy_env_cleared(|| async move {
        let capture = TracingCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(capture.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let mock = ProbeMock::chat(|_| text_response("must not be called")).await;
        let model = "glm-5.2";
        let upstream = UpstreamConfig {
            id: "stale-key-probe".into(),
            name: "stale-key-probe".into(),
            base_url: mock.base_url.clone(),
            api_key: "key-a".into(),
            supported_models: vec![model.into()],
            active: true,
            ..UpstreamConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![upstream.clone()],
                ..PersistedState::default()
            },
            tempdir().unwrap().path().join("state.json"),
            AppConfig::default(),
        );
        let key_a_fingerprint =
            chat_responses_codex::keys::upstream_key_fingerprint(&upstream.id, "key-a");
        let stale_job = state
            .build_capability_probe_job(
                &upstream.id,
                &key_a_fingerprint,
                model,
                model,
                UpstreamProtocol::ChatCompletions,
                ProbeReason::Manual,
            )
            .await
            .unwrap()
            .unwrap();
        let configuration_fingerprint = stale_job.configuration.configuration_fingerprint.clone();
        let route_id = chat_responses_codex::keys::anonymous_route_id(
            &upstream.id,
            &key_a_fingerprint,
            model,
            WireProtocol::ChatCompletions,
        );
        state
            .update_upstream(
                &upstream.id,
                UpstreamConfig {
                    api_key: "key-b".into(),
                    ..upstream.clone()
                },
            )
            .await
            .unwrap();

        let _service = CapabilityProbeService::spawn(state.clone());
        assert!(state.queue_capability_probe(stale_job));
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(mock.request_count(), 0);
        assert!(state.capability_snapshot().profiles.is_empty());

        let trace = capture.contents();
        assert!(trace.contains(&format!("route_id={route_id}")), "{trace}");
        assert!(trace.contains("capability probe queued"), "{trace}");
        assert!(trace.contains("capability probe completed"), "{trace}");
        for secret in [
            "key-a",
            key_a_fingerprint.as_str(),
            configuration_fingerprint.as_str(),
            "key_fingerprint",
            "configuration_fingerprint",
        ] {
            assert!(
                !trace.contains(secret),
                "probe trace leaked {secret}: {trace}"
            );
        }
    })
    .await;
}

async fn run_probe_against(mock: &ProbeMock, plan: CapabilityProbePlan) -> ProbeOutcome {
    run_probe_plan_for_test(&mock.base_url, "probe-secret", plan, 20)
        .await
        .expect("probe execution should complete")
}

async fn run_responses_stream_probe(sse_body: String) -> ProbeOutcome {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let sse_body = Arc::new(sse_body);
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let sse_body = sse_body.clone();
            async move {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    (*sse_body).clone(),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/responses-model",
        CapabilityProbePlan {
            protocol: WireProtocol::Responses,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true }],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap()
}

async fn run_chat_stream_probe(sse_body: String) -> ProbeOutcome {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let sse_body = Arc::new(sse_body);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let sse_body = sse_body.clone();
            async move {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    (*sse_body).clone(),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/chat-model",
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true }],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap()
}

async fn run_responses_nonstream_probe(response_body: Value) -> ProbeOutcome {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let response_body = Arc::new(response_body);
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let response_body = response_body.clone();
            async move { axum::Json((*response_body).clone()) }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/responses-model",
        CapabilityProbePlan {
            protocol: WireProtocol::Responses,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false }],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap()
}

async fn run_responses_tool_continuation_probe(response_body: Value) -> ProbeOutcome {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let response_body = Arc::new(response_body);
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let response_body = response_body.clone();
            async move { axum::Json((*response_body).clone()) }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/responses-model",
        CapabilityProbePlan {
            protocol: WireProtocol::Responses,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::ToolContinuation {
                    reasoning_carrier: None,
                },
            ],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap()
}

async fn run_responses_function_tools_probe(response_body: Value) -> ProbeOutcome {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let response_body = Arc::new(response_body);
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let response_body = response_body.clone();
            async move { axum::Json((*response_body).clone()) }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/responses-model",
        CapabilityProbePlan {
            protocol: WireProtocol::Responses,
            cases: vec![chat_responses_codex::server::CoreProbeCase::FunctionTools],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap()
}

async fn run_oversized_stream_probe(
    protocol: WireProtocol,
) -> (Result<ProbeOutcome, std::io::Error>, usize, usize) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let produced = Arc::new(AtomicUsize::new(0));
    let produced_for_route = produced.clone();
    let frame = b": keepalive\n\n";
    let chunk = Bytes::from(frame.repeat(4096));
    let total_chunks = MAX_STREAM_AGGREGATE_TOTAL_BYTES / chunk.len() + 8;
    let route = match protocol {
        WireProtocol::ChatCompletions => "/v1/chat/completions",
        WireProtocol::Responses => "/v1/responses",
        WireProtocol::Messages => unreachable!(),
    };
    let app = Router::new().route(
        route,
        post(move || {
            let produced = produced_for_route.clone();
            let chunk = chunk.clone();
            async move {
                let body = Body::from_stream(stream::unfold(0usize, move |index| {
                    let produced = produced.clone();
                    let chunk = chunk.clone();
                    async move {
                        if index >= total_chunks {
                            return None;
                        }
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        produced.fetch_add(1, Ordering::SeqCst);
                        Some((Ok::<Bytes, std::io::Error>(chunk), index + 1))
                    }
                }));
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    body,
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let result = run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/stream-model",
        CapabilityProbePlan {
            protocol,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true }],
            output_token_cap: 16,
        },
        5,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    (result, produced.load(Ordering::SeqCst), total_chunks)
}

#[tokio::test]
async fn candidate_and_extension_probe_evidence_is_persisted_in_profile() {
    let mock = ProbeMock::chat(|request| {
        if request["service_tier"] == "auto" {
            json!({"accepted": true})
        } else {
            text_response("ok")
        }
    })
    .await;
    let extension = DeclarativeProbeCase {
        id: "service-tier".into(),
        protocol: WireProtocol::ChatCompletions,
        prerequisites: Default::default(),
        request_patch: json!({"service_tier": "auto"}),
        response_predicate: ResponsePredicate {
            path: "/accepted".into(),
            operator: PredicateOperator::Equals,
            value: Some(json!(true)),
        },
    };
    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::TokenLimit {
                    field: TokenLimitField::MaxCompletionTokens,
                },
                chat_responses_codex::server::CoreProbeCase::ReasoningControl {
                    field: "reasoning_effort".into(),
                    value: "high".into(),
                },
                chat_responses_codex::server::CoreProbeCase::Declarative(extension),
            ],
            output_token_cap: 16,
        },
    )
    .await;
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "probe-upstream".into(),
        runtime_model_slug: "probe-model".into(),
        protocol: WireProtocol::ChatCompletions,
    });

    apply_probe_outcome(&mut profile, outcome);

    assert_eq!(
        profile.token_limit_field,
        Some(TokenLimitField::MaxCompletionTokens)
    );
    assert_eq!(
        profile.reasoning_controls.get("reasoning_effort"),
        Some(&vec!["high".into()])
    );
    assert_eq!(
        profile.extension_evidence.get("service-tier"),
        Some(&EvidenceState::Supported)
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["max_completion_tokens"], 16);
    assert!(requests[0].get("max_tokens").is_none());
    assert!(requests[0].get("max_output_tokens").is_none());
    assert_eq!(requests[1]["reasoning_effort"], "high");
    assert!(requests[1].get("max_completion_tokens").is_none());
    assert_eq!(requests[2]["service_tier"], "auto");
}

#[tokio::test]
async fn probe_payload_uses_exact_runtime_model_slug() {
    let mock = ProbeMock::chat(|request| {
        if request["model"] == "opaque/runtime-model" {
            text_response("ok")
        } else {
            json!({"error": {"message": "wrong model"}})
        }
    })
    .await;

    let outcome = run_probe_plan_for_model_for_test(
        &mock.base_url,
        "probe-secret",
        "opaque/runtime-model",
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false }],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
    let requests = mock.requests();
    let request = &requests[0];
    assert_eq!(request["model"], "opaque/runtime-model");
    assert!(request.get("max_tokens").is_none());
    assert!(request.get("max_completion_tokens").is_none());
    assert!(request.get("max_output_tokens").is_none());
}

#[tokio::test]
async fn stream_rejection_does_not_overwrite_supported_text_input() {
    let mock = ProbeMock::scripted(vec![
        CapabilityProbeMockReply::ChatJson(text_response("ok")),
        CapabilityProbeMockReply::ChatSse(vec!["data: [DONE]\n\n".into()]),
    ])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false },
                chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true },
            ],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Rejected
    );
}

#[tokio::test]
async fn stream_only_chat_route_rejects_nonstream_without_losing_text_input() {
    let mock = ProbeMock::responding(|request| {
        if request["stream"] == true {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",",
                    "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},",
                    "\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response();
        }

        axum::Json(json!({
            "id": "chatcmpl-empty",
            "object": "chat.completion",
            "created": 1,
            "model": "opaque-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": ""},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 0,
                "total_tokens": 1
            }
        }))
        .into_response()
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false },
                chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true },
            ],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::NonStreamingResponse),
        EvidenceState::Rejected
    );
    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn stream_only_probe_missing_usage_is_not_explicit_zero_output() {
    let mock = ProbeMock::chat(|_| {
        json!({
            "id": "chatcmpl-empty",
            "object": "chat.completion",
            "created": 1,
            "model": "opaque-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": ""},
                "finish_reason": "stop"
            }]
        })
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false }],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::NonStreamingResponse),
        EvidenceState::Unobserved
    );
    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Unobserved
    );
}

#[tokio::test]
async fn stream_only_probe_healthy_nonstream_proves_both_capabilities() {
    let mock = ProbeMock::chat(|_| text_response("OK")).await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false }],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::NonStreamingResponse),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn stream_only_probe_non_ok_fake_sse_is_not_supported() {
    let mock = ProbeMock::responding(|_| {
        (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",",
                "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"not-ok\"},",
                "\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
            .into_response()
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true }],
            output_token_cap: 16,
        },
    )
    .await;

    assert_ne!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Supported
    );
    assert_ne!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn stream_only_responses_incomplete_snapshot_proves_text_stream() {
    let outcome = run_responses_stream_probe(format!(
        "data: {}\n\ndata: {}\n\n",
        json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "content_index": 0,
            "delta": "OK"
        }),
        json!({
            "type": "response.incomplete",
            "response": {
                "id": "resp-incomplete",
                "object": "response",
                "status": "incomplete",
                "model": "opaque/responses-model",
                "output": [{
                    "id": "msg-1",
                    "type": "message",
                    "status": "incomplete",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "OK"}]
                }],
                "incomplete_details": {"reason": "max_output_tokens"}
            }
        })
    ))
    .await;

    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn stream_only_chat_accepts_standard_crlf_comments_and_multiline_data() {
    let outcome = run_chat_stream_probe(
        concat!(
            ": keepalive\r\n\r\n",
            "data:{\"id\":\"chunk-crlf\",\"object\":\"chat.completion.chunk\",\r\n",
            "data:\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},",
            "\"finish_reason\":null}]}\r\n\r\n",
            "data: {\"id\":\"chunk-crlf\",\"choices\":[{\"index\":0,",
            "\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\n",
            "data:[DONE]\r\n\r\n"
        )
        .into(),
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn stream_only_responses_accepts_standard_crlf_terminal_and_trailing_done() {
    for status in ["completed", "incomplete"] {
        let outcome = run_responses_stream_probe(format!(
            concat!(
                ": keepalive\r\n\r\n",
                "event: response.output_text.delta\r\n",
                "data:{{\"type\":\"response.output_text.delta\",\r\n",
                "data:\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}}\r\n\r\n",
                "event: response.{status}\r\n",
                "data:{{\"type\":\"response.{status}\",\r\n",
                "data:\"response\":{{\"id\":\"resp-crlf\",\"object\":\"response\",",
                "\"status\":\"{status}\",\"model\":\"opaque/responses-model\",",
                "\"output\":[{{\"id\":\"msg-1\",\"type\":\"message\",",
                "\"status\":\"{status}\",\"role\":\"assistant\",",
                "\"content\":[{{\"type\":\"output_text\",\"text\":\"OK\"}}]}}]}}}}\r\n\r\n",
                "data:[DONE]\r\n\r\n"
            ),
            status = status,
        ))
        .await;

        assert_eq!(
            outcome.capability(Capability::TextInput),
            EvidenceState::Supported,
            "{status}"
        );
        assert_eq!(
            outcome.capability(Capability::TextStream),
            EvidenceState::Supported,
            "{status}"
        );
    }
}

#[tokio::test]
async fn stream_only_responses_completed_without_output_is_rejected() {
    let outcome = run_responses_stream_probe(format!(
        "data: {}\n\ndata: {}\n\n",
        json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "content_index": 0,
            "delta": "must-not-be-enough"
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-invalid",
                "object": "response",
                "status": "completed",
                "model": "opaque/responses-model"
            }
        })
    ))
    .await;

    assert_ne!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Supported
    );
    assert_ne!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn stream_only_chat_refusal_is_usable_nonstream_output() {
    let mock = ProbeMock::chat(|_| {
        json!({
            "id": "chatcmpl-refusal",
            "object": "chat.completion",
            "created": 1,
            "model": "opaque-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": null, "refusal": "cannot comply"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 0, "total_tokens": 1}
        })
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false }],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::NonStreamingResponse),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::TextInput),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn stream_only_responses_non_message_outputs_are_usable() {
    let cases = [
        (
            "reasoning-summary",
            json!({"id": "rs-summary", "type": "reasoning", "status": "completed",
                "summary": [{"type": "summary_text", "text": "plan"}], "content": []}),
        ),
        (
            "reasoning-content",
            json!({"id": "rs-content", "type": "reasoning", "status": "completed",
                "summary": [], "content": [{"type": "reasoning_text", "text": "plan"}]}),
        ),
        (
            "function",
            json!({"id": "fc-1", "type": "function_call", "status": "completed",
                "call_id": "call-1", "name": "lookup", "arguments": "{}"}),
        ),
        (
            "custom",
            json!({"id": "ct-1", "type": "custom_tool_call", "status": "completed",
                "call_id": "call-2", "name": "shell", "input": "pwd"}),
        ),
        (
            "hosted",
            json!({"id": "ws-1", "type": "web_search_call", "status": "completed",
                "action": {"type": "search", "query": "compatibility"}}),
        ),
        (
            "computer",
            json!({"id": "cc-1", "type": "computer_call", "status": "completed",
                "call_id": "call-3", "action": {"type": "click", "x": 1, "y": 2}}),
        ),
        (
            "unknown",
            json!({"id": "vendor-1", "type": "vendor_extension", "status": "completed",
                "payload": {"value": "observed"}}),
        ),
    ];

    for (label, item) in cases {
        let outcome = run_responses_nonstream_probe(json!({
            "id": format!("resp-{label}"),
            "object": "response",
            "status": "completed",
            "model": "opaque/responses-model",
            "output": [item],
            "usage": {"input_tokens": 1, "output_tokens": 0, "total_tokens": 1}
        }))
        .await;

        assert_eq!(
            outcome.capability(Capability::NonStreamingResponse),
            EvidenceState::Supported,
            "{label}"
        );
        assert_eq!(
            outcome.capability(Capability::TextInput),
            EvidenceState::Supported,
            "{label}"
        );
    }
}

#[tokio::test]
async fn stream_only_chat_oversized_sse_stops_reading_and_is_unobserved() {
    let (outcome, produced, total_chunks) =
        run_oversized_stream_probe(WireProtocol::ChatCompletions).await;
    let outcome = outcome.expect("stream limit must become operational evidence");

    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Unobserved
    );
    assert!(
        produced < total_chunks,
        "probe consumed all {produced} oversized stream chunks"
    );
}

#[tokio::test]
async fn stream_only_responses_oversized_sse_stops_reading_and_is_unobserved() {
    let (outcome, produced, total_chunks) =
        run_oversized_stream_probe(WireProtocol::Responses).await;
    let outcome = outcome.expect("stream limit must become operational evidence");

    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Unobserved
    );
    assert!(
        produced < total_chunks,
        "probe consumed all {produced} oversized stream chunks"
    );
}

#[tokio::test]
async fn probe_case_timeout_becomes_operational_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async { std::future::pending::<Response>().await }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let outcome = run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/runtime-model",
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: false }],
            output_token_cap: 16,
        },
        1,
    )
    .await
    .expect("timeout should be recorded as probe evidence");

    assert!(matches!(
        outcome,
        ProbeOutcome::OperationalFailure {
            ref code,
            http_status: None,
            ..
        } if code == "probe_timeout"
    ));
}

#[tokio::test]
async fn specialized_stream_probes_keep_operational_http_failures_unobserved() {
    let cases = [
        (
            "chat-indexed",
            WireProtocol::ChatCompletions,
            chat_responses_codex::server::CoreProbeCase::IndexedToolArguments,
            Capability::IndexedToolArgumentStream,
            StatusCode::TOO_MANY_REQUESTS,
        ),
        (
            "chat-usage",
            WireProtocol::ChatCompletions,
            chat_responses_codex::server::CoreProbeCase::UsageStream,
            Capability::UsageStream,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "responses-usage",
            WireProtocol::Responses,
            chat_responses_codex::server::CoreProbeCase::UsageStream,
            Capability::UsageStream,
            StatusCode::TOO_MANY_REQUESTS,
        ),
    ];

    for (label, protocol, case, capability, status) in cases {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let chat_status = status;
        let responses_status = status;
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(move || async move {
                    (
                        chat_status,
                        axum::Json(json!({"error": {"message": "temporarily unavailable"}})),
                    )
                }),
            )
            .route(
                "/v1/responses",
                post(move || async move {
                    (
                        responses_status,
                        axum::Json(json!({"error": {"message": "temporarily unavailable"}})),
                    )
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let outcome = run_probe_plan_for_model_for_test(
            &format!("http://{address}"),
            "probe-secret",
            "opaque/stream-model",
            CapabilityProbePlan {
                protocol,
                cases: vec![case],
                output_token_cap: 16,
            },
            5,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.capability(capability),
            EvidenceState::Unobserved,
            "{label} must not persist rejection"
        );
        assert!(
            matches!(
                outcome,
                ProbeOutcome::OperationalFailure {
                    http_status: Some(observed),
                    ..
                } if observed == status.as_u16()
            ),
            "{label} must remain an operational failure: {outcome:?}"
        );
    }
}

fn tool_call_response(
    call_id: &str,
    name: &str,
    arguments: &str,
    reasoning: Option<&str>,
) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": call_id,
            "type": "function",
            "function": {
                "name": name,
                "arguments": arguments
            }
        }]
    });
    if let Some(reasoning) = reasoning {
        message["reasoning_content"] = Value::String(reasoning.to_string());
    }
    json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion",
        "created": 1,
        "model": "probe-model",
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        }
    })
}

fn text_response(text: &str) -> Value {
    json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion",
        "created": 1,
        "model": "probe-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        }
    })
}

#[tokio::test]
async fn forced_tool_plain_text_is_not_positive_tool_evidence() {
    let saw_forced_tool = Arc::new(AtomicUsize::new(0));
    let saw_forced_tool_clone = saw_forced_tool.clone();
    let mock = ProbeMock::chat(move |request| {
        if request["tool_choice"]["function"]["name"] == "gateway_compat_probe" {
            saw_forced_tool_clone.fetch_add(1, Ordering::SeqCst);
        }
        text_response("done")
    })
    .await;

    let outcome = run_probe_against(&mock, CapabilityProbePlan::agent_core()).await;
    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Unobserved
    );
    assert_eq!(
        outcome.capability(Capability::ForcedToolChoice),
        EvidenceState::Rejected
    );
    assert!(outcome
        .evidence_codes()
        .contains("forced_tool_not_selected"));
    assert!(saw_forced_tool.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn missing_auto_tool_call_does_not_reject_tool_continuation() {
    let mock = ProbeMock::chat(|_| text_response("tool call not selected")).await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::ToolContinuation {
                    reasoning_carrier: None,
                },
            ],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Unobserved
    );
    assert!(outcome
        .evidence_codes()
        .contains("tool_continuation_missing_call"));
}

#[tokio::test]
async fn chat_tool_call_without_id_rejects_tool_continuation() {
    let mock = ProbeMock::chat(|_| {
        let mut response = tool_call_response(
            "call_probe",
            "gateway_compat_probe",
            r#"{"nonce":"n-17"}"#,
            None,
        );
        response["choices"][0]["message"]["tool_calls"][0]
            .as_object_mut()
            .unwrap()
            .remove("id");
        response
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::ToolContinuation {
                    reasoning_carrier: None,
                },
            ],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Rejected
    );
    assert!(outcome
        .evidence_codes()
        .contains("tool_continuation_invalid_call"));
}

#[tokio::test]
async fn responses_tool_call_without_call_id_rejects_tool_continuation() {
    let outcome = run_responses_tool_continuation_probe(json!({
        "id": "resp-probe",
        "object": "response",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "id": "fc-probe",
            "name": "gateway_compat_probe",
            "arguments": "{\"nonce\":\"n-17\"}",
            "status": "completed"
        }]
    }))
    .await;

    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Rejected
    );
    assert!(outcome
        .evidence_codes()
        .contains("tool_continuation_invalid_call"));
}

#[tokio::test]
async fn chat_function_tool_with_empty_id_is_rejected() {
    let mock = ProbeMock::chat(|_| {
        tool_call_response("", "gateway_compat_probe", r#"{"nonce":"n-17"}"#, None)
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::FunctionTools],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Rejected
    );
    assert!(outcome
        .evidence_codes()
        .contains("function_tools_invalid_call"));
}

#[tokio::test]
async fn responses_function_tool_with_empty_call_id_is_rejected() {
    let outcome = run_responses_function_tools_probe(json!({
        "id": "resp-probe",
        "object": "response",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "id": "fc-probe",
            "call_id": "",
            "name": "gateway_compat_probe",
            "arguments": "{\"nonce\":\"n-17\"}",
            "status": "completed"
        }]
    }))
    .await;

    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Rejected
    );
    assert!(outcome
        .evidence_codes()
        .contains("function_tools_invalid_call"));
}

#[tokio::test]
async fn function_tools_survive_forced_tool_choice_rejection() {
    let mock = ProbeMock::chat(|request| {
        if request["tools"].is_array() && request.get("tool_choice").is_none() {
            tool_call_response(
                "call_probe",
                "gateway_compat_probe",
                r#"{"nonce":"n-17"}"#,
                None,
            )
        } else {
            text_response("forced choice unavailable")
        }
    })
    .await;

    let outcome = run_probe_against(&mock, CapabilityProbePlan::agent_core()).await;

    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::ForcedToolChoice),
        EvidenceState::Rejected
    );
    assert!(mock
        .requests()
        .iter()
        .any(|request| request["tools"].is_array() && request.get("tool_choice").is_none()));
}

#[tokio::test]
async fn function_probe_exposes_nonce_to_model_without_unrelated_metadata() {
    let mock = ProbeMock::chat(|request| {
        let prompt = request["messages"][0]["content"]
            .as_str()
            .unwrap_or_default();
        if prompt.contains("n-17") && request.get("metadata").is_none() {
            tool_call_response(
                "call_probe",
                "gateway_compat_probe",
                r#"{"nonce":"n-17"}"#,
                None,
            )
        } else {
            text_response("nonce unavailable")
        }
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::FunctionSelection],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Supported
    );
}

#[tokio::test]
async fn indexed_tool_argument_probe_requires_observed_stream_fragments() {
    let mock = ProbeMock::scripted(vec![CapabilityProbeMockReply::ChatSse(vec![
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-probe\",\"type\":\"function\",\"function\":{\"name\":\"gateway_compat_probe\",\"arguments\":\"{\\\"nonce\\\":\"}}]},\"finish_reason\":null}]}\n\n".into(),
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"n-17\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n".into(),
        "data: [DONE]\n\n".into(),
    ])])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::IndexedToolArguments],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::IndexedToolArgumentStream),
        EvidenceState::Supported
    );
    assert_eq!(mock.request_count(), 1);
    assert!(mock.requests()[0].get("tool_choice").is_none());
}

#[tokio::test]
async fn continuation_probe_rejects_wrong_tool_arguments_before_replay() {
    let mock = ProbeMock::scripted(vec![
        CapabilityProbeMockReply::ChatJson(tool_call_response(
            "call_probe",
            "gateway_compat_probe",
            r#"{"nonce":"wrong"}"#,
            Some("reasoning"),
        )),
        CapabilityProbeMockReply::ChatJson(text_response("must not be requested")),
    ])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::ToolContinuation {
                    reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
                },
            ],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::ReasoningReplay),
        EvidenceState::Rejected
    );
    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Unobserved
    );
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn missing_reasoning_replay_does_not_erase_basic_tool_continuation() {
    let valid_call = || {
        CapabilityProbeMockReply::ChatJson(tool_call_response(
            "call_probe",
            "gateway_compat_probe",
            r#"{"nonce":"n-17"}"#,
            None,
        ))
    };
    let mock = ProbeMock::scripted(vec![
        valid_call(),
        CapabilityProbeMockReply::ChatJson(text_response("basic continuation")),
        valid_call(),
    ])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::ToolContinuation {
                    reasoning_carrier: None,
                },
                chat_responses_codex::server::CoreProbeCase::ToolContinuation {
                    reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
                },
            ],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::ReasoningReplay),
        EvidenceState::Rejected
    );
    assert_eq!(mock.request_count(), 3);
}

#[tokio::test]
async fn basic_continuation_does_not_require_forced_tool_choice() {
    let request_number = Arc::new(AtomicUsize::new(0));
    let request_number_clone = request_number.clone();
    let mock = ProbeMock::chat(move |request| {
        request_number_clone.fetch_add(1, Ordering::SeqCst);
        if request.get("tool_choice").is_some() {
            return text_response("forced choice unavailable");
        }
        if request["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
        {
            text_response("continued")
        } else {
            tool_call_response(
                "call_probe",
                "gateway_compat_probe",
                r#"{"nonce":"n-17"}"#,
                None,
            )
        }
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![
                chat_responses_codex::server::CoreProbeCase::ToolContinuation {
                    reasoning_carrier: None,
                },
            ],
            output_token_cap: 16,
        },
    )
    .await;

    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Supported
    );
    assert_eq!(request_number.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn responses_probe_uses_native_payloads_and_official_stream_events() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error": {"message": "wrong endpoint"}})),
                )
            }),
        )
        .route(
            "/v1/responses",
            post({
                let captured = captured.clone();
                move |request: Request<Body>| {
                    let captured = captured.clone();
                    async move {
                        let (_, body) = request.into_parts();
                        let payload: Value = serde_json::from_slice(
                            &to_bytes(body, usize::MAX).await.unwrap(),
                        )
                        .unwrap();
                        captured.lock().unwrap().push(payload.clone());
                        if payload["stream"] == true {
                            return (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-probe\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"opaque/responses-model\",\"output\":[{\"id\":\"msg-probe\",\"type\":\"message\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
                            )
                                .into_response();
                        }
                        if payload["input"].as_array().is_some_and(|items| {
                            items
                                .iter()
                                .any(|item| item["type"] == "function_call_output")
                        }) {
                            return axum::Json(json!({
                                "id": "resp-probe-continuation",
                                "object": "response",
                                "status": "completed",
                                "output": [{
                                    "type": "message",
                                    "role": "assistant",
                                    "status": "completed",
                                    "content": [{"type": "output_text", "text": "continued"}]
                                }]
                            }))
                            .into_response();
                        }
                        if payload["tools"].is_array() {
                            return axum::Json(json!({
                                "id": "resp-probe",
                                "object": "response",
                                "status": "completed",
                                "output": [{
                                    "type": "function_call",
                                    "id": "fc-probe",
                                    "call_id": "call-probe",
                                    "name": "gateway_compat_probe",
                                    "arguments": "{\"nonce\":\"n-17\"}",
                                    "status": "completed"
                                }]
                            }))
                            .into_response();
                        }
                        if payload["service_tier"] == "auto" {
                            return axum::Json(json!({"accepted": true})).into_response();
                        }
                        axum::Json(json!({
                            "id": "resp-probe",
                            "object": "response",
                            "status": "completed",
                            "output": [{
                                "type": "message",
                                "role": "assistant",
                                "status": "completed",
                                "content": [{"type": "output_text", "text": "ok"}]
                            }]
                        }))
                        .into_response()
                    }
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut plan = CapabilityProbePlan::agent_core();
    plan.protocol = WireProtocol::Responses;
    plan.cases
        .push(chat_responses_codex::server::CoreProbeCase::TokenLimit {
            field: TokenLimitField::MaxOutputTokens,
        });
    plan.cases
        .push(chat_responses_codex::server::CoreProbeCase::Declarative(
            DeclarativeProbeCase {
                id: "response-service-tier".into(),
                protocol: WireProtocol::Responses,
                prerequisites: Default::default(),
                request_patch: json!({"service_tier": "auto"}),
                response_predicate: ResponsePredicate {
                    path: "/accepted".into(),
                    operator: PredicateOperator::Equals,
                    value: Some(json!(true)),
                },
            },
        ));

    let outcome = run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/responses-model",
        plan,
        5,
    )
    .await
    .unwrap();

    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::FunctionTools,
        Capability::ForcedToolChoice,
        Capability::ToolContinuation,
        Capability::UsageStream,
    ] {
        assert_eq!(
            outcome.capability(capability),
            EvidenceState::Supported,
            "{capability:?}"
        );
    }
    match &outcome {
        ProbeOutcome::Conclusive {
            token_limit_field,
            extension_evidence,
            ..
        } => {
            assert_eq!(*token_limit_field, Some(TokenLimitField::MaxOutputTokens));
            assert_eq!(
                extension_evidence.get("response-service-tier"),
                Some(&EvidenceState::Supported)
            );
        }
        ProbeOutcome::OperationalFailure { .. } => panic!("expected conclusive evidence"),
    }
    let captured = captured.lock().unwrap();
    assert!(!captured.is_empty());
    assert!(captured.iter().all(|body| body.get("input").is_some()));
    assert!(captured.iter().all(|body| body.get("messages").is_none()));
    let tool_request = captured
        .iter()
        .find(|body| body["tools"].is_array())
        .unwrap();
    assert_eq!(tool_request["tools"][0]["name"], "gateway_compat_probe");
    assert!(tool_request["tools"][0].get("function").is_none());
}

#[tokio::test]
async fn auth_failure_stops_remaining_cases_and_is_operational() {
    let mock = ProbeMock::status(StatusCode::UNAUTHORIZED).await;
    let outcome = run_probe_against(&mock, CapabilityProbePlan::full()).await;

    assert!(matches!(
        outcome,
        ProbeOutcome::OperationalFailure {
            http_status: Some(401),
            ..
        }
    ));
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn tool_and_reasoning_pass_requires_linked_continuation() {
    let mock = ProbeMock::scripted(vec![
        CapabilityProbeMockReply::ChatJson(text_response("minimal-ok")),
        CapabilityProbeMockReply::ChatSse(vec![
            "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"S\"},\"finish_reason\":null}]}\n\n".to_string(),
            "data: {\"id\":\"chunk-2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ]),
        CapabilityProbeMockReply::ChatJson(text_response("forced-tool-miss")),
        CapabilityProbeMockReply::ChatJson(tool_call_response(
            "call_basic",
            "gateway_compat_probe",
            r#"{"nonce":"n-17"}"#,
            None,
        )),
        CapabilityProbeMockReply::ChatJson(text_response("basic-continuation-ok")),
        CapabilityProbeMockReply::ChatSse(vec![
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-indexed\",\"type\":\"function\",\"function\":{\"name\":\"gateway_compat_probe\",\"arguments\":\"{\\\"nonce\\\":\"}}]},\"finish_reason\":null}]}\n\n".into(),
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"n-17\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n".into(),
            "data: [DONE]\n\n".into(),
        ]),
        CapabilityProbeMockReply::ChatSse(vec![
            "data: {\"id\":\"chunk-3\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"U\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ]),
        CapabilityProbeMockReply::ChatJson(tool_call_response(
            "call_probe",
            "gateway_compat_probe",
            r#"{"nonce":"n-17"}"#,
            Some("think-exactly-once"),
        )),
        CapabilityProbeMockReply::ChatJson(text_response("continuation-ok")),
    ])
    .await;

    let outcome = run_probe_against(&mock, CapabilityProbePlan::reasoning_agent()).await;
    assert_eq!(
        outcome.capability(Capability::FunctionTools),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::ToolContinuation),
        EvidenceState::Supported
    );
    assert_eq!(
        outcome.capability(Capability::ReasoningReplay),
        EvidenceState::Supported
    );
    let requests = mock.requests();
    let continuation = requests
        .iter()
        .find(|request| {
            request["messages"]
                .as_array()
                .map(|messages| {
                    messages
                        .iter()
                        .any(|message| message["tool_call_id"] == "call_probe")
                })
                .unwrap_or(false)
        })
        .expect("continuation request should include linked tool result");
    assert_eq!(
        continuation["messages"][1]["reasoning_content"],
        "think-exactly-once"
    );
    assert_eq!(continuation["messages"][2]["tool_call_id"], "call_probe");
}

#[tokio::test]
async fn normal_gateway_request_never_launches_a_probe() {
    with_proxy_env_cleared(|| async move {
        let mock = ProbeMock::chat(|_| text_response("ok")).await;
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let downstream_key = generate_downstream_key("gw");
        let key = DialectProfileKey {
            key_fingerprint: String::new(),
            upstream_id: "up-1".into(),
            runtime_model_slug: "gpt-4.1-mini".into(),
            protocol: WireProtocol::ChatCompletions,
        };
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: mock.base_url.clone(),
                    api_key: "probe-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
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
        state
            .replace_capability_configuration(CapabilityConfiguration::default())
            .await
            .unwrap();
        state
            .upsert_dialect_profile(UpstreamDialectProfile::unknown(key))
            .await
            .unwrap();

        let app = build_router(state);
        let response = app
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
                            "model": "gpt-4.1-mini",
                            "messages": [{"role": "user", "content": "hi"}],
                            "stream": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(mock.request_count(), 1);
    })
    .await;
}

#[tokio::test]
async fn recognized_field_level_400_queues_future_probe_without_blocking_request() {
    with_proxy_env_cleared(|| async move {
        let mock = ProbeMock::status_with_body(
            StatusCode::BAD_REQUEST,
            json!({"error": {"message": "parallel_tool_calls unsupported"}}),
        )
        .await;
        let tempdir = tempdir().unwrap();
        let state_path = tempdir.path().join("state.json");
        let downstream_key = generate_downstream_key("gw");
        let state = AppState::new(
            PersistedState {
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "primary".into(),
                    base_url: mock.base_url.clone(),
                    api_key: "probe-secret".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    protocols: vec![UpstreamProtocol::ChatCompletions],
                    supported_models: vec!["gpt-4.1-mini".into()],
                    active: true,
                    ..Default::default()
                }],
                downstreams: vec![DownstreamConfig {
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
            AppConfig {
                automatic_capability_probes_enabled: true,
                ..AppConfig::default()
            },
        );
        let (sender, mut receiver) = mpsc::channel(8);
        state.set_capability_probe_sender(sender);
        let app = build_router(state);

        let response = app
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
                            "model": "gpt-4.1-mini",
                            "messages": [{"role": "user", "content": "hi"}],
                            "parallel_tool_calls": true,
                            "stream": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_client_error() || response.status().is_server_error());
        let batch = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let mut jobs = batch.into_jobs();
        assert_eq!(jobs.len(), 1);
        let job = jobs.remove(0);
        assert_eq!(job.key.upstream_id, "up-1");
        assert_eq!(job.key.runtime_model_slug, "gpt-4.1-mini");
        assert_eq!(job.key.protocol, WireProtocol::ChatCompletions);
    })
    .await;
}

#[tokio::test]
async fn stream_probe_requires_meaningful_delta_and_done() {
    let mock = ProbeMock::scripted(vec![CapabilityProbeMockReply::ChatSse(vec![
        "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n".to_string(),
        "data: {\"id\":\"chunk-2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ])])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::MinimalText { stream: true }],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::TextStream),
        EvidenceState::Supported
    );
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn usage_stream_probe_requires_usage_chunk() {
    let mock = ProbeMock::scripted(vec![CapabilityProbeMockReply::ChatSse(vec![
        "data: {\"id\":\"chunk-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n".to_string(),
        "data: {\"id\":\"chunk-2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ])])
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::UsageStream],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::UsageStream),
        EvidenceState::Supported
    );
    let requests = mock.requests();
    assert_eq!(requests[0]["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn image_data_url_probe_requires_expected_label_via_forced_tool_call() {
    let mock = ProbeMock::chat(|_| {
        tool_call_response(
            "call_image",
            "gateway_compat_probe",
            r#"{"label":"red"}"#,
            None,
        )
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ImageDataUrl],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::ImageDataUrl),
        EvidenceState::Supported
    );
    let requests = mock.requests();
    let request = requests.first().unwrap();
    assert!(request["messages"][0]["content"][1]["image_url"]["url"]
        .as_str()
        .is_some_and(|url| url.starts_with("data:image/png;base64,")));
    assert_eq!(
        request["tool_choice"]["function"]["name"],
        "gateway_compat_probe"
    );
    assert_image_color_probe_contract(
        &request["messages"][0]["content"][0]["text"],
        &request["tools"][0]["function"]["description"],
        &request["tools"][0]["function"]["parameters"],
    );
}

#[tokio::test]
async fn responses_image_probe_uses_native_input_image_shape() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error": {"message": "wrong endpoint"}})),
                )
            }),
        )
        .route(
            "/v1/responses",
            post({
                let captured = captured.clone();
                move |request: Request<Body>| {
                    let captured = captured.clone();
                    async move {
                        let (_, body) = request.into_parts();
                        let payload: Value =
                            serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap())
                                .unwrap();
                        captured.lock().unwrap().push(payload);
                        axum::Json(json!({
                            "id": "resp-image",
                            "object": "response",
                            "status": "completed",
                            "output": [{
                                "type": "function_call",
                                "id": "fc-image",
                                "call_id": "call-image",
                                "name": "gateway_compat_probe",
                                "arguments": "{\"label\":\"red\"}",
                                "status": "completed"
                            }]
                        }))
                    }
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let outcome = run_probe_plan_for_model_for_test(
        &format!("http://{address}"),
        "probe-secret",
        "opaque/responses-vision",
        CapabilityProbePlan {
            protocol: WireProtocol::Responses,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ImageDataUrl],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.capability(Capability::ImageDataUrl),
        EvidenceState::Supported
    );
    let captured = captured.lock().unwrap();
    let body = captured.first().unwrap();
    assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
    assert!(body["input"][0]["content"][1]["image_url"]
        .as_str()
        .is_some_and(|url| url.starts_with("data:image/png;base64,")));
    assert_eq!(body["tools"][0]["name"], "gateway_compat_probe");
    assert!(body["tools"][0].get("function").is_none());
    assert_eq!(body["tool_choice"]["name"], "gateway_compat_probe");
    assert_image_color_probe_contract(
        &body["input"][0]["content"][0]["text"],
        &body["tools"][0]["description"],
        &body["tools"][0]["parameters"],
    );
}

fn assert_image_color_probe_contract(prompt: &Value, tool_description: &Value, schema: &Value) {
    let prompt = prompt.as_str().expect("image probe prompt must be text");
    let prompt_words = prompt
        .split(|character: char| !character.is_ascii_alphabetic())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    for required_word in ["actual", "image", "color"] {
        assert!(
            prompt_words.iter().any(|word| word == required_word),
            "image probe prompt must mention {required_word:?}: {prompt:?}"
        );
    }

    let tool_description = tool_description
        .as_str()
        .expect("image probe tool must have a description")
        .to_ascii_lowercase();
    assert!(tool_description.contains("image"));
    assert!(tool_description.contains("color"));

    let label = &schema["properties"]["label"];
    let label_description = label["description"]
        .as_str()
        .expect("image probe label must have a description")
        .to_ascii_lowercase();
    assert!(label_description.contains("image"));
    assert!(label_description.contains("color"));
    assert_eq!(
        label["enum"],
        json!(["red", "green", "blue", "black", "white"])
    );
    assert!(label["enum"].as_array().unwrap().iter().all(|value| value
        .as_str()
        .is_some_and(|label| label == label.to_ascii_lowercase())));
    assert_eq!(schema["required"], json!(["label"]));
    assert_eq!(schema["additionalProperties"], false);
}

async fn responses_image_probe_mock(
    returned_label: &'static str,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new().route(
        "/v1/responses",
        post({
            let captured = captured.clone();
            move |request: Request<Body>| {
                let captured = captured.clone();
                async move {
                    let (_, body) = request.into_parts();
                    let payload: Value =
                        serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                    captured.lock().unwrap().push(payload);
                    axum::Json(json!({
                        "id": "resp-image-https",
                        "object": "response",
                        "status": "completed",
                        "output": [{
                            "type": "function_call",
                            "id": "fc-image-https",
                            "call_id": "call-image-https",
                            "name": "gateway_compat_probe",
                            "arguments": json!({"label": returned_label}).to_string(),
                            "status": "completed"
                        }]
                    }))
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), captured)
}

#[tokio::test]
async fn responses_https_probe_accepts_arbitrary_expected_label_without_color_enum() {
    let fixture_url = "https://example.com/labeled-fixture.png";
    let (base_url, captured) = responses_image_probe_mock("OK").await;

    let outcome = run_probe_plan_for_model_for_test(
        &base_url,
        "probe-secret",
        "opaque/responses-vision",
        CapabilityProbePlan {
            protocol: WireProtocol::Responses,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ImageHttps {
                url: fixture_url.to_string(),
                expected_label: "OK".to_string(),
            }],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.capability(Capability::ImageHttps),
        EvidenceState::Supported
    );
    let captured = captured.lock().unwrap();
    let body = captured.first().unwrap();
    assert_eq!(body["input"][0]["content"][1]["image_url"], fixture_url);
    assert_eq!(body["tool_choice"]["name"], "gateway_compat_probe");
    let schema = &body["tools"][0]["parameters"];
    assert_generic_image_label_probe_contract(
        &body["input"][0]["content"][0]["text"],
        &body["tools"][0]["description"],
        schema,
    );
    assert!(!body.to_string().contains("OK"));
}

#[tokio::test]
async fn responses_https_probe_rejects_label_that_does_not_exactly_match_fixture() {
    let (base_url, _) = responses_image_probe_mock("ok").await;

    let outcome = run_probe_plan_for_model_for_test(
        &base_url,
        "probe-secret",
        "opaque/responses-vision",
        CapabilityProbePlan {
            protocol: WireProtocol::Responses,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ImageHttps {
                url: "https://example.com/labeled-fixture.png".to_string(),
                expected_label: "OK".to_string(),
            }],
            output_token_cap: 16,
        },
        5,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.capability(Capability::ImageHttps),
        EvidenceState::Rejected
    );
}

fn assert_generic_image_label_probe_contract(
    prompt: &Value,
    tool_description: &Value,
    schema: &Value,
) {
    let prompt = prompt
        .as_str()
        .expect("HTTPS image probe prompt must be text")
        .to_ascii_lowercase();
    assert!(prompt.contains("actual"));
    assert!(prompt.contains("image"));
    assert!(prompt.contains("label"));
    assert!(!prompt.contains("color"));

    let tool_description = tool_description
        .as_str()
        .expect("HTTPS image probe tool must have a description")
        .to_ascii_lowercase();
    assert!(tool_description.contains("image"));
    assert!(tool_description.contains("label"));
    assert!(!tool_description.contains("color"));

    let label = &schema["properties"]["label"];
    let label_description = label["description"]
        .as_str()
        .expect("HTTPS image probe label must have a description")
        .to_ascii_lowercase();
    assert!(label_description.contains("image"));
    assert!(label_description.contains("label"));
    assert!(!label_description.contains("color"));
    assert!(label.get("enum").is_none());
    assert_eq!(schema["required"], json!(["label"]));
    assert_eq!(schema["additionalProperties"], false);
}

#[tokio::test]
async fn image_https_probe_requires_expected_label_via_forced_tool_call() {
    let fixture_url = "https://example.com/labeled-fixture.png";
    let mock = ProbeMock::chat(move |_| {
        tool_call_response(
            "call_image_https",
            "gateway_compat_probe",
            r#"{"label":"CERULEAN_42"}"#,
            None,
        )
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ImageHttps {
                url: fixture_url.to_string(),
                expected_label: "CERULEAN_42".to_string(),
            }],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::ImageHttps),
        EvidenceState::Supported
    );
    let requests = mock.requests();
    let request = requests.first().unwrap();
    assert_eq!(
        request["messages"][0]["content"][1]["image_url"]["url"],
        fixture_url
    );
    assert_eq!(
        request["tool_choice"]["function"]["name"],
        "gateway_compat_probe"
    );
    assert_generic_image_label_probe_contract(
        &request["messages"][0]["content"][0]["text"],
        &request["tools"][0]["function"]["description"],
        &request["tools"][0]["function"]["parameters"],
    );
    assert!(!request.to_string().contains("CERULEAN_42"));
}

#[tokio::test]
async fn parallel_tools_probe_requires_multiple_tool_calls_in_one_turn() {
    let mock = ProbeMock::chat(|request| {
        assert_eq!(request["parallel_tool_calls"], true);
        json!({
            "id": "chatcmpl-probe",
            "object": "chat.completion",
            "created": 1,
            "model": "probe-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "gateway_compat_probe", "arguments": "{\"slot\":1}"}
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {"name": "gateway_compat_probe", "arguments": "{\"slot\":2}"}
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    })
    .await;

    let outcome = run_probe_against(
        &mock,
        CapabilityProbePlan {
            protocol: WireProtocol::ChatCompletions,
            cases: vec![chat_responses_codex::server::CoreProbeCase::ParallelTools],
            output_token_cap: 64,
        },
    )
    .await;
    assert_eq!(
        outcome.capability(Capability::ParallelToolCalls),
        EvidenceState::Supported
    );
}
