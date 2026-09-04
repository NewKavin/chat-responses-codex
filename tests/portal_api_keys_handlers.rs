mod common;

use chat_responses_codex::state::AppConfig;
use chat_responses_codex::state::AppState;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

fn database_url() -> String {
    common::oidc::database_url()
        .expect("OIDC_TEST_DATABASE_URL unset; tests should skip before reaching here")
}

async fn postgres_client(database_url: &str) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(database_url, tokio_postgres::NoTls)
        .await
        .expect("oidc test db must connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// 插入一个真实存在的 downstream，供 Bearer 回退的自动建档使用。
async fn insert_test_downstream(database_url: &str, downstream_id: &str) {
    let client = postgres_client(database_url).await;
    client
        .execute(
            "INSERT INTO downstreams (id, name, hash, per_minute_limit, active)
             VALUES ($1, $1, $1, 60, TRUE) ON CONFLICT (id) DO NOTHING",
            &[&downstream_id],
        )
        .await
        .expect("inserting test downstream must succeed");
}

async fn load_state(database_url: &str) -> AppState {
    let state = AppState::load_from_database_url(database_url, AppConfig::default())
        .await
        .expect("gateway state must load against the oidc test database");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

// ============================================================================
// Handler 1: portal_list_keys
// ============================================================================

#[tokio::test]
async fn test_list_keys_empty() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user with no keys
    let user = store.create_user_with_identity(
        "empty@example.com",
        None,
        None,
        "google",
        "google_empty"
    )
    .await
    .expect("Failed to create user");

    // Create a session for this user
    let raw_sid = "test_session_id_empty";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Call GET /api/portal/keys with session cookie
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let status = response.status();

    // Debug: Print response status and body if not 200
    if status != StatusCode::OK {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        eprintln!("Response status: {}", status);
        eprintln!("Response body: {}", String::from_utf8_lossy(&body));
        panic!("Expected 200 OK, got {}", status);
    }

    // GREEN: Expect 200 OK with empty keys list
    assert_eq!(status, StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_create_and_list_keys() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "listkeys@example.com",
        None,
        None,
        "google",
        "google_listkeys"
    )
    .await
    .expect("Failed to create user");

    // Add two keys
    store.add_downstream_binding_with_label(&user.id, "key1", Some("Work Key"), Some("basic"))
        .await
        .expect("Failed to add key1");

    store.add_downstream_binding_with_label(&user.id, "key2", Some("Personal Key"), Some("premium"))
        .await
        .expect("Failed to add key2");

    // Set key1 as default
    store.set_default_key(&user.id, "key1")
        .await
        .expect("Failed to set default");

    // Create a session
    let raw_sid = "test_session_id_list";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Call GET /api/portal/keys
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let keys = json.as_array().unwrap();
    assert_eq!(keys.len(), 2);

    // Verify key1 details
    let key1 = keys.iter().find(|k| k["downstream_id"] == "key1").unwrap();
    assert_eq!(key1["label"], "Work Key");
    assert_eq!(key1["model_group_id"], "basic");
    assert_eq!(key1["is_default"], true);
    assert_eq!(key1["usage_count"], 0);

    // Verify key2 details
    let key2 = keys.iter().find(|k| k["downstream_id"] == "key2").unwrap();
    assert_eq!(key2["label"], "Personal Key");
    assert_eq!(key2["model_group_id"], "premium");
    assert_eq!(key2["is_default"], false);
    assert_eq!(key2["usage_count"], 0);
}

// ============================================================================
// Handler 2: portal_create_key
// ============================================================================

#[tokio::test]
async fn test_create_key() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "createkey@example.com",
        None,
        None,
        "google",
        "google_createkey"
    )
    .await
    .expect("Failed to create user");

    // Create a session
    let raw_sid = "test_session_id_create";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Call POST /api/portal/keys
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("POST")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "downstream_id": "new-key-1",
                "label": "Test Key",
                "model_group_id": "basic"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();

    // GREEN: Expect 201 CREATED
    assert_eq!(response.status(), StatusCode::CREATED);

    // Verify key was created by listing keys
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let keys = json.as_array().unwrap();
    assert_eq!(keys.len(), 1);

    let key = &keys[0];
    assert_eq!(key["downstream_id"], "new-key-1");
    assert_eq!(key["label"], "Test Key");
    assert_eq!(key["model_group_id"], "basic");
}

// ============================================================================
// Handler 3: portal_get_key_by_id
// ============================================================================

