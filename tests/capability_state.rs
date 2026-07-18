use chat_responses_codex::capabilities::*;
use chat_responses_codex::server::{probe_plan_for_job, CoreProbeCase};
use chat_responses_codex::state::{AppConfig, AppState, FreekeySyncItem, PersistedState};
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::mpsc;

fn default_plan_configuration() -> Arc<CompiledCapabilityConfiguration> {
    Arc::new(CapabilityConfiguration::default().compile().unwrap())
}

fn single_probe_job(batch: ProbeJobBatch) -> ProbeJob {
    let mut jobs = batch.into_jobs();
    assert_eq!(jobs.len(), 1, "expected a single-job probe batch");
    jobs.remove(0)
}

fn blocker_probe_batch() -> ProbeJobBatch {
    ProbeJobBatch::single(ProbeJob {
        key: DialectProfileKey {
            upstream_id: "blocker-upstream".into(),
            runtime_model_slug: "Lab/Blocker".into(),
            protocol: WireProtocol::ChatCompletions,
        },
        exposed_model_slugs: std::collections::BTreeSet::from(["Lab/Blocker".into()]),
        reason: ProbeReason::Manual,
        configuration: chat_responses_codex::capabilities::ProbeConfigurationBinding {
            configuration_fingerprint: "test-fingerprint".into(),
            configuration_digest: "test-digest".into(),
            configuration_schema_version: 1,
            configuration_revision: 1,
            probe_schema_version: chat_responses_codex::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
        },
        plan_configuration: default_plan_configuration(),
    })
}

#[tokio::test]
async fn file_backend_keeps_capabilities_out_of_main_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("gateway-state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    let config = CapabilityConfiguration {
        revision: 7,
        ..Default::default()
    };
    state
        .replace_capability_configuration(config)
        .await
        .unwrap();
    let main = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".into());
    assert!(!main.contains("compatibility_expectations"));
    let sidecar =
        tokio::fs::read_to_string(dir.path().join("gateway-state.json.capabilities.json"))
            .await
            .unwrap();
    assert!(sidecar.contains("\"revision\": 7"));
}

#[tokio::test]
async fn file_startup_migrates_legacy_sensitive_capability_urls_before_compile() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("gateway-state.json");
    let sidecar_path = dir.path().join("gateway-state.json.capabilities.json");
    let legacy_secret = "legacy-credential-do-not-export";
    let document = CapabilityStateDocument {
        configuration: CapabilityConfiguration {
            policies: vec![CapabilityPolicy {
                id: "legacy-url-policy".into(),
                evidence: vec![EvidenceReference {
                    title: "legacy evidence".into(),
                    url: format!("https://evidence-user:{legacy_secret}@evidence.invalid/source"),
                    retrieved_at: "2026-01-01".into(),
                    version: None,
                }],
                extension_probes: vec![DeclarativeProbeCase {
                    id: "legacy-callback".into(),
                    protocol: WireProtocol::ChatCompletions,
                    prerequisites: Default::default(),
                    request_patch: json!({
                        "callback": format!("https://callback.invalid/result?token={legacy_secret}")
                    }),
                    response_predicate: ResponsePredicate {
                        path: "/accepted".into(),
                        operator: PredicateOperator::Exists,
                        value: None,
                    },
                }],
                ..CapabilityPolicy::default()
            }],
            probe: ProbeConfiguration {
                https_image_fixture: Some(HttpsImageFixture {
                    url: format!("https://fixture.invalid/image.png?credential={legacy_secret}"),
                    expected_label: "fixture".into(),
                }),
                ..ProbeConfiguration::default()
            },
            ..CapabilityConfiguration::default()
        },
        profiles: Default::default(),
    };
    tokio::fs::write(&sidecar_path, serde_json::to_vec_pretty(&document).unwrap())
        .await
        .unwrap();

    let loaded = AppState::load_from_path(&path, AppConfig::default())
        .await
        .expect("trusted legacy capability document should migrate before compile");
    let runtime =
        serde_json::to_string(loaded.capability_snapshot().configuration.source()).unwrap();
    assert!(!runtime.contains(legacy_secret));
    assert!(runtime.contains("https://redacted.invalid/"));
    loaded
        .capability_snapshot()
        .configuration
        .source()
        .compile()
        .unwrap();

    let persisted = tokio::fs::read_to_string(&sidecar_path).await.unwrap();
    assert!(!persisted.contains(legacy_secret));
    assert!(persisted.contains("https://redacted.invalid/"));
    serde_json::from_str::<CapabilityStateDocument>(&persisted)
        .unwrap()
        .configuration
        .compile()
        .unwrap();
}

