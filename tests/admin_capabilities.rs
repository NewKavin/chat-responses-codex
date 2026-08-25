use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::auth::generate_admin_token;
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector, DialectProfileKey,
    DialectProfileState, EvidenceState, ProbeConfiguration, ProbeProfileOutcome,
    RouteCapabilityOverride, SemanticPolicy, UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::keys::{anonymous_route_id, upstream_key_fingerprint};
use chat_responses_codex::server::{build_router, CapabilityProbeService};
use chat_responses_codex::state::{
    ApiKeyModelConfig, AppConfig, AppState, PersistedState, UpstreamConfig, UpstreamModelMapping,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tempfile::tempdir;
use tokio::sync::{mpsc, Notify};
use tokio::time::{sleep, timeout, Duration};
use tower::ServiceExt;

struct AdminCapabilityFixture {
    app: axum::Router,
    state: AppState,
    token: String,
}

#[derive(Clone, Default)]
struct RejectingCapabilityStore;

impl chat_responses_codex::state::StateStore for RejectingCapabilityStore {
    fn persist_config<'a>(
        &'a self,
        _state: &'a PersistedState,
    ) -> chat_responses_codex::state::StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn persist_capability_configuration<'a>(
        &'a self,
        _configuration: &'a CapabilityConfiguration,
    ) -> chat_responses_codex::state::StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Err(io::Error::other("credential-do-not-echo")) })
    }
}

impl AdminCapabilityFixture {
    async fn new() -> Self {
        let fixture = Self::new_with_upstream_base_url("https://example.invalid").await;
        CapabilityProbeService::spawn(fixture.state.clone());
        fixture
    }

    async fn new_with_upstream_base_url(base_url: &str) -> Self {
        let tempdir = tempdir().unwrap();
        let config = AppConfig {
            jwt_secret: "test_secret".into(),
            ..AppConfig::default()
        };
        let state = AppState::new(
            PersistedState {
                upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "Primary".into(),
                    base_url: base_url.into(),
                    api_key: "upstream-secret".into(),
                    supported_models: vec!["opaque".into()],
                    active: true,
                    ..Default::default()
                }]),
                ..PersistedState::default()
            },
            tempdir.path().join("state.json"),
            config,
        );

        let key = DialectProfileKey {
            key_fingerprint: upstream_key_fingerprint("up-1", "upstream-secret"),
            upstream_id: "up-1".into(),
            runtime_model_slug: "opaque".into(),
            protocol: WireProtocol::ChatCompletions,
        };
        let mut profile = UpstreamDialectProfile::unknown(key);
        profile.state = DialectProfileState::Verified;
        profile
            .capabilities
            .insert(Capability::FunctionTools, EvidenceState::Supported);
        state.upsert_dialect_profile(profile).await.unwrap();

        Self {
            app: build_router(state.clone()),
            state,
            token: generate_admin_token("admin", "test_secret").unwrap(),
        }
    }

    async fn new_with_rejecting_capability_store() -> Self {
        let config = AppConfig {
            jwt_secret: "test_secret".into(),
            ..AppConfig::default()
        };
        let state = AppState::new_with_store(
            PersistedState {
                upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "Primary".into(),
                    base_url: "https://example.invalid".into(),
                    api_key: "upstream-secret".into(),
                    supported_models: vec!["opaque".into()],
                    active: true,
                    ..UpstreamConfig::default()
                }]),
                ..PersistedState::default()
            },
            tempfile::tempdir().unwrap().path().join("state.json"),
            config,
            Arc::new(RejectingCapabilityStore),
        );
        Self {
            app: build_router(state.clone()),
            state,
            token: generate_admin_token("admin", "test_secret").unwrap(),
        }
    }

    async fn get(&self, path: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_json(&self, path: &str, body: Value) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn put_json(&self, path: &str, body: Value) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn export(&self) -> Value {
        response_json(self.get("/api/admin/capabilities/export").await).await
    }

    async fn import_revision(&self, revision: u64) {
        let config = CapabilityConfiguration {
            revision,
            ..CapabilityConfiguration::default()
        };
        self.state
            .replace_capability_configuration(config)
            .await
            .unwrap();
    }

    fn valid_bundle(&self) -> Value {
        serde_json::to_value(CapabilityConfiguration::default()).unwrap()
    }
}

fn route_override_configuration(
    revision: u64,
    capability: Capability,
    state: EvidenceState,
) -> CapabilityConfiguration {
    CapabilityConfiguration {
        revision,
        route_overrides: vec![RouteCapabilityOverride {
            id: format!("route-{revision}"),
            priority: 100,
            selector: CapabilitySelector {
                upstream_id: Some("up-1".into()),
                exposed_model: Some("opaque".into()),
                runtime_model: Some("opaque".into()),
                protocol: Some(WireProtocol::ChatCompletions),
                ..CapabilitySelector::default()
            },
            capabilities: BTreeMap::from([(capability, state)]),
            ..RouteCapabilityOverride::default()
        }],
        ..CapabilityConfiguration::default()
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn assert_probe_error(
    response: axum::response::Response,
    expected_status: StatusCode,
    expected_code: &str,
    expected_message: &str,
) {
    assert_eq!(response.status(), expected_status);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], expected_code);
    assert_eq!(body["error"]["message"], expected_message);
    assert!(!body.to_string().contains("upstream-secret"));
}

fn manual_probe_request(model: &str) -> Value {
    json!({
        "upstream_id": "up-1",
        "exposed_model_slug": model,
        "runtime_model_slug": model,
        "protocol": "chat_completions"
    })
}

#[tokio::test]
async fn admin_can_export_import_and_inspect_capability_sources() {
    let fixture = AdminCapabilityFixture::new().await;
    let export = fixture.get("/api/admin/capabilities/export").await;
    assert_eq!(export.status(), StatusCode::OK);
    assert_eq!(response_json(export).await["schema_version"], 1);

    let mut bundle = fixture.valid_bundle();
    bundle["revision"] = json!(42);
    let import = fixture
        .post_json("/api/admin/capabilities/import", bundle)
        .await;
    assert_eq!(import.status(), StatusCode::OK);

    let resolved = fixture
        .get("/api/admin/capabilities/resolved?upstream_id=up-1&model=opaque&protocol=chat_completions")
        .await;
    let body = response_json(resolved).await;
    assert_eq!(body["configuration_revision"], 42);
    assert!(body["capabilities"]["function_tools"]["source"].is_string());
    assert!(body["profile_age_seconds"].is_number() || body["profile_age_seconds"].is_null());
}

