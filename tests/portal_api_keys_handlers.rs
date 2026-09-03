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
    let _guard = common::oidc::lock().lock().unwrap();
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

    assert_eq!(json["keys"].as_array().unwrap().len(), 0);
    assert_eq!(json["total"].as_i64().unwrap(), 0);
}

#[tokio::test]
async fn test_create_and_list_keys() {
    let _guard = common::oidc::lock().lock().unwrap();
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

    store.add_downstream_binding_with_label(&user.id, "key2", Some("Personal Key"), Some("advanced"))
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

    assert_eq!(json["total"].as_i64().unwrap(), 2);
    let keys = json["keys"].as_array().unwrap();
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
    assert_eq!(key2["model_group_id"], "advanced");
    assert_eq!(key2["is_default"], false);
    assert_eq!(key2["usage_count"], 0);
}

// ============================================================================
// Handler 2: portal_create_key
// ============================================================================

#[tokio::test]
async fn test_create_key() {
    let _guard = common::oidc::lock().lock().unwrap();
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

    assert_eq!(json["total"].as_i64().unwrap(), 1);
    let keys = json["keys"].as_array().unwrap();
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
    let _guard = common::oidc::lock().lock().unwrap();
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
    let _guard = common::oidc::lock().lock().unwrap();
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

    store.add_downstream_binding_with_label(&user.id, "key2", Some("Key 2"), Some("advanced"))
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

    let keys = json["keys"].as_array().unwrap();
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
    let _guard = common::oidc::lock().lock().unwrap();
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

    let keys = json["keys"].as_array().unwrap();

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
    let _guard = common::oidc::lock().lock().unwrap();
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

    store.add_downstream_binding_with_label(&user.id, "key2", Some("Key 2"), Some("advanced"))
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

    assert_eq!(json["total"].as_i64().unwrap(), 1);
    let keys = json["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["downstream_id"], "key1"); // Only key1 remains
}

#[tokio::test]
async fn test_delete_key_forbidden_default() {
    let _guard = common::oidc::lock().lock().unwrap();
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
    let _guard = common::oidc::lock().lock().unwrap();
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
            "INSERT INTO response_history (id, downstream_key_id, model, prompt_tokens, completion_tokens, created_at) \
             VALUES (gen_random_uuid()::text, $1, 'test-model', 10, 20, NOW())",
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
