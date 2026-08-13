use super::*;
use crate::capabilities::{
    Capability, CapabilityHintKey, CapabilityRuntimeSnapshot, DialectProfileKey, EvidenceState,
    RequestedFeatures, RuntimeCapabilityHints, UpstreamDialectProfile, WireProtocol,
};
use crate::state::PersistedState;
use crate::upstream_feedback::UpstreamFeedbackClassification;
use axum::body::to_bytes;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicUsize;
use tempfile::tempdir;
use tower::ServiceExt;

#[test]
fn route_key_rotation_is_stable_and_spreads_first_choice() {
    let original = vec![
        "key-a".to_string(),
        "key-b".to_string(),
        "key-c".to_string(),
        "key-d".to_string(),
    ];
    let mut first = original.clone();
    rotate_route_keys_for_request(
        &mut first,
        "request-a",
        "upstream-a",
        "glm-5",
        UpstreamProtocol::ChatCompletions,
    );
    let mut repeated = original.clone();
    rotate_route_keys_for_request(
        &mut repeated,
        "request-a",
        "upstream-a",
        "glm-5",
        UpstreamProtocol::ChatCompletions,
    );
    assert_eq!(first, repeated);

    let first_keys = (0..64)
        .map(|index| {
            let mut keys = original.clone();
            rotate_route_keys_for_request(
                &mut keys,
                &format!("request-{index}"),
                "upstream-a",
                "glm-5",
                UpstreamProtocol::ChatCompletions,
            );
            keys[0].clone()
        })
        .collect::<BTreeSet<_>>();
    assert!(first_keys.len() > 1);
}

#[test]
fn exact_continuation_promotion_preserves_rotated_fallback_order() {
    let upstream = UpstreamConfig {
        id: "upstream-a".into(),
        api_key: "key-a".into(),
        api_keys: vec!["key-b".into(), "key-c".into(), "key-d".into()],
        ..Default::default()
    };
    let mut keys = upstream.account_api_keys();
    rotate_route_keys_for_request(
        &mut keys,
        "request-a",
        &upstream.id,
        "glm-5",
        UpstreamProtocol::Responses,
    );
    let rotated = keys.clone();
    let preferred = rotated[2].clone();
    let preferred_fingerprint = route_key_fingerprint(&upstream, &preferred);

    promote_preferred_route_key(&upstream, &mut keys, &preferred_fingerprint);

    assert_eq!(keys[0], preferred);
    assert_eq!(
        keys[1..],
        rotated
            .into_iter()
            .filter(|key| key != &preferred)
            .collect::<Vec<_>>()
    );
}

#[test]
fn stream_timeouts_use_runtime_idle_and_startup_transport_limits() {
    let config = AppConfig {
        upstream_stream_keepalive_interval_seconds: 11,
        upstream_stream_idle_timeout_seconds: 22,
        upstream_stream_max_duration_seconds: 33,
        ..AppConfig::default()
    };
    let mut runtime_settings = RuntimeSettings::from_app_config(&config);
    runtime_settings.upstream_stream_keepalive_interval_seconds = 44;
    runtime_settings.upstream_stream_idle_timeout_seconds = 55;
    runtime_settings.upstream_stream_max_duration_seconds = 66;

    let timeouts = StreamTimeouts::from_sources(&config, &runtime_settings);

    assert_eq!(timeouts.keepalive_interval, Duration::from_secs(11));
    assert_eq!(timeouts.idle_timeout, Duration::from_secs(55));
    assert_eq!(timeouts.max_duration, Duration::from_secs(33));
}

#[test]
fn route_attempts_prefers_temporary_failures_and_shortest_retry() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route-a".into(),
        upstream_status: Some(503),
        class: FailureClass::TransientServer,
        retry_after: Some(Duration::from_secs(30)),
    });
    ledger.record_cooled(AttemptFailure {
        route_id: "route-b".into(),
        upstream_status: Some(429),
        class: FailureClass::RateLimited,
        retry_after: Some(Duration::from_secs(7)),
    });

    assert_eq!(
        ledger.terminal_failure(),
        TerminalFailure::Temporary {
            retry_after: Duration::from_secs(7)
        }
    );
}

#[test]
fn custom_tool_replay_counts_as_tool_replay_semantics() {
    assert!(responses_body_contains_tool_replay_semantics(
        &serde_json::json!({
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-custom",
                "output": "opaque"
            }]
        })
    ));
}

#[test]
fn route_attempts_groups_homogeneous_terminal_classes() {
    for (class, expected) in [
        (FailureClass::Credentials, TerminalFailure::Credentials),
        (
            FailureClass::ModelUnsupported,
            TerminalFailure::ModelUnsupported,
        ),
        (
            FailureClass::FeatureUnsupported,
            TerminalFailure::CapabilityUnsupported,
        ),
        (
            FailureClass::ProtocolUnsupported,
            TerminalFailure::ProtocolUnsupported,
        ),
    ] {
        let mut ledger = AttemptLedger::default();
        ledger.record(AttemptFailure {
            route_id: "route".into(),
            upstream_status: Some(400),
            class,
            retry_after: None,
        });
        assert_eq!(ledger.terminal_failure(), expected);
    }
}

#[test]
fn route_attempts_reports_mixed_non_temporary_exhaustion() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route-a".into(),
        upstream_status: Some(401),
        class: FailureClass::Credentials,
        retry_after: None,
    });
    ledger.record(AttemptFailure {
        route_id: "route-b".into(),
        upstream_status: Some(400),
        class: FailureClass::ModelUnsupported,
        retry_after: None,
    });
    assert_eq!(
        ledger.terminal_failure(),
        TerminalFailure::MixedRoutesExhausted
    );
}

fn terminal_error_for(classes: &[FailureClass]) -> GatewayError {
    let mut ledger = AttemptLedger::default();
    for (index, class) in classes.iter().copied().enumerate() {
        ledger.record(AttemptFailure {
            route_id: format!("route-secret-{index}"),
            upstream_status: Some(400),
            class,
            retry_after: class
                .is_temporary()
                .then(|| Duration::from_secs(11 + index as u64)),
        });
    }
    terminal_route_failure_error(&ledger, 1, Duration::ZERO, None, classes.len(), None, false)
}

#[test]
fn client_error_message_adds_the_matching_prefix_once() {
    assert_eq!(
        client_error_message("gateway_code", "plain message"),
        "[gateway_code] plain message"
    );
    assert_eq!(
        client_error_message("gateway_code", "[gateway_code] plain message"),
        "[gateway_code] plain message"
    );
}

#[test]
fn route_attempts_maps_terminal_failures_to_stable_errors() {
    for (classes, expected_status, expected_code) in [
        (
            vec![FailureClass::TransientServer],
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream_routes_exhausted",
        ),
        (
            vec![FailureClass::Credentials],
            StatusCode::BAD_GATEWAY,
            "upstream_credentials_exhausted",
        ),
        (
            vec![FailureClass::ModelUnsupported],
            StatusCode::BAD_GATEWAY,
            "upstream_model_unsupported",
        ),
        (
            vec![FailureClass::FeatureUnsupported],
            StatusCode::BAD_REQUEST,
            "capability_not_supported",
        ),
        (
            vec![FailureClass::ProtocolUnsupported],
            StatusCode::BAD_GATEWAY,
            "upstream_protocol_unsupported",
        ),
        (
            vec![FailureClass::Credentials, FailureClass::ModelUnsupported],
            StatusCode::BAD_GATEWAY,
            "upstream_routes_exhausted",
        ),
    ] {
        let error = terminal_error_for(&classes);
        assert_eq!(error.status_code(), expected_status);
        assert_eq!(error.error_code(), expected_code);
    }
}

#[test]
fn route_attempts_terminal_details_are_numeric_and_secret_free() {
    let error = terminal_error_for(&[FailureClass::TransientServer, FailureClass::RateLimited]);
    let details = error.safe_details();
    assert_eq!(details["attempt_count"], 2);
    assert_eq!(details["class_counts"]["transient_server"], 1);
    assert_eq!(details["class_counts"]["rate_limited"], 1);
    assert_eq!(details["retry_after_seconds"], 11);
    let rendered = details.to_string();
    for forbidden in ["route-secret", "fingerprint", "upstream body", "prompt"] {
        assert!(
            !rendered.contains(forbidden),
            "leaked {forbidden}: {rendered}"
        );
    }
}

#[test]
fn terminal_retry_after_seconds_are_rounded_up() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route".into(),
        upstream_status: Some(503),
        class: FailureClass::TransientServer,
        retry_after: Some(Duration::from_millis(1_001)),
    });

    let error = terminal_route_failure_error(&ledger, 1, Duration::ZERO, None, 1, None, false);
    assert_eq!(error.retry_after_seconds(), Some(2));
    assert_eq!(error.safe_details()["retry_after_seconds"], 2);
    // No recovery and no probe: A5 details report the absence explicitly.
    assert_eq!(error.safe_details()["give_up_reason"], serde_json::Value::Null);
    assert_eq!(
        error.safe_details()["live_recovery_seconds"],
        serde_json::Value::Null
    );
    assert_eq!(error.safe_details()["last_resort_probe_attempted"], false);
}

#[test]
fn terminal_details_report_give_up_reason_recovery_and_probe() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route".into(),
        upstream_status: Some(502),
        class: FailureClass::TransientServer,
        retry_after: Some(Duration::from_secs(9)),
    });

    let error = terminal_route_failure_error(
        &ledger,
        3,
        Duration::from_millis(6_800),
        Some(RouteRecovery {
            half_open_remaining: Some(Duration::from_secs(14)),
            class: FailureClass::TransientServer,
            retry_after: Duration::from_secs(12),
        }),
        0,
        Some(GiveUpReason::AlignmentExhausted),
        true,
    );

    let details = error.safe_details();
    assert_eq!(details["give_up_reason"], "alignment_exhausted");
    assert_eq!(
        details["live_recovery_seconds"],
        14,
        "live recovery must prefer the half-open remaining time"
    );
    assert_eq!(details["last_resort_probe_attempted"], true);
    // The client-facing retry hint keeps preferring the live recovery too.
    assert_eq!(error.retry_after_seconds(), Some(14));
}

