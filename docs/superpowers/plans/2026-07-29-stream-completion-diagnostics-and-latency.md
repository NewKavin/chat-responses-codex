# Stream Completion Diagnostics And Latency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correct Responses terminal classification, expose incomplete upstream EOF as a typed failure, and show persisted first-token plus total latency in Admin and Portal usage logs.

**Architecture:** Replace message-derived body-drop classification with a typed lifecycle value and track both `response.completed` and `response.incomplete` as valid Responses terminals. Share a once-only first-token measurement between prefetch, stream parsing, and terminal usage logging, persist it as a nullable usage-log field, then render it through one frontend formatter. Preserve the existing pre-output recovery budget and prohibit gateway replay after usable output.

**Tech Stack:** Rust, Tokio, Axum, SSE protocol adapters, Serde, PostgreSQL, Vue 3, TypeScript, Element Plus, Vitest.

---

## File Structure

- `src/server/gateway.rs`: owns typed interruption classification, the shared first-token measurement handle, and terminal usage-log emission.
- `src/server/gateway/stream.rs`: observes usable output and protocol terminals, validates upstream stream endings, and emits typed in-stream errors.
- `src/server/gateway/upstream.rs`: carries the winning prefetch attempt's first-token handle through hedge selection into the stream body.
- `src/state/types.rs`: defines the backward-compatible `UsageLog.first_token_latency_ms` API field.
- `src/state/postgres.rs`: persists and reloads the nullable first-token value.
- `frontend/src/utils/logDisplay.ts`: owns the seconds-with-two-decimals formatter shared by both log pages.
- `frontend/src/views/admin/Logs.vue`: replaces the single admin duration value with the two-line latency cell.
- `frontend/src/views/portal/UsageHistory.vue`: applies the same latency cell to Portal usage history.
- Gateway, persistence, API, and frontend test files listed below lock each boundary before implementation.

### Task 1: Type downstream body drops and recognize explicit incomplete terminals

**Files:**
- Modify: `src/server/gateway.rs:3140-3295`
- Modify: `src/server/gateway/stream.rs:600-1140`
- Modify: `src/server/gateway/stream.rs:1180-1765`
- Modify: `tests/gateway/chat/streaming.rs:4920-5020`
- Modify: `tests/gateway/responses/stream_lifecycle.rs:780-950`
- Modify: `tests/gateway/responses/stream_lifecycle.rs:950-1135`

- [ ] **Step 1: Write failing typed-drop and `response.incomplete` terminal tests**

Update the existing Chat partial-drop assertion to require the neutral message:

```rust
assert_eq!(
    log.error_message.as_deref(),
    Some(
        "downstream response body dropped before semantic completion \
         (partial output delivered)"
    )
);
assert_eq!(log.status_code, 499);
assert_eq!(
    log.error_category.as_deref(),
    Some("stream_incomplete_close")
);
```

Add a Responses lifecycle test whose Chat upstream emits a text delta followed
by `finish_reason: "length"`. Read through the translated
`response.incomplete` frame, drop the body before any trailing frame, and assert:

```rust
assert!(delivered.iter().any(|frame| frame.contains("response.incomplete")));
drop(body);
wait_for_upstream_in_flight(&state, "up-1", 0).await;

let snapshot = state.snapshot().await;
assert_eq!(snapshot.usage_logs.len(), 1);
assert_eq!(snapshot.usage_logs[0].status_code, 200);
assert_eq!(snapshot.usage_logs[0].error_category, None);
assert_eq!(snapshot.upstreams[0].failure_count, 0);
```

