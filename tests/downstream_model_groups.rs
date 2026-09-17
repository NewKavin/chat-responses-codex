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
    let state = AppState::load_from_database_url(database_url, AppConfig {
        upstream_route_exhaustion_retry_max_wait_ms: 0,
        upstream_route_exhaustion_budget_alignment_enabled: false,
        ..AppConfig::default()
    })
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
            id: "group-delete-me".into(),
            name: "Group Delete Me".into(),
            description: Some("Temp group deleted during tests".into()),
            allowed_models: vec!["temp-model".into()],
            created_at: 1234567890,
            updated_at: 1234567890,
        },
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
        portal_store.create_model_group(&group).await.expect("create fixture group");
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
            model_group_id: Some("group-basic".into()),
            rate_limit_enabled: true,
            per_minute_limit: 100,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
            billing_mode: "request".into(),
            model_concurrency_groups: vec![],
    ..Default::default()},
        DownstreamConfig {
            id: "downstream-with-invalid-group".into(),
            name: "Downstream With Invalid Group".into(),
            hash: "hash2".into(),
            plaintext_key: Some("test-key-2".into()),
            plaintext_key_prefix: None,
            model_allowlist: vec!["fallback-model".into()],
            model_group_id: Some("group-delete-me".into()),
            rate_limit_enabled: true,
            per_minute_limit: 100,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
            billing_mode: "request".into(),
    ..Default::default()},
        DownstreamConfig {
            id: "downstream-wildcard".into(),
            name: "Downstream Wildcard".into(),
            hash: key5.hash.clone(),
            plaintext_key: Some(key5.plaintext.clone()),
            plaintext_key_prefix: None,
            model_group_id: Some("group-wildcard".into()),
            rate_limit_enabled: true,
            per_minute_limit: 100,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
            billing_mode: "request".into(),
            model_concurrency_groups: vec![],
    ..Default::default()},
    ];

    for downstream in downstreams {
        state.insert_downstream(downstream).await.expect("insert fixture downstream");
    }

    // 未绑组历史形态（manual allowlist / 空白名单）：这些用例验证的是
    // T16 删除前仍然生效的"未绑组 → model_allowlist 回退"读路径；只放内存、
    // 不落库（T15 后 DB 层不会再产生 NULL 行，故用 add_downstream 造内存态）。
    let manual = DownstreamConfig {
        id: "downstream-manual".into(),
        name: "Downstream Manual".into(),
        hash: key3.hash.clone(),
        plaintext_key: Some(key3.plaintext.clone()),
        model_allowlist: vec!["manual-model-1".into(), "manual-model-2".into()],
        per_minute_limit: 100,
        active: true,
        ..Default::default()
    };
    state.add_downstream(manual).await.expect("add manual downstream in memory");
    let empty = DownstreamConfig {
        id: "downstream-empty".into(),
        name: "Downstream Empty".into(),
        hash: "hash4".into(),
        plaintext_key: Some("test-key-4".into()),
        per_minute_limit: 100,
        active: true,
        ..Default::default()
    };
    state.add_downstream(empty).await.expect("add empty downstream in memory");

    // 一个不可达的 upstream，使「放行后路由」与「被分组拒绝」可区分。
    state
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
        .await.expect("insert fixture upstream");
}

fn gateway_app(state: AppState) -> axum::Router {
    chat_responses_codex::server::build_router(state)
}