#[test]
fn rate_limit_only_exhaustion_returns_429_with_cause_in_message() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route-a".into(),
        upstream_status: Some(429),
        class: FailureClass::RateLimited,
        retry_after: Some(Duration::from_secs(24)),
    });
    ledger.record_cooled(AttemptFailure {
        route_id: "route-b".into(),
        upstream_status: Some(503),
        class: FailureClass::ConcurrencySaturated,
        retry_after: Some(Duration::from_secs(2)),
    });

    let error = terminal_route_failure_error(&ledger, 3, Duration::from_millis(9_800), None, 1, None, false);

    assert_eq!(error.status_code(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error.error_type(), "rate_limit_error");
    assert_eq!(error.error_code(), "upstream_routes_exhausted");
    assert_eq!(error.retry_after_seconds(), Some(2));
    let message = error.message();
    assert!(
        message.contains("rate limited by upstream (1 route, upstream HTTP 429)"),
        "message must name the rate limit cause: {message}"
    );
    assert!(
        message.contains("upstream concurrency limit saturated (1 route, upstream HTTP 503)"),
        "message must name the concurrency cause with its real status: {message}"
    );
    assert!(
        message.contains("please try again in 2s"),
        "message must include the recovery estimate: {message}"
    );
    assert!(
        message.contains("retried for 9.8s across 3 routing rounds"),
        "message must include the retry budget spent: {message}"
    );
    let details = error.safe_details();
    assert_eq!(details["routing_rounds"], 3);
    assert_eq!(details["waited_ms"], 9_800);
}

#[test]
fn cooled_routes_carry_real_upstream_status_in_summary() {
    let mut ledger = AttemptLedger::default();
    for index in 0..8 {
        ledger.record_cooled(AttemptFailure {
            route_id: format!("route-{index}"),
            upstream_status: Some(502),
            class: FailureClass::TransientServer,
            retry_after: Some(Duration::from_secs(3)),
        });
    }

    let error = terminal_route_failure_error(&ledger, 3, Duration::from_millis(6_900), None, 0, None, false);

    assert_eq!(error.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    let message = error.message();
    assert!(
        message.contains("transient upstream server errors (8 routes, upstream HTTP 502)"),
        "message must surface the real upstream status from cooldowns: {message}"
    );
    assert!(
        message.contains("please try again in 3s"),
        "message must include the recovery estimate: {message}"
    );
    let details = error.safe_details();
    assert_eq!(details["class_counts"]["transient_server"], 8);
}

#[test]
fn mixed_temporary_exhaustion_keeps_503_but_names_causes() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route-a".into(),
        upstream_status: Some(429),
        class: FailureClass::RateLimited,
        retry_after: Some(Duration::from_secs(24)),
    });
    ledger.record(AttemptFailure {
        route_id: "route-b".into(),
        upstream_status: Some(502),
        class: FailureClass::TransientServer,
        retry_after: Some(Duration::from_secs(10)),
    });

    let error = terminal_route_failure_error(&ledger, 1, Duration::ZERO, None, 2, None, false);

    assert_eq!(error.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.error_type(), "upstream_error");
    let message = error.message();
    assert!(
        message.contains("rate limited by upstream (1 route, upstream HTTP 429)"),
        "message must name each cause: {message}"
    );
    assert!(
        message.contains("transient upstream server errors (1 route, upstream HTTP 502)"),
        "message must name each cause: {message}"
    );
    assert!(
        !message.contains("retried for"),
        "single-round exhaustion must not claim a retry wait: {message}"
    );
}

#[test]
fn mixed_capacity_429_and_503_exhaustion_keeps_503() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route-a".into(),
        upstream_status: Some(429),
        class: FailureClass::CapacityUnavailable,
        retry_after: Some(Duration::from_secs(2)),
    });
    ledger.record(AttemptFailure {
        route_id: "route-b".into(),
        upstream_status: Some(503),
        class: FailureClass::CapacityUnavailable,
        retry_after: Some(Duration::from_secs(10)),
    });

    let error = terminal_route_failure_error(&ledger, 1, Duration::ZERO, None, 2, None, false);

    assert_eq!(error.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.error_type(), "upstream_error");
}

#[test]
fn live_recovery_overrides_understated_upstream_retry_after() {
    // Upstream said "retry in 1s" but the local cooldown is ~27s; the client
    // must be told the number that will actually succeed.
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "route".into(),
        upstream_status: Some(429),
        class: FailureClass::RateLimited,
        retry_after: Some(Duration::from_secs(1)),
    });

    let error = terminal_route_failure_error(
        &ledger,
        1,
        Duration::ZERO,
        Some(RouteRecovery {
            half_open_remaining: None,
            class: FailureClass::RateLimited,
            retry_after: Duration::from_secs(27),
        }),
        1,
        None,
        true,
    );

    assert_eq!(error.status_code(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error.retry_after_seconds(), Some(27));
    let message = error.message();
    assert!(
        message.contains("please try again in 27s"),
        "message must carry the live recovery estimate: {message}"
    );
}

#[test]
fn concurrency_error_keeps_public_capacity_class_and_specific_route_health() {
    let error = GatewayError::ConcurrencyFull {
        message: "concurrency limit exceeded".into(),
        retry_after: None,
        upstream_status: None,
    };

    assert_eq!(
        error.route_failure_class(),
        Some(FailureClass::CapacityUnavailable)
    );
    assert_eq!(
        route_health_outcome(&error, false),
        RouteOutcome::RouteFailure {
            class: FailureClass::ConcurrencySaturated,
            upstream_status: None,
            repeat_within_request: false,
        }
    );
}

#[test]
fn rollback_coordination_failure_replaces_the_original_gateway_error() {
    let original = GatewayError::BadRequest("invalid request".into());

    let error = replace_error_on_runtime_rollback_failure(
        original,
        Err(crate::state::RuntimeCoordinationError),
    );

    assert_eq!(error.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.error_category(), "runtime_coordination_unavailable");
    assert_eq!(error.retry_after_seconds(), Some(1));
}

#[test]
fn only_runtime_coordination_usage_errors_become_gateway_503s() {
    let coordination_error = std::io::Error::other(crate::state::RuntimeCoordinationError);
    let persistence_error = std::io::Error::other("disk unavailable");

    let error = runtime_coordination_gateway_error(&coordination_error)
        .expect("runtime coordination failures must fail closed");

    assert_eq!(error.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.error_category(), "runtime_coordination_unavailable");
    assert!(runtime_coordination_gateway_error(&persistence_error).is_none());
}

#[test]
fn runtime_coordination_stream_error_uses_a_stable_public_code() {
    let frame = runtime_coordination_sse_error_frame(EndpointKind::ChatCompletions, 1);
    let body = String::from_utf8(frame.to_vec()).unwrap();

    assert!(body.contains("\"code\":\"runtime_coordination_unavailable\""));
    assert!(body.contains("\"category\":\"runtime_coordination_unavailable\""));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[test]
fn hedge_coordination_failure_is_terminal_but_capacity_rejection_is_not() {
    let coordination = runtime_coordination_unavailable_gateway_error();
    let capacity = GatewayError::upstream_temporary_unavailable(
        "upstream hedge concurrency capacity is full",
        "upstream_hedge_capacity_unavailable",
    );

    assert!(upstream::hedge_error_is_terminal(&coordination));
    assert!(!upstream::hedge_error_is_terminal(&capacity));
}

fn tracked_route(fingerprint: &str) -> RouteHealthKey {
    RouteHealthKey {
        upstream_id: "up-1".into(),
        key_fingerprint: fingerprint.into(),
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    }
}

fn tracked_aggregate() -> RouteSetAggregateKey {
    RouteSetAggregateKey {
        upstream_id: "up-1".into(),
        runtime_model_slug: "glm-5.2".into(),
        protocol: WireProtocol::Responses,
    }
}

#[test]
fn request_route_tracker_prevents_reselecting_a_physical_route() {
    let mut tracker = RequestRouteTracker::default();
    let route = tracked_route("fingerprint-a");

    assert!(tracker.should_attempt(&route));
    tracker.record_physical_attempt(route.clone());
    assert!(!tracker.should_attempt(&route));
}

#[test]
fn request_route_tracker_aggregates_once_only_after_every_eligible_route_failed() {
    let mut tracker = RequestRouteTracker::default();
    let aggregate = tracked_aggregate();
    let route_a = tracked_route("fingerprint-a");
    let route_b = tracked_route("fingerprint-b");
    tracker.register_eligible(aggregate.clone(), route_a.clone());
    tracker.register_eligible(aggregate.clone(), route_b.clone());

    assert!(tracker.take_newly_exhausted().is_empty());

    tracker.record_physical_attempt(route_a.clone());
    tracker.record_failure(
        &route_a,
        FailureClass::TransientServer,
        Some(Duration::from_secs(30)),
    );
    assert!(tracker.take_newly_exhausted().is_empty());

    tracker.record_physical_attempt(route_b.clone());
    assert!(tracker.take_newly_exhausted().is_empty());
    tracker.record_failure(
        &route_b,
        FailureClass::RateLimited,
        Some(Duration::from_secs(7)),
    );

    let observations = tracker.take_newly_exhausted();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].key, aggregate);
    assert_eq!(observations[0].class, FailureClass::RateLimited);
    assert_eq!(observations[0].retry_after, Some(Duration::from_secs(7)));
    assert!(tracker.take_newly_exhausted().is_empty());
}

