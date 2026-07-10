use std::collections::{BTreeMap, BTreeSet};

use chat_responses_codex::capabilities::*;

fn route(protocol: WireProtocol) -> RouteIdentity {
    RouteIdentity {
        upstream_id: "relay-17".to_owned(),
        exposed_model_slug: "opaque-public-name".to_owned(),
        runtime_model_slug: "opaque-runtime-name".to_owned(),
        protocol,
        tags: BTreeSet::new(),
    }
}

#[test]
fn explicit_override_beats_probe_and_probe_beats_baseline() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures::text_stream();
    let semantic = SemanticPolicy::default();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::ParallelToolCalls, EvidenceState::Supported);
    let route_override = RouteCapabilityOverride {
        id: "reject-parallel-tools".to_owned(),
        capabilities: BTreeMap::from([(Capability::ParallelToolCalls, EvidenceState::Rejected)]),
        token_limit_field: Some(TokenLimitField::MaxCompletionTokens),
        ..Default::default()
    };
    let route_overrides = [&route_override];

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &route_overrides,
            policy_extensions: &[],
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();

    assert_eq!(
        resolved.state(Capability::ParallelToolCalls),
        EvidenceState::Rejected
    );
    assert_eq!(
        resolved.source(Capability::ParallelToolCalls),
        CapabilitySource::Override
    );
    assert_eq!(
        resolved.token_limit_field,
        TokenLimitField::MaxCompletionTokens
    );
}

#[test]
fn unprobed_chat_is_conservative_and_unprobed_responses_is_restricted() {
    let requested = RequestedFeatures::text_stream();
    let chat_route = route(WireProtocol::ChatCompletions);
    let chat = CapabilityResolver
        .resolve(ResolutionInput::baseline(&chat_route, &requested))
        .unwrap();

    assert!(chat.supports(Capability::FunctionTools));
    assert!(!chat.supports(Capability::ImageDataUrl));

    let responses_route = route(WireProtocol::Responses);
    let responses = CapabilityResolver
        .resolve(ResolutionInput::baseline(&responses_route, &requested))
        .unwrap();

    assert!(responses.provisional);
    assert!(!responses.native_preferred);
}

#[test]
fn required_image_is_rejected_before_dispatch_without_positive_evidence() {
    let route = route(WireProtocol::ChatCompletions);
    let mut requested = RequestedFeatures::text_stream();
    requested.required.insert(Capability::ImageHttps);

    let error = CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &requested))
        .unwrap_err();

    assert_eq!(error.capability, Capability::ImageHttps);
    assert_eq!(error.category(), "gateway_protocol_capability_unsupported");
}

#[test]
fn legacy_strip_flag_cannot_remove_required_continuation_state() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures {
        required: BTreeSet::from([Capability::FunctionTools, Capability::ReasoningReplay]),
        ..RequestedFeatures::text_stream()
    };
    let semantic = SemanticPolicy::default();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::ReasoningReplay, EvidenceState::Supported);
    profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &[],
            policy_extensions: &[],
            profile: Some(&profile),
            strip_nonstandard_chat_fields: true,
        })
        .unwrap();

    assert!(resolved.supports(Capability::ReasoningReplay));
    assert!(resolved.omit_optional_extensions);
}

#[test]
fn capability_all_and_messages_baseline_are_complete_and_conservative() {
    assert_eq!(
        Capability::ALL,
        [
            Capability::TextInput,
            Capability::ImageHttps,
            Capability::ImageDataUrl,
            Capability::ImageDetail,
            Capability::NativeFileId,
            Capability::FunctionTools,
            Capability::NamespaceTools,
            Capability::CustomTools,
            Capability::HostedTools,
            Capability::ParallelToolCalls,
            Capability::ForcedToolChoice,
            Capability::ToolContinuation,
            Capability::ReasoningOutput,
            Capability::ReasoningReplay,
            Capability::TextStream,
            Capability::ReasoningStream,
            Capability::IndexedToolArgumentStream,
            Capability::UsageStream,
            Capability::StructuredOutput,
        ]
    );

    let route = route(WireProtocol::Messages);
    let requested = RequestedFeatures::text_stream();
    let resolved = CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &requested))
        .unwrap();

    assert_eq!(resolved.values.len(), Capability::ALL.len());
    let baseline_supported = BTreeSet::from([
        Capability::TextInput,
        Capability::FunctionTools,
        Capability::ForcedToolChoice,
        Capability::ToolContinuation,
        Capability::TextStream,
    ]);
    for capability in Capability::ALL {
        let expected = if baseline_supported.contains(&capability) {
            EvidenceState::Supported
        } else {
            EvidenceState::Unobserved
        };
        assert_eq!(resolved.state(capability), expected, "{capability:?}");
        assert_eq!(
            resolved.source(capability),
            CapabilitySource::Baseline,
            "{capability:?}"
        );
    }
    assert!(resolved.provisional);
    assert!(!resolved.native_preferred);
}

