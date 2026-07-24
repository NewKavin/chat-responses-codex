use chat_responses_codex::capabilities::{CapabilityConfiguration, ReasoningMode};
use chat_responses_codex::state::AppConfig;
use serde_json::Value;
use std::fs;
use std::path::Path;

fn deployment_capabilities() -> CapabilityConfiguration {
    serde_json::from_str(
        &fs::read_to_string("templates/capabilities/current-deployment.example.json").unwrap(),
    )
    .expect("deployment template must deserialize through the public capability schema")
}

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
    assert!(
        models.is_empty(),
        "template catalog should be an empty scaffold"
    );
}

#[test]
fn codex_config_example_uses_live_model_slug_exactly() {
    let config = fs::read_to_string("templates/codex/config.toml.example").unwrap();

    assert!(config.contains(r#"model = "<model_slug>""#));
    assert!(config.contains(r#"review_model = "<model_slug>""#));
    assert!(config.contains(r#"model_catalog_json = "model-catalog.json""#));
    assert!(config.contains(r#"web_search = "disabled""#));
    assert!(config.contains("stream_max_retries = 8"));
    assert!(
        config.find(r#"web_search = "disabled""#).unwrap() < config.find("[features]").unwrap(),
        "web_search is a top-level Codex setting, not a model-provider field"
    );
    assert!(!config.contains("disable_response_storage"));
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

    assert_eq!(config.upstream_stream_keepalive_interval_seconds, 3);
    assert_eq!(config.upstream_stream_idle_timeout_seconds, 1_800);
    assert_eq!(config.upstream_stream_max_duration_seconds, 86_400);
    assert_eq!(config.model_probe_refresh_interval_seconds, 15);
    assert_eq!(config.upstream_model_key_sync_interval_seconds, 0);
    assert!(!config.upstream_model_auto_discovery_enabled);
    assert!(!config.automatic_capability_probes_enabled);
}

#[test]
fn deployment_surface_omits_obsolete_concurrency_retry_settings() {
    let files = [
        (".env.example", fs::read_to_string(".env.example").unwrap()),
        (
            "docker-compose.yml",
            fs::read_to_string("docker-compose.yml").unwrap(),
        ),
        (
            "DEPLOYMENT.md",
            fs::read_to_string("DEPLOYMENT.md").unwrap(),
        ),
        (
            "docs/codex-integration-guide.md",
            fs::read_to_string("docs/codex-integration-guide.md").unwrap(),
        ),
    ];

    for marker in [
        "UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS",
        "UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS",
        "UPSTREAM_CONCURRENCY_RETRY_MAX_WAIT_SECONDS",
        "UPSTREAM_CONCURRENCY_RETRY_EXCLUSIVE_WAIT_MULTIPLIER",
    ] {
        for (path, contents) in &files {
            assert!(
                !contents.contains(marker),
                "{path} should not expose obsolete setting {marker}"
            );
        }
    }
}

#[test]
fn app_config_defaults_upstream_hedge_policy() {
    let config = AppConfig::default();

    assert!(config.upstream_hedge_enabled);
    assert_eq!(config.upstream_hedge_delay_ms, 12_000);
    assert_eq!(config.upstream_hedge_interval_ms, 12_000);
    assert_eq!(config.upstream_hedge_max_extra_attempts, 1);
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
        "AUTOMATIC_CAPABILITY_PROBES_ENABLED",
        "UPSTREAM_HEDGE_ENABLED",
        "UPSTREAM_HEDGE_DELAY_MS",
        "UPSTREAM_HEDGE_INTERVAL_MS",
        "UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS",
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
fn deployment_docs_explain_multi_key_route_resilience_contract() {
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();

    for (name, documentation) in [("README.md", readme), ("DEPLOYMENT.md", deployment)] {
        for marker in [
            "authoritative empty mapping",
            "persisted model catalog",
            "same exact route once",
            "without sleeping inside the request",
            "full `Retry-After`",
            "503 `upstream_routes_exhausted`",
            "502 `upstream_credentials_exhausted`",
            "502 `upstream_model_unsupported`",
            "400 `capability_not_supported`",
            "502 `upstream_protocol_unsupported`",
            "same idempotency identifier",
            "at-least-once",
            "runtime route health resets on restart",
            "does not change the persisted model catalog",
        ] {
            assert!(
                documentation.contains(marker),
                "{name} should document `{marker}`"
            );
        }
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
    assert!(guide.contains("白名单中的全部模型"));
    assert!(guide.contains("替换完整的 `model-catalog.json`"));
    assert!(guide.contains("不要复制其他模型条目"));
    assert!(guide.contains("不需要配置 `upstream_id`"));
    assert!(guide.contains("指纹是网关内部状态"));
    assert!(guide.contains("新建 Codex 会话"));
    assert!(!readme.contains("Gitee"));
    assert!(!deployment.contains("Gitee"));
    assert!(!contributing.contains("Gitee"));
}

#[test]
fn codex_integration_examples_document_multi_agent_validation() {
    let codex = fs::read_to_string("templates/codex/config.toml.example").unwrap();
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();
    let guide = fs::read_to_string("docs/codex-integration-guide.md").unwrap();

    for marker in [
        "client_version=0.144.6",
        "cli_auth_credentials_store = \"file\"",
        "multi_agent = true",
        "[agents]",
        "max_threads = 8",
        "max_depth = 3",
    ] {
        assert!(
            codex.contains(marker),
            "Codex template should contain {marker}"
        );
        assert!(
            guide.contains(marker),
            "Codex guide should contain {marker}"
        );
    }

    for documentation in [readme, deployment, guide] {
        assert!(documentation.contains("codex --strict-config doctor --summary"));
        assert!(documentation.contains("max_threads"));
        assert!(documentation.contains("max_depth"));
    }
}

#[test]
fn deployment_capabilities_are_external_versioned_and_model_agnostic_in_code() {
    let configuration = deployment_capabilities();
    assert_eq!(configuration.schema_version, 1);
    assert!(configuration.route_overrides.is_empty());
    for bundle_id in ["agent_core", "reasoning_agent", "image_agent"] {
        assert!(
            configuration
                .bundles
                .iter()
                .any(|bundle| bundle.id == bundle_id),
            "missing bundle {bundle_id}"
        );
    }
    assert!(configuration.compatibility_expectations.len() >= 6);
    configuration
        .compile()
        .expect("deployment template must compile through the runtime policy compiler");
}

#[test]
fn deployment_policies_externalize_semantics_and_probe_candidates() {
    let configuration = deployment_capabilities();
    for policy_id in [
        "glm-5.2",
        "deepseek-v4-flash",
        "minimax-m2.5",
        "minimax-m2.7",
        "kimi-k2.5",
        "kimi-k2.6",
    ] {
        let policy = configuration
            .policies
            .iter()
            .find(|policy| policy.id == policy_id)
            .unwrap_or_else(|| panic!("missing policy {policy_id}"));
        assert_eq!(
            policy.semantic.reasoning_mode,
            Some(ReasoningMode::Optional)
        );
        assert!(policy.semantic.context_window.is_some(), "{policy_id}");
        assert!(policy.semantic.max_output_tokens.is_some(), "{policy_id}");
        assert!(!policy.evidence.is_empty(), "{policy_id}");
        assert!(
            !policy.probe_candidates.token_limit_fields.is_empty(),
            "{policy_id}"
        );
        assert!(
            !policy.probe_candidates.reasoning_controls.is_empty(),
            "{policy_id}"
        );
        assert!(!policy.extension_probes.is_empty(), "{policy_id}");
    }

    let deepseek = configuration
        .policies
        .iter()
        .find(|policy| policy.id == "deepseek-v4-flash")
        .unwrap();
    for field in [
        "temperature",
        "top_p",
        "presence_penalty",
        "frequency_penalty",
        "logprobs",
        "top_logprobs",
    ] {
        assert!(
            deepseek.semantic.omit_sampling_fields.contains(field),
            "deepseek policy must externalize omission of {field}"
        );
    }
}

#[test]
fn reasoning_expectations_require_replay_only_when_policy_requires_it() {
    let configuration = deployment_capabilities();
    for (policy_id, expectation_id) in [
        ("glm-5.2", "glm-5.2-core"),
        ("deepseek-v4-flash", "deepseek-v4-flash-core"),
        ("minimax-m2.5", "minimax-m2.5-core"),
        ("minimax-m2.7", "minimax-m2.7-core"),
        ("kimi-k2.5", "kimi-k2.5-core"),
        ("kimi-k2.6", "kimi-k2.6-core"),
    ] {
        let policy = configuration
            .policies
            .iter()
            .find(|policy| policy.id == policy_id)
            .unwrap();
        let expectation = configuration
            .compatibility_expectations
            .iter()
            .find(|expectation| expectation.id == expectation_id)
            .unwrap();
        assert_eq!(
            expectation.bundles.contains("reasoning_agent"),
            policy.semantic.reasoning_replay_required == Some(true),
            "{expectation_id} must match {policy_id} replay requirements"
        );
    }
}

#[test]
fn all_client_templates_use_only_gateway_url_key_and_exposed_slug() {
    let codex = std::fs::read_to_string("templates/codex/config.toml.example").unwrap();
    assert!(codex.contains("web_search = \"disabled\""));
    for path in [
        "templates/opencode/opencode.json",
        "templates/claude-code/settings.json",
        "templates/hermes/config.yaml",
    ] {
        let body = std::fs::read_to_string(path).unwrap();
        assert!(!body.contains("api.deepseek.com"));
        assert!(!body.contains("api.minimax.io"));
        assert!(!body.contains("api.moonshot.cn"));
    }
}

#[test]
fn all_client_templates_use_gateway_placeholders_without_hardcoded_hosts() {
    let codex = fs::read_to_string("templates/codex/config.toml.example").unwrap();
    assert!(codex.contains("base_url = \"<gateway_url>/v1\""));
    assert!(codex.contains("model = \"<model_slug>\""));
    assert!(codex.contains("web_search = \"disabled\""));

    let opencode = fs::read_to_string("templates/opencode/opencode.json").unwrap();
    assert!(opencode.contains("https://<gateway_url>/v1"));
    assert!(opencode.contains("<downstream_key>"));
    assert!(opencode.contains("<model_slug>"));

    let claude = fs::read_to_string("templates/claude-code/settings.json").unwrap();
    assert!(claude.contains("https://<gateway_url>"));
    assert!(claude.contains("<downstream_key>"));
    assert!(claude.contains("<model_slug>"));

    let hermes = fs::read_to_string("templates/hermes/config.yaml").unwrap();
    assert!(hermes.contains("https://<gateway_url>/v1"));
    assert!(hermes.contains("<model_slug>"));
    assert!(hermes.contains("${CHAT2RESPONSES_KEY}"));

    for template in [codex, opencode, claude, hermes] {
        assert!(!template.contains("gateway-host:3001"));
        assert!(!template.contains("gateway.example"));
    }
}

#[test]
fn opencode_template_denies_unlisted_permissions_by_default() {
    let opencode: Value =
        serde_json::from_str(&fs::read_to_string("templates/opencode/opencode.json").unwrap())
            .expect("OpenCode template must be valid JSON");

    assert_eq!(
        opencode["permission"],
        serde_json::json!({"*": "deny", "read": "allow"})
    );
}