#[test]
fn shared_request_route_attempts_unify_hedge_physical_attempts_and_failures() {
    let attempts = RequestRouteAttempts::default();
    let aggregate = tracked_aggregate();
    let primary = tracked_route("fingerprint-a");
    let hedge = tracked_route("fingerprint-b");
    attempts.register_eligible(aggregate, primary.clone());
    attempts.register_eligible(tracked_aggregate(), hedge.clone());

    attempts.record_physical_attempt(primary.clone());
    attempts.record_physical_attempt(hedge.clone());
    assert!(!attempts.should_attempt(&primary));
    assert!(!attempts.should_attempt(&hedge));

    attempts.record_failure(
        &primary,
        FailureClass::TransientServer,
        Some(Duration::from_secs(30)),
    );
    attempts.record_failure(
        &hedge,
        FailureClass::RateLimited,
        Some(Duration::from_secs(7)),
    );

    let observations = attempts.take_newly_exhausted();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].class, FailureClass::RateLimited);
    let ledger = attempts.ledger_snapshot();
    assert_eq!(ledger.distinct_route_count(), 2);
    assert_eq!(ledger.class_count(FailureClass::TransientServer), 1);
    assert_eq!(ledger.class_count(FailureClass::RateLimited), 1);
}

#[test]
fn request_route_attempts_start_each_round_fresh_and_share_physical_send_count() {
    let round_one = RequestRouteAttempts::default();
    let aggregate = tracked_aggregate();
    let route = tracked_route("fingerprint-rounds");
    round_one.register_eligible(aggregate.clone(), route.clone());
    round_one.record_physical_attempt(route.clone());
    round_one.record_failure(
        &route,
        FailureClass::TransientServer,
        Some(Duration::from_secs(7)),
    );

    let round_two = round_one.next_round();
    assert_eq!(round_one.routing_round(), 1);
    assert_eq!(round_two.routing_round(), 2);
    assert_eq!(round_two.physical_attempt_count(), 1);
    assert!(round_two.should_attempt(&route));
    assert!(round_two.ledger_snapshot().is_empty());
    assert!(round_two.eligible_routes().is_empty());

    round_two.register_eligible(aggregate, route.clone());
    assert_eq!(round_two.eligible_routes(), vec![route.clone()]);
    round_two.record_physical_attempt(route);
    assert_eq!(round_one.physical_attempt_count(), 2);
    assert_eq!(round_two.physical_attempt_count(), 2);
}

#[test]
fn transient_failure_suppression_spans_routing_rounds() {
    // A1: a route that failed earlier in the same downstream request stays
    // suppressed in later routing rounds (fresh per-round ledgers), so the
    // request contributes at most one failure step per route.
    let round_one = RequestRouteAttempts::default();
    let aggregate = tracked_aggregate();
    let route = tracked_route("fingerprint-suppression");
    round_one.register_eligible(aggregate.clone(), route.clone());
    round_one.record_physical_attempt(route.clone());
    round_one.record_failure(
        &route,
        FailureClass::TransientServer,
        Some(Duration::from_secs(7)),
    );
    assert!(round_one.has_transient_failure_for(&route));

    let round_two = round_one.next_round();
    assert!(round_two.ledger_snapshot().is_empty());
    assert!(
        round_two.has_transient_failure_for(&route),
        "in-request suppression must span routing rounds, not just one round ledger"
    );

    // A different route that only appears in round two is not suppressed.
    round_two.register_eligible(aggregate, route.clone());
    let other = tracked_route("fingerprint-other");
    round_two.register_eligible(tracked_aggregate(), other.clone());
    round_two.record_physical_attempt(other.clone());
    assert!(!round_two.has_transient_failure_for(&other));
}

#[test]
fn rate_limit_failures_never_suppress_a_later_transient_failure() {
    // The request-scoped suppression set only tracks the transient family;
    // a 429-family failure on the same route must not mark it "already seen"
    // (B3 stays untouched: rate-limit failures are never swallowed).
    let attempts = RequestRouteAttempts::default();
    let aggregate = tracked_aggregate();
    let route = tracked_route("fingerprint-rate-limited");
    attempts.register_eligible(aggregate, route.clone());
    attempts.record_physical_attempt(route.clone());
    attempts.record_failure(
        &route,
        FailureClass::RateLimited,
        Some(Duration::from_secs(7)),
    );
    assert!(!attempts.has_transient_failure_for(&route));
}

#[test]
fn direct_hedge_send_count_does_not_change_round_route_selection() {
    let attempts = RequestRouteAttempts::default();
    let route = tracked_route("fingerprint-direct-hedge");

    attempts.record_physical_send();

    assert_eq!(attempts.physical_attempt_count(), 1);
    assert!(attempts.should_attempt(&route));
}

#[test]
fn terminal_observation_matches_the_public_terminal_failure_class() {
    let mut ledger = AttemptLedger::default();
    ledger.record(AttemptFailure {
        route_id: "temporary-route".into(),
        upstream_status: Some(503),
        class: FailureClass::TransientServer,
        retry_after: Some(Duration::from_secs(7)),
    });
    ledger.record(AttemptFailure {
        route_id: "credential-route".into(),
        upstream_status: Some(403),
        class: FailureClass::Credentials,
        retry_after: None,
    });
    let terminal = ledger.terminal_failure();

    assert_eq!(
        terminal,
        TerminalFailure::Temporary {
            retry_after: Duration::from_secs(7)
        }
    );
    let selected = ledger
        .terminal_observation_for(terminal)
        .expect("temporary terminal response should select a temporary observation");
    assert_eq!(selected.route_id, "temporary-route");
    assert_eq!(selected.class, FailureClass::TransientServer);
    assert_eq!(selected.upstream_status, Some(503));
}

async fn stream_completion_fixture(
    route: RouteHealthKey,
    route_attempts: RequestRouteAttempts,
    permit: RouteHealthPermit,
    state: AppState,
) -> StreamCompletionContext {
    let upstream = crate::state::UpstreamConfig {
        id: "up-1".into(),
        ..Default::default()
    };
    let upstream_lease = state
        .try_reserve_upstream_request(&upstream, "model-a")
        .await
        .unwrap();
    let downstream = crate::state::DownstreamConfig {
        id: "down-1".into(),
        name: "fixture".into(),
        hash: String::new(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: vec![],
        rate_limit_enabled: false,
        per_minute_limit: 1,
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
    let downstream_lease = state
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    StreamCompletionContext {
        state: state.clone(),
        route_health_key: route,
        route_attempts,
        route_health_permit: Arc::new(TokioMutex::new(Some(permit))),
        upstream_request_guard: UpstreamRequestReservation::new(UpstreamRequestGuard::new(
            state.clone(),
            upstream_lease,
        )),
        downstream_concurrency_guard: DownstreamConcurrencyGuard::new(state, downstream_lease),
        hedge_control: None,
    }
}

#[tokio::test]
async fn local_upstream_concurrency_is_scoped_per_account() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let upstream = UpstreamConfig {
        id: "shared-upstream".into(),
        max_concurrency: 1,
        active: true,
        ..Default::default()
    };
    let fingerprint_a = crate::keys::upstream_key_fingerprint(&upstream.id, "account-a");
    let fingerprint_b = crate::keys::upstream_key_fingerprint(&upstream.id, "account-b");

    let lease_a = state
        .try_reserve_upstream_account_request(&upstream, &fingerprint_a, "model-a")
        .await
        .expect("account A should reserve its first slot");
    let lease_b = state
        .try_reserve_upstream_account_request(&upstream, &fingerprint_b, "model-a")
        .await
        .expect("account B should have an independent slot");
    let same_account_rejection = state
        .try_reserve_upstream_account_request(&upstream, &fingerprint_a, "model-a")
        .await
        .expect_err("a second request on account A must exceed its limit");

    assert_eq!(same_account_rejection.retry_after_seconds, 1);
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .expect("shared upstream runtime snapshot")
            .in_flight,
        2,
    );

    state.release_upstream_request(lease_a).await.unwrap();
    state.release_upstream_request(lease_b).await.unwrap();
}

#[tokio::test]
async fn configured_upstream_concurrency_applies_independently_to_eight_keys() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let configured_limit = 3_u32;
    let upstream = UpstreamConfig {
        id: "eight-key-upstream".into(),
        max_concurrency: configured_limit,
        active: true,
        ..Default::default()
    };
    let fingerprints = (0..8)
        .map(|index| {
            crate::keys::upstream_key_fingerprint(&upstream.id, &format!("account-{index}"))
        })
        .collect::<Vec<_>>();
    let mut leases = Vec::new();

    for fingerprint in &fingerprints {
        for _ in 0..configured_limit {
            leases.push(
                state
                    .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
                    .await
                    .expect("each key should reserve its configured slots"),
            );
        }
        state
            .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
            .await
            .expect_err("each key must reject work above its configured limit");
    }

    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .expect("shared upstream runtime snapshot")
            .in_flight,
        configured_limit * fingerprints.len() as u32
    );
    for lease in leases {
        state.release_upstream_request(lease).await.unwrap();
    }
}