#[tokio::test]
async fn invalid_import_is_400_and_keeps_previous_revision() {
    let fixture = AdminCapabilityFixture::new().await;
    fixture.import_revision(9).await;
    let response = fixture
        .post_json(
            "/api/admin/capabilities/import",
            json!({
                "schema_version": 999,
                "revision": 10
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fixture.export().await["revision"], 9);
}

#[tokio::test]
async fn manual_probe_only_enqueues_and_returns_accepted() {
    let fixture = AdminCapabilityFixture::new().await;
    fixture.import_revision(1).await;
    let response = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            json!({
                "upstream_id": "up-1",
                "runtime_model_slug": "opaque",
                "protocol": "chat_completions"
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(response).await["queued"], true);
}

#[tokio::test]
async fn capability_probe_all_models_filter_limits_batch_scope() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    fixture.import_revision(1).await;
    let (sender, mut receiver) = mpsc::channel(1);
    fixture.state.set_capability_probe_sender(sender);

    // A models filter restricts the batch to the requested scope and the
    // response marks the probed model list.
    let scoped = fixture
        .post_json(
            "/api/admin/capabilities/probe-all",
            json!({"models": ["opaque"]}),
        )
        .await;
    assert_eq!(scoped.status(), StatusCode::OK);
    let scoped = response_json(scoped).await;
    assert_eq!(scoped["queued_routes"], 1);
    assert_eq!(scoped["models"], json!(["opaque"]));
    let scoped_job = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap()
        .into_jobs();
    assert_eq!(scoped_job.len(), 1);
    assert_eq!(
        scoped_job[0].exposed_model_slugs.iter().collect::<Vec<_>>(),
        vec!["opaque"]
    );
    fixture.state.finish_capability_probe_job(
        &scoped_job[0].key,
        &scoped_job[0].configuration,
        &chat_responses_codex::state::ProbeJobExecution::Completed,
    );

    // An unmatched model filter leaves no eligible routes.
    let unmatched = fixture
        .post_json(
            "/api/admin/capabilities/probe-all",
            json!({"models": ["not-configured"]}),
        )
        .await;
    assert_eq!(unmatched.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response_json(unmatched).await["error"]["code"],
        "capability_probe_no_eligible_routes"
    );

    // An empty filter keeps the old full-scope behavior and still reports
    // the model list.
    let full = fixture
        .post_json("/api/admin/capabilities/probe-all", json!({}))
        .await;
    assert_eq!(full.status(), StatusCode::OK);
    let full = response_json(full).await;
    assert_eq!(full["queued_routes"], 1);
    assert_eq!(full["models"], json!(["opaque"]));
}

#[tokio::test]
async fn reasoning_probe_all_requires_explicit_model_scope() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    fixture.import_revision(1).await;

    let response = fixture
        .post_json(
            "/api/admin/capabilities/probe-all",
            json!({"mode": "reasoning", "models": []}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "capability_probe_scope_required");
    assert_eq!(body["error"]["message"], "必须显式选择探测模型");
}

#[tokio::test]
async fn reasoning_probe_all_limits_candidates_to_selected_models() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    fixture
        .state
        .update_upstream(
            "up-1",
            UpstreamConfig {
                id: "up-1".into(),
                name: "Primary".into(),
                base_url: "https://example.invalid".into(),
                api_key: "upstream-secret".into(),
                supported_models: vec!["model-a".into(), "model-b".into(), "model-c".into()],
                active: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture.import_revision(1).await;
    let (sender, mut receiver) = mpsc::channel(1);
    fixture.state.set_capability_probe_sender(sender);

    let response = fixture
        .post_json(
            "/api/admin/capabilities/probe-all",
            json!({"mode": "reasoning", "models": ["model-a", "model-c"]}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["models"], json!(["model-a", "model-c"]));
    let candidate_models = body["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["exposed_model_slug"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        candidate_models,
        ["model-a", "model-c"].into_iter().collect()
    );

    let jobs = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap()
        .into_jobs();
    assert!(jobs
        .iter()
        .all(|job| job.mode == chat_responses_codex::capabilities::ProbeMode::Reasoning));
    let job_models = jobs
        .iter()
        .flat_map(|job| job.exposed_model_slugs.iter().map(String::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(job_models, ["model-a", "model-c"].into_iter().collect());
}

#[tokio::test]
async fn capability_probe_all_rejects_revision_zero_policy() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    let response = fixture
        .post_json("/api/admin/capabilities/probe-all", json!({}))
        .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "capability_policy_missing"
    );
}

#[tokio::test]
async fn manual_probe_endpoints_distinguish_policy_route_and_queue_failures() {
    let missing =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    let mut missing_route = manual_probe_request("opaque");
    missing_route["upstream_id"] = json!("missing");
    assert_probe_error(
        missing
            .post_json("/api/admin/capabilities/probe", missing_route)
            .await,
        StatusCode::CONFLICT,
        "capability_policy_missing",
        "capability policy is required before probing",
    )
    .await;
    assert_probe_error(
        missing
            .post_json("/api/admin/capabilities/probe-all", json!({}))
            .await,
        StatusCode::CONFLICT,
        "capability_policy_missing",
        "capability policy is required before probing",
    )
    .await;

    let disabled =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    disabled
        .state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 1,
            probe: ProbeConfiguration {
                enabled: false,
                ..ProbeConfiguration::default()
            },
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();
    for response in [
        disabled
            .post_json(
                "/api/admin/capabilities/probe",
                manual_probe_request("opaque"),
            )
            .await,
        disabled
            .post_json("/api/admin/capabilities/probe-all", json!({}))
            .await,
    ] {
        assert_probe_error(
            response,
            StatusCode::CONFLICT,
            "capability_probe_disabled",
            "capability probing is disabled by policy",
        )
        .await;
    }

    let no_routes =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    no_routes.import_revision(1).await;
    for response in [
        no_routes
            .post_json(
                "/api/admin/capabilities/probe",
                manual_probe_request("not-configured"),
            )
            .await,
        no_routes
            .post_json(
                "/api/admin/capabilities/probe-all",
                json!({"models": ["not-configured"]}),
            )
            .await,
    ] {
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "capability_probe_no_eligible_routes");
        let message = body["error"]["message"]
            .as_str()
            .expect("no eligible routes message");
        for expected in [
            "active upstream",
            "per-Key model mappings",
            "requested model filters",
            "supported protocols",
        ] {
            assert!(message.contains(expected), "missing {expected}: {message}");
        }
        assert!(!body.to_string().contains("upstream-secret"));
    }

    let unavailable =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    unavailable.import_revision(1).await;
    for response in [
        unavailable
            .post_json(
                "/api/admin/capabilities/probe",
                manual_probe_request("opaque"),
            )
            .await,
        unavailable
            .post_json("/api/admin/capabilities/probe-all", json!({}))
            .await,
    ] {
        assert_probe_error(
            response,
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_capability_probe_unavailable",
            "capability probe queue is unavailable",
        )
        .await;
    }
}

#[tokio::test]
async fn capability_probe_all_builds_every_exact_key_and_protocol() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    fixture.import_revision(1).await;

    let mut chat_upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    chat_upstream.api_key = "chat-key-a".into();
    chat_upstream.api_keys = vec!["chat-key-b".into()];
    chat_upstream.api_key_models = vec![
        ApiKeyModelConfig {
            api_key: "chat-key-a".into(),
            supported_models: vec!["opaque".into()],
        },
        ApiKeyModelConfig {
            api_key: "chat-key-b".into(),
            supported_models: vec!["opaque".into()],
        },
    ];
    fixture
        .state
        .update_upstream("up-1", chat_upstream)
        .await
        .unwrap();
    fixture
        .state
        .insert_upstream(UpstreamConfig {
            id: "up-responses".into(),
            name: "Responses only".into(),
            base_url: "https://responses.example.invalid".into(),
            api_key: "responses-key".into(),
            protocol: chat_responses_codex::routing::UpstreamProtocol::Responses,
            protocols: vec![chat_responses_codex::routing::UpstreamProtocol::Responses],
            supported_models: vec!["opaque".into()],
            active: true,
            ..UpstreamConfig::default()
        })
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(1);
    fixture.state.set_capability_probe_sender(sender);

    let response = fixture
        .post_json("/api/admin/capabilities/probe-all", json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["configuration_revision"], 1);
    assert_eq!(body["queued_routes"], 3);
    assert_eq!(body["reused_routes"], 0);
    assert!(body["batch_id"].is_string());
    assert!(body["started_at"].is_number());
    let candidates = body["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3);
    for candidate in candidates {
        assert!(candidate["route_id"]
            .as_str()
            .unwrap()
            .starts_with("route_"));
        assert!(candidate.get("key_fingerprint").is_none());
    }
    assert!(candidates
        .iter()
        .any(|candidate| candidate["protocol"] == "responses"));
    assert!(!body.to_string().contains("key_fingerprint"));

    let reused = fixture
        .post_json("/api/admin/capabilities/probe-all", json!({}))
        .await;
    assert_eq!(reused.status(), StatusCode::OK);
    let reused = response_json(reused).await;
    assert_eq!(reused["queued_routes"], 0);
    assert_eq!(reused["reused_routes"], 3);
    assert!(reused["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|candidate| candidate["state"] == "reused"));
    let batch_status = fixture
        .get(&format!(
            "/api/admin/capabilities/probe-batches/{}",
            body["batch_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(batch_status.status(), StatusCode::OK);
    assert_eq!(
        response_json(batch_status).await["batch_id"],
        body["batch_id"]
    );
    let missing = fixture
        .get("/api/admin/capabilities/probe-batches/not-a-batch")
        .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(missing).await["error"]["code"],
        "capability_probe_batch_not_found"
    );

    let batch = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let jobs = batch.into_jobs();
    assert_eq!(jobs.len(), 3);
    let chat_key_fingerprints = jobs
        .iter()
        .filter(|job| job.key.protocol == WireProtocol::ChatCompletions)
        .map(|job| job.key.key_fingerprint.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(chat_key_fingerprints.len(), 2);
    for job in &jobs {
        assert!(!body.to_string().contains(&job.key.key_fingerprint));
    }
    assert_eq!(
        jobs.iter()
            .filter(|job| job.key.protocol == WireProtocol::ChatCompletions)
            .count(),
        2
    );
    assert_eq!(
        jobs.iter()
            .filter(|job| job.key.protocol == WireProtocol::Responses)
            .count(),
        1
    );
}

#[tokio::test]
async fn capability_discovery_unions_successful_routes_and_keeps_failures() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    let model = "deepseek-v4-flash";
    let mut upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    upstream.api_key = "key-a".into();
    upstream.api_keys = vec!["key-b".into(), "key-c".into()];
    upstream.supported_models = vec![model.into()];
    upstream.api_key_models = ["key-a", "key-b", "key-c"]
        .into_iter()
        .map(|api_key| ApiKeyModelConfig {
            api_key: api_key.into(),
            supported_models: vec![model.into()],
        })
        .collect();
    fixture
        .state
        .update_upstream("up-1", upstream.clone())
        .await
        .unwrap();
    fixture
        .state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 1,
            policies: vec![CapabilityPolicy {
                id: "deepseek-effort-map".into(),
                selector: CapabilitySelector {
                    upstream_id: Some(upstream.id.clone()),
                    exposed_model: Some(model.into()),
                    runtime_model: Some(model.into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..CapabilitySelector::default()
                },
                semantic: SemanticPolicy {
                    effort_map: BTreeMap::from([
                        ("low".into(), "provider-low".into()),
                        ("medium".into(), "provider-medium".into()),
                        ("high".into(), "provider-high".into()),
                        ("xhigh".into(), "provider-xhigh".into()),
                        ("max".into(), "provider-max".into()),
                    ]),
                    ..SemanticPolicy::default()
                },
                ..CapabilityPolicy::default()
            }],
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();

    for (api_key, accepted_levels) in [
        ("key-a", vec!["provider-low", "provider-medium"]),
        ("key-b", vec!["provider-high"]),
        ("key-c", Vec::new()),
    ] {
        let key_fingerprint = upstream_key_fingerprint(&upstream.id, api_key);
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::for_key(
            upstream.id.clone(),
            key_fingerprint.clone(),
            model,
            WireProtocol::ChatCompletions,
        ));
        profile.configuration_fingerprint = fixture
            .state
            .route_configuration_fingerprint(
                &upstream,
                &key_fingerprint,
                model,
                model,
                chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            )
            .unwrap();
        if accepted_levels.is_empty() {
            profile.last_probe_outcome = Some(ProbeProfileOutcome::OperationalFailure);
            profile.last_operational_failure = Some("minimal_text_failed".into());
            profile.http_status = Some(503);
            profile.last_attempt_at = Some(1_000);
            profile.next_probe_at = Some(1_005);
        } else {
            profile.state = DialectProfileState::Verified;
            profile
                .capabilities
                .insert(Capability::ReasoningOutput, EvidenceState::Supported);
            profile.reasoning_controls.insert(
                "reasoning_effort".into(),
                accepted_levels.into_iter().map(Value::from).collect(),
            );
            profile.last_probe_outcome = Some(ProbeProfileOutcome::Accepted);
            profile.last_success_at = Some(999);
        }
        fixture.state.upsert_dialect_profile(profile).await.unwrap();
    }

    let response = fixture.get("/api/admin/capabilities/discovery").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let model_summary = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["exposed_model_slug"] == model)
        .unwrap();
    assert_eq!(
        model_summary["verified_reasoning_levels"],
        json!(["low", "medium", "high"])
    );
    assert_eq!(model_summary["routes"].as_array().unwrap().len(), 3);
    let failed = model_summary["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["outcome"] == "operational_failure")
        .unwrap();
    assert_eq!(failed["http_status"], 503);
    assert_eq!(failed["operational_code"], "minimal_text_failed");
    assert!(!body.to_string().contains("key_fingerprint"));
    for api_key in ["key-a", "key-b", "key-c"] {
        assert!(!body
            .to_string()
            .contains(&upstream_key_fingerprint(&upstream.id, api_key)));
    }
}

#[tokio::test]
async fn admin_reasoning_override_upserts_clears_and_reports_effective_source() {
    let fixture = AdminCapabilityFixture::new().await;
    let key_fingerprint = upstream_key_fingerprint("up-1", "upstream-secret");
    let route_id = anonymous_route_id(
        "up-1",
        &key_fingerprint,
        "opaque",
        WireProtocol::ChatCompletions,
    );
    let before = response_json(fixture.get("/api/admin/capabilities/discovery").await).await;
    let before_outcome = before["models"][0]["routes"][0]["outcome"].clone();

    let updated = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            json!({
                "upstream_id": "up-1",
                "route_id": route_id,
                "exposed_model_slug": "opaque",
                "runtime_model_slug": "opaque",
                "protocol": "chat_completions",
                "levels": ["high", "none", "low", "none", "high"],
                "scope": "route"
            }),
        )
        .await;
    assert_eq!(updated.status(), StatusCode::OK);
    let updated_body = response_json(updated).await;
    assert_eq!(updated_body["configuration_revision"], 1);
    assert_eq!(updated_body["affected_route_count"], 1);
    assert_eq!(updated_body["affected_route_ids"], json!([route_id]));
    assert!(!updated_body.to_string().contains(&key_fingerprint));

    let exported = fixture.export().await;
    let managed = &exported["route_overrides"][0];
    assert_eq!(managed["id"], format!("operator-reasoning-{route_id}"));
    assert_eq!(managed["reasoning_control_field"], "reasoning_effort");
    assert_eq!(
        managed["effort_map"],
        json!({"high": "high", "low": "low", "none": "none"})
    );
    assert_eq!(managed["capabilities"]["reasoning_output"], "supported");

    let discovery = response_json(fixture.get("/api/admin/capabilities/discovery").await).await;
    let route = &discovery["models"][0]["routes"][0];
    assert_eq!(
        route["accepted_reasoning_levels"],
        json!(["none", "low", "high"])
    );
    assert_eq!(route["reasoning_source"], "override");
    assert_eq!(route["managed_reasoning_override"], true);
    assert_eq!(route["outcome"], before_outcome);
    assert!(!discovery.to_string().contains("key_fingerprint"));
    assert!(!discovery.to_string().contains(&key_fingerprint));

    let cleared = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            json!({
                "upstream_id": "up-1",
                "route_id": route_id,
                "exposed_model_slug": "opaque",
                "runtime_model_slug": "opaque",
                "protocol": "chat_completions",
                "levels": [],
                "scope": "route"
            }),
        )
        .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(response_json(cleared).await["configuration_revision"], 2);
    assert!(fixture.export().await["route_overrides"]
        .as_array()
        .unwrap()
        .is_empty());
    let discovery = response_json(fixture.get("/api/admin/capabilities/discovery").await).await;
    let route = &discovery["models"][0]["routes"][0];
    assert_eq!(route["reasoning_source"], "baseline");
    assert_eq!(route["managed_reasoning_override"], false);
}

#[tokio::test]
async fn admin_reasoning_override_rejects_invalid_levels_and_stale_routes_atomically() {
    let fixture = AdminCapabilityFixture::new().await;
    let key_fingerprint = upstream_key_fingerprint("up-1", "upstream-secret");
    let route_id = anonymous_route_id(
        "up-1",
        &key_fingerprint,
        "opaque",
        WireProtocol::ChatCompletions,
    );
    let payload = |route_id: &str, runtime_model_slug: &str, levels: Value| {
        json!({
            "upstream_id": "up-1",
            "route_id": route_id,
            "exposed_model_slug": "opaque",
            "runtime_model_slug": runtime_model_slug,
            "protocol": "chat_completions",
            "levels": levels,
            "scope": "route"
        })
    };

    let invalid_level = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            payload(&route_id, "opaque", json!(["ultra"])),
        )
        .await;
    assert_eq!(invalid_level.status(), StatusCode::BAD_REQUEST);
    let invalid_level_body = response_json(invalid_level).await;
    assert_eq!(
        invalid_level_body["error"]["code"],
        "capability_reasoning_override_invalid_level"
    );
    assert_eq!(
        invalid_level_body["error"]["message"],
        "levels must contain only none, low, medium, high, xhigh, or max"
    );

    let stale_route = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            payload("route_stale", "opaque", json!(["low"])),
        )
        .await;
    assert_eq!(stale_route.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(stale_route).await["error"]["code"],
        "capability_reasoning_override_invalid_route"
    );

    let mismatched_model = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            payload(&route_id, "other-model", json!(["low"])),
        )
        .await;
    assert_eq!(mismatched_model.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(mismatched_model).await["error"]["code"],
        "capability_reasoning_override_invalid_route"
    );

    let exported = fixture.export().await;
    assert_eq!(exported["revision"], 0);
    assert!(exported["route_overrides"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_reasoning_override_applies_to_all_current_model_routes_once() {
    let fixture = AdminCapabilityFixture::new().await;
    let mut first = fixture.state.upstreams().await.into_iter().next().unwrap();
    first.api_key = "key-a".into();
    first.api_keys = vec!["key-b".into()];
    first.protocols = vec![
        chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        chat_responses_codex::routing::UpstreamProtocol::Responses,
    ];
    fixture
        .state
        .update_upstream("up-1", first.clone())
        .await
        .unwrap();
    fixture
        .state
        .add_upstream(UpstreamConfig {
            id: "up-2".into(),
            name: "Secondary".into(),
            base_url: "https://secondary.invalid".into(),
            api_key: "key-c".into(),
            supported_models: vec!["opaque".into()],
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let selected_route_id = anonymous_route_id(
        "up-1",
        &upstream_key_fingerprint("up-1", "key-a"),
        "opaque",
        WireProtocol::ChatCompletions,
    );

    let updated = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            json!({
                "upstream_id": "up-1",
                "route_id": selected_route_id,
                "exposed_model_slug": "opaque",
                "runtime_model_slug": "opaque",
                "protocol": "chat_completions",
                "levels": ["medium", "max"],
                "scope": "model_routes"
            }),
        )
        .await;
    assert_eq!(updated.status(), StatusCode::OK);
    let body = response_json(updated).await;
    assert_eq!(body["configuration_revision"], 1);
    assert_eq!(body["affected_route_count"], 5);
    let affected = body["affected_route_ids"].as_array().unwrap();
    assert_eq!(affected.len(), 5);
    assert_eq!(
        affected
            .iter()
            .filter_map(Value::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        5
    );
    assert!(!body.to_string().contains("key-a"));
    assert!(!body.to_string().contains("key-b"));
    assert!(!body.to_string().contains("key-c"));

    let exported = fixture.export().await;
    assert_eq!(exported["route_overrides"].as_array().unwrap().len(), 5);
    assert!(exported["route_overrides"]
        .as_array()
        .unwrap()
        .iter()
        .all(|route_override| route_override["effort_map"]
            == json!({
                "max": "max",
                "medium": "medium"
            })));
}

#[tokio::test]
async fn admin_reasoning_model_route_clear_preserves_unrelated_overrides_and_evidence() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    let model = "deepseek-v4-flash";
    let mut upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    upstream.api_key = "key-a".into();
    upstream.api_keys = vec!["key-b".into()];
    upstream.supported_models = vec![model.into()];
    upstream.api_key_models = ["key-a", "key-b"]
        .into_iter()
        .map(|api_key| ApiKeyModelConfig {
            api_key: api_key.into(),
            supported_models: vec![model.into()],
        })
        .collect();
    upstream.dialect_preset = Some("deepseek".into());
    fixture
        .state
        .update_upstream("up-1", upstream.clone())
        .await
        .unwrap();

    fixture
        .state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 7,
            policies: vec![CapabilityPolicy {
                id: "keep-policy".into(),
                selector: CapabilitySelector {
                    upstream_id: Some(upstream.id.clone()),
                    exposed_model: Some(model.into()),
                    runtime_model: Some(model.into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                semantic: SemanticPolicy {
                    effort_map: BTreeMap::from([
                        ("low".into(), "provider-low".into()),
                        ("high".into(), "provider-high".into()),
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            }],
            route_overrides: vec![RouteCapabilityOverride {
                id: "keep-unrelated".into(),
                selector: CapabilitySelector {
                    runtime_model: Some("other-model".into()),
                    ..Default::default()
                },
                capabilities: BTreeMap::from([(Capability::CustomTools, EvidenceState::Supported)]),
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();

    let key_a_fingerprint = upstream_key_fingerprint(&upstream.id, "key-a");
    let key_b_fingerprint = upstream_key_fingerprint(&upstream.id, "key-b");
    let profile_key = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_a_fingerprint.clone(),
        model,
        WireProtocol::ChatCompletions,
    );
    let mut profile = UpstreamDialectProfile::unknown(profile_key.clone());
    profile.configuration_fingerprint = fixture
        .state
        .route_configuration_fingerprint(
            &upstream,
            &key_a_fingerprint,
            model,
            model,
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    profile.reasoning_controls.insert(
        "reasoning_effort".into(),
        vec!["provider-low".into(), "provider-high".into()],
    );
    profile.last_probe_outcome = Some(ProbeProfileOutcome::Accepted);
    fixture
        .state
        .upsert_dialect_profile(profile.clone())
        .await
        .unwrap();

    let selected_route_id = anonymous_route_id(
        &upstream.id,
        &key_a_fingerprint,
        model,
        WireProtocol::ChatCompletions,
    );
    let second_route_id = anonymous_route_id(
        &upstream.id,
        &key_b_fingerprint,
        model,
        WireProtocol::ChatCompletions,
    );
    let update = |levels: Value| {
        json!({
            "upstream_id": upstream.id,
            "route_id": selected_route_id,
            "exposed_model_slug": model,
            "runtime_model_slug": model,
            "protocol": "chat_completions",
            "levels": levels,
            "scope": "model_routes"
        })
    };

    let applied = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            update(json!(["none", "high"])),
        )
        .await;
    assert_eq!(applied.status(), StatusCode::OK);
    assert_eq!(response_json(applied).await["affected_route_count"], 2);
    assert_eq!(
        fixture.export().await["route_overrides"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let cleared = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            update(json!([])),
        )
        .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared = response_json(cleared).await;
    assert_eq!(cleared["affected_route_count"], 2);
    let mut expected_route_ids = vec![selected_route_id.clone(), second_route_id.clone()];
    expected_route_ids.sort();
    assert_eq!(cleared["affected_route_ids"], json!(expected_route_ids));

    let exported = fixture.export().await;
    assert_eq!(exported["policies"].as_array().unwrap().len(), 1);
    assert_eq!(exported["policies"][0]["id"], "keep-policy");
    assert_eq!(exported["route_overrides"].as_array().unwrap().len(), 1);
    assert_eq!(exported["route_overrides"][0]["id"], "keep-unrelated");
    assert_eq!(
        fixture
            .state
            .capability_snapshot()
            .profiles
            .get(&profile_key),
        Some(&profile)
    );
    assert_eq!(
        fixture
            .state
            .upstreams()
            .await
            .into_iter()
            .find(|candidate| candidate.id == upstream.id)
            .and_then(|candidate| candidate.dialect_preset),
        Some("deepseek".into())
    );

    let discovery = response_json(fixture.get("/api/admin/capabilities/discovery").await).await;
    let routes = discovery["models"][0]["routes"].as_array().unwrap();
    let probed = routes
        .iter()
        .find(|route| route["route_id"] == selected_route_id)
        .unwrap();
    assert_eq!(probed["reasoning_source"], "probe");
    assert_eq!(probed["accepted_reasoning_levels"], json!(["low", "high"]));
    assert_eq!(probed["outcome"], "accepted");
    assert_eq!(probed["managed_reasoning_override"], false);
    let preset = routes
        .iter()
        .find(|route| route["route_id"] == second_route_id)
        .unwrap();
    assert_eq!(preset["reasoning_source"], "policy");
    assert_eq!(
        preset["accepted_reasoning_levels"],
        json!(["low", "medium", "high", "xhigh", "max"])
    );
    assert_eq!(preset["managed_reasoning_override"], false);
}

#[tokio::test]
async fn admin_reasoning_model_routes_share_case_folded_identity_and_keep_mapping_label() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    let mut first = fixture.state.upstreams().await.into_iter().next().unwrap();
    first.supported_models = vec!["Runtime-X".into()];
    first.model_mappings = vec![UpstreamModelMapping {
        upstream_model: "Runtime-X".into(),
        downstream_model: "Model-X".into(),
    }];
    fixture.state.update_upstream("up-1", first).await.unwrap();
    fixture
        .state
        .add_upstream(UpstreamConfig {
            id: "up-2".into(),
            name: "Secondary".into(),
            base_url: "https://secondary.invalid".into(),
            api_key: "key-b".into(),
            supported_models: vec!["model-x".into()],
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let discovery = response_json(fixture.get("/api/admin/capabilities/discovery").await).await;
    let models = discovery["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["exposed_model_slug"], "Model-X");
    assert_eq!(models[0]["routes"].as_array().unwrap().len(), 2);

    let route = &models[0]["routes"][0];
    let updated = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            json!({
                "upstream_id": route["upstream_id"],
                "route_id": route["route_id"],
                "exposed_model_slug": models[0]["exposed_model_slug"],
                "runtime_model_slug": route["runtime_model_slug"],
                "protocol": route["protocol"],
                "levels": ["none", "high"],
                "scope": "model_routes"
            }),
        )
        .await;
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(response_json(updated).await["affected_route_count"], 2);
}

#[tokio::test]
async fn admin_reasoning_model_routes_restore_exact_identity_when_case_folding_is_disabled() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    let mut first = fixture.state.upstreams().await.into_iter().next().unwrap();
    first.supported_models = vec!["Runtime-X".into()];
    first.model_mappings = vec![UpstreamModelMapping {
        upstream_model: "Runtime-X".into(),
        downstream_model: "Model-X".into(),
    }];
    fixture.state.update_upstream("up-1", first).await.unwrap();
    fixture
        .state
        .add_upstream(UpstreamConfig {
            id: "up-2".into(),
            name: "Secondary".into(),
            base_url: "https://secondary.invalid".into(),
            api_key: "key-b".into(),
            supported_models: vec!["model-x".into()],
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut settings = fixture.state.runtime_settings().as_ref().clone();
    settings.model_case_insensitive_matching = false;
    // T1.1: keep the config compliant (base=2 -> ceiling 8s < 30s budget) so
    // update_runtime_settings validation passes.
    settings.upstream_transient_route_cooldown_base_seconds = 2;
    fixture
        .state
        .update_runtime_settings(0, settings)
        .await
        .unwrap();

    let discovery = response_json(fixture.get("/api/admin/capabilities/discovery").await).await;
    let models = discovery["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    let model = models
        .iter()
        .find(|model| model["exposed_model_slug"] == "Model-X")
        .unwrap();
    assert_eq!(model["routes"].as_array().unwrap().len(), 1);

    let route = &model["routes"][0];
    let updated = fixture
        .put_json(
            "/api/admin/capabilities/reasoning-overrides",
            json!({
                "upstream_id": route["upstream_id"],
                "route_id": route["route_id"],
                "exposed_model_slug": model["exposed_model_slug"],
                "runtime_model_slug": route["runtime_model_slug"],
                "protocol": route["protocol"],
                "levels": ["high"],
                "scope": "model_routes"
            }),
        )
        .await;
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = response_json(updated).await;
    assert_eq!(updated["affected_route_count"], 1);

    let exported = fixture.export().await;
    let overrides = exported["route_overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0]["selector"]["upstream_id"], "up-1");
    assert_eq!(overrides[0]["selector"]["runtime_model"], "Runtime-X");
}

#[tokio::test]
async fn admin_capability_routes_require_and_preserve_the_selected_key_identity() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    fixture.import_revision(1).await;
    let mut upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    upstream.api_key = "key-a".into();
    upstream.api_keys = vec!["key-b".into()];
    upstream.api_key_models = vec![
        ApiKeyModelConfig {
            api_key: "key-a".into(),
            supported_models: vec!["opaque".into()],
        },
        ApiKeyModelConfig {
            api_key: "key-b".into(),
            supported_models: vec!["opaque".into()],
        },
    ];
    fixture
        .state
        .update_upstream("up-1", upstream.clone())
        .await
        .unwrap();
    let key_b_fingerprint = upstream_key_fingerprint("up-1", "key-b");
    let key_b_route_id = anonymous_route_id(
        "up-1",
        &key_b_fingerprint,
        "opaque",
        WireProtocol::ChatCompletions,
    );
    let (sender, mut receiver) = mpsc::channel(2);
    fixture.state.set_capability_probe_sender(sender);

    let ambiguous = fixture
        .get("/api/admin/capabilities/resolved?upstream_id=up-1&model=opaque&protocol=chat_completions")
        .await;
    assert_eq!(ambiguous.status(), StatusCode::BAD_REQUEST);

    let resolved = fixture
        .get(&format!(
            "/api/admin/capabilities/resolved?upstream_id=up-1&route_id={key_b_route_id}&model=opaque&protocol=chat_completions"
        ))
        .await;
    assert_eq!(resolved.status(), StatusCode::OK);
    assert_eq!(
        response_json(resolved).await["route"]["route_id"],
        key_b_route_id
    );

    let queued = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            json!({
                "upstream_id": "up-1",
                "route_id": key_b_route_id,
                "runtime_model_slug": "opaque",
                "protocol": "chat_completions"
            }),
        )
        .await;
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let batch = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let jobs = batch.into_jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].key.key_fingerprint, key_b_fingerprint);
}

#[tokio::test]
async fn admin_capability_views_redaction_hides_key_and_configuration_fingerprints() {
    let fixture = AdminCapabilityFixture::new().await;
    let upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    let key_fingerprint = upstream_key_fingerprint("up-1", "upstream-secret");
    let configuration_fingerprint = fixture
        .state
        .route_configuration_fingerprint(
            &upstream,
            &key_fingerprint,
            "opaque",
            "opaque",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let route_id = anonymous_route_id(
        "up-1",
        &key_fingerprint,
        "opaque",
        WireProtocol::ChatCompletions,
    );

    let profiles = response_json(fixture.get("/api/admin/capabilities/profiles").await).await;
    let resolved = response_json(
        fixture
            .get("/api/admin/capabilities/resolved?upstream_id=up-1&model=opaque&protocol=chat_completions")
            .await,
    )
    .await;

    assert_eq!(profiles["profiles"][0]["key"]["route_id"], route_id);
    assert_eq!(resolved["route"]["route_id"], route_id);
    for payload in [&profiles, &resolved] {
        let serialized = payload.to_string();
        assert!(!serialized.contains("upstream-secret"));
        assert!(!serialized.contains(&key_fingerprint));
        assert!(!serialized.contains(&configuration_fingerprint));
        assert!(!serialized.contains("key_fingerprint"));
    }
}

#[tokio::test]
async fn manual_probe_requires_exact_active_route_and_real_queue_capacity() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    fixture.import_revision(1).await;
    let payload =
        |upstream_id: &str, exposed_model_slug: &str, runtime_model_slug: &str, protocol: &str| {
            json!({
                "upstream_id": upstream_id,
                "exposed_model_slug": exposed_model_slug,
                "runtime_model_slug": runtime_model_slug,
                "protocol": protocol,
            })
        };

    let unknown = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("missing", "opaque", "opaque", "chat_completions"),
        )
        .await;
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let unconfigured = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload(
                "up-1",
                "not-configured",
                "not-configured",
                "chat_completions",
            ),
        )
        .await;
    assert_eq!(unconfigured.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let disabled_protocol = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque", "opaque", "responses"),
        )
        .await;
    assert_eq!(disabled_protocol.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let no_worker = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque", "opaque", "chat_completions"),
        )
        .await;
    assert_eq!(no_worker.status(), StatusCode::SERVICE_UNAVAILABLE);

    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    fixture.state.set_capability_probe_sender(sender);
    let closed_worker = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque", "opaque", "chat_completions"),
        )
        .await;
    assert_probe_error(
        closed_worker,
        StatusCode::SERVICE_UNAVAILABLE,
        "gateway_capability_probe_unavailable",
        "capability probe queue is unavailable",
    )
    .await;

    let mut upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    upstream.supported_models.push("opaque-two".into());
    fixture
        .state
        .update_upstream("up-1", upstream)
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(1);
    fixture.state.set_capability_probe_sender(sender);
    let accepted = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque", "opaque", "chat_completions"),
        )
        .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(accepted).await["queued"], true);

    let full_queue = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque-two", "opaque-two", "chat_completions"),
        )
        .await;
    assert_eq!(full_queue.status(), StatusCode::SERVICE_UNAVAILABLE);
    receiver
        .try_recv()
        .expect("first distinct probe should occupy the queue");
    let retried = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque-two", "opaque-two", "chat_completions"),
        )
        .await;
    assert_eq!(retried.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn admin_manual_probe_accepts_identical_pending_jobs_idempotently() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    fixture.import_revision(1).await;
    let (sender, mut receiver) = mpsc::channel(2);
    fixture.state.set_capability_probe_sender(sender);
    let body = json!({
        "upstream_id": "up-1",
        "exposed_model_slug": "opaque",
        "runtime_model_slug": "opaque",
        "protocol": "chat_completions"
    });

    let first = fixture
        .post_json("/api/admin/capabilities/probe", body.clone())
        .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(first).await["queued"], true);
    let duplicate = fixture
        .post_json("/api/admin/capabilities/probe", body)
        .await;
    assert_eq!(duplicate.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(duplicate).await["queued"], true);
    receiver
        .try_recv()
        .expect("the first request should enqueue one probe batch");
    assert!(
        receiver.try_recv().is_err(),
        "duplicate must not enqueue twice"
    );
}

#[tokio::test]
async fn completed_probe_does_not_relabel_old_evidence_after_configuration_import() {
    let first_request = Arc::new(AtomicBool::new(true));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post({
            let first_request = first_request.clone();
            let started = started.clone();
            let release = release.clone();
            move || {
                let first_request = first_request.clone();
                let started = started.clone();
                let release = release.clone();
                async move {
                    if first_request.swap(false, Ordering::SeqCst) {
                        started.notify_one();
                        release.notified().await;
                    }
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

    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url(&format!("http://{address}")).await;
    let configuration_a =
        route_override_configuration(1, Capability::UsageStream, EvidenceState::Supported);
    fixture
        .state
        .replace_capability_configuration(configuration_a)
        .await
        .unwrap();
    let upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    let fingerprint_a = fixture
        .state
        .route_configuration_fingerprint(
            &upstream,
            &upstream_key_fingerprint("up-1", "upstream-secret"),
            "opaque",
            "opaque",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let key = DialectProfileKey {
        key_fingerprint: upstream_key_fingerprint("up-1", "upstream-secret"),
        upstream_id: "up-1".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(key.clone());
    profile.configuration_fingerprint = fingerprint_a.clone();
    profile.state = DialectProfileState::Verified;
    profile.last_success_at = Some(u64::MAX);
    profile
        .capabilities
        .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    fixture.state.upsert_dialect_profile(profile).await.unwrap();
    CapabilityProbeService::spawn(fixture.state.clone());

    let accepted = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            json!({
                "upstream_id": "up-1",
                "exposed_model_slug": "opaque",
                "runtime_model_slug": "opaque",
                "protocol": "chat_completions"
            }),
        )
        .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(accepted).await["queued"], true);
    timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();

    let imported = fixture
        .post_json(
            "/api/admin/capabilities/import",
            serde_json::to_value(route_override_configuration(
                2,
                Capability::UsageStream,
                EvidenceState::Rejected,
            ))
            .unwrap(),
        )
        .await;
    assert_eq!(imported.status(), StatusCode::OK);
    release.notify_one();

    sleep(Duration::from_millis(50)).await;

    let profile = fixture
        .state
        .capability_snapshot()
        .profiles
        .get(&key)
        .unwrap()
        .clone();
    assert_eq!(profile.configuration_fingerprint, fingerprint_a);
    let resolved = fixture
        .get("/api/admin/capabilities/resolved?upstream_id=up-1&model=opaque&protocol=chat_completions")
        .await;
    let body = response_json(resolved).await;
    assert_eq!(
        body["capabilities"]["parallel_tool_calls"]["source"],
        "baseline"
    );
    assert_eq!(body["profile_state"], "unknown");
}

#[tokio::test]
async fn admin_capability_views_treat_schema_mismatched_profiles_as_stale() {
    let fixture = AdminCapabilityFixture::new().await;
    let upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    let fingerprint = fixture
        .state
        .route_configuration_fingerprint(
            &upstream,
            &upstream_key_fingerprint("up-1", "upstream-secret"),
            "opaque",
            "opaque",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let key = DialectProfileKey {
        key_fingerprint: upstream_key_fingerprint("up-1", "upstream-secret"),
        upstream_id: "up-1".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(key);
    profile.configuration_fingerprint = fingerprint;
    profile.probe_schema_version =
        chat_responses_codex::capabilities::DIALECT_PROBE_SCHEMA_VERSION - 1;
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    profile
        .extension_evidence
        .insert("probe_extension".into(), EvidenceState::Supported);
    profile
        .evidence_codes
        .insert("probe_parallel_tool_calls_supported".into());
    profile.event_types.insert("response.completed".into());
    profile.http_status = Some(200);
    profile.last_operational_failure = Some("probe_timeout".into());
    fixture.state.upsert_dialect_profile(profile).await.unwrap();

    let profiles = response_json(fixture.get("/api/admin/capabilities/profiles").await).await;
    assert_eq!(profiles["profiles"][0]["currentness"], "stale");
    assert_eq!(profiles["profiles"][0]["state"], "unknown");
    assert_eq!(
        profiles["profiles"][0]["evidence"]["capabilities"]["parallel_tool_calls"],
        "unobserved"
    );
    assert_eq!(
        profiles["profiles"][0]["sources"]["capabilities"]["parallel_tool_calls"],
        "baseline"
    );
    assert_eq!(profiles["profiles"][0]["evidence"]["extensions"], json!({}));
    assert_eq!(profiles["profiles"][0]["sources"]["extensions"], json!({}));
    assert_eq!(profiles["profiles"][0]["evidence"]["codes"], json!([]));
    assert_eq!(profiles["profiles"][0]["event_summary"]["types"], json!([]));
    assert!(profiles["profiles"][0]["status_summary"]["http_status"].is_null());
    assert!(profiles["profiles"][0]["status_summary"]["operational_code"].is_null());

    let resolved = response_json(
        fixture
            .get("/api/admin/capabilities/resolved?upstream_id=up-1&model=opaque&protocol=chat_completions")
            .await,
    )
    .await;
    assert_eq!(resolved["profile_currentness"], "stale");
    assert_eq!(resolved["profile_state"], "unknown");
    assert!(resolved["profile"]["fingerprint"].is_null());
    assert_eq!(
        resolved["capabilities"]["parallel_tool_calls"]["source"],
        "baseline"
    );
}

#[tokio::test]
async fn admin_resolved_uses_the_first_key_mapped_to_the_requested_model() {
    let tempdir = tempdir().unwrap();
    let upstream = UpstreamConfig {
        id: "mapped-upstream".into(),
        name: "Mapped upstream".into(),
        base_url: "https://example.invalid".into(),
        api_key: "key-without-model".into(),
        api_keys: vec!["key-with-model".into()],
        api_key_models: vec![
            ApiKeyModelConfig {
                api_key: "key-without-model".into(),
                supported_models: Vec::new(),
            },
            ApiKeyModelConfig {
                api_key: "key-with-model".into(),
                supported_models: vec!["opaque".into()],
            },
        ],
        supported_models: vec!["opaque".into()],
        active: true,
        ..UpstreamConfig::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(vec![upstream.clone()]),
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig {
            jwt_secret: "test_secret".into(),
            ..AppConfig::default()
        },
    );
    let key = DialectProfileKey::for_key(
        upstream.id.clone(),
        upstream_key_fingerprint(&upstream.id, "key-with-model"),
        "opaque",
        WireProtocol::ChatCompletions,
    );
    let mut profile = UpstreamDialectProfile::unknown(key);
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            &upstream_key_fingerprint(&upstream.id, "key-with-model"),
            "opaque",
            "opaque",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    profile.state = DialectProfileState::Verified;
    state.upsert_dialect_profile(profile).await.unwrap();

    let response = build_router(state)
        .oneshot(
            Request::builder()
                .uri("/api/admin/capabilities/resolved?upstream_id=mapped-upstream&model=opaque&protocol=chat_completions")
                .header(
                    header::AUTHORIZATION,
                    format!(
                        "Bearer {}",
                        generate_admin_token("admin", "test_secret").unwrap()
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["profile_currentness"], "current");
    assert_eq!(body["profile_state"], "verified");
}

#[tokio::test]
async fn capability_admin_contract_exposes_sanitized_evidence_and_structured_conflicts() {
    let fixture = AdminCapabilityFixture::new().await;
    let configuration =
        route_override_configuration(7, Capability::ParallelToolCalls, EvidenceState::Rejected);
    fixture
        .state
        .replace_capability_configuration(configuration)
        .await
        .unwrap();
    let upstream = fixture.state.upstreams().await.into_iter().next().unwrap();
    let fingerprint = fixture
        .state
        .route_configuration_fingerprint(
            &upstream,
            &upstream_key_fingerprint("up-1", "upstream-secret"),
            "opaque",
            "opaque",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let key = DialectProfileKey {
        key_fingerprint: upstream_key_fingerprint("up-1", "upstream-secret"),
        upstream_id: "up-1".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(key);
    profile.configuration_fingerprint = fingerprint.clone();
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    profile.evidence_codes = [
        "probe_parallel_tools_supported".into(),
        "prompt=do-not-return-this".into(),
    ]
    .into_iter()
    .collect();
    profile.event_types = ["response.completed".into(), "tool_result=secret".into()]
        .into_iter()
        .collect();
    fixture.state.upsert_dialect_profile(profile).await.unwrap();

    let profiles = response_json(fixture.get("/api/admin/capabilities/profiles").await).await;
    let summary = &profiles["profiles"][0];
    assert_eq!(summary["key"]["upstream_id"], "up-1");
    assert_eq!(summary["currentness"], "current");
    assert!(summary["age_seconds"].is_number() || summary["age_seconds"].is_null());
    assert_eq!(
        summary["fingerprint"],
        format!("sha256:{}", &fingerprint[..16])
    );
    assert_eq!(
        summary["evidence"]["capabilities"]["parallel_tool_calls"],
        "supported"
    );
    assert_eq!(
        summary["sources"]["capabilities"]["parallel_tool_calls"],
        "probe"
    );
    assert!(summary["evidence"]["codes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|code| code != "prompt=do-not-return-this"));
    assert!(!summary.to_string().contains("tool_result=secret"));

    let resolved = response_json(
        fixture
            .get("/api/admin/capabilities/resolved?upstream_id=up-1&model=opaque&protocol=chat_completions")
            .await,
    )
    .await;
    assert_eq!(
        resolved["capabilities"]["parallel_tool_calls"]["source"],
        "override"
    );
    assert!(resolved["field_sources"].is_object());
    assert!(resolved["token"]["field"].is_string());
    assert!(resolved["reasoning"]["carrier"].is_string());
    assert!(resolved["extensions"]["ids"].is_array());
    assert_eq!(
        resolved["conflicts"][0]["subject"],
        "capability.parallel_tool_calls"
    );
    assert!(resolved["conflicts"][0]["probe"]["code"].is_string());
    assert!(resolved["conflicts"][0]["policy"]["code"].is_string());
    assert!(!resolved.to_string().contains("prompt=do-not-return-this"));
}

#[tokio::test]
async fn capability_import_errors_are_sanitized_and_persistence_failure_keeps_old_snapshot() {
    let fixture = AdminCapabilityFixture::new().await;
    let invalid = fixture
        .post_json(
            "/api/admin/capabilities/import",
            json!({"schema_version": "credential-do-not-echo"}),
        )
        .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let invalid_body = response_json(invalid).await;
    assert_eq!(
        invalid_body["error"]["code"],
        "gateway_capability_policy_invalid"
    );
    assert!(!invalid_body.to_string().contains("credential-do-not-echo"));

    let failing = AdminCapabilityFixture::new_with_rejecting_capability_store().await;
    let failed = failing
        .post_json(
            "/api/admin/capabilities/import",
            serde_json::to_value(route_override_configuration(
                9,
                Capability::UsageStream,
                EvidenceState::Supported,
            ))
            .unwrap(),
        )
        .await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let failed_body = response_json(failed).await;
    assert_eq!(
        failed_body["error"]["code"],
        "gateway_capability_policy_persist_failed"
    );
    assert!(!failed_body.to_string().contains("credential-do-not-echo"));
    assert_eq!(failing.export().await["revision"], 0);
    assert_eq!(
        failing
            .state
            .capability_snapshot()
            .configuration
            .source()
            .revision,
        0
    );
}

#[tokio::test]
async fn capability_import_rejects_sensitive_fixture_urls_without_exporting_them() {
    let fixture = AdminCapabilityFixture::new().await;
    let secret_url = "https://fixture-user:fixture-password@fixture.invalid/image.png?signature=fixture-signature";
    let rejected = fixture
        .post_json(
            "/api/admin/capabilities/import",
            json!({
                "schema_version": 1,
                "probe": {
                    "https_image_fixture": {
                        "url": secret_url,
                        "expected_label": "fixture"
                    }
                }
            }),
        )
        .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let body = response_json(rejected).await;
    assert_eq!(body["error"]["code"], "gateway_capability_policy_invalid");
    assert!(!body.to_string().contains("fixture-password"));
    assert!(!body.to_string().contains("fixture-signature"));
    assert!(!fixture
        .export()
        .await
        .to_string()
        .contains("fixture-password"));
    assert!(!fixture
        .export()
        .await
        .to_string()
        .contains("fixture-signature"));
}

#[tokio::test]
async fn capability_export_import_round_trips_safe_fixture_urls() {
    let fixture = AdminCapabilityFixture::new().await;
    let configuration = CapabilityConfiguration {
        probe: chat_responses_codex::capabilities::ProbeConfiguration {
            https_image_fixture: Some(chat_responses_codex::capabilities::HttpsImageFixture {
                url: "https://fixture.invalid/image.png?width=64".into(),
                expected_label: "fixture".into(),
            }),
            ..Default::default()
        },
        ..CapabilityConfiguration::default()
    };
    fixture
        .state
        .replace_capability_configuration(configuration)
        .await
        .unwrap();

    let exported = fixture.export().await;
    assert_eq!(
        exported["probe"]["https_image_fixture"]["url"],
        "https://fixture.invalid/image.png?width=64"
    );
    let imported = fixture
        .post_json("/api/admin/capabilities/import", exported)
        .await;
    assert_eq!(imported.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_policy_rebootstrap_merges_builtin_entries_and_requires_confirm_for_replace() {
    let fixture = AdminCapabilityFixture::new().await;
    let operator_policy = CapabilityPolicy {
        id: "operator-custom".into(),
        ..Default::default()
    };
    fixture
        .state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 3,
            policies: vec![operator_policy.clone()],
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();

    // merge mode (default): builtin domestic entries appended, operator kept.
    let response = fixture
        .post_json(
            "/api/admin/capabilities/policy/rebootstrap",
            json!({"mode": "merge"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["mode"], "merge");
    let added = body["added"].as_array().expect("added array");
    assert!(
        added
            .iter()
            .any(|id| id.as_str().unwrap().starts_with("domestic-")),
        "merge must add builtin domestic entries, got {added:?}"
    );
    assert_eq!(body["revision"], 4);
    assert_eq!(body["builtin_policy_version"], 2);

    let export = fixture.export().await;
    let exported_policies = export["policies"].as_array().expect("policies array");
    assert!(exported_policies
        .iter()
        .any(|policy| policy["id"] == "domestic-deepseek-family"));
    assert!(
        exported_policies
            .iter()
            .any(|policy| policy["id"] == "operator-custom"),
        "operator entries must be preserved"
    );

    // Second merge is idempotent.
    let response = fixture
        .post_json(
            "/api/admin/capabilities/policy/rebootstrap",
            json!({"mode": "merge"}),
        )
        .await;
    let body = response_json(response).await;
    assert!(body["added"].as_array().expect("added").is_empty());
    assert_eq!(body["revision"], 4);

    // Replace requires explicit confirmation.
    let bad = fixture
        .post_json(
            "/api/admin/capabilities/policy/rebootstrap",
            json!({"mode": "replace"}),
        )
        .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(bad).await["error"]["code"],
        "capability_rebootstrap_requires_confirm"
    );

    // Replace with confirm reinstalls the embedded template wholesale.
    let ok = fixture
        .post_json(
            "/api/admin/capabilities/policy/rebootstrap",
            json!({"mode": "replace", "confirm": true}),
        )
        .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let export = fixture.export().await;
    assert_eq!(export["revision"], 3, "template carries its own revision");
    assert_eq!(export["builtin_policy_version"], 2);
    let exported_policies = export["policies"].as_array().expect("policies array");
    assert!(
        exported_policies
            .iter()
            .any(|policy| policy["id"] == "deepseek-v4-flash"),
        "replace must install all template policies"
    );
    assert!(
        !exported_policies
            .iter()
            .any(|policy| policy["id"] == "operator-custom"),
        "replace must not keep operator entries"
    );
}
