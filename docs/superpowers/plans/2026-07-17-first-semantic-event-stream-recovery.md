# First Semantic Event Stream Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover a streaming Codex or OpenCode request when the upstream's first semantic SSE event is an error, without replaying after usable output or changing 499/502/504 semantics.

**Architecture:** Add a protocol classifier that consumes only the first semantic SSE event, then add a replayable upstream reader that owns both prefetched raw chunks and the existing watchdog. Prefetch runs inline before pass-through body handoff; a classified upstream protocol error returns to the existing bounded `SsePassThrough` to `Json` routing branch, while a normal first event commits the request to streaming and replays every consumed byte unchanged.

**Tech Stack:** Rust, Tokio, Axum, Reqwest streaming, serde_json, existing protocol SSE decoder, Cargo integration tests, Docker Compose, installed Codex and OpenCode smoke script.

---

## File Structure

- Modify `src/protocol/stream_aggregate.rs`: define the first-semantic-event classifier beside the existing `SseDecoder` and `StreamResponseAggregator` so it reuses the same parser and error kinds.
- Modify `src/protocol.rs`: re-export the classifier and result enum for the gateway.
- Modify `src/server/gateway.rs`: replace direct response/watchdog pairing with a replayable upstream stream reader whose watchdog spans prefetch and body delivery.
- Modify `src/server/gateway/stream.rs`: prefetch the first semantic event, return protocol errors before body handoff, and make aggregate/proxied/translated streams consume the replayable reader.
- Modify `src/server/gateway/upstream.rs`: create and prefetch the reader only for successful SSE pass-through responses before constructing the downstream body.
- Modify `tests/gateway/chat/streaming.rs`: cover first-event error recovery, late-error no-retry behavior, cancellation, resource release, and bounded fallback.
- Modify `tests/gateway/responses/streaming.rs`: strengthen raw CRLF, comments, multi-line data, same-chunk trailing frames, and split-frame replay assertions.
- Modify `docs/verification/2026-07-16-compatible-model-codex-opencode.md`: record the new build, focused client results, and post-deployment 499/502 evidence without retaining payloads.

## Task 1: Add A First-Semantic-Event Protocol Classifier

**Files:**
- Modify: `src/protocol/stream_aggregate.rs:158-240`
- Modify: `src/protocol/stream_aggregate.rs:771-850`
- Modify: `src/protocol.rs:10`

- [ ] **Step 1: Write failing classifier tests**

Add these tests to the existing test module in `src/protocol/stream_aggregate.rs`:

```rust
#[test]
fn first_semantic_event_classifier_waits_through_comments_and_split_frames() {
    let mut classifier =
        FirstSemanticEventClassifier::new(UpstreamProtocol::ChatCompletions);

    assert_eq!(
        classifier.push(b": keepalive\r\n\r\ndata: {\"id\":\"chunk-1\","),
        Ok(FirstSemanticEventResult::Pending)
    );
    assert_eq!(
        classifier.push(
            b"\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ready\"},\"finish_reason\":null}]}\r\n\r\n"
        ),
        Ok(FirstSemanticEventResult::Ready)
    );
}

#[test]
fn first_semantic_event_classifier_stops_before_later_error_in_same_chunk() {
    let mut classifier =
        FirstSemanticEventClassifier::new(UpstreamProtocol::ChatCompletions);
    let input = concat!(
        "data: {\"id\":\"chunk-1\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"ready\"},\"finish_reason\":null}]}\n\n",
        "event: error\n",
        "data: {\"error\":{\"message\":\"later failure\"}}\n\n"
    );

    assert_eq!(
        classifier.push(input.as_bytes()),
        Ok(FirstSemanticEventResult::Ready)
    );
}

#[test]
fn first_semantic_event_classifier_rejects_an_initial_error_event() {
    let mut classifier = FirstSemanticEventClassifier::new(UpstreamProtocol::Responses);
    let error = classifier
        .push(b"event: error\ndata: {\"error\":{\"message\":\"temporary\"}}\n\n")
        .unwrap_err();

    assert!(matches!(
        error,
        ProtocolError::InvalidUpstreamStream {
            kind: UpstreamStreamErrorKind::UpstreamEvent,
            ..
        }
    ));
}

#[test]
fn first_semantic_event_classifier_rejects_eof_without_an_event() {
    let classifier = FirstSemanticEventClassifier::new(UpstreamProtocol::Responses);
    let error = classifier.finish().unwrap_err();

    assert!(matches!(
        error,
        ProtocolError::InvalidUpstreamStream {
            kind: UpstreamStreamErrorKind::Incomplete,
            ..
        }
    ));
}
```

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
rtk cargo test --locked --offline --lib first_semantic_event_classifier -- --nocapture
```

Expected: compilation fails because `FirstSemanticEventClassifier` and `FirstSemanticEventResult` do not exist. This is the required RED result.

- [ ] **Step 3: Implement the minimal classifier**

Add this code immediately before `StreamResponseAggregator` in `src/protocol/stream_aggregate.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstSemanticEventResult {
    Pending,
    Ready,
}

