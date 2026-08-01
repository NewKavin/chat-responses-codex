use chat_responses_codex::state::{
    AccountConcurrencyKey, AccountConcurrencyRegistry, AccountConcurrencyTuning, AccountProbeLease,
    AccountProbeOutcome, AppConfig, ProbeDecision,
};
use std::time::Duration;
use tokio::time::Instant;

fn test_tuning() -> AccountConcurrencyTuning {
    AccountConcurrencyTuning {
        probe_delays: vec![Duration::from_millis(100)],
        jitter_max: Duration::ZERO,
        waiter_budget: Duration::from_secs(600),
        waiter_ttl: Duration::from_secs(660),
        probe_ttl: Duration::from_secs(660),
        renewal_interval: Duration::from_secs(30),
        observation_freshness: Duration::from_secs(5),
        idle_retention: Duration::from_secs(600),
    }
}

#[test]
fn provider_observation_freshness_is_not_extended_by_the_poll_interval() {
    let mut config = AppConfig::default();
    config.upstream_concurrency_status_refresh_seconds = 30;

    assert_eq!(
        AccountConcurrencyTuning::from_config(&config).observation_freshness,
        Duration::from_secs(5)
    );
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
    tokio::time::advance(Duration::from_millis(100)).await;

    assert!(matches!(
        coordinator.try_probe(&first, Instant::now()),
        ProbeDecision::Wait { .. }
    ));
    let permit = match coordinator.try_probe(&second, Instant::now()) {
        ProbeDecision::Granted(permit) => permit,
        other => panic!("oldest waiter must win: {other:?}"),
    };
    assert!(matches!(
        coordinator.try_probe(&first, Instant::now()),
        ProbeDecision::Wait { .. }
    ));
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
    coordinator
        .observe_provider_status(&first, 0, 4, Instant::now())
        .unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        coordinator.snapshot(&first, Instant::now()).retry_after,
        Duration::from_secs(59)
    );
    assert!(coordinator.snapshot(&second, Instant::now()).retry_after <= Duration::from_secs(2));
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
async fn one_logical_request_has_only_one_ticket_across_accounts() {
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

    assert_eq!(coordinator.snapshot(&first, Instant::now()).waiters, 0);
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
