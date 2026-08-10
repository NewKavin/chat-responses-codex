use chat_responses_codex::capabilities::{CapabilityConfiguration, ReasoningMode};
use chat_responses_codex::state::{
    AppConfig, IMMEDIATE_RUNTIME_SETTING_FIELDS, RESTART_RUNTIME_SETTING_FIELDS,
};
use serde_json::Value;
use std::collections::BTreeSet;
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
    assert!(Path::new("templates/codex/agents/default.toml.example").exists());
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
    assert!(config.contains("stream_max_retries = 2"));
    assert!(
        config.find(r#"web_search = "disabled""#).unwrap() < config.find("[features]").unwrap(),
        "web_search is a top-level Codex setting, not a model-provider field"
    );
    assert!(!config.contains("disable_response_storage"));
    assert!(!config
        .contains("/absolute/path/to/chat-responses-codex/templates/codex/model-catalog.json"));
}

#[test]
fn codex_default_agent_example_uses_live_selection_placeholders() {
    let role = fs::read_to_string("templates/codex/agents/default.toml.example").unwrap();

    assert!(role.contains(r#"model = "<model_slug>""#));
    assert!(role.contains(r#"model_reasoning_effort = "<reasoning_effort_from_live_catalog>""#));
    assert!(role.contains("developer_instructions ="));
    assert!(!role.contains("file and line references"));
    assert!(!role.contains("gpt-5.6-sol"));
    assert!(!role.contains("model_reasoning_effort = \"low\""));
}

#[test]
fn codex_template_uses_the_internal_long_stream_profile() {
    let config = fs::read_to_string("templates/codex/config.toml.example").unwrap();
    assert!(config.contains("stream_idle_timeout_ms = 3600000"));
    assert!(config.contains("stream_max_retries = 2"));
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
    assert_eq!(config.upstream_user_agent, "codex/0.144.6");
    assert!(!config.automatic_capability_probes_enabled);
    assert_eq!(config.usage_log_retention_days, 14);
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
fn app_config_defaults_upstream_route_retry_policy() {
    let config = AppConfig::default();

    assert!(config.upstream_route_exhaustion_retry_enabled);
    assert_eq!(config.upstream_route_exhaustion_retry_max_wait_ms, 10_000);
    assert_eq!(config.upstream_route_exhaustion_retry_max_rounds, 3);
}

#[test]
fn compose_omits_legacy_stream_and_probe_fallbacks() {
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
        assert!(!env_example.contains(marker), ".env should omit {marker}");
        assert!(
            !compose.contains(marker),
            "docker-compose.yml should not advertise {marker}"
        );
        assert!(
            !deployment.contains(marker),
            "DEPLOYMENT.md should point operators to Admin Settings instead of {marker}"
        );
    }
}

#[test]
fn deployment_templates_expose_optional_redis_runtime_coordination() {
    let env_example = fs::read_to_string(".env.example").unwrap();
    let compose = fs::read_to_string("docker-compose.yml").unwrap();
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();

    for marker in ["REDIS_ENABLED", "REDIS_URL", "REDIS_KEY_PREFIX"] {
        for (path, contents) in [
            (".env.example", env_example.as_str()),
            ("docker-compose.yml", compose.as_str()),
            ("README.md", readme.as_str()),
            ("DEPLOYMENT.md", deployment.as_str()),
        ] {
            assert!(contents.contains(marker), "{path} should expose {marker}");
        }
    }

    for contents in [readme.as_str(), deployment.as_str()] {
        assert!(contents.contains("docker compose up -d"));
        assert!(contents.contains("docker compose --profile redis up -d"));
        assert!(contents.contains("starts Redis and one gateway"));
        assert!(contents.contains("fail fast"));
        assert!(contents.contains("fail closed"));
        assert!(contents.contains("Redis does not replace PostgreSQL"));
    }
}

#[test]
fn runtime_settings_precedence_is_documented() {
    const CONTRACT: &str = "Saved values from Admin > Settings override legacy behavior environment variables. Existing variables are used only until the first settings save. Bootstrap connections and credentials remain environment-only.";

    let documents = [
        ("README.md", fs::read_to_string("README.md").unwrap()),
        (
            "DEPLOYMENT.md",
            fs::read_to_string("DEPLOYMENT.md").unwrap(),
        ),
    ];

    for (path, contents) in &documents {
        let normalized = contents.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains(CONTRACT),
            "{path} should state the runtime-settings precedence contract"
        );

        for field in IMMEDIATE_RUNTIME_SETTING_FIELDS
            .iter()
            .chain(RESTART_RUNTIME_SETTING_FIELDS)
        {
            let key = field.to_ascii_uppercase();
            assert!(
                !contents.contains(&key),
                "{path} should direct managed setting {key} to Admin Settings"
            );
        }
    }
}

#[test]
fn route_exhaustion_retry_moves_to_admin_settings_with_compose_fallback() {
    let env_example = fs::read_to_string(".env.example").unwrap();
    let compose = fs::read_to_string("docker-compose.yml").unwrap();
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();

    for marker in [
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED",
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS",
        "UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS",
    ] {
        assert!(!env_example.contains(marker), ".env should omit {marker}");
        assert!(
            !compose.contains(marker),
            "Compose should not advertise {marker}"
        );
        assert!(!readme.contains(marker), "README should omit {marker}");
        assert!(
            !deployment.contains(marker),
            "DEPLOYMENT should omit {marker}"
        );
    }

    assert!(readme.contains("Admin > Settings"));
    assert!(deployment.contains("Admin > Settings"));
}

#[test]
fn transient_route_retry_moves_to_admin_settings_with_compose_fallback() {
    let env_example = fs::read_to_string(".env.example").unwrap();
    let compose = fs::read_to_string("docker-compose.yml").unwrap();
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();
    let deployment_words = deployment.split_whitespace().collect::<Vec<_>>().join(" ");

    for marker in [
        "UPSTREAM_SAME_ROUTE_RETRY_ENABLED",
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS",
        "UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS",
    ] {
        assert!(!env_example.contains(marker), ".env should omit {marker}");
        assert!(
            !compose.contains(marker),
            "Compose should not advertise {marker}"
        );
        assert!(!readme.contains(marker), "README should omit {marker}");
        assert!(
            !deployment.contains(marker),
            "DEPLOYMENT should omit {marker}"
        );
    }

    for contract in [
        "disable the fixed same-route retry",
        "3-second base",
        "60-second cooldown cap",
        "Codex owns SSE interruption retries",
        "Key and upstream fallback remains available",
    ] {
        assert!(
            deployment_words.contains(contract),
            "DEPLOYMENT.md should document `{contract}`"
        );
    }
}

#[test]
fn route_exhaustion_docs_preserve_retry_and_replay_safety_contract() {
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();

    for (path, contents) in [
        ("README.md", readme.as_str()),
        ("DEPLOYMENT.md", deployment.as_str()),
    ] {
        let normalized = contents.split_whitespace().collect::<Vec<_>>().join(" ");
        for contract in [
            "zero disables waiting",
            "total rounds include the initial round",
            "full `Retry-After`",
            "priority cannot make an unhealthy route eligible",
            "output or tool calls are never replayed after delivery",
        ] {
            assert!(
                normalized.contains(contract),
                "{path} should document `{contract}`"
            );
        }
    }

    let deployment_words = deployment.split_whitespace().collect::<Vec<_>>().join(" ");
    for profile in [
        "2-second delay",
        "two extra attempts",
        "at most three admitted attempts",
        "concurrency and quota admission",
        "disable route-exhaustion retry",
    ] {
        assert!(
            deployment_words.contains(profile),
            "DEPLOYMENT.md should document `{profile}`"
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
    let default_agent = fs::read_to_string("templates/codex/agents/default.toml.example").unwrap();
    let readme = fs::read_to_string("README.md").unwrap();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();
    let guide = fs::read_to_string("docs/codex-integration-guide.md").unwrap();

    for marker in [
        "format=codex",
        "cli_auth_credentials_store = \"file\"",
        "multi_agent = true",
        "multi_agent_v2 = false",
        "[agents]",
        "max_threads = 4",
        "max_depth = 2",
        "stream_max_retries = 2",
        "effective_context_window_percent = 80",
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

    assert!(!codex.contains("client_version=0.144.6"));
    assert!(!guide.contains("client_version=0.144.6"));
    assert!(guide.contains("read -rsp 'Gateway downstream key: '"));
    assert!(guide.contains("client_version"));
    assert!(guide.contains("multi_agent_version"));
    assert!(guide.contains("multi_agent_v2"));
    assert!(guide.contains("V1"));
    assert!(codex.contains("model_reasoning_effort = \"<reasoning_effort_from_live_catalog>\""));
    assert!(
        default_agent.contains("model_reasoning_effort = \"<reasoning_effort_from_live_catalog>\"")
    );

    for documentation in [readme, deployment, guide] {
        assert!(documentation.contains("codex --strict-config doctor --summary"));
        assert!(documentation.contains("max_threads"));
        assert!(documentation.contains("max_depth"));
    }
}

#[test]
fn deployment_docs_cover_the_default_codex_agent_profile() {
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();
    let readme = fs::read_to_string("README.md").unwrap();
    let guide = fs::read_to_string("docs/codex-integration-guide.md").unwrap();

    assert!(deployment.contains("~/.codex/agents/default.toml"));
    assert!(deployment.contains("model_reasoning_effort"));
    assert!(deployment.contains("same live catalog"));
    assert!(deployment.contains("codex login --with-api-key"));
    assert!(deployment.contains("Codex CLI `0.146.0`"));
    assert!(readme.contains("Codex CLI `0.146.0`"));
    assert!(!readme.contains("0.144.4"));
    assert!(guide.contains(
        "[codex-agent-default.toml.example](../templates/codex/agents/default.toml.example)"
    ));
    assert!(guide.contains("~/.codex/agents/default.toml"));
    assert!(guide.contains("codex login --with-api-key"));
    assert!(guide.contains("## 这四个地方分别在哪改"));
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
fn deployment_context_limits_are_conservative_until_qualified() {
    let configuration = deployment_capabilities();
    let deployment = fs::read_to_string("DEPLOYMENT.md").unwrap();
    let deployment_words = deployment.split_whitespace().collect::<Vec<_>>().join(" ");
    let context_window = |policy_id: &str| {
        configuration
            .policies
            .iter()
            .find(|policy| policy.id == policy_id)
            .unwrap_or_else(|| panic!("missing policy {policy_id}"))
            .semantic
            .context_window
            .unwrap_or_else(|| panic!("missing context window for {policy_id}"))
    };

    assert_eq!(context_window("glm-5.2"), 131_072);
    assert_eq!(context_window("deepseek-v4-flash"), 131_072);
    assert!(context_window("glm-5.2") < 1_000_000);
    assert!(context_window("deepseek-v4-flash") < 142_000);
    for requirement in [
        "32k, 64k, 128k, and the configured maximum",
        "text, reasoning, and read-only tool",
        "three consecutive times",
        "largest passing tier",
        "A failed 32k tier blocks model qualification",
        "Normal traffic never auto-learns a higher context limit",
    ] {
        assert!(
            deployment_words.contains(requirement),
            "deployment context qualification rule is missing: {requirement}"
        );
    }
}

#[test]
fn deployment_policies_cover_domestic_reasoning_families_with_verified_efforts_only() {
    use chat_responses_codex::capabilities::{RouteIdentity, WireProtocol};

    let configuration = deployment_capabilities();
    let compiled = configuration
        .clone()
        .compile()
        .expect("deployment capability template must compile");
    for policy_id in [
        "domestic-glm-5-family",
        "domestic-deepseek-family",
        "domestic-kimi-family",
        "domestic-qwen-family",
        "domestic-minimax-family",
    ] {
        let policy = configuration
            .policies
            .iter()
            .find(|policy| policy.id == policy_id)
            .unwrap_or_else(|| panic!("missing policy {policy_id}"));
        assert_eq!(
            policy.priority, 1,
            "domestic family policies must share a rank so future conflicts fail compilation"
        );
    }

    // Real-world probe results: the self-hosted GLM-5 and DeepSeek families
    // accept all five Codex effort levels, while Kimi / Qwen / MiniMax accept
    // only low/medium/high. The catalog must reflect each family's actual
    // supported set so Codex never offers an unselectable level.
    let five_level_models = [
        "glm-5.2",
        "deepseek-v4-flash",
        "deepseek-ai/DeepSeek-R1-0528",
    ];
    let three_level_models = ["kimi-k2.6", "qwen3.7-plus", "MiniMax-M2.7"];
    let expected_efforts = BTreeSet::from([
        "low".to_owned(),
        "medium".to_owned(),
        "high".to_owned(),
        "xhigh".to_owned(),
        "max".to_owned(),
    ]);
    let expected_controls = vec![
        "low".to_owned(),
        "medium".to_owned(),
        "high".to_owned(),
        "xhigh".to_owned(),
        "max".to_owned(),
    ];

    for runtime_model_slug in five_level_models.into_iter().chain(three_level_models) {
        let route = RouteIdentity {
            key_fingerprint: String::new(),
            upstream_id: "deployment-upstream".to_owned(),
            exposed_model_slug: runtime_model_slug.to_owned(),
            runtime_model_slug: runtime_model_slug.to_owned(),
            protocol: WireProtocol::ChatCompletions,
            tags: BTreeSet::new(),
        };
        let semantic = compiled.semantic_for(&route);
        let candidates = compiled.probe_candidates_for(&route);
        let is_five_level = five_level_models.contains(&runtime_model_slug);

        assert_eq!(
            semantic.reasoning_replay_required,
            Some(false),
            "{runtime_model_slug} must not require a protocol-specific replay carrier"
        );
        assert_eq!(
            semantic.effort_map.keys().cloned().collect::<BTreeSet<_>>(),
            if is_five_level {
                expected_efforts.clone()
            } else {
                BTreeSet::from(["low".to_owned(), "medium".to_owned(), "high".to_owned()])
            },
            "{runtime_model_slug} must publish only directly probed effort names"
        );
        assert_eq!(
            candidates.reasoning_controls.get("reasoning_effort"),
            Some(&if is_five_level {
                expected_controls.clone()
            } else {
                vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()]
            }),
            "{runtime_model_slug} must probe only upstream wire values"
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