#[tokio::test]
async fn file_startup_legacy_url_migration_fails_closed_without_leaking_secrets() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("gateway-state.json");
    let sidecar_path = dir.path().join("gateway-state.json.capabilities.json");
    let legacy_secret = "legacy-invalid-secret";
    let document = CapabilityStateDocument {
        configuration: CapabilityConfiguration {
            probe: ProbeConfiguration {
                https_image_fixture: Some(HttpsImageFixture {
                    url: format!("https://fixture.invalid/image.png?token={legacy_secret}"),
                    expected_label: " ".into(),
                }),
                ..ProbeConfiguration::default()
            },
            ..CapabilityConfiguration::default()
        },
        profiles: Default::default(),
    };
    tokio::fs::write(&sidecar_path, serde_json::to_vec_pretty(&document).unwrap())
        .await
        .unwrap();

    let error = match AppState::load_from_path(&path, AppConfig::default()).await {
        Ok(_) => panic!("invalid migrated configuration must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(!error.to_string().contains(legacy_secret));
    assert!(tokio::fs::read_to_string(sidecar_path)
        .await
        .unwrap()
        .contains(legacy_secret));
}

#[tokio::test]
async fn invalid_reload_retains_last_valid_snapshot() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let good = CapabilityConfiguration {
        revision: 11,
        ..Default::default()
    };
    state.replace_capability_configuration(good).await.unwrap();
    let bad = CapabilityConfiguration {
        schema_version: 999,
        ..Default::default()
    };
    assert!(state.replace_capability_configuration(bad).await.is_err());
    assert_eq!(
        state.capability_snapshot().configuration.source().revision,
        11
    );
}

#[tokio::test]
async fn disabled_capability_probe_configuration_rejects_jobs_and_reconciliation() {
    let dir = tempdir().unwrap();
    let upstream = learning_upstream("up-disabled-probe", "Lab/Disabled");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..PersistedState::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    state
        .replace_capability_configuration(CapabilityConfiguration {
            probe: ProbeConfiguration {
                enabled: false,
                ..ProbeConfiguration::default()
            },
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(1);
    state.set_capability_probe_sender(sender);

    assert!(state
        .build_capability_probe_job(
            &upstream.id,
            "Lab/Disabled",
            "Lab/Disabled",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            ProbeReason::Manual,
        )
        .await
        .unwrap()
        .is_none());
    assert!(state
        .reconcile_dialect_profiles(u64::MAX)
        .await
        .unwrap()
        .is_empty());
    assert!(!state.queue_capability_probe(single_probe_job(blocker_probe_batch())));
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn probe_job_rejects_same_revision_when_immutable_configuration_changes() {
    let dir = tempdir().unwrap();
    let upstream = learning_upstream("up-binding", "Lab/Binding");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..PersistedState::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 7,
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();
    let job = state
        .build_capability_probe_job(
            &upstream.id,
            "Lab/Binding",
            "Lab/Binding",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            ProbeReason::Manual,
        )
        .await
        .unwrap()
        .unwrap();

    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 7,
            policies: vec![CapabilityPolicy {
                id: "changed-probe-plan".into(),
                priority: 1,
                selector: CapabilitySelector::default(),
                semantic: SemanticPolicy {
                    context_window: Some(65_536),
                    ..SemanticPolicy::default()
                },
                ..CapabilityPolicy::default()
            }],
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();

    assert!(!AppState::capability_probe_job_is_current(
        &state.capability_snapshot(),
        &upstream,
        &job,
    ));
}

#[tokio::test]
async fn queued_probe_plan_keeps_configuration_snapshot_after_import() {
    let dir = tempdir().unwrap();
    let upstream = learning_upstream("up-plan-snapshot", "Lab/Plan");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..PersistedState::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let extension_a = DeclarativeProbeCase {
        id: "configuration-a-extension".into(),
        protocol: WireProtocol::ChatCompletions,
        prerequisites: Default::default(),
        request_patch: json!({"configuration_a": true}),
        response_predicate: ResponsePredicate {
            path: "/accepted".into(),
            operator: PredicateOperator::Equals,
            value: Some(json!(true)),
        },
    };
    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 7,
            policies: vec![CapabilityPolicy {
                id: "configuration-a".into(),
                selector: CapabilitySelector::default(),
                probe_candidates: ProbeCandidates {
                    token_limit_fields: vec![TokenLimitField::MaxCompletionTokens],
                    reasoning_controls: std::collections::BTreeMap::from([(
                        "reasoning_effort".into(),
                        vec!["a-only".into()],
                    )]),
                    reasoning_carriers: Default::default(),
                },
                extension_probes: vec![extension_a.clone()],
                ..CapabilityPolicy::default()
            }],
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();
    let job = state
        .build_capability_probe_job(
            &upstream.id,
            "Lab/Plan",
            "Lab/Plan",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            ProbeReason::Manual,
        )
        .await
        .unwrap()
        .unwrap();

    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 8,
            policies: vec![CapabilityPolicy {
                id: "configuration-b".into(),
                selector: CapabilitySelector::default(),
                probe_candidates: ProbeCandidates {
                    token_limit_fields: vec![TokenLimitField::MaxOutputTokens],
                    reasoning_controls: std::collections::BTreeMap::from([(
                        "reasoning_effort".into(),
                        vec!["b-only".into()],
                    )]),
                    reasoning_carriers: Default::default(),
                },
                extension_probes: vec![DeclarativeProbeCase {
                    id: "configuration-b-extension".into(),
                    protocol: WireProtocol::ChatCompletions,
                    prerequisites: Default::default(),
                    request_patch: json!({"configuration_b": true}),
                    response_predicate: ResponsePredicate {
                        path: "/accepted".into(),
                        operator: PredicateOperator::Equals,
                        value: Some(json!(true)),
                    },
                }],
                ..CapabilityPolicy::default()
            }],
            ..CapabilityConfiguration::default()
        })
        .await
        .unwrap();

    let plan = probe_plan_for_job(&job);

    assert!(plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::TokenLimit {
            field: TokenLimitField::MaxCompletionTokens
        }
    )));
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::ReasoningControl { field, value }
            if field == "reasoning_effort" && value == "a-only"
    )));
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::Declarative(extension) if extension == &extension_a
    )));
    assert!(!plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::TokenLimit {
            field: TokenLimitField::MaxOutputTokens
        }
    )));
    assert!(!plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::ReasoningControl { field, value }
            if field == "reasoning_effort" && value == "b-only"
    )));
    assert!(!plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::Declarative(extension) if extension.id == "configuration-b-extension"
    )));
}

