use chat_responses_codex::capabilities::WireProtocol;
use chat_responses_codex::keys::upstream_key_fingerprint;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    ApiKeyModelConfig, AppConfig, AppState, KeyHealthKey, PersistedState, RouteAvailability,
    RouteFailureClass, RouteHealthKey, RouteHealthRegistry, RouteOutcome, RouteSetAggregateKey,
    UpstreamConfig,
};
use std::time::Duration;

fn key(fingerprint: &str) -> KeyHealthKey {
    KeyHealthKey {
        upstream_id: "up-1".into(),
        key_fingerprint: fingerprint.into(),
    }
}

fn route(fingerprint: &str, model: &str) -> RouteHealthKey {
    RouteHealthKey {
        upstream_id: "up-1".into(),
        key_fingerprint: fingerprint.into(),
        runtime_model_slug: model.into(),
        protocol: WireProtocol::Responses,
    }
}

fn snapshot_upstream(active: bool) -> UpstreamConfig {
    UpstreamConfig {
        id: "snapshot-upstream".into(),
        name: "snapshot-upstream".into(),
        base_url: "https://example.invalid".into(),
        api_key: "snapshot-secret".into(),
        api_key_models: vec![ApiKeyModelConfig {
            api_key: "snapshot-secret".into(),
            supported_models: vec!["glm-5.2".into()],
        }],
        protocol: UpstreamProtocol::Responses,
        supported_models: vec!["glm-5.2".into()],
        active,
        ..UpstreamConfig::default()
    }
}

#[test]
fn route_health_snapshot_excludes_inactive_upstreams() {
    let registry = RouteHealthRegistry::new(16, 16);
    let upstream = snapshot_upstream(false);

    let snapshot = registry.upstream_snapshots(&[upstream]);
    let snapshot = &snapshot["snapshot-upstream"];

    assert_eq!(snapshot.healthy_routes, 0);
    assert_eq!(snapshot.cooldown_routes, 0);
    assert_eq!(snapshot.half_open_routes, 0);
}

#[tokio::test(start_paused = true)]
async fn route_health_snapshot_uses_key_cooldown_before_route_half_open() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let upstream = snapshot_upstream(true);
    let fingerprint = upstream_key_fingerprint(&upstream.id, "snapshot-secret");
    let key = KeyHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint: fingerprint.clone(),
    };
    let route = RouteHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint: fingerprint,
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    };
    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    tokio::time::advance(Duration::from_secs(20)).await;
    let _route_half_open = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected route half-open lease, got {other:?}"),
    };
    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);
    assert!(registry.route_health_snapshot(&route).unwrap().half_open);
    assert!(!registry.key_health_snapshot(&key).unwrap().half_open);

    let snapshot = registry.upstream_snapshots(&[upstream]);
    let snapshot = &snapshot["snapshot-upstream"];
    assert_eq!(snapshot.healthy_routes, 0);
    assert_eq!(snapshot.cooldown_routes, 1);
    assert_eq!(snapshot.half_open_routes, 0);
    assert_eq!(snapshot.failure_classes["credentials"], 1);
}