#[derive(Debug)]
pub struct FirstSemanticEventClassifier {
    decoder: SseDecoder,
    validator: StreamResponseAggregator,
}

impl FirstSemanticEventClassifier {
    pub fn new(protocol: UpstreamProtocol) -> Self {
        Self {
            decoder: SseDecoder::new(),
            validator: StreamResponseAggregator::new(protocol),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<FirstSemanticEventResult, ProtocolError> {
        self.decoder.append(chunk)?;
        self.classify_next()
    }

    pub fn finish(mut self) -> Result<FirstSemanticEventResult, ProtocolError> {
        self.decoder.finish();
        match self.classify_next()? {
            FirstSemanticEventResult::Ready => Ok(FirstSemanticEventResult::Ready),
            FirstSemanticEventResult::Pending => Err(invalid_stream(
                UpstreamStreamErrorKind::Incomplete,
                "stream ended before the first semantic event",
            )),
        }
    }

    fn classify_next(&mut self) -> Result<FirstSemanticEventResult, ProtocolError> {
        let Some(event) = self.decoder.next_event()? else {
            return Ok(FirstSemanticEventResult::Pending);
        };
        self.validator.process_event(&event)?;
        Ok(FirstSemanticEventResult::Ready)
    }
}
```

Update the re-export in `src/protocol.rs`:

```rust
pub use stream_aggregate::{
    FirstSemanticEventClassifier, FirstSemanticEventResult, StreamAggregateResult,
    StreamResponseAggregator,
};
```

- [ ] **Step 4: Run focused protocol tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --lib first_semantic_event_classifier -- --nocapture
rtk cargo test --locked --offline --test compatibility_semantics structured_gateway_sse_errors_preserve_their_category_for_every_client_protocol -- --nocapture
```

Expected: the four classifier tests pass and the existing `upstream_stream_error_event` category test remains green.

- [ ] **Step 5: Commit the protocol classifier**

```bash
rtk git add src/protocol.rs src/protocol/stream_aggregate.rs
rtk git commit -m "feat(protocol): classify the first semantic SSE event" -m "Constraint: Stop classification after one semantic event and preserve provider-neutral error kinds" -m "Confidence: high" -m "Scope-risk: narrow"
```

## Task 2: Recover An Initial SSE Error Through The Existing JSON Retry

**Files:**
- Modify: `tests/gateway/chat/streaming.rs:376-520`
- Modify: `src/server/gateway.rs:2520-2641`
- Modify: `src/server/gateway/stream.rs:330-540`
- Modify: `src/server/gateway/stream.rs:823-1018`
- Modify: `src/server/gateway/upstream.rs:1486-1630`

- [ ] **Step 1: Write the failing gateway integration test**

Add `use axum::response::IntoResponse;` to `tests/gateway/chat/streaming.rs`, then add this test after `downstream_chat_stream_is_proxied_as_event_stream`:

```rust
#[tokio::test]
async fn first_sse_error_retries_without_stream_before_output() {
    let attempts = Arc::new(Mutex::new(Vec::<bool>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let attempts_for_handler = attempts.clone();
    let upstream_app = Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request<Body>| {
            let attempts = attempts_for_handler.clone();
            async move {
                let payload: Value = serde_json::from_slice(
                    &to_bytes(request.into_body(), usize::MAX).await.unwrap(),
                )
                .unwrap();
                let request_stream = payload["stream"].as_bool().unwrap_or(false);
                attempts.lock().unwrap().push(request_stream);

                if request_stream {
                    let chunks = vec![Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"event: error\ndata: {\"error\":{\"message\":\"temporary stream failure\"}}\n\n",
                    ))];
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream::iter(chunks)),
                    )
                        .into_response();
                }

                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "chatcmpl-recovered",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "gpt-4.1-mini",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "recovered"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 2,
                            "completion_tokens": 1,
                            "total_tokens": 3
                        }
                    })),
                )
                    .into_response()
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let downstream_key = generate_downstream_key("gw");
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "primary".into(),
                base_url: format!("http://{address}"),
                api_key: "fixture-key".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["gpt-4.1-mini".into()],
                active: true,
                ..Default::default()
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "team-a".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["gpt-4.1-mini".into()],
                rate_limit_enabled: true,
                per_minute_limit: 60,
                max_concurrency: 10,
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
    let app = build_router(state.clone());
    let response = app
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
                        "model": "gpt-4.1-mini",
                        "stream": true,
                        "messages": [{
                            "role": "user",
                            "content": "Explain one protocol compatibility invariant."
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("chat.completion.chunk"));
    assert!(!body.contains("upstream_stream_error_event"));
    assert_eq!(*attempts.lock().unwrap(), vec![true, false]);
    wait_for_upstream_in_flight(&state, "up-1", 0).await;
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.usage_logs.len(), 1);
    assert_eq!(snapshot.usage_logs[0].status_code, 200);
}
```

- [ ] **Step 2: Run the integration test and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test gateway first_sse_error_retries_without_stream_before_output -- --nocapture
```

Expected: FAIL because the current pass-through path exposes the first SSE error and records only the `stream: true` attempt.

- [ ] **Step 3: Add the replayable reader with one continuous watchdog**

Replace the response-specific wait helper in `src/server/gateway.rs` with this internal reader shape:

```rust
struct UpstreamStreamReader {
    response: reqwest::Response,
    replay: VecDeque<Bytes>,
    watchdog: StreamWatchdog,
}

impl UpstreamStreamReader {
    fn new(response: reqwest::Response, timeouts: StreamTimeouts) -> Self {
        Self {
            response,
            replay: VecDeque::new(),
            watchdog: StreamWatchdog::new(timeouts),
        }
    }

    fn replay_later(&mut self, chunk: Bytes) {
        self.replay.push_back(chunk);
    }

    async fn next_chunk(&mut self) -> StreamReadOutcome {
        if let Some(chunk) = self.replay.pop_front() {
            return StreamReadOutcome::Chunk(Ok(Some(chunk)));
        }
        self.next_network_chunk().await
    }

    async fn next_network_chunk(&mut self) -> StreamReadOutcome {
        let outcome = wait_for_upstream_chunk(&mut self.response, &self.watchdog).await;
        match &outcome {
            StreamReadOutcome::Chunk(Ok(Some(_))) => {
                self.watchdog.record_upstream_activity(TokioInstant::now());
            }
            StreamReadOutcome::Heartbeat => {
                self.watchdog.record_heartbeat(TokioInstant::now());
            }
            _ => {}
        }
        outcome
    }

    fn debug_state(&self, now: TokioInstant) -> String {
        self.watchdog.debug_state(now)
    }
}
```

Keep `wait_for_upstream_chunk` as the network-only primitive. Update `aggregate_upstream_sse_response`, `ProxiedStreamState`, and `TranslatedStreamState` to own `UpstreamStreamReader`; remove their separate `response` and `watchdog` fields. Replace each direct wait with `reader.next_chunk().await` and remove duplicate `record_upstream_activity` and `record_heartbeat` calls.

- [ ] **Step 4: Add first-event prefetch**

Add this function in `src/server/gateway/stream.rs` next to `aggregate_upstream_sse_response`:

```rust
pub(super) async fn prefetch_first_semantic_event(
    mut reader: UpstreamStreamReader,
    protocol: UpstreamProtocol,
) -> Result<UpstreamStreamReader, GatewayError> {
    let mut classifier = FirstSemanticEventClassifier::new(protocol);

    loop {
        match reader.next_network_chunk().await {
            StreamReadOutcome::Chunk(Ok(Some(chunk))) => {
                reader.replay_later(chunk.clone());
                match classifier.push(&chunk).map_err(protocol_error_to_gateway)? {
                    FirstSemanticEventResult::Pending => {}
                    FirstSemanticEventResult::Ready => return Ok(reader),
                }
            }
            StreamReadOutcome::Chunk(Ok(None)) => {
                classifier.finish().map_err(protocol_error_to_gateway)?;
                return Ok(reader);
            }
            StreamReadOutcome::Chunk(Err(error)) => {
                let message = error.to_string();
                let (status, category) =
                    classify_upstream_stream_error(&message, error.is_timeout(), error.is_decode());
                return Err(stream_gateway_error(status, message, category));
            }
            StreamReadOutcome::Heartbeat => {}
            StreamReadOutcome::IdleTimeout => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "idle timeout waiting for SSE ({})",
                        reader.debug_state(TokioInstant::now())
                    ),
                    "stream_idle_timeout",
                ));
            }
            StreamReadOutcome::MaxDurationExceeded => {
                return Err(stream_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "stream max duration exceeded before completion ({})",
                        reader.debug_state(TokioInstant::now())
                    ),
                    "stream_max_duration",
                ));
            }
        }
    }
}
```

Import `FirstSemanticEventClassifier` and `FirstSemanticEventResult` from `crate::protocol`.

- [ ] **Step 5: Integrate prefetch at the successful SSE handoff**

In `src/server/gateway/upstream.rs`, keep aggregation unchanged except for constructing an `UpstreamStreamReader` internally. In the `request_stream` and SSE content-type branch, create and prefetch before building log context or a body:

```rust
let reader = UpstreamStreamReader::new(response, stream_timeouts);
let reader = prefetch_first_semantic_event(reader, upstream_protocol).await?;