Add the equivalent native Responses case using an upstream
`event: response.incomplete` frame and no client-side cancellation before that
frame. Assert that dropping immediately after the explicit terminal does not
produce `stream_incomplete_close` or an additional `response.failed` frame.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
rtk cargo test --test gateway translated_drop_after_response_incomplete_is_not_499
rtk cargo test --test gateway native_drop_after_response_incomplete_is_not_499
rtk cargo test --test gateway partial_output -- --nocapture
```

Expected: incomplete-terminal tests record 499 under the current
`response.completed`-only flag, and the neutral-message assertion fails with the
old `client disconnected during stream` text.

- [ ] **Step 3: Introduce a typed interruption value**

In `gateway.rs`, replace `stream_drop_interruption_message` and the Drop path's
call to `classify_stream_failure(&message)` with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamInterruption {
    DownstreamBodyDropped {
        usable_output_delivered: bool,
    },
}

impl StreamInterruption {
    fn status_and_category(self) -> (StatusCode, &'static str) {
        let status = StatusCode::from_u16(499).expect("499 is a valid status code");
        match self {
            Self::DownstreamBodyDropped {
                usable_output_delivered: true,
            } => (status, "stream_incomplete_close"),
            Self::DownstreamBodyDropped {
                usable_output_delivered: false,
            } => (status, "stream_client_cancelled"),
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::DownstreamBodyDropped {
                usable_output_delivered: true,
            } => "downstream response body dropped before semantic completion \
                  (partial output delivered)",
            Self::DownstreamBodyDropped {
                usable_output_delivered: false,
            } => "downstream response body dropped before semantic completion",
        }
    }
}
```

Change `finalize_stream_interruption` and
`spawn_stream_interruption_cleanup` to accept `StreamInterruption`, derive the
static message and category from the type, and pass those values directly to
`finalize_stream_error`. Keep `attribute_route_failure = false` for every typed
downstream body drop. Existing upstream error and watchdog paths continue to use
their current typed categories.

- [ ] **Step 4: Track a semantic terminal instead of completed-only state**

Rename the two stream-state fields from `semantic_completion_emitted` to
`semantic_terminal_emitted`. Add the bounded helper:

```rust
fn responses_event_is_terminal(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed" | "response.incomplete")
    )
}
```

Use the helper in proxied and translated event loops, including events returned
by `StreamTranslator::finish()`. Drop cleanup treats
`finished || semantic_terminal_emitted` as terminal lifecycle completion.
`response.incomplete` is forwarded byte-for-byte or through the existing
translator and is never rewritten to `response.completed`.

Update structured tracing field names to
`semantic_terminal_observed`. A valid incomplete terminal finalizes the current
usage row with HTTP status 200 and no gateway error category; it remains a
distinct Responses event visible to Codex.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway responses::stream_lifecycle::
rtk cargo test --test gateway chat::streaming::
```

Expected: explicit completed/incomplete terminal drops finalize without 499,
pre-terminal drops retain the two existing 499 categories, and downstream drops
do not increase route failure counts.

- [ ] **Step 6: Commit**

```bash
rtk git add src/server/gateway.rs src/server/gateway/stream.rs tests/gateway/chat/streaming.rs tests/gateway/responses/stream_lifecycle.rs
rtk git commit -m "fix(stream): classify explicit terminal outcomes"
```

### Task 2: Reject native Responses endings without a semantic terminal

**Files:**
- Modify: `src/server/gateway/stream.rs:650-1020`
- Modify: `tests/gateway/responses/stream_lifecycle.rs`
- Modify: `frontend/src/utils/logDisplay.ts`
- Modify: `frontend/src/utils/troubleshooting.ts`
- Modify: `frontend/tests/utils/logDisplay.spec.ts`
- Modify: `frontend/tests/utils/troubleshooting.spec.ts`

- [ ] **Step 1: Write failing native Responses clean-EOF tests**

Add a mock native Responses upstream that emits `response.created`, an output
item, and `response.output_text.delta`, then ends its body cleanly without
`response.completed`, `response.incomplete`, or `[DONE]`. Configure a second
candidate whose body contains `unexpected-replay`. Assert:

```rust
let body = body_text(response).await;
assert!(body.contains("partial-before-eof"), "{body}");
assert!(body.contains("event: response.failed"), "{body}");
assert!(body.contains("stream_upstream_incomplete_eof"), "{body}");
assert!(!body.contains("unexpected-replay"), "{body}");
assert_eq!(first_hits.load(Ordering::SeqCst), 1);
assert_eq!(second_hits.load(Ordering::SeqCst), 0);

