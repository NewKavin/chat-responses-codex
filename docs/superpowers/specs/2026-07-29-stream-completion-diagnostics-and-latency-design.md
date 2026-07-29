# Stream Completion Diagnostics And Latency Design

Date: 2026-07-29

## Goal

Make Codex streaming failures diagnosable and prevent incomplete upstream
streams from being recorded as successful gateway requests. Replace the current
single duration display in usage history with a consistent first-output and
total-duration display.

The motivating production symptom is a usage-log entry with:

```text
client disconnected during stream (partial output received)
```

The affected Codex client connects directly to the gateway, no reverse proxy is
in the request path, no person cancelled the request, and Codex subsequently
retries. The internal deployment cannot currently provide a request ID or raw
capture, so this design does not claim one unproven root cause. It corrects the
gateway states that can be proven locally and records enough safe structure to
distinguish the remaining causes on the next occurrence.

## Evidence And Current Behavior

The quoted message is generated only by the response-body `Drop` cleanup paths.
It means the downstream body was dropped before the gateway observed semantic
completion. It does not prove that a person cancelled the request. In this
deployment shape, Codex can close the old HTTP response when its own streaming
retry logic decides that the stream was interrupted.

The current code has three relevant weaknesses:

1. Drop cleanup derives status and category by parsing an English error message.
   A body dropped after usable output becomes `499 stream_incomplete_close`, but
   the text incorrectly sounds like a confirmed user action.
2. Stream cleanup marks `response.completed` as a terminal Responses event but
   does not give `response.incomplete` the same terminal-lifecycle treatment.
   Codex can legitimately receive `response.incomplete`, decide to retry, and
   close the old body while the gateway incorrectly records a partial-output
   499.
3. A raw pass-through stream with no canonicalizer can reach clean upstream EOF,
   call `finish_stream(false)`, and set `finished = true` without proving that a
   required protocol terminal was observed. A native Responses stream can
   therefore be recorded as successful after partial output and naked EOF.

Upstream read errors, decode errors, idle timeouts, and maximum-duration timeouts
already have separate classifications. They must remain separate from a
downstream body drop.

The usage log currently stores only `latency_ms`. For streaming requests this is
the elapsed time from the gateway's existing logical-request timing origin until
normal or abnormal stream finalization. No persistent first-output latency field
exists. Admin Logs and Portal Usage History both render the raw value as
`<milliseconds>ms` in a single `耗时` column.

## Existing Contracts And Precedence

This design extends, rather than replaces, the following contracts:

- `2026-07-17-first-semantic-event-stream-recovery-design.md`: recovery is only
  allowed before any usable semantic output reaches the downstream. Text,
  reasoning, and tool output all commit the stream. A committed stream is never
  replayed by the gateway.
- `2026-07-22-domestic-model-stream-stability-design.md`: Chat canonicalization
  may synthesize a terminal on clean EOF only after unambiguous usable Chat
  semantics. Role-only, usage-only, comment-only, and empty-delta streams remain
  failures. This narrow Chat compatibility rule must not be generalized to
  native Responses streams.
- `2026-07-02-gateway-error-visibility-design.md`: public error envelopes and
  existing error categories remain stable. New diagnostics contain only
  gateway-owned structural facts and never contain raw SSE, prompts, responses,
  reasoning, tool content, provider messages, or secrets.
- `2026-07-29-portal-codex-recommendation-optimization-design.md`: generated
  Codex configuration keeps `stream_max_retries = 8`. This newer explicit
  product decision supersedes the earlier stability-test setting of zero.

The current Codex manual describes `stream_max_retries` as the retry count for
SSE streaming interruptions, with a default of five, and
`stream_idle_timeout_ms` as the SSE idle timeout, with a default of 300,000 ms.
The project intentionally generates eight retries and does not override the
client idle timeout. This work does not change either setting because there is
no timing evidence that a client timeout caused the internal incident.

## Scope

This design covers:

- typed gateway classification for downstream body drops;
- strict semantic-terminal validation for raw pass-through EOF;
- safe structural stream diagnostics in gateway tracing and usage-log errors;
- first usable output latency collection and persistence;
- Admin Logs and Portal Usage History latency display;
- backward-compatible file and PostgreSQL storage changes;
- regression coverage for Responses, Chat, translated streams, cancellation,
  terminal events, persistence, APIs, and frontend rendering.

## Non-Goals

