use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{
    AppConfig, AppState, DownstreamConfig, PersistedState, UpstreamConfig,
};
use serde_json::json;
use std::net::SocketAddr;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[tokio::test]
async fn admin_login_page_renders() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/login?next=/admin/downstreams")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("管理员登录"));
    assert!(html.contains(r#"name="next" value="/admin/downstreams""#));
}

#[tokio::test]
async fn admin_login_sets_session_cookie_and_redirects_to_target_page() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let login = serde_urlencoded::to_string(json!({
        "username": "admin",
        "password": "admin",
        "next": "/admin/downstreams"
    }))
    .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(login))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin/downstreams")
    );
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("login should set a session cookie");
    assert!(set_cookie.contains("chat_responses_codex_admin_session="));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("Max-Age=43200"));

    let cookie = set_cookie
        .split(';')
        .next()
        .map(str::to_string)
        .expect("session cookie should have a name/value pair");

    let authed = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/downstreams")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(authed.status(), StatusCode::OK);
    let body = to_bytes(authed.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("下游密钥"));
}

#[tokio::test]
async fn admin_pages_redirect_to_login_without_basic_auth_challenge() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin/login")
    );
    assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
}

#[tokio::test]
async fn invalid_admin_login_renders_inline_error_without_basic_auth_challenge() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let login = serde_urlencoded::to_string(json!({
        "username": "admin",
        "password": "wrong-password",
        "next": "/admin"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(login))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("用户名或密码不正确"));
    assert!(html.contains("管理员登录"));
}

#[tokio::test]
async fn admin_logout_clears_session_cookie_and_requires_login_again() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let login = serde_urlencoded::to_string(json!({
        "username": "admin",
        "password": "admin"
    }))
    .unwrap();

    let login_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(login))
                .unwrap(),
        )
        .await
        .unwrap();

    let session_cookie = login_response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::to_string)
        .expect("login should set a session cookie");

    let logout_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/logout")
                .header(header::COOKIE, session_cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(logout_response.status().is_redirection());
    assert_eq!(
        logout_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin/login")
    );
    let clear_cookie = logout_response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("logout should clear the session cookie");
    assert!(clear_cookie.contains("chat_responses_codex_admin_session=;"));
    assert!(clear_cookie.contains("Max-Age=0"));

    let after_logout = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin")
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(after_logout.status().is_redirection());
    assert_eq!(
        after_logout
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin/login")
    );
}

#[tokio::test]
async fn admin_can_create_downstream_key_and_view_dashboard() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let dashboard = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(dashboard.status(), StatusCode::OK);
    let dashboard_body = to_bytes(dashboard.into_body(), usize::MAX).await.unwrap();
    let dashboard_html = String::from_utf8(dashboard_body.to_vec()).unwrap();
    assert!(dashboard_html.contains("仪表盘"));
    assert!(dashboard_html.contains("上游密钥"));
    assert!(dashboard_html.contains("管理上游"));
    assert!(dashboard_html.contains("查看运行日志"));

    let form = serde_urlencoded::to_string(json!({
        "name": "Team Alpha",
        "models": "gpt-4.1-mini,gpt-4o-mini",
        "per_minute_limit": 60,
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("已生成的下游密钥"));

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.downstreams.len(), 1);
    assert_eq!(snapshot.downstreams[0].name, "Team Alpha");
}

#[tokio::test]
async fn admin_upstreams_page_uses_a_drawer_layout_and_summary_cards() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary".into(),
                base_url: "https://api.example.com".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::Responses,
                supported_models: vec!["gpt-4.1-mini".into()],
                model_aliases: vec![],
                active: true,
                failure_count: 0,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/upstreams")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("上游概览"));
    assert!(html.contains("新增上游"));
    assert!(html.contains("drawer"));
    assert!(html.contains("总上游"));
    assert!(html.contains("Responses 上游"));
}

