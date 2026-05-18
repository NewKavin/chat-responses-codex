use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamProtocol {
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRequest {
    pub model: String,
    pub protocol: UpstreamProtocol,
    pub stream: bool,
}

impl RouteRequest {
    pub fn new(model: impl Into<String>, protocol: UpstreamProtocol, stream: bool) -> Self {
        Self {
            model: model.into(),
            protocol,
            stream,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamCandidate {
    pub id: String,
    pub name: String,
    pub protocol: UpstreamProtocol,
    pub models: Vec<String>,
    pub failure_count: u32,
}

impl UpstreamCandidate {
    pub fn new(id: impl Into<String>, name: impl Into<String>, protocol: UpstreamProtocol) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            protocol,
            models: Vec::new(),
            failure_count: 0,
        }
    }

    pub fn with_models<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.models = models.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_failure_count(mut self, failure_count: u32) -> Self {
        self.failure_count = failure_count;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    ModelUnavailable(String),
    NoHealthyUpstream(String),
}

pub fn select_upstream(
    request: &RouteRequest,
    candidates: &[UpstreamCandidate],
) -> Result<UpstreamCandidate, RouteError> {
    let mut supported = candidates
        .iter()
        .filter(|candidate| {
            candidate.protocol == request.protocol
                && candidate.models.iter().any(|model| model == &request.model)
        })
        .cloned();

    if let Some(candidate) = supported.find(|candidate| candidate.failure_count < 3) {
        return Ok(candidate);
    }

    if candidates.iter().any(|candidate| {
        candidate.protocol == request.protocol
            && candidate.models.iter().any(|model| model == &request.model)
    }) {
        return Err(RouteError::NoHealthyUpstream(request.model.clone()));
    }

    Err(RouteError::ModelUnavailable(request.model.clone()))
}