- Inferring an exact internal-network root cause without a request capture.
- Suppressing genuine downstream body-drop records.
- Retrying or replaying a gateway stream after usable output was delivered.
- Changing Codex `stream_max_retries`, Codex idle timeout, gateway stream timeout,
  or request retry budgets.
- Adding provider-name, model-name, or internal-network special cases.
- Recording raw SSE frames or model-generated content for diagnosis.
- Changing dashboard aggregate latency metrics in this release.

## Considered Approaches

### 1. Protocol-correct completion, typed diagnostics, and first-output latency

Validate the semantic terminal lifecycle, classify interruption causes without message
parsing, emit safe structural diagnostics, and persist first-output latency.

This is the selected approach. It fixes a concrete success-classification gap,
keeps the no-replay safety boundary, and gives future internal incidents an
actionable category even when no request ID is supplied in advance.

### 2. Observability only

Rename the current 499 message and add first-output latency without changing EOF
validation. This is lower risk, but incomplete upstream streams can still be
recorded as successful and Codex can still retry without the gateway exposing
the primary protocol failure. It is rejected.

### 3. Codex retry or timeout tuning only

Reduce retries, increase idle timeout, or both. This may change how often the
symptom appears, but it does not repair invalid terminal handling and can hide
the distinction between protocol failure and transport interruption. It is
rejected without incident timing or a client trace.

## Selected Architecture

### Typed stream termination

Replace the Drop cleanup path's free-form message classification with an
internal typed termination value. The type carries only the facts needed for
classification and structured tracing:

- termination kind;
- stream phase (`prefetch`, `proxied`, `translated`, or `downstream_body_drop`);
- whether usable output was delivered;
- whether a semantic terminal was observed and which bounded terminal kind it
  was (`completed` or `incomplete`);
- upstream protocol and downstream endpoint from the existing diagnostic
  context;
- elapsed logical-request time.

The stable mappings are:

| Observed termination | Usage status | Error category |
| --- | ---: | --- |
| Downstream body dropped before usable output | 499 | `stream_client_cancelled` |
| Downstream body dropped after usable output but before completion | 499 | `stream_incomplete_close` |
| Upstream clean EOF without a required semantic terminal after stream commit | 502 | `stream_upstream_incomplete_eof` |
| Existing upstream read/decode failure | 502 | Existing category |
| Existing idle or maximum-duration timeout | 504 | Existing category |
| Proven `response.completed` terminal | 200 | None |
| Proven `response.incomplete` terminal | 200 | None |

The two 499 categories describe an observed response-body lifecycle, not human
intent. The partial-output message becomes the neutral static text:

```text
downstream response body dropped before semantic completion (partial output delivered)
```

The before-output variant uses the same wording without the parenthetical. Drop
cleanup passes the typed value directly to finalization. Error status/category
must no longer depend on searching message substrings.

499 outcomes continue to release all reservations, record one terminal usage
row, and avoid route-health penalties. They do not trigger route fallback or a
gateway retry.

### Semantic completion validation

Stream finalization must separate transport EOF from semantic completion.

For native Responses pass-through:

- `response.completed` proves successful semantic completion.
- `response.incomplete` proves an explicit, valid terminal outcome. It ends the
  current gateway stream lifecycle without a 499, but it is not promoted to
  `response.completed` and Codex remains free to start a new retry request.
- `response.failed` and error events retain their existing failure handling.
- `[DONE]` alone does not prove successful completion when
  neither `response.completed` nor `response.incomplete` was observed.
- Clean EOF after partial usable output without either valid terminal produces
  a typed `stream_upstream_incomplete_eof` failure frame, a 502 terminal usage
  record, and no gateway retry.
- Clean EOF before usable output remains eligible only for the existing bounded
  pre-commit recovery policy.

For Chat pass-through and Chat-to-Responses translation:

- An explicit valid Chat terminal reason remains semantic completion.
- The existing canonicalizer may synthesize the already-approved terminal for a
  clean EOF after unambiguous text, reasoning, or tool output.
- Role-only, usage-only, comment-only, and empty-delta EOF remain failures.
- Translation has reached a valid terminal lifecycle when it emits either
  downstream `response.completed` or `response.incomplete`; only the former is
  successful semantic completion.

If a valid terminal outcome is already proven, a later downstream drop during
queued terminal delivery finalizes that terminal outcome and does not become a
spurious 499.

### Error delivery after stream commit

When an incomplete upstream EOF is discovered before downstream semantic output,
the request can return an ordinary HTTP 502 or use the existing bounded
pre-commit recovery path.

