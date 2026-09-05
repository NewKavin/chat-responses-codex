//! Integration tests for downstream model group functionality

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{AppConfig, AppState, DownstreamConfig, ModelGroup, UpstreamConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

fn database_url() -> Option<String> {
    common::oidc::database_url()
}

async fn load_state(database_url: &str) -> AppState {
    let state = AppState::load_from_database_url(database_url, AppConfig::default())
        .await
        .expect("gateway state must load against the test database");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

async fn setup_test_data(state: &AppState) {
    let portal_store = state.portal_store().expect("Portal store required");

    // Create test model groups
    let model_groups = vec![
        ModelGroup {
            id: "group-basic".into(),
            name: "Basic Models".into(),
            description: Some("Basic tier models".into()),
            allowed_models: vec!["gpt-3.5-turbo".into(), "claude-instant".into()],
            created_at: 1234567890,
            updated_at: 1234567890,
        },
        ModelGroup {
            id: "group-wildcard".into(),
            name: "All Models".into(),
            description: Some("Wildcard group allowing all models".into()),
            allowed_models: vec!["*".into()],
            created_at: 1234567890,
            updated_at: 1234567890,
        },
    ];

    for group in model_groups {
        let _ = portal_store.create_model_group(&group).await;
    }

    // Create test downstreams (hash/plaintext must be a matched pair so
    // gateway auth can resolve them over HTTP).
    let key1 = generate_downstream_key("gw");
    let key3 = generate_downstream_key("gw");
    let key5 = generate_downstream_key("gw");
    let downstreams = vec![
        DownstreamConfig {
            id: "downstream-with-group".into(),
            name: "Downstream With Group".into(),
            hash: key1.hash.clone(),
            plaintext_key: Some(key1.plaintext.clone()),
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            model_group_id: Some("group-basic".into()),
            rate_limit_enabled: true,
            per_minute_limit: 100,
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
        },
        DownstreamConfig {
            id: "downstream-with-invalid-group".into(),
            name: "Downstream With Invalid Group".into(),
            hash: "hash2".into(),
            plaintext_key: Some("test-key-2".into()),
            plaintext_key_prefix: None,
            model_allowlist: vec!["fallback-model".into()],
            model_group_id: Some("non-existent-group".into()),
            rate_limit_enabled: true,
            per_minute_limit: 100,
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
        },
        DownstreamConfig {
            id: "downstream-manual".into(),
            name: "Downstream Manual".into(),
            hash: key3.hash.clone(),
            plaintext_key: Some(key3.plaintext.clone()),
            plaintext_key_prefix: None,
            model_allowlist: vec!["manual-model-1".into(), "manual-model-2".into()],
            model_group_id: None,
            rate_limit_enabled: true,
            per_minute_limit: 100,
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
        },
        DownstreamConfig {
            id: "downstream-empty".into(),
            name: "Downstream Empty".into(),
            hash: "hash4".into(),
            plaintext_key: Some("test-key-4".into()),
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            model_group_id: None,
            rate_limit_enabled: true,
            per_minute_limit: 100,
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
        },
        DownstreamConfig {
            id: "downstream-wildcard".into(),
            name: "Downstream Wildcard".into(),
            hash: key5.hash.clone(),
            plaintext_key: Some(key5.plaintext.clone()),
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            model_group_id: Some("group-wildcard".into()),
            rate_limit_enabled: true,
            per_minute_limit: 100,
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
        },
    ];

    for downstream in downstreams {
        let _ = state.insert_downstream(downstream).await;
    }

    // 一个不可达的 upstream，使「放行后路由」与「被分组拒绝」可区分。
    let _ = state
        .insert_upstream(UpstreamConfig {
            id: "up-unreachable".into(),
            name: "Unreachable".into(),
            base_url: "http://127.0.0.1:9".into(),
            api_key: "unused".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["gpt-3.5-turbo".into(), "claude-instant".into(), "manual-model-1".into()],
            active: true,
            failure_count: 0,
            ..Default::default()
        })
        .await;
}

fn gateway_app(state: AppState) -> axum::Router {
    chat_responses_codex::server::build_router(state)
}

async fn fresh_gateway_env() -> Option<(AppState, axum::Router, String, String, String)> {
    let url = database_url()?;
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    setup_test_data(&state).await;
    let app = gateway_app(state.clone());
    let snapshot = state.routing_snapshot().await;
    let key1 = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-with-group")
        .and_then(|d| d.plaintext_key.clone())?;
    let key3 = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-manual")
        .and_then(|d| d.plaintext_key.clone())?;
    let key5 = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-wildcard")
        .and_then(|d| d.plaintext_key.clone())?;
    Some((state, app, key1, key3, key5))
}

async fn chat_request(
    app: &axum::Router,
    key: &str,
    model: &str,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "Hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, payload)
}

async fn models_request(app: &axum::Router, key: &str) -> (StatusCode, Vec<String>) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let ids = payload["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    (status, ids)
}

/// 网关 HTTP 路径必须执行 downstream.model_group_id 的模型限制：
/// 组内模型放行（后续因上游不可达而 502/503），组外模型 403。
#[tokio::test]
async fn gateway_http_enforces_downstream_model_group() {
    let _guard = common::oidc::lock().lock();
    let Some((_state, app, key1, _key3, _key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    // /v1/models 列表必须按分组过滤：只出现 group-basic 的模型。
    let (status, ids) = models_request(&app, &key1).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ids.contains(&"gpt-3.5-turbo".to_string()),
        "in-group model must be listed: {ids:?}"
    );
    assert!(
        !ids.contains(&"claude-3-opus".to_string()),
        "out-of-group model must NOT be listed: {ids:?}"
    );

    // 组外模型请求：403（在路由前被分组检查拒绝，无需上游）。
    let (status, payload) = chat_request(&app, &key1, "claude-3-opus").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "out-of-group model must be 403: {payload}");
    let code = payload["error"]["code"].as_str().unwrap_or("");
    assert!(
        code.ends_with("model_not_allowed"),
        "expected model_not_allowed code, got {code}"
    );
}

/// 分组匹配与 config 层一致：大小写不敏感、归一化。
#[tokio::test]
async fn gateway_group_matching_is_normalized() {
    let _guard = common::oidc::lock().lock();
    let Some((_state, app, key1, _key3, _key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    // 分组写的是小写，请求大写：config 层归一化放行，分组层也必须放行。
    let (status, _payload) = chat_request(&app, &key1, "GPT-3.5-TURBO").await;
    assert_ne!(status, StatusCode::FORBIDDEN, "case-insensitive group match");
    let (_status, ids) = models_request(&app, &key1).await;
    assert!(
        ids.contains(&"gpt-3.5-turbo".to_string()),
        "canonical model must appear for downstream: {ids:?}"
    );
}

/// 通配分组（*）放行任意模型。
#[tokio::test]
async fn gateway_wildcard_group_allows_any_model() {
    let _guard = common::oidc::lock().lock();
    let Some((_state, app, _key1, _key3, key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    let (status, ids) = models_request(&app, &key5).await;
    assert_eq!(status, StatusCode::OK, "wildcard group must list models");
    assert!(!ids.is_empty(), "wildcard group should expose models: {ids:?}");
}

/// 未配置分组的 downstream 继续走 model_allowlist 语义。
#[tokio::test]
async fn gateway_manual_allowlist_still_enforced() {
    let _guard = common::oidc::lock().lock();
    let Some((_state, app, _key1, key3, _key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    // manual allowlist 内的模型在列表中。
    let (status, ids) = models_request(&app, &key3).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ids.contains(&"manual-model-1".to_string()), "manual allowlist model: {ids:?}");

    // 不在 manual allowlist 的模型 403。
    let (status, payload) = chat_request(&app, &key3, "not-in-allowlist").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manual allowlist must still reject: {payload}");
}

// ============================================================================
// RED Phase Tests - These should FAIL initially
// ============================================================================

#[tokio::test]
async fn downstream_with_model_group_allows_models_from_group() {
    let _guard = common::oidc::lock().lock();
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-with-group")
        .expect("Downstream should exist");

    let portal_store = state.portal_store().expect("Portal store required");
    let allowed_models = downstream
        .get_allowed_models(&*portal_store)
        .await
        .unwrap();

    // Should get models from "group-basic": gpt-3.5-turbo, claude-instant
    assert_eq!(allowed_models.len(), 2);
    assert!(allowed_models.contains(&"gpt-3.5-turbo".to_string()));
    assert!(allowed_models.contains(&"claude-instant".to_string()));
}

#[tokio::test]
async fn downstream_with_model_group_rejects_models_not_in_group() {
    let _guard = common::oidc::lock().lock();
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-with-group")
        .expect("Downstream should exist");

    let portal_store = state.portal_store().expect("Portal store required");

    // gpt-3.5-turbo is in the group - should be allowed
    assert!(downstream.allows_model("gpt-3.5-turbo", &*portal_store).await);

    // gpt-4 is NOT in the group - should be rejected
    assert!(!downstream.allows_model("gpt-4", &*portal_store).await);
}

#[tokio::test]
async fn downstream_without_model_group_uses_allowlist() {
    let _guard = common::oidc::lock().lock();
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-manual")
        .expect("Downstream should exist");

    let portal_store = state.portal_store().expect("Portal store required");
    let allowed_models = downstream
        .get_allowed_models(&*portal_store)
        .await
        .unwrap();

    // Should use model_allowlist
    assert_eq!(allowed_models.len(), 2);
    assert!(allowed_models.contains(&"manual-model-1".to_string()));
    assert!(allowed_models.contains(&"manual-model-2".to_string()));
}

#[tokio::test]
async fn downstream_with_invalid_group_falls_back_to_allowlist() {
    let _guard = common::oidc::lock().lock();
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-with-invalid-group")
        .expect("Downstream should exist");

    let portal_store = state.portal_store().expect("Portal store required");
    let allowed_models = downstream
        .get_allowed_models(&*portal_store)
        .await
        .unwrap();

    // Should fall back to model_allowlist
    assert_eq!(allowed_models.len(), 1);
    assert!(allowed_models.contains(&"fallback-model".to_string()));
}

#[tokio::test]
async fn downstream_with_wildcard_group_allows_all_models() {
    let _guard = common::oidc::lock().lock();
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-wildcard")
        .expect("Downstream should exist");

    let portal_store = state.portal_store().expect("Portal store required");

    // Should allow any model due to wildcard
    assert!(downstream.allows_model("gpt-4", &*portal_store).await);
    assert!(downstream.allows_model("claude-3", &*portal_store).await);
    assert!(downstream.allows_model("any-random-model", &*portal_store).await);
}

#[tokio::test]
async fn empty_allowlist_and_no_group_allows_all_models() {
    let _guard = common::oidc::lock().lock();
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-empty")
        .expect("Downstream should exist");

    let portal_store = state.portal_store().expect("Portal store required");

    // Empty allowlist with no group should allow all models
    assert!(downstream.allows_model("any-model", &*portal_store).await);
    assert!(downstream.allows_model("another-model", &*portal_store).await);
}
