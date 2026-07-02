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
    pub priority: u32,
    pub premium_models: Vec<String>,
    pub protect_premium_quota: bool,
    pub failure_count: u32,
}

impl UpstreamCandidate {
    pub fn new(id: impl Into<String>, name: impl Into<String>, protocol: UpstreamProtocol) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            protocol,
            models: Vec::new(),
            priority: 0,
            premium_models: Vec::new(),
            protect_premium_quota: false,
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

    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_premium_models<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.premium_models = models.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_protect_premium_quota(mut self, protect: bool) -> Self {
        self.protect_premium_quota = protect;
        self
    }

    pub fn with_failure_count(mut self, failure_count: u32) -> Self {
        self.failure_count = failure_count;
        self
    }

    /// Check if this is a premium model for this upstream
    pub fn is_premium_model(&self, model: &str) -> bool {
        !self.premium_models.is_empty() && self.premium_models.iter().any(|m| m == model)
    }

    /// Check if this upstream should be avoided for non-premium models
    pub fn should_avoid_for_non_premium(&self, model: &str) -> bool {
        self.protect_premium_quota
            && !self.premium_models.is_empty()
            && !self.is_premium_model(model)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    ModelUnavailable(String),
    NoHealthyUpstream(String),
}

/// Intelligent upstream selection algorithm with premium quota protection
///
/// Algorithm:
/// 1. Filter candidates by protocol and model support
/// 2. Separate into preferred and fallback groups based on premium protection
/// 3. Try preferred group first (non-premium-protected or premium model match)
/// 4. Fall back to protected upstreams only if no preferred option available
/// 5. Within each group, sort by priority and select first healthy upstream
pub fn select_upstream(
    request: &RouteRequest,
    candidates: &[UpstreamCandidate],
) -> Result<UpstreamCandidate, RouteError> {
    // Step 1: Filter by protocol and model support
    let supported = candidates
        .iter()
        .filter(|candidate| {
            candidate.protocol == request.protocol
                && candidate.models.iter().any(|model| model == &request.model)
        })
        .cloned()
        .collect::<Vec<_>>();

    if supported.is_empty() {
        return Err(RouteError::ModelUnavailable(request.model.clone()));
    }

    // Step 2: Separate into preferred and fallback groups
    let (mut preferred, mut fallback): (Vec<_>, Vec<_>) = supported
        .into_iter()
        .partition(|candidate| !candidate.should_avoid_for_non_premium(&request.model));

    // Step 3: Try preferred group first
    if !preferred.is_empty() {
        // Sort by priority (higher first), then by failure count (lower first)
        preferred.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.failure_count.cmp(&b.failure_count))
        });

        // Find the first healthy upstream
        if let Some(candidate) = preferred.iter().find(|c| c.failure_count < 3) {
            return Ok(candidate.clone());
        }
    }

    // Step 4: Fall back to protected upstreams if no preferred option
    if !fallback.is_empty() {
        fallback.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.failure_count.cmp(&b.failure_count))
        });

        if let Some(candidate) = fallback.iter().find(|c| c.failure_count < 3) {
            return Ok(candidate.clone());
        }
    }

    // All upstreams are unhealthy
    Err(RouteError::NoHealthyUpstream(request.model.clone()))
}