#[test]
fn profile_key_uses_exact_upstream_runtime_and_protocol_only() {
    use chat_responses_codex::routing::UpstreamProtocol;

    let first = route(WireProtocol::ChatCompletions);
    let mut different_public_name = first.clone();
    different_public_name.exposed_model_slug = "another-public-name".to_owned();

    assert_eq!(
        DialectProfileKey::from_route(&first),
        DialectProfileKey::from_route(&different_public_name)
    );
    assert_eq!(
        WireProtocol::from(UpstreamProtocol::ChatCompletions),
        WireProtocol::ChatCompletions
    );
    assert_eq!(
        WireProtocol::from(UpstreamProtocol::Responses),
        WireProtocol::Responses
    );
}

#[test]
fn unknown_profile_has_stable_empty_serializable_state() {
    let key = DialectProfileKey::from_route(&route(WireProtocol::Responses));
    let profile = UpstreamDialectProfile::unknown(key.clone());

    assert_eq!(profile.key, key);
    assert_eq!(profile.probe_schema_version, DIALECT_PROBE_SCHEMA_VERSION);
    assert_eq!(profile.state, DialectProfileState::Unknown);
    assert!(profile.configuration_fingerprint.is_empty());
    assert!(profile.capabilities.is_empty());
    assert!(profile.correction_rules.is_empty());
    assert!(profile.reasoning_controls.is_empty());
    assert!(profile.extension_evidence.is_empty());
    assert!(profile.evidence_codes.is_empty());
    assert!(profile.event_types.is_empty());
    assert_eq!(
        serde_json::from_value::<UpstreamDialectProfile>(serde_json::to_value(&profile).unwrap())
            .unwrap(),
        profile
    );
}

#[test]
fn continuation_capabilities_require_an_exact_profile_key_match() {
    let route = route(WireProtocol::ChatCompletions);
    let foreign_key = DialectProfileKey {
        upstream_id: "relay-elsewhere".to_owned(),
        runtime_model_slug: route.runtime_model_slug.clone(),
        protocol: route.protocol,
    };
    let mismatched = RequestedFeatures {
        optional: BTreeSet::from([Capability::ReasoningOutput, Capability::ReasoningReplay]),
        continuation_profile: Some(foreign_key),
        continuation_reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
        ..RequestedFeatures::text_stream()
    };

    let resolved = CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &mismatched))
        .unwrap();
    assert!(!resolved.supports(Capability::ReasoningOutput));
    assert!(!resolved.supports(Capability::ReasoningReplay));

    let matching = RequestedFeatures {
        required: BTreeSet::from([Capability::ReasoningReplay]),
        continuation_profile: Some(DialectProfileKey::from_route(&route)),
        continuation_reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
        ..RequestedFeatures::text_stream()
    };
    let resolved = CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &matching))
        .unwrap();

    assert!(resolved.supports(Capability::ReasoningOutput));
    assert!(resolved.supports(Capability::ReasoningReplay));
    assert_eq!(
        resolved.source(Capability::ReasoningReplay),
        CapabilitySource::Baseline
    );

    let non_content_carrier = RequestedFeatures {
        optional: BTreeSet::from([Capability::ReasoningOutput, Capability::ReasoningReplay]),
        continuation_profile: Some(DialectProfileKey::from_route(&route)),
        continuation_reasoning_carrier: Some(ReasoningCarrier::ResponsesReasoningItem),
        ..RequestedFeatures::text_stream()
    };
    let resolved = CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &non_content_carrier))
        .unwrap();
    assert!(!resolved.supports(Capability::ReasoningOutput));
    assert!(!resolved.supports(Capability::ReasoningReplay));
    assert_eq!(
        resolved.reasoning_carrier,
        ReasoningCarrier::ResponsesReasoningItem
    );
}