#[tokio::test]
async fn profile_round_trip_uses_exact_case_sensitive_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    let key = DialectProfileKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "Lab/Case-Sensitive".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    state
        .upsert_dialect_profile(UpstreamDialectProfile::unknown(key.clone()))
        .await
        .unwrap();
    let loaded = AppState::load_from_path(&path, AppConfig::default())
        .await
        .unwrap();
    assert!(loaded.capability_snapshot().profiles.contains_key(&key));
    assert!(!loaded
        .capability_snapshot()
        .profiles
        .keys()
        .any(|candidate| candidate.runtime_model_slug == "lab/case-sensitive"));
}

fn stream_only_profile(key: DialectProfileKey, fingerprint: &str) -> UpstreamDialectProfile {
    let mut profile = UpstreamDialectProfile::unknown(key);
    profile.configuration_fingerprint = fingerprint.into();
    profile
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Supported);
    profile
}

fn learning_upstream(id: &str, model: &str) -> chat_responses_codex::state::UpstreamConfig {
    chat_responses_codex::state::UpstreamConfig {
        id: id.into(),
        name: id.into(),
        base_url: format!("https://{id}.example/v1"),
        api_key: "secret".into(),
        protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
        supported_models: vec![model.into()],
        active: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn stream_only_learning_and_configuration_replace_do_not_lose_updates() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let upstream = learning_upstream("up-learning", "Lab/Atomic");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..Default::default()
        },
        &path,
        AppConfig::default(),
    );
    let key = DialectProfileKey {
        upstream_id: "up-learning".into(),
        runtime_model_slug: "Lab/Atomic".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            "Lab/Atomic",
            "Lab/Atomic",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    state
        .upsert_dialect_profile(stream_only_profile(key.clone(), &fingerprint))
        .await
        .unwrap();

    let replace = state.replace_capability_configuration(CapabilityConfiguration {
        revision: 91,
        ..Default::default()
    });
    let learn = state.learn_stream_only_route(&key, "Lab/Atomic", &fingerprint);
    let (replace, learn) = tokio::join!(replace, learn);
    replace.unwrap();
    assert!(learn.unwrap());

    let snapshot = state.capability_snapshot();
    assert_eq!(snapshot.configuration.source().revision, 91);
    let profile = snapshot.profiles.get(&key).unwrap();
    assert_eq!(
        profile.capabilities.get(&Capability::FunctionTools),
        Some(&EvidenceState::Supported)
    );
    assert_eq!(
        profile.capabilities.get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
    assert_eq!(
        profile.capabilities.get(&Capability::TextStream),
        Some(&EvidenceState::Supported)
    );
}

#[tokio::test]
async fn stream_only_learning_reloads_latest_sidecar_before_single_publish() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let upstream = learning_upstream("up-learning", "Lab/Target");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..Default::default()
        },
        &path,
        AppConfig::default(),
    );
    let target_key = DialectProfileKey {
        upstream_id: "up-learning".into(),
        runtime_model_slug: "Lab/Target".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            "Lab/Target",
            "Lab/Target",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    state
        .upsert_dialect_profile(stream_only_profile(target_key.clone(), &fingerprint))
        .await
        .unwrap();
    state.persist().await.unwrap();

    let external = AppState::load_from_path(&path, AppConfig::default())
        .await
        .unwrap();
    external
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 92,
            ..Default::default()
        })
        .await
        .unwrap();
    let unrelated_key = DialectProfileKey {
        upstream_id: "up-unrelated".into(),
        runtime_model_slug: "Lab/Unrelated".into(),
        protocol: WireProtocol::Responses,
    };
    let mut unrelated = UpstreamDialectProfile::unknown(unrelated_key.clone());
    unrelated
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    external.upsert_dialect_profile(unrelated).await.unwrap();

    assert!(state
        .learn_stream_only_route(&target_key, "Lab/Target", &fingerprint)
        .await
        .unwrap());

    let snapshot = state.capability_snapshot();
    assert_eq!(snapshot.configuration.source().revision, 92);
    assert!(snapshot.profiles.contains_key(&unrelated_key));
    let target = snapshot.profiles.get(&target_key).unwrap();
    assert_eq!(
        target.capabilities.get(&Capability::FunctionTools),
        Some(&EvidenceState::Supported)
    );
    assert_eq!(
        target.capabilities.get(&Capability::NonStreamingResponse),
        Some(&EvidenceState::Rejected)
    );
}