#[tokio::test]
async fn local_upstream_hedge_concurrency_is_scoped_per_account() {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    let upstream = UpstreamConfig {
        id: "shared-hedge-upstream".into(),
        max_concurrency: 1,
        requests_per_minute: 100,
        request_quota_requests: 100,
        active: true,
        ..Default::default()
    };
    let fingerprint_a = crate::keys::upstream_key_fingerprint(&upstream.id, "account-a");
    let fingerprint_b = crate::keys::upstream_key_fingerprint(&upstream.id, "account-b");

    let lease_a = state
        .try_reserve_upstream_account_hedge(&upstream, &fingerprint_a, "model-a")
        .await
        .expect("account A should reserve its first hedge slot");
    let lease_b = state
        .try_reserve_upstream_account_hedge(&upstream, &fingerprint_b, "model-a")
        .await
        .expect("account B should have an independent hedge slot");
    let rejection = state
        .try_reserve_upstream_account_hedge(&upstream, &fingerprint_a, "model-a")
        .await
        .expect_err("a second hedge on account A must exceed its limit");
    assert_eq!(
        rejection.reason,
        crate::state::UpstreamAdmissionRejectionReason::LocalConcurrency,
    );

    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .expect("shared hedge upstream runtime snapshot")
            .in_flight,
        2,
    );
    state.release_upstream_request(lease_a).await.unwrap();
    state.release_upstream_request(lease_b).await.unwrap();
}

#[tokio::test]
async fn gateway_uses_independent_local_slots_for_keys_on_one_upstream() {
    let hits = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_for_handler = hits.clone();
    let gate_for_handler = gate.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = hits_for_handler.clone();
            let gate = gate_for_handler.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let _permit = gate.acquire().await.unwrap();
                axum::Json(json!({
                    "id": "chatcmpl-independent-account-slots",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "unit-account-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = crate::keys::generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: "shared-key-upstream".into(),
        name: "shared key upstream".into(),
        base_url: format!("http://{address}"),
        api_key: "account-a".into(),
        api_keys: vec!["account-b".into()],
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["unit-account-model".into()],
        max_concurrency: 1,
        active: true,
        ..Default::default()
    };
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream]),
            downstreams: Arc::new(vec![crate::state::DownstreamConfig {
                id: "down-independent-account-slots".into(),
                name: "independent account slots client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["unit-account-model".into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 2,
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
            }]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 0,
            upstream_route_exhaustion_retry_max_wait_ms: 0,
            upstream_route_exhaustion_retry_max_rounds: 1,
            ..AppConfig::default()
        },
    );
    let app = build_router(state);
    let request = |prompt: &str| {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", downstream_key.plaintext),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "model": "unit-account-model",
                    "messages": [{"role": "user", "content": prompt}],
                    "stream": false
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = tokio::spawn(app.clone().oneshot(request("first")));
    tokio::time::timeout(Duration::from_secs(2), async {
        while hits.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first account request should reach the upstream");
    let second = tokio::spawn(app.oneshot(request("second")));
    tokio::time::timeout(Duration::from_secs(2), async {
        while hits.load(Ordering::SeqCst) < 2 && !second.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second request should either use account B or fail admission");

    gate.add_permits(2);
    let first_status = first.await.unwrap().unwrap().status();
    let second_status = second.await.unwrap().unwrap().status();
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn probe_reservation_failure_requeues_before_retrying_the_account() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_for_handler = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = hits_for_handler.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "id": "chatcmpl-probe-reservation-retry",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "unit-probe-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "recovered"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = crate::keys::generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: "up-probe-reservation-retry".into(),
        name: "probe reservation retry".into(),
        base_url: format!("http://{address}"),
        api_key: "probe-reservation-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["unit-probe-model".into()],
        active: true,
        ..Default::default()
    };
    let account = AccountConcurrencyKey::new(
        upstream.id.clone(),
        route_key_fingerprint(&upstream, &upstream.api_key),
    );
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream]),
            downstreams: Arc::new(vec![crate::state::DownstreamConfig {
                id: "down-probe-reservation-retry".into(),
                name: "probe reservation client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["unit-probe-model".into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 2,
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
            }]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 2_000,
            upstream_concurrency_recovery_max_rounds: 4,
            upstream_concurrency_probe_delays_ms: vec![1],
            ..AppConfig::default()
        },
    );
    state
        .observe_account_concurrency(&account, None)
        .await
        .unwrap();
    install_upstream_reservation_failure_test_hook(account.upstream_id.clone());

    let response = tokio::time::timeout(
        Duration::from_secs(3),
        build_router(state.clone()).oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "unit-probe-model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        ),
    )
    .await
    .expect("reservation failure recovery must stay within the account budget")
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let snapshot = state
        .account_concurrency_registry()
        .snapshot(&account, tokio::time::Instant::now());
    assert!(!snapshot.saturated);
    assert!(!snapshot.probe_in_flight);
    assert_eq!(snapshot.waiters, 0);
}

#[tokio::test]
async fn reservation_capacity_rejection_is_request_local_and_does_not_cool_route() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_for_handler = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = hits_for_handler.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "id": "chatcmpl-reservation-capacity",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "unit-probe-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "unexpected"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = crate::keys::generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: "up-reservation-capacity".into(),
        name: "reservation capacity".into(),
        base_url: format!("http://{address}"),
        api_key: "reservation-capacity-secret".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["unit-probe-model".into()],
        active: true,
        ..Default::default()
    };
    let route = RouteHealthKey {
        upstream_id: upstream.id.clone(),
        key_fingerprint: route_key_fingerprint(&upstream, &upstream.api_key),
        runtime_model_slug: "unit-probe-model".into(),
        protocol: WireProtocol::ChatCompletions,
    };
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream]),
            downstreams: Arc::new(vec![crate::state::DownstreamConfig {
                id: "down-reservation-capacity".into(),
                name: "reservation capacity client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["unit-probe-model".into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 2,
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
            }]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 0,
            upstream_route_exhaustion_retry_max_wait_ms: 0,
            // No retry rounds: the reservation stays rejected so the terminal
            // error is deterministic.
            upstream_route_exhaustion_retry_enabled: false,
            ..AppConfig::default()
        },
    );
    install_upstream_reservation_failure_test_hook("up-reservation-capacity".to_string());

    let response = tokio::time::timeout(
        Duration::from_secs(3),
        build_router(state.clone()).oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "unit-probe-model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        ),
    )
    .await
    .expect("capacity rejection must resolve within the timeout")
    .unwrap();

    // A local concurrency admission rejection must never surface as an
    // upstream 502 "invalid response": the upstream was never called.
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "upstream_routes_exhausted");
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["details"]["physical_attempt_count"], 0);
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("upstream concurrency limit saturated"),
        "message should name concurrency saturation, got: {message}"
    );
    assert!(
        !message.contains("upstream_invalid_response"),
        "message must not claim an invalid upstream response: {message}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "upstream must never be called"
    );
    assert!(
        state.route_health_snapshot(&route).await.unwrap().is_none(),
        "local admission must not create provider route health"
    );
}

#[tokio::test]
async fn recovery_session_keeps_multi_account_tickets_and_selects_the_oldest() {
    let directory = tempdir().unwrap();
    let downstream = crate::state::DownstreamConfig {
        id: "down-multi-account-recovery".into(),
        name: "multi-account recovery".into(),
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
            upstream_concurrency_recovery_max_wait_ms: 2_000,
            upstream_concurrency_recovery_max_rounds: 4,
            upstream_concurrency_probe_delays_ms: vec![1],
            ..AppConfig::default()
        },
    );
    let first = AccountConcurrencyKey::new("up-first", "fingerprint-first");
    let second = AccountConcurrencyKey::new("up-second", "fingerprint-second");
    state
        .observe_account_concurrency(&first, None)
        .await
        .unwrap();
    state
        .observe_account_concurrency(&second, None)
        .await
        .unwrap();
    let downstream_lease = state
        .try_reserve_downstream_concurrency(&downstream)
        .await
        .unwrap();
    let mut session = AccountRecoverySession::new(
        state.clone(),
        "request-multi-account".into(),
        downstream_lease,
        tokio::time::Instant::now() + Duration::from_secs(2),
        state
            .runtime_settings()
            .upstream_concurrency_recovery_max_rounds,
    );

    assert!(matches!(
        session.wait_for_account(first.clone()).await.unwrap(),
        AccountAdmission::Deferred { .. }
    ));
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(matches!(
        session.wait_for_account(second.clone()).await.unwrap(),
        AccountAdmission::Deferred { .. }
    ));
    assert_eq!(
        state
            .account_concurrency_registry()
            .snapshot(&first, tokio::time::Instant::now())
            .waiters,
        1
    );
    assert_eq!(
        state
            .account_concurrency_registry()
            .snapshot(&second, tokio::time::Instant::now())
            .waiters,
        1
    );

    assert!(session.wait_for_pending_account().await.unwrap());
    assert_eq!(session.active_probe_account(), Some(&first));
    session.finish().await.unwrap();
}