#[test]
fn semantic_reasoning_requirements_fail_closed_without_wire_evidence() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures::text_stream();
    let fixed_on = SemanticPolicy {
        reasoning_mode: Some(ReasoningMode::FixedOn),
        ..Default::default()
    };

    let error = CapabilityResolver
        .resolve(ResolutionInput {
            semantic: &fixed_on,
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap_err();
    assert_eq!(error.capability, Capability::ReasoningOutput);

    let replay_required = SemanticPolicy {
        reasoning_replay_required: Some(true),
        ..Default::default()
    };
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    profile.reasoning_carrier = Some(ReasoningCarrier::ReasoningContent);
    let error = CapabilityResolver
        .resolve(ResolutionInput {
            semantic: &replay_required,
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap_err();

    assert_eq!(error.capability, Capability::ReasoningReplay);
    assert_eq!(error.category(), "gateway_protocol_capability_unsupported");
}

#[test]
fn unobserved_probe_does_not_erase_baseline_but_rejection_does() {
    let route = route(WireProtocol::Responses);
    let requested = RequestedFeatures::text_stream();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Unobserved);

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap();
    assert!(resolved.supports(Capability::FunctionTools));
    assert_eq!(
        resolved.source(Capability::FunctionTools),
        CapabilitySource::Baseline
    );

    profile
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Rejected);
    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap();
    assert_eq!(
        resolved.state(Capability::FunctionTools),
        EvidenceState::Rejected
    );
    assert_eq!(
        resolved.source(Capability::FunctionTools),
        CapabilitySource::Probe
    );
}

#[test]
fn later_capability_override_wins_and_required_rejection_fails_closed() {
    let route = route(WireProtocol::ChatCompletions);
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Supported);
    let first_override = RouteCapabilityOverride {
        id: "first".to_owned(),
        capabilities: BTreeMap::from([(Capability::ImageHttps, EvidenceState::Supported)]),
        ..Default::default()
    };
    let last_override = RouteCapabilityOverride {
        id: "last".to_owned(),
        capabilities: BTreeMap::from([(Capability::ImageHttps, EvidenceState::Rejected)]),
        ..Default::default()
    };
    let overrides = [&first_override, &last_override];
    let optional = RequestedFeatures {
        optional: BTreeSet::from([Capability::ImageHttps]),
        ..RequestedFeatures::text_stream()
    };

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &optional,
            semantic: &SemanticPolicy::default(),
            route_overrides: &overrides,
            policy_extensions: &[],
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert_eq!(
        resolved.state(Capability::ImageHttps),
        EvidenceState::Rejected
    );
    assert_eq!(
        resolved.source(Capability::ImageHttps),
        CapabilitySource::Override
    );

    let required = RequestedFeatures {
        required: BTreeSet::from([Capability::ImageHttps]),
        ..RequestedFeatures::text_stream()
    };
    let error = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &required,
            semantic: &SemanticPolicy::default(),
            route_overrides: &overrides,
            policy_extensions: &[],
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap_err();
    assert_eq!(error.capability, Capability::ImageHttps);
}

#[test]
fn mismatched_profile_is_ignored_for_all_route_scoped_evidence() {
    let route = route(WireProtocol::Responses);
    let foreign_key = DialectProfileKey {
        upstream_id: "another-relay".to_owned(),
        runtime_model_slug: route.runtime_model_slug.clone(),
        protocol: route.protocol,
    };
    let mut profile = UpstreamDialectProfile::unknown(foreign_key);
    profile.state = DialectProfileState::Verified;
    profile.capabilities =
        BTreeMap::from([(Capability::ParallelToolCalls, EvidenceState::Supported)]);
    profile.token_limit_field = Some(TokenLimitField::MaxOutputTokens);
    profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
    profile.reasoning_controls =
        BTreeMap::from([("effort".to_owned(), vec!["accepted".to_owned()])]);
    profile.extension_evidence =
        BTreeMap::from([("foreign-extension".to_owned(), EvidenceState::Supported)]);
    let extension = extension(
        "foreign-extension",
        WireProtocol::Responses,
        BTreeSet::new(),
    );
    let extensions = [&extension];
    let required = RequestedFeatures {
        required: BTreeSet::from([Capability::ParallelToolCalls]),
        ..RequestedFeatures::text_stream()
    };

    let error = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &required,
            semantic: &SemanticPolicy::default(),
            route_overrides: &[],
            policy_extensions: &extensions,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap_err();
    assert_eq!(error.capability, Capability::ParallelToolCalls);

    let requested = RequestedFeatures::text_stream();
    let semantic = SemanticPolicy {
        effort_map: BTreeMap::from([("high".to_owned(), "accepted".to_owned())]),
        ..Default::default()
    };
    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &[],
            policy_extensions: &extensions,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();

    assert_eq!(
        resolved.state(Capability::ParallelToolCalls),
        EvidenceState::Unobserved
    );
    assert_eq!(resolved.token_limit_field, TokenLimitField::Omit);
    assert_eq!(resolved.reasoning_carrier, ReasoningCarrier::None);
    assert_eq!(resolved.reasoning_control_field, None);
    assert!(resolved.effort_map.is_empty());
    assert!(resolved.request_extensions.is_empty());
    assert!(resolved.provisional);
    assert!(!resolved.native_preferred);
}

fn extension(
    id: &str,
    protocol: WireProtocol,
    prerequisites: BTreeSet<Capability>,
) -> DeclarativeProbeCase {
    DeclarativeProbeCase {
        id: id.to_owned(),
        protocol,
        prerequisites,
        request_patch: serde_json::json!({"metadata": {id: true}}),
        response_predicate: ResponsePredicate {
            path: "/accepted".to_owned(),
            operator: PredicateOperator::Exists,
            value: None,
        },
    }
}