#[tokio::test]
async fn stream_only_learning_rejects_fingerprint_and_schema_mismatches() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let upstream = learning_upstream("up-learning", "Lab/Stale");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..Default::default()
        },
        &path,
        AppConfig::default(),
    );
    let key = DialectProfileKey {
        upstream_id: "up-learning".into(),
        runtime_model_slug: "Lab/Stale".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let current_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            "Lab/Stale",
            "Lab/Stale",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let mut profile = stream_only_profile(key.clone(), &current_fingerprint);
    state.upsert_dialect_profile(profile.clone()).await.unwrap();

    assert!(!state
        .learn_stream_only_route(&key, "Lab/Stale", "stale-fingerprint")
        .await
        .unwrap());
    profile.probe_schema_version = DIALECT_PROBE_SCHEMA_VERSION.saturating_sub(1);
    state.upsert_dialect_profile(profile).await.unwrap();
    assert!(!state
        .learn_stream_only_route(&key, "Lab/Stale", &current_fingerprint)
        .await
        .unwrap());

    let loaded = AppState::load_from_path(&path, AppConfig::default())
        .await
        .unwrap();
    let snapshot = loaded.capability_snapshot();
    let profile = snapshot.profiles.get(&key).unwrap();
    assert!(!profile
        .capabilities
        .contains_key(&Capability::NonStreamingResponse));
    assert!(!profile.capabilities.contains_key(&Capability::TextStream));
}

#[tokio::test]
async fn stream_only_learning_rejects_fingerprint_stale_against_latest_configuration() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let upstream = chat_responses_codex::state::UpstreamConfig {
        id: "up-learning".into(),
        name: "learning".into(),
        base_url: "https://learning.example/v1".into(),
        api_key: "secret".into(),
        protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
        supported_models: vec!["Lab/Changed".into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..Default::default()
        },
        &path,
        AppConfig::default(),
    );
    let key = DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "Lab/Changed".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let observed_fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            "Lab/Changed",
            "Lab/Changed",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    state
        .upsert_dialect_profile(stream_only_profile(key.clone(), &observed_fingerprint))
        .await
        .unwrap();

    state
        .replace_capability_configuration(CapabilityConfiguration {
            revision: 93,
            route_overrides: vec![RouteCapabilityOverride {
                id: "changed-route".into(),
                priority: 100,
                selector: CapabilitySelector {
                    upstream_id: Some(upstream.id.clone()),
                    exposed_model: Some("Lab/Changed".into()),
                    runtime_model: Some("Lab/Changed".into()),
                    protocol: Some(WireProtocol::ChatCompletions),
                    ..Default::default()
                },
                capabilities: std::collections::BTreeMap::from([(
                    Capability::UsageStream,
                    EvidenceState::Rejected,
                )]),
                ..Default::default()
            }],
            ..Default::default()
        })
        .await
        .unwrap();

    assert!(!state
        .learn_stream_only_route(&key, "Lab/Changed", &observed_fingerprint)
        .await
        .unwrap());
    let snapshot = state.capability_snapshot();
    let profile = snapshot.profiles.get(&key).unwrap();
    assert!(!profile
        .capabilities
        .contains_key(&Capability::NonStreamingResponse));
    assert!(!profile.capabilities.contains_key(&Capability::TextStream));
}

#[tokio::test]
async fn stream_only_learning_does_not_recreate_a_deleted_upstream_profile() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let upstream = chat_responses_codex::state::UpstreamConfig {
        id: "up-deleted".into(),
        name: "deleted".into(),
        base_url: "https://deleted.example/v1".into(),
        api_key: "secret".into(),
        protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
        supported_models: vec!["Lab/Deleted".into()],
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![upstream.clone()],
            ..Default::default()
        },
        &path,
        AppConfig::default(),
    );
    let key = DialectProfileKey {
        upstream_id: upstream.id.clone(),
        runtime_model_slug: "Lab/Deleted".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            "Lab/Deleted",
            "Lab/Deleted",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    state
        .upsert_dialect_profile(stream_only_profile(key.clone(), &fingerprint))
        .await
        .unwrap();

    assert!(state.remove_upstream(&upstream.id).await.unwrap());
    assert!(!state
        .learn_stream_only_route(&key, "Lab/Deleted", &fingerprint)
        .await
        .unwrap());
    assert!(!state.capability_snapshot().profiles.contains_key(&key));
}

#[tokio::test]
async fn removing_upstream_clears_capability_profiles_for_that_upstream() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-1".into(),
            name: "primary".into(),
            base_url: "https://upstream.example".into(),
            api_key: "secret".into(),
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let key = DialectProfileKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "Lab/Case-Sensitive".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    state
        .upsert_dialect_profile(UpstreamDialectProfile::unknown(key.clone()))
        .await
        .unwrap();

    assert!(state.remove_upstream("up-1").await.unwrap());

    let loaded = AppState::load_from_path(&path, AppConfig::default())
        .await
        .unwrap();
    assert!(!loaded.capability_snapshot().profiles.contains_key(&key));
}

