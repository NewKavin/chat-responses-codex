# Domestic Model Stream Stability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Codex reliably consume common non-standard Chat Completions streams from GLM, MiniMax, and DeepSeek routes without weakening explicit error handling or replaying completed work.

**Architecture:** Keep `ChatStreamCanonicalizer` as the single normalization boundary. Relax only null-delta, stable-identity, and semantically proven EOF terminal cases; retain strict rejection for ambiguous or explicit failures. Record static structural failure reasons in server-only tracing before preserving the existing generic downstream error.

**Tech Stack:** Rust, Tokio, Axum, serde_json, tracing, Cargo integration tests, Codex CLI 0.144.6, Docker.

---

### Task 1: Add Protocol Compatibility Red Tests

**Files:**
- Modify: `tests/protocol.rs:1226-1418`
- Test: `tests/protocol.rs`

- [ ] **Step 1: Replace the strict EOF expectation with a text-terminal synthesis test**

Replace `chat_stream_canonicalizer_rejects_eof_without_explicit_terminal` with:

```rust
#[test]
fn chat_stream_canonicalizer_synthesizes_stop_at_eof_after_text_output() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("chatcmpl-stable", "opaque", 1);
    canonicalizer
        .push(json!({
            "choices": [{"delta": {"content": "partial"}, "finish_reason": null}]
        }))
        .unwrap();

    let terminal = canonicalizer.finish().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["id"], "chatcmpl-stable");
    assert_eq!(terminal[0]["choices"][0]["delta"], json!({}));
    assert_eq!(terminal[0]["choices"][0]["finish_reason"], "stop");
}
```

- [ ] **Step 2: Add null-delta normalization coverage**

```rust
#[test]
fn chat_stream_canonicalizer_normalizes_null_delta_to_empty_object() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    let events = canonicalizer
        .push(json!({
            "choices": [{"index": 0, "delta": null, "finish_reason": null}]
        }))
        .unwrap();
    assert_eq!(events[0]["choices"][0]["delta"], json!({}));
    assert!(canonicalizer.finish().is_err());
}
```

- [ ] **Step 3: Add stable-identity coverage**

```rust
#[test]
fn chat_stream_canonicalizer_keeps_first_identity_when_later_chunks_drift() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("fallback", "fallback", 1);
    canonicalizer
        .push(json!({
            "id": "first-id", "model": "first-model", "created": 10,
            "choices": [{"delta": {"content": "hello"}, "finish_reason": null}]
        }))
        .unwrap();
    let terminal = canonicalizer
        .push(json!({
            "id": "later-id", "model": "later-model", "created": 20,
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }))
        .unwrap();
    assert_eq!(terminal[0]["id"], "first-id");
    assert_eq!(terminal[0]["model"], "first-model");
    assert_eq!(terminal[0]["created"], 10);
}
```

- [ ] **Step 4: Add tool EOF and ambiguous-empty negative coverage**

Add separate tests which assert a non-empty `delta.tool_calls` followed by
`finish()` synthesizes `tool_calls`, while a role-only event plus usage-only
event still makes `finish()` return `Err`.

```rust
#[test]
fn chat_stream_canonicalizer_synthesizes_tool_calls_at_eof_after_tool_output() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    canonicalizer.push(json!({"choices": [{
        "index": 0,
        "delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
            "function": {"name": "read_file", "arguments": "{}"}}]},
        "finish_reason": null
    }]})).unwrap();
    let terminal = canonicalizer.finish().unwrap();
    assert_eq!(terminal[0]["choices"][0]["finish_reason"], "tool_calls");
}

#[test]
fn chat_stream_canonicalizer_rejects_eof_with_only_role_and_usage() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    canonicalizer.push(json!({"choices": [{
        "index": 0, "delta": {"role": "assistant"}, "finish_reason": null
    }]})).unwrap();
    canonicalizer.push(json!({
        "choices": [],
        "usage": {"prompt_tokens": 1, "completion_tokens": 0, "total_tokens": 1}
    })).unwrap();
    assert!(canonicalizer.latest_usage().is_some());
    assert!(canonicalizer.finish().is_err());
}
```

Change `chat_stream_canonicalizer_rejects_eof_when_any_choice_lacks_terminal`
so its unterminated second choice contains only `delta: {"role":"assistant"}`.
This preserves rejection when any choice lacks usable semantics.

- [ ] **Step 5: Run the focused protocol tests and verify RED**

Run:

```bash
rtk cargo test --test protocol chat_stream_canonicalizer_ -- --nocapture
```

Expected: the null-delta, identity-drift, text-EOF, and tool-EOF tests fail for
their documented current behavior; existing negative tests remain green.

### Task 2: Add Gateway Combination Red Test

**Files:**
- Modify: `tests/gateway/chat/streaming.rs:379-519`
- Test: `tests/gateway/chat/streaming.rs`

