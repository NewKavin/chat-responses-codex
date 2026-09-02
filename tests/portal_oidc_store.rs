mod common;

use chat_responses_codex::state::AppConfig;
use std::str::FromStr;
use tokio_postgres::{Config, NoTls};

fn database_url() -> String {
    common::oidc::database_url()
        .expect("OIDC_TEST_DATABASE_URL unset; tests should skip before reaching here")
}

async fn table_exists(database_url: &str, table: &str) -> bool {
    let mut config = Config::from_str(database_url).unwrap();
    if config.get_password().is_none() {
        if let Ok(password) = std::env::var("PGPASSWORD") {
            config.password(password);
        }
    }
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1)",
            &[&table],
        )
        .await
        .map(|row| row.get::<_, bool>(0))
        .unwrap_or(false)
}

#[tokio::test]
async fn schema_init_creates_all_four_portal_tables() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;

    let _state =
        chat_responses_codex::state::AppState::load_from_database_url(&url, AppConfig::default())
            .await
            .expect("gateway state must load against the oidc test database");

    for table in [
        "portal_users",
        "portal_identities",
        "portal_user_downstreams",
        "portal_sessions",
    ] {
        assert!(
            table_exists(&url, table).await,
            "schema initializer must create {table}"
        );
    }
}

use chat_responses_codex::state::{AppState, PersistedState};

async fn load_state(database_url: &str) -> AppState {
    let state = AppState::load_from_database_url(database_url, AppConfig::default())
        .await
        .expect("gateway state must load against the oidc test database");
    let (probe_sender, mut probe_receiver) = tokio::sync::mpsc::channel(16);
    state.set_capability_probe_sender(probe_sender);
    tokio::spawn(async move { while probe_receiver.recv().await.is_some() {} });
    state
}

fn test_downstream(id: &str) -> chat_responses_codex::state::DownstreamConfig {
    chat_responses_codex::state::DownstreamConfig {
        id: id.to_string(),
        name: id.to_string(),
        hash: format!("hash-{id}"),
        active: true,
        model_allowlist: vec![],
        ..Default::default()
    }
}

async fn seed_downstream(state: &AppState, id: &str) {
    state
        .insert_downstream(test_downstream(id))
        .await
        .expect("downstream insert must succeed");
}

#[tokio::test]
async fn file_mode_has_no_portal_store() {
    use tempfile::TempDir;
    let directory = TempDir::new().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    assert!(
        state.portal_store().is_none(),
        "file mode must not offer a portal store (OIDC endpoints 503)"
    );
}

#[tokio::test]
async fn create_user_with_identity_then_find_by_identity() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state
        .portal_store()
        .expect("postgres mode must have a portal store");

    let user = store
        .create_user_with_identity(
            "alice@example.com",
            Some("Alice"),
            Some("alice"),
            "oidc",
            "sub-alice",
        )
        .await
        .unwrap();
    assert!(!user.id.is_empty());
    assert_eq!(user.email, "alice@example.com");
    assert_eq!(user.display_name.as_deref(), Some("Alice"));
    assert!(!user.disabled);
    assert_eq!(user.provider.as_deref(), Some("oidc"));

    let found = store
        .find_user_by_identity("oidc", "sub-alice")
        .await
        .unwrap()
        .expect("identity lookup must hit");
    assert_eq!(found.id, user.id);
    assert_eq!(found.email, "alice@example.com");

    assert!(
        store
            .find_user_by_identity("oidc", "unknown-sub")
            .await
            .unwrap()
            .is_none(),
        "unbound subject must miss"
    );
}

