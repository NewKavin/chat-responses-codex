use super::{GiveUpReason, TerminalFailure};
use crate::state::{AppConfig, RouteRecovery, RuntimeSettings};
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_RETRY_JITTER_MS: u64 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryBudget {
    current_round: u32,
    waited: Duration,
    /// Whether the request already consumed its single budget-aligned last
    /// wait (R2): the round cap can be crossed exactly once per request when
    /// a live transient recovery fits the remaining time budget.
    alignment_used: bool,
}

impl Default for RouteRetryBudget {
    fn default() -> Self {
        Self {
            current_round: 1,
            waited: Duration::ZERO,
            alignment_used: false,
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn alignment_used(self) -> bool {
        self.alignment_used
    }

    /// Record a wait that happened outside `RouteRetryPolicy::decide`
    /// without advancing the round counter (e.g. the intra-round same-route
    /// retry backoff), so later round-level decisions still see the
    /// accumulated wait time.
    pub fn record_wait_time(&mut self, waited: Duration) {
        self.waited = self.waited.checked_add(waited).unwrap_or(Duration::MAX);
    }

    /// Consume a wait that is not the decision of `RouteRetryPolicy::decide`
    /// (e.g. the transient common-mode replay round) while keeping the round
    /// and budget accounting consistent for later `decide` calls.
    pub fn record_external_wait(&mut self, sleep_for: Duration) {
        let next_round = self.current_round.saturating_add(1);
        self.current_round = next_round;
        self.waited = self.waited.checked_add(sleep_for).unwrap_or(Duration::MAX);
    }

    pub fn record_wait(&mut self, wait: RouteRetryWait) {
        debug_assert_eq!(wait.next_round, self.current_round.saturating_add(1));
        self.current_round = wait.next_round;
        self.waited = self
            .waited
            .checked_add(wait.sleep_for)
            .unwrap_or(Duration::MAX);
        self.alignment_used = self.alignment_used || wait.alignment;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryWait {
    pub next_round: u32,
    pub required_delay: Duration,
    pub jitter: Duration,
    pub sleep_for: Duration,
    pub remaining_after: Duration,
    /// True for the single budget-aligned last wait granted after the round
    /// cap when a live transient recovery fits the remaining time budget.
    pub alignment: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteRetryPolicy {
    enabled: bool,
    max_wait: Duration,
    max_rounds: u32,
    concurrency_max_wait: Duration,
    concurrency_max_rounds: u32,
    budget_alignment_enabled: bool,
}

impl RouteRetryPolicy {
    #[cfg(test)]
    pub fn new(enabled: bool, max_wait: Duration, max_rounds: u32) -> Self {
        Self::new_with_tuning(enabled, max_wait, max_rounds, max_wait, max_rounds, true)
    }

    #[cfg(test)]
    pub fn new_with_alignment(
        enabled: bool,
        max_wait: Duration,
        max_rounds: u32,
        budget_alignment_enabled: bool,
    ) -> Self {
        Self::new_with_tuning(
            enabled,
            max_wait,
            max_rounds,
            max_wait,
            max_rounds,
            budget_alignment_enabled,
        )
    }

    fn new_with_tuning(
        enabled: bool,
        max_wait: Duration,
        max_rounds: u32,
        concurrency_max_wait: Duration,
        concurrency_max_rounds: u32,
        budget_alignment_enabled: bool,
    ) -> Self {
        Self {
            enabled,
            max_wait,
            max_rounds: max_rounds.max(1),
            concurrency_max_wait,
            concurrency_max_rounds: concurrency_max_rounds.max(1),
            budget_alignment_enabled,
        }
    }

    pub fn from_sources(_config: &AppConfig, runtime_settings: &RuntimeSettings) -> Self {
        Self::new_with_tuning(
            runtime_settings.upstream_route_exhaustion_retry_enabled,
            Duration::from_millis(runtime_settings.upstream_route_exhaustion_retry_max_wait_ms),
            runtime_settings.upstream_route_exhaustion_retry_max_rounds,
            Duration::from_millis(runtime_settings.upstream_concurrency_recovery_max_wait_ms),
            runtime_settings.upstream_concurrency_recovery_max_rounds,
            runtime_settings.upstream_route_exhaustion_budget_alignment_enabled,
        )
    }

    pub fn remaining_wait_budget(self, waited: Duration) -> Duration {
        self.max_wait.saturating_sub(waited)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn decide(
        self,
        budget: &RouteRetryBudget,
        terminal: TerminalFailure,
        health_recovery: Option<RouteRecovery>,
        client_retryable_rate_limit: bool,
        request_id: &str,
    ) -> Option<RouteRetryWait> {
        self.decide_with_reason(
            budget,
            terminal,
            health_recovery,
            client_retryable_rate_limit,
            request_id,
        )
        .0
    }

    /// Like [`Self::decide`], but also reports why no wait was granted when
    /// that is a terminal give-up (A5 observability).  The reason is only
    /// meaningful for the temporary-exhaustion path: non-temporary terminals
    /// and retry-disabled requests report `None`.
    pub fn decide_with_reason(
        self,
        budget: &RouteRetryBudget,
        terminal: TerminalFailure,
        health_recovery: Option<RouteRecovery>,
        client_retryable_rate_limit: bool,
        request_id: &str,
    ) -> (Option<RouteRetryWait>, Option<GiveUpReason>) {
        if !self.enabled {
            return (None, None);
        }
        let TerminalFailure::Temporary { retry_after } = terminal else {
            return (None, None);
        };
        if client_retryable_rate_limit {
            // A pure upstream rate-limit / key-quota exhaustion (429 family)
            // is a client-side retry signal: codex honors Retry-After and
            // keeps the task alive, so the gateway must not absorb the
            // cooldown in-process (B3).
            return (None, None);
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
            // The round cap normally means give up.  With budget alignment
            // enabled, a live transient-family recovery that fits the
            // remaining time budget earns one final aligned wait before the
            // request truly gives up (R2): max_rounds bounds blind retries,
            // the time budget bounds evidence-backed waits.
            if self.budget_alignment_enabled
                && !budget.alignment_used
                && !client_retryable_rate_limit
                && health_recovery.is_some_and(|recovery| {
                    matches!(
                        recovery.class,
                        crate::state::RouteFailureClass::TransientServer
                            | crate::state::RouteFailureClass::EdgeProxyError
                    )
                })
            {
                let required_delay = health_recovery
                    .expect("guarded by is_some_and above")
                    .retry_after;
                let next_round = budget.current_round.saturating_add(1);
                let jitter = deterministic_jitter(request_id, next_round);
                let Some(sleep_for) = required_delay.checked_add(jitter) else {
                    return (None, Some(GiveUpReason::WaitBudget));
                };
                let remaining = self.max_wait.saturating_sub(budget.waited);
                if sleep_for <= remaining {
                    return (
                        Some(RouteRetryWait {
                            next_round,
                            required_delay,
                            jitter,
                            sleep_for,
                            remaining_after: remaining - sleep_for,
                            alignment: true,
                        }),
                        None,
                    );
                }
                // An aligned recovery even longer than the remaining budget
                // is a wait-budget give-up, not a round-cap one: the cap was
                // already provisionally answered by the alignment check.
                return (None, Some(GiveUpReason::WaitBudget));
            }
            // No budget-aligned wait available at the round cap.  Classify
            // the give-up for observability: the alignment was already used,
            // the switch is off, or the live recovery does not qualify.
            if budget.alignment_used {
                return (None, Some(GiveUpReason::AlignmentExhausted));
            }
            let give_up_reason = if !self.budget_alignment_enabled {
                GiveUpReason::RoundCap
            } else if health_recovery.is_none() {
                GiveUpReason::NoRecovery
            } else {
                // Recovery exists but is not in the alignable transient
                // family (for example a concurrency recovery on its own
                // budget): that is the plain round-cap give-up.
                GiveUpReason::RoundCap
            };
            return (None, Some(give_up_reason));
        }
        let required_delay = health_recovery
            .map(|recovery| recovery.retry_after)
            .unwrap_or(retry_after);
        let next_round = budget.current_round.saturating_add(1);
        let jitter = deterministic_jitter(request_id, next_round);
        let Some(sleep_for) = required_delay.checked_add(jitter) else {
            return (None, Some(GiveUpReason::WaitBudget));
        };
        let remaining = max_wait.saturating_sub(budget.waited);
        if sleep_for > remaining {
            return (None, Some(GiveUpReason::WaitBudget));
        }

        (
            Some(RouteRetryWait {
                next_round,
                required_delay,
                jitter,
                sleep_for,
                remaining_after: remaining - sleep_for,
                alignment: false,
            }),
            None,
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
        runtime_settings.upstream_route_exhaustion_budget_alignment_enabled = false;

        let policy = RouteRetryPolicy::from_sources(&config, &runtime_settings);

        assert_eq!(policy.max_wait, Duration::from_millis(222));
        assert_eq!(policy.max_rounds, 5);
        assert_eq!(policy.concurrency_max_wait, Duration::from_millis(999));
        assert_eq!(policy.concurrency_max_rounds, 7);
        assert!(!policy.budget_alignment_enabled);
        assert!(
            RouteRetryPolicy::from_sources(&config, &RuntimeSettings::from_app_config(&config))
                .budget_alignment_enabled
        );
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
        let policy = RouteRetryPolicy::new_with_tuning(
            true,
            Duration::from_secs(10),
            3,
            Duration::from_secs(30),
            32,
            true,
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
        // With budget alignment (R2), the ordinary round cap now earns one
        // final aligned wait because the live transient recovery fits the
        // remaining budget; after that the ordinary budget gives up for good.
        let aligned = policy
            .decide(&budget, terminal, Some(ordinary), false, "ordinary-budget")
            .expect("round cap plus live transient recovery earns one aligned wait");
        assert!(aligned.alignment);
        budget.record_wait(aligned);
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
    fn round_cap_alignment_wait_granted_once_when_recovery_fits_budget() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(30), 3);
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let recovery = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_secs(5),
            half_open_remaining: None,
        };
        let mut budget = RouteRetryBudget::default();

        for _ in 0..2 {
            let wait = policy
                .decide(&budget, terminal, Some(recovery), false, "aligned")
                .expect("ordinary waits must fill the round budget");
            assert!(!wait.alignment);
            budget.record_wait(wait);
        }
        assert_eq!(budget.current_round(), 3);
        assert!(!budget.alignment_used());

        let wait = policy
            .decide(&budget, terminal, Some(recovery), false, "aligned")
            .expect("a live transient recovery inside the remaining budget earns one aligned wait");
        assert!(wait.alignment, "the last wait must be marked aligned");
        assert_eq!(wait.required_delay, recovery.retry_after);
        assert!(wait.sleep_for >= recovery.retry_after);
        budget.record_wait(wait);
        assert!(budget.alignment_used());

        assert!(
            policy
                .decide(&budget, terminal, Some(recovery), false, "aligned")
                .is_none(),
            "the alignment wait must happen at most once per request"
        );
    }

    #[test]
    fn round_cap_alignment_skipped_when_recovery_exceeds_budget() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(30), 3);
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let mut budget = RouteRetryBudget::default();
        let small = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_secs(5),
            half_open_remaining: None,
        };
        for _ in 0..2 {
            let wait = policy
                .decide(&budget, terminal, Some(small), false, "over-budget")
                .expect("ordinary waits must fill the round budget");
            budget.record_wait(wait);
        }
        let big = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_secs(25),
            half_open_remaining: None,
        };
        assert!(
            policy
                .decide(&budget, terminal, Some(big), false, "over-budget")
                .is_none(),
            "an alignment wait that exceeds the remaining time budget must be refused"
        );
    }

    #[test]
    fn round_cap_alignment_skips_non_transient_recovery_classes() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(30), 3);
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let budget = RouteRetryBudget {
            current_round: 3,
            waited: Duration::from_secs(10),
            alignment_used: false,
        };
        for class in [
            RouteFailureClass::RateLimited,
            RouteFailureClass::KeyQuota,
            RouteFailureClass::ConcurrencySaturated,
            RouteFailureClass::CapacityUnavailable,
            RouteFailureClass::Transport,
        ] {
            let recovery = RouteRecovery {
                class,
                retry_after: Duration::from_secs(5),
                half_open_remaining: None,
            };
            assert!(
                policy
                    .decide(&budget, terminal, Some(recovery), false, "non-transient")
                    .is_none(),
                "alignment must only follow TransientServer/EdgeProxyError recovery"
            );
        }
    }