#[tokio::test]
async fn admin_logs_page_uses_summary_cards_and_context_panel() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            usage_logs: vec![chat_responses_codex::state::UsageLog {
                id: "log-1".into(),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini".into(),
                request_id: "req-1".into(),
                status_code: 200,
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                latency_ms: 42,
                created_at: 1_700_000_000,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/logs")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("运行概览"));
    assert!(html.contains("最近 50 条"));
    assert!(html.contains("Total tokens"));
    assert!(html.contains("Tokens"));
    assert!(html.contains("最新请求"));
    assert!(html.contains("请求 ID"));
}

#[tokio::test]
async fn admin_can_persist_the_plaintext_downstream_secret() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let form = serde_urlencoded::to_string(json!({
        "name": "Team Beta",
        "models": "gpt-4.1-mini",
        "per_minute_limit": 60,
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    let secret = extract_keybox_secret(&html).expect("generated secret should be visible");

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.downstreams.len(), 1);
    assert_eq!(snapshot.downstreams[0].name, "Team Beta");
    assert_eq!(
        snapshot.downstreams[0].plaintext_key.as_deref(),
        Some(secret.as_str())
    );
}

#[tokio::test]
async fn admin_can_create_permanent_downstream_without_expiry_time() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let form = serde_urlencoded::to_string(json!({
        "name": "Team Permanent",
        "models": "gpt-4.1-mini",
        "per_minute_limit": 60,
        "expires_at": "",
        "never_expires": "on",
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.downstreams.len(), 1);
    let downstream = &snapshot.downstreams[0];
    assert_eq!(downstream.name, "Team Permanent");
    assert_eq!(downstream.expires_at, None);
    assert!(downstream.plaintext_key.is_some());
}

#[tokio::test]
async fn admin_downstreams_form_includes_copy_fallback_and_expiry_toggle() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/downstreams/new")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("copyTextToClipboard"));
    assert!(html.contains("fallbackCopyTextToClipboard"));
    assert!(html.contains("syncExpiryField"));
    assert!(html.contains("Daily token limit"));
    assert!(html.contains("Monthly token limit"));
    assert!(html.contains(
        r#"id="never-expires-checkbox" type="checkbox" name="never_expires" value="on" checked"#
    ));
    assert!(html.contains(
        r#"id="expires-at-input" name="expires_at" type="number" placeholder="unix 秒，可选" value="" disabled"#
    ));
    assert!(html.contains("勾选后无需填写生效时间。"));
}

#[tokio::test]
async fn admin_can_edit_downstream_metadata_without_changing_the_secret() {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: generated.hash.clone(),
                plaintext_key: Some(generated.plaintext.clone()),
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                daily_token_limit: Some(1_000),
                monthly_token_limit: Some(2_000),
                ip_allowlist: vec!["1.2.3.4".into()],
                expires_at: Some(1_900_000_000),
                active: true,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let edit_page = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/downstreams/down-1/edit")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(edit_page.status(), StatusCode::OK);
    let edit_body = to_bytes(edit_page.into_body(), usize::MAX).await.unwrap();
    let edit_html = String::from_utf8(edit_body.to_vec()).unwrap();
    assert!(edit_html.contains("编辑下游"));
    assert!(edit_html.contains("Team Alpha"));

    let form = serde_urlencoded::to_string(json!({
        "name": "Team Alpha Updated",
        "models": "gpt-4.1-mini,gpt-4o-mini",
        "per_minute_limit": 120,
        "daily_token_limit": 3000,
        "monthly_token_limit": 4000,
        "ip_allowlist": "1.2.3.4,5.6.7.8",
        "expires_at": 1_950_000_000u64,
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams/down-1")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];
    assert_eq!(downstream.name, "Team Alpha Updated");
    assert_eq!(
        downstream.model_allowlist,
        vec!["gpt-4.1-mini".to_string(), "gpt-4o-mini".to_string()]
    );
    assert_eq!(downstream.per_minute_limit, 120);
    assert_eq!(downstream.daily_token_limit, Some(3000));
    assert_eq!(downstream.monthly_token_limit, Some(4000));
    assert_eq!(
        downstream.ip_allowlist,
        vec!["1.2.3.4".to_string(), "5.6.7.8".to_string()]
    );
    assert_eq!(downstream.expires_at, Some(1_950_000_000));
    assert_eq!(
        downstream.plaintext_key.as_deref(),
        Some(generated.plaintext.as_str())
    );
}

#[tokio::test]
async fn admin_can_rotate_a_downstream_secret() {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: generated.hash.clone(),
                plaintext_key: Some(generated.plaintext.clone()),
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                daily_token_limit: None,
                monthly_token_limit: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams/down-1/rotate")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    let rotated_secret = extract_keybox_secret(&html).expect("rotated secret should be visible");

    let snapshot = state.snapshot().await;
    let downstream = &snapshot.downstreams[0];
    assert_eq!(
        downstream.plaintext_key.as_deref(),
        Some(rotated_secret.as_str())
    );
    assert_ne!(
        downstream.plaintext_key.as_deref(),
        Some(generated.plaintext.as_str())
    );
    assert!(state
        .downstream_for_secret(&generated.plaintext)
        .await
        .is_none());
    assert!(state.downstream_for_secret(&rotated_secret).await.is_some());
}

#[tokio::test]
async fn admin_can_delete_a_downstream_record() {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: generated.hash.clone(),
                plaintext_key: Some(generated.plaintext.clone()),
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                daily_token_limit: None,
                monthly_token_limit: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams/down-1/delete")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = state.snapshot().await;
    assert!(snapshot.downstreams.is_empty());
}

#[tokio::test]
async fn admin_can_filter_downstreams_by_name_status_and_lifetime() {
    let tempdir = tempdir().unwrap();
    let alpha = generate_downstream_key("gw");
    let beta = generate_downstream_key("gw");
    let gamma = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![
                DownstreamConfig {
                    id: "down-1".into(),
                    name: "Team Alpha".into(),
                    hash: alpha.hash.clone(),
                    plaintext_key: Some(alpha.plaintext.clone()),
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    per_minute_limit: 60,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    ip_allowlist: vec![],
                    expires_at: None,
                    active: true,
                },
                DownstreamConfig {
                    id: "down-2".into(),
                    name: "Team Beta".into(),
                    hash: beta.hash.clone(),
                    plaintext_key: Some(beta.plaintext.clone()),
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    per_minute_limit: 60,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    ip_allowlist: vec![],
                    expires_at: Some(1_950_000_000),
                    active: true,
                },
                DownstreamConfig {
                    id: "down-3".into(),
                    name: "Team Gamma".into(),
                    hash: gamma.hash.clone(),
                    plaintext_key: Some(gamma.plaintext.clone()),
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    per_minute_limit: 60,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    ip_allowlist: vec![],
                    expires_at: None,
                    active: false,
                },
            ],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/downstreams?search=Alpha&status=active&lifetime=unlimited")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("Team Alpha"));
    assert!(!html.contains("Team Beta"));
    assert!(!html.contains("Team Gamma"));
    assert!(html.contains(r#"name="search" value="Alpha""#));
    assert!(html.contains(r#"value="active" selected"#));
    assert!(html.contains(r#"value="unlimited" selected"#));
}

#[tokio::test]
async fn admin_can_search_downstreams_by_secret_fragment() {
    let tempdir = tempdir().unwrap();
    let alpha = generate_downstream_key("gw");
    let beta = generate_downstream_key("gw");
    let fragment = beta.plaintext.chars().take(6).collect::<String>();
    let state = AppState::new(
        PersistedState {
            downstreams: vec![
                DownstreamConfig {
                    id: "down-1".into(),
                    name: "Team Alpha".into(),
                    hash: alpha.hash.clone(),
                    plaintext_key: Some(alpha.plaintext.clone()),
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    per_minute_limit: 60,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    ip_allowlist: vec![],
                    expires_at: None,
                    active: true,
                },
                DownstreamConfig {
                    id: "down-2".into(),
                    name: "Team Beta".into(),
                    hash: beta.hash.clone(),
                    plaintext_key: Some(beta.plaintext.clone()),
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    per_minute_limit: 60,
                    daily_token_limit: None,
                    monthly_token_limit: None,
                    ip_allowlist: vec![],
                    expires_at: Some(1_950_000_000),
                    active: false,
                },
            ],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let uri = format!(
        "/admin/downstreams?{}",
        serde_urlencoded::to_string(json!({
            "search": fragment,
            "status": "inactive",
            "lifetime": "expiring"
        }))
        .unwrap()
    );

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("Team Beta"));
    assert!(!html.contains("Team Alpha"));
    assert!(html.contains(&format!(r#"name="search" value="{}""#, fragment)));
    assert!(html.contains(r#"value="inactive" selected"#));
    assert!(html.contains(r#"value="expiring" selected"#));
}

#[tokio::test]
async fn root_redirects_to_admin_dashboard() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin")
    );
}

#[tokio::test]
async fn admin_can_disable_downstream_key_and_block_requests() {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: generated.hash.clone(),
                plaintext_key: Some(generated.plaintext.clone()),
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                daily_token_limit: None,
                monthly_token_limit: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/downstreams/down-1/toggle")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());

    let snapshot = state.snapshot().await;
    assert!(!snapshot.downstreams[0].active);

    let blocked = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", generated.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1-mini",
                        "messages": [
                            {"role": "user", "content": "Hello"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(blocked.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_can_disable_upstream_key_and_remove_its_models() {
    let tempdir = tempdir().unwrap();
    let generated = generate_downstream_key("gw");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary".into(),
                base_url: "https://api.example.com".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4.1-mini".into()],
                model_aliases: vec![],
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team Alpha".into(),
                hash: generated.hash.clone(),
                plaintext_key: Some(generated.plaintext.clone()),
                model_allowlist: vec!["gpt-4.1-mini".into()],
                per_minute_limit: 60,
                daily_token_limit: None,
                monthly_token_limit: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let before = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", generated.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let before_body = to_bytes(before.into_body(), usize::MAX).await.unwrap();
    let before_json: serde_json::Value = serde_json::from_slice(&before_body).unwrap();
    assert_eq!(before_json["data"].as_array().unwrap().len(), 1);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/upstreams/up-1/toggle")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());

    let snapshot = state.snapshot().await;
    assert!(!snapshot.upstreams[0].active);

    let after = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", generated.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after_body = to_bytes(after.into_body(), usize::MAX).await.unwrap();
    let after_json: serde_json::Value = serde_json::from_slice(&after_body).unwrap();
    assert_eq!(after_json["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn admin_can_edit_an_upstream_key() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary".into(),
                base_url: "https://api.example.com".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4.1-mini".into()],
                model_aliases: vec![],
                active: true,
                failure_count: 7,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let edit_page = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/upstreams/up-1/edit")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(edit_page.status(), StatusCode::OK);
    let edit_body = to_bytes(edit_page.into_body(), usize::MAX).await.unwrap();
    let edit_html = String::from_utf8(edit_body.to_vec()).unwrap();
    assert!(edit_html.contains("编辑上游"));
    assert!(edit_html.contains("value=\"Primary\""));

    let form = serde_urlencoded::to_string(json!({
        "id": "up-1",
        "name": "Primary Updated",
        "base_url": "https://api.example.com/v2",
        "api_key": "updated-secret",
        "protocol": "responses",
        "models": "gpt-4.1-mini,gpt-4o-mini",
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/upstreams/up-1")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());

    let snapshot = state.snapshot().await;
    let upstream = &snapshot.upstreams[0];
    assert_eq!(upstream.name, "Primary Updated");
    assert_eq!(upstream.base_url, "https://api.example.com/v2");
    assert_eq!(upstream.api_key, "updated-secret");
    assert_eq!(upstream.protocol, UpstreamProtocol::Responses);
    assert_eq!(
        upstream.supported_models,
        vec!["gpt-4.1-mini".to_string(), "gpt-4o-mini".to_string()]
    );
    assert_eq!(upstream.failure_count, 7);
}

#[tokio::test]
async fn admin_can_delete_an_upstream_key() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary".into(),
                base_url: "https://api.example.com".into(),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                supported_models: vec!["gpt-4.1-mini".into()],
                model_aliases: vec![],
                active: true,
                failure_count: 0,
            }],
            ..PersistedState::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/upstreams/up-1/delete")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection());
    let snapshot = state.snapshot().await;
    assert!(snapshot.upstreams.is_empty());
}

#[tokio::test]
async fn admin_can_fetch_current_models_into_the_upstream_form() {
    let tempdir = tempdir().unwrap();
    let upstream_server =
        spawn_upstream_model_server(vec!["gpt-4.1-mini".to_string(), "gpt-4o-mini".to_string()])
            .await;

    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let form = serde_urlencoded::to_string(json!({
        "intent": "fetch",
        "name": "Primary",
        "base_url": upstream_server,
        "api_key": "upstream-secret",
        "protocol": "chat",
        "models": "",
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/upstreams")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("gpt-4.1-mini,gpt-4o-mini"));
    assert!(html.contains("获取当前模型"));

    let snapshot = state.snapshot().await;
    assert!(snapshot.upstreams.is_empty());
}

#[tokio::test]
async fn admin_can_fetch_current_models_and_auto_generate_aliases_for_uppercase_models() {
    let tempdir = tempdir().unwrap();
    let upstream_server = spawn_upstream_model_server(vec![
        "GLM-5".to_string(),
        "gpt-4.1-mini".to_string(),
        "MiniMax-M2.7".to_string(),
    ])
    .await;

    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let app = build_router(state.clone());

    let form = serde_urlencoded::to_string(json!({
        "intent": "fetch",
        "name": "Primary",
        "base_url": upstream_server,
        "api_key": "upstream-secret",
        "protocol": "chat",
        "models": "",
        "active": "on"
    }))
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/upstreams")
                .header(header::AUTHORIZATION, basic_auth("admin", "admin"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains(r#"value="glm-5,gpt-4.1-mini,minimax-m2.7""#));
    assert!(html.contains(r#"value="glm-5=GLM-5,minimax-m2.7=MiniMax-M2.7""#));

    let snapshot = state.snapshot().await;
    assert!(snapshot.upstreams.is_empty());
}

#[tokio::test]
async fn admin_can_fetch_current_models_when_base_url_includes_v1_prefix() {
    let tempdir = tempdir().unwrap();
    let captured_path = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let upstream_server = spawn_upstream_model_server_with_path_capture(
        vec!["gpt-4.1-mini".to_string(), "gpt-4o-mini".to_string()],
        captured_path.clone(),
    )
    .await;

    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let models = state
        .fetch_models_from_endpoint(&format!("{upstream_server}/v1"), "upstream-secret")
        .await
        .unwrap();

    assert_eq!(models, vec!["gpt-4.1-mini", "gpt-4o-mini"]);
    assert_eq!(
        captured_path.lock().unwrap().clone().as_deref(),
        Some("/v1/models")
    );
}

async fn spawn_upstream_model_server(models: Vec<String>) -> String {
    let app = Router::new().route(
        "/v1/models",
        get(move || {
            let models = models.clone();
            async move { models_response(models) }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}")
}

async fn spawn_upstream_model_server_with_path_capture(
    models: Vec<String>,
    captured_path: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> String {
    let app = Router::new().fallback(
        move |axum::extract::OriginalUri(uri): axum::extract::OriginalUri| {
            let models = models.clone();
            let captured_path = captured_path.clone();
            async move {
                *captured_path.lock().unwrap() = Some(uri.path().to_string());
                models_response(models)
            }
        },
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}")
}

fn models_response(models: Vec<String>) -> axum::Json<serde_json::Value> {
    axum::Json(json!({
        "object": "list",
        "data": models.into_iter().map(|model| json!({
            "id": model,
            "object": "model"
        })).collect::<Vec<_>>()
    }))
}

fn basic_auth(username: &str, password: &str) -> String {
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {token}")
}

fn extract_keybox_secret(html: &str) -> Option<String> {
    let marker = r#"<div class="keybox">"#;
    let start = html.find(marker)? + marker.len();
    let rest = &html[start..];
    let end = rest.find("</div>")?;
    Some(rest[..end].to_string())
}
