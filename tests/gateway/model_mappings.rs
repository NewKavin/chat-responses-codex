//! Part B-3: per-upstream model mappings — routing integration tests.
//!
//! Covers plan section 5 items 1/2/3/6: isolated per-upstream mappings,
//! case-folding + global alias overlay, stale mapping skip/revive, and
//! downstream-name usage accounting.

use super::common::*;
use chat_responses_codex::capabilities::{
    DialectProfileKey, DialectProfileState, UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::UpstreamModelMapping;

fn mapped_upstream(
    id: &str,
    supported_models: &[&str],
    mappings: &[(&str, &str)],
) -> UpstreamConfig {
    UpstreamConfig {
        id: id.into(),
        name: id.into(),
        base_url: "https://placeholder.invalid".into(),
        api_key: format!("secret-{id}"),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: supported_models.iter().map(|m| (*m).to_owned()).collect(),
        model_mappings: mappings
            .iter()
            .map(|(upstream_model, downstream_model)| UpstreamModelMapping {
                upstream_model: (*upstream_model).to_owned(),
                downstream_model: (*downstream_model).to_owned(),
            })
            .collect(),
        active: true,
        ..Default::default()
    }
}

fn catalog_state_with_aliases(
    upstreams: Vec<UpstreamConfig>,
    model_allowlist: Vec<String>,
    model_aliases: Vec<chat_responses_codex::state::model_identity::ModelAliasRule>,
) -> (tempfile::TempDir, AppState, String) {
    let tempdir = tempdir().unwrap();
    let downstream_key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: std::sync::Arc::new(upstreams),
            downstreams: std::sync::Arc::new(vec![DownstreamConfig {
                id: "catalog-downstream".into(),
                name: "catalog-downstream".into(),
                hash: downstream_key.hash,
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist,
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 0,
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
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
            runtime_settings: None,
            model_aliases,
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    (tempdir, state, downstream_key.plaintext)
}

/// Mock chat upstream that records `(label, payload model)` per hit.
async fn spawn_payload_recording_chat_upstream(
    label: &'static str,
    _api_key: &'static str,
    recorded: Arc<Mutex<Vec<(String, String)>>>,
) -> String {
    let recorded_for_handler = recorded.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let recorded = recorded_for_handler.clone();
            async move {
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                recorded
                    .lock()
                    .unwrap()
                    .push((
                        label.to_string(),
                        payload["model"].as_str().unwrap_or_default().to_string(),
                    ));
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-mappings",
                        "object": "chat.completion",
                        "created": 1,
                        "model": payload["model"],
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });
    format!("http://{address}")
}

/// Seed a Verified profile keyed by the *upstream* spelling while exposing
/// the *downstream* requested name (mirrors the request-time fingerprint).
async fn seed_verified_profile(
    state: &AppState,
    upstream: &UpstreamConfig,
    exposed_model: &str,
    runtime_model_slug: &str,
) {
    let key_fingerprint = chat_responses_codex::keys::upstream_key_fingerprint(
        &upstream.id,
        &upstream.api_key,
    );
    let profile_key = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint.clone(),
        runtime_model_slug,
        WireProtocol::ChatCompletions,
    );
    let mut profile = UpstreamDialectProfile::unknown(profile_key);
    profile.configuration_fingerprint = state
        .route_configuration_fingerprint(
            upstream,
            &key_fingerprint,
            exposed_model,
            runtime_model_slug,
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    profile.state = DialectProfileState::Verified;
    state.upsert_dialect_profile(profile).await.unwrap();
}

async fn send_chat_request(app: &Router, secret: &str, model: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, payload)
}

#[tokio::test]
async fn per_upstream_mappings_isolate_same_named_upstream_models() {
    // A: gpt-4 -> gpt-4-premium; B: gpt-4 -> gpt-4-standard; C: unmapped gpt-4.
    let recorded = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let url_a = spawn_payload_recording_chat_upstream("up-a", "secret-up-a", recorded.clone()).await;
    let url_b = spawn_payload_recording_chat_upstream("up-b", "secret-up-b", recorded.clone()).await;
    let url_c = spawn_payload_recording_chat_upstream("up-c", "secret-up-c", recorded.clone()).await;

    let mut upstream_a = mapped_upstream("up-a", &["gpt-4"], &[("gpt-4", "gpt-4-premium")]);
    upstream_a.base_url = url_a;
    let mut upstream_b = mapped_upstream("up-b", &["gpt-4"], &[("gpt-4", "gpt-4-standard")]);
    upstream_b.base_url = url_b;
    let mut upstream_c = mapped_upstream("up-c", &["gpt-4"], &[]);
    upstream_c.base_url = url_c;

    let (_tempdir, state, secret) = catalog_state_with_aliases(
        vec![upstream_a.clone(), upstream_b.clone(), upstream_c.clone()],
        vec![
            "gpt-4-premium".into(),
            "gpt-4-standard".into(),
            "gpt-4".into(),
        ],
        vec![],
    );
    seed_verified_profile(&state, &upstream_a, "gpt-4-premium", "gpt-4").await;
    seed_verified_profile(&state, &upstream_b, "gpt-4-standard", "gpt-4").await;
    seed_verified_profile(&state, &upstream_c, "gpt-4", "gpt-4").await;
    let app = build_router(state.clone());

    let (status, _payload) = send_chat_request(&app, &secret, "gpt-4-premium").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        recorded.lock().unwrap().as_slice(),
        [("up-a".to_string(), "gpt-4".to_string())],
        "gpt-4-premium must route only to upstream A with the upstream spelling"
    );

    let (status, _payload) = send_chat_request(&app, &secret, "gpt-4-standard").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        recorded.lock().unwrap().as_slice(),
        [
            ("up-a".to_string(), "gpt-4".to_string()),
            ("up-b".to_string(), "gpt-4".to_string()),
        ],
        "gpt-4-standard must route only to upstream B"
    );

    let (status, _payload) = send_chat_request(&app, &secret, "gpt-4").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        recorded.lock().unwrap().as_slice(),
        [
            ("up-a".to_string(), "gpt-4".to_string()),
            ("up-b".to_string(), "gpt-4".to_string()),
            ("up-c".to_string(), "gpt-4".to_string()),
        ],
        "plain gpt-4 must route only to the unmapped upstream C (A/B originals occupied)"
    );

    // Usage/quotas record the downstream-facing name (order of the snapshot
    // is not guaranteed; the mapped entries must never leak the upstream
    // spelling "gpt-4" for A/B).
    let usage_logs = state.usage_logs().await;
    assert_eq!(usage_logs.len(), 3);
    let mut recorded_models = usage_logs
        .iter()
        .map(|log| log.model.clone())
        .collect::<Vec<_>>();
    recorded_models.sort();
    assert_eq!(
        recorded_models,
        vec!["gpt-4".to_string(), "gpt-4-premium".to_string(), "gpt-4-standard".to_string()],
        "usage logs must record downstream-facing names: {recorded_models:?}"
    );
}