#[tokio::test(start_paused = true)]
async fn route_cooldown_has_one_half_open_lease_and_resets_after_success() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));

    tokio::time::advance(Duration::from_secs(12)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));
    registry.finish(lease, RouteOutcome::Success);
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn repeated_transient_failure_within_same_request_keeps_step_flat() {
    // A1: one downstream request that burns several routing rounds against the
    // same route must not amplify the failure step (R1): only the first
    // failure of the request escalates, later rounds reset the cooldown start
    // without growing the cooldown.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let route = route("request-suppressed-step", "glm-5.2");
    let key = key("request-suppressed-step");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let first_step = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(first_step.consecutive_failures, 1);
    let first_cooldown = first_step.cooldown_remaining;

    for round in 2..=3 {
        let remaining = registry
            .route_health_snapshot(&route)
            .unwrap()
            .cooldown_remaining;
        tokio::time::advance(remaining + Duration::from_millis(1)).await;
        let lease = match registry.reserve(&route, &key) {
            RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
            other => panic!("expected half-open permit in round {round}, got {other:?}"),
        };
        registry.finish(
            lease,
            RouteOutcome::RouteFailure {
                class: RouteFailureClass::TransientServer,
                upstream_status: Some(500),
                repeat_within_request: true,
                sole_candidate: false,
                shared_host_failure_domain: false,
            },
        );
        let snapshot = registry.route_health_snapshot(&route).unwrap();
        assert_eq!(
            snapshot.consecutive_failures, 1,
            "round {round} of the same request must not escalate the failure step"
        );
        assert_eq!(
            snapshot.cooldown_remaining, first_cooldown,
            "round {round} of the same request must not grow the cooldown"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn sole_candidate_cross_request_failures_keep_step_flat() {
    // P4 (R3): when every request can only reach one route (e.g. a
    // continuation-pinned pool of one), independent-request failures must
    // not escalate the cooldown step — otherwise the sole route pins its
    // own cooldown at max and the session stays stuck until the max lapses.
    // The gateway marks such failures with sole_candidate = true; the health
    // layer then applies the repeat_within_request semantics (reset the
    // cooldown start, keep the step flat).
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let route = route("sole-candidate-pinned", "glm-5.2");
    let key = key("sole-candidate-pinned");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let first_step = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(first_step.consecutive_failures, 1);
    let first_cooldown = first_step.cooldown_remaining;

    // A later, independent request fails on the same sole-candidate route.
    tokio::time::advance(first_cooldown + Duration::from_millis(1)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    registry.finish(
        lease,
        RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: Some(500),
            repeat_within_request: false,
            sole_candidate: true,
            shared_host_failure_domain: false,
        },
    );
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(
        snapshot.consecutive_failures, 1,
        "a sole-candidate failure must not escalate the failure step"
    );
    assert_eq!(
        snapshot.cooldown_remaining, first_cooldown,
        "a sole-candidate failure must not grow the cooldown"
    );
}

#[tokio::test(start_paused = true)]
async fn independent_request_failures_still_escalate_the_step() {
    // A1 counter-check: failures from independent requests keep escalating.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let route = route("independent-escalation", "glm-5.2");
    let key = key("independent-escalation");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let first_cooldown = registry
        .route_health_snapshot(&route)
        .unwrap()
        .cooldown_remaining;

    tokio::time::advance(first_cooldown + Duration::from_millis(1)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    registry.finish(
        lease,
        RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: Some(500),
            repeat_within_request: false,
            sole_candidate: false,
            shared_host_failure_domain: false,
        },
    );
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(
        snapshot.consecutive_failures, 2,
        "an independent request failure must escalate the step"
    );
    assert!(
        snapshot.cooldown_remaining > first_cooldown,
        "second independent failure must produce a longer cooldown"
    );
}

#[tokio::test(start_paused = true)]
async fn half_open_probe_failure_step_is_capped_so_cooldown_cannot_pin_at_max() {
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        8,
        300,
        3000,
        60,
    );
    let route = route("half-open-step-cap", "glm-5.2");
    let key = key("half-open-step-cap");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let mut streak = 1;
    for _ in 0..12 {
        let cooldown = registry
            .route_health_snapshot(&route)
            .unwrap()
            .cooldown_remaining;
        tokio::time::advance(cooldown + Duration::from_millis(1)).await;
        let lease = match registry.reserve(&route, &key) {
            RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
            other => panic!("expected half-open permit, got {other:?}"),
        };
        registry.finish(
            lease,
            RouteOutcome::RouteFailure {
                class: RouteFailureClass::TransientServer,
                upstream_status: Some(500),
                repeat_within_request: false,
                sole_candidate: false,
                shared_host_failure_domain: false,
            },
        );
        streak = registry
            .route_health_snapshot(&route)
            .unwrap()
            .consecutive_failures;
    }
    assert_eq!(
        streak, 5,
        "half-open probe failures must cap the failure step at 5"
    );
}

#[tokio::test(start_paused = true)]
async fn transient_route_cooldown_uses_configured_base_and_cap() {
    for class in [
        RouteFailureClass::TransientServer,
        RouteFailureClass::Transport,
    ] {
        let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
            16,
            16,
            vec![100, 200],
            3,
            4,
            3,
            300,
            3000,
            60,
        );
        let route = route(class.as_str(), "glm-5.2");

        registry.observe_route_failure(&route, class, None, false);
        let first = registry.route_health_snapshot(&route).unwrap();
        assert_eq!(first.consecutive_failures, 1);
        assert!(first.cooldown_remaining >= Duration::from_millis(2_400));
        assert!(first.cooldown_remaining <= Duration::from_millis(3_600));

        registry.observe_route_failure(&route, class, None, false);
        let second = registry.route_health_snapshot(&route).unwrap();
        assert_eq!(second.consecutive_failures, 2);
        assert_eq!(second.cooldown_remaining, Duration::from_secs(4));
    }
}

#[tokio::test(start_paused = true)]
async fn transient_route_cooldown_config_does_not_change_other_classes() {
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let concurrency_route = route("concurrency-config-isolation", "glm-5.2");

    registry.observe_route_failure(
        &concurrency_route,
        RouteFailureClass::ConcurrencySaturated,
        None,
        false,
    );
    assert_eq!(
        registry
            .route_health_snapshot(&concurrency_route)
            .unwrap()
            .cooldown_remaining,
        Duration::from_millis(100)
    );
    registry.observe_route_failure(
        &concurrency_route,
        RouteFailureClass::ConcurrencySaturated,
        None,
        false,
    );
    assert_eq!(
        registry
            .route_health_snapshot(&concurrency_route)
            .unwrap()
            .cooldown_remaining,
        Duration::from_millis(200)
    );

    let capacity_route = route("capacity-config-isolation", "glm-5.2");
    registry.observe_route_failure(
        &capacity_route,
        RouteFailureClass::CapacityUnavailable,
        None,
        false,
    );
    assert!(
        registry
            .route_health_snapshot(&capacity_route)
            .unwrap()
            .cooldown_remaining
            > Duration::from_secs(4)
    );
}

#[tokio::test(start_paused = true)]
async fn runtime_tuning_updates_future_delays_and_clamps_existing_transient_cooldown() {
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        60,
        3,
        300,
        3000,
        60,
    );
    let route = route("key-runtime", "model-runtime");
    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    assert!(
        registry
            .route_health_snapshot(&route)
            .unwrap()
            .cooldown_remaining
            > Duration::from_secs(2)
    );

    registry.update_runtime_tuning(vec![7, 11], 1, 2, 3, 5, 3000, 60);
    let clamped = registry.route_health_snapshot(&route).unwrap();
    assert!(clamped.cooldown_remaining <= Duration::from_secs(2));

    tokio::time::advance(Duration::from_secs(2)).await;
    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let updated = registry.route_health_snapshot(&route).unwrap();
    assert!(updated.cooldown_remaining <= Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn successful_route_state_does_not_create_a_new_half_open_lease() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    tokio::time::advance(Duration::from_secs(12)).await;
    let recovery = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open recovery lease, got {other:?}"),
    };
    registry.finish(recovery, RouteOutcome::Success);

    let healthy = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) => lease,
        other => panic!("expected healthy route, got {other:?}"),
    };
    assert!(!healthy.is_half_open());
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn key_credentials_cool_all_routes_for_that_key_but_not_another_key() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let key_a = key("fingerprint-a");
    let key_b = key("fingerprint-b");
    let route_a_model = route("fingerprint-a", "glm-5.2");
    let route_a_other_model = route("fingerprint-a", "glm-4.7");
    let route_b = route("fingerprint-b", "glm-5.2");

    registry.observe_key_failure(&key_a, RouteFailureClass::Credentials, None);
    assert!(matches!(
        registry.reserve(&route_a_model, &key_a),
        RouteAvailability::Cooling {
            class: RouteFailureClass::Credentials,
            ..
        }
    ));
    assert!(matches!(
        registry.reserve(&route_a_other_model, &key_a),
        RouteAvailability::Cooling { .. }
    ));
    assert!(matches!(
        registry.reserve(&route_b, &key_b),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn credentials_first_strike_cools_short_then_escalates_to_key_curve() {
    // T5: the first credential strike uses upstream_credentials_first_strike_seconds
    // (default 60s, jitter 80-120% => 48..=72s) instead of the 15min curve;
    // a second strike within the 10min streak window escalates to the curve.
    let mut registry = RouteHealthRegistry::new(16, 16);
    let key = key("fingerprint-a");
    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);
    let first = registry.key_health_snapshot(&key).unwrap();
    assert!(
        first.cooldown_remaining >= Duration::from_secs(48)
            && first.cooldown_remaining <= Duration::from_secs(72),
        "first credential strike should cool ~60s (first-strike setting), got {:?}",
        first.cooldown_remaining
    );

    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);
    let second = registry.key_health_snapshot(&key).unwrap();
    assert!(
        second.cooldown_remaining >= Duration::from_secs(24 * 60)
            && second.cooldown_remaining <= Duration::from_secs(36 * 60),
        "second credential strike should escalate to the ~30min key curve, got {:?}",
        second.cooldown_remaining
    );
}

