use chat_responses_codex::capabilities::*;

#[test]
fn route_fingerprint_changes_for_every_dialect_input() {
    let base = RouteFingerprintInput {
        normalized_base_url: "https://relay.example/v1".into(),
        enabled_protocols: vec![WireProtocol::ChatCompletions],
        runtime_model_slug: "Lab/Model".into(),
        route_override_digest: "override-a".into(),
        probe_schema_version: DIALECT_PROBE_SCHEMA_VERSION,
    };
    let original = route_fingerprint(&base);

    let mut changed = base.clone();
    changed.normalized_base_url = "https://relay-2.example/v1".into();
    assert_ne!(original, route_fingerprint(&changed));

    let mut changed = base.clone();
    changed.runtime_model_slug = "Lab/model".into();
    assert_ne!(original, route_fingerprint(&changed));

    let mut changed = base.clone();
    changed.enabled_protocols.push(WireProtocol::Responses);
    assert_ne!(original, route_fingerprint(&changed));

    let mut changed = base.clone();
    changed.route_override_digest = "override-b".into();
    assert_ne!(original, route_fingerprint(&changed));

    let mut changed = base;
    changed.probe_schema_version += 1;
    assert_ne!(original, route_fingerprint(&changed));
}

#[test]
fn operational_probe_failure_does_not_erase_verified_evidence() {
    let key = DialectProfileKey {
        upstream_id: "u".into(),
        runtime_model_slug: "m".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let mut profile = UpstreamDialectProfile::unknown(key);
    profile.state = DialectProfileState::Verified;
    profile
        .capabilities
        .insert(Capability::FunctionTools, EvidenceState::Supported);

    apply_probe_outcome(
        &mut profile,
        ProbeOutcome::OperationalFailure {
            code: "upstream_authentication".into(),
            http_status: Some(401),
            attempted_at: 99,
        },
    );

    assert_eq!(profile.state, DialectProfileState::Verified);
    assert_eq!(
        profile.capabilities[&Capability::FunctionTools],
        EvidenceState::Supported
    );
    assert_eq!(profile.last_attempt_at, Some(99));
}
