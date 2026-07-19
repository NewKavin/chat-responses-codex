use super::*;
use crate::capabilities::{
    Capability, CapabilityRuntimeSnapshot, DialectProfileKey, EvidenceState, RequestedFeatures,
    UpstreamDialectProfile, WireProtocol,
};
use axum::body::to_bytes;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicUsize;
use tempfile::tempdir;
use tower::ServiceExt;

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
            upstreams,
            downstreams: vec![crate::state::DownstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
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
    assert_eq!(
        stream_drop_interruption_message(false),
        "client disconnected before any upstream output"
    );
}

#[test]
fn drop_message_with_observed_output_means_partial_output() {
    assert_eq!(
        stream_drop_interruption_message(true),
        "client disconnected during stream (partial output received)"
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
            upstreams: vec![UpstreamConfig {
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
            }],
            downstreams: vec![crate::state::DownstreamConfig {
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
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
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
            .get("up-1")
            .expect("upstream runtime should exist")
            .in_flight,
        1
    );
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 0);

    state.mark_upstream_rate_limited("up-1", 60).await;
    let cooldown_before_cancel = state
        .upstream_runtime_snapshots()
        .await
        .get("up-1")
        .expect("upstream runtime should exist")
        .cooldown_until;
    drop(response.into_body());

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let upstream_released = state
                .upstream_runtime_snapshots()
                .await
                .get("up-1")
                .is_some_and(|runtime| runtime.in_flight == 0);
            if upstream_released
                && state
                    .try_reserve_downstream_concurrency(&downstream)
                    .is_ok()
            {
                state.release_downstream_concurrency(&downstream.id);
                break;
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
        3
    );
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
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
fn safe_upstream_error_summary_includes_truncated_upstream_message() {
    // A structured upstream error message (e.g. "This token has no access to
    // model X") is valuable diagnostic information for the downstream client.
    // It should be included in the summary so users can understand the real
    // cause of the failure.
    let upstream_message = "This token has no access to model deepseek-v4-pro";
    let summary = safe_upstream_error_summary(
        StatusCode::BAD_REQUEST,
        Some(400),
        UpstreamFeedbackClassification::Unknown,
        upstream_message,
    );

    assert!(summary.contains("status 400"));
    assert!(summary.contains("upstream code 400"));
    assert!(
        summary.contains(upstream_message),
        "safe summary should include the upstream error message: {summary}"
    );
}

#[test]
fn safe_upstream_error_summary_truncates_long_upstream_message() {
    // An oversized upstream message (which might echo request content) must be
    // truncated so it cannot flood logs or leak large prompt payloads.
    let long_message = "SECRET_PROMPT_BODY_SHOULD_NOT_LEAK".repeat(50);
    let summary = safe_upstream_error_summary(
        StatusCode::BAD_REQUEST,
        Some(400),
        UpstreamFeedbackClassification::Unknown,
        &long_message,
    );

    assert!(summary.contains("status 400"));
    // The summary should be bounded — not contain the full oversized message.
    assert!(
        summary.len() < long_message.len(),
        "safe summary must truncate oversized upstream messages: {summary}"
    );
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