#[tokio::test]
async fn inserting_upstream_queues_capability_probe_jobs_for_active_routes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);

    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-1".into(),
            name: "primary".into(),
            base_url: "https://upstream.example/v1".into(),
            api_key: "secret".into(),
            protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
            supported_models: vec!["Lab/Case-Sensitive".into()],
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let job = single_probe_job(
        tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(job.key.upstream_id, "up-1");
    assert_eq!(job.key.runtime_model_slug, "Lab/Case-Sensitive");
    assert_eq!(job.key.protocol, WireProtocol::ChatCompletions);
}

#[tokio::test]
async fn updating_upstream_queues_capability_probe_jobs_for_active_routes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);

    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-1".into(),
            name: "primary".into(),
            base_url: "https://upstream.example/v1".into(),
            api_key: "secret".into(),
            protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
            protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
            supported_models: vec!["Lab/Case-Sensitive".into()],
            active: false,
            ..Default::default()
        })
        .await
        .unwrap();

    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .ok()
        .and_then(|job| job);

    assert!(state
        .update_upstream(
            "up-1",
            chat_responses_codex::state::UpstreamConfig {
                id: "ignored".into(),
                name: "primary".into(),
                base_url: "https://upstream.example/v1".into(),
                api_key: "secret".into(),
                protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
                supported_models: vec!["Lab/Case-Sensitive".into()],
                active: true,
                ..Default::default()
            }
        )
        .await
        .unwrap());

    let job = single_probe_job(
        tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(job.key.upstream_id, "up-1");
    assert_eq!(job.key.runtime_model_slug, "Lab/Case-Sensitive");
    assert_eq!(job.key.protocol, WireProtocol::ChatCompletions);
}

#[tokio::test]
async fn inserting_upstream_does_not_wait_for_full_probe_queue() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let (sender, mut receiver) = mpsc::channel(1);
    sender.try_send(blocker_probe_batch()).unwrap();
    state.set_capability_probe_sender(sender);
    let insert_state = state.clone();
    let mut insert = tokio::spawn(async move {
        insert_state
            .insert_upstream(chat_responses_codex::state::UpstreamConfig {
                id: "up-insert".into(),
                name: "insert fixture".into(),
                base_url: "https://insert.example/v1".into(),
                api_key: "fixture-secret".into(),
                protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
                supported_models: vec!["Lab/Insert-One".into(), "Lab/Insert-Two".into()],
                active: true,
                ..Default::default()
            })
            .await
    });

    tokio::time::timeout(std::time::Duration::from_millis(250), &mut insert)
        .await
        .expect("configuration persistence must not wait for probe queue capacity")
        .unwrap()
        .unwrap();
    let blocker = receiver.try_recv().expect("the blocker batch should remain queued");
    assert_eq!(
        single_probe_job(blocker).key.upstream_id,
        "blocker-upstream"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), receiver.recv())
            .await
            .is_err(),
        "a full queue must not receive a partial probe batch"
    );
    assert!(state
        .snapshot()
        .await
        .upstreams
        .iter()
        .any(|upstream| upstream.id == "up-insert"));
}

#[tokio::test]
async fn updating_upstream_does_not_wait_for_full_probe_queue() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![chat_responses_codex::state::UpstreamConfig {
                id: "up-update".into(),
                name: "before update".into(),
                base_url: "https://update.example/v1".into(),
                api_key: "fixture-secret".into(),
                active: false,
                ..Default::default()
            }],
            ..Default::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let (sender, mut receiver) = mpsc::channel(1);
    sender.try_send(blocker_probe_batch()).unwrap();
    state.set_capability_probe_sender(sender);
    let update_state = state.clone();
    let mut update = tokio::spawn(async move {
        update_state
            .update_upstream(
                "up-update",
                chat_responses_codex::state::UpstreamConfig {
                    id: "ignored".into(),
                    name: "after update".into(),
                    base_url: "https://update.example/v1".into(),
                    api_key: "fixture-secret".into(),
                    protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                    protocols: vec![
                        chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                    ],
                    supported_models: vec!["Lab/Update-One".into(), "Lab/Update-Two".into()],
                    active: true,
                    ..Default::default()
                },
            )
            .await
    });

    tokio::time::timeout(std::time::Duration::from_millis(250), &mut update)
        .await
        .expect("configuration persistence must not wait for probe queue capacity")
        .unwrap()
        .unwrap();
    let blocker = receiver.try_recv().expect("the blocker batch should remain queued");
    assert_eq!(
        single_probe_job(blocker).key.upstream_id,
        "blocker-upstream"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), receiver.recv())
            .await
            .is_err(),
        "a full queue must not receive a partial probe batch"
    );
    assert_eq!(
        state
            .snapshot()
            .await
            .upstreams
            .into_iter()
            .find(|upstream| upstream.id == "up-update")
            .unwrap()
            .name,
        "after update"
    );
}

