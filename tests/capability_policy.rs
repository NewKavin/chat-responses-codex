use std::collections::BTreeSet;

use chat_responses_codex::capabilities::{
    AgentClientProfile, Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector,
    CompatibilityBundle, CompatibilityExpectation, DeclarativeProbeCase, EvidenceReference,
    EvidenceState, HttpsImageFixture, PredicateOperator, ResponsePredicate,
    RouteCapabilityOverride, RouteIdentity, RouteTagAssignment, SemanticPolicy, TokenLimitField,
    WireProtocol, CAPABILITY_SCHEMA_VERSION,
};

fn policy(id: &str, runtime_model_glob: &str, context_window: u64) -> CapabilityPolicy {
    CapabilityPolicy {
        id: id.to_owned(),
        priority: 10,
        selector: CapabilitySelector {
            runtime_model_glob: Some(runtime_model_glob.to_owned()),
            protocol: Some(WireProtocol::ChatCompletions),
            ..Default::default()
        },
        semantic: SemanticPolicy {
            context_window: Some(context_window),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn route(runtime_model_slug: &str) -> RouteIdentity {
    RouteIdentity {
        upstream_id: "up-random".to_owned(),
        exposed_model_slug: "public-alias".to_owned(),
        runtime_model_slug: runtime_model_slug.to_owned(),
        protocol: WireProtocol::ChatCompletions,
        tags: BTreeSet::new(),
    }
}

#[test]
fn arbitrary_slug_uses_external_selector_without_recompilation() {
    let compiled = CapabilityConfiguration {
        policies: vec![policy("future-lab-models", "lab/*", 131_072)],
        ..Default::default()
    }
    .compile()
    .unwrap();

    let semantic = compiled.semantic_for(&route("lab/model-that-did-not-exist-at-build-time"));

    assert_eq!(semantic.context_window, Some(131_072));
}

#[test]
fn administrator_route_tags_feed_policy_selection() {
    let tag = "primary_vision".to_owned();
    let compiled = CapabilityConfiguration {
        policies: vec![CapabilityPolicy {
            id: "vision-tag-policy".to_owned(),
            priority: 10,
            selector: CapabilitySelector {
                tag: Some(tag.clone()),
                ..Default::default()
            },
            semantic: SemanticPolicy {
                context_window: Some(65_536),
                ..Default::default()
            },
            ..Default::default()
        }],
        route_tags: vec![RouteTagAssignment {
            id: "tag-random-lab-upstream".to_owned(),
            selector: CapabilitySelector {
                upstream_id: Some("up-random".to_owned()),
                runtime_model_glob: Some("lab/*".to_owned()),
                ..Default::default()
            },
            tags: BTreeSet::from([tag.clone()]),
        }],
        ..Default::default()
    }
    .compile()
    .unwrap();
    let mut route = route("lab/runtime-model");

    compiled.apply_route_tags(&mut route);

    assert!(route.tags.contains(&tag));
    assert_eq!(compiled.semantic_for(&route).context_window, Some(65_536));
}

#[test]
fn equal_priority_equal_specificity_conflict_rejects_bundle() {
    let error = CapabilityConfiguration {
        policies: vec![
            policy("context-small", "lab/*", 32_000),
            policy("context-large", "lab/*", 64_000),
        ],
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("ambiguous semantic field context_window"),
        "unexpected error: {error}"
    );
}

#[test]
fn expectation_bundle_is_diagnostic_data() {
    let compiled = CapabilityConfiguration {
        bundles: vec![CompatibilityBundle {
            id: "agent_core".to_owned(),
            required: BTreeSet::from([Capability::FunctionTools]),
        }],
        compatibility_expectations: vec![CompatibilityExpectation {
            id: "public-codex".to_owned(),
            selector: CapabilitySelector {
                exposed_model: Some("public-alias".to_owned()),
                ..Default::default()
            },
            bundles: BTreeSet::from(["agent_core".to_owned()]),
            client_profiles: BTreeSet::from([AgentClientProfile::Codex]),
            ..Default::default()
        }],
        ..Default::default()
    }
    .compile()
    .unwrap();
    let route = route("lab/runtime-model");
    let expectations = compiled.expectations_for(&route);

    assert_eq!(expectations.len(), 1);
    assert!(expectations[0]
        .required
        .contains(&Capability::FunctionTools));
    assert!(compiled.route_overrides().is_empty());
}

#[test]
fn protected_extension_paths_are_rejected() {
    let configuration: CapabilityConfiguration = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "policies": [{
            "id": "unsafe-extension",
            "extension_probes": [{
                "id": "writes-model",
                "protocol": "chat_completions",
                "request_patch": { "model": "forbidden" },
                "response_predicate": {
                    "path": "/accepted",
                    "operator": "exists"
                }
            }]
        }]
    }))
    .unwrap();

    let error = configuration.compile().unwrap_err();

    assert!(
        error.to_string().contains("protected request path /model"),
        "unexpected error: {error}"
    );
}

