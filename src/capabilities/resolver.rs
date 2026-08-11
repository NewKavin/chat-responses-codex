use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use super::types::{
    baseline_capabilities, compile_dialect_preset, Capability, CapabilitySource,
    DeclarativeProbeCase, DialectCorrectionRule, DialectProfileKey, DialectProfileState,
    EvidenceState, ReasoningCarrier, ReasoningMode, RequestedFeatures, ResolvedCapabilities,
    ResolvedCapability, ResolvedRequestExtension, RouteCapabilityOverride, RouteIdentity,
    SemanticPolicy, TokenLimitField, UpstreamDialectProfile, WireProtocol,
};

static EMPTY_SEMANTIC_POLICY: SemanticPolicy = SemanticPolicy {
    reasoning_mode: None,
    reasoning_replay_required: None,
    effort_map: BTreeMap::new(),
    context_window: None,
    max_output_tokens: None,
    omit_sampling_fields: BTreeSet::new(),
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityResolutionError {
    pub capability: Capability,
}

impl CapabilityResolutionError {
    pub fn category(&self) -> &'static str {
        "gateway_protocol_capability_unsupported"
    }
}

impl fmt::Display for CapabilityResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "route cannot preserve required capability {:?}",
            self.capability
        )
    }
}

impl Error for CapabilityResolutionError {}

#[derive(Clone, Copy, Debug)]
pub struct ResolutionInput<'a> {
    pub route: &'a RouteIdentity,
    pub requested: &'a RequestedFeatures,
    pub semantic: &'a SemanticPolicy,
    pub route_overrides: &'a [&'a RouteCapabilityOverride],
    pub policy_extensions: &'a [&'a DeclarativeProbeCase],
    pub profile: Option<&'a UpstreamDialectProfile>,
    pub dialect_preset: Option<&'a str>,
    pub strip_nonstandard_chat_fields: crate::state::NonstandardFieldPolicy,
}