#[tokio::test]
async fn inserting_upstream_succeeds_without_probe_worker() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );

    state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-no-worker".into(),
            name: "no worker fixture".into(),
            base_url: "https://no-worker.example/v1".into(),
            api_key: "fixture-secret".into(),
            supported_models: vec!["Lab/Exact".into()],
            active: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(state
        .snapshot()
        .await
        .upstreams
        .iter()
        .any(|upstream| upstream.id == "up-no-worker"));
}

#[tokio::test]
async fn updating_upstream_succeeds_with_closed_probe_queue() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![chat_responses_codex::state::UpstreamConfig {
                id: "up-closed".into(),
                name: "before update".into(),
                base_url: "https://closed.example/v1".into(),
                api_key: "fixture-secret".into(),
                active: false,
                ..Default::default()
            }],
            ..Default::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    state.set_capability_probe_sender(sender);

    state
        .update_upstream(
            "up-closed",
            chat_responses_codex::state::UpstreamConfig {
                id: "ignored".into(),
                name: "after update".into(),
                base_url: "https://closed.example/v1".into(),
                api_key: "fixture-secret".into(),
                supported_models: vec!["Lab/Exact".into()],
                active: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let upstream = state
        .snapshot()
        .await
        .upstreams
        .into_iter()
        .find(|upstream| upstream.id == "up-closed")
        .unwrap();
    assert_eq!(upstream.name, "after update");
}

#[tokio::test]
async fn inserting_upstream_succeeds_without_waiting_on_full_probe_queue() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let (sender, _receiver) = mpsc::channel(1);
    sender.try_send(blocker_probe_batch()).unwrap();
    state.set_capability_probe_sender(sender);

    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        state.insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-timeout".into(),
            name: "timeout fixture".into(),
            base_url: "https://timeout.example/v1".into(),
            api_key: "fixture-secret".into(),
            protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
            supported_models: vec!["Lab/Timeout-One".into(), "Lab/Timeout-Two".into()],
            active: true,
            ..Default::default()
        }),
    )
    .await
    .expect("configuration persistence must not wait for probe queue capacity")
    .unwrap();
    assert!(state
        .snapshot()
        .await
        .upstreams
        .iter()
        .any(|upstream| upstream.id == "up-timeout"));
}

#[tokio::test]
async fn inserting_upstream_drops_full_probe_batch_without_partial_delivery() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let upstream = chat_responses_codex::state::UpstreamConfig {
        id: "up-atomic-timeout".into(),
        name: "atomic timeout fixture".into(),
        base_url: "https://atomic-timeout.example/v1".into(),
        api_key: "fixture-secret".into(),
        protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
        supported_models: vec!["Lab/Atomic-One".into(), "Lab/Atomic-Two".into()],
        active: true,
        ..Default::default()
    };
    let (sender, mut receiver) = mpsc::channel(1);
    sender.try_send(blocker_probe_batch()).unwrap();
    state.set_capability_probe_sender(sender);

    state.insert_upstream(upstream.clone()).await.unwrap();
    let blocker = receiver
        .try_recv()
        .expect("blocker batch should remain queued");
    assert_eq!(
        single_probe_job(blocker).key.upstream_id,
        "blocker-upstream"
    );
    assert!(
        receiver.try_recv().is_err(),
        "a failed submission must not expose a partial route batch"
    );
    let snapshot = state.snapshot().await;
    assert_eq!(
        snapshot
            .upstreams
            .iter()
            .filter(|candidate| candidate.id == upstream.id)
            .count(),
        1,
        "the full-queue insert must persist exactly one upstream"
    );
    assert_eq!(
        snapshot
            .upstreams
            .iter()
            .find(|candidate| candidate.id == upstream.id)
            .unwrap()
            .failure_count,
        0,
        "the insert must not mutate runtime health fields"
    );
}

#[tokio::test]
async fn inserting_same_upstream_id_with_different_configuration_is_rejected() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![chat_responses_codex::state::UpstreamConfig {
                id: "up-conflict".into(),
                name: "original".into(),
                base_url: "https://original.example/v1".into(),
                api_key: "fixture-secret".into(),
                supported_models: vec!["Lab/Exact".into()],
                active: true,
                failure_count: 3,
                ..Default::default()
            }],
            ..Default::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );

    let error = state
        .insert_upstream(chat_responses_codex::state::UpstreamConfig {
            id: "up-conflict".into(),
            name: "changed".into(),
            base_url: "https://changed.example/v1".into(),
            api_key: "fixture-secret".into(),
            supported_models: vec!["Lab/Exact".into()],
            active: true,
            ..Default::default()
        })
        .await
        .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.upstreams.len(), 1);
    assert_eq!(snapshot.upstreams[0].name, "original");
    assert_eq!(snapshot.upstreams[0].failure_count, 3);
}