#[test]
fn extensions_require_protocol_prerequisites_and_positive_final_evidence() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures {
        optional: BTreeSet::from([Capability::ImageHttps, Capability::FunctionTools]),
        ..RequestedFeatures::text_stream()
    };
    let cases = [
        extension(
            "profile-supported",
            WireProtocol::ChatCompletions,
            BTreeSet::from([Capability::FunctionTools]),
        ),
        extension("wrong-protocol", WireProtocol::Responses, BTreeSet::new()),
        extension(
            "missing-prerequisite",
            WireProtocol::ChatCompletions,
            BTreeSet::from([Capability::NativeFileId]),
        ),
        extension(
            "optional-prerequisite",
            WireProtocol::ChatCompletions,
            BTreeSet::from([Capability::ImageHttps]),
        ),
        extension(
            "last-override-rejects",
            WireProtocol::ChatCompletions,
            BTreeSet::new(),
        ),
        extension(
            "last-override-supports",
            WireProtocol::ChatCompletions,
            BTreeSet::new(),
        ),
    ];
    let case_refs = cases.iter().collect::<Vec<_>>();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile.extension_evidence = BTreeMap::from([
        ("profile-supported".to_owned(), EvidenceState::Supported),
        ("wrong-protocol".to_owned(), EvidenceState::Supported),
        ("missing-prerequisite".to_owned(), EvidenceState::Supported),
        ("optional-prerequisite".to_owned(), EvidenceState::Supported),
        ("last-override-rejects".to_owned(), EvidenceState::Supported),
        ("last-override-supports".to_owned(), EvidenceState::Rejected),
    ]);
    let first_override = RouteCapabilityOverride {
        id: "first".to_owned(),
        extensions: BTreeMap::from([
            ("last-override-rejects".to_owned(), EvidenceState::Supported),
            ("last-override-supports".to_owned(), EvidenceState::Rejected),
        ]),
        ..Default::default()
    };
    let last_override = RouteCapabilityOverride {
        id: "last".to_owned(),
        extensions: BTreeMap::from([
            ("last-override-rejects".to_owned(), EvidenceState::Rejected),
            (
                "last-override-supports".to_owned(),
                EvidenceState::Supported,
            ),
        ]),
        ..Default::default()
    };
    let overrides = [&first_override, &last_override];

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &SemanticPolicy::default(),
            route_overrides: &overrides,
            policy_extensions: &case_refs,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();

    assert_eq!(
        resolved
            .request_extensions
            .iter()
            .map(|extension| extension.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "profile-supported",
            "optional-prerequisite",
            "last-override-supports"
        ]
    );
    assert!(!resolved.omit_optional_extensions);
    assert_eq!(
        resolved.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Override)
    );

    let stripped = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &SemanticPolicy::default(),
            route_overrides: &overrides,
            policy_extensions: &case_refs,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: true,
        })
        .unwrap();
    assert!(stripped.request_extensions.is_empty());
    assert!(stripped.omit_optional_extensions);
    assert_eq!(
        stripped.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Baseline)
    );
}

#[test]
fn rejected_override_evidence_sources_a_nonempty_extension_resolution() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures::text_stream();
    let cases = [
        extension(
            "profile-kept",
            WireProtocol::ChatCompletions,
            BTreeSet::new(),
        ),
        extension(
            "override-rejected",
            WireProtocol::ChatCompletions,
            BTreeSet::new(),
        ),
    ];
    let case_refs = cases.iter().collect::<Vec<_>>();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile.extension_evidence = BTreeMap::from([
        ("profile-kept".to_owned(), EvidenceState::Supported),
        ("override-rejected".to_owned(), EvidenceState::Supported),
    ]);
    let route_override = RouteCapabilityOverride {
        id: "reject-one-extension".to_owned(),
        extensions: BTreeMap::from([("override-rejected".to_owned(), EvidenceState::Rejected)]),
        ..Default::default()
    };
    let overrides = [&route_override];

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &SemanticPolicy::default(),
            route_overrides: &overrides,
            policy_extensions: &case_refs,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();

    assert_eq!(resolved.request_extensions.len(), 1);
    assert_eq!(resolved.request_extensions[0].id, "profile-kept");
    assert_eq!(
        resolved.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Override)
    );
}

#[test]
fn rejected_only_extension_evidence_preserves_the_effective_source() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures::text_stream();
    let semantic = SemanticPolicy::default();
    let cases = [
        extension("eligible", WireProtocol::ChatCompletions, BTreeSet::new()),
        extension("wrong-protocol", WireProtocol::Responses, BTreeSet::new()),
        extension(
            "missing-prerequisite",
            WireProtocol::ChatCompletions,
            BTreeSet::from([Capability::NativeFileId]),
        ),
    ];
    let case_refs = cases.iter().collect::<Vec<_>>();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile.extension_evidence = BTreeMap::from([
        ("wrong-protocol".to_owned(), EvidenceState::Rejected),
        ("missing-prerequisite".to_owned(), EvidenceState::Rejected),
    ]);

    let unrelated = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &[],
            policy_extensions: &case_refs,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert!(unrelated.request_extensions.is_empty());
    assert_eq!(
        unrelated.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Baseline)
    );

    profile
        .extension_evidence
        .insert("eligible".to_owned(), EvidenceState::Rejected);
    let profile_rejected = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &[],
            policy_extensions: &case_refs,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert!(profile_rejected.request_extensions.is_empty());
    assert_eq!(
        profile_rejected.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Probe)
    );

    let route_override = RouteCapabilityOverride {
        id: "reject-eligible".to_owned(),
        extensions: BTreeMap::from([
            ("eligible".to_owned(), EvidenceState::Rejected),
            ("wrong-protocol".to_owned(), EvidenceState::Rejected),
            ("missing-prerequisite".to_owned(), EvidenceState::Rejected),
        ]),
        ..Default::default()
    };
    let overrides = [&route_override];
    let override_rejected = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &overrides,
            policy_extensions: &case_refs,
            profile: None,
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert!(override_rejected.request_extensions.is_empty());
    assert_eq!(
        override_rejected.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Override)
    );

    let both = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &overrides,
            policy_extensions: &case_refs,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert!(both.request_extensions.is_empty());
    assert_eq!(
        both.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Override)
    );
}