let log = &state.snapshot().await.usage_logs[0];
assert_eq!(log.status_code, StatusCode::BAD_GATEWAY.as_u16());
assert_eq!(
    log.error_category.as_deref(),
    Some("stream_upstream_incomplete_eof")
);
```

Add a separate `[DONE]`-without-terminal fixture. It must produce the existing
`upstream_stream_incomplete` protocol category and must not forward `[DONE]`
before its typed error frames. Preserve tests proving `response.completed` and
`response.incomplete` followed by EOF are valid terminals.

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
rtk cargo test --test gateway native_responses_clean_eof_without_terminal_is_typed_502
rtk cargo test --test gateway native_responses_done_without_terminal_is_incomplete
```

Expected: clean EOF is currently recorded as 200, and `[DONE]` can be accepted
without the required Responses terminal.

- [ ] **Step 3: Make stream ending explicit**

Replace the boolean `allow_missing_terminal` parameter with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEnd {
    Done,
    Eof,
}

fn upstream_incomplete_eof_error() -> GatewayError {
    stream_gateway_error(
        StatusCode::BAD_GATEWAY,
        "upstream SSE ended before a required semantic terminal",
        "stream_upstream_incomplete_eof",
    )
}
```

`finish_stream(StreamEnd::Done)` uses the existing canonicalizer
`finish_after_done`; `finish_stream(StreamEnd::Eof)` uses `finish`. Before
setting `finished = true`, native Responses pass-through requires
`semantic_terminal_emitted`:

```rust
if self.canonicalizer.is_none()
    && self.rewrite_responses_events
    && !self.semantic_terminal_emitted
{
    return Err(match end {
        StreamEnd::Done => stream_gateway_error(
            StatusCode::BAD_GATEWAY,
            "upstream SSE emitted [DONE] before a required semantic terminal",
            "upstream_stream_incomplete",
        ),
        StreamEnd::Eof => upstream_incomplete_eof_error(),
    });
}
```

Validate `[DONE]` before adding it to `pending`; invalid `[DONE]` must emit only
the endpoint-appropriate typed error sequence. A post-commit error goes through
`finish_with_gateway_error_after_pending`, records status 502, attributes the
upstream route failure, releases all reservations, and never returns to the
routing loop.

Keep the existing Chat canonicalizer clean-EOF synthesis. Metadata-only Chat
EOF still fails through the canonicalizer and is not converted to the new native
Responses category.

- [ ] **Step 4: Add the new category to log presentation metadata**

Add a bounded label and troubleshooting mapping without changing existing
categories:

```ts
{ value: 'stream_upstream_incomplete_eof', label: '上游流未完整结束' }
```

The suggestion must tell the operator to verify the upstream Responses terminal
sequence. It must not claim client cancellation or recommend increasing timeouts
without evidence.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway responses::stream_lifecycle::
rtk npm --prefix frontend test -- --run tests/utils/logDisplay.spec.ts tests/utils/troubleshooting.spec.ts
```

Expected: incomplete native EOF is a typed 502 after output, terminal events
remain valid, and no second upstream is called after stream commit.

- [ ] **Step 6: Commit**

```bash
rtk git add src/server/gateway/stream.rs tests/gateway/responses/stream_lifecycle.rs frontend/src/utils/logDisplay.ts frontend/src/utils/troubleshooting.ts frontend/tests/utils/logDisplay.spec.ts frontend/tests/utils/troubleshooting.spec.ts
rtk git commit -m "fix(stream): reject Responses EOF without terminal"
```

### Task 3: Persist first-token latency through gateway, storage, and APIs

**Files:**
- Modify: `src/server/gateway.rs:1135-1290`
- Modify: `src/server/gateway.rs:1408-1460`
- Modify: `src/server/gateway/stream.rs:492-570`
- Modify: `src/server/gateway/stream.rs:600-1765`
- Modify: `src/server/gateway/upstream.rs:830-1020`
- Modify: `src/server/gateway/upstream.rs:2080-2205`
- Modify: `src/state/types.rs:444-475`
- Modify: `src/state/postgres.rs:276-289`
- Modify: `src/state/postgres.rs:515-544`
- Modify: `src/state/postgres.rs:1190-1243`
- Modify: `src/state/postgres.rs:1290-1315`
- Modify: `src/state/postgres.rs:1552-1595`
- Modify: `src/state/log_queries.rs`
- Modify: `tests/admin_dashboard.rs`
- Modify: `tests/downstream_quota.rs`
- Modify: `tests/log_rotation.rs`
- Modify: `tests/portal_helpers.rs`
- Modify: `tests/redis_runtime.rs`
- Modify: `tests/state_store.rs`
- Modify: `tests/gateway/responses/stream_lifecycle.rs`
- Modify: `tests/admin_logs.rs`
- Modify: `tests/portal_api.rs`
- Modify: `tests/postgres_roundtrip.rs`

