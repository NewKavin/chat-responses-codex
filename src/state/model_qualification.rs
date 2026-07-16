use crate::capabilities::{
    Capability, DialectProfileState, EvidenceState, UpstreamDialectProfile,
};
use crate::routing::UpstreamProtocol;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelQualificationLevel {
    Full,
    Adapted,
    Unusable,
    OperationalFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelQualificationCategory {
    Passed,
    Authentication,
    RateLimit,
    UpstreamUnavailable,
    RequestRejected,
    ModelNotFound,
    MalformedResponse,
    EmptyResponse,
    Timeout,
    Network,
}

impl ModelQualificationCategory {
    pub fn is_operational(self) -> bool {
        matches!(
            self,
            Self::Authentication
                | Self::RateLimit
                | Self::UpstreamUnavailable
                | Self::Timeout
                | Self::Network
        )
    }

    pub fn requires_confirmation(self) -> bool {
        matches!(
            self,
            Self::RequestRejected
                | Self::ModelNotFound
                | Self::MalformedResponse
                | Self::EmptyResponse
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelQualificationEvidence {
    pub upstream_id: String,
    pub key_prefix: String,
    pub model: String,
    pub protocol: UpstreamProtocol,
    pub level: ModelQualificationLevel,
    pub category: ModelQualificationCategory,
    pub latency_ms: u64,
    pub attempted_at: u64,
}

pub fn classify_qualification_level(
    category: ModelQualificationCategory,
    profile: Option<&UpstreamDialectProfile>,
) -> ModelQualificationLevel {
    if category.is_operational() {
        return ModelQualificationLevel::OperationalFailure;
    }
    if category != ModelQualificationCategory::Passed {
        return ModelQualificationLevel::Unusable;
    }

    let full = profile.is_some_and(|profile| {
        profile.state == DialectProfileState::Verified
            && [
                Capability::TextInput,
                Capability::TextStream,
                Capability::FunctionTools,
                Capability::ToolContinuation,
            ]
            .into_iter()
            .all(|capability| {
                profile.capabilities.get(&capability) == Some(&EvidenceState::Supported)
            })
    });

    if full {
        ModelQualificationLevel::Full
    } else {
        ModelQualificationLevel::Adapted
    }
}