#[tokio::test(start_paused = true)]
async fn credentials_first_strike_honors_registry_tuning_and_key_quota_unaffected() {
    // T5: the first-strike window is runtime-tunable; KeyQuota (quota-style
    // 429 family) keeps its plain 30s base and is not shortened.
    let mut registry =
        RouteHealthRegistry::new_with_runtime_tuning(16, 16, vec![100, 200], 3, 4, 3, 300, 3000, 2);
    let key = key("fingerprint-a");
    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);
    let first = registry.key_health_snapshot(&key).unwrap();
    assert!(
        first.cooldown_remaining >= Duration::from_millis(1_600)
            && first.cooldown_remaining <= Duration::from_millis(2_400),
        "configured first strike of 2s should be honored, got {:?}",
        first.cooldown_remaining
    );

    let mut quota_registry = RouteHealthRegistry::new(16, 16);
    quota_registry.observe_key_failure(&key, RouteFailureClass::KeyQuota, None);
    let quota = quota_registry.key_health_snapshot(&key).unwrap();
    assert!(
        quota.cooldown_remaining >= Duration::from_secs(24)
            && quota.cooldown_remaining <= Duration::from_secs(36),
        "KeyQuota first hit must keep the 30s base, got {:?}",
        quota.cooldown_remaining
    );
}

#[tokio::test(start_paused = true)]
async fn same_base_url_upstreams_keep_route_health_fully_isolated() {
    let shared_base_url = "https://shared-provider.example/v1";
    let upstream_a = UpstreamConfig {
        id: "up-account-a".into(),
        base_url: shared_base_url.into(),
        api_key: "key-a".into(),
        ..snapshot_upstream(true)
    };
    let upstream_b = UpstreamConfig {
        id: "up-account-b".into(),
        base_url: shared_base_url.into(),
        api_key: "key-b".into(),
        ..snapshot_upstream(true)
    };
    let route_for = |upstream: &UpstreamConfig| RouteHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint: upstream_key_fingerprint(&upstream.id, &upstream.api_key),
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    };
    let key_for = |route: &RouteHealthKey| KeyHealthKey {
        upstream_id: route.upstream_id.clone(),
        key_fingerprint: route.key_fingerprint.clone(),
    };
    let route_a = route_for(&upstream_a);
    let route_b = route_for(&upstream_b);
    let key_a = key_for(&route_a);
    let key_b = key_for(&route_b);
    let mut registry = RouteHealthRegistry::new(16, 16);

    registry.observe_route_failure(&route_a, RouteFailureClass::TransientServer, None, false);

    assert!(matches!(
        registry.reserve(&route_a, &key_a),
        RouteAvailability::Cooling {
            class: RouteFailureClass::TransientServer,
            ..
        }
    ));
    assert!(matches!(
        registry.reserve(&route_b, &key_b),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn route_failure_isolated_from_another_model_on_the_same_key() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let key = key("fingerprint-a");
    let failed = route("fingerprint-a", "glm-5.2");
    let healthy = route("fingerprint-a", "glm-4.7");

    registry.observe_route_failure(&failed, RouteFailureClass::CapacityUnavailable, None, false);
    assert!(matches!(
        registry.reserve(&failed, &key),
        RouteAvailability::Cooling {
            class: RouteFailureClass::CapacityUnavailable,
            ..
        }
    ));
    assert!(matches!(
        registry.reserve(&healthy, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn concurrency_saturation_uses_configured_probe_sequence() {
    let mut registry = RouteHealthRegistry::new_with_concurrency_probe_delays(
        16,
        16,
        vec![100, 200, 400, 800, 1_000, 2_000],
    );
    let key = key("fingerprint-a");
    let route = route("fingerprint-a", "glm-5.2");

    registry.observe_route_failure(&route, RouteFailureClass::ConcurrencySaturated, None, false);
    let first = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(first.cooldown_remaining, Duration::from_millis(100));

    let mut previous_delay = Duration::from_millis(100);
    for expected_delay in [200, 400, 800, 1_000, 2_000, 2_000] {
        tokio::time::advance(previous_delay).await;
        let lease = match registry.reserve(&route, &key) {
            RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
            other => panic!("expected concurrency probe lease, got {other:?}"),
        };
        registry.finish(
            lease,
            RouteOutcome::RouteFailure {
                class: RouteFailureClass::ConcurrencySaturated,
                upstream_status: None,
                repeat_within_request: false,
                sole_candidate: false,
                shared_host_failure_domain: false,
            },
        );
        let expected_delay = Duration::from_millis(expected_delay);
        assert_eq!(
            registry
                .route_health_snapshot(&route)
                .unwrap()
                .cooldown_remaining,
            expected_delay
        );
        previous_delay = expected_delay;
    }
}

#[tokio::test(start_paused = true)]
async fn concurrency_half_open_uncertainty_reapplies_current_delay() {
    let mut registry =
        RouteHealthRegistry::new_with_concurrency_probe_delays(16, 16, vec![100, 200]);
    let key = key("fingerprint-a");
    let route = route("fingerprint-a", "glm-5.2");

    registry.observe_route_failure(&route, RouteFailureClass::ConcurrencySaturated, None, false);
    tokio::time::advance(Duration::from_millis(100)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected concurrency probe lease, got {other:?}"),
    };
    registry.finish(
        lease,
        RouteOutcome::UncertainRouteFailure(RouteFailureClass::Transport),
    );

    let state = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(
        state.last_failure_class,
        Some(RouteFailureClass::ConcurrencySaturated)
    );
    assert_eq!(state.consecutive_failures, 1);
    assert_eq!(state.cooldown_remaining, Duration::from_millis(100));
}

#[tokio::test(start_paused = true)]
async fn concurrency_retry_after_is_authoritative() {
    let mut registry =
        RouteHealthRegistry::new_with_concurrency_probe_delays(16, 16, vec![100, 200]);
    let key = key("fingerprint-a");
    let route = route("fingerprint-a", "glm-5.2");

    registry.observe_route_failure(
        &route,
        RouteFailureClass::ConcurrencySaturated,
        Some(Duration::from_millis(1_500)),
        false,
    );
    assert_eq!(
        registry
            .route_health_snapshot(&route)
            .unwrap()
            .cooldown_remaining,
        Duration::from_millis(1_500)
    );

    tokio::time::advance(Duration::from_millis(1_499)).await;
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(lease) if lease.is_half_open()
    ));
}

#[tokio::test(start_paused = true)]
async fn stale_healthy_success_does_not_clear_newer_concurrency_saturation() {
    let mut registry = RouteHealthRegistry::new_with_concurrency_probe_delays(16, 16, vec![100]);
    let key = key("fingerprint-a");
    let route = route("fingerprint-a", "glm-5.2");

    let stale = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) => lease,
        other => panic!("expected healthy lease, got {other:?}"),
    };
    registry.observe_route_failure(&route, RouteFailureClass::ConcurrencySaturated, None, false);
    registry.finish(stale, RouteOutcome::Success);

    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling {
            class: RouteFailureClass::ConcurrencySaturated,
            retry_after,
            ..
        } if retry_after > Duration::ZERO
    ));
}