#[tokio::test]
async fn probe_completion_coordination_failure_is_not_replaced_by_route_exhaustion() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits_for_handler = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let hits = hits_for_handler.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "id": "chatcmpl-coordination-failure",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "unit-coordination-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let upstreams = (1..=4)
        .map(|index| UpstreamConfig {
            id: format!("up-coordination-{index}"),
            name: format!("coordination {index}"),
            base_url: format!("http://{address}"),
            api_key: format!("coordination-secret-{index}"),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["unit-coordination-model".into()],
            priority: 10 - index,
            active: true,
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let recovery_upstream = upstreams[2].clone();
    let recovery_account = AccountConcurrencyKey::new(
        recovery_upstream.id.clone(),
        route_key_fingerprint(&recovery_upstream, &recovery_upstream.api_key),
    );
    let downstream_key = crate::keys::generate_downstream_key("gw");
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(upstreams.clone()),
            downstreams: Arc::new(vec![crate::state::DownstreamConfig {
                id: "down-coordination-failure".into(),
                name: "coordination failure client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["unit-coordination-model".into()],
                rate_limit_enabled: true,
                per_minute_limit: 60,
                max_concurrency: 2,
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
            }]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_hedge_enabled: false,
            upstream_route_exhaustion_retry_enabled: false,
            upstream_concurrency_recovery_max_wait_ms: 2_000,
            upstream_concurrency_recovery_max_rounds: 4,
            upstream_concurrency_probe_delays_ms: vec![1],
            ..AppConfig::default()
        },
    );
    for upstream in &upstreams[..2] {
        state
            .observe_route_failure(
                &RouteHealthKey {
                    upstream_id: upstream.id.clone(),
                    key_fingerprint: route_key_fingerprint(upstream, &upstream.api_key),
                    runtime_model_slug: "unit-coordination-model".into(),
                    protocol: WireProtocol::ChatCompletions,
                },
                FailureClass::ConcurrencySaturated,
                Some(Duration::from_secs(60)),
            )
            .await
            .unwrap();
    }
    let fallback = &upstreams[3];
    state
        .observe_route_failure(
            &RouteHealthKey {
                upstream_id: fallback.id.clone(),
                key_fingerprint: route_key_fingerprint(fallback, &fallback.api_key),
                runtime_model_slug: "unit-coordination-model".into(),
                protocol: WireProtocol::ChatCompletions,
            },
            FailureClass::ConcurrencySaturated,
            Some(Duration::from_millis(50)),
        )
        .await
        .unwrap();
    state
        .observe_account_concurrency(&recovery_account, Some(Duration::from_millis(200)))
        .await
        .unwrap();
    account_recovery::install_probe_completion_failure_test_hook(
        recovery_account.upstream_id.clone(),
    );

    let response = tokio::time::timeout(
        Duration::from_secs(3),
        build_router(state).oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "unit-coordination-model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        ),
    )
    .await
    .expect("coordination failure should terminate immediately")
    .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        payload["error"]["code"],
        Value::String("runtime_coordination_unavailable".into())
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stream_transport_failure_updates_only_its_exact_route_and_shared_aggregate() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let route = tracked_route("stream-failure");
    let key = KeyHealthKey {
        upstream_id: route.upstream_id.clone(),
        key_fingerprint: route.key_fingerprint.clone(),
    };
    let permit = match state
        .reserve_route_health(&route, &key)
        .await
        .expect("route health reservation")
    {
        RouteAvailability::Ready(permit) => permit,
        availability => panic!("unexpected route availability: {availability:?}"),
    };
    let attempts = RequestRouteAttempts::default();
    attempts.register_eligible(tracked_aggregate(), route.clone());
    attempts.record_physical_attempt(route.clone());
    let completion =
        stream_completion_fixture(route.clone(), attempts.clone(), permit, state.clone()).await;

    completion.mark_failure().await;

    let route_health = state
        .route_health_snapshot(&route)
        .await
        .expect("route health snapshot")
        .unwrap();
    assert_eq!(route_health.consecutive_failures, 1);
    assert_eq!(
        route_health.last_failure_class,
        Some(FailureClass::Transport)
    );
    let aggregate_health = state
        .route_set_health_snapshot(&tracked_aggregate())
        .await
        .expect("route set health snapshot")
        .unwrap();
    assert_eq!(aggregate_health.consecutive_failures, 1);
    assert_eq!(
        attempts
            .ledger_snapshot()
            .class_count(FailureClass::Transport),
        1
    );
}

#[tokio::test]
async fn stream_cancellation_releases_exact_route_without_recording_failure() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let route = tracked_route("stream-cancelled");
    let key = KeyHealthKey {
        upstream_id: route.upstream_id.clone(),
        key_fingerprint: route.key_fingerprint.clone(),
    };
    let permit = match state
        .reserve_route_health(&route, &key)
        .await
        .expect("route health reservation")
    {
        RouteAvailability::Ready(permit) => permit,
        availability => panic!("unexpected route availability: {availability:?}"),
    };
    let attempts = RequestRouteAttempts::default();
    attempts.register_eligible(tracked_aggregate(), route.clone());
    attempts.record_physical_attempt(route.clone());
    let completion =
        stream_completion_fixture(route.clone(), attempts.clone(), permit, state.clone()).await;

    completion.mark_cancelled().await;

    if let Some(route_health) = state
        .route_health_snapshot(&route)
        .await
        .expect("route health snapshot")
    {
        assert_eq!(route_health.consecutive_failures, 0);
        assert_eq!(route_health.last_failure_class, None);
    }
    assert!(attempts.ledger_snapshot().is_empty());
    assert!(state
        .route_set_health_snapshot(&tracked_aggregate())
        .await
        .expect("route set health snapshot")
        .is_none());
}