When it is discovered after the stream is committed, response headers are
already sent. The gateway emits the existing endpoint-appropriate typed SSE
error frame, records terminal status 502 with
`stream_upstream_incomplete_eof`, releases resources, and ends the body. It does
not start another upstream attempt.

This converts a naked, ambiguous close into a protocol-visible failure while
preserving the no-replay rule.

### Safe diagnostics

Every abnormal stream termination emits one structured warning using bounded
gateway-owned fields:

- request ID;
- upstream and route ID;
- upstream protocol and downstream endpoint;
- stream phase;
- stable error category;
- usable-output-delivered boolean;
- semantic-terminal-observed boolean and bounded terminal kind;
- elapsed milliseconds and existing routing-attempt counters.

The usage row keeps the stable error category and a static error message. The
design does not add raw event names or payloads to persistent logs. Unknown or
provider-defined event text is never copied into diagnostics.

## Latency Contract

### Data model

Add the following field to `UsageLog`:

```rust
#[serde(default)]
pub first_token_latency_ms: Option<u64>,
```

`latency_ms` remains unchanged and continues to mean total logical-request
duration at terminal usage-log emission.

The first-token name is the external data contract requested by the UI. Its
measurement uses the gateway's existing usable-output classifier, so its precise
meaning is first usable semantic output rather than first network byte.

### Measurement

For a streaming logical request, measure from the same existing `started`
instant used by `latency_ms` until the first event for which
`stream_event_has_usable_output` is true. Eligible first output includes:

- text output;
- reasoning output;
- tool or function-call output.

The following do not set the value:

- HTTP response headers;
- gateway or upstream keepalive comments;
- empty SSE data;
- role-only or usage-only metadata;
- empty deltas;
- `[DONE]` or terminal-only events.

The value is set once, on the winning stream attempt, when prefetch classifies
the first usable event and commits it for downstream replay. Internal retries,
candidate fallback, and hedging remain part of the same logical timing origin.

Required null behavior:

- non-streaming request: `None`;
- failure, timeout, or cancellation before usable output: `None`;
- empty or terminal-only stream: `None`;
- failure or downstream drop after usable output: preserve the measured value.

For every populated row,
`first_token_latency_ms <= latency_ms` must hold naturally because both values
use the same monotonic timing origin and total latency is captured later. The
implementation must not clamp or fabricate a first-token value to hide a broken
measurement path.

### Persistence And API Compatibility

PostgreSQL adds a nullable column:

```sql
ALTER TABLE usage_logs
    ADD COLUMN IF NOT EXISTS first_token_latency_ms BIGINT NULL;
```

The create schema, insert statement, both usage-log SELECT lists, and the shared
row decoder must be updated together. Old rows naturally read as `None`.

File-backed JSON remains compatible because the Rust field uses
`#[serde(default)]`; older batches without the field deserialize to `None`.
Older binaries ignore the new JSON field. Admin and Portal APIs expose the new
flattened field as a number or `null` without changing their pagination or
filter contracts.

No existing aggregate, billing, quota, or dashboard calculation changes from
`latency_ms` to the new field.

## User Interface

Replace the current `耗时` column in both Admin Logs and Portal Usage History
with a `延迟` column containing two stable lines:

```text
首字    10.65s
总耗时  15.12s
```

Both values use seconds with exactly two decimal places. Missing
`first_token_latency_ms` renders as `首字 -`; total duration is always rendered.
The frontend shared `UsageLog` type accepts
`first_token_latency_ms?: number | null` so rolling deployment and old API
fixtures remain compatible.

The cell uses fixed labels and stable dimensions so the two lines do not change
table row height or overlap adjacent columns. The implementation should share
one duration formatter between the two pages. No explanatory copy or tooltip is
added to the main workflow.

## Data Flow

1. The gateway creates the existing logical-request `started` instant.
2. Routing, fallback, and optional hedging proceed under existing budgets.
3. Streaming prefetch ignores comments and metadata until it observes usable
   text, reasoning, or tool output.
4. The winning attempt records first-token elapsed time once and replays the
   buffered bytes unchanged.
5. Normal stream processing validates protocol terminals while forwarding
   incremental output.
6. `response.completed` finalizes success; `response.incomplete` finalizes an
   explicit incomplete outcome without a 499. An upstream EOF without either
   required terminal emits a typed in-stream failure. A downstream body drop
   before any terminal records the corresponding typed 499 lifecycle result.
