use chat_responses_codex::state::AppConfig;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[test]
fn template_files_live_under_templates_directory() {
    assert!(Path::new("templates/codex/config.toml.example").exists());
    assert!(Path::new("templates/codex/model-catalog.json").exists());
    assert!(Path::new("templates/state/gateway-state.example.json").exists());
}

#[test]
fn codex_model_catalog_preserves_upstream_model_slugs_exactly() {
    let catalog: Value =
        serde_json::from_str(&fs::read_to_string("templates/codex/model-catalog.json").unwrap())
            .unwrap();
    let models = catalog["models"].as_array().expect("catalog models array");
    let slugs = models
        .iter()
        .map(|model| model["slug"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        slugs,
        vec![
            "ZhipuAI/GLM-5",
            "MiniMax/MiniMax-M2.7",
            "deepseek-ai/DeepSeek-R1-0528",
        ]
    );

    for model in models {
        assert_eq!(
            model["supports_search_tool"], false,
            "template catalog should not overstate search tool support"
        );
    }
}

#[test]
fn codex_config_example_uses_live_model_slug_exactly() {
    let config = fs::read_to_string("templates/codex/config.toml.example").unwrap();

    assert!(config.contains(r#"model = "ZhipuAI/GLM-5""#));
    assert!(config.contains(r#"review_model = "ZhipuAI/GLM-5""#));
    assert!(!config.contains(r#"model = "glm-5""#));
    assert!(config.contains(r#"model_catalog_json = "model-catalog.json""#));
    assert!(!config
        .contains("/absolute/path/to/chat-responses-codex/templates/codex/model-catalog.json"));
}

#[test]
fn gateway_state_example_exposes_live_model_ids_exactly() {
    let state: Value = serde_json::from_str(
        &fs::read_to_string("templates/state/gateway-state.example.json").unwrap(),
    )
    .unwrap();
    let upstreams = state["upstreams"].as_array().expect("upstreams array");
    let supported_models = upstreams
        .iter()
        .map(|upstream| {
            upstream["supported_models"]
                .as_array()
                .expect("supported_models array")
                .iter()
                .map(|model| model.as_str().expect("model slug"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        supported_models,
        vec![
            vec!["ZhipuAI/GLM-5"],
            vec!["MiniMax/MiniMax-M2.7"],
            vec!["deepseek-ai/DeepSeek-R1-0528"],
        ]
    );
}

#[test]
fn app_config_defaults_stream_watchdog_settings() {
    let config = AppConfig::default();

    assert_eq!(config.upstream_stream_keepalive_interval_seconds, 10);
    assert_eq!(config.upstream_stream_idle_timeout_seconds, 1_800);
    assert_eq!(config.upstream_stream_max_duration_seconds, 86_400);
    assert_eq!(config.model_probe_refresh_interval_seconds, 15);
    assert_eq!(config.upstream_model_key_sync_interval_seconds, 900);
}

#[test]
fn app_config_defaults_concurrency_retry_policy() {
    let config = AppConfig::default();

    assert_eq!(config.upstream_concurrency_retry_attempts, 20);
    assert_eq!(config.upstream_concurrency_retry_backoff_ms, 50);
    assert_eq!(config.upstream_concurrency_retry_max_wait_seconds, 10);
    assert_eq!(
        config.upstream_concurrency_retry_exclusive_wait_multiplier,
        2
    );
}

#[test]
fn deployment_templates_expose_configurable_stream_keepalive_and_hard_timeout_settings() {
    let env_example = fs::read_to_string(".env.example").unwrap();
    let compose = fs::read_to_string("docker-compose.yml").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();

    for marker in [
        "UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS",
        "UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS",
        "UPSTREAM_STREAM_MAX_DURATION_SECONDS",
        "MODEL_PROBE_REFRESH_INTERVAL_SECONDS",
        "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
    ] {
        assert!(
            env_example.contains(marker),
            ".env.example should expose {marker}"
        );
        assert!(
            compose.contains(marker),
            "docker-compose.yml should expose {marker}"
        );
        assert!(
            deployment.contains(marker),
            "DEPLOYMENT.md should document {marker}"
        );
    }

    for marker in [
        "UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS",
        "UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS",
        "UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS",
        "UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER",
        "MODEL_PROBE_REFRESH_INTERVAL_SECONDS",
        "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
    ] {
        assert!(
            deployment.contains(marker),
            "DEPLOYMENT.md should document {marker}"
        );
    }
}

#[test]
fn codex_docs_mention_the_copy_ready_relative_catalog_path() {
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();
    let guide = fs::read_to_string("docs/codex-integration-guide.md").unwrap();
    let contributing = fs::read_to_string("CONTRIBUTING.md").unwrap();

    assert!(readme.contains("/portal/integration"));
    assert!(readme.contains("model_catalog_json"));
    assert!(deployment.contains(r#"model_catalog_json = "model-catalog.json""#));
    assert!(guide.contains(r#"model_catalog_json = "model-catalog.json""#));
    assert!(!readme.contains("Gitee"));
    assert!(!deployment.contains("Gitee"));
    assert!(!contributing.contains("Gitee"));
}
