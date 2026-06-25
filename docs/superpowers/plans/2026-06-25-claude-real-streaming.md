# Claude Real Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `/v1/messages` stream real Anthropic-style SSE from upstream chat chunk streams instead of synthesizing SSE from a completed response body.

**Architecture:** Keep the existing `/v1/messages` request translation and routing path, but preserve `stream: true` into upstream chat requests and add a gateway-local streamed body adapter from `chat.completion.chunk` SSE to Anthropic Messages SSE. Preserve current non-stream JSON behavior and existing usage/logging cleanup semantics.

**Tech Stack:** Rust, Axum, reqwest SSE streaming, serde_json, existing gateway stream state utilities, Cargo test

---

### Task 1: Lock the New Streaming Contract in Tests

**Files:**
- Modify: `tests/gateway/claude.rs`
- Test: `tests/gateway/claude.rs`

- [ ] **Step 1: Write the failing test updates for text streaming**

Replace the existing text streaming assertions so the test requires real upstream streaming:

```rust
assert_eq!(captured_body["messages"][0]["content"], "Hello");
assert_eq!(
    captured_body.get("stream").and_then(serde_json::Value::as_bool),
    Some(true)
);
assert!(payload.contains("event: message_start"));
assert!(payload.contains("\"type\":\"message_start\""));
assert!(payload.contains("event: content_block_delta"));
assert!(payload.contains("\"type\":\"text_delta\""));
assert!(payload.contains("\"text\":\"Hi\""));
assert!(payload.contains("event: message_delta"));
assert!(payload.contains("\"stop_reason\":\"end_turn\""));
assert!(payload.contains("event: message_stop"));
assert!(!payload.contains("data: [DONE]"));
```

- [ ] **Step 2: Run the text streaming test to verify it fails**

Run: `rtk cargo test -q claude_messages_stream_true_returns_anthropic_sse_events`

Expected: FAIL because the current implementation does not forward `stream: true` and still synthesizes the stream from a full JSON response.

- [ ] **Step 3: Write the failing tool-call streaming assertion update**

Update the tool streaming test to require real upstream streaming and no downstream `[DONE]` sentinel:

```rust
assert_eq!(response.status(), StatusCode::OK);
assert_eq!(
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok()),
    Some("text/event-stream")
);
let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
let payload = String::from_utf8(body.to_vec()).unwrap();
assert!(payload.contains("\"type\":\"tool_use\""));
assert!(payload.contains("\"name\":\"get_weather\""));
assert!(payload.contains("\"type\":\"input_json_delta\""));
assert!(payload.contains("\\\"city\\\":\\\"Paris\\\""));
assert!(payload.contains("\"stop_reason\":\"tool_use\""));
assert!(!payload.contains("data: [DONE]"));
```

- [ ] **Step 4: Run the tool streaming test to verify it fails**

Run: `rtk cargo test -q claude_messages_stream_true_emits_tool_use_block_events`

Expected: FAIL for the same reason as the text streaming case.

- [ ] **Step 5: Add a new failing streamed-chunk upstream fixture test**

Add a new test in `tests/gateway/claude.rs` where the upstream `/v1/chat/completions` endpoint returns `text/event-stream` chat chunks:

```rust
#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_true_adapts_chat_chunk_sse_without_gateway_synthesis() {
    with_proxy_env_cleared(|| async move {
        // Upstream emits:
        // 1. assistant role + "Hel"
        // 2. content "lo"
        // 3. finish_reason stop + usage
        // 4. [DONE]
        //
        // Downstream must receive Anthropic named SSE events with text deltas,
        // no fabricated JSON body and no data: [DONE].
    })
    .await;
}
```

- [ ] **Step 6: Run the new streamed-chunk test to verify it fails**

Run: `rtk cargo test -q claude_messages_stream_true_adapts_chat_chunk_sse_without_gateway_synthesis`

Expected: FAIL because `dispatch_claude_success()` currently rejects streamed dispatch bodies.

- [ ] **Step 7: Commit the failing tests**

```bash
git add tests/gateway/claude.rs
git commit -m "test: lock real claude streaming behavior"
```

### Task 2: Preserve Upstream Streaming in Claude Request Translation

