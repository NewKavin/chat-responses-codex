use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::model_identity::ModelAliasRule;
use chat_responses_codex::state::{
    AppConfig, AppState, DefaultModelContextConfig, DownstreamConfig, GlobalContextProfile,
    ModelContextConfig, PersistedState, UpstreamConfig, UpstreamModelMapping,
};
use serde_json::Value;
use tower::ServiceExt;

fn catalog_state(allowlist: &[&str]) -> (tempfile::TempDir, AppState, String) {
    let dir = tempfile::tempdir().unwrap();
    let key = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "provider".into(),
                name: "provider".into(),
                base_url: "http://127.0.0.1:9".into(),
                api_key: "test-upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["deepseek-chat".into(), "vendor-model".into()],
                model_mappings: vec![UpstreamModelMapping {
                    upstream_model: "vendor-model".into(),
                    downstream_model: "PublicModel".into(),
                }],
                active: true,
                ..Default::default()
            }].into(),
            downstreams: vec![DownstreamConfig {
                id: "consumer".into(),
                name: "consumer".into(),
                hash: key.hash,
                plaintext_key: Some(key.plaintext.clone()),
                model_allowlist: allowlist.iter().map(|s| (*s).into()).collect(),
                active: true,
                rate_limit_enabled: false,
                ..Default::default()
            }].into(),
            model_aliases: vec![ModelAliasRule {
                canonical: "deepseek-v3".into(),
                aliases: vec!["deepseek-chat".into()],
            }],
            ..Default::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    (dir, state, key.plaintext)
}

#[tokio::test]
async fn canonical_group_name_publishes_the_aliased_model() {
    let (_dir, state, key) = catalog_state(&["deepseek-v3"]);
    assert_eq!(state.available_models_for_downstream(&key).await, ["deepseek-v3"]);
}

#[tokio::test]
async fn legacy_alias_and_canonical_publish_the_same_identity() {
    let (_dir, state, key) = catalog_state(&["deepseek-chat"]);
    assert_eq!(state.available_models_for_downstream(&key).await, ["deepseek-v3"]);
}

#[tokio::test]
async fn codex_catalog_does_not_publish_denial_sentinels_or_unroutable_names() {
    for allowlist in [vec!["__none__"], vec!["not-a-published-model"]] {
        let (_dir, state, key) = catalog_state(&allowlist);
        let response = build_router(state).oneshot(
            Request::builder().uri("/v1/models?format=codex")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["models"], serde_json::json!([]));
    }
}

async fn set_contexts(state: &AppState) {
    let mut upstream = state.upstreams().await.remove(0);
    upstream.model_contexts = vec![
        chat_responses_codex::state::ModelContextConfig {
            slug:"deepseek-chat".into(),context_limit:64000,output_reserve:1024,max_output_tokens:0,context_group:String::new(),
        },
        chat_responses_codex::state::ModelContextConfig {
            slug:"vendor-model".into(),context_limit:32000,output_reserve:512,max_output_tokens:0,context_group:String::new(),
        },
    ];
    state.update_upstream("provider",upstream).await.unwrap();
}

#[tokio::test]
async fn wildcard_contexts_use_public_names_and_keep_wire_context_limits() {
    let (_dir,state,key) = catalog_state(&["*"]);
    set_contexts(&state).await;
    let downstream = state.downstream_for_secret(&key).await.unwrap();
    let contexts = state.compute_portal_model_context_limits(&downstream).await;
    assert_eq!(contexts.get("deepseek-v3").map(|c| c.context_limit),Some(64000));
    assert_eq!(contexts.get("PublicModel").map(|c| c.context_limit),Some(32000));
    assert!(!contexts.contains_key("*"));
}

#[tokio::test]
async fn codex_alias_metadata_uses_the_underlying_route_context() {
    let (_dir,state,key) = catalog_state(&["deepseek-v3"]);
    set_contexts(&state).await;
    let response = build_router(state).oneshot(Request::builder().uri("/v1/models?format=codex")
        .header("authorization",format!("Bearer {key}")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    let bytes = to_bytes(response.into_body(),usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["models"][0]["slug"],"deepseek-v3");
    assert_eq!(body["models"][0]["context_window"],64000);
}

#[tokio::test]
async fn portal_context_limits_prefer_global_profile_over_upstream_default() {
    let (_dir, state, key) = catalog_state(&["deepseek-v3"]);
    let mut upstream = state.upstreams().await.remove(0);
    upstream.model_contexts = vec![];
    upstream.default_model_context = Some(DefaultModelContextConfig {
        context_limit: 200_000,
        output_reserve: 4_096,
        max_output_tokens: 0,
        context_group: String::new(),
    });
    state.update_upstream("provider", upstream).await.unwrap();

    let mut profiles = std::collections::HashMap::new();
    profiles.insert(
        "http://127.0.0.1:9".to_string(),
        GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: "deepseek-chat".into(),
                context_limit: 1_000_000,
                output_reserve: 8_192,
                max_output_tokens: 0,
                context_group: String::new(),
            }],
            default_model_context: None,
        },
    );
    state.set_global_context_profiles(profiles).await.unwrap();

    let downstream = state.downstream_for_secret(&key).await.unwrap();
    let contexts = state.compute_portal_model_context_limits(&downstream).await;
    assert_eq!(
        contexts.get("deepseek-v3").map(|c| c.context_limit),
        Some(1_000_000),
        "global profile per-model entry must beat upstream default_model_context"
    );
}
