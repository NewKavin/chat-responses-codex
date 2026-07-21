use chat_responses_codex::capabilities::*;
use chat_responses_codex::state::{
    classify_qualification_level, ModelQualificationCategory, ModelQualificationLevel,
};

fn verified_profile(
    upstream_id: &str,
    runtime_model_slug: &str,
    protocol: WireProtocol,
) -> UpstreamDialectProfile {
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: upstream_id.into(),
        runtime_model_slug: runtime_model_slug.into(),
        protocol,
    });
    profile.configuration_fingerprint = "test-fingerprint".into();
    profile.state = DialectProfileState::Verified;
    profile
}

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
        key_fingerprint: String::new(),
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

#[test]
fn direct_success_with_complete_agent_profile_is_full() {
    let mut profile = verified_profile("up", "opaque", WireProtocol::ChatCompletions);
    for capability in [
        Capability::TextInput,
        Capability::TextStream,
        Capability::FunctionTools,
        Capability::ToolContinuation,
    ] {
        profile
            .capabilities
            .insert(capability, EvidenceState::Supported);
    }
    assert_eq!(
        classify_qualification_level(ModelQualificationCategory::Passed, Some(&profile)),
        ModelQualificationLevel::Full,
    );
}

#[test]
fn usable_text_with_partial_profile_is_adapted_not_unusable() {
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
        key_fingerprint: String::new(),
        upstream_id: "up".into(),
        runtime_model_slug: "opaque".into(),
        protocol: WireProtocol::ChatCompletions,
    });
    profile.state = DialectProfileState::Partial;
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    assert_eq!(
        classify_qualification_level(ModelQualificationCategory::Passed, Some(&profile)),
        ModelQualificationLevel::Adapted,
    );
}

#[test]
fn transient_failures_never_classify_as_unusable() {
    for category in [
        ModelQualificationCategory::Authentication,
        ModelQualificationCategory::RateLimit,
        ModelQualificationCategory::UpstreamUnavailable,
        ModelQualificationCategory::Timeout,
        ModelQualificationCategory::Network,
    ] {
        assert_eq!(
            classify_qualification_level(category, None),
            ModelQualificationLevel::OperationalFailure,
        );
    }
}
