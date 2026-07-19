pub(super) use crate::upstream_feedback::FailureClass;
use std::collections::HashSet;
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AttemptFailure {
    pub route_id: String,
    pub upstream_status: Option<u16>,
    pub class: FailureClass,
    pub retry_after: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalFailure {
    Temporary { retry_after: Duration },
    Credentials,
    ModelUnsupported,
    CapabilityUnsupported,
    ProtocolUnsupported,
    MixedRoutesExhausted,
}

#[derive(Default)]
pub(super) struct AttemptLedger {
    failures: Vec<AttemptFailure>,
    cooled_candidates: Vec<AttemptFailure>,
}

impl AttemptLedger {
    pub fn record(&mut self, failure: AttemptFailure) {
        self.failures.push(failure);
    }

    #[allow(dead_code)] // Used once route-health cooldown candidates are wired in Task 7.
    pub fn record_cooled(&mut self, failure: AttemptFailure) {
        self.cooled_candidates.push(failure);
    }

    pub fn is_empty(&self) -> bool {
        self.failures.is_empty() && self.cooled_candidates.is_empty()
    }

    pub fn attempt_count(&self) -> usize {
        self.failures.len()
    }

    pub fn cooled_candidate_count(&self) -> usize {
        self.cooled_candidates.len()
    }

    pub fn distinct_route_count(&self) -> usize {
        self.failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .map(|failure| failure.route_id.as_str())
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn class_count(&self, class: FailureClass) -> usize {
        self.failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .filter(|failure| failure.class == class)
            .count()
    }

    pub fn terminal_failure(&self) -> TerminalFailure {
        let candidates = self
            .failures
            .iter()
            .chain(self.cooled_candidates.iter())
            .collect::<Vec<_>>();
        assert!(
            !candidates.is_empty(),
            "terminal failure requires a candidate"
        );

        if candidates
            .iter()
            .any(|failure| failure.class.is_temporary())
        {
            let retry_after = candidates
                .iter()
                .filter(|failure| failure.class.is_temporary())
                .filter_map(|failure| failure.retry_after)
                .min()
                .unwrap_or(Duration::from_secs(1));
            return TerminalFailure::Temporary { retry_after };
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::Credentials)
        {
            return TerminalFailure::Credentials;
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::ModelUnsupported)
        {
            return TerminalFailure::ModelUnsupported;
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::FeatureUnsupported)
        {
            return TerminalFailure::CapabilityUnsupported;
        }
        if candidates
            .iter()
            .all(|failure| failure.class == FailureClass::ProtocolUnsupported)
        {
            return TerminalFailure::ProtocolUnsupported;
        }
        TerminalFailure::MixedRoutesExhausted
    }
}