- [ ] **Step 1: Add a mock upstream EOF compatibility test**

Copy the setup pattern from `downstream_chat_stream_is_proxied_as_event_stream`
into a new test named
`downstream_chat_stream_canonicalizes_domestic_provider_eof_variants`. The mock
upstream returns these frames and then closes without `[DONE]`:

```rust
let chunks = vec![
    Ok::<Bytes, std::io::Error>(Bytes::from_static(
        b"data: {\"id\":\"first-id\",\"created\":10,\"model\":\"gpt-4.1-mini\",\"choices\":[{\"index\":0,\"delta\":null,\"finish_reason\":null}]}\n\n",
    )),
    Ok(Bytes::from_static(
        b"data: {\"id\":\"later-id\",\"created\":20,\"model\":\"provider-alias\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
    )),
];
```

Assert HTTP 200, no `upstream_stream_error_event`, canonical output identity
`first-id`/`gpt-4.1-mini`/`10`, `delta:{}`, content `OK`, synthesized
`finish_reason:"stop"`, exactly one `data: [DONE]`, and a successful usage log.

- [ ] **Step 2: Run the exact gateway test and verify RED**

Run:

```bash
rtk cargo test downstream_chat_stream_canonicalizes_domestic_provider_eof_variants -- --nocapture
```

Expected: FAIL because the first `delta:null` is rejected by the current
canonicalizer.

### Task 3: Implement Minimal Stream Normalization

**Files:**
- Modify: `src/protocol.rs:94-347`
- Test: `tests/protocol.rs`
- Test: `tests/gateway/chat/streaming.rs`

- [ ] **Step 1: Normalize a null delta without counting it as output**

Before `as_object_mut`, replace only JSON null with an empty object:

```rust
let delta = choice
    .entry("delta")
    .or_insert_with(|| Value::Object(Map::new()));
if delta.is_null() {
    *delta = Value::Object(Map::new());
}
let delta = delta.as_object_mut().ok_or_else(Self::invalid_stream)?;
```

Strings, arrays, numbers, and booleans remain invalid.

- [ ] **Step 2: Preserve the first locked identity**

Keep initial type validation and first-event identity adoption. When
`identity_locked` is true, ignore later valid identity values and return `Ok(())`
instead of comparing them with the locked values. Continue to reject later
identity fields with invalid JSON types.

```rust
if self.identity_locked {
    return Ok(());
}
```

- [ ] **Step 3: Reuse semantic terminal synthesis for clean EOF**

Change `finish()` to call the same semantic synthesis path as `[DONE]` and
update its comment:

```rust
/// Finishes on a clean upstream EOF. A terminal may be synthesized only when
/// prior output makes the completion reason unambiguous.
pub fn finish(&mut self) -> Result<Vec<Value>, ProtocolError> {
    self.finish_inner(true)
}
```

The existing `saw_output_indices` and `saw_tool_call_indices` checks continue to
reject role/usage-only streams.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --test protocol chat_stream_canonicalizer_ -- --nocapture
rtk cargo test downstream_chat_stream_canonicalizes_domestic_provider_eof_variants -- --nocapture
```

Expected: all matching tests PASS.

- [ ] **Step 5: Commit the compatibility behavior**

```bash
rtk git add src/protocol.rs tests/protocol.rs tests/gateway/chat/streaming.rs
rtk git commit -m "fix(stream): tolerate recoverable chat SSE variants"
```

### Task 4: Add Safe Diagnostic Red Test

**Files:**
- Modify: `src/server/gateway/stream.rs`
- Test: `src/server/gateway/stream.rs`

- [ ] **Step 1: Add a test-only tracing capture and failing diagnostic test**

Add a `#[cfg(test)]` module which calls the planned diagnostic mapper with an
`InvalidUpstreamStream` static reason and a `StreamDiagnosticContext` containing
known request/upstream fields. Capture a local `tracing_subscriber::fmt`
subscriber and assert the output contains:

```text
request-diagnostic-marker
upstream-diagnostic-marker
canonicalize_push
Chat stream event has an invalid envelope or terminal
```

Seed `prompt-secret`, `tool-argument-secret`, `provider-message-secret`, and
`api-key-secret` only outside the context passed to the logger, then assert none
appear. Also assert the returned public error retains category
`upstream_stream_error_event` and generic message.

- [ ] **Step 2: Run the exact test and verify RED**

Run:

```bash
rtk cargo test stream_protocol_error_logs_safe_diagnostics -- --nocapture
```

Expected: FAIL to compile because `StreamDiagnosticContext` and the diagnostic
mapper do not exist.

### Task 5: Implement Safe Structural Diagnostics

**Files:**
- Modify: `src/server/gateway/stream.rs:419-436,744-833,1227-1289,1554-1590`
- Modify: `src/server/gateway/upstream.rs:1937-1942`
- Test: `src/server/gateway/stream.rs`