#[test]
fn scalar_resolution_uses_last_override_then_probe_then_continuation() {
    let route = route(WireProtocol::Responses);
    let requested = RequestedFeatures {
        continuation_profile: Some(DialectProfileKey::from_route(&route)),
        continuation_reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
        ..RequestedFeatures::text_stream()
    };
    let semantic = SemanticPolicy {
        reasoning_mode: Some(ReasoningMode::Optional),
        context_window: Some(4_294_967_296),
        max_output_tokens: Some(4_294_967_297),
        omit_sampling_fields: BTreeSet::from(["temperature".to_owned()]),
        ..Default::default()
    };
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile.state = DialectProfileState::Verified;
    profile.token_limit_field = Some(TokenLimitField::MaxOutputTokens);
    profile.reasoning_carrier = Some(ReasoningCarrier::ResponsesReasoningItem);
    let first_override = RouteCapabilityOverride {
        id: "first".to_owned(),
        token_limit_field: Some(TokenLimitField::MaxCompletionTokens),
        reasoning_carrier: Some(ReasoningCarrier::MessagesThinking),
        ..Default::default()
    };
    let last_override = RouteCapabilityOverride {
        id: "last".to_owned(),
        token_limit_field: Some(TokenLimitField::MaxTokens),
        ..Default::default()
    };
    let overrides = [&first_override, &last_override];

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &overrides,
            policy_extensions: &[],
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();

    assert_eq!(resolved.token_limit_field, TokenLimitField::MaxTokens);
    assert_eq!(
        resolved.reasoning_carrier,
        ReasoningCarrier::MessagesThinking
    );
    assert_eq!(resolved.reasoning_mode, ReasoningMode::Optional);
    assert_eq!(resolved.context_window, Some(4_294_967_296));
    assert_eq!(resolved.max_output_tokens, Some(4_294_967_297));
    assert_eq!(
        resolved.omit_sampling_fields,
        BTreeSet::from(["temperature".to_owned()])
    );
    assert_eq!(
        resolved.field_sources.get("token_limit_field"),
        Some(&CapabilitySource::Override)
    );
    assert_eq!(
        resolved.field_sources.get("reasoning_carrier"),
        Some(&CapabilitySource::Override)
    );
    assert_eq!(
        resolved.field_sources.get("reasoning_mode"),
        Some(&CapabilitySource::Policy)
    );
    assert_eq!(
        resolved.field_sources.get("context_window"),
        Some(&CapabilitySource::Policy)
    );
    assert_eq!(
        resolved.field_sources.get("max_output_tokens"),
        Some(&CapabilitySource::Policy)
    );
    assert!(!resolved.provisional);
    assert!(resolved.native_preferred);

    let profile_only = CapabilityResolver
        .resolve(ResolutionInput {
            route_overrides: &[],
            ..ResolutionInput {
                route: &route,
                requested: &requested,
                semantic: &SemanticPolicy::default(),
                route_overrides: &[],
                policy_extensions: &[],
                profile: Some(&profile),
                strip_nonstandard_chat_fields: false,
            }
        })
        .unwrap();
    assert_eq!(
        profile_only.token_limit_field,
        TokenLimitField::MaxOutputTokens
    );
    assert_eq!(
        profile_only.reasoning_carrier,
        ReasoningCarrier::ResponsesReasoningItem
    );
    assert_eq!(
        profile_only.field_sources.get("token_limit_field"),
        Some(&CapabilitySource::Probe)
    );

    let continuation_only = CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &requested))
        .unwrap();
    assert_eq!(
        continuation_only.reasoning_carrier,
        ReasoningCarrier::ReasoningContent
    );
    assert_eq!(
        continuation_only.field_sources.get("reasoning_carrier"),
        Some(&CapabilitySource::Baseline)
    );
    assert_eq!(continuation_only.token_limit_field, TokenLimitField::Omit);
}