async fn fresh_gateway_env() -> Option<(AppState, axum::Router, String, String, String)> {
    let url = database_url()?;
    common::oidc::reset_portal_tables(&url).await;
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
    let _guard = common::oidc::lock().await;
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
    let _guard = common::oidc::lock().await;
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
    let _guard = common::oidc::lock().await;
    let Some((_state, app, _key1, _key3, key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    let (status, ids) = models_request(&app, &key5).await;
    assert_eq!(status, StatusCode::OK, "wildcard group must list models");
    assert!(!ids.is_empty(), "wildcard group should expose models: {ids:?}");
}

/// File mode retains legacy allowlist semantics without a portal store.
#[tokio::test]
async fn gateway_manual_allowlist_still_enforced() {
    let _guard = common::oidc::lock().await;
    let Some((state, _app, _key1, key3, _key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file_state = AppState::new(state.snapshot().await,dir.path().join("state.json"),AppConfig::default());
    let app = gateway_app(file_state);

    // manual allowlist 内的模型在列表中。
    let (status, ids) = models_request(&app, &key3).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ids.contains(&"manual-model-1".to_string()), "manual allowlist model: {ids:?}");

    // 不在 manual allowlist 的模型 403。
    let (status, payload) = chat_request(&app, &key3, "not-in-allowlist").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manual allowlist must still reject: {payload}");
}

/// 阶段 1：批量解析下游有效白名单与逐条解析结果一致（组优先、无组用白名单、组不存在回退）。
#[tokio::test]
async fn batch_effective_allowlist_matches_single_resolution() {
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let snapshot = state.routing_snapshot().await;
    let downstreams: Vec<DownstreamConfig> = snapshot.downstreams.to_vec();

    // 批量版
    let batch = state.effective_model_allowlist_map(&downstreams).await;
    // 逐条版（对照）
    let mut single = std::collections::HashMap::new();
    for downstream in &downstreams {
        let resolved = state
            .effective_model_allowlist(downstream)
            .await
            .unwrap_or_else(|_| downstream.model_allowlist.clone());
        single.insert(downstream.id.clone(), resolved);
    }

    assert_eq!(batch.len(), downstreams.len(), "batch map must cover all downstreams");
    for downstream in &downstreams {
        let batch_value = batch.get(&downstream.id).unwrap();
        let single_value = single.get(&downstream.id).unwrap();
        assert_eq!(
            batch_value, single_value,
            "batch resolution diverges for {}",
            downstream.id
        );
    }

    // 组优先语义抽查：downstream-with-group 应解析为组模型。
    let grouped = batch.get("downstream-with-group").unwrap();
    assert!(
        grouped.contains(&"gpt-3.5-turbo".to_string()),
        "grouped downstream should resolve group models, got {:?}",
        grouped
    );
}

/// W1（通配符缺口）：下游绑 all 组（["*"）时 Codex 目录必须返回全部
/// active upstream 模型，而不是只有一个字面叫 * 的模型。
#[tokio::test]
async fn gateway_codex_catalog_wildcard_group_returns_all_upstream_models() {
    let _guard = common::oidc::lock().await;
    let Some((state, app, _key1, _key3, _key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    // 造一个绑 all 组的下游（all 组 allowed_models = ["*"）
    let key6 = generate_downstream_key("gw");
    let ds = chat_responses_codex::state::DownstreamConfig {
        id: "downstream-wildcard-all".into(),
        name: "Wildcard All".into(),
        hash: key6.hash.clone(),
        plaintext_key: Some(key6.plaintext.clone()),
        model_allowlist: vec![],
        model_group_id: Some("all".into()),
        ..Default::default()
    };
    state.insert_downstream(ds).await.expect("insert wildcard-all downstream");

    // 直接走 HTTP：Codex 目录应包含全部 3 个 upstream 模型
    let res = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/v1/models?format=codex")
                .header(header::AUTHORIZATION, format!("Bearer {}", key6.plaintext))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let slugs: Vec<String> = payload["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["slug"].as_str().map(String::from))
        .collect();

    // upstream 模型：gpt-3.5-turbo / claude-instant / manual-model-1
    for expected in ["gpt-3.5-turbo", "claude-instant", "manual-model-1"] {
        assert!(
            slugs.contains(&expected.to_string()),
            "wildcard group must expose upstream model {expected}, got {slugs:?}"
        );
    }
    assert!(
        !slugs.iter().any(|s| s == "*"),
        "literal '*' must not appear in catalog, got {slugs:?}"
    );
}

/// W2（通配符缺口）：绑 all 组的下游请求 OpenAI 风格 /v1/models
/// 必须返回全部 active upstream 模型。portal_model_is_allowed 只做精确
/// 成员判定，["*"] 会把它滤成空列表；应改走 model_list_allows。
#[tokio::test]
async fn gateway_openai_models_wildcard_group_returns_all_upstream_models() {
    let _guard = common::oidc::lock().await;
    let Some((state, app, _key1, _key3, _key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    let key7 = generate_downstream_key("gw");
    let ds = chat_responses_codex::state::DownstreamConfig {
        id: "downstream-wildcard-all-openai".into(),
        name: "Wildcard All OpenAI".into(),
        hash: key7.hash.clone(),
        plaintext_key: Some(key7.plaintext.clone()),
        model_allowlist: vec![],
        model_group_id: Some("all".into()),
        ..Default::default()
    };
    state.insert_downstream(ds).await.expect("insert downstream");

    let res = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header(header::AUTHORIZATION, format!("Bearer {}", key7.plaintext))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let ids: Vec<String> = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect();

    for expected in ["gpt-3.5-turbo", "claude-instant", "manual-model-1"] {
        assert!(
            ids.contains(&expected.to_string()),
            "wildcard group must expose upstream model {expected}, got {ids:?}"
        );
    }
    assert!(!ids.is_empty(), "catalog must not be emptied by wildcard group");
}

/// W2（通配符缺口）：scope=visible 的模型集合（downstream_visible_models）
/// 对绑 all 组的下游必须包含全部 active upstream 模型。
/// 场景刻意保持"仅 all 组下游存在"，避免其他下游的 allowlist 掩盖缺口。
#[tokio::test]
async fn visible_models_wildcard_group_returns_all_upstream_models() {
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;

    // 只放一个绑 all 组的下游（all 种子组已在 SCHEMA_SQL 中）
    let key8 = generate_downstream_key("gw");
    let ds = chat_responses_codex::state::DownstreamConfig {
        id: "downstream-wildcard-visible".into(),
        name: "Wildcard Visible".into(),
        hash: key8.hash.clone(),
        plaintext_key: Some(key8.plaintext.clone()),
        model_allowlist: vec![],
        model_group_id: Some("all".into()),
        ..Default::default()
    };
    state.insert_downstream(ds).await.expect("insert downstream");

    // 一个 active upstream，3 个模型
    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-wildcard".into(),
            name: "Wildcard Upstream".into(),
            base_url: "http://127.0.0.1:9".into(),
            api_key: "unused".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec![
                "gpt-3.5-turbo".into(),
                "claude-instant".into(),
                "manual-model-1".into(),
            ],
            active: true,
            failure_count: 0,
            ..Default::default()
        })
        .await.expect("insert wildcard fixture upstream");

    let visible = state.downstream_visible_models().await;
    for expected in ["gpt-3.5-turbo", "claude-instant", "manual-model-1"] {
        assert!(
            visible.contains(&expected.to_string()),
            "wildcard group must expose upstream model {expected}, got {visible:?}"
        );
    }
    assert!(!visible.is_empty(), "visible set must not be emptied by wildcard group");
}

/// 阶段 1：Codex 目录（/v1/models?format=codex）必须与模型分组一致，
/// 而不是只读 model_allowlist（空白名单 = 全放行会暴露组外模型）。
#[tokio::test]
async fn gateway_codex_catalog_matches_model_group() {
    let _guard = common::oidc::lock().await;
    let Some((_state, app, key1, _key3, _key5)) = fresh_gateway_env().await else {
        eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
        return;
    };

    let res = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/v1/models?format=codex")
                .header(header::AUTHORIZATION, format!("Bearer {}", key1))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let slugs: Vec<String> = payload["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["slug"].as_str().map(String::from))
        .collect();

    // group-basic = [gpt-3.5-turbo, claude-instant]；这两个必须在目录里。
    assert!(slugs.contains(&"gpt-3.5-turbo".to_string()), "in-group model missing: {slugs:?}");
    assert!(slugs.contains(&"claude-instant".to_string()), "in-group model missing: {slugs:?}");
    // manual-model-1 不在组里（虽然上游支持、且 allowlist 为空=旧逻辑会全放行），必须不在目录里。
    assert!(
        !slugs.contains(&"manual-model-1".to_string()),
        "out-of-group model leaked into codex catalog: {slugs:?}"
    );
}

// ============================================================================
// RED Phase Tests - These should FAIL initially
// ============================================================================

#[tokio::test]
async fn downstream_with_model_group_allows_models_from_group() {
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
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
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
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
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
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

    // Should use model_allowlist (still the fallback source until T16)
    assert_eq!(allowed_models.len(), 2);
    assert!(allowed_models.contains(&"manual-model-1".to_string()));
    assert!(allowed_models.contains(&"manual-model-2".to_string()));
}

#[tokio::test]
async fn downstream_with_invalid_group_falls_back_to_allowlist() {
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    setup_test_data(&state).await;

    let portal_store = state.portal_store().expect("Portal store required");

    // 组存在时：使用分组模型（FK 保证不会绑定不存在的组）。
    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-with-invalid-group")
        .expect("Downstream should exist");
    let allowed_models = downstream
        .get_allowed_models(&*portal_store)
        .await
        .unwrap();
    assert_eq!(allowed_models.len(), 1);
    assert!(allowed_models.contains(&"temp-model".to_string()));

    // 删除组后（FK ON DELETE SET NULL / T15 后为 SET DEFAULT deny-all）：
    // 组为空时回退到 model_allowlist（T16 前仍生效）。
    portal_store
        .delete_model_group("group-delete-me")
        .await
        .expect("delete group should succeed");
    let snapshot = state.routing_snapshot().await;
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|d| d.id == "downstream-with-invalid-group")
        .expect("Downstream should exist");
    let allowed_models = downstream
        .get_allowed_models(&*portal_store)
        .await
        .unwrap();
    assert_eq!(allowed_models.len(), 1);
    assert!(allowed_models.contains(&"fallback-model".to_string()));
}

#[tokio::test]
async fn downstream_with_wildcard_group_allows_all_models() {
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
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
    let _guard = common::oidc::lock().await;
    let url = match database_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: OIDC_TEST_DATABASE_URL not set");
            return;
        }
    };

    common::oidc::reset_portal_tables(&url).await;
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