#[tokio::test(start_paused = true)]
async fn explicit_retry_after_is_a_lower_bound_and_failure_streak_resets() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");

    registry.observe_route_failure(
        &route,
        RouteFailureClass::RateLimited,
        Some(Duration::from_secs(73)),
        false,
    );
    let first = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(first.consecutive_failures, 1);
    assert!(first.cooldown_remaining >= Duration::from_secs(73));

    tokio::time::advance(Duration::from_secs(74)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) => lease,
        other => panic!("expected route recovery, got {other:?}"),
    };
    registry.finish(lease, RouteOutcome::Success);
    registry.observe_route_failure(&route, RouteFailureClass::RateLimited, None, false);
    assert_eq!(
        registry
            .route_health_snapshot(&route)
            .unwrap()
            .consecutive_failures,
        1
    );
}

#[tokio::test(start_paused = true)]
async fn aggregate_failure_never_blocks_a_recovered_exact_route() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");
    let aggregate = chat_responses_codex::state::RouteSetAggregateKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    };

    registry.observe_route_set_failure(
        &aggregate,
        RouteFailureClass::TransientServer,
        Some(Duration::from_secs(60)),
    );
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn aggregate_snapshot_retains_failure_class_step_and_retry_after() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let aggregate = RouteSetAggregateKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    };

    registry.observe_route_set_failure(
        &aggregate,
        RouteFailureClass::RateLimited,
        Some(Duration::from_secs(7)),
    );
    let snapshot = registry
        .route_set_health_snapshot(&aggregate)
        .expect("aggregate observation should be inspectable");
    assert_eq!(snapshot.consecutive_failures, 1);
    assert_eq!(
        snapshot.last_failure_class,
        Some(RouteFailureClass::RateLimited)
    );
    assert_eq!(snapshot.cooldown_remaining, Duration::from_secs(7));
}

#[tokio::test(start_paused = true)]
async fn uncertain_route_result_releases_but_does_not_clear_key_half_open_state() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let key = key("fingerprint-a");
    let route = route("fingerprint-a", "glm-5.2");
    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);

    tokio::time::advance(Duration::from_secs(20 * 60)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected key half-open permit, got {other:?}"),
    };
    registry.finish(
        lease,
        RouteOutcome::UncertainRouteFailure(RouteFailureClass::TransientServer),
    );

    let key_state = registry.key_health_snapshot(&key).unwrap();
    assert_eq!(key_state.consecutive_failures, 1);
    assert_eq!(
        key_state.last_failure_class,
        Some(RouteFailureClass::Credentials)
    );
    assert!(!key_state.half_open);
    assert_eq!(
        registry
            .route_health_snapshot(&route)
            .unwrap()
            .last_failure_class,
        Some(RouteFailureClass::TransientServer)
    );
}

#[tokio::test(start_paused = true)]
async fn stale_failure_streak_restarts_and_local_jitter_is_deterministic() {
    let route = route("fingerprint-a", "glm-5.2");
    let mut first = RouteHealthRegistry::new(16, 16);
    let mut second = RouteHealthRegistry::new(16, 16);

    first.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    second.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    assert_eq!(
        first
            .route_health_snapshot(&route)
            .unwrap()
            .cooldown_remaining,
        second
            .route_health_snapshot(&route)
            .unwrap()
            .cooldown_remaining
    );

    tokio::time::advance(Duration::from_secs(11 * 60)).await;
    first.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    assert_eq!(
        first
            .route_health_snapshot(&route)
            .unwrap()
            .consecutive_failures,
        1
    );
}

#[tokio::test(start_paused = true)]
async fn health_registry_keeps_active_half_open_leases_when_bounded() {
    let mut registry = RouteHealthRegistry::new(2, 2);
    let key_a = key("fingerprint-a");
    let route_a = route("fingerprint-a", "glm-5.2");
    registry.observe_route_failure(
        &route_a,
        RouteFailureClass::CapacityUnavailable,
        None,
        false,
    );
    tokio::time::advance(Duration::from_secs(20)).await;
    let lease = match registry.reserve(&route_a, &key_a) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };

    for index in 0..8 {
        let fingerprint = format!("fingerprint-{index}");
        let route = route(&fingerprint, "glm-5.2");
        registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    }

    assert!(registry.route_count() <= 2);
    assert!(registry.contains_route(&route_a));
    registry.finish(lease, RouteOutcome::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn route_and_key_half_open_leases_are_acquired_atomically() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let key = key("fingerprint-a");
    let route = route("fingerprint-a", "glm-5.2");
    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);
    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);

    tokio::time::advance(Duration::from_secs(20 * 60)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected combined half-open permit, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy {
            class: RouteFailureClass::Credentials,
            ..
        }
    ));
    registry.finish(lease, RouteOutcome::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn temporary_recovery_uses_larger_route_and_key_delay() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-recovery", "glm-5.2");
    let key = key("fingerprint-recovery");

    registry.observe_route_failure(
        &route,
        RouteFailureClass::TransientServer,
        Some(Duration::from_secs(40)),
        false,
    );
    registry.observe_key_failure(
        &key,
        RouteFailureClass::KeyQuota,
        Some(Duration::from_secs(90)),
    );
    let route_delay = registry
        .route_health_snapshot(&route)
        .expect("route state")
        .cooldown_remaining;
    let key_delay = registry
        .key_health_snapshot(&key)
        .expect("key state")
        .cooldown_remaining;

    let recovery = registry
        .earliest_temporary_recovery(std::slice::from_ref(&route))
        .expect("temporary route should expose recovery");

    assert_eq!(recovery.class, RouteFailureClass::KeyQuota);
    assert_eq!(recovery.retry_after, route_delay.max(key_delay));
    assert!(recovery.retry_after >= Duration::from_secs(90));
}