#[test]
fn omit_sampling_fields_source_tracks_policy_presence() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures::text_stream();

    let baseline = CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &requested))
        .unwrap();
    assert_eq!(
        baseline.field_sources.get("omit_sampling_fields"),
        Some(&CapabilitySource::Baseline)
    );

    let semantic = SemanticPolicy {
        omit_sampling_fields: BTreeSet::from(["temperature".to_owned()]),
        ..Default::default()
    };
    let policy = CapabilityResolver
        .resolve(ResolutionInput {
            semantic: &semantic,
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap();
    assert_eq!(policy.omit_sampling_fields, semantic.omit_sampling_fields);
    assert_eq!(
        policy.field_sources.get("omit_sampling_fields"),
        Some(&CapabilitySource::Policy)
    );
}

#[test]
fn effort_map_uses_first_ordered_control_with_accepted_upstream_values() {
    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures::text_stream();
    let semantic = SemanticPolicy {
        effort_map: BTreeMap::from([
            ("low".to_owned(), "tiny".to_owned()),
            ("medium".to_owned(), "balanced".to_owned()),
            ("high".to_owned(), "maximum".to_owned()),
        ]),
        ..Default::default()
    };
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile.reasoning_controls = BTreeMap::from([
        (
            "a_control".to_owned(),
            vec!["balanced".to_owned(), "unsupported".to_owned()],
        ),
        ("z_control".to_owned(), vec!["tiny".to_owned()]),
    ]);

    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &[],
            policy_extensions: &[],
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();

    assert_eq!(
        resolved.reasoning_control_field.as_deref(),
        Some("a_control")
    );
    assert_eq!(
        resolved.effort_map,
        BTreeMap::from([("medium".to_owned(), "balanced".to_owned())])
    );
    assert_eq!(
        resolved.field_sources.get("effort_map"),
        Some(&CapabilitySource::Probe)
    );

    profile.reasoning_controls =
        BTreeMap::from([("control".to_owned(), vec!["not-mapped".to_owned()])]);
    let resolved = CapabilityResolver
        .resolve(ResolutionInput {
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap();
    assert_eq!(resolved.reasoning_control_field, None);
    assert!(resolved.effort_map.is_empty());
    assert_eq!(
        resolved.field_sources.get("effort_map"),
        Some(&CapabilitySource::Baseline)
    );
}

#[test]
fn required_reasoning_needs_a_protocol_compatible_carrier() {
    for (protocol, correct_carrier, wrong_carrier) in [
        (
            WireProtocol::ChatCompletions,
            ReasoningCarrier::ReasoningContent,
            ReasoningCarrier::ResponsesReasoningItem,
        ),
        (
            WireProtocol::Responses,
            ReasoningCarrier::ResponsesReasoningItem,
            ReasoningCarrier::MessagesThinking,
        ),
        (
            WireProtocol::Messages,
            ReasoningCarrier::MessagesThinking,
            ReasoningCarrier::ReasoningContent,
        ),
    ] {
        let route = route(protocol);
        let mut explicit_output = RequestedFeatures::text_stream();
        explicit_output.required.insert(Capability::ReasoningOutput);
        let mut explicit_replay = RequestedFeatures::text_stream();
        explicit_replay.required.insert(Capability::ReasoningReplay);
        let cases = [
            (
                "explicit-output",
                explicit_output,
                SemanticPolicy::default(),
                Capability::ReasoningOutput,
            ),
            (
                "explicit-replay",
                explicit_replay,
                SemanticPolicy::default(),
                Capability::ReasoningReplay,
            ),
            (
                "semantic-fixed-on",
                RequestedFeatures::text_stream(),
                SemanticPolicy {
                    reasoning_mode: Some(ReasoningMode::FixedOn),
                    ..Default::default()
                },
                Capability::ReasoningOutput,
            ),
            (
                "semantic-replay",
                RequestedFeatures::text_stream(),
                SemanticPolicy {
                    reasoning_replay_required: Some(true),
                    ..Default::default()
                },
                Capability::ReasoningOutput,
            ),
        ];

        for (case_name, requested, semantic, expected_capability) in cases {
            let mut profile =
                UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
            profile.capabilities = BTreeMap::from([
                (Capability::ReasoningOutput, EvidenceState::Supported),
                (Capability::ReasoningReplay, EvidenceState::Supported),
            ]);

            let missing = CapabilityResolver
                .resolve(ResolutionInput {
                    route: &route,
                    requested: &requested,
                    semantic: &semantic,
                    route_overrides: &[],
                    policy_extensions: &[],
                    profile: Some(&profile),
                    strip_nonstandard_chat_fields: false,
                })
                .unwrap_err();
            assert_eq!(
                missing.capability, expected_capability,
                "{protocol:?}/{case_name}"
            );
            assert_eq!(
                missing.category(),
                "gateway_protocol_capability_unsupported"
            );

            profile.reasoning_carrier = Some(wrong_carrier);
            let wrong = CapabilityResolver
                .resolve(ResolutionInput {
                    route: &route,
                    requested: &requested,
                    semantic: &semantic,
                    route_overrides: &[],
                    policy_extensions: &[],
                    profile: Some(&profile),
                    strip_nonstandard_chat_fields: false,
                })
                .unwrap_err();
            assert_eq!(
                wrong.capability, expected_capability,
                "{protocol:?}/{case_name}"
            );

            profile.reasoning_carrier = Some(correct_carrier);
            CapabilityResolver
                .resolve(ResolutionInput {
                    route: &route,
                    requested: &requested,
                    semantic: &semantic,
                    route_overrides: &[],
                    policy_extensions: &[],
                    profile: Some(&profile),
                    strip_nonstandard_chat_fields: false,
                })
                .unwrap_or_else(|error| panic!("{protocol:?}/{case_name}: {error}"));
        }
    }

    let route = route(WireProtocol::ChatCompletions);
    let requested = RequestedFeatures {
        optional: BTreeSet::from([Capability::ReasoningOutput]),
        ..RequestedFeatures::text_stream()
    };
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    let optional = CapabilityResolver
        .resolve(ResolutionInput {
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &requested)
        })
        .expect("optional reasoning retains downgrade semantics without a carrier");
    assert!(optional.supports(Capability::ReasoningOutput));
}

#[test]
fn only_exact_chat_continuation_can_supply_a_required_reasoning_carrier() {
    let route = route(WireProtocol::ChatCompletions);
    let matching = RequestedFeatures {
        required: BTreeSet::from([Capability::ReasoningReplay]),
        continuation_profile: Some(DialectProfileKey::from_route(&route)),
        continuation_reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
        ..RequestedFeatures::text_stream()
    };
    CapabilityResolver
        .resolve(ResolutionInput::baseline(&route, &matching))
        .expect("an exact Chat continuation preserves its reasoning carrier");

    let foreign = RequestedFeatures {
        required: BTreeSet::from([Capability::ReasoningReplay]),
        continuation_profile: Some(DialectProfileKey {
            upstream_id: "another-relay".to_owned(),
            runtime_model_slug: route.runtime_model_slug.clone(),
            protocol: route.protocol,
        }),
        continuation_reasoning_carrier: Some(ReasoningCarrier::ReasoningContent),
        ..RequestedFeatures::text_stream()
    };
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::ReasoningReplay, EvidenceState::Supported);

    let error = CapabilityResolver
        .resolve(ResolutionInput {
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &foreign)
        })
        .unwrap_err();
    assert_eq!(error.capability, Capability::ReasoningReplay);
    assert_eq!(error.category(), "gateway_protocol_capability_unsupported");
}

