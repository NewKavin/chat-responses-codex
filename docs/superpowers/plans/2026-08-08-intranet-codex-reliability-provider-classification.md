# Provider Failure Classification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route explicit provider concurrency hidden behind HTTP 502/503 through account recovery, recognize explicit context overflow before generic 5xx, and leave generic transient failures bounded and honest.

**Architecture:** Parse one bounded structured error document, assign a semantic identity before applying HTTP defaults, and derive both gateway action and legacy log summary from that same result. Keep context overflow outside persistent route-health classes; it is a request-shape condition with one pre-output retry. Preserve the provider status and authoritative `Retry-After` in diagnostics and recovery state.

**Tech Stack:** Rust, Reqwest headers, Serde JSON, Axum integration tests, account recovery.

---

## File Structure

- `src/upstream_feedback.rs`: owns structured parsing, semantic precedence, and a single concurrency predicate.
- `src/server/gateway/errors.rs`: maps semantic concurrency to `ConcurrencyFull` regardless of outer 429/502/503.
- `src/server/gateway/upstream.rs`: consumes classified context identity and sends account feedback.
- `src/server/gateway.rs`: preserves same-route and next-route retry boundaries.
- `tests/unit/upstream_feedback.rs`: table-driven semantic and false-positive tests.
- `tests/gateway/responses/upstream_feedback.rs`: end-to-end account recovery and terminal behavior.
- `tests/gateway/chat/context.rs`: context wrapper and route-health behavior.

### Task 1: Add A Semantic Identity To Classified Failures

**Files:**
- Modify: `src/upstream_feedback.rs:8-20`
- Modify: `tests/unit/upstream_feedback.rs:1-270`

- [ ] **Step 1: Add semantic assertion helpers and failing tables**

Add:

```rust
fn assert_semantic(status: u16, body: &str, expected: UpstreamResponseSemantic) {
    let headers = reqwest::header::HeaderMap::new();
    let classified = classify_upstream_response(UpstreamFeedbackInput {
        status,
        headers: &headers,
        body: Some(body),
        target_model: Some("glm-5.2"),
    });
    assert_eq!(classified.semantic, expected);
}

#[test]
fn explicit_concurrency_semantics_are_status_independent() {
    for status in [429, 502, 503] {
        for body in [
            r#"{"error":{"code":"concurrency_limit_exceeded"}}"#,
            r#"{"error":{"message":"concurrency limit exceeded"}}"#,
            r#"{"error":{"message":"您当前使用该API的并发数过高"}}"#,
            r#"{"error":{"message":"当前分组上游负载已饱和"}}"#,
        ] {
            assert_semantic(status, body, UpstreamResponseSemantic::ExplicitConcurrency);
        }
    }
}

#[test]
fn explicit_context_overflow_wins_over_outer_5xx() {
    for status in [400, 413, 502, 503] {
        assert_semantic(
            status,
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#,
            UpstreamResponseSemantic::ExplicitContextOverflow,
        );
    }
}

#[test]
fn generic_busy_and_relay_failures_are_not_concurrency_or_context() {
    for body in [
        r#"{"error":{"message":"server busy"}}"#,
        r#"{"error":{"message":"temporarily unavailable"}}"#,
        r#"{"error":{"code":"relay_error","message":"relay failed"}}"#,
    ] {
        assert_semantic(503, body, UpstreamResponseSemantic::Generic);
    }
}
```

- [ ] **Step 2: Run and verify RED**

```bash
rtk cargo test --lib explicit_concurrency_semantics_are_status_independent
rtk cargo test --lib explicit_context_overflow_wins_over_outer_5xx
```

Expected: `UpstreamResponseSemantic` and `semantic` are missing.

