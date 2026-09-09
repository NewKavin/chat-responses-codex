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
    pub weight: u32,
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
            weight: 1,
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

    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
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

/// Deterministic weighted pick within the highest-priority tier.
///
/// Finds the maximum `priority` among the candidates, collects the
/// positive-weight members of that tier, and picks one using a cumulative
/// weighted round-robin over `cursor` (a monotonically increasing per-key
/// counter; no random state). For weights `3` and `1`, successive cursors
/// yield indices in the cumulative 3:1 pattern (a, a, a, b, ...).
///
/// Weight `0` members are never actively picked while a positive-weight
/// member exists. If every member of the highest tier has weight `0`, the
/// tier's first member (existing stable ordering) is returned. Returns
/// `None` when there are no candidates at all.
pub fn select_weighted_candidate_index(
    candidates: &[UpstreamCandidate],
    cursor: u64,
) -> Option<usize> {
    let highest_priority = candidates
        .iter()
        .map(|candidate| candidate.priority)
        .max()?;
    let tier = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.priority == highest_priority)
        .collect::<Vec<_>>();
    let positive = tier
        .iter()
        .filter(|(_, candidate)| candidate.weight > 0)
        .map(|(index, candidate)| (*index, candidate.weight))
        .collect::<Vec<_>>();
    if positive.is_empty() {
        // Stable ordering: first member of the highest tier.
        return tier.first().map(|(index, _)| *index);
    }
    let total: u64 = positive.iter().map(|(_, weight)| *weight as u64).sum();
    let mut bucket = cursor % total;
    for (index, weight) in &positive {
        if bucket < *weight as u64 {
            return Some(*index);
        }
        bucket -= *weight as u64;
    }
    positive.first().map(|(index, _)| *index)
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
    select_upstream_with_model_matching(request, candidates, true)
}

pub fn select_upstream_with_model_matching(
    request: &RouteRequest,
    candidates: &[UpstreamCandidate],
    case_insensitive: bool,
) -> Result<UpstreamCandidate, RouteError> {
    // Step 1: Filter by protocol and model support
    let supported = candidates
        .iter()
        .filter(|candidate| {
            candidate.protocol == request.protocol
                && candidate.models.iter().any(|model| {
                    crate::state::models_equivalent_with(model, &request.model, case_insensitive)
                })
        })
        .cloned()
        .collect::<Vec<_>>();

    if supported.is_empty() {
        return Err(RouteError::ModelUnavailable(request.model.clone()));
    }

    // Step 2: Sort by priority (higher first), then by failure count (lower first)
    let mut preferred = supported;
    preferred.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.failure_count.cmp(&b.failure_count))
    });

    // Find the first healthy upstream
    if let Some(candidate) = preferred.iter().find(|c| c.failure_count < 3) {
        return Ok(candidate.clone());
    }

    // All upstreams are unhealthy
    Err(RouteError::NoHealthyUpstream(request.model.clone()))
}
