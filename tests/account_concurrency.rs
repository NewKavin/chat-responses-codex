use chat_responses_codex::state::{
    AccountConcurrencyKey, AccountConcurrencyRegistry, AccountConcurrencyTuning, AccountProbeLease,
    AccountProbeOutcome, AppConfig, AppState, DownstreamConfig, PersistedState, ProbeDecision,
};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::Instant;

fn test_tuning() -> AccountConcurrencyTuning {
    AccountConcurrencyTuning {
        probe_delays: vec![Duration::from_millis(100)],
        jitter_max: Duration::ZERO,
        waiter_budget: Duration::from_secs(600),
        waiter_ttl: Duration::from_secs(660),
        probe_ttl: Duration::from_secs(660),
        renewal_interval: Duration::from_secs(30),
        idle_retention: Duration::from_secs(600),
    }
}

#[test]
fn healthy_accounts_do_not_register_recovery_waiters() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");

    assert!(coordinator
        .register_waiter_if_saturated(
            account.clone(),
            "req-healthy",
            "down-a",
            "lease-healthy",
            Instant::now(),
        )
        .is_none());

    coordinator.reject(&account, None, Instant::now());
    assert!(coordinator
        .register_waiter_if_saturated(
            account,
            "req-saturated",
            "down-a",
            "lease-saturated",
            Instant::now(),
        )
        .is_some());
}