let body = if upstream_protocol == endpoint.native_protocol() {
    proxied_stream_body(
        reader,
        endpoint,
        stream_log_context,
        stream_completion_context,
        response_history_context,
    )?
} else {
    translated_stream_body(
        reader,
        upstream_protocol,
        endpoint.native_protocol(),
        endpoint,
        stream_log_context,
        stream_completion_context,
        response_history_context,
    )?
};
```

Change the two body constructor signatures from `reqwest::Response` plus `StreamTimeouts` to one `UpstreamStreamReader`. Add `error_category = %error.error_category()` and `stream_to_json_recovery = true` to the existing `streaming upstream attempt failed; retrying without stream` debug event in `src/server/gateway.rs`.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test gateway first_sse_error_retries_without_stream_before_output -- --nocapture
rtk cargo test --locked --offline --test gateway downstream_chat_stream_is_proxied_as_event_stream -- --nocapture
rtk cargo test --locked --offline --test gateway downstream_responses_stream_is_proxied_as_event_stream -- --nocapture
rtk cargo test --locked --offline --test compatibility_semantics structured_gateway_sse_errors_preserve_their_category_for_every_client_protocol -- --nocapture
```

Expected: all focused tests pass; the recovery test records attempts `[true, false]`, while normal Chat and Responses streams retain their existing event shapes.