#[tokio::test(start_paused = true)]
async fn temporary_recovery_chooses_earliest_exact_route() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let slower = route("fingerprint-slower", "glm-5.2");
    let faster = route("fingerprint-faster", "glm-5.2");
    registry.observe_route_failure(
        &slower,
        RouteFailureClass::TransientServer,
        Some(Duration::from_secs(80)),
        false,
    );
    registry.observe_route_failure(
        &faster,
        RouteFailureClass::TransientServer,
        Some(Duration::from_secs(45)),
        false,
    );
    let slower_recovery = registry
        .earliest_temporary_recovery(std::slice::from_ref(&slower))
        .expect("slower route recovery");
    let faster_recovery = registry
        .earliest_temporary_recovery(std::slice::from_ref(&faster))
        .expect("faster route recovery");

    let combined = registry
        .earliest_temporary_recovery(&[slower, faster])
        .expect("one temporary route should recover");

    assert_eq!(combined, faster_recovery);
    assert!(combined.retry_after < slower_recovery.retry_after);
}

#[tokio::test(start_paused = true)]
async fn temporary_recovery_excludes_a_credential_blocked_exact_route() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-credentials", "glm-5.2");
    let key = key("fingerprint-credentials");
    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);

    assert!(registry
        .earliest_temporary_recovery(std::slice::from_ref(&route))
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn temporary_recovery_query_is_read_only_and_reports_half_open_busy() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-half-open", "glm-5.2");
    let key = key("fingerprint-half-open");
    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let cooldown = registry
        .route_health_snapshot(&route)
        .expect("route state")
        .cooldown_remaining;
    tokio::time::advance(cooldown + Duration::from_nanos(1)).await;

    let ready_recovery = registry
        .earliest_temporary_recovery(std::slice::from_ref(&route))
        .expect("expired temporary state should be immediately recoverable");
    assert_eq!(ready_recovery.retry_after, Duration::ZERO);

    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("read-only query must leave half-open lease available, got {other:?}"),
    };
    let busy_recovery = registry
        .earliest_temporary_recovery(std::slice::from_ref(&route))
        .expect("active half-open route is temporarily busy");
    assert!(busy_recovery.retry_after >= Duration::from_secs(1));
    registry.finish(lease, RouteOutcome::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn per_upstream_capacity_evicts_only_from_the_full_upstream() {
    let mut registry = RouteHealthRegistry::new(8, 1);
    let route_a = route("fingerprint-a", "glm-5.2");
    let route_c = route("fingerprint-c", "glm-4.7");
    let route_b = RouteHealthKey {
        upstream_id: "up-2".into(),
        key_fingerprint: "fingerprint-b".into(),
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    };
    registry.observe_route_failure(&route_a, RouteFailureClass::TransientServer, None, false);
    registry.observe_route_failure(&route_b, RouteFailureClass::TransientServer, None, false);
    registry.observe_route_failure(&route_c, RouteFailureClass::TransientServer, None, false);

    assert_eq!(registry.route_count(), 2);
    assert!(!registry.contains_route(&route_a));
    assert!(registry.contains_route(&route_b));
    assert!(registry.contains_route(&route_c));
}

#[tokio::test(start_paused = true)]
async fn app_state_permit_drop_releases_half_open_without_punishment() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let key = key("fingerprint-a");
    let route = route("fingerprint-a", "glm-5.2");
    state
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None, false)
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(12)).await;

    let permit = match state.reserve_route_health(&route, &key).await.unwrap() {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    drop(permit);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert!(matches!(
        state.reserve_route_health(&route, &key).await.unwrap(),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn expired_route_half_open_lease_releases_for_next_caller() {
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let route = route("fingerprint-expired-lease", "glm-5.2");
    let key = key("fingerprint-expired-lease");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    tokio::time::advance(Duration::from_secs(12)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    // 超过半开租约 TTL：旧租约过期，下一个调用者必须能拿到新租约
    tokio::time::advance(Duration::from_secs(301)).await;
    match registry.reserve(&route, &key) {
        RouteAvailability::Ready(new_lease) => {
            assert!(new_lease.is_half_open());
            // 旧租约迟到 finish 不得破坏新租约
            registry.finish(lease, RouteOutcome::Cancelled);
            registry.finish(new_lease, RouteOutcome::Success);
        }
        other => panic!("expired half-open lease must release for next caller, got {other:?}"),
    }
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn expired_key_half_open_lease_releases_for_next_caller() {
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let key = key("fingerprint-expired-key-lease");
    let route = route("fingerprint-expired-key-lease", "glm-5.2");

    registry.observe_key_failure(&key, RouteFailureClass::KeyQuota, None);
    tokio::time::advance(Duration::from_secs(40)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open key permit, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    tokio::time::advance(Duration::from_secs(301)).await;
    match registry.reserve(&route, &key) {
        RouteAvailability::Ready(new_lease) => {
            registry.finish(lease, RouteOutcome::Cancelled);
            registry.finish(new_lease, RouteOutcome::Success);
        }
        other => panic!("expired key half-open lease must release, got {other:?}"),
    }
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn half_open_busy_reports_wait_bounded_by_exclusive_window() {
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let route = route("fingerprint-busy-recovery", "glm-5.2");
    let key = key("fingerprint-busy-recovery");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    tokio::time::advance(Duration::from_secs(12)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };

    let busy = match registry.reserve(&route, &key) {
        RouteAvailability::HalfOpenBusy { retry_after, .. } => retry_after,
        other => panic!("expected half-open busy, got {other:?}"),
    };
    // 调度语义：busy 时乐观轮询 1s，探针通常在数秒内完成
    assert_eq!(busy, Duration::from_secs(1));

    // T3：独占窗口（3s）还未过去时，诚实等待时间 =
    // min(剩余独占窗口, 剩余租约) = 当前窗口剩余 3s，而不是 300s 租约 TTL。
    let early_recovery = registry
        .earliest_temporary_recovery(std::slice::from_ref(&route))
        .expect("active half-open route is temporarily busy");
    assert_eq!(
        early_recovery.half_open_remaining,
        Some(Duration::from_secs(3)),
        "T3: before the exclusive window elapses the honest wait is min(lease, window) = 3s, got {:?}",
        early_recovery.half_open_remaining
    );

    tokio::time::advance(Duration::from_secs(100)).await;
    let recovery = registry
        .earliest_temporary_recovery(std::slice::from_ref(&route))
        .expect("active half-open route is temporarily busy");
    assert_eq!(
        recovery.retry_after,
        Duration::from_secs(1),
        "gateway must poll optimistically"
    );
    // T3：窗口早已过去（3s vs 已过 100s），再告诉客户端等剩余租约（~200s）
    // 是谎言——窗口之后并发请求已被放行（T1），诚实等待时间回到 1s 轮询下限。
    assert_eq!(
        recovery.half_open_remaining,
        Some(Duration::from_secs(1)),
        "T3: after the exclusive window elapses the honest wait is the 1s poll floor, got {:?}",
        recovery.half_open_remaining
    );

    registry.finish(lease, RouteOutcome::Cancelled);
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn edge_proxy_error_cools_briefly_and_never_escalates_streak() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("edge-proxy-brief", "glm-5.2");
    let key = key("edge-proxy-brief");

    // Edge proxy errors are cooldown classes (short base), unlike pure
    // request rejections which never cool.
    registry.observe_route_failure(&route, RouteFailureClass::EdgeProxyError, None, false);
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));
    let first = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(first.consecutive_failures, 1);
    assert!(
        first.cooldown_remaining <= Duration::from_secs(5),
        "edge proxy cooldown must be short, got {:?}",
        first.cooldown_remaining
    );

    // Repeated identical edge proxy failures must not escalate the streak or
    // lengthen the cooldown beyond the short base.
    tokio::time::advance(Duration::from_secs(10)).await;
    registry.observe_route_failure(&route, RouteFailureClass::EdgeProxyError, None, false);
    let second = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(second.consecutive_failures, 1);
    assert!(
        second.cooldown_remaining <= Duration::from_secs(5),
        "edge proxy cooldown must not escalate, got {:?}",
        second.cooldown_remaining
    );
}

#[tokio::test(start_paused = true)]
async fn reserve_route_health_probe_ignores_cooldown_and_is_single_flight() {
    // A3: while a route is cooling, the last-resort probe API ignores the
    // remaining cooldown and grants a single-flight half-open lease; a second
    // caller is busy until the first finishes, and a successful probe clears
    // the cooldown entirely.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let route = route("early-probe-single-flight", "glm-5.2");
    let key = key("early-probe-single-flight");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));

    let lease = match registry.reserve_route_health_probe(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected early half-open lease, got {other:?}"),
    };
    // Single-flight: the second early probe on the same route is busy while
    // the lease is active, even though the cooldown has not elapsed.
    assert!(matches!(
        registry.reserve_route_health_probe(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    // A successful probe clears the cooldown and the route is healthy again:
    // the entry stays (local registry keeps cleared entries) but the failure
    // streak is reset and normal reserves are ready immediately.
    registry.finish(lease, RouteOutcome::Success);
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(snapshot.consecutive_failures, 0);
    assert!(snapshot.cooldown_remaining.is_zero());
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn reserve_route_health_probe_enforces_one_second_interval_per_route() {
    // A3: after an early probe (even a cancelled one) the same route refuses
    // another early probe for HALF_OPEN_BUSY_RETRY (1s); normal reserves stay
    // cooling during the window and a fresh probe is granted after it.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        3000,
        60,
    );
    let route = route("early-probe-interval", "glm-5.2");
    let key = key("early-probe-interval");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);

    let first = match registry.reserve_route_health_probe(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected early half-open lease, got {other:?}"),
    };
    // Released without a physical attempt: the interval still applies.
    registry.finish(first, RouteOutcome::Cancelled);

    let busy = match registry.reserve_route_health_probe(&route, &key) {
        RouteAvailability::HalfOpenBusy { retry_after, .. } => retry_after,
        other => panic!("expected interval-busy, got {other:?}"),
    };
    assert!(
        busy <= Duration::from_secs(1),
        "interval refusal must report the remaining 1s window, got {busy:?}"
    );
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));

    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        registry.reserve_route_health_probe(&route, &key),
        RouteAvailability::Ready(lease) if lease.is_half_open()
    ));
}

#[tokio::test(start_paused = true)]
async fn reserve_route_health_probe_failure_stays_capped_and_keeps_interval() {
    // A3: a failing early probe follows the half-open failure path: the step
    // cannot exceed ROUTE_HALF_OPEN_FAILURE_STEP_CAP and the 1s probe window
    // stays armed for the next caller.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        8,
        300,
        3000,
        60,
    );
    let route = route("early-probe-capped-step", "glm-5.2");
    let key = key("early-probe-capped-step");

    // Seed a step of 5 through independent failures; a probe failure must not
    // push it to 6.
    for _ in 0..5 {
        registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    }
    let seeded = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(seeded.consecutive_failures, 5);

    let lease = match registry.reserve_route_health_probe(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected early half-open lease, got {other:?}"),
    };
    registry.finish(
        lease,
        RouteOutcome::RouteFailure {
            class: RouteFailureClass::TransientServer,
            upstream_status: Some(502),
            repeat_within_request: false,
            sole_candidate: false,
            shared_host_failure_domain: false,
        },
    );
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(
        snapshot.consecutive_failures, 5,
        "an early probe failure must stay capped at the half-open step"
    );
    assert!(snapshot.cooldown_remaining > Duration::ZERO);

    // The interval persists after the failed probe: no immediate re-probe.
    assert!(matches!(
        registry.reserve_route_health_probe(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn reserve_route_health_probe_refuses_when_key_cooling_or_route_healthy() {
    // A3 guards: a cooling key (credentials/quota quarantine) must not be
    // probed, and a route without health state has nothing to probe.
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("early-probe-guards", "glm-5.2");
    let key = key("early-probe-guards");

    assert!(matches!(
        registry.reserve_route_health_probe(&route, &key),
        RouteAvailability::Cooling { .. }
    ));

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    registry.observe_key_failure(&key, RouteFailureClass::Credentials, None);
    assert!(matches!(
        registry.reserve_route_health_probe(&route, &key),
        RouteAvailability::Cooling { .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn half_open_exclusive_window_admits_requests_after_window_elapses() {
    // T1: a half-open probe occupies a recovering route only for the
    // exclusive window (default 3s), not for the whole lease (300s).
    // After the window, concurrent requests are admitted WITHOUT a half-open
    // lease; their success still clears the route via `same_observation`.
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("exclusive-window", "glm-5.2");
    let key = key("exclusive-window");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    // Step-1 transient cooldown is jittered up to 12s (10s base).
    tokio::time::advance(Duration::from_secs(13)).await;
    let first = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected first half-open lease, got {other:?}"),
    };

    // Within the exclusive window the route is still single-flight.
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    // The window (3s) elapses while the original lease is still alive
    // (half-open TTL 300s): concurrent requests must now be admitted.
    tokio::time::advance(Duration::from_millis(3_001)).await;
    let second = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if !lease.is_half_open() => lease,
        other => panic!("expected no-lease admission after window, got {other:?}"),
    };

    // A successful request admitted after the window clears the route via
    // the same-observation path, restoring full concurrency.
    registry.finish(second, RouteOutcome::Success);
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert!(!snapshot.half_open);
    assert_eq!(snapshot.cooldown_remaining, Duration::ZERO);
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(lease) if !lease.is_half_open()
    ));

    // The original prober can still finish its lease without disturbing the
    // already-cleared state.
    registry.finish(first, RouteOutcome::Success);
}

#[tokio::test(start_paused = true)]
async fn half_open_exclusive_window_zero_never_blocks_concurrent_requests() {
    // T1: window = 0 disables the exclusivity window entirely: the first
    // prober still holds the half-open lease, but every concurrent request
    // is admitted without a lease immediately.
    let mut registry =
        RouteHealthRegistry::new_with_runtime_tuning(16, 16, vec![100, 200], 3, 4, 3, 300, 0, 60);
    let route = route("exclusive-window-zero", "glm-5.2");
    let key = key("exclusive-window-zero");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    tokio::time::advance(Duration::from_secs(13)).await;
    let first = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open lease, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(lease) if !lease.is_half_open()
    ));
    assert!(matches!(
        registry.reserve_route_health_probe(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));
    registry.finish(first, RouteOutcome::Success);
}

#[tokio::test(start_paused = true)]
async fn half_open_exclusive_window_max_degrades_to_single_flight() {
    // T1: a very large window reproduces the pre-T1 behavior: the route stays
    // busy for the whole half-open lease lifetime and is then reclaimed.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        600_000,
        60,
    );
    let route = route("exclusive-window-max", "glm-5.2");
    let key = key("exclusive-window-max");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    tokio::time::advance(Duration::from_secs(13)).await;
    let first = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open lease, got {other:?}"),
    };
    // Still busy long after the default 3s window would have elapsed.
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));
    // The lease expires at the half-open TTL (300s) and may be reclaimed.
    tokio::time::advance(Duration::from_secs(240)).await;
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(lease) if lease.is_half_open()
    ));
    registry.finish(first, RouteOutcome::Success);
}