#[tokio::test]
async fn route_attempts_preserves_openai_and_anthropic_error_envelopes() {
    let openai = terminal_error_for(&[FailureClass::Credentials]).into_response();
    assert_eq!(openai.status(), StatusCode::BAD_GATEWAY);
    let openai_payload: Value =
        serde_json::from_slice(&to_bytes(openai.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        openai_payload["error"]["code"],
        "upstream_credentials_exhausted"
    );
    assert_eq!(openai_payload["error"]["details"]["attempt_count"], 1);

    let anthropic =
        terminal_error_for(&[FailureClass::ProtocolUnsupported]).into_anthropic_response();
    assert_eq!(anthropic.status(), StatusCode::BAD_GATEWAY);
    let anthropic_payload: Value =
        serde_json::from_slice(&to_bytes(anthropic.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(anthropic_payload["type"], "error");
    assert_eq!(
        anthropic_payload["error"]["code"],
        "upstream_protocol_unsupported"
    );
    assert_eq!(
        anthropic_payload["error"]["details"]["class_counts"]["protocol_unsupported"],
        1
    );
}

#[test]
fn route_attempts_converts_classified_upstream_feedback_before_aggregation() {
    let error = GatewayError::from_classified_upstream_failure(
        crate::upstream_feedback::ClassifiedUpstreamFailure {
            class: FailureClass::ModelUnsupported,
            semantic: crate::upstream_feedback::UpstreamResponseSemantic::Generic,
            upstream_status: Some(400),
            retry_after: None,
        },
        "provider model rejection",
    );
    assert_eq!(error.status_code(), StatusCode::BAD_GATEWAY);
    assert_eq!(error.error_code(), "upstream_model_unsupported");
    assert_eq!(
        error.route_failure_class(),
        Some(FailureClass::ModelUnsupported)
    );
}

#[test]
fn explicit_concurrency_5xx_maps_to_account_recovery_with_provider_status() {
    let error = GatewayError::from_classified_upstream_failure(
        crate::upstream_feedback::ClassifiedUpstreamFailure {
            class: FailureClass::CapacityUnavailable,
            semantic: crate::upstream_feedback::UpstreamResponseSemantic::ExplicitConcurrency,
            upstream_status: Some(502),
            retry_after: Some(Duration::from_secs(3)),
        },
        "provider concurrency rejection",
    );

    assert!(matches!(
        error,
        GatewayError::ConcurrencyFull {
            retry_after: Some(retry_after),
            ..
        } if retry_after == Duration::from_secs(3)
    ));
    assert_eq!(error.upstream_status(), Some(502));
}

fn resolved_stream_capabilities(
    text_stream_source: CapabilitySource,
    nonstream_state: EvidenceState,
) -> ResolvedCapabilities {
    ResolvedCapabilities {
        values: BTreeMap::from([
            (
                Capability::TextStream,
                crate::capabilities::ResolvedCapability {
                    state: EvidenceState::Supported,
                    source: text_stream_source,
                },
            ),
            (
                Capability::NonStreamingResponse,
                crate::capabilities::ResolvedCapability {
                    state: nonstream_state,
                    source: CapabilitySource::Probe,
                },
            ),
        ]),
        token_limit_field: crate::capabilities::TokenLimitField::Omit,
        reasoning_mode: crate::capabilities::ReasoningMode::Off,
        reasoning_carrier: crate::capabilities::ReasoningCarrier::None,
        correction_rules: Vec::new(),
        reasoning_control_field: None,
        effort_map: BTreeMap::new(),
        omit_sampling_fields: BTreeSet::new(),
        context_window: None,
        max_output_tokens: None,
        omit_optional_extensions: false,
        profile_state: crate::capabilities::DialectProfileState::Verified,
        provisional: false,
        native_preferred: true,
        adapters: BTreeSet::new(),
        request_extensions: Vec::new(),
        field_sources: BTreeMap::new(),
    }
}

#[test]
fn nonstream_rejection_does_not_aggregate_with_baseline_only_text_stream() {
    let resolved =
        resolved_stream_capabilities(CapabilitySource::Baseline, EvidenceState::Rejected);

    assert_eq!(
        select_upstream_attempt_mode(false, Some(&resolved)),
        UpstreamAttemptMode::Json
    );
}

#[test]
fn verified_text_stream_evidence_allows_nonstream_aggregation() {
    for source in [CapabilitySource::Probe, CapabilitySource::Override] {
        let resolved = resolved_stream_capabilities(source, EvidenceState::Rejected);

        assert_eq!(
            select_upstream_attempt_mode(false, Some(&resolved)),
            UpstreamAttemptMode::SseAggregate
        );
    }
}

fn gateway_global_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn stream_only_unit_profile_key(index: usize) -> DialectProfileKey {
    DialectProfileKey::legacy(
        format!("unit-up-{index}"),
        format!("Unit/Route-{index}"),
        WireProtocol::ChatCompletions,
    )
}

#[test]
fn stream_only_recovery_marker_is_not_part_of_public_error_details() {
    let error = recoverable_upstream_empty_response_error();
    assert!(error.is_stream_only_recovery_candidate());
    let details = error.safe_details();
    assert_eq!(details["scope"], "upstream");
    assert!(details.get("stream_only_recovery_candidate").is_none());
}

#[tokio::test]
async fn stream_only_recovery_registry_is_bounded_and_cleans_completed_flights() {
    let _global_test_guard = gateway_global_test_lock().lock().await;
    let dir = tempdir().unwrap();
    let state = AppState::new(
        crate::state::PersistedState::default(),
        dir.path().join("state.json"),
        AppConfig::default(),
    );
    let key = stream_only_unit_profile_key(0);
    let leader = match begin_stream_only_recovery(&state, key.clone(), "fingerprint-0".into()) {
        StreamOnlyRecoveryRole::Leader(leader) => leader,
        role => panic!("expected leader, got {role:?}"),
    };
    let follower = match begin_stream_only_recovery(&state, key.clone(), "fingerprint-0".into()) {
        StreamOnlyRecoveryRole::Follower(follower) => follower,
        role => panic!("expected follower, got {role:?}"),
    };
    drop(leader);
    tokio::time::timeout(Duration::from_secs(1), follower.wait())
        .await
        .expect("leader drop must wake followers");
    let replacement = match begin_stream_only_recovery(&state, key, "fingerprint-0".into()) {
        StreamOnlyRecoveryRole::Leader(leader) => leader,
        role => panic!("completed flight was not removed: {role:?}"),
    };
    drop(replacement);

    let mut leaders = Vec::new();
    let mut reached_capacity = false;
    for index in 1..=1_024 {
        match begin_stream_only_recovery(
            &state,
            stream_only_unit_profile_key(index),
            format!("fingerprint-{index}"),
        ) {
            StreamOnlyRecoveryRole::Leader(leader) => leaders.push(leader),
            StreamOnlyRecoveryRole::AtCapacity => {
                reached_capacity = true;
                break;
            }
            StreamOnlyRecoveryRole::Follower(_) => panic!("unique key became a follower"),
        }
    }
    assert!(reached_capacity, "registry did not enforce a finite bound");
    assert!(!leaders.is_empty());
    drop(leaders);

    assert!(matches!(
        begin_stream_only_recovery(
            &state,
            stream_only_unit_profile_key(2_000),
            "fingerprint-2000".into(),
        ),
        StreamOnlyRecoveryRole::Leader(_)
    ));
}

#[tokio::test]
async fn stream_only_recovery_at_capacity_preserves_ordinary_candidate_fallback() {
    let _global_test_guard = gateway_global_test_lock().lock().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post({
            let requests = requests.clone();
            move |request: Request<Body>| {
                let requests = requests.clone();
                async move {
                    let authorization = request
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let payload: Value = serde_json::from_slice(
                        &to_bytes(request.into_body(), usize::MAX).await.unwrap(),
                    )
                    .unwrap();
                    let stream = payload["stream"] == true;
                    requests
                        .lock()
                        .unwrap()
                        .push((authorization.clone(), stream));

                    if authorization == "Bearer first-secret" {
                        return (
                            StatusCode::OK,
                            axum::Json(json!({
                                "id": "chatcmpl-empty",
                                "object": "chat.completion",
                                "model": "unit-capacity-model",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": ""},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 0,
                                    "total_tokens": 1
                                }
                            })),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "chatcmpl-healthy",
                            "object": "chat.completion",
                            "model": "unit-capacity-model",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "healthy"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = crate::keys::generate_downstream_key("gw");
    let upstreams = vec![
        UpstreamConfig {
            id: "up-capacity-first".into(),
            name: "capacity-first".into(),
            base_url: format!("http://{address}"),
            api_key: "first-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["unit-capacity-model".into()],
            active: true,
            ..Default::default()
        },
        UpstreamConfig {
            id: "up-capacity-second".into(),
            name: "capacity-second".into(),
            base_url: format!("http://{address}"),
            api_key: "second-secret".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            protocols: vec![UpstreamProtocol::ChatCompletions],
            supported_models: vec!["unit-capacity-model".into()],
            failure_count: 1,
            active: true,
            ..Default::default()
        },
    ];
    let dir = tempdir().unwrap();
    let state = AppState::new(
        crate::state::PersistedState {
            upstreams: std::sync::Arc::new(upstreams),
            downstreams: std::sync::Arc::new(vec![crate::state::DownstreamConfig {
                id: "down-capacity".into(),
                name: "capacity-client".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["unit-capacity-model".into()],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 2,
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
            }]),
            ..Default::default()
        },
        dir.path().join("state.json"),
        AppConfig {
            routing_affinity_enabled: false,
            ..AppConfig::default()
        },
    );
    let mut leaders = Vec::new();
    for index in 0..STREAM_ONLY_RECOVERY_MAX_FLIGHTS {
        match begin_stream_only_recovery(
            &state,
            stream_only_unit_profile_key(index + 10_000),
            format!("capacity-fingerprint-{index}"),
        ) {
            StreamOnlyRecoveryRole::Leader(leader) => leaders.push(leader),
            role => panic!("failed to fill recovery registry: {role:?}"),
        }
    }

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        build_router(state).oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "unit-capacity-model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        ),
    )
    .await
    .expect("at-capacity fallback must not hang")
    .unwrap();

    assert_eq!(
        requests.lock().unwrap().as_slice(),
        [
            ("Bearer first-secret".to_string(), false),
            ("Bearer second-secret".to_string(), false),
        ]
    );
    assert_eq!(response.status(), StatusCode::OK);
    drop(leaders);
}

#[test]
fn request_route_capability_cache_stays_on_captured_snapshot() {
    let upstream = UpstreamConfig {
        id: "up-fixed-snapshot".into(),
        base_url: "https://unit.invalid".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["opaque".into()],
        active: true,
        ..Default::default()
    };
    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &upstream.api_key);
    let key = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint.clone(),
        "opaque",
        WireProtocol::ChatCompletions,
    );
    let mut captured_snapshot = CapabilityRuntimeSnapshot::default();
    let mut captured_profile = UpstreamDialectProfile::unknown(key.clone());
    captured_profile.configuration_fingerprint =
        AppState::route_configuration_fingerprint_with_snapshot(
            &captured_snapshot,
            &upstream,
            &key_fingerprint,
            "opaque",
            "opaque",
            UpstreamProtocol::ChatCompletions,
        )
        .unwrap();
    captured_profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    captured_snapshot
        .profiles
        .insert(key.clone(), captured_profile);
    let requested = RequestedFeatures {
        optional: BTreeSet::from([Capability::ReasoningOutput]),
        ..RequestedFeatures::default()
    };

    let cache = build_request_route_capability_cache(
        &captured_snapshot,
        std::slice::from_ref(&upstream),
        "opaque",
        EndpointKind::ChatCompletions,
        &requested,
    );

    let mut hot_updated_snapshot = captured_snapshot.clone();
    hot_updated_snapshot
        .profiles
        .get_mut(&key)
        .unwrap()
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Rejected);
    let cached = cache
        .get(&(
            WireProtocol::ChatCompletions,
            upstream.id.clone(),
            key_fingerprint,
        ))
        .unwrap();
    assert!(cached.eligible);
    assert_eq!(cached.optional_misses, 0);
    assert!(cached
        .resolved
        .as_ref()
        .is_some_and(|resolved| resolved.supports(Capability::ReasoningOutput)));
    assert_eq!(
        hot_updated_snapshot.profiles[&key].capabilities[&Capability::ReasoningOutput],
        EvidenceState::Rejected
    );
}

#[test]
fn request_route_capability_cache_preserves_the_actual_rejected_capability() {
    let upstream = UpstreamConfig {
        id: "up-replay-rejected".into(),
        base_url: "https://unit.invalid".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["opaque".into()],
        active: true,
        ..Default::default()
    };
    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &upstream.api_key);
    let profile_key = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint.clone(),
        "opaque",
        WireProtocol::ChatCompletions,
    );
    let mut snapshot = CapabilityRuntimeSnapshot::default();
    let mut profile = UpstreamDialectProfile::unknown(profile_key);
    profile.configuration_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
        &snapshot,
        &upstream,
        &key_fingerprint,
        "opaque",
        "opaque",
        UpstreamProtocol::ChatCompletions,
    )
    .unwrap();
    profile.reasoning_carrier = Some(crate::capabilities::ReasoningCarrier::ReasoningContent);
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    profile
        .capabilities
        .insert(Capability::ReasoningReplay, EvidenceState::Rejected);
    snapshot.profiles.insert(profile.key.clone(), profile);
    let requested = RequestedFeatures {
        required: BTreeSet::from([
            Capability::FunctionTools,
            Capability::ToolContinuation,
            Capability::ReasoningOutput,
            Capability::ReasoningReplay,
        ]),
        ..RequestedFeatures::default()
    };

    let cache = build_request_route_capability_cache(
        &snapshot,
        std::slice::from_ref(&upstream),
        "opaque",
        EndpointKind::Responses,
        &requested,
    );
    let cached = cache
        .get(&(WireProtocol::ChatCompletions, upstream.id, key_fingerprint))
        .unwrap();

    assert!(!cached.eligible);
    assert_eq!(cached.failed_capability, Some(Capability::ReasoningReplay));
}

#[test]
fn chat_fallback_marks_dropped_reasoning_history() {
    let mut downgrades = BTreeSet::new();
    let converted = responses_request_to_chat_payload_with_fallback(
        &json!({
            "model": "opaque",
            "input": [
                {"type": "reasoning", "id": "rs_opaque", "summary": []},
                {"role": "user", "content": "continue"}
            ]
        }),
        None,
        None,
        &mut downgrades,
    )
    .unwrap();

    assert_eq!(
        converted["messages"],
        json!([{"role": "user", "content": "continue"}])
    );
    assert_eq!(
        downgrades,
        BTreeSet::from(["reasoning_history_dropped".to_string()])
    );
}

#[test]
fn request_route_capability_cache_overlays_value_and_protocol_hints_exactly() {
    let upstream = UpstreamConfig {
        id: "up-runtime-hint".into(),
        base_url: "https://unit.invalid".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["opaque".into()],
        active: true,
        ..Default::default()
    };
    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &upstream.api_key);
    let profile_key = DialectProfileKey::for_key(
        upstream.id.clone(),
        key_fingerprint.clone(),
        "opaque",
        WireProtocol::ChatCompletions,
    );
    let mut snapshot = CapabilityRuntimeSnapshot::default();
    let configuration_fingerprint = AppState::route_configuration_fingerprint_with_snapshot(
        &snapshot,
        &upstream,
        &key_fingerprint,
        "opaque",
        "opaque",
        UpstreamProtocol::ChatCompletions,
    )
    .unwrap();
    let mut profile = UpstreamDialectProfile::unknown(profile_key.clone());
    profile.configuration_fingerprint = configuration_fingerprint.clone();
    profile
        .capabilities
        .insert(Capability::ReasoningOutput, EvidenceState::Supported);
    snapshot.profiles.insert(profile_key.clone(), profile);
    let requested = RequestedFeatures {
        optional: BTreeSet::from([Capability::ReasoningOutput]),
        ..RequestedFeatures::default()
    };

    let mut hints = RuntimeCapabilityHints::new(8, Duration::from_secs(900));
    hints.insert(
        CapabilityHintKey::feature(
            profile_key.clone(),
            Capability::ReasoningOutput,
            Some("xhigh".into()),
        ),
        configuration_fingerprint.clone(),
    );
    let value_snapshot = hints.snapshot();
    let xhigh = build_request_route_capability_cache_with_hints(
        &snapshot,
        std::slice::from_ref(&upstream),
        "opaque",
        EndpointKind::ChatCompletions,
        &requested,
        &value_snapshot,
        Some("xhigh"),
        true,
    );
    assert!(
        !xhigh
            .get(&(
                WireProtocol::ChatCompletions,
                upstream.id.clone(),
                key_fingerprint.clone(),
            ))
            .unwrap()
            .eligible
    );

    let plain = build_request_route_capability_cache_with_hints(
        &snapshot,
        std::slice::from_ref(&upstream),
        "opaque",
        EndpointKind::ChatCompletions,
        &requested,
        &value_snapshot,
        None,
        true,
    );
    assert!(
        plain
            .get(&(
                WireProtocol::ChatCompletions,
                upstream.id.clone(),
                key_fingerprint.clone(),
            ))
            .unwrap()
            .eligible
    );

    hints.insert(
        CapabilityHintKey::protocol(profile_key),
        configuration_fingerprint,
    );
    let protocol_snapshot = hints.snapshot();
    let protocol_blocked = build_request_route_capability_cache_with_hints(
        &snapshot,
        std::slice::from_ref(&upstream),
        "opaque",
        EndpointKind::ChatCompletions,
        &requested,
        &protocol_snapshot,
        None,
        true,
    );
    assert!(
        !protocol_blocked
            .get(&(WireProtocol::ChatCompletions, upstream.id, key_fingerprint,))
            .unwrap()
            .eligible
    );
}