- [ ] **Step 7: Add the equivalent Codex Responses recovery test**

In `tests/gateway/responses/streaming.rs`, import `axum::response::IntoResponse` and add `downstream_responses_stream_recovers_when_chat_upstream_first_event_is_error` beside `downstream_responses_stream_retries_without_stream_when_upstream_rejects_stream`. Configure one active Chat Completions upstream for `gpt-4.1-mini`, one active downstream with that model in its allowlist, and a streaming `/v1/responses` request. The mock handler records every request body and uses this stream branch:

```rust
if request_stream {
    return (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream::iter([Ok::<Bytes, std::io::Error>(
            Bytes::from_static(
                b"event: error\ndata: {\"error\":{\"message\":\"temporary stream failure\"}}\n\n",
            ),
        )])),
    )
        .into_response();
}
```

Keep the successful non-streaming Chat completion response and assert:

```rust
assert_eq!(*captured_stream_modes.lock().unwrap(), vec![true, false]);
assert!(body.contains("response.created"));
assert!(body.contains("response.completed"));
assert!(body.contains("data: [DONE]"));
assert!(!body.contains("upstream_stream_error_event"));
```

Run:

```bash
rtk cargo test --locked --offline --test gateway downstream_responses_stream_recovers_when_chat_upstream_first_event_is_error -- --nocapture
```

Expected: PASS, proving the same recovery path synthesizes a valid Responses stream for Codex.

- [ ] **Step 8: Commit the recovery path**

