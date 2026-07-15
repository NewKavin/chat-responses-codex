use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    Capability, DialectCorrectionRule, DialectProfileState, EvidenceState, ReasoningCarrier,
    TokenLimitField, UpstreamDialectProfile, WireProtocol, DIALECT_PROBE_SCHEMA_VERSION,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RouteFingerprintInput {
    pub normalized_base_url: String,
    pub enabled_protocols: Vec<WireProtocol>,
    pub runtime_model_slug: String,
    pub route_override_digest: String,
    pub probe_schema_version: u32,
}

pub fn normalize_route_base_url(value: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(value.trim()).map_err(|error| error.to_string())?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let normalized_path = format!("/{}", url.path().trim_matches('/'));
    url.set_path(normalized_path.trim_end_matches('/'));
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

pub fn route_fingerprint(input: &RouteFingerprintInput) -> String {
    let mut canonical = input.clone();
    canonical.enabled_protocols.sort();
    canonical.enabled_protocols.dedup();
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&canonical).expect("serializable route fingerprint input")
        )
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeOutcome {
    Conclusive {
        capabilities: BTreeMap<Capability, EvidenceState>,
        token_limit_field: Option<TokenLimitField>,
        reasoning_carrier: Option<ReasoningCarrier>,
        reasoning_controls: BTreeMap<String, Vec<String>>,
        correction_rules: Vec<DialectCorrectionRule>,
        extension_evidence: BTreeMap<String, EvidenceState>,
        evidence_codes: BTreeSet<String>,
        event_types: BTreeSet<String>,
        http_status: u16,
        attempted_at: u64,
    },
    OperationalFailure {
        code: String,
        http_status: Option<u16>,
        attempted_at: u64,
    },
}

impl ProbeOutcome {
    pub fn capability(&self, capability: Capability) -> EvidenceState {
        match self {
            Self::Conclusive { capabilities, .. } => capabilities
                .get(&capability)
                .copied()
                .unwrap_or(EvidenceState::Unobserved),
            Self::OperationalFailure { .. } => EvidenceState::Unobserved,
        }
    }

    pub fn evidence_codes(&self) -> BTreeSet<String> {
        match self {
            Self::Conclusive { evidence_codes, .. } => evidence_codes.clone(),
            Self::OperationalFailure { code, .. } => [code.clone()].into_iter().collect(),
        }
    }
}

pub fn apply_probe_outcome(profile: &mut UpstreamDialectProfile, outcome: ProbeOutcome) {
    match outcome {
        ProbeOutcome::OperationalFailure {
            code,
            http_status,
            attempted_at,
        } => {
            profile.last_attempt_at = Some(attempted_at);
            profile.http_status = http_status;
            profile.last_operational_failure = Some(code);
        }
        ProbeOutcome::Conclusive {
            capabilities,
            token_limit_field,
            reasoning_carrier,
            reasoning_controls,
            correction_rules,
            extension_evidence,
            evidence_codes,
            event_types,
            http_status,
            attempted_at,
        } => {
            profile.capabilities = capabilities;
            profile.token_limit_field = token_limit_field;
            profile.reasoning_carrier = reasoning_carrier;
            profile.reasoning_controls = reasoning_controls;
            profile.correction_rules = correction_rules;
            profile.extension_evidence = extension_evidence;
            profile.evidence_codes = evidence_codes;
            profile.event_types = event_types;
            profile.http_status = Some(http_status);
            profile.last_attempt_at = Some(attempted_at);
            profile.last_success_at = Some(attempted_at);
            profile.last_operational_failure = None;
            let supported = profile
                .capabilities
                .values()
                .filter(|value| **value == EvidenceState::Supported)
                .count();
            let rejected = profile
                .capabilities
                .values()
                .filter(|value| **value == EvidenceState::Rejected)
                .count();
            profile.state = if supported == 0 && rejected > 0 {
                DialectProfileState::Unsupported
            } else if rejected == 0 {
                DialectProfileState::Verified
            } else {
                DialectProfileState::Partial
            };
        }
    }
}

pub fn profile_is_current(
    profile: &UpstreamDialectProfile,
    fingerprint: &str,
    now: u64,
    refresh_interval_seconds: u64,
) -> bool {
    profile.configuration_fingerprint == fingerprint
        && profile.probe_schema_version == DIALECT_PROBE_SCHEMA_VERSION
        && profile
            .last_success_at
            .map(|at| now.saturating_sub(at) < refresh_interval_seconds)
            .unwrap_or(false)
}