#[tokio::test]
async fn updating_upstream_succeeds_after_probe_queue_closes() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![chat_responses_codex::state::UpstreamConfig {
                id: "up-atomic-close".into(),
                name: "before update".into(),
                base_url: "https://atomic-close.example/v1".into(),
                api_key: "fixture-secret".into(),
                active: false,
                ..Default::default()
            }],
            ..Default::default()
        },
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let updated = chat_responses_codex::state::UpstreamConfig {
        id: "ignored".into(),
        name: "after update".into(),
        base_url: "https://atomic-close.example/v1".into(),
        api_key: "fixture-secret".into(),
        protocols: vec![chat_responses_codex::routing::UpstreamProtocol::ChatCompletions],
        supported_models: vec!["Lab/Close-One".into(), "Lab/Close-Two".into()],
        active: true,
        ..Default::default()
    };
    let (sender, receiver) = mpsc::channel(1);
    sender.try_send(blocker_probe_batch()).unwrap();
    state.set_capability_probe_sender(sender);
    drop(receiver);
    assert!(state
        .update_upstream("up-atomic-close", updated)
        .await
        .unwrap());
    assert_eq!(
        state
            .snapshot()
            .await
            .upstreams
            .into_iter()
            .find(|upstream| upstream.id == "up-atomic-close")
            .unwrap()
            .name,
        "after update"
    );
}

#[tokio::test]
async fn freekey_sync_queues_capability_probe_for_created_route() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);

    let summary = state
        .sync_freekey_upstreams(
            "fixture-source".into(),
            vec![FreekeySyncItem {
                name: Some("managed fixture".into()),
                base_url: "https://managed.example/v1".into(),
                api_key: "fixture-secret".into(),
                model: "Lab/Managed-Model".into(),
                valid: true,
            }],
            1_700_000_000,
        )
        .await
        .unwrap();
    assert_eq!(summary.created, 1);

    let upstream = state
        .snapshot()
        .await
        .upstreams
        .into_iter()
        .find(|upstream| upstream.base_url == "https://managed.example/v1")
        .unwrap();
    let job = single_probe_job(
        tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv())
            .await
            .expect("freekey sync should queue the created route")
            .unwrap(),
    );
    assert_eq!(job.key.upstream_id, upstream.id);
    assert_eq!(job.key.runtime_model_slug, "Lab/Managed-Model");
    assert_eq!(job.key.protocol, WireProtocol::ChatCompletions);
    assert_eq!(job.reason, ProbeReason::ConfigurationChanged);
}

#[tokio::test]
async fn freekey_sync_does_not_requeue_current_route_profile() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);
    let sync_item = FreekeySyncItem {
        name: Some("managed fixture".into()),
        base_url: "https://managed.example/v1".into(),
        api_key: "fixture-secret".into(),
        model: "Lab/Managed-Model".into(),
        valid: true,
    };
    let now = 1_700_000_000;

    state
        .sync_freekey_upstreams("fixture-source".into(), vec![sync_item.clone()], now)
        .await
        .unwrap();
    let first_job = single_probe_job(
        tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv())
            .await
            .expect("the newly created route should be queued")
            .unwrap(),
    );
    let upstream = state
        .snapshot()
        .await
        .upstreams
        .into_iter()
        .find(|upstream| upstream.base_url == "https://managed.example/v1")
        .unwrap();
    let fingerprint = state
        .route_configuration_fingerprint(
            &upstream,
            "Lab/Managed-Model",
            "Lab/Managed-Model",
            chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(first_job.key);
    profile.configuration_fingerprint = fingerprint;
    profile.last_success_at = Some(now);
    state.upsert_dialect_profile(profile).await.unwrap();

    let summary = state
        .sync_freekey_upstreams(
            "fixture-source".into(),
            vec![sync_item],
            now.saturating_add(1),
        )
        .await
        .unwrap();
    assert_eq!(summary.updated, 1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv())
            .await
            .is_err(),
        "an unchanged route with a fresh matching profile must not be requeued"
    );
}

#[tokio::test]
async fn freekey_sync_does_not_wait_for_full_probe_queue() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(PersistedState::default(), &path, AppConfig::default());
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(1);
    sender.try_send(blocker_probe_batch()).unwrap();
    state.set_capability_probe_sender(sender);
    let summary = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        state.sync_freekey_upstreams(
            "fixture-source".into(),
            vec![
                FreekeySyncItem {
                    name: Some("managed fixture".into()),
                    base_url: "https://managed.example/v1".into(),
                    api_key: "fixture-secret".into(),
                    model: "Lab/Exact-One".into(),
                    valid: true,
                },
                FreekeySyncItem {
                    name: Some("managed fixture".into()),
                    base_url: "https://managed.example/v1".into(),
                    api_key: "fixture-secret".into(),
                    model: "Lab/Exact-Two".into(),
                    valid: true,
                },
            ],
            1_700_000_000,
        ),
    )
    .await
    .expect("configuration persistence must not wait for probe queue capacity")
    .unwrap();
    assert_eq!(summary.created, 2);
    let blocker = receiver.try_recv().expect("the blocker batch should remain queued");
    assert_eq!(
        single_probe_job(blocker).key.upstream_id,
        "blocker-upstream"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), receiver.recv())
            .await
            .is_err(),
        "a full queue must not receive a partial route batch"
    );
}