```bash
rtk git add src/server/gateway.rs src/server/gateway/stream.rs src/server/gateway/upstream.rs tests/gateway/chat/streaming.rs
rtk git commit -m "fix(stream): recover first-event upstream failures" -m "Constraint: Retry only before the first semantic event and preserve one continuous watchdog" -m "Rejected: Buffer the complete stream | breaks streaming and cancellation" -m "Confidence: high" -m "Scope-risk: moderate"
```

## Task 3: Lock Byte Fidelity, Cancellation, And Bounded Fallback

**Files:**
- Modify: `tests/gateway/responses/streaming.rs:3-147`
- Modify: `tests/gateway/chat/streaming.rs`
- Modify if a RED test exposes a defect: `src/server/gateway.rs`
- Modify if a RED test exposes a defect: `src/server/gateway/stream.rs`

- [ ] **Step 1: Strengthen the normal replay assertions**

Update `downstream_responses_stream_is_proxied_as_event_stream` so the mock chunks are exactly:

```rust
let chunks = vec![
    Ok::<Bytes, std::io::Error>(Bytes::from_static(concat!(
        ": upstream-comment\r\nevent: custom-response-event\r\n",
        "id: event-42\r\nretry: 1500\r\n",
        "data: {\"id\":\"resp-stream\",\r\n",
        "data: \"object\":\"response.chunk\"}\r\n\r\n",
        "event: metadata-only\r\nid: event-43\r\nretry: 1600\r\n\r\n"
    ).as_bytes())),
    Ok(Bytes::from_static(concat!(
        ": done-comment\r\nevent: terminal\r\n",
        "id: done-42\r\nretry: 2500\r\ndata: [DONE]\r\n\r"
    ).as_bytes())),
    Ok(Bytes::from_static(b"\n")),
];
```

Preserve the existing exact substring assertions and add:

```rust
assert_eq!(text.matches("event: custom-response-event").count(), 1);
assert_eq!(text.matches("event: metadata-only").count(), 1);
assert_eq!(text.matches("event: terminal").count(), 1);
```

Run:

```bash
rtk cargo test --locked --offline --test gateway downstream_responses_stream_is_proxied_as_event_stream -- --nocapture
```

Expected: PASS, proving prefetched same-chunk trailing bytes, split delimiters, comments, CRLF, and multi-line data replay once.

- [ ] **Step 2: Add a no-retry-after-normal-output regression test**

Add `normal_first_event_then_error_is_not_retried` in `tests/gateway/chat/streaming.rs`. Its mock handler must record the request's `stream` boolean and return this complete body for the streaming attempt:

```rust
let chunks = vec![Ok::<Bytes, std::io::Error>(Bytes::from_static(concat!(
    "data: {\"id\":\"chatcmpl-late-error\",",
    "\"object\":\"chat.completion.chunk\",\"created\":1,",
    "\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,",
    "\"delta\":{\"content\":\"ready\"},\"finish_reason\":null}]}\n\n",
    "event: error\n",
    "data: {\"error\":{\"message\":\"late stream failure\"}}\n\n"
).as_bytes()))];
(
    StatusCode::OK,
    [(header::CONTENT_TYPE, "text/event-stream")],
    Body::from_stream(stream::iter(chunks)),
)
```

Return a successful JSON response only if an unexpected `stream: false` request occurs. Construct an inline `AppState` with one active Chat Completions upstream (`id = "up-1"`, model `gpt-4.1-mini`, `max_concurrency = 10`) and one active downstream (`id = "down-1"`, the generated hash/plaintext pair, the same model allowlist, `max_concurrency = 10`). Send a streaming `/v1/chat/completions` request, consume the body, and call `wait_for_upstream_in_flight(&state, "up-1", 0)` before asserting:

```rust
assert_eq!(response.status(), StatusCode::OK);
assert!(body.contains("ready"));
assert!(body.contains("upstream_stream_error_event"));
assert_eq!(*attempts.lock().unwrap(), vec![true]);
```

Run:

```bash
rtk cargo test --locked --offline --test gateway normal_first_event_then_error_is_not_retried -- --nocapture
```

Expected: PASS. A normal first semantic event commits the stream even when a later error was already present in the same raw chunk.

- [ ] **Step 3: Add cancellation-during-prefetch coverage**