#[tokio::test]
async fn test_get_key_by_id() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "getkey@example.com",
        None,
        None,
        "google",
        "google_getkey"
    )
    .await
    .expect("Failed to create user");

    // Add a key
    store.add_downstream_binding_with_label(&user.id, "existing-key", Some("My Key"), Some("basic"))
        .await
        .expect("Failed to add key");

    // Create a session
    let raw_sid = "test_session_id_get";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Test 1: Get existing key
    let req = Request::builder()
        .uri("/api/portal/keys/existing-key")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();

    // GREEN: Expect 200 OK
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["downstream_id"], "existing-key");
    assert_eq!(json["label"], "My Key");
    assert_eq!(json["model_group_id"], "basic");

    // Test 2: Get non-existing key -> 404
    let req = Request::builder()
        .uri("/api/portal/keys/non-existing-key")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// Handler 4: portal_set_default_key
// ============================================================================

#[tokio::test]
async fn test_set_default_key() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "setdefault@example.com",
        None,
        None,
        "google",
        "google_setdefault"
    )
    .await
    .expect("Failed to create user");

    // Add two keys
    store.add_downstream_binding_with_label(&user.id, "key1", Some("Key 1"), Some("basic"))
        .await
        .expect("Failed to add key1");

    store.add_downstream_binding_with_label(&user.id, "key2", Some("Key 2"), Some("premium"))
        .await
        .expect("Failed to add key2");

    // Set key1 as default initially
    store.set_default_key(&user.id, "key1")
        .await
        .expect("Failed to set default");

    // Create a session
    let raw_sid = "test_session_id_setdefault";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Set key2 as default
    let req = Request::builder()
        .uri("/api/portal/keys/key2/default")
        .method("PUT")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();

    // GREEN: Expect 204 NO_CONTENT
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify key2 is now default by listing keys
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let keys = json.as_array().unwrap();
    let key1 = keys.iter().find(|k| k["downstream_id"] == "key1").unwrap();
    let key2 = keys.iter().find(|k| k["downstream_id"] == "key2").unwrap();

    // Only key2 should be default now
    assert_eq!(key1["is_default"], false);
    assert_eq!(key2["is_default"], true);
}

// ============================================================================
// Handler 5: portal_rotate_key_by_id
// ============================================================================

#[tokio::test]
async fn test_rotate_key() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "rotate@example.com",
        None,
        None,
        "google",
        "google_rotate"
    )
    .await
    .expect("Failed to create user");

    // Add a key with label and set as default
    store.add_downstream_binding_with_label(&user.id, "old-key-1", Some("Production Key"), Some("premium"))
        .await
        .expect("Failed to add key");

    store.set_default_key(&user.id, "old-key-1")
        .await
        .expect("Failed to set default");

    // Create a session
    let raw_sid = "test_session_id_rotate";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Rotate the key
    let req = Request::builder()
        .uri("/api/portal/keys/old-key-1/rotate")
        .method("POST")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "new_downstream_id": "new-key-1"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();

    // GREEN: Expect 204 NO_CONTENT
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify rotation: new key exists with same label/model_group/default status
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let keys = json.as_array().unwrap();

    // Old key should be gone (or still present if it had usage)
    let _old_key = keys.iter().find(|k| k["downstream_id"] == "old-key-1");

    // New key should exist
    let new_key = keys.iter().find(|k| k["downstream_id"] == "new-key-1").unwrap();
    assert_eq!(new_key["label"], "Production Key");
    assert_eq!(new_key["model_group_id"], "premium");
    assert_eq!(new_key["is_default"], true); // Preserved default status
}

// ============================================================================
// Handler 6: portal_delete_key
// ============================================================================

#[tokio::test]
async fn test_delete_key_success() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "delete@example.com",
        None,
        None,
        "google",
        "google_delete"
    )
    .await
    .expect("Failed to create user");

    // Add two keys
    store.add_downstream_binding_with_label(&user.id, "key1", Some("Key 1"), Some("basic"))
        .await
        .expect("Failed to add key1");

    store.add_downstream_binding_with_label(&user.id, "key2", Some("Key 2"), Some("premium"))
        .await
        .expect("Failed to add key2");

    // Set key1 as default (so key2 is not default and not used)
    store.set_default_key(&user.id, "key1")
        .await
        .expect("Failed to set default");

    // Create a session
    let raw_sid = "test_session_id_delete";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Delete key2 (non-default, unused)
    let req = Request::builder()
        .uri("/api/portal/keys/key2")
        .method("DELETE")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();

    // GREEN: Expect 204 NO_CONTENT
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify key2 was deleted
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let keys = json.as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["downstream_id"], "key1"); // Only key1 remains
}