#[tokio::test]
async fn duplicate_identity_or_email_is_conflict() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().unwrap();

    store
        .create_user_with_identity("alice@example.com", None, None, "oidc", "sub-1")
        .await
        .unwrap();

    let dup_identity = store
        .create_user_with_identity("bob@example.com", None, None, "oidc", "sub-1")
        .await;
    assert!(matches!(
        dup_identity,
        Err(chat_responses_codex::state::PortalStoreError::Conflict(_))
    ));

    let dup_email = store
        .create_user_with_identity("alice@example.com", None, None, "oidc", "sub-2")
        .await;
    assert!(matches!(
        dup_email,
        Err(chat_responses_codex::state::PortalStoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn bind_identity_already_bound_to_other_user_conflicts() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().unwrap();

    let alice = store
        .create_user_with_identity("alice@example.com", None, None, "oidc", "sub-a")
        .await
        .unwrap();
    let bob = store
        .create_user_with_identity("bob@example.com", None, None, "oidc", "sub-b")
        .await
        .unwrap();

    // bind sub-b to alice: sub-b already owned by bob -> conflict
    let err = store.create_identity(&alice.id, "oidc", "sub-b").await;
    assert!(matches!(
        err,
        Err(chat_responses_codex::state::PortalStoreError::Conflict(_))
    ));

    // a fresh subject binds fine
    store
        .create_identity(&alice.id, "oidc", "sub-alice-2")
        .await
        .unwrap();
    assert!(store
        .find_user_by_identity("oidc", "sub-alice-2")
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        store
            .find_user_by_identity("oidc", "sub-alice-2")
            .await
            .unwrap()
            .unwrap()
            .id,
        alice.id
    );
    let _ = bob;
}

#[tokio::test]
async fn downstream_bindings_and_default_promotion() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().unwrap();
    seed_downstream(&state, "team-a").await;
    seed_downstream(&state, "team-b").await;

    let user = store
        .create_user_with_identity("alice@example.com", None, None, "oidc", "sub-a")
        .await
        .unwrap();

    store
        .add_downstream_binding(&user.id, "team-a", true)
        .await
        .unwrap();
    store
        .add_downstream_binding(&user.id, "team-b", false)
        .await
        .unwrap();
    assert_eq!(
        store.default_downstream(&user.id).await.unwrap().as_deref(),
        Some("team-a")
    );

    // promote team-b -> team-a demoted
    store
        .add_downstream_binding(&user.id, "team-b", true)
        .await
        .unwrap();
    assert_eq!(
        store.default_downstream(&user.id).await.unwrap().as_deref(),
        Some("team-b")
    );
    let bindings = store.list_downstream_bindings(&user.id).await.unwrap();
    assert_eq!(bindings.iter().filter(|b| b.is_default).count(), 1);

    // removing the default promotes the remaining row
    store
        .remove_downstream_binding(&user.id, "team-b")
        .await
        .unwrap();
    assert_eq!(
        store.default_downstream(&user.id).await.unwrap().as_deref(),
        Some("team-a")
    );

    // removing the last binding leaves none
    store
        .remove_downstream_binding(&user.id, "team-a")
        .await
        .unwrap();
    assert_eq!(store.default_downstream(&user.id).await.unwrap(), None);
}

#[tokio::test]
async fn sessions_roundtrip_expire_when_stale_and_survive_until_then() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().unwrap();
    let user = store
        .create_user_with_identity("alice@example.com", None, None, "oidc", "sub-a")
        .await
        .unwrap();
    let now = chat_responses_codex::state::unix_seconds() as i64;
    let sid_hash = "a".repeat(64);
    store
        .create_session(
            sid_hash.as_str(),
            &user.id,
            now + 3600,
            Some("ua"),
            Some("1.2.3.4"),
        )
        .await
        .unwrap();
    let session = store
        .find_session(&sid_hash)
        .await
        .unwrap()
        .expect("live session must be found");
    assert_eq!(session.user_id, user.id);
    assert_eq!(session.user_agent.as_deref(), Some("ua"));
    assert_eq!(session.ip.as_deref(), Some("1.2.3.4"));

    // expired session is not found
    store
        .create_session("b".repeat(64).as_str(), &user.id, now - 10, None, None)
        .await
        .unwrap();
    assert!(store
        .find_session("b".repeat(64).as_str())
        .await
        .unwrap()
        .is_none());

    // touched session refreshes last_seen_at
    store.touch_session(&sid_hash).await.unwrap();
    let touched = store.find_session(&sid_hash).await.unwrap().unwrap();
    assert!(touched.last_seen_at.unwrap() >= now);
}

#[tokio::test]
async fn disabling_user_purges_sessions_immediately() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().unwrap();
    let user = store
        .create_user_with_identity("alice@example.com", None, None, "oidc", "sub-a")
        .await
        .unwrap();
    store
        .create_session(
            "a".repeat(64).as_str(),
            &user.id,
            (chat_responses_codex::state::unix_seconds() as i64) + 3600,
            None,
            None,
        )
        .await
        .unwrap();

    assert!(store.set_user_disabled(&user.id, true).await.unwrap());
    assert!(
        store
            .find_session("a".repeat(64).as_str())
            .await
            .unwrap()
            .is_none(),
        "disabled users' sessions must be purged in the disabling transaction"
    );
    assert!(
        store
            .find_user_by_identity("oidc", "sub-a")
            .await
            .unwrap()
            .unwrap()
            .disabled
    );

    // re-enable clears the disabled flag but the session is gone for good
    assert!(store.set_user_disabled(&user.id, false).await.unwrap());
    assert!(
        store
            .find_user_by_identity("oidc", "sub-a")
            .await
            .unwrap()
            .unwrap()
            .disabled
            == false
    );
}

#[tokio::test]
async fn list_users_keywords_and_totals() {
    let Some(url) = common::oidc::database_url() else {
        eprintln!("skipping: OIDC_TEST_DATABASE_URL unset");
        return;
    };
    let _guard = common::oidc::lock().lock();
    if !common::oidc::ensure_database(&url).await {
        return;
    }
    common::oidc::reset_portal_tables(&url).await;
    let state = load_state(&url).await;
    let store = state.portal_store().unwrap();
    seed_downstream(&state, "team-a").await;

    store
        .create_user_with_identity(
            "alice@example.com",
            Some("Alice"),
            Some("alice"),
            "oidc",
            "sub-a",
        )
        .await
        .unwrap();
    let bob = store
        .create_user_with_identity("bob@example.com", Some("Bob"), Some("bob"), "oidc", "sub-b")
        .await
        .unwrap();
    store
        .add_downstream_binding(&bob.id, "team-a", true)
        .await
        .unwrap();

    let (total, page) = store.list_users("", 10, 0).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(page.len(), 2);
    let bob_row = page.iter().find(|u| u.email == "bob@example.com").unwrap();
    assert_eq!(bob_row.binding_count, 1);
    assert_eq!(bob_row.provider.as_deref(), Some("oidc"));

    let (total, page) = store.list_users("alice", 10, 0).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(page[0].email, "alice@example.com");

    let (total, page) = store.list_users("", 1, 1).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(page.len(), 1);
}
