use chat_responses_codex::state::{AppConfig, AppState};
use std::env;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::timeout;

#[tokio::test]
async fn load_from_path_prefers_postgres_when_database_url_is_set() {
    let _guard = env_lock().lock().unwrap();
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");

    env::set_var(
        "DATABASE_URL",
        "postgres://127.0.0.1:1/chat_responses_codex?connect_timeout=1",
    );

    let result = timeout(
        Duration::from_secs(5),
        AppState::load_from_path(&state_path, AppConfig::default()),
    )
    .await;

    env::remove_var("DATABASE_URL");

    match result {
        Ok(Ok(_)) => panic!("startup should prefer postgres and fail without it"),
        Ok(Err(error)) => assert!(
            error.to_string().to_lowercase().contains("postgres"),
            "error should mention postgres, got: {error}"
        ),
        Err(_) => panic!("startup should not hang while trying postgres"),
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
