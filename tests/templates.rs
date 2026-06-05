use serde_json::Value;
use std::fs;
use std::path::Path;
use chat_responses_codex::state::AppConfig;

#[test]
fn template_files_live_under_templates_directory() {
    assert!(Path::new("templates/codex/config.toml.example").exists());
    assert!(Path::new("templates/codex/model-catalog.json").exists());
    assert!(Path::new("templates/state/gateway-state.example.json").exists());
}

#[test]
fn codex_model_catalog_preserves_upstream_model_slugs_exactly() {
    let catalog: Value = serde_json::from_str(
        &fs::read_to_string("templates/codex/model-catalog.json").unwrap(),
    )
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
}

#[test]
fn codex_config_example_uses_live_model_slug_exactly() {
    let config = fs::read_to_string("templates/codex/config.toml.example").unwrap();

    assert!(config.contains(r#"model = "ZhipuAI/GLM-5""#));
    assert!(config.contains(r#"review_model = "ZhipuAI/GLM-5""#));
    assert!(!config.contains(r#"model = "glm-5""#));
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
fn app_config_defaults_stream_idle_timeout_to_thirty_minutes() {
    let config = AppConfig::default();

    assert_eq!(
        config.upstream_stream_idle_timeout_seconds,
        1_800,
        "the default stream idle timeout should give long SSE requests more headroom"
    );
}