#[tokio::test]
async fn freekey_multi_upstream_persists_when_probe_queue_is_full() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    state
        .replace_capability_configuration(CapabilityConfiguration::default())
        .await
        .unwrap();
    let sync_items = vec![
        FreekeySyncItem {
            name: Some("managed one".into()),
            base_url: "https://managed-one.example/v1".into(),
            api_key: "fixture-secret-one".into(),
            model: "Lab/Multi-One".into(),
            valid: true,
        },
        FreekeySyncItem {
            name: Some("managed two".into()),
            base_url: "https://managed-two.example/v1".into(),
            api_key: "fixture-secret-two".into(),
            model: "Lab/Multi-Two".into(),
            valid: true,
        },
    ];
    let (sender, mut receiver) = mpsc::channel(1);
    sender.try_send(blocker_probe_batch()).unwrap();
    state.set_capability_probe_sender(sender);

    let summary = state
        .sync_freekey_upstreams("fixture-source".into(), sync_items.clone(), 1_700_000_000)
        .await
        .unwrap();
    assert_eq!(summary.created, 2);
    let blocker = receiver
        .try_recv()
        .expect("blocker batch should remain queued");
    assert_eq!(
        single_probe_job(blocker).key.upstream_id,
        "blocker-upstream"
    );
    assert!(
        receiver.try_recv().is_err(),
        "a failed multi-upstream sync must not expose a partial route batch"
    );

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), receiver.recv())
            .await
            .is_err(),
        "a full queue must not receive a partial route batch"
    );
    assert_eq!(state.snapshot().await.upstreams.len(), 2);
}

#[tokio::test]
async fn freekey_sync_succeeds_without_probe_worker() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );

    let summary = state
        .sync_freekey_upstreams(
            "fixture-source".into(),
            vec![FreekeySyncItem {
                name: None,
                base_url: "https://managed.example/v1".into(),
                api_key: "fixture-secret".into(),
                model: "Lab/Exact".into(),
                valid: true,
            }],
            1_700_000_000,
        )
        .await
        .unwrap();
    assert_eq!(summary.created, 1);
    assert_eq!(state.snapshot().await.upstreams.len(), 1);
}

#[tokio::test]
async fn freekey_sync_succeeds_with_closed_probe_queue() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    state.set_capability_probe_sender(sender);

    let summary = state
        .sync_freekey_upstreams(
            "fixture-source".into(),
            vec![FreekeySyncItem {
                name: None,
                base_url: "https://managed.example/v1".into(),
                api_key: "fixture-secret".into(),
                model: "Lab/Exact".into(),
                valid: true,
            }],
            1_700_000_000,
        )
        .await
        .unwrap();
    assert_eq!(summary.created, 1);
    assert_eq!(state.snapshot().await.upstreams.len(), 1);
}

#[tokio::test]
async fn freekey_sync_skips_probe_job_when_route_fingerprint_is_invalid() {
    let dir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let (sender, mut receiver) = mpsc::channel(1);
    state.set_capability_probe_sender(sender);

    let summary = state
        .sync_freekey_upstreams(
            "fixture-source".into(),
            vec![FreekeySyncItem {
                name: None,
                base_url: "not a valid route url".into(),
                api_key: "fixture-secret".into(),
                model: "Lab/Exact".into(),
                valid: true,
            }],
            1_700_000_000,
        )
        .await
        .unwrap();

    assert_eq!(summary.created, 1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), receiver.recv())
            .await
            .is_err(),
        "an un-fingerprintable route must not enqueue an unusable probe job"
    );
}

#[tokio::test]
async fn manual_probe_queue_for_downstream_model_emits_exact_jobs() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let state = AppState::new(
        PersistedState {
            upstreams: vec![chat_responses_codex::state::UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: "https://upstream.example/v1".into(),
                api_key: "secret".into(),
                protocol: chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                protocols: vec![
                    chat_responses_codex::routing::UpstreamProtocol::ChatCompletions,
                    chat_responses_codex::routing::UpstreamProtocol::Responses,
                ],
                supported_models: vec!["Lab/Case-Sensitive".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![chat_responses_codex::state::DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: "hash".into(),
                plaintext_key: Some("plain".into()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["Lab/Case-Sensitive".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
                max_concurrency: 10,
                daily_token_limit: None,
                monthly_token_limit: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: std::collections::HashMap::new(),
        },
        &path,
        AppConfig::default(),
    );
    let (sender, mut receiver) = mpsc::channel(8);
    state.set_capability_probe_sender(sender);

    let queued = state
        .queue_capability_probes_for_downstream_model("down-1", "Lab/Case-Sensitive")
        .await;
    assert_eq!(queued, 2);

    let first = single_probe_job(
        tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap(),
    );
    let second = single_probe_job(
        tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(first.key.upstream_id, "up-1");
    assert_eq!(first.key.runtime_model_slug, "Lab/Case-Sensitive");
    assert_eq!(second.key.upstream_id, "up-1");
    assert_eq!(second.key.runtime_model_slug, "Lab/Case-Sensitive");
}