- [ ] **Step 1: Write failing serde, stream, API, and PostgreSQL tests**

Add an old-JSON compatibility test:

```rust
let value = serde_json::json!({
    "id": "old-log",
    "downstream_key_id": "down",
    "upstream_key_id": "up",
    "endpoint": "/v1/responses",
    "model": "gpt-4",
    "request_id": "req-old",
    "status_code": 200,
    "prompt_tokens": 1,
    "completion_tokens": 1,
    "total_tokens": 2,
    "latency_ms": 100,
    "created_at": 1
});
let log: UsageLog = serde_json::from_value(value).unwrap();
assert_eq!(log.first_token_latency_ms, None);
```

In the delayed streaming fixture, delay the first usable output by at least 60
ms while allowing earlier comment/metadata frames. After completion assert:

```rust
let log = &state.snapshot().await.usage_logs[0];
let first = log
    .first_token_latency_ms
    .expect("stream should record first usable output latency");
assert!(first >= 40, "first usable output was recorded too early: {first}");
assert!(first <= log.latency_ms);
```

Extend the partial-drop test to require `Some(first)` and the before-output drop
test to require `None`. Add a non-streaming request assertion requiring `None`.

Set one Admin fixture to `Some(10_650)` and one Portal fixture to
`Some(10_650)`, then assert the JSON field is the number `10650`; retain another
fixture with `None` and assert JSON `null`.

In `tests/postgres_roundtrip.rs`, set:

```rust
first_token_latency_ms: Some(42),
latency_ms: 78,
```

The existing whole-object JSON equality must round trip both values. Add a
second row with `first_token_latency_ms: None` to prove SQL `NULL` decoding.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
rtk cargo test --test gateway first_token_latency -- --nocapture
rtk cargo test --test admin_logs --test portal_api first_token_latency
rtk env PG_TEST_DATABASE_URL="$PG_TEST_DATABASE_URL" cargo test --test postgres_roundtrip postgres_roundtrip_preserves_normalized_state_and_authoritative_empty_collections
```

Expected: `UsageLog` has no first-token field, API fixtures cannot compile, and
PostgreSQL does not persist the value. If `PG_TEST_DATABASE_URL` is absent, keep
the test ignored/skipped and run it during Task 7 with the project test database.

- [ ] **Step 3: Add the backward-compatible data field and PostgreSQL column**

Add to `UsageLog` immediately before `latency_ms`:

```rust
#[serde(default)]
pub first_token_latency_ms: Option<u64>,
pub latency_ms: u64,
```

Add `first_token_latency_ms BIGINT NULL` to `CREATE TABLE usage_logs` and:

```sql
ALTER TABLE usage_logs
    ADD COLUMN IF NOT EXISTS first_token_latency_ms BIGINT NULL;
```

Add the column immediately before `latency_ms` in both SELECT lists and the
INSERT list. Bind it with:

```rust
let first_token_latency_ms = log.first_token_latency_ms.map(|value| value as i64);
```

Shift the row decoder so it reads the nullable value before total latency:

```rust
first_token_latency_ms: row
    .get::<_, Option<i64>>(19)
    .map(i64_to_u64),
latency_ms: i64_to_u64(row.get::<_, i64>(20)),
created_at: i64_to_u64(row.get::<_, i64>(21)),
```

Insert `first_token_latency_ms: None` into every existing test/fixture and
non-stream production literal. Do not change aggregate calculations that use
`latency_ms`.

- [ ] **Step 4: Add a winning-attempt first-token measurement handle**

Define next to `StreamBodyReadDiagnosticContext`:

```rust
#[derive(Clone, Debug, Default)]
struct FirstTokenLatency(Arc<OnceLock<u64>>);