**Files:**
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/claude.rs`

- [ ] **Step 1: Update `claude_messages_to_chat_payload()` to preserve `stream`**

In `src/server/gateway.rs`, copy the downstream Claude `stream` flag into the chat payload:

```rust
if let Some(stream) = body.get("stream").and_then(Value::as_bool) {
    output.insert("stream".into(), Value::Bool(stream));
}
```

Place this alongside the other optional request field copies in `claude_messages_to_chat_payload()`.

- [ ] **Step 2: Run the text streaming test**

Run: `rtk cargo test -q claude_messages_stream_true_returns_anthropic_sse_events`

Expected: Still FAIL, but now because streamed dispatch bodies are not yet adapted, not because `stream` is missing upstream.

- [ ] **Step 3: Commit the request-path change**

```bash
git add src/server/gateway.rs tests/gateway/claude.rs
git commit -m "feat: forward claude stream flag upstream"
```

### Task 3: Add a Real-Time Chat-to-Claude Streaming Adapter

**Files:**
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/claude.rs`

- [ ] **Step 1: Add a Claude streaming state struct**

Add a gateway-local state machine near the existing stream helpers:

```rust
struct ClaudeStreamState {
    response: reqwest::Response,
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    usage: Option<(u64, u64, u64)>,
    log_context: Option<StreamUsageLogContext>,
    completion_context: Option<StreamCompletionContext>,
    finished: bool,
    semantic_completion_emitted: bool,
    usage_log_flushed: bool,
    watchdog: StreamWatchdog,
    message_id: Option<String>,
    model: Option<String>,
    created_at: Option<u64>,
    message_start_emitted: bool,
    text_block_started: bool,
    text_block_stopped: bool,
    stop_reason: Option<String>,
}
```

This struct should mirror the existing stream-state patterns for cleanup and timeout behavior.

- [ ] **Step 2: Add helper functions for Claude SSE output**

Add focused helpers for emitting named Anthropic events:

```rust
fn claude_sse_event(event: &str, payload: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {payload}\n\n"))
}

fn chat_finish_reason_to_claude_stop_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ => "end_turn",
    }
}
```

Reuse the existing `claude_sse_event()` if it already matches this format; only move or share it if needed.

- [ ] **Step 3: Implement frame parsing and chunk adaptation**

Inside the new state machine, parse each SSE frame and translate `chat.completion.chunk` deltas:

```rust
if payload.trim() == "[DONE]" {
    self.finish_stream()?;
    break;
}

let event: Value = serde_json::from_str(&payload)
    .map_err(|error| std::io::Error::other(error.to_string()))?;
if let Some(usage) = stream_usage_from_value(&event) {
    self.usage = Some(usage);
}
self.translate_chat_chunk(&event)?;
```

`translate_chat_chunk()` must:
- emit `message_start` once
- emit `content_block_start` for the first text block
- emit text `content_block_delta` for each content fragment
- emit tool-use start/delta events when `delta.tool_calls` or `delta.function_call` appears
- capture finish reason for final `message_delta`

- [ ] **Step 4: Finalize the Claude downstream stream**

Implement `finish_stream()` so it closes any open content block, emits `message_delta`, emits `message_stop`, and does not enqueue `data: [DONE]`:

```rust
self.emit_open_block_stop_if_needed();
self.pending.push_back(claude_sse_event(
    "message_delta",
    json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": self.stop_reason_value(),
            "stop_sequence": Value::Null
        },
        "usage": {
            "output_tokens": self.usage.unwrap_or((0, 0, 0)).1
        }
    }),
));
self.pending.push_back(claude_sse_event(
    "message_stop",
    json!({"type": "message_stop"}),
));
self.finished = true;
```

- [ ] **Step 5: Add a `claude_stream_body(...)` constructor**

Create a streaming body adapter similar to `proxied_stream_body()` and `translated_stream_body()`:

```rust
fn claude_stream_body(
    response: reqwest::Response,
    log_context: StreamUsageLogContext,
    stream_completion_context: Option<StreamCompletionContext>,
    stream_timeouts: StreamTimeouts,
) -> Result<Body, GatewayError> {
    // try_unfold over ClaudeStreamState
}
```

This must reuse `wait_for_upstream_chunk()`, the watchdog, usage flushing, and normal/interrupted completion cleanup patterns already used by other stream bodies.