    #[test]
    fn round_cap_alignment_respects_pure_client_rate_limit_b3() {
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(30), 3);
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let recovery = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_secs(5),
            half_open_remaining: None,
        };
        let budget = RouteRetryBudget {
            current_round: 3,
            waited: Duration::from_secs(10),
            alignment_used: false,
        };
        assert!(
            policy
                .decide(
                    &budget,
                    terminal,
                    Some(recovery),
                    true,
                    "client-rate-limited"
                )
                .is_none(),
            "a pure 429-family exhaustion must never absorb an in-gateway wait (B3)"
        );
    }

    #[test]
    fn round_cap_alignment_switch_off_restores_current_behavior() {
        let policy = RouteRetryPolicy::new_with_alignment(true, Duration::from_secs(30), 3, false);
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let recovery = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_secs(5),
            half_open_remaining: None,
        };
        let budget = RouteRetryBudget {
            current_round: 3,
            waited: Duration::from_secs(10),
            alignment_used: false,
        };
        assert!(
            policy
                .decide(&budget, terminal, Some(recovery), false, "switch-off")
                .is_none(),
            "with the switch off, the round cap must keep giving up immediately"
        );
    }

    #[test]
    fn give_up_reasons_classify_decide_refusals() {
        let terminal = TerminalFailure::Temporary {
            retry_after: Duration::from_secs(1),
        };
        let transient = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_secs(5),
            half_open_remaining: None,
        };

        // 1. Round cap with the alignment switch off -> round_cap.
        let policy = RouteRetryPolicy::new_with_alignment(true, Duration::from_secs(30), 3, false);
        let budget = RouteRetryBudget {
            current_round: 3,
            waited: Duration::from_secs(10),
            alignment_used: false,
        };
        assert_eq!(
            policy
                .decide_with_reason(&budget, terminal, Some(transient), false, "cap")
                .1,
            Some(GiveUpReason::RoundCap),
        );

        // 2. Round cap reached again after the aligned wait was consumed
        //    -> alignment_exhausted.
        let policy = RouteRetryPolicy::new(true, Duration::from_secs(30), 3);
        let exhausted = RouteRetryBudget {
            current_round: 3,
            waited: Duration::from_secs(10),
            alignment_used: true,
        };
        assert_eq!(
            policy
                .decide_with_reason(&exhausted, terminal, Some(transient), false, "cap-again")
                .1,
            Some(GiveUpReason::AlignmentExhausted),
        );

        // 3. Round cap with alignment enabled but no live recovery
        //    -> no_recovery.
        assert_eq!(
            policy
                .decide_with_reason(&budget, terminal, None, false, "no-recovery")
                .1,
            Some(GiveUpReason::NoRecovery),
        );

        // 4. The next evidence-backed wait exceeds the remaining budget
        //    -> wait_budget (both inside the aligned branch and in the
        //    ordinary branch).
        let small_budget = RouteRetryBudget {
            current_round: 1,
            waited: Duration::from_secs(29),
            alignment_used: false,
        };
        assert_eq!(
            policy
                .decide_with_reason(&small_budget, terminal, None, false, "budget")
                .1,
            Some(GiveUpReason::WaitBudget),
        );
        let aligned_over_budget = RouteRetryBudget {
            current_round: 3,
            waited: Duration::from_secs(29),
            alignment_used: false,
        };
        let long_recovery = RouteRecovery {
            class: RouteFailureClass::TransientServer,
            retry_after: Duration::from_secs(30),
            half_open_remaining: None,
        };
        assert_eq!(
            policy
                .decide_with_reason(
                    &aligned_over_budget,
                    terminal,
                    Some(long_recovery),
                    false,
                    "aligned-budget",
                )
                .1,
            Some(GiveUpReason::WaitBudget),
        );

        // A granted wait never carries a reason, and non-temporary terminals /
        // pure 429-family exhaustions report None rather than a gateway
        // give-up reason.
        let fresh = RouteRetryBudget {
            current_round: 1,
            waited: Duration::ZERO,
            alignment_used: false,
        };
        let (wait, reason) =
            policy.decide_with_reason(&fresh, terminal, Some(transient), false, "granted");
        assert!(wait.is_some());
        assert_eq!(reason, None);
        let credentials = TerminalFailure::Credentials;
        assert_eq!(
            policy
                .decide_with_reason(&fresh, credentials, None, false, "credentials")
                .1,
            None,
        );
        assert_eq!(
            policy
                .decide_with_reason(&fresh, terminal, Some(transient), true, "client-429")
                .1,
            None,
            "pure 429-family exhaustion is a client signal, not a gateway give-up (B3)"
        );
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