#[tokio::test]
async fn test_delete_key_forbidden_default() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "delete2@example.com",
        None,
        None,
        "google",
        "google_delete2"
    )
    .await
    .expect("Failed to create user");

    // Add a key and set as default
    store.add_downstream_binding_with_label(&user.id, "default-key", Some("Default Key"), Some("basic"))
        .await
        .expect("Failed to add key");

    store.set_default_key(&user.id, "default-key")
        .await
        .expect("Failed to set default");

    // Create a session
    let raw_sid = "test_session_id_delete2";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Try to delete default key
    let req = Request::builder()
        .uri("/api/portal/keys/default-key")
        .method("DELETE")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    // GREEN: Expect 403 FORBIDDEN
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"]["message"].as_str().unwrap().contains("default or in use"));
}

#[tokio::test]
async fn test_delete_key_forbidden_used() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user
    let user = store.create_user_with_identity(
        "delete3@example.com",
        None,
        None,
        "google",
        "google_delete3"
    )
    .await
    .expect("Failed to create user");

    // Add two keys
    store.add_downstream_binding_with_label(&user.id, "used-key", Some("Used Key"), Some("basic"))
        .await
        .expect("Failed to add used-key");

    store.add_downstream_binding_with_label(&user.id, "default-key", Some("Default Key"), Some("basic"))
        .await
        .expect("Failed to add default-key");

    store.set_default_key(&user.id, "default-key")
        .await
        .expect("Failed to set default");

    // Simulate usage by inserting a response_history record
    let client = store.get_client().await.expect("Failed to get client");
    client
        .execute(
            "INSERT INTO response_history (downstream_key_id, response_id, items, state, created_at) \
             VALUES ($1, gen_random_uuid()::text, '[]', '{}', EXTRACT(EPOCH FROM NOW())::bigint)",
            &[&"used-key"],
        )
        .await
        .expect("Failed to insert response history");

    // Create a session
    let raw_sid = "test_session_id_delete3";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store.create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    // Build the app
    let app = chat_responses_codex::server::build_router(state.clone());

    // Try to delete used key
    let req = Request::builder()
        .uri("/api/portal/keys/used-key")
        .method("DELETE")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    // GREEN: Expect 403 FORBIDDEN
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"]["message"].as_str().unwrap().contains("default or in use"));
}

#[tokio::test]
async fn test_list_model_groups_requires_portal_session() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let app = chat_responses_codex::server::build_router(state.clone());

    // No session cookie -> unauthorized
    let req = Request::builder()
        .uri("/api/portal/model-groups")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_list_model_groups_with_session() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Create a test user + session
    let user = store
        .create_user_with_identity(
            "modelgroups@example.com",
            None,
            None,
            "google",
            "google-mg-1",
        )
        .await
        .expect("Failed to create user");
    // basic 恒可访问；授予 premium / all 后，用户应能看到全部 3 个种子分组
    store
        .grant_user_model_group(&user.id, "premium", Some("admin"))
        .await
        .expect("grant premium");
    store
        .grant_user_model_group(&user.id, "all", Some("admin"))
        .await
        .expect("grant all");
    let raw_sid = "test_session_id_model_groups";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store
        .create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    let app = chat_responses_codex::server::build_router(state.clone());
    let req = Request::builder()
        .uri("/api/portal/model-groups")
        .method("GET")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let groups = json["groups"].as_array().expect("groups must be an array");
    // 当前语义：只返回用户可访问的分组（basic + 已授权分组）
    assert_eq!(groups.len(), 3, "user-accessible groups should be exactly the three seeded groups after grant");
    let ids: Vec<&str> = groups
        .iter()
        .filter_map(|g| g["id"].as_str())
        .collect();
    assert!(ids.contains(&"basic"));
    assert!(ids.contains(&"premium"));
    assert!(ids.contains(&"all"));
}