#[test]
fn configuration_defaults_round_trip_and_reject_unknown_fields() {
    let configuration: CapabilityConfiguration = serde_json::from_value(serde_json::json!({}))
        .expect("an empty object uses schema defaults");

    assert_eq!(configuration.schema_version, CAPABILITY_SCHEMA_VERSION);
    assert_eq!(configuration.revision, 0);
    assert!(configuration.probe.enabled);
    assert_eq!(configuration.probe.refresh_interval_seconds, 604_800);
    assert_eq!(configuration.probe.max_global_concurrency, 2);
    assert_eq!(configuration.probe.max_per_upstream_concurrency, 1);
    assert_eq!(configuration.probe.output_token_cap, 64);
    assert_eq!(
        serde_json::from_value::<CapabilityConfiguration>(
            serde_json::to_value(&configuration).unwrap()
        )
        .unwrap(),
        configuration
    );

    let error = serde_json::from_value::<CapabilityConfiguration>(serde_json::json!({
        "unknown": true
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn formal_public_wire_schema_round_trips_large_limits_and_diagnostic_codes() {
    let configuration: CapabilityConfiguration = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "policies": [{
            "id": "large-limit-policy",
            "semantic": {
                "context_window": 4_294_967_296_u64,
                "max_output_tokens": 4_294_967_297_u64
            }
        }],
        "compatibility_expectations": [{
            "id": "diagnostic-expectation",
            "selector": {},
            "bundles": [],
            "client_profiles": [],
            "permitted_optional_downgrades": ["optional_image_detail"]
        }],
        "probe": {
            "https_image_fixture": {
                "url": "https://fixture.invalid/image.png",
                "expected_label": "fixture"
            }
        }
    }))
    .expect("the formal wire schema must deserialize");

    let serialized = serde_json::to_value(&configuration).unwrap();
    assert_eq!(
        serialized["policies"][0]["semantic"]["context_window"],
        4_294_967_296_u64
    );
    assert_eq!(
        serialized["policies"][0]["semantic"]["max_output_tokens"],
        4_294_967_297_u64
    );
    assert_eq!(
        serialized["compatibility_expectations"][0]["permitted_optional_downgrades"][0],
        "optional_image_detail"
    );
    assert_eq!(
        serialized["probe"]["https_image_fixture"]["expected_label"],
        "fixture"
    );
    assert!(serialized["probe"].get("fixture").is_none());
    assert_eq!(
        serde_json::from_value::<CapabilityConfiguration>(serialized).unwrap(),
        configuration
    );
}

#[test]
fn unsupported_schema_version_is_rejected() {
    let error = CapabilityConfiguration {
        schema_version: CAPABILITY_SCHEMA_VERSION + 1,
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(error.to_string().contains("unsupported schema version"));
}

#[test]
fn ids_share_one_non_empty_namespace() {
    let duplicate = CapabilityConfiguration {
        policies: vec![policy("shared", "lab/*", 32_000)],
        bundles: vec![CompatibilityBundle {
            id: "shared".to_owned(),
            required: BTreeSet::new(),
        }],
        ..Default::default()
    }
    .compile()
    .unwrap_err();
    assert!(duplicate.to_string().contains("duplicate capability id"));

    let empty = CapabilityConfiguration {
        policies: vec![policy("  ", "lab/*", 32_000)],
        ..Default::default()
    }
    .compile()
    .unwrap_err();
    assert!(empty.to_string().contains("empty capability id"));
}

#[test]
fn expectations_must_reference_known_bundles() {
    let error = CapabilityConfiguration {
        compatibility_expectations: vec![CompatibilityExpectation {
            id: "unknown-bundle-expectation".to_owned(),
            bundles: BTreeSet::from(["missing".to_owned()]),
            ..Default::default()
        }],
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(error.to_string().contains("unknown compatibility bundle"));
}

#[test]
fn selector_globs_are_validated_at_compile_time() {
    let error = CapabilityConfiguration {
        policies: vec![policy("invalid-glob", "lab/[", 32_000)],
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(error.to_string().contains("invalid selector glob"));
}

#[test]
fn fixtures_require_https_and_a_non_empty_label() {
    for fixture in [
        HttpsImageFixture {
            url: "http://fixture.invalid/image.png".to_owned(),
            expected_label: "fixture".to_owned(),
        },
        HttpsImageFixture {
            url: "https://fixture.invalid/image.png".to_owned(),
            expected_label: "  ".to_owned(),
        },
    ] {
        let error = CapabilityConfiguration {
            probe: chat_responses_codex::capabilities::ProbeConfiguration {
                https_image_fixture: Some(fixture),
                ..Default::default()
            },
            ..Default::default()
        }
        .compile()
        .unwrap_err();

        assert!(error.to_string().contains("invalid HTTPS fixture"));
    }
}

#[test]
fn fixtures_reject_sensitive_url_userinfo_and_query_credentials() {
    for url in [
        "https://fixture-user:fixture-password@fixture.invalid/image.png",
        "https://fixture.invalid/image.png?X-Amz-Signature=fixture-signature",
    ] {
        let error = CapabilityConfiguration {
            probe: chat_responses_codex::capabilities::ProbeConfiguration {
                https_image_fixture: Some(HttpsImageFixture {
                    url: url.into(),
                    expected_label: "fixture".into(),
                }),
                ..Default::default()
            },
            ..CapabilityConfiguration::default()
        }
        .compile()
        .unwrap_err();

        assert!(error.to_string().contains("sensitive URL"));
        assert!(!error.to_string().contains(url));
    }
}

fn extension_case(id: &str, request_patch: serde_json::Value) -> DeclarativeProbeCase {
    DeclarativeProbeCase {
        id: id.to_owned(),
        protocol: WireProtocol::ChatCompletions,
        prerequisites: BTreeSet::new(),
        request_patch,
        response_predicate: ResponsePredicate {
            path: "/accepted".to_owned(),
            operator: PredicateOperator::Exists,
            value: None,
        },
    }
}

#[test]
fn extension_predicate_paths_are_bounded() {
    for path in ["accepted".to_owned(), format!("/{}", "x".repeat(256))] {
        let mut probe = extension_case("bounded-path", serde_json::json!({"metadata": true}));
        probe.response_predicate.path = path;
        let error = CapabilityConfiguration {
            policies: vec![CapabilityPolicy {
                id: "extension-policy".to_owned(),
                extension_probes: vec![probe],
                ..Default::default()
            }],
            ..Default::default()
        }
        .compile()
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("invalid bounded response predicate path"));
    }
}

#[test]
fn extension_cases_have_a_serialized_size_limit() {
    let error = CapabilityConfiguration {
        policies: vec![CapabilityPolicy {
            id: "large-extension-policy".to_owned(),
            extension_probes: vec![extension_case(
                "large-extension",
                serde_json::json!({"metadata": "x".repeat(16_384)}),
            )],
            ..Default::default()
        }],
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("extension case over 16384 serialized bytes"));
}

#[test]
fn route_tag_assignments_cannot_depend_on_tags_or_assign_empty_tags() {
    let selecting_by_tag = CapabilityConfiguration {
        route_tags: vec![RouteTagAssignment {
            id: "recursive-tag".to_owned(),
            selector: CapabilitySelector {
                tag: Some("already-tagged".to_owned()),
                ..Default::default()
            },
            tags: BTreeSet::from(["new-tag".to_owned()]),
        }],
        ..Default::default()
    }
    .compile()
    .unwrap_err();
    assert!(selecting_by_tag
        .to_string()
        .contains("route tag assignment cannot select by tag"));

    for tags in [BTreeSet::new(), BTreeSet::from([" ".to_owned()])] {
        let error = CapabilityConfiguration {
            route_tags: vec![RouteTagAssignment {
                id: "empty-tag".to_owned(),
                selector: CapabilitySelector::default(),
                tags,
            }],
            ..Default::default()
        }
        .compile()
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("route tag assignment cannot assign empty tags"));
    }
}

#[test]
fn semantic_merge_and_policy_order_are_deterministic() {
    let mut base = policy("base", "lab/*", 32_000);
    base.priority = 1;
    base.semantic
        .effort_map
        .insert("high".to_owned(), "8".to_owned());
    base.semantic
        .omit_sampling_fields
        .insert("temperature".to_owned());
    base.extension_probes.push(extension_case(
        "base-extension",
        serde_json::json!({"metadata": {"base": true}}),
    ));

    let mut upstream = CapabilityPolicy {
        id: "upstream".to_owned(),
        priority: 2,
        selector: CapabilitySelector {
            upstream_id: Some("up-random".to_owned()),
            protocol: Some(WireProtocol::ChatCompletions),
            ..Default::default()
        },
        semantic: SemanticPolicy {
            max_output_tokens: Some(4_096),
            effort_map: [("high".to_owned(), "12".to_owned())].into(),
            omit_sampling_fields: BTreeSet::from(["top_p".to_owned()]),
            ..Default::default()
        },
        ..Default::default()
    };
    upstream.extension_probes.push(extension_case(
        "upstream-extension",
        serde_json::json!({"metadata": {"upstream": true}}),
    ));

    let configuration = CapabilityConfiguration {
        revision: 42,
        policies: vec![upstream, base],
        ..Default::default()
    };
    let compiled = configuration.compile().unwrap();
    let route = route("lab/runtime-model");
    let semantic = compiled.semantic_for(&route);

    assert_eq!(compiled.source(), &configuration);
    assert_eq!(compiled.digest().len(), 64);
    assert_eq!(compiled.policy_ids_for(&route), vec!["base", "upstream"]);
    assert_eq!(semantic.context_window, Some(32_000));
    assert_eq!(semantic.max_output_tokens, Some(4_096));
    assert_eq!(semantic.effort_map.get("high").unwrap(), "12");
    assert_eq!(
        semantic.omit_sampling_fields,
        BTreeSet::from(["temperature".to_owned(), "top_p".to_owned()])
    );
    assert_eq!(
        compiled
            .extensions_for(&route)
            .iter()
            .map(|probe| probe.id.as_str())
            .collect::<Vec<_>>(),
        vec!["base-extension", "upstream-extension"]
    );
    assert_eq!(configuration.compile().unwrap().digest(), compiled.digest());
}

#[test]
fn non_overlapping_selectors_do_not_conflict() {
    let mut first = policy("first", "lab/*", 32_000);
    first.selector.runtime_model = Some("lab/one".to_owned());
    let mut second = policy("second", "lab/*", 64_000);
    second.selector.runtime_model = Some("lab/two".to_owned());

    CapabilityConfiguration {
        policies: vec![first, second],
        ..Default::default()
    }
    .compile()
    .unwrap();

    let mut exact = policy("exact", "lab/*", 32_000);
    exact.selector.runtime_model = Some("lab/one".to_owned());
    let mismatched_glob = policy("mismatched-glob", "other/*", 64_000);
    CapabilityConfiguration {
        policies: vec![exact, mismatched_glob],
        ..Default::default()
    }
    .compile()
    .unwrap();
}

#[test]
fn equal_rank_override_conflicts_are_rejected() {
    let override_for = |id: &str, state| RouteCapabilityOverride {
        id: id.to_owned(),
        priority: 10,
        selector: CapabilitySelector {
            runtime_model_glob: Some("lab/*".to_owned()),
            ..Default::default()
        },
        capabilities: [(Capability::FunctionTools, state)].into(),
        ..Default::default()
    };
    let error = CapabilityConfiguration {
        route_overrides: vec![
            override_for("supported-tools", EvidenceState::Supported),
            override_for("rejected-tools", EvidenceState::Rejected),
        ],
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(error.to_string().contains("ambiguous override field"));
}

#[test]
fn different_tag_selectors_can_overlap_for_policy_conflicts() {
    let tagged_policy = |id: &str, tag: &str, context_window| CapabilityPolicy {
        id: id.to_owned(),
        priority: 10,
        selector: CapabilitySelector {
            tag: Some(tag.to_owned()),
            ..Default::default()
        },
        semantic: SemanticPolicy {
            context_window: Some(context_window),
            ..Default::default()
        },
        ..Default::default()
    };
    let error = CapabilityConfiguration {
        policies: vec![
            tagged_policy("tag-a-context", "tag-a", 32_000),
            tagged_policy("tag-b-context", "tag-b", 64_000),
        ],
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("ambiguous semantic field context_window"),
        "unexpected error: {error}"
    );
}

#[test]
fn different_tag_selectors_can_overlap_for_override_conflicts() {
    let tagged_override = |id: &str, tag: &str, state| RouteCapabilityOverride {
        id: id.to_owned(),
        priority: 10,
        selector: CapabilitySelector {
            tag: Some(tag.to_owned()),
            ..Default::default()
        },
        capabilities: [(Capability::FunctionTools, state)].into(),
        ..Default::default()
    };
    let error = CapabilityConfiguration {
        route_overrides: vec![
            tagged_override("tag-a-tools", "tag-a", EvidenceState::Supported),
            tagged_override("tag-b-tools", "tag-b", EvidenceState::Rejected),
        ],
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(
        error.to_string().contains("ambiguous override field"),
        "unexpected error: {error}"
    );
}

#[test]
fn matching_route_overrides_are_sorted_by_rank() {
    let override_for = |id: &str, priority, token_limit_field| RouteCapabilityOverride {
        id: id.to_owned(),
        priority,
        selector: CapabilitySelector {
            runtime_model_glob: Some("lab/*".to_owned()),
            ..Default::default()
        },
        token_limit_field: Some(token_limit_field),
        ..Default::default()
    };
    let compiled = CapabilityConfiguration {
        route_overrides: vec![
            override_for("high", 20, TokenLimitField::MaxOutputTokens),
            override_for("low", 10, TokenLimitField::MaxTokens),
        ],
        ..Default::default()
    }
    .compile()
    .unwrap();

    assert_eq!(
        compiled
            .route_overrides_for(&route("lab/runtime-model"))
            .iter()
            .map(|route_override| route_override.id.as_str())
            .collect::<Vec<_>>(),
        vec!["low", "high"]
    );
}

#[test]
fn route_overrides_are_sorted_without_filtering() {
    let route_override = |id: &str, priority, selector| RouteCapabilityOverride {
        id: id.to_owned(),
        priority,
        selector,
        ..Default::default()
    };
    let compiled = CapabilityConfiguration {
        route_overrides: vec![
            route_override("high", 10, CapabilitySelector::default()),
            route_override(
                "specific",
                5,
                CapabilitySelector {
                    upstream_id: Some("up-random".to_owned()),
                    ..Default::default()
                },
            ),
            route_override("rank-z", 5, CapabilitySelector::default()),
            route_override("rank-a", 5, CapabilitySelector::default()),
        ],
        ..Default::default()
    }
    .compile()
    .unwrap();

    assert_eq!(
        compiled
            .route_overrides()
            .iter()
            .map(|route_override| route_override.id.as_str())
            .collect::<Vec<_>>(),
        vec!["rank-a", "rank-z", "specific", "high"]
    );
}

#[test]
fn resource_policy_count_is_bounded_before_conflict_analysis() {
    let policies = (0..1_025)
        .map(|index| {
            policy(
                &format!("bounded-policy-{index}"),
                "lab/*",
                if index == 0 { 32_000 } else { 64_000 },
            )
        })
        .collect();

    let error = CapabilityConfiguration {
        policies,
        ..Default::default()
    }
    .compile()
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("too many entries in field policies"),
        "unexpected error: {error}"
    );
}

#[test]
fn resource_selector_and_assigned_tag_values_are_bounded() {
    let too_long = "x".repeat(1_025);
    let configurations = [
        CapabilityConfiguration {
            policies: vec![CapabilityPolicy {
                id: "long-glob".to_owned(),
                selector: CapabilitySelector {
                    runtime_model_glob: Some(too_long.clone()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        CapabilityConfiguration {
            policies: vec![CapabilityPolicy {
                id: "long-selector-tag".to_owned(),
                selector: CapabilitySelector {
                    tag: Some(too_long.clone()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        CapabilityConfiguration {
            route_tags: vec![RouteTagAssignment {
                id: "long-assigned-tag".to_owned(),
                selector: CapabilitySelector::default(),
                tags: BTreeSet::from([too_long]),
            }],
            ..Default::default()
        },
    ];

    for configuration in configurations {
        let error = match configuration.compile() {
            Ok(_) => panic!("overlong selector value compiled"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("selector value exceeds 1024 bytes"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn resource_total_extension_probe_count_is_bounded() {
    let extension_probes = (0..1_025)
        .map(|index| {
            extension_case(
                &format!("bounded-extension-{index}"),
                serde_json::json!({"metadata": true}),
            )
        })
        .collect();
    let configuration = CapabilityConfiguration {
        policies: vec![CapabilityPolicy {
            id: "many-extensions".to_owned(),
            extension_probes,
            ..Default::default()
        }],
        ..Default::default()
    };

    let error = match configuration.compile() {
        Ok(_) => panic!("too many extension probes compiled"),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("too many entries in field extension_probes"),
        "unexpected error: {error}"
    );
}

#[test]
fn resource_total_configuration_size_is_bounded() {
    let configuration = CapabilityConfiguration {
        policies: vec![CapabilityPolicy {
            id: "large-evidence-policy".to_owned(),
            evidence: vec![EvidenceReference {
                title: "x".repeat(1_048_576),
                url: "https://evidence.invalid/reference".to_owned(),
                retrieved_at: "2026-07-10".to_owned(),
                version: None,
            }],
            ..Default::default()
        }],
        ..Default::default()
    };

    let error = match configuration.compile() {
        Ok(_) => panic!("oversized configuration compiled"),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("configuration exceeds 1048576 bytes"),
        "unexpected error: {error}"
    );
}