Add `downstream_drop_during_first_event_prefetch_cancels_without_retry` in `tests/gateway/chat/streaming.rs`. The upstream handler must increment an `AtomicUsize`, return successful SSE headers, and use `stream::pending::<Result<Bytes, std::io::Error>>()` so no semantic event arrives. After receiving the downstream response, wait until upstream `in_flight == 1`, drop the downstream body, then poll until the slot is zero and one usage log exists. Assert:

```rust
assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
assert_eq!(snapshot.usage_logs.len(), 1);
assert_eq!(snapshot.usage_logs[0].status_code, 499);
assert_eq!(
    snapshot.usage_logs[0].error_category.as_deref(),
    Some("stream_client_cancelled")
);
assert_eq!(snapshot.upstreams[0].failure_count, 0);
```

Run:

```bash
rtk cargo test --locked --offline --test gateway downstream_drop_during_first_event_prefetch_cancels_without_retry -- --nocapture
```

Expected: PASS. If it fails, make the smallest lifecycle correction so prefetch remains inside the request future selected against downstream channel closure; do not spawn a detached task or add a second cancellation owner.

- [ ] **Step 4: Add first-error plus JSON-failure candidate fallback coverage**

Add `first_sse_error_then_json_failure_advances_to_next_candidate` in `tests/gateway/chat/streaming.rs` with two mock upstreams. Configure the first upstream with `priority: 100` to return an initial SSE error for `stream: true` and HTTP 502 for `stream: false`. Configure the second upstream with `priority: 0` to return a complete normal SSE response. Record `(upstream_label, stream)` in a shared vector and assert:

```rust
assert_eq!(
    *attempts.lock().unwrap(),
    vec![
        ("first".to_string(), true),
        ("first".to_string(), false),
        ("second".to_string(), true),
    ]
);
assert!(body.contains("chat.completion.chunk"));
assert!(!body.contains("upstream_stream_error_event"));
```

Run:

```bash
rtk cargo test --locked --offline --test gateway first_sse_error_then_json_failure_advances_to_next_candidate -- --nocapture
```

Expected: PASS with exactly three attempts. Do not increase retry counts or add a provider-specific branch.

- [ ] **Step 5: Run the complete stream regression slice**

Run:

```bash
rtk cargo test --locked --offline --test gateway chat::streaming -- --nocapture
rtk cargo test --locked --offline --test gateway responses::streaming -- --nocapture
rtk cargo test --locked --offline --test compatibility_semantics -- --nocapture
rtk cargo test --locked --offline --lib stream_ -- --nocapture
```

Expected: all stream lifecycle, protocol category, byte fidelity, and cancellation tests pass.

- [ ] **Step 6: Commit the regression coverage**

```bash
rtk git add tests/gateway/chat/streaming.rs tests/gateway/responses/streaming.rs src/server/gateway.rs src/server/gateway/stream.rs
rtk git commit -m "test(stream): lock first-event recovery boundaries" -m "Constraint: Preserve byte fidelity, 499 cancellation, and finite candidate fallback" -m "Confidence: high" -m "Scope-risk: narrow"
```

## Task 4: Verify, Deploy, And Re-run Common Client Models

**Files:**
- Modify: `docs/verification/2026-07-16-compatible-model-codex-opencode.md`
- Verify: all files changed in Tasks 1-3
- Deployment directory: `/home/kavin/docker/chat-responses-codex`

- [ ] **Step 1: Run fresh repository verification**