#[tokio::test]
async fn mapped_routes_fold_case_and_stack_after_global_alias_normalization() {
    let recorded = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let url = spawn_payload_recording_chat_upstream("up-deepseek", "secret-deepseek", recorded.clone()).await;
    let mut upstream = mapped_upstream(
        "up-deepseek",
        &["DeepSeek-Chat"],
        &[("DeepSeek-Chat", "deepseek-v3")],
    );
    upstream.base_url = url;

    // Global rule: alias "deepseek-chat" -> canonical "deepseek-v3".
    let (_tempdir, state, secret) = catalog_state_with_aliases(
        vec![upstream.clone()],
        vec!["deepseek-v3".into(), "deepseek-chat".into()],
        vec![chat_responses_codex::state::model_identity::ModelAliasRule {
            canonical: "deepseek-v3".into(),
            aliases: vec!["deepseek-chat".into()],
        }],
    );
    seed_verified_profile(&state, &upstream, "deepseek-v3", "DeepSeek-Chat").await;
    let app = build_router(state.clone());

    // Case-folded direct hit on the mapping's downstream name.
    let (status, _payload) = send_chat_request(&app, &secret, "DEEPSEEK-V3").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        recorded.lock().unwrap().as_slice(),
        [("up-deepseek".to_string(), "DeepSeek-Chat".to_string())],
        "mapped request must use the upstream stored spelling on the wire"
    );

    // Global alias normalizes first, then the mapping resolves (3.3 order).
    let (status, _payload) = send_chat_request(&app, &secret, "deepseek-chat").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        recorded.lock().unwrap().as_slice(),
        [
            ("up-deepseek".to_string(), "DeepSeek-Chat".to_string()),
            ("up-deepseek".to_string(), "DeepSeek-Chat".to_string()),
        ],
        "global alias -> canonical -> per-upstream mapping"
    );
}