#[test]
fn responses_keepalive_frame_is_a_comment_not_a_fake_openai_event() {
    // OpenAI Responses streams are typed semantic events. Keepalive must stay
    // at the SSE transport layer and must not inject a fake `data: {}` event.
    let frame = sse_keepalive_frame();
    let text = std::str::from_utf8(&frame).unwrap();
    assert!(
        text.starts_with(':'),
        "responses keepalive frame must be a comment, got: {text:?}"
    );
    assert!(
        !text.contains("data:"),
        "responses keepalive frame must not include fake OpenAI data, got: {text:?}"
    );
    assert!(
        text.ends_with("\n\n"),
        "keepalive frame must be terminated with a blank line, got: {text:?}"
    );
}

#[test]
fn malformed_upstream_aggregate_is_typed_and_maps_to_bad_gateway() {
    let mut aggregator =
        crate::protocol::StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);

    let protocol_error = aggregator.push(b"data: {not-json}\n\n").unwrap_err();

    assert!(matches!(
        &protocol_error,
        ProtocolError::InvalidUpstreamStream {
            kind: crate::protocol::UpstreamStreamErrorKind::Decode,
            ..
        }
    ));
    assert!(!protocol_error.to_string().contains("not-json"));
    let gateway_error = protocol_error_to_gateway(protocol_error);
    assert_eq!(gateway_error.status_code(), StatusCode::BAD_GATEWAY);
    assert_eq!(gateway_error.error_type(), "upstream_error");
    assert_eq!(gateway_error.error_code(), "upstream_stream_decode_error");
}

#[test]
fn oversized_upstream_aggregate_maps_to_distinct_bad_gateway_category() {
    let mut aggregator =
        crate::protocol::StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
    let oversized =
        vec![b'x'; crate::protocol::stream_aggregate::MAX_STREAM_AGGREGATE_FRAME_BYTES + 1];

    let protocol_error = aggregator.push(&oversized).unwrap_err();

    assert!(matches!(
        &protocol_error,
        ProtocolError::InvalidUpstreamStream {
            kind: crate::protocol::UpstreamStreamErrorKind::LimitExceeded,
            ..
        }
    ));
    let gateway_error = protocol_error_to_gateway(protocol_error);
    assert_eq!(gateway_error.status_code(), StatusCode::BAD_GATEWAY);
    assert_eq!(gateway_error.error_type(), "upstream_error");
    assert_eq!(gateway_error.error_code(), "upstream_stream_limit_exceeded");
}

#[test]
fn chat_keepalive_frame_is_a_comment_not_a_data_event() {
    let frame = sse_keepalive_frame_for_endpoint(EndpointKind::ChatCompletions);
    let text = std::str::from_utf8(&frame).unwrap();
    assert!(
        text.starts_with(':'),
        "chat keepalive frame must be a comment, got: {text:?}"
    );
    assert!(
        text.ends_with("\n\n"),
        "chat keepalive frame must be terminated with a blank line, got: {text:?}"
    );
}

#[test]
fn empty_success_requires_a_recognized_substantive_output() {
    for body in [
        json!({}),
        json!({"usage": {"completion_tokens": 0, "output_tokens": 0}}),
        json!({"choices": []}),
        json!({"choices": [{"message": {"role": "assistant", "content": ""}}]}),
        json!({"output": []}),
        json!({"output": [{"type": "message", "content": []}]}),
        json!({"output": [{"type": "reasoning", "summary": [], "content": []}]}),
        json!({"output": [{"type": "function_call"}]}),
        json!({"output": [{"type": "computer_call", "id": "computer_1", "status": "completed"}]}),
        json!({"output": [{"type": "provider_extension", "id": "opaque_1", "status": "completed"}]}),
    ] {
        assert!(
            is_empty_success_response(&body),
            "body without substantive output must be empty: {body}"
        );
    }
}

#[test]
fn zero_token_substantive_outputs_are_not_empty_successes() {
    let zero_usage = json!({
        "completion_tokens": 0,
        "output_tokens": 0,
        "total_tokens": 0
    });
    for body in [
        json!({
            "choices": [{"message": {"role": "assistant", "refusal": "not allowed"}}],
            "usage": zero_usage.clone()
        }),
        json!({
            "choices": [{"message": {"role": "assistant", "reasoning_content": "plan"}}],
            "usage": zero_usage.clone()
        }),
        json!({
            "output": [{"type": "reasoning", "summary": [{"type": "summary_text", "text": "plan"}]}],
            "usage": zero_usage.clone()
        }),
        json!({
            "output": [{"type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{}"}],
            "usage": zero_usage.clone()
        }),
        json!({
            "output": [{"type": "custom_tool_call", "call_id": "call_2", "name": "shell", "input": "pwd"}],
            "usage": zero_usage.clone()
        }),
        json!({
            "output": [{"type": "computer_call", "id": "computer_1", "action": {"type": "screenshot"}}],
            "usage": zero_usage.clone()
        }),
        json!({
            "output": [{"type": "provider_extension", "id": "opaque_1", "payload": {"result": "ok"}}],
            "usage": zero_usage.clone()
        }),
        json!({
            "output": [{"type": "reasoning", "encrypted_content": "opaque-state"}],
            "usage": zero_usage.clone()
        }),
    ] {
        assert!(
            !is_empty_success_response(&body),
            "substantive output must survive zero-token metadata: {body}"
        );
    }
}

#[test]
fn downstream_disconnect_stays_499() {
    let (status, category) = classify_stream_failure("stream disconnected before completion");
    assert_eq!(status, StatusCode::from_u16(499).unwrap());
    assert_eq!(category, "stream_interrupted");
}

#[test]
fn drop_message_without_observed_output_means_cancelled_before_output() {
    let interruption = StreamInterruption::DownstreamBodyDropped {
        usable_output_delivered: false,
    };
    assert_eq!(
        interruption.message(),
        "downstream response body dropped before semantic completion"
    );
    assert_eq!(
        interruption.status_and_category(),
        (
            StatusCode::from_u16(499).unwrap(),
            "stream_client_cancelled"
        )
    );
}

#[test]
fn drop_message_with_observed_output_means_partial_output() {
    let interruption = StreamInterruption::DownstreamBodyDropped {
        usable_output_delivered: true,
    };
    assert_eq!(
        interruption.message(),
        "downstream response body dropped before semantic completion (partial output delivered)"
    );
    assert_eq!(
        interruption.status_and_category(),
        (
            StatusCode::from_u16(499).unwrap(),
            "stream_incomplete_close"
        )
    );
}

#[test]
fn client_cancelled_before_output_is_categorized() {
    // Codex/user cancelled the turn before any upstream output arrived.
    let (status, category) =
        classify_stream_failure("client disconnected before any upstream output");
    assert_eq!(status, StatusCode::from_u16(499).unwrap());
    assert_eq!(category, "stream_client_cancelled");
}

