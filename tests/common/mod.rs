use chat_responses_codex::state::{AppConfig, AppState, PersistedState};
use axum::Router;
use tempfile::TempDir;

pub async fn setup_test_app() -> (Router, AppState, TempDir) {
    let temp_dir = tempfile::tempdir().unwrap();
    let state_path = temp_dir.path().join("state.json");

    let config = AppConfig {
        admin_username: "admin".to_string(),
        admin_password: "admin_password".to_string(),
        jwt_secret: "test_secret".to_string(),
        ..Default::default()
    };

    let persisted_state = PersistedState::default();
    let state = AppState::new(persisted_state, state_path, config.clone());
    let app = chat_responses_codex::server::build_router(state.clone());

    (app, state, temp_dir)
}
