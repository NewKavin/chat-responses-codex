use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::auth::generate_admin_token;
use chat_responses_codex::capabilities::{
    Capability, CapabilityConfiguration, CapabilitySelector, DialectProfileKey,
    DialectProfileState, EvidenceState, RouteCapabilityOverride, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::keys::upstream_key_fingerprint;
use chat_responses_codex::server::{build_router, CapabilityProbeService};
use chat_responses_codex::state::{
    ApiKeyModelConfig, AppConfig, AppState, PersistedState, UpstreamConfig,
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "Primary".into(),
                    base_url: base_url.into(),
                    api_key: "upstream-secret".into(),
                    supported_models: vec!["opaque".into()],
                    active: true,
                    ..Default::default()
                }],
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
                upstreams: vec![UpstreamConfig {
                    id: "up-1".into(),
                    name: "Primary".into(),
                    base_url: "https://example.invalid".into(),
                    api_key: "upstream-secret".into(),
                    supported_models: vec!["opaque".into()],
                    active: true,
                    ..UpstreamConfig::default()
                }],
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
async fn manual_probe_requires_exact_active_route_and_real_queue_capacity() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
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
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

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
    assert_eq!(unconfigured.status(), StatusCode::BAD_REQUEST);

    let disabled_protocol = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque", "opaque", "responses"),
        )
        .await;
    assert_eq!(disabled_protocol.status(), StatusCode::BAD_REQUEST);

    let no_worker = fixture
        .post_json(
            "/api/admin/capabilities/probe",
            payload("up-1", "opaque", "opaque", "chat_completions"),
        )
        .await;
    assert_eq!(no_worker.status(), StatusCode::SERVICE_UNAVAILABLE);

    let (sender, _receiver) = mpsc::channel(1);
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
            payload("up-1", "opaque", "opaque", "chat_completions"),
        )
        .await;
    assert_eq!(full_queue.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn admin_manual_probe_deduplicates_identical_pending_jobs() {
    let fixture =
        AdminCapabilityFixture::new_with_upstream_base_url("https://example.invalid").await;
    let (sender, _receiver) = mpsc::channel(2);
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
    let duplicate = fixture
        .post_json("/api/admin/capabilities/probe", body)
        .await;
    assert_eq!(duplicate.status(), StatusCode::SERVICE_UNAVAILABLE);
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
            upstreams: vec![upstream.clone()],
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
