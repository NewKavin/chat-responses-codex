use chat_responses_codex::state::{AppConfig, AppState, PersistedState, UsageLog};
use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn usage_logs_rotate_by_size_into_archive_files() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(
        PersistedState::default(),
        &state_path,
        AppConfig {
            usage_log_rotation_max_bytes: 900,
            ..AppConfig::default()
        },
    );

    for index in 1..=3 {
        state
            .append_usage_log(UsageLog {
                id: format!("log-{index}"),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini-with-a-long-name".into(),
                request_id: format!("req-{index}-{}", "x".repeat(120)),
                status_code: 200,
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                latency_ms: 10,
                created_at: index,
            })
            .await
            .unwrap();
    }

    let snapshot = state.snapshot().await;
    let ids = snapshot
        .usage_logs
        .iter()
        .map(|log| log.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["log-1", "log-2", "log-3"]);

    let persisted_state: PersistedState =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let current_ids = persisted_state
        .usage_logs
        .iter()
        .map(|log| log.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(current_ids, vec!["log-3"]);

    let archive_files = archive_files(&tempdir);
    assert!(!archive_files.is_empty());

    let mut archived_ids = Vec::new();
    for archive in archive_files {
        let archived_logs: Vec<UsageLog> =
            serde_json::from_slice(&fs::read(&archive).unwrap()).unwrap();
        archived_ids.extend(archived_logs.iter().map(|log| log.id.as_str().to_string()));
    }
    assert_eq!(archived_ids, vec!["log-1", "log-2"]);
}

#[tokio::test]
async fn load_from_path_loads_rotated_usage_logs() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(
        PersistedState::default(),
        &state_path,
        AppConfig {
            usage_log_rotation_max_bytes: 900,
            ..AppConfig::default()
        },
    );

    for index in 1..=3 {
        state
            .append_usage_log(UsageLog {
                id: format!("log-{index}"),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini-with-a-long-name".into(),
                request_id: format!("req-{index}-{}", "x".repeat(120)),
                status_code: 200,
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                latency_ms: 10,
                created_at: index,
            })
            .await
            .unwrap();
    }

    let reloaded = AppState::load_from_path(
        &state_path,
        AppConfig {
            usage_log_rotation_max_bytes: 900,
            ..AppConfig::default()
        },
    )
    .await
    .unwrap();

    let snapshot = reloaded.snapshot().await;
    let ids = snapshot
        .usage_logs
        .iter()
        .map(|log| log.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["log-1", "log-2", "log-3"]);
}

#[tokio::test]
async fn usage_log_archives_are_capped_by_count() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(
        PersistedState::default(),
        &state_path,
        AppConfig {
            usage_log_rotation_max_bytes: 900,
            usage_log_archive_max_files: 2,
            ..AppConfig::default()
        },
    );

    for index in 1..=4 {
        state
            .append_usage_log(UsageLog {
                id: format!("log-{index}"),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini-with-a-long-name".into(),
                request_id: format!("req-{index}-{}", "x".repeat(120)),
                status_code: 200,
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                latency_ms: 10,
                created_at: index,
            })
            .await
            .unwrap();
    }

    let archive_files = archive_files(&tempdir);
    assert_eq!(archive_files.len(), 2);

    let snapshot = state.snapshot().await;
    let ids = snapshot
        .usage_logs
        .iter()
        .map(|log| log.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["log-2", "log-3", "log-4"]);
}

#[tokio::test]
async fn load_from_path_prunes_existing_usage_log_archives() {
    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let state = AppState::new(
        PersistedState::default(),
        &state_path,
        AppConfig {
            usage_log_rotation_max_bytes: 900,
            usage_log_archive_max_files: 10,
            ..AppConfig::default()
        },
    );

    for index in 1..=4 {
        state
            .append_usage_log(UsageLog {
                id: format!("log-{index}"),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                endpoint: "/v1/chat/completions".into(),
                model: "gpt-4.1-mini-with-a-long-name".into(),
                request_id: format!("req-{index}-{}", "x".repeat(120)),
                status_code: 200,
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                latency_ms: 10,
                created_at: index,
            })
            .await
            .unwrap();
    }

    let reloaded = AppState::load_from_path(
        &state_path,
        AppConfig {
            usage_log_rotation_max_bytes: 900,
            usage_log_archive_max_files: 2,
            ..AppConfig::default()
        },
    )
    .await
    .unwrap();

    let archive_files = archive_files(&tempdir);
    assert_eq!(archive_files.len(), 2);

    let snapshot = reloaded.snapshot().await;
    let ids = snapshot
        .usage_logs
        .iter()
        .map(|log| log.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["log-2", "log-3", "log-4"]);
}

fn archive_files(tempdir: &tempfile::TempDir) -> Vec<std::path::PathBuf> {
    let mut files = fs::read_dir(tempdir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let file_name = path.file_name()?.to_str()?;
            if !file_name.starts_with("state.json.usage.") {
                return None;
            }

            let logs: Vec<UsageLog> = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
            let sort_key = logs.first().map(|log| log.created_at).unwrap_or(0);
            Some((sort_key, file_name.to_string(), path))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    files.into_iter().map(|(_, _, path)| path).collect()
}
