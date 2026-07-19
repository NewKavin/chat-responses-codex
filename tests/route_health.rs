use chat_responses_codex::capabilities::WireProtocol;
use chat_responses_codex::state::{
    AppConfig, AppState, KeyHealthKey, PersistedState, RouteAvailability, RouteFailureClass,
    RouteHealthKey, RouteHealthRegistry, RouteOutcome,
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

#[tokio::test(start_paused = true)]
async fn route_cooldown_has_one_half_open_lease_and_resets_after_success() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let route = route("fingerprint-a", "glm-5.2");
    let key = key("fingerprint-a");

    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None);
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
        RouteAvailability::Cooling { .. }
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
async fn route_failure_isolated_from_another_model_on_the_same_key() {
    let mut registry = RouteHealthRegistry::new(16, 16);
    let key = key("fingerprint-a");
    let failed = route("fingerprint-a", "glm-5.2");
    let healthy = route("fingerprint-a", "glm-4.7");

    registry.observe_route_failure(&failed, RouteFailureClass::CapacityUnavailable, None);
    assert!(matches!(
        registry.reserve(&failed, &key),
        RouteAvailability::Cooling { .. }
    ));
    assert!(matches!(
        registry.reserve(&healthy, &key),
        RouteAvailability::Ready(_)
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
    registry.observe_route_failure(&route, RouteFailureClass::RateLimited, None);
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

    first.observe_route_failure(&route, RouteFailureClass::TransientServer, None);
    second.observe_route_failure(&route, RouteFailureClass::TransientServer, None);
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
    first.observe_route_failure(&route, RouteFailureClass::TransientServer, None);
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
    registry.observe_route_failure(&route_a, RouteFailureClass::CapacityUnavailable, None);
    tokio::time::advance(Duration::from_secs(20)).await;
    let lease = match registry.reserve(&route_a, &key_a) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected half-open permit, got {other:?}"),
    };

    for index in 0..8 {
        let fingerprint = format!("fingerprint-{index}");
        let route = route(&fingerprint, "glm-5.2");
        registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None);
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
    registry.observe_route_failure(&route, RouteFailureClass::TransientServer, None);

    tokio::time::advance(Duration::from_secs(20 * 60)).await;
    let lease = match registry.reserve(&route, &key) {
        RouteAvailability::Ready(lease) if lease.is_half_open() => lease,
        other => panic!("expected combined half-open permit, got {other:?}"),
    };
    assert!(matches!(
        registry.reserve(&route, &key),
        RouteAvailability::HalfOpenBusy { .. }
    ));
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
    registry.observe_route_failure(&route_a, RouteFailureClass::TransientServer, None);
    registry.observe_route_failure(&route_b, RouteFailureClass::TransientServer, None);
    registry.observe_route_failure(&route_c, RouteFailureClass::TransientServer, None);

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
        .observe_route_failure(&route, RouteFailureClass::TransientServer, None)
        .await;
    tokio::time::advance(Duration::from_secs(12)).await;

    let permit = match state.reserve_route_health(&route, &key).await {
        RouteAvailability::Ready(permit) if permit.is_half_open() => permit,
        other => panic!("expected half-open permit, got {other:?}"),
    };
    drop(permit);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert!(matches!(
        state.reserve_route_health(&route, &key).await,
        RouteAvailability::Ready(_)
    ));
}