impl FirstTokenLatency {
    fn observe(&self, started: Instant) {
        let _ = self
            .0
            .set(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
    }

    fn get(&self) -> Option<u64> {
        self.0.get().copied()
    }
}
```

Add `first_token_latency: FirstTokenLatency` to
`StreamBodyReadDiagnosticContext` and `StreamUsageLogContext`. Every physical
stream attempt receives its own handle. Hedge selection carries the winning
attempt's diagnostic context, and `StreamUsageLogContext` clones that winning
handle. Losing attempts can never write the winner's usage row.

In `prefetch_first_usable_output`, record only on
`FirstUsableOutputResult::Ready`:

```rust
diagnostic_context
    .first_token_latency
    .observe(diagnostic_context.started);
return Ok(reader);
```

For paths that do not prefetch, proxied and translated stream parsing call the
same once-only `observe` method when `stream_event_has_usable_output` first
returns true. Keepalive, metadata, empty deltas, and terminal-only events never
call it.

When emitting a terminal streaming usage row, use:

```rust
first_token_latency_ms: first_token_latency.get(),
latency_ms: started.elapsed().as_millis() as u64,
```

Immediate/non-stream usage rows always set `first_token_latency_ms: None`.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway responses::stream_lifecycle::
rtk cargo test --test gateway chat::streaming::
rtk cargo test --test admin_logs
rtk cargo test --test portal_api
rtk cargo test --no-run
```

Expected: all UsageLog literals compile, old JSON loads as `None`, streaming rows
preserve first-token latency on later failure/drop, and non-stream rows remain
null.

- [ ] **Step 6: Run PostgreSQL round trip**

Run with the repository PostgreSQL test URL:

```bash
rtk env PG_TEST_DATABASE_URL="$PG_TEST_DATABASE_URL" cargo test --test postgres_roundtrip -- --test-threads=1
```

Expected: numeric and null first-token values round trip and existing usage rows
remain equal.

- [ ] **Step 7: Commit**

```bash
rtk git add src/server/gateway.rs src/server/gateway/stream.rs src/server/gateway/upstream.rs src/state/types.rs src/state/postgres.rs src/state/log_queries.rs tests/admin_dashboard.rs tests/admin_logs.rs tests/downstream_quota.rs tests/gateway/responses/stream_lifecycle.rs tests/log_rotation.rs tests/portal_api.rs tests/portal_helpers.rs tests/postgres_roundtrip.rs tests/redis_runtime.rs tests/state_store.rs
rtk git commit -m "feat(logs): persist first-token latency"
```

### Task 4: Render first-token and total latency consistently

**Files:**
- Modify: `frontend/src/types/index.ts:165-187`
- Modify: `frontend/src/utils/logDisplay.ts`
- Modify: `frontend/tests/utils/logDisplay.spec.ts`
- Modify: `frontend/src/views/admin/Logs.vue:224-228`
- Modify: `frontend/src/views/portal/UsageHistory.vue:101-105`
- Modify: `frontend/tests/views/admin-ui.spec.ts:105-117`
- Modify: `frontend/tests/views/portal-ui.spec.ts:26-39`

- [ ] **Step 1: Write failing formatter and page-source tests**

Add to `frontend/tests/utils/logDisplay.spec.ts`:

```ts
expect(formatLatencySeconds(10_650)).toBe('10.65s')
expect(formatLatencySeconds(15_120)).toBe('15.12s')
expect(formatLatencySeconds(0)).toBe('0.00s')
expect(formatLatencySeconds(null)).toBe('-')
expect(formatLatencySeconds(undefined)).toBe('-')
```

Extend both view source tests:

```ts
expect(page).toContain('label="延迟"')
expect(page).toContain('首字')
expect(page).toContain('总耗时')
expect(page).toContain('formatLatencySeconds(row.first_token_latency_ms)')
expect(page).toContain('formatLatencySeconds(row.latency_ms)')
expect(page).not.toContain('{{ row.latency_ms }}ms')
```

Use `history` instead of `page` for the Portal assertions.

- [ ] **Step 2: Run frontend tests and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/utils/logDisplay.spec.ts tests/views/admin-ui.spec.ts tests/views/portal-ui.spec.ts
```

Expected: formatter export, new field, and latency markup are absent.

- [ ] **Step 3: Add the shared type and formatter**

Extend `UsageLog`:

```ts
first_token_latency_ms?: number | null
latency_ms: number
```

Add to `frontend/src/utils/logDisplay.ts`:

```ts
export const formatLatencySeconds = (milliseconds?: number | null) => {
  if (milliseconds == null || !Number.isFinite(milliseconds)) return '-'
  return `${(Math.max(0, milliseconds) / 1000).toFixed(2)}s`
}
```

- [ ] **Step 4: Replace both table cells**

Import `formatLatencySeconds` from `@/utils/logDisplay` in both views. Replace
the old column with:

```vue
<el-table-column label="延迟" width="132">
  <template #default="{ row }">
    <div class="latency-cell">
      <span><small>首字</small>{{ formatLatencySeconds(row.first_token_latency_ms) }}</span>
      <span><small>总耗时</small>{{ formatLatencySeconds(row.latency_ms) }}</span>
    </div>
  </template>
</el-table-column>
```

Add local, non-shifting styles:

```css
.latency-cell {
  display: grid;
  gap: 2px;
  line-height: 1.35;
  white-space: nowrap;
}

.latency-cell span {
  display: grid;
  grid-template-columns: 42px minmax(0, 1fr);
  align-items: baseline;
}

.latency-cell small {
  color: var(--crc-text-muted);
  font-size: 11px;
}
```

Use the existing `--crc-text-muted` token shown above; do not introduce a new
palette value.

- [ ] **Step 5: Run frontend tests and build GREEN**

Run:

```bash
rtk npm --prefix frontend test -- --run tests/utils/logDisplay.spec.ts tests/views/admin-ui.spec.ts tests/views/portal-ui.spec.ts
rtk npm --prefix frontend run build
```

Expected: both pages contain the two labels, numeric/null formatting passes, and
TypeScript/Vue compilation succeeds.

- [ ] **Step 6: Commit**

```bash
rtk git add frontend/src/types/index.ts frontend/src/utils/logDisplay.ts frontend/src/views/admin/Logs.vue frontend/src/views/portal/UsageHistory.vue frontend/tests/utils/logDisplay.spec.ts frontend/tests/views/admin-ui.spec.ts frontend/tests/views/portal-ui.spec.ts
rtk git commit -m "feat(ui): show first-token and total latency"
```

### Task 5: Integrate, review, and return to the Redis deployment plan

**Files:**
- Verify all files changed by Tasks 1-4
- Resume: `docs/superpowers/plans/2026-07-29-optional-redis-runtime-coordination.md`

- [ ] **Step 1: Run complete feature verification**

```bash
rtk cargo fmt --check
rtk cargo check --all-targets
rtk cargo test --test gateway chat::streaming::
rtk cargo test --test gateway responses::stream_lifecycle::
rtk cargo test --test admin_logs --test portal_api
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run build
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --no-run
rtk git diff --check
```

Expected: zero failures and zero warnings.

- [ ] **Step 2: Perform two independent read-only reviews**

One reviewer checks terminal lifecycle, error ordering, retry/resource semantics,
and Codex behavior. A second reviewer checks UsageLog compatibility, PostgreSQL
column order, API nullability, and frontend formatting. Every Critical or
Important finding receives a failing regression test before correction.

- [ ] **Step 3: Verify the feature commit range and clean worktree**

```bash
rtk git log --oneline 28358fb..HEAD
rtk git status --short
rtk git diff --check 28358fb..HEAD
```

Expected: only the planned feature commits are present and the worktree is
clean.

- [ ] **Step 4: Resume the original Redis plan without skipping tasks**

Continue at Task 6 of
`docs/superpowers/plans/2026-07-29-optional-redis-runtime-coordination.md`:

1. Add the optional Redis Compose profile, environment surface, documentation,
   and isolated smoke script.
2. Run original Task 6 verification and commit it separately.
3. Execute original Task 7 full Rust/frontend/Redis verification, independent
   reviews, disposable Redis cleanup, default-disabled production deployment,
   GLM/Codex/tool/stream smoke tests, and enabled multi-instance Redis smoke.

Production must remain Redis-disabled by default. The disposable development
Redis remains `redis://127.0.0.1:16380`; never use or stop port `16379`, which is
owned by the unrelated `sub2api-redis` service.