#[tokio::test(start_paused = true)]
async fn half_open_exclusive_window_update_runtime_tuning_applies_to_live_leases() {
    // T1: shrinking the exclusive window at runtime must apply to leases that
    // are already in flight (immediate toggle; window=0 unblocks everything).
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        3,
        4,
        3,
        300,
        600_000,
        60,
    );
    let route = route("exclusive-window-tuning", "glm-5.2");
    let key = key("exclusive-window-tuning");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    tokio::time::advance(Duration::from_secs(13)).await;
    let first = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open lease, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    registry.update_runtime_tuning(vec![100, 200], 3, 4, 3, 300, 0, 60);
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Ready(lease) if !lease.is_half_open()
    ));
    registry.finish(first, RouteOutcome::Success);
}

#[tokio::test(start_paused = true)]
async fn half_open_exclusive_window_does_not_affect_early_probe_single_flight() {
    // T1 invariant: the A3 last-resort early-probe path stays strictly
    // single-flight regardless of the exclusive window (probe-held leases
    // pin the route until the probe finishes).
    let mut registry =
        RouteHealthRegistry::new_with_runtime_tuning(16, 16, vec![100, 200], 3, 4, 3, 300, 0, 60);
    let route = route("probe-single-flight-window", "glm-5.2");
    let key = key("probe-single-flight-window");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    // The route is still deep in cooldown; the early probe ignores it.
    let first = match registry.reserve_route_health_probe(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open probe lease, got {other:?}"),
    };
    // Even with window = 0 a second probe must not be admitted while the
    // first probe lease is alive.
    assert!(matches!(
        registry.reserve_route_health_probe(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));
    // And the cooling route stays cooling for regular reserves.
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));
    registry.finish(first, RouteOutcome::Success);
}

