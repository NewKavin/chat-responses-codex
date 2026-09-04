use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

fn unique_state_path() -> PathBuf {
    let unique = Uuid::new_v4();
    PathBuf::from(format!("/tmp/test_portal_keys_routes_{unique}.json"))
}

fn create_test_state() -> (AppState, String) {
    let config = AppConfig::default();
    let generated = generate_downstream_key("sk");

    let state = PersistedState {
        upstreams: Arc::new(vec![UpstreamConfig {
            id: "upstream-1".to_string(),
            name: "Test Upstream".to_string(),
            base_url: "https://example.invalid".to_string(),
            api_key: "test-key".to_string(),
            supported_models: vec!["test-model".to_string()],
            active: true,
            ..UpstreamConfig::default()
        }]),
        downstreams: Arc::new(vec![DownstreamConfig {
            id: "test-user".to_string(),
            name: "Test User".to_string(),
            hash: generated.hash,
            plaintext_key: Some(generated.plaintext.clone()),
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            ip_allowlist: vec![],
            max_concurrency: 10,
            per_minute_limit: 100,
            rate_limit_enabled: true,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            daily_cost_limit_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            expires_at: None,
            active: true,
            billing_mode: "request".into(),
            model_concurrency_groups: vec![],
        }]),
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: Arc::new(std::collections::HashMap::new()),
        runtime_settings: None,
        model_aliases: vec![],
    };

    let portal_key = state.downstreams[0].plaintext_key.clone().unwrap();
    let app_state = AppState::new(state, unique_state_path(), config);
    (app_state, portal_key)
}

#[tokio::test]
async fn test_keys_routes_registered() {
    let (state, _portal_key) = create_test_state();
    let app = chat_responses_codex::server::build_router(state);

    // The multi-key management API is implemented; these requests are routed
    // to real handlers and fail authentication with an unauthorized response
    // (not 404, not the old 501 placeholder).
    let cases: Vec<(Method, &str, Option<&str>)> = vec![
        (Method::GET, "/api/portal/keys", None),
        (
            Method::POST,
            "/api/portal/keys",
            Some(r#"{"downstream_id": "sk-test", "label": "Test Key"}"#),
        ),
        (Method::GET, "/api/portal/keys/sk-test123", None),
        (Method::DELETE, "/api/portal/keys/sk-test123", None),
        (
            Method::POST,
            "/api/portal/keys/sk-test123/rotate",
            Some(r#"{"new_downstream_id": "sk-rotated456"}"#),
        ),
        (Method::PUT, "/api/portal/keys/sk-test123/default", None),
        (Method::PUT, "/api/portal/keys/sk-test123/model-group", None),
    ];

    for (method, uri, body) in cases {
        let mut builder = Request::builder()
            .uri(uri)
            .method(method.clone())
            .header(header::AUTHORIZATION, "Bearer test-token");
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let request = builder.body(Body::from(body.unwrap_or(""))).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        assert!(
            status != StatusCode::NOT_FOUND && status != StatusCode::NOT_IMPLEMENTED,
            "route {method} {uri} must be registered and implemented (got {status})"
        );
    }
}