- [ ] **Step 1: Add the lightweight diagnostic context**

```rust
#[derive(Clone, Debug)]
pub(super) struct StreamDiagnosticContext {
    pub request_id: String,
    pub upstream_id: String,
    pub upstream_protocol: UpstreamProtocol,
    pub endpoint: String,
}
```

Add a constructor from `&StreamUsageLogContext` inside `stream.rs`; it must map
`upstream_key_id` to `upstream_id` and copy only the four fields above.

- [ ] **Step 2: Add the logging mapper**

Implement a helper that pattern-matches only
`ProtocolError::InvalidUpstreamStream { kind, message }`, emits
`tracing::warn!` with the diagnostic context, fixed phase, `?kind`, and the
static message, then calls unchanged `protocol_error_to_gateway(error)`.
No dynamic protocol error string or payload is logged.

- [ ] **Step 3: Route canonicalizer errors through the logging mapper**

Replace the four direct canonicalizer mappings in proxied/translated
`push`/`finish` paths with the new helper and phases `canonicalize_push` and
`canonicalize_finish`. Construct context from the existing state's
`log_context` only on the error path.

- [ ] **Step 4: Route aggregation errors through the logging mapper**

Add an owned `StreamDiagnosticContext` parameter to
`aggregate_upstream_sse_response`. Construct it at
`src/server/gateway/upstream.rs:1937` from the active request ID, upstream ID,
protocol, and endpoint. Use phases `aggregate_push` and `aggregate_finish`.

- [ ] **Step 5: Run diagnostic and stream tests and verify GREEN**

Run:

```bash
rtk cargo test stream_protocol_error_logs_safe_diagnostics -- --nocapture
rtk cargo test --test protocol chat_stream_canonicalizer_ -- --nocapture
rtk cargo test downstream_chat_stream_canonicalizes_domestic_provider_eof_variants -- --nocapture
```

Expected: all PASS; capture includes structural fields and no seeded secret.

- [ ] **Step 6: Commit diagnostics**

```bash
rtk git add src/server/gateway/stream.rs src/server/gateway/upstream.rs
rtk git commit -m "fix(stream): retain safe upstream failure diagnostics"
```

### Task 6: Run Regression Gates

**Files:**
- Verify: `src/protocol.rs`
- Verify: `src/server/gateway/stream.rs`
- Verify: `src/server/gateway/upstream.rs`
- Verify: `tests/protocol.rs`
- Verify: `tests/gateway/chat/streaming.rs`

- [ ] **Step 1: Format and check the diff**

```bash
rtk cargo fmt --all -- --check
rtk git diff --check
```

- [ ] **Step 2: Run the stream and Responses lifecycle suites**

```bash
rtk cargo test --test protocol
rtk cargo test --test gateway gateway::chat::streaming
rtk cargo test --test gateway gateway::responses::streaming
rtk cargo test --test gateway gateway::responses::stream_lifecycle
```

- [ ] **Step 3: Run Clippy and the full offline Rust suite**

```bash
rtk cargo clippy --all-targets --all-features --offline -- -D warnings
rtk cargo test --all-targets --all-features --offline
```

Expected: zero failures and zero warnings.

### Task 7: Run Live Upstream And Codex Acceptance

**Files:**
- Create: `docs/verification/2026-07-22-domestic-model-stream-stability.md`
- Verify: `scripts/installed_client_smoke.sh`

- [ ] **Step 1: Build and start an isolated candidate gateway**

Build the current commit, run it on a free local port with the existing state
copied into an isolated temporary store, and never recreate or mutate production
PostgreSQL/Redis volumes. Record the binary SHA-256 and image ID.

- [ ] **Step 2: Run required models serially**

For `glm-5.1`, `glm-5.2`, `MiniMax-M2.7`, and `deepseek-v4-pro`, run portal-
equivalent Codex 0.144.6 with `stream_max_retries=0`. Execute the model-specific
text, read-only tool, reasoning, and approximately-20k cases defined by the
design. Run `deepseek-v4-flash` text as non-blocking coverage.

- [ ] **Step 3: Record exact acceptance evidence**

Write the candidate commit/image, gateway port, client version, model route,
case, latency, input/output usage, terminal event, and pass/fail result to the
verification document. Do not record keys, prompts, tool arguments, model
reasoning, provider messages, or response bodies.

- [ ] **Step 4: Verify deployment readiness**

Acceptance requires every blocking case to finish with `response.completed`
and `[DONE]`, no structured error, all concurrency counters returned to zero,
and no secret marker in logs. Upstream 429/503 is rerun once serially and then
recorded as external availability rather than hidden.

- [ ] **Step 5: Commit the verification record**

```bash
rtk git add docs/verification/2026-07-22-domestic-model-stream-stability.md
rtk git commit -m "test(stream): record domestic model acceptance"
```
