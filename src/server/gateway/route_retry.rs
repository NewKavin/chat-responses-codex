use super::TerminalFailure;
use crate::state::{AppConfig, RouteRecovery, RuntimeSettings};
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
    #[cfg_attr(not(test), allow(dead_code))]
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
    concurrency_max_wait: Duration,
    concurrency_max_rounds: u32,
}

impl RouteRetryPolicy {
    #[cfg(test)]
    pub fn new(enabled: bool, max_wait: Duration, max_rounds: u32) -> Self {
        Self::new_with_concurrency(enabled, max_wait, max_rounds, max_wait, max_rounds)
    }

    fn new_with_concurrency(
        enabled: bool,
        max_wait: Duration,
        max_rounds: u32,
        concurrency_max_wait: Duration,
        concurrency_max_rounds: u32,
    ) -> Self {
        Self {
            enabled,
            max_wait,
            max_rounds: max_rounds.max(1),
            concurrency_max_wait,
            concurrency_max_rounds: concurrency_max_rounds.max(1),
        }
    }

    pub fn from_sources(_config: &AppConfig, runtime_settings: &RuntimeSettings) -> Self {
        Self::new_with_concurrency(
            runtime_settings.upstream_route_exhaustion_retry_enabled,
            Duration::from_millis(runtime_settings.upstream_route_exhaustion_retry_max_wait_ms),
            runtime_settings.upstream_route_exhaustion_retry_max_rounds,
            Duration::from_millis(runtime_settings.upstream_concurrency_recovery_max_wait_ms),
            runtime_settings.upstream_concurrency_recovery_max_rounds,
        )
    }

    pub fn decide(
        self,
        budget: &RouteRetryBudget,
        terminal: TerminalFailure,
        health_recovery: Option<RouteRecovery>,
        client_retryable_rate_limit: bool,
        request_id: &str,
    ) -> Option<RouteRetryWait> {
        if !self.enabled {
            return None;
        }
        let TerminalFailure::Temporary { retry_after } = terminal else {
            return None;
        };
        if client_retryable_rate_limit {
            // A pure upstream rate-limit / key-quota exhaustion (429 family)
            // is a client-side retry signal: codex honors Retry-After and
            // keeps the task alive, so the gateway must not absorb the
            // cooldown in-process (B3).
            return None;
        }
        let concurrency_recovery = health_recovery.is_some_and(|recovery| {
            recovery.class == crate::state::RouteFailureClass::ConcurrencySaturated
        });
        let (max_wait, max_rounds) = if concurrency_recovery {
            (self.concurrency_max_wait, self.concurrency_max_rounds)
        } else {
            (self.max_wait, self.max_rounds)
        };
        if budget.current_round >= max_rounds {
            return None;
        }
        let required_delay = health_recovery
            .map(|recovery| recovery.retry_after)
            .unwrap_or(retry_after);
        let next_round = budget.current_round.saturating_add(1);
        let jitter = deterministic_jitter(request_id, next_round);
        let sleep_for = required_delay.checked_add(jitter)?;
        let remaining = max_wait.saturating_sub(budget.waited);
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
    fn policy_uses_runtime_rounds_and_concurrency_tuning() {
        let config = AppConfig {
            upstream_concurrency_recovery_max_wait_ms: 111,
            ..AppConfig::default()
        };
        let mut runtime_settings = RuntimeSettings::from_app_config(&config);
        runtime_settings.upstream_route_exhaustion_retry_max_wait_ms = 222;
        runtime_settings.upstream_route_exhaustion_retry_max_rounds = 5;
        runtime_settings.upstream_concurrency_recovery_max_wait_ms = 999;
        runtime_settings.upstream_concurrency_recovery_max_rounds = 7;

        let policy = RouteRetryPolicy::from_sources(&config, &runtime_settings);

        assert_eq!(policy.max_wait, Duration::from_millis(222));
        assert_eq!(policy.max_rounds, 5);
        assert_eq!(policy.concurrency_max_wait, Duration::from_millis(999));
        assert_eq!(policy.concurrency_max_rounds, 7);
    }

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
                false,
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
                false,
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
            .decide(&budget, temporary, None, false, "disabled")
            .is_none());
        assert!(RouteRetryPolicy::new(true, Duration::from_secs(10), 3)
            .decide(
                &budget,
                TerminalFailure::Credentials,
                None,
                false,
                "permanent"
            )
            .is_none());
        assert!(RouteRetryPolicy::new(true, Duration::from_secs(10), 3)
            .decide(
                &budget,
                TerminalFailure::Temporary {
                    retry_after: Duration::from_secs(11),
                },
                None,
                false,
                "over-budget",
            )
            .is_none());
        assert!(RouteRetryPolicy::new(true, Duration::ZERO, 3)
            .decide(&budget, temporary, None, false, "zero-budget")
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
            .decide(&budget, temporary, None, false, "round-limited")
            .expect("round two should be allowed");
        budget.record_wait(round_two);
        let round_three = policy
            .decide(&budget, temporary, None, false, "round-limited")
            .expect("round three should be allowed");
        budget.record_wait(round_three);

        assert_eq!(budget.current_round(), 3);
        assert!(budget.waited() <= Duration::from_secs(10));
        assert!(policy
            .decide(&budget, temporary, None, false, "round-limited")
            .is_none());
    }