#[tokio::test]
async fn test_update_key_model_group() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    let user = store
        .create_user_with_identity(
            "updategroup@example.com",
            None,
            None,
            "google",
            "google-updategroup-1",
        )
        .await
        .expect("Failed to create user");
    let raw_sid = "test_session_id_update_group";
    let sid_hash = sha256_hex(raw_sid.as_bytes());
    let now = chat_responses_codex::state::unix_seconds() as i64;
    store
        .create_session(&sid_hash, &user.id, now + 3600, None, None)
        .await
        .expect("Failed to create session");

    store
        .add_downstream_binding_with_label(
            &user.id,
            "key-group-test",
            Some("Group Test Key"),
            Some("basic"),
        )
        .await
        .expect("Failed to add binding");
    // 当前语义：切换分组要求用户已被授予该分组访问权限
    store
        .grant_user_model_group(&user.id, "premium", Some("admin"))
        .await
        .expect("grant premium");

    let app = chat_responses_codex::server::build_router(state.clone());

    // Move the key to the premium group
    let req = Request::builder()
        .uri("/api/portal/keys/key-group-test/model-group")
        .method("PUT")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({ "model_group_id": "premium" }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Verify the binding now points at premium
    let bindings = store
        .list_downstream_bindings_with_labels(&user.id)
        .await
        .expect("list bindings");
    let binding = bindings
        .iter()
        .find(|b| b.downstream_id == "key-group-test")
        .expect("binding must exist");
    assert_eq!(binding.model_group_id, "premium");

    // Unknown group -> 404
    let req = Request::builder()
        .uri("/api/portal/keys/key-group-test/model-group")
        .method("PUT")
        .header(header::COOKIE, format!("portal_session={}", raw_sid))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({ "model_group_id": "does-not-exist" }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// Bearer fallback: 工号+密钥 login has no OIDC cookie; the multi-key API must
// accept the Bearer JWT (sub = employee_id / downstream id) and lazily
// provision the portal account with the downstream bound as default key.
// ============================================================================



#[tokio::test]
async fn test_list_keys_bearer_jwt_fallback_provisions_user() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }
    common::oidc::reset_portal_tables(&url).await;
    // 自动建档只对真实存在的 downstream 生效（H1 修复），先插入。
    insert_test_downstream(&url, "downstream-bearer-1").await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // Simulate the JWT produced by POST /api/portal/login for 工号+密钥 login.
    let token = chat_responses_codex::auth::generate_admin_token(
        "downstream-bearer-1",
        &state.config.jwt_secret,
    )
    .expect("token must sign");

    let app = chat_responses_codex::server::build_router(state.clone());

    // No cookie at all: the request carries only the Bearer token.
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Response is a plain array containing the auto-bound default key.
    let keys = json.as_array().expect("list keys must return an array");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["downstream_id"], "downstream-bearer-1");
    assert_eq!(keys[0]["is_default"], true);

    // The downstream now owns a lazily provisioned portal account.
    let owner = store
        .find_user_id_by_downstream("downstream-bearer-1")
        .await
        .unwrap();
    assert!(
        owner.is_some(),
        "bearer login must provision a portal user for the downstream"
    );

    // A second call is idempotent and still returns one key.
    let req2 = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::OK);
    let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX)
        .await
        .unwrap();
    let json2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
    assert_eq!(json2.as_array().unwrap().len(), 1);
}

// ============================================================================
// H1: Bearer 身份对应的 downstream 不存在时，禁止自动建档（避免 admin
// JWT / 幽灵下游产生幻影门户用户和默认 key）。
// ============================================================================

#[tokio::test]
async fn test_list_keys_bearer_unknown_downstream_rejected() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }
    common::oidc::reset_portal_tables(&url).await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    // sub 指向不存在的 downstream（例如 admin 后台 JWT 的 sub）。
    let token = chat_responses_codex::auth::generate_admin_token(
        "ghost-downstream",
        &state.config.jwt_secret,
    )
    .expect("token must sign");

    let app = chat_responses_codex::server::build_router(state.clone());
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 不得留下幻影绑定。
    let owner = store
        .find_user_id_by_downstream("ghost-downstream")
        .await
        .unwrap();
    assert!(
        owner.is_none(),
        "unknown downstream must not provision a phantom portal user"
    );
}

// ============================================================================
// H2: 管理员禁用门户用户后，Bearer 路径同样被拒（cookie 路径由
// find_session 的 `AND NOT u.disabled` 保证，这里验证 Bearer 补齐）。
// ============================================================================

#[tokio::test]
async fn test_list_keys_bearer_disabled_user_rejected() {
    let _guard = common::oidc::lock().lock();
    let url = database_url();

    if !common::oidc::ensure_database(&url).await {
        return; // Skip test when database is unavailable
    }
    common::oidc::reset_portal_tables(&url).await;
    insert_test_downstream(&url, "downstream-disabled-1").await;

    let state = load_state(&url).await;
    let portal_store_opt = state.portal_store();
    let store = portal_store_opt.as_ref().expect("portal_store must exist");

    let token = chat_responses_codex::auth::generate_admin_token(
        "downstream-disabled-1",
        &state.config.jwt_secret,
    )
    .expect("token must sign");

    let app = chat_responses_codex::server::build_router(state.clone());

    // 首次访问自动建档成功。
    let req = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 管理员禁用该门户用户。
    let owner = store
        .find_user_id_by_downstream("downstream-disabled-1")
        .await
        .unwrap()
        .expect("user must exist after provisioning");
    let client = postgres_client(&url).await;
    client
        .execute(
            "UPDATE portal_users SET disabled = TRUE WHERE id = $1",
            &[&owner],
        )
        .await
        .expect("disabling user must succeed");

    // 同一 Bearer 身份再次访问必须 403，且不再返回 keys。
    let req2 = Request::builder()
        .uri("/api/portal/keys")
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::FORBIDDEN);
}