- [ ] **Step 6: Run the new streamed-chunk test**

Run: `rtk cargo test -q claude_messages_stream_true_adapts_chat_chunk_sse_without_gateway_synthesis`

Expected: PASS

- [ ] **Step 7: Run the two updated Claude streaming tests**

Run: `rtk cargo test -q claude_messages_stream_true_returns_anthropic_sse_events`

Expected: PASS

Run: `rtk cargo test -q claude_messages_stream_true_emits_tool_use_block_events`

Expected: PASS

- [ ] **Step 8: Commit the adapter implementation**

```bash
git add src/server/gateway.rs tests/gateway/claude.rs
git commit -m "feat: stream claude messages from chat chunks"
```

### Task 4: Route Streamed Claude Dispatch Results Through the New Adapter

**Files:**
- Modify: `src/server/gateway.rs`
- Test: `tests/gateway/claude.rs`

- [ ] **Step 1: Update `dispatch_claude_success()` to accept `DispatchBody::Stream`**

Change the current streamed-body rejection branch into real adaptation:

```rust
let body = match result.body {
    DispatchBody::Json(body) => { /* existing non-stream path */ }
    DispatchBody::Stream(body) => {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        return (result.status, headers, body).into_response();
    }
};
```

If the adapter is created earlier in the dispatch pipeline instead of here, wire the result through so `/v1/messages` streaming returns the adapted body directly and never falls back to `claude_message_to_sse_body()`.

- [ ] **Step 2: Keep non-stream behavior unchanged**

Preserve the existing `DispatchBody::Json -> chat_completion_to_claude_message()` branch exactly for `stream: false`.

- [ ] **Step 3: Run all Claude gateway tests**

Run: `rtk cargo test -q claude_`

Expected: PASS

- [ ] **Step 4: Commit the dispatch-path change**

```bash
git add src/server/gateway.rs tests/gateway/claude.rs
git commit -m "feat: wire claude streamed dispatch results"
```

### Task 5: Verify Stream Lifecycle and Full Regression Safety

**Files:**
- Modify: `tests/gateway/claude.rs`
- Modify: `src/server/gateway.rs` if small cleanup is needed
- Test: `tests/gateway/claude.rs`
- Test: full suite

- [ ] **Step 1: Add a drop-after-semantic-completion regression test if missing**

Add a Claude `/v1/messages` stream test where the downstream client disconnects after receiving `message_delta` or `message_stop` but before upstream transport has fully settled, then assert the usage log shows success:

```rust
#[tokio::test(flavor = "current_thread")]
async fn claude_messages_stream_drop_after_semantic_completion_is_logged_as_success() {
    // Build a delayed upstream [DONE] case
    // Read through semantic completion
    // Drop downstream body
    // Assert final usage log status_code == 200 and no error
}
```

- [ ] **Step 2: Run the drop regression test**

Run: `rtk cargo test -q claude_messages_stream_drop_after_semantic_completion_is_logged_as_success`

Expected: PASS

- [ ] **Step 3: Run focused Claude and chat streaming suites**

Run: `rtk cargo test -q claude_`

Expected: PASS

Run: `rtk cargo test -q downstream_responses_stream_is_translated_from_chat_stream_with_tool_calls`

Expected: PASS

- [ ] **Step 4: Run the full suite**

Run: `rtk cargo test -q`

Expected: PASS with the existing suite count and no new failures.

- [ ] **Step 5: Commit the verification pass**

```bash
git add src/server/gateway.rs tests/gateway/claude.rs
git commit -m "test: verify claude real streaming lifecycle"
```

## Self-Review

Spec coverage:
- Real upstream streaming for `/v1/messages`: covered by Tasks 1, 2, 4
- Real-time Anthropic SSE adaptation from chat chunks: covered by Task 3
- Preserve non-stream JSON behavior: covered by Task 4
- Lifecycle/logging safety: covered by Task 5

Placeholder scan:
- No `TODO`, `TBD`, or implied “fill this in later” steps remain
- Each code-touching step names exact files and concrete commands

Type consistency:
- Plan consistently uses `DispatchBody::Stream`, `claude_stream_body`, `StreamUsageLogContext`, and `StreamCompletionContext`
- The proposed adapter stays in `src/server/gateway.rs`, matching current gateway stream helpers