- [ ] **Step 3: Add the semantic enum**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamResponseSemantic {
    Generic,
    ExplicitConcurrency,
    ExplicitContextOverflow,
    TargetModelCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifiedUpstreamFailure {
    pub class: FailureClass,
    pub semantic: UpstreamResponseSemantic,
    pub upstream_status: Option<u16>,
    pub retry_after: Option<Duration>,
}
```

Keep `class` for route-health and terminal aggregation compatibility. `ExplicitContextOverflow` uses `FailureClass::RequestRejected`, which already finishes route health as success/no cooldown; semantic action must never be inferred back from that class.

- [ ] **Step 4: Run and commit the type change**

```bash
rtk cargo test --lib upstream_feedback
rtk git add src/upstream_feedback.rs tests/unit/upstream_feedback.rs
rtk git commit -m "refactor(feedback): separate response semantics from route health" -m "Constraint: Context overflow is not route-health evidence" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 2: Implement Semantic Precedence With One Predicate

**Files:**
- Modify: `src/upstream_feedback.rs:23-495`
- Test: `tests/unit/upstream_feedback.rs`

- [ ] **Step 1: Add narrow structured context recognition**

Extend `StructuredError` with:

```rust
fn is_context_overflow(&self, message: &str) -> bool {
    self.has_code(&[
        "context_length_exceeded",
        "context_window_exceeded",
        "input_too_long",
        "request_too_large",
        "prompt_too_long",
        "max_context_length_exceeded",
    ]) || [
        "request exceeds limit",
        "maximum context length",
        "context length exceeded",
        "context window exceeded",
        "input is too long",
        "prompt is too long",
        "超过最大上下文",
        "上下文长度超出",
        "输入内容过长",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn is_explicit_concurrency(&self, message: &str) -> bool {
    self.is_concurrency_capacity() || message_is_concurrency_capacity(message)
}
```

Do not include `busy`, `繁忙`, `overloaded`, `temporary`, or a bare numeric 429 in the explicit concurrency predicate.

- [ ] **Step 2: Replace status-first classification with the approved precedence**

At the start of `classify_upstream_response`, compute:

```rust
let (semantic, class) = if parsed.is_context_overflow(&message) {
    (
        UpstreamResponseSemantic::ExplicitContextOverflow,
        FailureClass::RequestRejected,
    )
} else if parsed.is_explicit_concurrency(&message) {
    (
        UpstreamResponseSemantic::ExplicitConcurrency,
        FailureClass::CapacityUnavailable,
    )
} else if message_names_target_model(&message, input.target_model) {
    (
        UpstreamResponseSemantic::TargetModelCapacity,
        FailureClass::CapacityUnavailable,
    )
} else {
    (
        UpstreamResponseSemantic::Generic,
        classify_nonsemantic_default(input.status, &parsed, &message),
    )
};
```

Extract the existing credential, quota, model, feature, protocol, request-rejection, and HTTP-default branches into `classify_nonsemantic_default`. Inside that helper, HTTP 500-599 defaults to `TransientServer`; do not repeat concurrency or context matching there.

- [ ] **Step 3: Derive the legacy log summary from the same classified value**

Add:

```rust
impl ClassifiedUpstreamFailure {
    pub fn summary_classification(self) -> UpstreamFeedbackClassification {
        match self.semantic {
            UpstreamResponseSemantic::ExplicitConcurrency => {
                UpstreamFeedbackClassification::ConcurrencyFull
            }
            UpstreamResponseSemantic::TargetModelCapacity => {
                UpstreamFeedbackClassification::ProviderBusy
            }
            UpstreamResponseSemantic::ExplicitContextOverflow => {
                UpstreamFeedbackClassification::Unknown
            }
            UpstreamResponseSemantic::Generic => match self.class {
                FailureClass::RateLimited | FailureClass::KeyQuota => {
                    UpstreamFeedbackClassification::RateLimited
                }
                FailureClass::TransientServer | FailureClass::Transport => {
                    UpstreamFeedbackClassification::TemporaryUnavailable
                }
                FailureClass::ProtocolUnsupported => {
                    UpstreamFeedbackClassification::ProtocolUnsupported
                }
                _ => UpstreamFeedbackClassification::Unknown,
            },
        }
    }
}
```

Use this in `src/server/gateway/upstream.rs` instead of separately reparsing the body through `UpstreamFeedbackClassification::from_response`. Leave the public legacy method only for external callers/tests and make it delegate to `classify_upstream_response`.

- [ ] **Step 4: Run all classifier tests**

```bash
rtk cargo test --lib upstream_feedback
```

Expected: the old 429 cases retain their class, explicit 502/503 concurrency is semantic concurrency, context wrappers are semantic context, and generic 5xx remains transient.

- [ ] **Step 5: Commit precedence**

```bash
rtk git add src/upstream_feedback.rs src/server/gateway/upstream.rs tests/unit/upstream_feedback.rs
rtk git commit -m "fix(feedback): classify explicit semantics before 5xx defaults" -m "Rejected: Treat every busy 503 as concurrency | false account recovery" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 3: Send Explicit 5xx Concurrency Through Account Recovery

**Files:**
- Modify: `src/server/gateway/errors.rs:374-480`
- Modify: `src/server/gateway/upstream.rs:1933-2075`
- Test: `tests/gateway/responses/upstream_feedback.rs`

- [ ] **Step 1: Add a two-failing-six-healthy integration test**

Create `explicit_concurrency_5xx_uses_account_recovery_and_healthy_routes`. Configure eight mock accounts for one model. Accounts 1 and 2 return the following before output; accounts 3-8 return success:

```rust
(
    StatusCode::BAD_GATEWAY,
    [(header::RETRY_AFTER, "1")],
    json!({"error": {"code": "concurrency_limit_exceeded", "message": "并发数过高"}}),
)
```

Assert:

```rust
assert_eq!(response.status(), StatusCode::OK);
assert!(failing_hits.load(Ordering::SeqCst) >= 1);
assert!(healthy_hits.load(Ordering::SeqCst) >= 1);
let usage = state.usage_logs().await;
assert_eq!(usage.iter().filter(|row| row.status_code >= 400).count(), 0);
```

Add a second all-accounts case that releases after a controlled delay and asserts elapsed time is governed by `upstream_concurrency_recovery_max_wait_ms`, not the ordinary three-round budget.

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test gateway explicit_concurrency_5xx_uses_account_recovery_and_healthy_routes
```

Expected: the 502 is mapped to transient server recovery and terminates or uses the ordinary route budget.

- [ ] **Step 3: Map semantic concurrency before route class**

At the start of `GatewayError::from_classified_upstream_failure`, add:

```rust
if failure.semantic == UpstreamResponseSemantic::ExplicitConcurrency {
    return Self::ConcurrencyFull {
        message,
        retry_after: failure.retry_after,
    };
}
```

Remove the `upstream_status == Some(429)` condition from the concurrency decision. Keep `upstream_status` in the classified input and account diagnostics; do not expose raw provider bodies.

- [ ] **Step 4: Preserve provider status in account feedback diagnostics**

Extend the bounded attempt diagnostic with `provider_concurrency_status: Option<u16>`. Populate it from `classified_feedback.upstream_status` when semantic concurrency is selected. Do not change the account key `(upstream_id, key_fingerprint)` or FIFO/single-probe algorithm.

- [ ] **Step 5: Run recovery tests**

```bash
rtk cargo test --test gateway explicit_concurrency_5xx_uses_account_recovery_and_healthy_routes
rtk cargo test --test gateway explicit_concurrency_retry_after_survives_account_wait
rtk cargo test --test account_concurrency
```

Expected: all pass and the explicit deadline is never shortened.

- [ ] **Step 6: Commit recovery mapping**

```bash
rtk git add src/server/gateway/errors.rs src/server/gateway/upstream.rs src/state/usage.rs tests/gateway/responses/upstream_feedback.rs tests/account_concurrency.rs
rtk git commit -m "fix(routing): recover explicit concurrency wrapped in 5xx" -m "Constraint: Preserve provider Retry-After" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 4: Recognize Context Wrappers Without Cooling Routes

**Files:**
- Modify: `src/server/gateway/upstream.rs:2023-2062`
- Modify: `src/server/gateway/errors.rs:777-827`
- Test: `tests/gateway/chat/context.rs`

- [ ] **Step 1: Add 400/413/502/503 context wrapper tests**

Use a table-driven mock with each outer status and a structured `context_length_exceeded` body. For each case assert the gateway performs at most two physical attempts, returns `upstream_context_limit` if the protected retry still fails, and leaves exact route health empty:

```rust
assert_eq!(response.status(), StatusCode::BAD_REQUEST);
assert_eq!(error["error"]["code"], "upstream_context_limit");
assert!(attempts.load(Ordering::SeqCst) <= 2);
assert!(state.route_health_snapshot(&route).await.unwrap().is_none());
```

- [ ] **Step 2: Verify RED for 5xx wrappers**

```bash
rtk cargo test --test gateway explicit_context_wrappers_do_not_cool_route
```

Expected: 502/503 currently become transient route failures and downstream 503.

- [ ] **Step 3: Branch on classified semantic, not status plus raw text**

Replace:

```rust
status == StatusCode::BAD_REQUEST && is_context_limit_error(&error_text)
```

with:

```rust
classified_feedback.semantic == UpstreamResponseSemantic::ExplicitContextOverflow
```

Return `GatewayError::upstream_context_limit(..., status)` after the single retry is exhausted. In `route_failure_class`, return `None` for `upstream_context_limit` so later routing code produces `RouteOutcome::Cancelled`; do not record it as a request rejection or transient failure.

- [ ] **Step 4: Prove generic 503 does not compact or wait as concurrency**

Add `generic_503_remains_bounded_transient_failure` with `{"message":"server busy"}`. Assert the context payload is byte-for-byte identical across attempts, the ordinary route budget is used, and the terminal category is `upstream_routes_exhausted` or the existing network category.

- [ ] **Step 5: Run and commit context identity**

```bash
rtk cargo test --test gateway explicit_context_wrappers_do_not_cool_route
rtk cargo test --test gateway generic_503_remains_bounded_transient_failure
rtk git add src/server/gateway/upstream.rs src/server/gateway/errors.rs tests/gateway/chat/context.rs
rtk git commit -m "fix(context): preserve explicit overflow semantics through 5xx" -m "Constraint: Generic 5xx never triggers compaction" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 5: Verify Replay And Terminal Boundaries

**Files:**
- Modify: `tests/gateway/responses/upstream_feedback.rs`
- Modify: `tests/gateway/stream_only_learning.rs`

- [ ] **Step 1: Add a semantic-output concurrency failure regression**

Stream a reasoning delta and partial tool arguments, then an upstream concurrency error. Assert the alternate route is never called and the downstream receives exactly one typed in-band failure:

```rust
assert_eq!(primary_hits.load(Ordering::SeqCst), 1);
assert_eq!(alternate_hits.load(Ordering::SeqCst), 0);
assert_eq!(count_tool_call_ids(&events, "call_1"), 1);
assert_eq!(count_terminal_events(&events), 1);
```

- [ ] **Step 2: Run stream and full feedback suites**

```bash
rtk cargo test --test gateway semantic_output_blocks_concurrency_failover
rtk cargo test --test gateway upstream_feedback
rtk cargo test --test gateway stream_only_learning
```

Expected: no route switch after text, reasoning, tool identity, or tool arguments.

- [ ] **Step 3: Commit the replay regression**

```bash
rtk git add tests/gateway/responses/upstream_feedback.rs tests/gateway/stream_only_learning.rs
rtk git commit -m "test(stream): forbid replay after semantic concurrency failure" -m "Confidence: high" -m "Scope-risk: narrow"
```