#[tokio::test(start_paused = true)]
async fn early_probe_exclusivity_ends_with_the_cooldown_not_the_lease() {
    // T9/F2: an A3 early-probe lease used to pin its route exclusively for
    // the whole half-open TTL (300s), so a probe with a slow first output
    // kept an otherwise-recovered route at one concurrent request long after
    // its cooldown ended (the tail of C1). The exclusive window now ends with
    // the route's remaining cooldown — strict single-flight while cooling,
    // then the ordinary exclusive window instead of the full lease.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        10,
        60,
        3,
        300,
        3_000,
        60,
    );
    let route = route("early-probe-exclusivity", "glm-5.2");
    let key = key("early-probe-exclusivity");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let cooldown_remaining = registry
        .route_health_snapshot(&route)
        .unwrap()
        .cooldown_remaining;

    // The early probe takes a half-open lease while the route is still
    // cooling (cooldown itself is deliberately ignored by the probe path).
    let probe = match registry.reserve_route_health_probe(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open probe lease, got {other:?}"),
    };

    // Invariant: cooldown checks precede half-open lease checks, so regular
    // reserves still see Cooling while the route is cooling.
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));

    // Advance past the cooldown plus the ordinary exclusive window. The probe
    // lease is still alive (TTL 300s), but its exclusivity ended with the
    // cooldown: a regular reserve is admitted as a plain ready lease instead
    // of HalfOpenBusy for the rest of the lease.
    tokio::time::advance(cooldown_remaining + Duration::from_secs(3) + Duration::from_millis(1))
        .await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if !lease.is_half_open() => lease,
        other => panic!("expected plain ready lease after cooldown, got {other:?}"),
    };
    registry.finish(probe, RouteOutcome::Success);
    registry.finish(lease, RouteOutcome::Success);
}

