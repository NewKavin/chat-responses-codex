use super::TerminalFailure;
use crate::state::{AppConfig, RouteRecovery};
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_RETRY_JITTER_MS: u64 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryBudget {
    current_round: u32,
    waited: Duration,
}

impl Default for RouteRetryBudget {
    fn default() -> Self {
        Self {
            current_round: 1,
            waited: Duration::ZERO,
        }
    }
}

impl RouteRetryBudget {
    pub fn current_round(self) -> u32 {
        self.current_round
    }

    pub fn waited(self) -> Duration {
        self.waited
    }

    pub fn record_wait(&mut self, wait: RouteRetryWait) {
        debug_assert_eq!(wait.next_round, self.current_round.saturating_add(1));
        self.current_round = wait.next_round;
        self.waited = self
            .waited
            .checked_add(wait.sleep_for)
            .unwrap_or(Duration::MAX);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryWait {
    pub next_round: u32,
    pub required_delay: Duration,
    pub jitter: Duration,
    pub sleep_for: Duration,
    pub remaining_after: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryPolicy {
    enabled: bool,
    max_wait: Duration,
    max_rounds: u32,
}

impl RouteRetryPolicy {
    pub fn new(enabled: bool, max_wait: Duration, max_rounds: u32) -> Self {
        Self {
            enabled,
            max_wait,
            max_rounds: max_rounds.max(1),
        }
    }

    pub fn decide(
        self,
        budget: &RouteRetryBudget,
        terminal: TerminalFailure,
        health_recovery: Option<RouteRecovery>,
        request_id: &str,
    ) -> Option<RouteRetryWait> {
        if !self.enabled || budget.current_round >= self.max_rounds {
            return None;
        }
        let TerminalFailure::Temporary { retry_after } = terminal else {
            return None;
        };
        let required_delay = health_recovery
            .map(|recovery| recovery.retry_after)
            .unwrap_or(retry_after);
        let next_round = budget.current_round.saturating_add(1);
        let jitter = deterministic_jitter(request_id, next_round);
        let sleep_for = required_delay.checked_add(jitter)?;
        let remaining = self.max_wait.saturating_sub(budget.waited);
        if sleep_for > remaining {
            return None;
        }

        Some(RouteRetryWait {
            next_round,
            required_delay,
            jitter,
            sleep_for,
            remaining_after: remaining - sleep_for,
        })
    }
}

impl From<&AppConfig> for RouteRetryPolicy {
    fn from(config: &AppConfig) -> Self {
        Self::new(
            config.upstream_route_exhaustion_retry_enabled,
            Duration::from_millis(config.upstream_route_exhaustion_retry_max_wait_ms),
            config.upstream_route_exhaustion_retry_max_rounds,
        )
    }
}

fn deterministic_jitter(request_id: &str, next_round: u32) -> Duration {
    let mut hasher = Sha256::new();
    hasher.update(request_id.as_bytes());
    hasher.update(next_round.to_le_bytes());
    let digest = hasher.finalize();
    let value = u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix must contain eight bytes"),
    );
    Duration::from_millis(value % (MAX_RETRY_JITTER_MS + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::gateway::TerminalFailure;
    use crate::state::{RouteFailureClass, RouteRecovery};
    use std::time::Duration;

    #[test]
    fn temporary_exhaustion_schedules_bounded_deterministic_wait() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(10), 3);
        let budget = RouteRetryBudget::default();

        let first = policy
            .decide(
                &budget,
                TerminalFailure::Temporary {
                    retry_after: Duration::from_secs(1),
                },
                None,
                "request-a",
            )
            .expect("short temporary exhaustion should retry");
        let repeated = policy
            .decide(
                &budget,
                TerminalFailure::Temporary {
                    retry_after: Duration::from_secs(1),
                },
                None,
                "request-a",
            )
            .expect("same decision should remain eligible");

        assert_eq!(first, repeated);
        assert_eq!(first.next_round, 2);
        assert_eq!(first.required_delay, Duration::from_secs(1));
        assert!(first.jitter <= Duration::from_millis(100));
        assert_eq!(first.sleep_for, first.required_delay + first.jitter);
        assert!(first.sleep_for <= Duration::from_secs(10));
        assert_eq!(
            first.remaining_after,
            Duration::from_secs(10) - first.sleep_for
        );
    }

    #[test]
    fn retry_policy_rejects_disabled_permanent_and_over_budget_waits() {
        let budget = RouteRetryBudget::default();
        let temporary = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };

        assert!(RouteRetryPolicy::new(false, Duration::from_secs(10), 3)
            .decide(&budget, temporary, None, "disabled")
            .is_none());
        assert!(RouteRetryPolicy::new(true, Duration::from_secs(10), 3)
            .decide(&budget, TerminalFailure::Credentials, None, "permanent")
            .is_none());
        assert!(RouteRetryPolicy::new(true, Duration::from_secs(10), 3)
            .decide(
                &budget,
                TerminalFailure::Temporary {
                    retry_after: Duration::from_secs(11),
                },
                None,
                "over-budget",
            )
            .is_none());
        assert!(RouteRetryPolicy::new(true, Duration::ZERO, 3)
            .decide(&budget, temporary, None, "zero-budget")
            .is_none());
    }

    #[test]
    fn retry_budget_never_exceeds_total_round_limit() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(10), 3);
        let temporary = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let mut budget = RouteRetryBudget::default();

        let round_two = policy
            .decide(&budget, temporary, None, "round-limited")
            .expect("round two should be allowed");
        budget.record_wait(round_two);
        let round_three = policy
            .decide(&budget, temporary, None, "round-limited")
            .expect("round three should be allowed");
        budget.record_wait(round_three);

        assert_eq!(budget.current_round(), 3);
        assert!(budget.waited() <= Duration::from_secs(10));
        assert!(policy
            .decide(&budget, temporary, None, "round-limited")
            .is_none());
    }

    #[test]
    fn health_recovery_delay_overrides_shorter_terminal_ledger_delay() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(10), 3);
        let recovery = RouteRecovery {
            class: RouteFailureClass::RateLimited,
            retry_after: Duration::from_secs(7),
        };

        let wait = policy
            .decide(
                &RouteRetryBudget::default(),
                TerminalFailure::Temporary {
                    retry_after: Duration::from_secs(1),
                },
                Some(recovery),
                "health-wins",
            )
            .expect("actual health recovery fits the wait budget");

        assert_eq!(wait.required_delay, Duration::from_secs(7));
        assert!(wait.sleep_for >= recovery.retry_after);
    }
}