#[test]
fn profile_fidelity_controls_provisional_and_native_preference() {
    for (protocol, expectations) in [
        (
            WireProtocol::ChatCompletions,
            [
                (DialectProfileState::Verified, false, true),
                (DialectProfileState::Partial, false, true),
                (DialectProfileState::Unsupported, false, false),
                (DialectProfileState::Unknown, true, true),
            ],
        ),
        (
            WireProtocol::Responses,
            [
                (DialectProfileState::Verified, false, true),
                (DialectProfileState::Partial, false, false),
                (DialectProfileState::Unsupported, false, false),
                (DialectProfileState::Unknown, true, false),
            ],
        ),
    ] {
        let route = route(protocol);
        let requested = RequestedFeatures::text_stream();

        for (state, provisional, native_preferred) in expectations {
            let mut profile =
                UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
            profile.state = state;
            let resolved = CapabilityResolver
                .resolve(ResolutionInput {
                    profile: Some(&profile),
                    ..ResolutionInput::baseline(&route, &requested)
                })
                .unwrap();

            assert_eq!(resolved.profile_state, state, "{protocol:?}/{state:?}");
            assert_eq!(resolved.provisional, provisional, "{protocol:?}/{state:?}");
            assert_eq!(
                resolved.native_preferred, native_preferred,
                "{protocol:?}/{state:?}"
            );
            assert!(resolved.supports(Capability::FunctionTools));
        }

        let no_profile = CapabilityResolver
            .resolve(ResolutionInput::baseline(&route, &requested))
            .unwrap();
        assert_eq!(no_profile.profile_state, DialectProfileState::Unknown);
        assert!(no_profile.provisional);
        assert_eq!(
            no_profile.native_preferred,
            protocol == WireProtocol::ChatCompletions
        );

        let mut foreign = UpstreamDialectProfile::unknown(DialectProfileKey {
            upstream_id: "foreign-relay".to_owned(),
            runtime_model_slug: route.runtime_model_slug.clone(),
            protocol: route.protocol,
        });
        foreign.state = DialectProfileState::Verified;
        let foreign_resolved = CapabilityResolver
            .resolve(ResolutionInput {
                profile: Some(&foreign),
                ..ResolutionInput::baseline(&route, &requested)
            })
            .unwrap();
        assert_eq!(foreign_resolved.profile_state, DialectProfileState::Unknown);
        assert!(foreign_resolved.provisional);
        assert_eq!(
            foreign_resolved.native_preferred,
            protocol == WireProtocol::ChatCompletions
        );
    }
}