    #[test]
    fn health_recovery_delay_overrides_shorter_terminal_ledger_delay() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(10), 3);
        let recovery = RouteRecovery {
            class: RouteFailureClass::RateLimited,
            retry_after: Duration::from_secs(7),
            half_open_remaining: None,
        };

        let wait = policy
            .decide(
                &RouteRetryBudget::default(),
                TerminalFailure::Temporary {
                    retry_after: Duration::from_secs(1),
                },
                Some(recovery),
                false,
                "health-wins",
            )
            .expect("actual health recovery fits the wait budget");

        assert_eq!(wait.required_delay, Duration::from_secs(7));
        assert!(wait.sleep_for >= recovery.retry_after);
    }

    #[test]
    fn concurrency_recovery_uses_its_own_wait_and_round_budget() {
        let policy = RouteRetryPolicy::new_with_concurrency(
            true,
            Duration::from_secs(10),
            3,
            Duration::from_secs(30),
            32,
        );
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_millis(100),
        };
        let concurrency = RouteRecovery {
            class: RouteFailureClass::ConcurrencySaturated,
            retry_after: Duration::from_millis(100),
            half_open_remaining: None,
        };
        let ordinary = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_millis(100),
            half_open_remaining: None,
        };
        let mut budget = RouteRetryBudget::default();

        for _ in 0..2 {
            let wait = policy
                .decide(
                    &budget,
                    terminal,
                    Some(concurrency),
                    false,
                    "concurrency-budget",
                )
                .expect("concurrency recovery should advance to round three");
            budget.record_wait(wait);
        }
        assert_eq!(budget.current_round(), 3);
        assert!(policy
            .decide(&budget, terminal, Some(ordinary), false, "ordinary-budget")
            .is_none());
        assert!(policy
            .decide(
                &budget,
                terminal,
                Some(concurrency),
                false,
                "concurrency-budget"
            )
            .is_some());
    }

    #[test]
    fn pure_client_rate_limit_never_schedules_an_in_gateway_wait() {
        // A 429 rate-limit exhaustion (RateLimited/KeyQuota family) is a
        // client-side retry signal: codex honors Retry-After, so the gateway
        // must not absorb the cooldown in-process even when the recovery fits
        // the wait budget (B3).
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(30), 3);
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let recovery = RouteRecovery {
            class: RouteFailureClass::RateLimited,
            retry_after: Duration::from_secs(25),
            half_open_remaining: None,
        };

        assert!(policy
            .decide(
                &RouteRetryBudget::default(),
                terminal,
                Some(recovery),
                true,
                "client-rate-limited",
            )
            .is_none());
    }
}