// A scoped, thread-local tracing dispatch (mirrors capability_probe.rs) so
// this test never installs the process-global tracing subscriber: the gateway
// suite already has exactly one test that claims the global slot.
#[tokio::test(flavor = "current_thread")]
async fn stale_model_mapping_is_skipped_and_revives_without_config_change() {
    // Phase 1: the mapped upstream_model is not in any model list -> the
    // mapping must be skipped (no panic, no route), with a visible log line.
    let capture = TracingCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let _capture_guard = tracing::dispatcher::set_default(&dispatch);

    let upstream = mapped_upstream("up-stale", &["gpt-4"], &[("removed-model", "gpt-x")]);
    let (_tempdir, state, secret) = catalog_state_with_aliases(
        vec![upstream.clone()],
        vec!["gpt-x".into(), "gpt-4".into()],
        vec![],
    );
    seed_verified_profile(&state, &upstream, "gpt-4", "gpt-4").await;
    let app = build_router(state.clone());

    let (status, payload) = send_chat_request(&app, &secret, "gpt-x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(payload["error"]["code"], "gateway_no_routable_upstream");
    drop(_capture_guard);
    let trace = capture.contents();
    assert!(
        trace.contains("stale") && trace.contains("removed-model"),
        "expected a stale-mapping log line, got: {trace}"
    );

    // Phase 2: same mapping config, upstream lists the model again
    // (model sync restored it) -> the mapping revives without edits.
    let mut revived = mapped_upstream("up-stale", &["gpt-4", "removed-model"], &[("removed-model", "gpt-x")]);
    let recorded = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let url = spawn_payload_recording_chat_upstream("up-stale", "secret-up-stale", recorded.clone()).await;
    revived.base_url = url;
    let (_tempdir2, state2, secret2) = catalog_state_with_aliases(
        vec![revived.clone()],
        vec!["gpt-x".into(), "gpt-4".into()],
        vec![],
    );
    seed_verified_profile(&state2, &revived, "gpt-x", "removed-model").await;
    let app2 = build_router(state2.clone());
    let (status, payload2) = send_chat_request(&app2, &secret2, "gpt-x").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        recorded.lock().unwrap().as_slice(),
        [("up-stale".to_string(), "removed-model".to_string())],
        "restored supported_models must revive the mapping without config edits"
    );
    assert_eq!(payload2["choices"][0]["message"]["content"], "ok");
}

/// Request `/v1/models` in the standard or codex format (mirrors the helper
/// in capability_routing.rs; kept local to avoid cross-test coupling).
async fn get_models(state: AppState, secret: &str, codex: bool) -> Value {
    let uri = if codex {
        "/v1/models?client_version=0.144.1"
    } else {
        "/v1/models"
    };
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Plan section 5 item 1 (catalog limb): with A `gpt-4 -> gpt-4-premium`,
/// B `gpt-4 -> gpt-4-standard`, C unmapped `gpt-4`, both catalog formats
/// expose exactly `gpt-4` (from C), `gpt-4-premium`, `gpt-4-standard` —
/// no duplicates, and never A/B's plain `gpt-4`.
#[tokio::test]
async fn per_upstream_mapping_catalogs_expose_downstream_names_only() {
    let upstream_a = mapped_upstream("up-a", &["gpt-4"], &[("gpt-4", "gpt-4-premium")]);
    let upstream_b = mapped_upstream("up-b", &["gpt-4"], &[("gpt-4", "gpt-4-standard")]);
    let upstream_c = mapped_upstream("up-c", &["gpt-4"], &[]);
    let (_tempdir, state, secret) = catalog_state_with_aliases(
        vec![upstream_a, upstream_b, upstream_c],
        vec![
            "gpt-4-premium".into(),
            "gpt-4-standard".into(),
            "gpt-4".into(),
        ],
        vec![],
    );

    let standard = get_models(state.clone(), &secret, false).await;
    let mut ids = standard["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "gpt-4".to_string(),
            "gpt-4-premium".to_string(),
            "gpt-4-standard".to_string()
        ],
        "standard catalog must expose the three effective downstream names exactly once"
    );

    let codex = get_models(state, &secret, true).await;
    let mut slugs = codex["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["slug"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    slugs.sort();
    assert_eq!(
        slugs,
        vec![
            "gpt-4".to_string(),
            "gpt-4-premium".to_string(),
            "gpt-4-standard".to_string()
        ],
        "codex catalog must expose the same effective downstream set"
    );
}