impl<'a> ResolutionInput<'a> {
    pub fn baseline(route: &'a RouteIdentity, requested: &'a RequestedFeatures) -> Self {
        Self {
            route,
            requested,
            semantic: &EMPTY_SEMANTIC_POLICY,
            route_overrides: &[],
            policy_extensions: &[],
            profile: None,
            dialect_preset: None,
            strip_nonstandard_chat_fields: crate::state::NonstandardFieldPolicy::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CapabilityResolver;

impl CapabilityResolver {
    pub fn resolve(
        &self,
        input: ResolutionInput<'_>,
    ) -> Result<ResolvedCapabilities, CapabilityResolutionError> {
        let profile_key = DialectProfileKey::from_route(input.route);
        let profile = input.profile.filter(|profile| profile.key == profile_key);
        let input = ResolutionInput { profile, ..input };
        // A static dialect preset is a fallback below probe evidence: it only
        // applies when the route has no usable probe profile.
        let preset = input.dialect_preset.and_then(compile_dialect_preset);
        let uses_preset = profile.is_none() && preset.is_some();
        let mut values = baseline_capabilities();
        let continuation_carrier = matching_continuation_carrier(&input);

        if continuation_carrier == Some(ReasoningCarrier::ReasoningContent) {
            for capability in [Capability::ReasoningOutput, Capability::ReasoningReplay] {
                values.insert(
                    capability,
                    ResolvedCapability {
                        state: EvidenceState::Supported,
                        source: CapabilitySource::Baseline,
                    },
                );
            }
        }

        if let Some(profile) = input.profile {
            for (&capability, &state) in &profile.capabilities {
                if state != EvidenceState::Unobserved {
                    values.insert(
                        capability,
                        ResolvedCapability {
                            state,
                            source: CapabilitySource::Probe,
                        },
                    );
                }
            }
        } else if let Some(preset) = preset.as_ref() {
            for (&capability, resolved) in &preset.values {
                if resolved.state != EvidenceState::Unobserved {
                    values.insert(capability, *resolved);
                }
            }
        }

        for route_override in input.route_overrides {
            for (&capability, &state) in &route_override.capabilities {
                if state != EvidenceState::Unobserved {
                    values.insert(
                        capability,
                        ResolvedCapability {
                            state,
                            source: CapabilitySource::Override,
                        },
                    );
                }
            }
        }

        let (reasoning_carrier, reasoning_carrier_source) =
            resolve_reasoning_carrier(&input, continuation_carrier, preset.as_ref());
        validate_required_capabilities(&input, &values, reasoning_carrier)?;

        let (token_limit_field, token_limit_source) =
            resolve_token_limit_field(&input, preset.as_ref());
        let correction_rules = resolve_correction_rules(&input);
        let (reasoning_control_field, effort_map) = resolve_effort_control(&input, preset.as_ref());
        let effort_source = if effort_map.is_empty() {
            CapabilitySource::Baseline
        } else if uses_preset {
            CapabilitySource::Policy
        } else {
            CapabilitySource::Probe
        };
        let (request_extensions, request_extension_source) =
            resolve_extensions(&input, preset.as_ref());

        let reasoning_mode = input.semantic.reasoning_mode.unwrap_or(ReasoningMode::Off);
        let profile_state = if let Some(profile) = input.profile {
            profile.state
        } else if uses_preset {
            DialectProfileState::Partial
        } else {
            DialectProfileState::Unknown
        };
        let omit_optional_extensions = if uses_preset {
            preset
                .as_ref()
                .map(|preset| preset.omit_optional_extensions)
                .unwrap_or(false)
        } else {
            match input.strip_nonstandard_chat_fields {
                crate::state::NonstandardFieldPolicy::AlwaysStrip => true,
                crate::state::NonstandardFieldPolicy::Forward => false,
                crate::state::NonstandardFieldPolicy::Auto => {
                    profile_state == DialectProfileState::Unknown
                }
            }
        };
        let field_sources = BTreeMap::from([
            ("token_limit_field".to_owned(), token_limit_source),
            ("reasoning_carrier".to_owned(), reasoning_carrier_source),
            (
                "reasoning_mode".to_owned(),
                source_for_option(input.semantic.reasoning_mode.as_ref()),
            ),
            (
                "context_window".to_owned(),
                source_for_option(input.semantic.context_window.as_ref()),
            ),
            (
                "max_output_tokens".to_owned(),
                source_for_option(input.semantic.max_output_tokens.as_ref()),
            ),
            (
                "omit_sampling_fields".to_owned(),
                if input.semantic.omit_sampling_fields.is_empty() {
                    CapabilitySource::Baseline
                } else {
                    CapabilitySource::Policy
                },
            ),
            ("effort_map".to_owned(), effort_source),
            ("request_extensions".to_owned(), request_extension_source),
        ]);

        Ok(ResolvedCapabilities {
            values,
            token_limit_field,
            reasoning_mode,
            reasoning_carrier,
            correction_rules,
            reasoning_control_field,
            effort_map,
            omit_sampling_fields: if uses_preset {
                preset
                    .as_ref()
                    .map(|preset| preset.omit_sampling_fields.clone())
                    .unwrap_or_default()
            } else {
                input.semantic.omit_sampling_fields.clone()
            },
            context_window: input.semantic.context_window,
            max_output_tokens: input.semantic.max_output_tokens,
            omit_optional_extensions,
            profile_state,
            provisional: profile_state == DialectProfileState::Unknown,
            native_preferred: match profile_state {
                DialectProfileState::Verified => true,
                DialectProfileState::Unsupported => false,
                DialectProfileState::Partial | DialectProfileState::Unknown => {
                    input.route.protocol == WireProtocol::ChatCompletions
                }
            },
            adapters: BTreeSet::new(),
            request_extensions,
            field_sources,
        })
    }
}

fn resolve_correction_rules(input: &ResolutionInput<'_>) -> Vec<DialectCorrectionRule> {
    let mut rules = input
        .profile
        .map(|profile| profile.correction_rules.clone())
        .unwrap_or_default();
    for route_override in input.route_overrides {
        if !route_override.correction_rules.is_empty() {
            rules = route_override.correction_rules.clone();
        }
    }
    rules
}

fn matching_continuation_carrier(input: &ResolutionInput<'_>) -> Option<ReasoningCarrier> {
    let current_key = DialectProfileKey::from_route(input.route);
    (input.requested.continuation_profile.as_ref() == Some(&current_key))
        .then_some(input.requested.continuation_reasoning_carrier)
        .flatten()
}

fn validate_required_capabilities(
    input: &ResolutionInput<'_>,
    values: &BTreeMap<Capability, ResolvedCapability>,
    reasoning_carrier: ReasoningCarrier,
) -> Result<(), CapabilityResolutionError> {
    let mut required = input.requested.required.clone();
    if input.semantic.reasoning_mode == Some(ReasoningMode::FixedOn) {
        required.insert(Capability::ReasoningOutput);
    }
    if input.semantic.reasoning_replay_required == Some(true) {
        required.insert(Capability::ReasoningOutput);
        required.insert(Capability::ReasoningReplay);
    }

    for capability in required {
        let supported = values
            .get(&capability)
            .map(|resolved| resolved.state == EvidenceState::Supported)
            .unwrap_or(false);
        let carrier_supported =
            !matches!(
                capability,
                Capability::ReasoningOutput | Capability::ReasoningReplay
            ) || reasoning_carrier_matches_protocol(reasoning_carrier, input.route.protocol);
        if !supported || !carrier_supported {
            return Err(CapabilityResolutionError { capability });
        }
    }
    Ok(())
}

fn reasoning_carrier_matches_protocol(carrier: ReasoningCarrier, protocol: WireProtocol) -> bool {
    matches!(
        (protocol, carrier),
        (
            WireProtocol::ChatCompletions,
            ReasoningCarrier::ReasoningContent
        ) | (
            WireProtocol::Responses,
            ReasoningCarrier::ResponsesReasoningItem
        ) | (WireProtocol::Messages, ReasoningCarrier::MessagesThinking)
    )
}

fn resolve_token_limit_field(
    input: &ResolutionInput<'_>,
    preset: Option<&ResolvedCapabilities>,
) -> (TokenLimitField, CapabilitySource) {
    let mut value = TokenLimitField::Omit;
    let mut source = CapabilitySource::Baseline;

    if let Some(profile_value) = input.profile.and_then(|profile| profile.token_limit_field) {
        value = profile_value;
        source = CapabilitySource::Probe;
    } else if let Some(preset) = preset {
        value = preset.token_limit_field;
        source = CapabilitySource::Policy;
    }
    for route_override in input.route_overrides {
        if let Some(override_value) = route_override.token_limit_field {
            value = override_value;
            source = CapabilitySource::Override;
        }
    }

    (value, source)
}

fn resolve_reasoning_carrier(
    input: &ResolutionInput<'_>,
    continuation_carrier: Option<ReasoningCarrier>,
    preset: Option<&ResolvedCapabilities>,
) -> (ReasoningCarrier, CapabilitySource) {
    let mut value = continuation_carrier.unwrap_or(ReasoningCarrier::None);
    let mut source = CapabilitySource::Baseline;

    if let Some(profile_value) = input.profile.and_then(|profile| profile.reasoning_carrier) {
        value = profile_value;
        source = CapabilitySource::Probe;
    } else if let Some(preset) = preset {
        value = preset.reasoning_carrier;
        source = CapabilitySource::Policy;
    }
    for route_override in input.route_overrides {
        if let Some(override_value) = route_override.reasoning_carrier {
            value = override_value;
            source = CapabilitySource::Override;
        }
    }

    (value, source)
}

fn resolve_effort_control(
    input: &ResolutionInput<'_>,
    preset: Option<&ResolvedCapabilities>,
) -> (Option<String>, BTreeMap<String, serde_json::Value>) {
    let Some(profile) = input.profile else {
        if let Some(preset) = preset {
            return (
                preset.reasoning_control_field.clone(),
                preset.effort_map.clone(),
            );
        }
        return (None, BTreeMap::new());
    };

    for (field, accepted_values) in &profile.reasoning_controls {
        let filtered = input
            .semantic
            .effort_map
            .iter()
            .filter(|(_, upstream_value)| {
                accepted_values
                    .iter()
                    .any(|accepted| accepted.as_str() == Some(upstream_value.as_str()))
            })
            .map(|(requested_value, upstream_value)| {
                (
                    requested_value.clone(),
                    serde_json::Value::String(upstream_value.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if !filtered.is_empty() {
            return (Some(field.clone()), filtered);
        }
    }

    (None, BTreeMap::new())
}

fn resolve_extensions(
    input: &ResolutionInput<'_>,
    preset: Option<&ResolvedCapabilities>,
) -> (Vec<ResolvedRequestExtension>, CapabilitySource) {
    if let Some(preset) = preset {
        return (preset.request_extensions.clone(), CapabilitySource::Policy);
    }
    let extensions_are_eligible = match input.strip_nonstandard_chat_fields {
        crate::state::NonstandardFieldPolicy::AlwaysStrip => false,
        crate::state::NonstandardFieldPolicy::Forward => true,
        crate::state::NonstandardFieldPolicy::Auto => input.profile.is_some(),
    };
    if !extensions_are_eligible {
        return (Vec::new(), CapabilitySource::Baseline);
    }

    let mut resolved = Vec::new();
    let mut resolution_source = CapabilitySource::Baseline;
    for extension in input.policy_extensions {
        if extension.protocol != input.route.protocol
            || !extension.prerequisites.iter().all(|prerequisite| {
                input.requested.required.contains(prerequisite)
                    || input.requested.optional.contains(prerequisite)
            })
        {
            continue;
        }

        let mut evidence = EvidenceState::Unobserved;
        let mut evidence_source = CapabilitySource::Baseline;
        if let Some(profile_evidence) = input
            .profile
            .and_then(|profile| profile.extension_evidence.get(&extension.id))
        {
            if *profile_evidence != EvidenceState::Unobserved {
                evidence = *profile_evidence;
                evidence_source = CapabilitySource::Probe;
            }
        }
        for route_override in input.route_overrides {
            if let Some(override_evidence) = route_override.extensions.get(&extension.id) {
                if *override_evidence != EvidenceState::Unobserved {
                    evidence = *override_evidence;
                    evidence_source = CapabilitySource::Override;
                }
            }
        }

        resolution_source = stronger_evidence_source(resolution_source, evidence_source);

        if evidence == EvidenceState::Supported {
            resolved.push(ResolvedRequestExtension {
                id: extension.id.clone(),
                request_patch: extension.request_patch.clone(),
            });
        }
    }

    (resolved, resolution_source)
}

fn stronger_evidence_source(
    current: CapabilitySource,
    candidate: CapabilitySource,
) -> CapabilitySource {
    match (current, candidate) {
        (_, CapabilitySource::Override) => CapabilitySource::Override,
        (CapabilitySource::Baseline, CapabilitySource::Probe) => CapabilitySource::Probe,
        _ => current,
    }
}

fn source_for_option<T>(value: Option<&T>) -> CapabilitySource {
    if value.is_some() {
        CapabilitySource::Policy
    } else {
        CapabilitySource::Baseline
    }
}