Run in this order:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --locked --offline --test gateway
rtk cargo test --locked --offline --test compatibility_semantics
rtk cargo test --locked --offline --manifest-path crates/gateway-core/Cargo.toml --all-targets
rtk cargo test --locked --offline
rtk cargo clippy --locked --offline --all-targets --all-features -- -D warnings
rtk git diff --check
```

Expected: rustfmt exits 0; gateway and protocol tests pass; `gateway-core` passes; the full workspace reports zero failed tests; Clippy reports no warnings; `git diff --check` prints nothing.

- [ ] **Step 2: Build the host release binary**

```bash
rtk cargo build --locked --offline --release
rtk sha256sum target/release/chat-responses-codex
```

Expected: the release build exits 0 and records one binary SHA-256 for deployment evidence.

- [ ] **Step 3: Create an offline local image with the host binary**

```bash
rtk docker create --name chat-responses-codex-first-event-build --network none chat-responses-codex:latest
rtk docker cp target/release/chat-responses-codex chat-responses-codex-first-event-build:/usr/local/bin/chat-responses-codex
rtk docker commit chat-responses-codex-first-event-build chat-responses-codex:first-event-recovery
rtk docker rm chat-responses-codex-first-event-build
rtk docker tag chat-responses-codex:first-event-recovery chat-responses-codex:latest
```

Expected: the stopped staging container has no network, the copied binary is committed to a rollback-addressable tag, and `latest` points at the same image without rebuilding dependencies inside Docker.

- [ ] **Step 4: Replace only the gateway container**

First capture only non-sensitive container metadata and the downstream credential digest used by the existing acceptance harness. Then run:

```bash
rtk docker compose --env-file /home/kavin/docker/chat-responses-codex/.env -f /home/kavin/docker/chat-responses-codex/docker-compose.yml --project-directory /home/kavin/docker/chat-responses-codex up -d --no-deps --force-recreate gateway
rtk docker inspect --format '{{.State.Status}} {{.State.Health.Status}} {{.RestartCount}}' chat-responses-codex
rtk docker exec chat-responses-codex sha256sum /usr/local/bin/chat-responses-codex
```

Expected: only `chat-responses-codex` is recreated, it becomes `running healthy 0`, PostgreSQL and Redis keep their existing containers, and the container binary SHA matches Step 2. Recompute the downstream credential digest and require it to match the pre-deployment value.

- [ ] **Step 5: Run installed Codex and OpenCode against common models**

Use the existing secured environment for `BASE_URL` and `DOWNSTREAM_KEY`. The script already uses a substantive protocol-analysis task and a read-only filesystem tool task.

```bash
rtk env CLIENTS_JSON='["codex","opencode"]' MODEL_SLUG=kimi-k2.5 scripts/installed_client_smoke.sh
rtk env CLIENTS_JSON='["codex","opencode"]' MODEL_SLUG=glm-5.2 scripts/installed_client_smoke.sh
rtk env CLIENTS_JSON='["codex","opencode"]' MODEL_SLUG=deepseek-v4-flash scripts/installed_client_smoke.sh
rtk env CLIENTS_JSON='["codex","opencode"]' MODEL_SLUG=MiniMax-M2.7 scripts/installed_client_smoke.sh
rtk env CLIENTS_JSON='["codex","opencode"]' MODEL_SLUG=qwen3.6-plus scripts/installed_client_smoke.sh
```

Expected: exact installed Codex `0.144.0` and OpenCode `1.17.9` complete both tasks for the retained common-model set. Do not run greeting-only probes and do not expand to all retained models.

- [ ] **Step 6: Inspect sanitized operational evidence**

Use status/category/count-only queries. Record:

- Count of 200, 499, and 502 terminal records per focused model.
- Count of `upstream_stream_error_event` records.
- Whether any recovery log shows the bounded SSE-to-JSON transition.
- Gateway health and restart count.
- PostgreSQL and Redis container IDs before and after deployment.

Do not retain or print credentials, prompts, responses, reasoning, tool arguments, or tool results.

- [ ] **Step 7: Update verification evidence and commit**

Update `docs/verification/2026-07-16-compatible-model-codex-opencode.md` with the deployed commit, binary SHA, focused test counts, client versions, common-model outcomes, and sanitized 499/502 counts.

Run:

```bash
rtk rg -n '(sk-[A-Za-z0-9]|Bearer [A-Za-z0-9]|postgres(ql)?://|redis://|api[_-]?key[[:space:]]*[:=])' docs/verification/2026-07-16-compatible-model-codex-opencode.md
rtk git diff --check
rtk git status --short
```

Expected: the secret-like scan returns no matches, `git diff --check` prints nothing, and only the intended verification document remains uncommitted.

Commit:

```bash
rtk git add docs/verification/2026-07-16-compatible-model-codex-opencode.md
rtk git commit -m "docs(verification): record first-event recovery acceptance" -m "Constraint: Retain only status, category, count, version, and digest evidence" -m "Confidence: high" -m "Scope-risk: narrow"
```

- [ ] **Step 8: Final clean-tree check**

```bash
rtk git status --short
rtk git log -6 --oneline --decorate
```

Expected: the worktree is clean and the protocol, recovery, regression, and verification commits appear at the branch tip.