#[tokio::test(start_paused = true)]
async fn early_probe_at_cooldown_tail_keeps_a_full_exclusive_window() {
    // T9/F2 boundary: a probe taken while the remaining cooldown is shorter
    // than the exclusive window must still hold exclusivity for at least one
    // full window, measured from the probe — it must not be immediately
    // pierced when the cooldown ends mid-window.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        10,
        60,
        3,
        300,
        3_000,
        60,
    );
    let route = route("early-probe-tail-window", "glm-5.2");
    let key = key("early-probe-tail-window");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    let cooldown_remaining = registry
        .route_health_snapshot(&route)
        .unwrap()
        .cooldown_remaining;
    // Walk into the tail so that only 2s of cooldown remain (< 3s window).
    assert!(cooldown_remaining > Duration::from_secs(3));
    tokio::time::advance(cooldown_remaining - Duration::from_secs(2)).await;

    let probe = match registry.reserve_route_health_probe(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open probe lease at cooldown tail, got {other:?}"),
    };

    // Still cooling for 2s more: regular reserves see Cooling (order
    // invariant), so the probe exclusivity manifesting after the cooldown is
    // what the next two steps observe.
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::Cooling { .. }
    ));

    // Cooldown just ended, but the exclusive window (3s from the probe) still
    // has 1s to run — regular reserves are HalfOpenBusy, not Ready.
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));

    // The full window has elapsed: admitted as a plain ready lease (old
    // behavior kept HalfOpenBusy until the 300s lease expired).
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(100)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if !lease.is_half_open() => lease,
        other => panic!("expected plain ready lease after full exclusive window, got {other:?}"),
    };
    registry.finish(probe, RouteOutcome::Success);
    registry.finish(lease, RouteOutcome::Success);
}
#[tokio::test(start_paused = true)]
async fn t13_equivalence_effective_cooldown_bounded_by_t11_ceiling() {
    // T1.1/T1.3 equivalence proof (plan §0.5 / §4.1): with base=2,
    // max_step=3 (curve ceiling 2 << 2 = 8s), cooldown_max=15s and the T1.2
    // cooldown cap=5s (applied by the gateway before this registry ever sees
    // the upstream Retry-After), the effective cooldown must stay <= 15s for
    // every failure step and every upstream hint — it can never exceed the
    // 30s intra-gateway retry wait budget, so a `WaitBudget` give-up is
    // structurally impossible regardless of what the upstream sends.
    let cooldown_cap = Duration::from_secs(5);
    let cooldown_max = Duration::from_secs(15);
    for raw_retry_after_secs in [0_u64, 1, 5, 28, 60, 300, 900, 1_800, 3_600] {
        let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
            16,
            16,
            vec![100, 200],
            2,
            15,
            3,
            300,
            3_000,
            60,
        );
        let route = route(&format!("t13-equiv-{raw_retry_after_secs}"), "glm-5.2");
        // The gateway clamps the upstream hint to the T1.2 cooldown cap before
        // observing; model that here.
        let clamped = Duration::from_secs(raw_retry_after_secs).min(cooldown_cap);
        for step in 1..=10u32 {
            registry.observe_route_failure(
                &route,
                RouteFailureClass::TransientServer,
                Some(clamped),
                false,
            );
            let snapshot = registry.route_health_snapshot(&route).unwrap();
            assert_eq!(
                snapshot.consecutive_failures,
                step.min(3),
                "step must be capped at max_step=3"
            );
            assert!(
                snapshot.cooldown_remaining <= cooldown_max,
                "raw={raw_retry_after_secs}s step={step} => cooldown {:?} exceeds the {cooldown_max:?} ceiling",
                snapshot.cooldown_remaining,
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn t12_upstream_hint_capped_so_local_curve_wins_never_28() {
    // Regression guard for the 2026-08-25 root cause: an upstream 502 carrying
    // `Retry-After: 28` must never pin the route cooldown to 28s. The gateway
    // clamps the hint to upstream_retry_after_cooldown_cap_seconds (5s)
    // before this registry sees it, and the effective cooldown is
    // `max(local, clamped)`, so the local curve (here 10s at step 1) wins —
    // the raw 28s can never surface.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        10,
        300,
        3,
        300,
        3_000,
        60,
    );
    let route = route("t12-hint-capped", "glm-5.2");

    // Simulate the gateway clamp: raw 28s -> 5s.
    let clamped = Duration::from_secs(28).min(Duration::from_secs(5));
    registry.observe_route_failure(
        &route,
        RouteFailureClass::TransientServer,
        Some(clamped),
        false,
    );
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert!(
        snapshot.cooldown_remaining < Duration::from_secs(28),
        "raw 28s hint must never surface as the route cooldown, got {:?}",
        snapshot.cooldown_remaining,
    );
    // Local step-1 cooldown with base=10 is 8..12s, above the 5s cap, so the
    // local curve dominates.
    assert!(
        snapshot.cooldown_remaining >= Duration::from_secs(8),
        "local curve should dominate the clamped hint, got {:?}",
        snapshot.cooldown_remaining,
    );
}

#[tokio::test(start_paused = true)]
async fn concurrency_saturated_retry_after_not_cut_by_t12_cooldown_cap() {
    // T1.2 exemption: a ConcurrencySaturated upstream's Retry-After is real
    // slot information. The gateway passes it through unclamped and the
    // registry must honor the full 28s — never the 5s cooldown cap.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        2,
        15,
        3,
        300,
        3_000,
        60,
    );
    let route = route("t12-concurrency-exempt", "glm-5.2");

    registry.observe_route_failure(
        &route,
        RouteFailureClass::ConcurrencySaturated,
        Some(Duration::from_secs(28)),
        false,
    );
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(
        snapshot.cooldown_remaining,
        Duration::from_secs(28),
        "ConcurrencySaturated Retry-After must remain authoritative (exempt from the cooldown cap)"
    );
}

#[tokio::test(start_paused = true)]
async fn t13_non_half_open_failures_cap_step_at_configured_max_step() {
    // T1.3: eight consecutive independent transient failures must cap the
    // failure step at the configured max_step (3), not keep doubling toward
    // the cooldown max. Local-backend side of the invariant; the Redis backend
    // applies the same min(step, max_step) inside route_health_observe.lua /
    // route_health_finish.lua over the same schedule.
    let mut registry = RouteHealthRegistry::new_with_runtime_tuning(
        16,
        16,
        vec![100, 200],
        2,
        300,
        3,
        300,
        3_000,
        60,
    );
    let route = route("t13-step-cap", "glm-5.2");

    for _ in 0..8 {
        registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None, false);
    }
    let snapshot = registry.route_health_snapshot(&route).unwrap();
    assert_eq!(
        snapshot.consecutive_failures, 3,
        "8 transient failures must cap the step at max_step=3"
    );
    // base=2, step=3 => 8s (+/-20%) — far below the 300s max.
    assert!(
        snapshot.cooldown_remaining <= Duration::from_secs(10),
        "cooldown must follow the capped step, got {:?}",
        snapshot.cooldown_remaining,
    );
}