7. Terminal usage logging stores first-token latency, total latency, status, and
   category in one logical-request row.
8. Admin and Portal APIs expose the same row, and both UIs render `首字` and
   `总耗时` consistently.

## Retry And Resource Semantics

- No gateway retry occurs after usable output is delivered.
- Existing pre-commit stream-to-JSON and route fallback budgets remain unchanged.
- Codex may independently retry an interrupted SSE according to its configured
  `stream_max_retries = 8`; the gateway does not rely on that retry for
  correctness.
- A Codex retry is a new downstream request. The gateway does not splice it into
  the prior usage row or attempt to deduplicate model/tool side effects.
- Downstream and upstream concurrency reservations are released exactly once on
  every terminal path.
- 499 downstream body drops do not mark the route unhealthy.
- 502 incomplete upstream EOF does mark the responsible route failure according
  to the existing route-health policy.
- One logical gateway request produces one terminal usage row regardless of
  internal pre-commit attempts.

## Test Strategy

Implementation follows red-green-refactor.

### Stream lifecycle tests

- Native Responses usable delta followed by clean EOF without
  `response.completed` emits a typed error, records
  `stream_upstream_incomplete_eof` with status 502, and does not hit a second
  upstream after commit.
- Native Responses `response.completed` followed by EOF remains success.
- Native or translated `response.incomplete` followed by downstream drop or EOF
  finalizes the explicit incomplete outcome and does not record a spurious 499
  or append a second failure event.
- A downstream drop before output remains `499 stream_client_cancelled`.
- A downstream drop after output remains `499 stream_incomplete_close`, uses the
  neutral message, does not penalize route health, and does not retry.
- A drop after `response.completed` remains success.
- Chat canonicalizer clean-EOF synthesis after usable text/tool output remains
  valid; metadata-only Chat EOF remains failure.
- Translated Responses streams apply the same downstream semantic-terminal
  boundary for both completed and incomplete outcomes.
- Status/category mapping tests construct typed termination values and prove no
  message-substring dependency remains.

### First-token latency tests

- Keepalive, comment, metadata, role-only, usage-only, empty-delta, and terminal
  events do not set first-token latency.
- Text, reasoning, and tool output each set it exactly once.
- The winning hedge attempt supplies the value; losing attempts do not overwrite
  it or create usage rows.
- Failure before output stores `None`; failure/drop after output preserves the
  measured value.
- Non-streaming requests store `None`.
- Every populated test row satisfies first-token latency less than or equal to
  total latency.

### Persistence And API tests

- Old file JSON without the field deserializes with `None`.
- PostgreSQL round trips both a numeric value and `NULL`.
- Admin Logs and Portal Usage History APIs expose numeric and null values.
- Existing old PostgreSQL rows and rolling schema initialization remain valid.

### Frontend tests

- Admin Logs renders `首字 10.65s` and `总耗时 15.12s` for sample values.
- Portal Usage History renders the same values.
- Missing first-token latency renders `首字 -` while total duration remains.
- The former raw `<milliseconds>ms` single-line duration display is absent.

### Regression verification

Run focused gateway stream suites, usage-log and PostgreSQL tests, frontend view
tests, full frontend build, all Rust targets, strict Clippy, and the existing
Codex/Chat/Responses compatibility suites. Verification must also retain byte
fidelity for normal SSE framing and prove that healthy streams still use one
upstream attempt.

## Deployment, Rollback, And Acceptance

The PostgreSQL change is additive and nullable. Deploying the new binary performs
the idempotent column addition. Rolling back to the previous binary is safe
because the extra column is ignored; no data rewrite is required.

Local acceptance uses deterministic mock streams because the affected internal
environment is unavailable:

- incomplete native Responses EOF is visible as the new 502 category instead of
  success;
- a valid `response.incomplete` terminal is not mislabeled as client
  disconnection when Codex retries;
- genuine downstream partial drops remain typed 499 records with neutral text;
- no partial-output gateway replay occurs;
- Admin and Portal display first-token and total latency with the requested
  formatting;
- all compatibility and persistence tests pass.

Internal-environment acceptance after deployment requires observing the next
Codex retry sequence. If the first request records
`stream_upstream_incomplete_eof`, the upstream terminal lifecycle is the cause.
If it records `stream_incomplete_close` while no preceding upstream failure is
present, the remaining boundary is Codex/network/task cancellation and requires
a packet or client trace. In either case, the gateway log no longer claims a
person cancelled the request.