#[tokio::test(start_paused = true)]
async fn local_probe_grant_atomically_requires_and_clears_downstream_waiting() {
    let directory = tempdir().unwrap();
    let downstream = DownstreamConfig {
        id: "down-atomic-grant".into(),
        name: "atomic grant".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
        rate_limit_enabled: true,
        per_minute_limit: 60,
        max_concurrency: 1,
        daily_token_limit: None,
        monthly_token_limit: None,
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: None,
        request_quota_requests: None,
        ip_allowlist: vec![],
        expires_at: None,
        active: true,
        billing_mode: "request".into(),
    };
    let state = AppState::new(
        PersistedState {
            downstreams: Arc::new(vec![downstream.clone()]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_concurrency_probe_delays_ms: vec![100],
            ..AppConfig::default()
        },
    );
    let account = AccountConcurrencyKey::new("up-atomic-grant", "fingerprint-atomic");
    let lease = state
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    state
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    let ticket = state
        .register_account_waiter_for_downstream_lease_if_saturated(
            &account,
            "request-atomic-grant",
            &lease,
        )
        .await
        .unwrap()
        .unwrap();
    tokio::time::advance(Duration::from_millis(200)).await;

    assert!(state
        .try_acquire_account_probe_for_downstream_lease(&ticket, &lease)
        .await
        .is_err());
    let unchanged = state
        .account_concurrency_registry()
        .snapshot(&account, Instant::now());
    assert_eq!(unchanged.waiters, 1);
    assert!(!unchanged.probe_in_flight);

    state.mark_downstream_waiting(&lease).await.unwrap();
    assert!(matches!(
        state
            .try_acquire_account_probe_for_downstream_lease(&ticket, &lease)
            .await
            .unwrap(),
        ProbeDecision::Granted(_)
    ));
    let granted = state
        .account_concurrency_registry()
        .snapshot(&account, Instant::now());
    assert_eq!(granted.waiters, 0);
    assert!(granted.probe_in_flight);
    let downstream_runtime = state
        .downstream_runtime_snapshot(&downstream)
        .await
        .unwrap();
    assert_eq!(downstream_runtime.waiting_upstream, 0);
    assert_eq!(downstream_runtime.running, 1);
}

async fn grant_one_probe(
    coordinator: &AccountConcurrencyRegistry,
    account: &AccountConcurrencyKey,
    request_id: &str,
) -> AccountProbeLease {
    coordinator.reject(account, None, Instant::now());
    let ticket = coordinator.register_waiter(
        account.clone(),
        request_id,
        "down-a",
        &format!("lease-{request_id}"),
        Instant::now(),
    );
    tokio::time::advance(Duration::from_millis(100)).await;
    match coordinator.try_probe(&ticket, Instant::now()) {
        ProbeDecision::Granted(lease) => lease,
        other => panic!("expected a probe grant, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn one_account_orders_waiters_across_models_and_protocols() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, None, Instant::now());

    let second = coordinator.register_waiter(
        account.clone(),
        "req-2",
        "down-a",
        "lease-2",
        Instant::now(),
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    let first = coordinator.register_waiter(
        account.clone(),
        "req-1",
        "down-a",
        "lease-1",
        Instant::now(),
    );
    let last = coordinator.register_waiter(
        account.clone(),
        "req-3",
        "down-a",
        "lease-3",
        Instant::now(),
    );
    tokio::time::advance(Duration::from_millis(100)).await;

    assert_eq!(
        coordinator.try_probe(&first, Instant::now()),
        ProbeDecision::Wait {
            retry_after: Duration::from_millis(100),
        }
    );
    assert_eq!(
        coordinator.try_probe(&last, Instant::now()),
        ProbeDecision::Wait {
            retry_after: Duration::from_millis(100),
        }
    );
    let permit = match coordinator.try_probe(&second, Instant::now()) {
        ProbeDecision::Granted(permit) => permit,
        other => panic!("oldest waiter must win: {other:?}"),
    };
    assert_eq!(
        coordinator.try_probe(&first, Instant::now()),
        ProbeDecision::Wait {
            retry_after: Duration::from_secs(660),
        }
    );
    assert_eq!(
        coordinator.try_probe(&last, Instant::now()),
        ProbeDecision::Wait {
            retry_after: Duration::from_millis(100),
        }
    );
    coordinator
        .finish_probe(permit, AccountProbeOutcome::Accepted, Instant::now())
        .unwrap();
    assert!(matches!(
        coordinator.try_probe(&first, Instant::now()),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn different_keys_are_independent_and_retry_after_is_not_shortened() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let first = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    let second = AccountConcurrencyKey::new("up-a", "fingerprint-b");
    coordinator.reject(&first, Some(Duration::from_secs(60)), Instant::now());
    coordinator.reject(&second, None, Instant::now());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        coordinator.snapshot(&first, Instant::now()).retry_after,
        Duration::from_secs(59)
    );
    assert!(coordinator.snapshot(&second, Instant::now()).retry_after <= Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn different_upstream_ids_keep_account_recovery_independent() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let first = AccountConcurrencyKey::new("up-account-a", "same-key-material");
    let second = AccountConcurrencyKey::new("up-account-b", "same-key-material");

    coordinator.reject(&first, Some(Duration::from_secs(60)), Instant::now());

    assert!(coordinator.snapshot(&first, Instant::now()).saturated);
    assert!(!coordinator.snapshot(&second, Instant::now()).saturated);
    assert_eq!(
        coordinator.snapshot(&second, Instant::now()).retry_after,
        Duration::ZERO
    );
}

#[tokio::test(start_paused = true)]
async fn stale_owner_cannot_complete_replacement_generation() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    let stale = grant_one_probe(&coordinator, &account, "req-stale").await;
    tokio::time::advance(Duration::from_secs(661)).await;
    let replacement = grant_one_probe(&coordinator, &account, "req-new").await;
    assert!(coordinator
        .finish_probe(stale, AccountProbeOutcome::Accepted, Instant::now())
        .is_err());
    coordinator
        .finish_probe(replacement, AccountProbeOutcome::Accepted, Instant::now())
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn concurrent_rejection_does_not_invalidate_an_active_probe() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-active", "fingerprint-a");
    let probe = grant_one_probe(&coordinator, &account, "req-probe").await;

    coordinator.reject(&account, None, Instant::now());

    coordinator.renew_probe(&probe, Instant::now()).unwrap();
    coordinator
        .finish_probe(probe, AccountProbeOutcome::Accepted, Instant::now())
        .unwrap();
}

#[test]
fn rejection_jitter_is_deterministic_and_bounded() {
    let mut tuning = test_tuning();
    tuning.jitter_max = Duration::from_millis(100);
    let first = AccountConcurrencyRegistry::new(tuning.clone());
    let second = AccountConcurrencyRegistry::new(tuning);
    let account = AccountConcurrencyKey::new("up-jitter", "fingerprint-a");

    let now = Instant::now();
    first.reject(&account, None, now);
    second.reject(&account, None, now);

    let first_delay = first.snapshot(&account, now).retry_after;
    let second_delay = second.snapshot(&account, now).retry_after;
    assert_eq!(first_delay, second_delay);
    assert!(first_delay >= Duration::from_millis(100));
    assert!(first_delay <= Duration::from_millis(200));
}

#[tokio::test(start_paused = true)]
async fn runtime_tuning_updates_waiter_budget_and_probe_delays() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-runtime", "key-runtime");
    coordinator.update_runtime_tuning(vec![7, 11], 25);
    coordinator.reject(&account, None, Instant::now());
    assert!(coordinator.snapshot(&account, Instant::now()).retry_after >= Duration::from_millis(7));

    let ticket = coordinator.register_waiter(
        account,
        "req-runtime",
        "down-a",
        "lease-runtime",
        Instant::now(),
    );
    tokio::time::advance(Duration::from_millis(26)).await;
    assert_eq!(
        coordinator
            .snapshot(&ticket.account, Instant::now())
            .waiters,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_removes_only_the_matching_ticket() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, None, Instant::now());
    let cancelled = coordinator.register_waiter(
        account.clone(),
        "req-cancel",
        "down-a",
        "lease-cancel",
        Instant::now(),
    );
    let retained = coordinator.register_waiter(
        account.clone(),
        "req-retain",
        "down-a",
        "lease-retain",
        Instant::now(),
    );

    coordinator.cancel_waiter(&cancelled);
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_eq!(coordinator.snapshot(&account, Instant::now()).waiters, 1);
    assert!(matches!(
        coordinator.try_probe(&retained, Instant::now()),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn concurrency_rejection_re_registers_the_request_at_the_tail() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, None, Instant::now());
    let oldest = coordinator.register_waiter(
        account.clone(),
        "req-oldest",
        "down-a",
        "lease-oldest",
        Instant::now(),
    );
    let next = coordinator.register_waiter(
        account.clone(),
        "req-next",
        "down-a",
        "lease-next",
        Instant::now(),
    );
    tokio::time::advance(Duration::from_millis(100)).await;
    let probe = match coordinator.try_probe(&oldest, Instant::now()) {
        ProbeDecision::Granted(probe) => probe,
        other => panic!("expected oldest grant, got {other:?}"),
    };
    coordinator
        .finish_probe(
            probe,
            AccountProbeOutcome::ConcurrencyRejected { retry_after: None },
            Instant::now(),
        )
        .unwrap();
    let retried = coordinator.register_waiter(
        account.clone(),
        "req-oldest",
        "down-a",
        "lease-oldest",
        Instant::now(),
    );
    tokio::time::advance(Duration::from_millis(100)).await;

    assert!(matches!(
        coordinator.try_probe(&retried, Instant::now()),
        ProbeDecision::Wait { .. }
    ));
    assert!(matches!(
        coordinator.try_probe(&next, Instant::now()),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn attempt_failure_preserves_saturation_until_an_explicit_accept() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    let probe = grant_one_probe(&coordinator, &account, "req-failed").await;
    let generation = probe.generation;
    coordinator
        .finish_probe(probe, AccountProbeOutcome::AttemptFailed, Instant::now())
        .unwrap();

    let snapshot = coordinator.snapshot(&account, Instant::now());
    assert!(snapshot.saturated);
    assert_eq!(snapshot.generation, generation);
}

#[tokio::test(start_paused = true)]
async fn logical_wait_budget_expires_before_cleanup_ttl() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, Some(Duration::from_secs(700)), Instant::now());
    let ticket = coordinator.register_waiter(
        account.clone(),
        "req-expired",
        "down-a",
        "lease-expired",
        Instant::now(),
    );
    tokio::time::advance(Duration::from_secs(601)).await;

    assert_eq!(coordinator.snapshot(&account, Instant::now()).waiters, 0);
    assert!(matches!(
        coordinator.try_probe(&ticket, Instant::now()),
        ProbeDecision::Wait { .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn one_logical_request_keeps_one_ticket_per_account() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let first = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    let second = AccountConcurrencyKey::new("up-b", "fingerprint-a");
    coordinator.register_waiter(
        first.clone(),
        "req-one",
        "down-a",
        "lease-one",
        Instant::now(),
    );
    coordinator.register_waiter(
        second.clone(),
        "req-one",
        "down-a",
        "lease-one",
        Instant::now(),
    );

    assert_eq!(coordinator.snapshot(&first, Instant::now()).waiters, 1);
    assert_eq!(coordinator.snapshot(&second, Instant::now()).waiters, 1);
}

#[tokio::test(start_paused = true)]
async fn inactive_account_entries_are_pruned_after_ten_minutes() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, None, Instant::now());
    tokio::time::advance(Duration::from_secs(601)).await;

    assert_eq!(coordinator.prune_idle(Instant::now()), 1);
    assert_eq!(coordinator.entry_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn waiter_renewal_extends_cleanup_lease_without_extending_logical_budget() {
    let mut tuning = test_tuning();
    tuning.waiter_budget = Duration::from_secs(2);
    tuning.waiter_ttl = Duration::from_secs(1);
    let coordinator = AccountConcurrencyRegistry::new(tuning);
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, Some(Duration::from_secs(10)), Instant::now());
    let ticket = coordinator.register_waiter(
        account.clone(),
        "req-renew",
        "down-a",
        "lease-renew",
        Instant::now(),
    );

    tokio::time::advance(Duration::from_millis(500)).await;
    coordinator.renew_waiter(&ticket, Instant::now()).unwrap();
    tokio::time::advance(Duration::from_millis(600)).await;
    assert_eq!(coordinator.snapshot(&account, Instant::now()).waiters, 1);
    tokio::time::advance(Duration::from_millis(900)).await;
    assert!(coordinator.renew_waiter(&ticket, Instant::now()).is_err());
    assert_eq!(coordinator.snapshot(&account, Instant::now()).waiters, 0);
}

#[tokio::test(start_paused = true)]
async fn probe_renewal_keeps_the_same_owner_valid_past_the_initial_ttl() {
    let mut tuning = test_tuning();
    tuning.probe_ttl = Duration::from_secs(1);
    let coordinator = AccountConcurrencyRegistry::new(tuning);
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    let probe = grant_one_probe(&coordinator, &account, "req-renew-probe").await;

    tokio::time::advance(Duration::from_millis(500)).await;
    coordinator.renew_probe(&probe, Instant::now()).unwrap();
    tokio::time::advance(Duration::from_millis(600)).await;
    coordinator
        .finish_probe(probe, AccountProbeOutcome::Accepted, Instant::now())
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn stale_local_ticket_cannot_cancel_same_generation_re_registration() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-a", "fingerprint-a");
    coordinator.reject(&account, None, Instant::now());
    let stale = coordinator.register_waiter(
        account.clone(),
        "req-ticket",
        "down-a",
        "lease-ticket",
        Instant::now(),
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    let current = coordinator.register_waiter(
        account.clone(),
        "req-ticket",
        "down-a",
        "lease-ticket",
        Instant::now(),
    );

    coordinator.cancel_waiter(&stale);
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_eq!(coordinator.snapshot(&account, Instant::now()).waiters, 1);
    assert!(matches!(
        coordinator.try_probe(&current, Instant::now()),
        ProbeDecision::Granted(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn stale_local_ticket_is_fenced_when_re_registered_in_the_same_millisecond() {
    let coordinator = AccountConcurrencyRegistry::new(test_tuning());
    let account = AccountConcurrencyKey::new("up-same-ms", "fingerprint-a");
    coordinator.reject(&account, None, Instant::now());
    let stale = coordinator.register_waiter(
        account.clone(),
        "req-ticket",
        "down-a",
        "lease-ticket",
        Instant::now(),
    );
    let current = coordinator.register_waiter(
        account.clone(),
        "req-ticket",
        "down-a",
        "lease-ticket",
        Instant::now(),
    );

    coordinator.cancel_waiter(&stale);
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_eq!(coordinator.snapshot(&account, Instant::now()).waiters, 1);
    assert!(matches!(
        coordinator.try_probe(&current, Instant::now()),
        ProbeDecision::Granted(_)
    ));
}
