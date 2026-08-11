use chat_responses_codex::capabilities::{CapabilityResolver, RequestedFeatures, ResolutionInput};
use chat_responses_codex::capabilities::{
    DialectProfileKey, DialectProfileState, EvidenceState, RouteIdentity, UpstreamDialectProfile,
    WireProtocol,
};
use chat_responses_codex::state::{NonstandardFieldPolicy, UpstreamConfig};

#[test]
fn legacy_boolean_deserializes_to_auto_and_always_strip() {
    let auto: NonstandardFieldPolicy = serde_json::from_value(serde_json::json!(false)).unwrap();
    assert_eq!(auto, NonstandardFieldPolicy::Auto);
    let strip: NonstandardFieldPolicy = serde_json::from_value(serde_json::json!(true)).unwrap();
    assert_eq!(strip, NonstandardFieldPolicy::AlwaysStrip);
}

#[test]
fn string_forms_deserialize_to_all_three_policies() {
    for (raw, expected) in [
        ("auto", NonstandardFieldPolicy::Auto),
        ("always_strip", NonstandardFieldPolicy::AlwaysStrip),
        ("forward", NonstandardFieldPolicy::Forward),
    ] {
        let value: NonstandardFieldPolicy = serde_json::from_value(serde_json::json!(raw)).unwrap();
        assert_eq!(value, expected, "unexpected policy for {raw:?}");
    }
}

#[test]
fn unknown_policy_string_is_rejected() {
    let result: Result<NonstandardFieldPolicy, _> =
        serde_json::from_value(serde_json::json!("strip-everything"));
    assert!(result.is_err());
}

#[test]
fn upstream_config_round_trips_all_three_policies() {
    for policy in [
        NonstandardFieldPolicy::Auto,
        NonstandardFieldPolicy::AlwaysStrip,
        NonstandardFieldPolicy::Forward,
    ] {
        let upstream = UpstreamConfig {
            id: "up-1".into(),
            name: "up-1".into(),
            base_url: "https://example.test/v1".into(),
            api_key: "secret".into(),
            strip_nonstandard_chat_fields: policy,
            ..UpstreamConfig::default()
        };
        let json = serde_json::to_string(&upstream).unwrap();
        let decoded: UpstreamConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.strip_nonstandard_chat_fields, policy);
    }
}

#[test]
fn legacy_boolean_upstream_config_decodes_to_policy() {
    let json = r#"{
        "id": "up-1",
        "name": "up-1",
        "base_url": "https://example.test/v1",
        "api_key": "secret",
        "protocol": "ChatCompletions",
        "supported_models": [],
        "strip_nonstandard_chat_fields": true
    }"#;
    let decoded: UpstreamConfig = serde_json::from_str(json).unwrap();
    assert_eq!(
        decoded.strip_nonstandard_chat_fields,
        NonstandardFieldPolicy::AlwaysStrip
    );
}

fn route(protocol: WireProtocol) -> RouteIdentity {
    RouteIdentity {
        key_fingerprint: "fp".into(),
        upstream_id: "up-1".into(),
        exposed_model_slug: "public-model".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol,
        tags: Default::default(),
    }
}

fn profile(protocol: WireProtocol) -> UpstreamDialectProfile {
    UpstreamDialectProfile {
        key: DialectProfileKey::for_key(
            "up-1",
            "fp".to_string(),
            "opaque/model".to_string(),
            protocol,
        ),
        state: DialectProfileState::Verified,
        capabilities: std::collections::BTreeMap::from([(
            chat_responses_codex::capabilities::Capability::ParallelToolCalls,
            EvidenceState::Supported,
        )]),
        ..UpstreamDialectProfile::unknown(DialectProfileKey::for_key(
            "up-1",
            "fp".to_string(),
            "opaque/model".to_string(),
            protocol,
        ))
    }
}

fn resolve(
    policy: NonstandardFieldPolicy,
    with_profile: bool,
) -> chat_responses_codex::capabilities::ResolvedCapabilities {
    let route = route(WireProtocol::ChatCompletions);
    let profile = with_profile.then(|| profile(WireProtocol::ChatCompletions));
    CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &RequestedFeatures::default(),
            semantic: &Default::default(),
            route_overrides: &[],
            policy_extensions: &[],
            profile: profile.as_ref(),
            strip_nonstandard_chat_fields: policy,
        })
        .unwrap()
}

#[test]
fn auto_strips_optional_extensions_on_unprobed_routes() {
    let unprobed = resolve(NonstandardFieldPolicy::Auto, false);
    assert!(
        unprobed.omit_optional_extensions,
        "Auto + no profile must strip"
    );

    let probed = resolve(NonstandardFieldPolicy::Auto, true);
    assert!(
        !probed.omit_optional_extensions,
        "Auto + verified profile must trust the profile"
    );
}

#[test]
fn always_strip_overrides_a_verified_profile() {
    let resolved = resolve(NonstandardFieldPolicy::AlwaysStrip, true);
    assert!(resolved.omit_optional_extensions);
}

#[test]
fn forward_never_strips_purely_by_policy() {
    let resolved = resolve(NonstandardFieldPolicy::Forward, false);
    assert!(!resolved.omit_optional_extensions);
}