#[test]
fn unobserved_capability_evidence_does_not_erase_conclusive_evidence() {
    let route = route(WireProtocol::Responses);
    let requested = RequestedFeatures::text_stream();
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Supported);
    let supported_override = RouteCapabilityOverride {
        id: "supported".to_owned(),
        capabilities: BTreeMap::from([(Capability::ImageHttps, EvidenceState::Supported)]),
        ..Default::default()
    };
    let rejected_override = RouteCapabilityOverride {
        id: "rejected".to_owned(),
        capabilities: BTreeMap::from([(Capability::ImageHttps, EvidenceState::Rejected)]),
        ..Default::default()
    };
    let unobserved_override = RouteCapabilityOverride {
        id: "unobserved".to_owned(),
        capabilities: BTreeMap::from([(Capability::ImageHttps, EvidenceState::Unobserved)]),
        ..Default::default()
    };
    let profile_then_unobserved = [&unobserved_override];

    let profile_supported = CapabilityResolver
        .resolve(ResolutionInput {
            route_overrides: &profile_then_unobserved,
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap();
    assert_eq!(
        profile_supported.state(Capability::ImageHttps),
        EvidenceState::Supported
    );
    assert_eq!(
        profile_supported.source(Capability::ImageHttps),
        CapabilitySource::Probe
    );

    profile
        .capabilities
        .insert(Capability::ImageHttps, EvidenceState::Rejected);
    let profile_rejected = CapabilityResolver
        .resolve(ResolutionInput {
            route_overrides: &profile_then_unobserved,
            profile: Some(&profile),
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap();
    assert_eq!(
        profile_rejected.state(Capability::ImageHttps),
        EvidenceState::Rejected
    );
    assert_eq!(
        profile_rejected.source(Capability::ImageHttps),
        CapabilitySource::Probe
    );

    for (conclusive, expected) in [
        (&supported_override, EvidenceState::Supported),
        (&rejected_override, EvidenceState::Rejected),
    ] {
        let overrides = [conclusive, &unobserved_override];
        let resolved = CapabilityResolver
            .resolve(ResolutionInput {
                route_overrides: &overrides,
                ..ResolutionInput::baseline(&route, &requested)
            })
            .unwrap();
        assert_eq!(resolved.state(Capability::ImageHttps), expected);
        assert_eq!(
            resolved.source(Capability::ImageHttps),
            CapabilitySource::Override
        );
    }

    let only_unobserved = [&unobserved_override];
    let baseline = CapabilityResolver
        .resolve(ResolutionInput {
            route_overrides: &only_unobserved,
            ..ResolutionInput::baseline(&route, &requested)
        })
        .unwrap();
    assert_eq!(
        baseline.state(Capability::ImageHttps),
        EvidenceState::Unobserved
    );
    assert_eq!(
        baseline.source(Capability::ImageHttps),
        CapabilitySource::Baseline
    );
}

#[test]
fn unobserved_extension_evidence_does_not_erase_conclusive_evidence() {
    let route = route(WireProtocol::Responses);
    let requested = RequestedFeatures::text_stream();
    let semantic = SemanticPolicy::default();
    let case = extension("eligible", WireProtocol::Responses, BTreeSet::new());
    let cases = [&case];
    let supported_override = RouteCapabilityOverride {
        id: "supported".to_owned(),
        extensions: BTreeMap::from([("eligible".to_owned(), EvidenceState::Supported)]),
        ..Default::default()
    };
    let rejected_override = RouteCapabilityOverride {
        id: "rejected".to_owned(),
        extensions: BTreeMap::from([("eligible".to_owned(), EvidenceState::Rejected)]),
        ..Default::default()
    };
    let unobserved_override = RouteCapabilityOverride {
        id: "unobserved".to_owned(),
        extensions: BTreeMap::from([("eligible".to_owned(), EvidenceState::Unobserved)]),
        ..Default::default()
    };
    let profile_then_unobserved = [&unobserved_override];
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile
        .extension_evidence
        .insert("eligible".to_owned(), EvidenceState::Supported);

    let profile_supported = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &profile_then_unobserved,
            policy_extensions: &cases,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert_eq!(profile_supported.request_extensions.len(), 1);
    assert_eq!(
        profile_supported.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Probe)
    );

    profile
        .extension_evidence
        .insert("eligible".to_owned(), EvidenceState::Rejected);
    let profile_rejected = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &profile_then_unobserved,
            policy_extensions: &cases,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert!(profile_rejected.request_extensions.is_empty());
    assert_eq!(
        profile_rejected.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Probe)
    );

    for (conclusive, expected_count) in [(&supported_override, 1), (&rejected_override, 0)] {
        let overrides = [conclusive, &unobserved_override];
        let resolved = CapabilityResolver
            .resolve(ResolutionInput {
                route: &route,
                requested: &requested,
                semantic: &semantic,
                route_overrides: &overrides,
                policy_extensions: &cases,
                profile: None,
                strip_nonstandard_chat_fields: false,
            })
            .unwrap();
        assert_eq!(resolved.request_extensions.len(), expected_count);
        assert_eq!(
            resolved.field_sources.get("request_extensions"),
            Some(&CapabilitySource::Override)
        );
    }

    profile
        .extension_evidence
        .insert("eligible".to_owned(), EvidenceState::Unobserved);
    let only_unobserved = [&unobserved_override];
    let baseline = CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &requested,
            semantic: &semantic,
            route_overrides: &only_unobserved,
            policy_extensions: &cases,
            profile: Some(&profile),
            strip_nonstandard_chat_fields: false,
        })
        .unwrap();
    assert!(baseline.request_extensions.is_empty());
    assert_eq!(
        baseline.field_sources.get("request_extensions"),
        Some(&CapabilitySource::Baseline)
    );
}
