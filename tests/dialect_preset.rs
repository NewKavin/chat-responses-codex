use std::collections::{BTreeMap, BTreeSet};

use chat_responses_codex::capabilities::{
    compile_dialect_preset, Capability, CapabilityResolver, CapabilitySource, DialectProfileKey,
    DialectProfileState, EvidenceState, ReasoningCarrier, ResolutionInput, ResolvedCapabilities,
    UpstreamDialectProfile, WireProtocol,
};
use chat_responses_codex::state::NonstandardFieldPolicy;
use serde_json::Value;

fn route(protocol: WireProtocol) -> chat_responses_codex::capabilities::RouteIdentity {
    chat_responses_codex::capabilities::RouteIdentity {
        upstream_id: "up-1".into(),
        key_fingerprint: "fp".into(),
        exposed_model_slug: "opaque/model".into(),
        runtime_model_slug: "opaque/model".into(),
        protocol,
        tags: BTreeSet::new(),
    }
}

fn resolve_with_preset(
    preset: Option<&str>,
    profile: Option<&UpstreamDialectProfile>,
) -> ResolvedCapabilities {
    let route = route(WireProtocol::ChatCompletions);
    CapabilityResolver
        .resolve(ResolutionInput {
            route: &route,
            requested: &chat_responses_codex::capabilities::RequestedFeatures::text_stream(),
            semantic: &Default::default(),
            route_overrides: &[],
            policy_extensions: &[],
            profile,
            dialect_preset: preset,
            strip_nonstandard_chat_fields: NonstandardFieldPolicy::Auto,
        })
        .unwrap()
}

#[test]
fn deepseek_preset_is_reasoning_content_carrier_with_verbatim_effort_passthrough() {
    let preset = compile_dialect_preset("deepseek").expect("deepseek preset compiles");
    assert_eq!(preset.reasoning_carrier, ReasoningCarrier::ReasoningContent);
    assert_eq!(
        preset.reasoning_control_field.as_deref(),
        Some("reasoning_effort")
    );
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        assert_eq!(
            preset.effort_map.get(effort),
            Some(&Value::String(effort.to_string())),
            "deepseek preset must pass {effort} through verbatim"
        );
    }
    assert!(!preset.omit_optional_extensions);
    assert_eq!(preset.profile_state, DialectProfileState::Partial);
    assert!(preset.supports(Capability::ReasoningOutput));
    assert!(preset.supports(Capability::ReasoningReplay));
}

#[test]
fn glm_preset_uses_object_valued_thinking_control_and_strips_stream_options() {
    let preset = compile_dialect_preset("glm").expect("glm preset compiles");
    assert_eq!(preset.reasoning_control_field.as_deref(), Some("thinking"));
    assert_eq!(
        preset.effort_map.get("low"),
        Some(&serde_json::json!({"type": "disabled"}))
    );
    assert_eq!(
        preset.effort_map.get("high"),
        Some(&serde_json::json!({"type": "enabled"}))
    );
    assert!(preset.omit_sampling_fields.contains("stream_options"));
    assert!(!preset.omit_optional_extensions);
    assert_eq!(preset.reasoning_carrier, ReasoningCarrier::ReasoningContent);
}

#[test]
fn generic_strict_preset_strips_everything() {
    let preset = compile_dialect_preset("generic-strict").expect("generic-strict preset compiles");
    assert!(preset.omit_optional_extensions);
    assert!(preset.omit_sampling_fields.contains("stream_options"));
    assert_eq!(preset.reasoning_carrier, ReasoningCarrier::None);
}

#[test]
fn openai_and_minimax_presets_are_neutral() {
    for name in ["openai", "minimax"] {
        let preset = compile_dialect_preset(name).expect("preset compiles");
        assert!(!preset.omit_optional_extensions, "{name} must not strip");
        assert!(
            preset.omit_sampling_fields.is_empty(),
            "{name} must not omit"
        );
        assert_eq!(preset.reasoning_carrier, ReasoningCarrier::None);
        assert!(preset.reasoning_control_field.is_none());
    }
}

#[test]
fn unknown_preset_does_not_compile() {
    assert!(compile_dialect_preset("unknown-vendor").is_none());
}

#[test]
fn deepseek_preset_applies_to_unprobed_route() {
    let resolved = resolve_with_preset(Some("deepseek"), None);
    assert_eq!(
        resolved.reasoning_carrier,
        ReasoningCarrier::ReasoningContent
    );
    assert_eq!(
        resolved.reasoning_control_field.as_deref(),
        Some("reasoning_effort")
    );
    assert!(!resolved.effort_map.is_empty());
    assert_eq!(resolved.profile_state, DialectProfileState::Partial);
    assert!(!resolved.provisional);
    assert!(!resolved.omit_optional_extensions);
    assert!(resolved.supports(Capability::ReasoningOutput));
    assert_eq!(
        resolved.field_sources.get("reasoning_carrier"),
        Some(&CapabilitySource::Policy)
    );
}

#[test]
fn verified_profile_wins_over_preset() {
    let route = route(WireProtocol::ChatCompletions);
    let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey::from_route(&route));
    profile.state = DialectProfileState::Verified;
    profile.reasoning_carrier = Some(ReasoningCarrier::None);
    profile
        .capabilities
        .insert(Capability::TextInput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::TextStream, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::NonStreamingResponse, EvidenceState::Supported);

    let resolved = resolve_with_preset(Some("deepseek"), Some(&profile));
    assert_eq!(resolved.profile_state, DialectProfileState::Verified);
    assert_eq!(resolved.reasoning_carrier, ReasoningCarrier::None);
    assert!(resolved.reasoning_control_field.is_none());
    assert!(resolved.effort_map.is_empty());
}

#[test]
fn glm_preset_resolves_thinking_control_with_object_values() {
    let resolved = resolve_with_preset(Some("glm"), None);
    assert_eq!(
        resolved.reasoning_control_field.as_deref(),
        Some("thinking")
    );
    assert_eq!(
        resolved.effort_map.get("high"),
        Some(&serde_json::json!({"type": "enabled"}))
    );
    assert!(resolved.omit_sampling_fields.contains("stream_options"));
}

#[test]
fn generic_strict_preset_resolves_omitted_extensions() {
    let resolved = resolve_with_preset(Some("generic-strict"), None);
    assert!(resolved.omit_optional_extensions);
    assert!(resolved.omit_sampling_fields.contains("stream_options"));
    assert_eq!(resolved.profile_state, DialectProfileState::Partial);
}

#[test]
fn unprobed_route_without_preset_remains_provisional() {
    let resolved = resolve_with_preset(None, None);
    assert!(resolved.provisional);
    assert_eq!(resolved.profile_state, DialectProfileState::Unknown);
    assert!(resolved.omit_optional_extensions);
}

#[test]
fn preset_effort_map_is_typed_as_json_values() {
    // Guard the object-valued effort_map contract used by the GLM preset.
    let preset = compile_dialect_preset("glm").unwrap();
    let map: BTreeMap<String, Value> = preset.effort_map;
    assert!(map.values().any(|value| value.is_object()));
}