#[tokio::test]
async fn aggregate_cancellation_during_panic_does_not_emit_a_usage_log() {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        crate::state::PersistedState::default(),
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    state.start_active_gateway_request(ActiveGatewayRequestStart {
        request_id: "aggregate-panic".into(),
        downstream_id: "down-panic".into(),
        downstream_name: "panic-client".into(),
        endpoint: "/v1/responses".into(),
        model: "panic-model".into(),
        protocol: "Responses".into(),
        user_agent: None,
    });

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
        let state = state.clone();
        move || {
            let mut guard = ActiveGatewayRequestGuard::new(state.clone(), "aggregate-panic".into());
            guard.arm_aggregate_cancellation_log(GatewayUsageLogContext {
                state,
                request_id: "aggregate-panic".into(),
                downstream_id: "down-panic".into(),
                downstream_name: "panic-client".into(),
                upstream_id: "up-panic".into(),
                upstream_name: Some("panic-upstream".into()),
                endpoint: "/v1/responses".into(),
                model: "panic-model".into(),
                inference_strength: None,
                user_agent: None,
                compatibility: None,
                started: Instant::now(),
            });
            panic!("synthetic aggregate panic");
        }
    }));
    assert!(unwind.is_err());
    tokio::task::yield_now().await;

    assert!(state.active_gateway_requests(None).is_empty());
    assert!(state.snapshot().await.usage_logs.is_empty());
}

#[tokio::test]
async fn preparation_stage_cancel_after_reservation_emits_one_499_and_releases_slots() {
    let _global_test_guard = gateway_global_test_lock().lock().await;
    let (preparation_entered, release_preparation) = install_pre_header_preparation_test_gate();
    let upstream_hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = upstream_hits.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"unexpected\"}}]}\n\ndata: [DONE]\n\n",
                )
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = crate::keys::generate_downstream_key("gw");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        crate::state::PersistedState {
            upstreams: std::sync::Arc::new(vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{address}"),
                api_key: "upstream-secret".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4".into()],
                request_quota_window_hours: 24,
                request_quota_requests: 1000,
                requests_per_minute: 60,
                max_concurrency: 1,
                active: true,
                failure_count: 3,
                ..Default::default()
            }]),
            downstreams: std::sync::Arc::new(vec![crate::state::DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4".into()],
                per_minute_limit: 60,
                rate_limit_enabled: true,
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
            }]),
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig::default(),
    );
    let downstream = state.snapshot().await.downstreams[0].clone();
    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", downstream_key.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-gateway-test-pre-header-gate", "1")
                .body(Body::from(
                    json!({
                        "model": "gpt-4",
                        "messages": [{"role": "user", "content": "Hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    tokio::time::timeout(Duration::from_secs(1), preparation_entered)
        .await
        .expect("request should enter the reserved pre-dispatch preparation boundary")
        .expect("preparation gate should remain installed");
    let active = state.active_gateway_requests(Some("down-1"));
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].status, "upstream");
    assert_eq!(active[0].upstream_id.as_deref(), Some("up-1"));
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get("up-1")
            .expect("upstream runtime should exist")
            .in_flight,
        1
    );
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);

    state.mark_upstream_rate_limited("up-1", 60).await.unwrap();
    let cooldown_before_cancel = state
        .upstream_runtime_snapshots()
        .await
        .unwrap()
        .get("up-1")
        .expect("upstream runtime should exist")
        .cooldown_until;
    drop(response.into_body());

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let upstream_released = state
                .upstream_runtime_snapshots()
                .await
                .unwrap()
                .get("up-1")
                .is_some_and(|runtime| runtime.in_flight == 0);
            if upstream_released {
                if let Ok(lease) = state.try_reserve_downstream_concurrency(&downstream).await {
                    state.release_downstream_concurrency(lease).await.unwrap();
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("preparation cancellation should release both concurrency slots");

    tokio::time::timeout(Duration::from_secs(1), async {
        while state.snapshot().await.usage_logs.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("preparation cancellation should emit exactly one usage log");
    drop(release_preparation);

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].status_code, 499);
    assert_eq!(
        snapshot.usage_logs[0].error_category.as_deref(),
        Some("stream_client_cancelled")
    );
    assert_eq!(
        snapshot
            .upstreams
            .iter()
            .find(|upstream| upstream.id == "up-1")
            .expect("upstream should still exist")
            .failure_count,
        0
    );
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get("up-1")
            .expect("upstream runtime should exist")
            .cooldown_until,
        cooldown_before_cancel
    );
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);
}

#[test]
fn safe_upstream_body_diagnostics_do_not_include_payload_values() {
    let diagnostics = safe_upstream_body_diagnostics(&json!({
        "model": "gpt-5.1-ca",
        "messages": [{
            "role": "user",
            "content": "secret prompt that must not enter logs"
        }],
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup_secret",
                "arguments": "{\"token\":\"tool-secret\"}"
            }
        }],
        "api_key": "request-secret",
        "max_tokens": 1000,
        "stream": true
    }));

    let rendered = format!("{diagnostics:?}");
    assert!(rendered.contains("json_bytes"));
    assert!(rendered.contains("message_count"));
    assert!(rendered.contains("tool_count"));
    for sensitive in [
        "secret prompt",
        "tool-secret",
        "request-secret",
        "lookup_secret",
        "gpt-5.1-ca",
    ] {
        assert!(
            !rendered.contains(sensitive),
            "safe diagnostics must not include payload value {sensitive:?}: {rendered}"
        );
    }
}

#[test]
fn safe_upstream_error_summary_excludes_upstream_message() {
    let upstream_message = "This token has no access to model deepseek-v4-pro";
    let summary = safe_upstream_error_summary(
        StatusCode::BAD_REQUEST,
        Some(400),
        UpstreamFeedbackClassification::Unknown,
    );

    assert!(summary.contains("status 400"));
    assert!(summary.contains("upstream code 400"));
    assert!(!summary.contains(upstream_message));
}

#[test]
fn safe_upstream_error_summary_excludes_long_upstream_message() {
    let long_message = "SECRET_PROMPT_BODY_SHOULD_NOT_LEAK".repeat(50);
    let summary = safe_upstream_error_summary(
        StatusCode::BAD_REQUEST,
        Some(400),
        UpstreamFeedbackClassification::Unknown,
    );

    assert!(summary.contains("status 400"));
    assert!(!summary.contains(&long_message));
}

#[test]
fn upstream_error_code_extraction_ignores_numbers_from_freeform_echoed_message() {
    let error_text = json!({
        "error": {
            "message": "parse failed near {\"code\":\"1234\",\"token\":\"secret\"}",
            "type": "badrequesterror"
        }
    })
    .to_string();

    assert_eq!(extract_upstream_error_code(&error_text), None);
}

#[test]
fn client_disconnected_during_partial_output_is_categorized() {
    // Downstream closed mid-stream after some (incomplete) output but
    // before the completion signal. Distinct from a clean cancel.
    let (status, category) =
        classify_stream_failure("client disconnected during stream (partial output received)");
    assert_eq!(status, StatusCode::from_u16(499).unwrap());
    assert_eq!(category, "stream_incomplete_close");
}

#[test]
fn upstream_stream_read_error_is_bad_gateway() {
    let (status, category) =
        classify_upstream_stream_error("error decoding response body: unexpected eof", false, true);
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(category, "stream_upstream_body_decode_error");
}

#[test]
fn upstream_stream_timeout_is_gateway_timeout() {
    let (status, category) = classify_upstream_stream_error(
        "request timed out while reading upstream response",
        true,
        false,
    );
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(category, "stream_upstream_timeout");
}

#[test]
fn capability_name_prefers_failed_capability_intersecting_required_set() {
    let mut cache = BTreeMap::new();
    cache.insert(
        (WireProtocol::Responses, "up-a".into(), "key-a".into()),
        RouteCapabilityEvaluation {
            eligible: false,
            optional_misses: 0,
            failed_capability: Some(Capability::HostedTools),
            resolved: None,
        },
    );
    cache.insert(
        (WireProtocol::Responses, "up-b".into(), "key-b".into()),
        RouteCapabilityEvaluation {
            eligible: false,
            optional_misses: 0,
            failed_capability: Some(Capability::ParallelToolCalls),
            resolved: None,
        },
    );
    let required = BTreeSet::from([Capability::FunctionTools, Capability::ParallelToolCalls]);

    let name = capability_name_for_failure(
        None,
        false,
        &ClaudeThinkingReplayRoute::NoReplay,
        &cache,
        &required,
    );
    assert_eq!(name, "ParallelToolCalls");
}

#[test]
fn capability_name_lists_required_set_when_no_route_failure_intersects() {
    let mut cache = BTreeMap::new();
    cache.insert(
        (WireProtocol::Responses, "up-a".into(), "key-a".into()),
        RouteCapabilityEvaluation {
            eligible: false,
            optional_misses: 0,
            failed_capability: Some(Capability::HostedTools),
            resolved: None,
        },
    );
    let required = BTreeSet::from([Capability::FunctionTools, Capability::TextStream]);

    let name = capability_name_for_failure(
        None,
        false,
        &ClaudeThinkingReplayRoute::NoReplay,
        &cache,
        &required,
    );
    assert_eq!(name, "FunctionTools, TextStream");
}

#[test]
fn capability_name_keeps_unknown_for_empty_required_set() {
    let name = capability_name_for_failure(
        None,
        false,
        &ClaudeThinkingReplayRoute::NoReplay,
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    assert_eq!(name, "Unknown");
}

#[test]
fn capability_name_prefers_constrained_route_failure() {
    let name = capability_name_for_failure(
        Some(Capability::ReasoningOutput),
        true,
        &ClaudeThinkingReplayRoute::NoReplay,
        &BTreeMap::new(),
        &BTreeSet::from([Capability::FunctionTools]),
    );
    assert_eq!(name, "ReasoningOutput");
}
