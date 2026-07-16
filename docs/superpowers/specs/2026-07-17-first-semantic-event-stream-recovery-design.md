# First Semantic Event Stream Recovery Design

Date: 2026-07-17

## Goal

Recover a streaming downstream request when an upstream accepts the HTTP request
but reports an error as its first semantic SSE event. The gateway will retry the
same route once without upstream streaming, then continue through the existing
candidate fallback policy if that JSON attempt also fails.

This behavior targets transient `upstream_stream_error_event` failures observed
with otherwise usable models. It must remain protocol-generic and preserve
Codex and OpenCode behavior.

## Scope

The recovery boundary is deliberately narrow:

- The upstream response has a successful HTTP status and an SSE content type.
- No non-comment semantic SSE event has been exposed downstream.
- The first semantic event is an upstream error, or the protocol parser rejects
  the stream before a normal first semantic event can be produced.
- Recovery uses the existing bounded `SsePassThrough` to `Json` transition.
- A normal first semantic event permanently commits the request to streaming.

The following are out of scope:

- Replaying a stream after any usable semantic output.
- Buffering the complete response before sending it downstream.
- Provider-name or model-name conditionals.
- Adding retry attempts beyond the current bounded stream-to-JSON transition.
- Changing public SSE event formats or downstream HTTP status semantics.

## Current Behavior

The streaming handler starts request processing in the background and emits SSE
comment keepalives when preparation takes longer than the short immediate-error
window. `send_to_upstream` currently returns a streaming body as soon as the
upstream sends successful HTTP headers. The response body is parsed only after
the routing loop has returned its `DispatchResult`.

Consequently, the routing loop can apply `should_retry_without_stream` to HTTP,
network, and aggregation failures that occur before body handoff, but it cannot
apply that policy to an error carried in the first SSE event. That later error is
correctly classified and emitted to the client, but the safe JSON recovery
opportunity has already been lost.

## Considered Approaches

### 1. Prefetch only the first semantic SSE event

Create a replayable upstream stream reader. Before handing an SSE pass-through
body to the downstream, read through comments and framing until the first data
event can be classified. Preserve every consumed byte and replay it unchanged if
the event is normal. Return the classified error to the routing loop if the first
semantic event is an error.

This is the selected approach. It enables a safe retry without buffering the
model output and does not add an extra upstream round trip to healthy streams.

### 2. Buffer the complete upstream stream

This would permit recovery after arbitrary late failures, but it would remove
real streaming, increase memory use, delay tool calls, and make cancellation less
effective. It is rejected.

### 3. Keep behavior unchanged and add observability only

This would improve diagnosis but leave known recoverable failures visible to
clients. It is rejected as the primary solution. Structural observability is
still included in the selected approach.

## Architecture

### Replayable upstream reader

Add a gateway-internal reader that owns:

- The remaining `reqwest::Response` body.
- A FIFO of raw chunks consumed during prefetch.
- The existing stream watchdog state used for idle and maximum-duration limits.

The reader exposes one chunk operation. It drains prefetched raw chunks first,
then reads from the network response. Existing proxied, translated, and
aggregated stream code uses this operation instead of calling
`reqwest::Response::chunk` directly.

The reader is the single watchdog owner from successful upstream HTTP headers
through terminal stream cleanup. Prefetch transfers the same reader into the
proxied or translated body; body construction must not create a new watchdog or
reset the idle or maximum-duration clocks. Each network chunk, including an
upstream comment, records activity exactly once when read. Replaying prefetched
bytes does not record activity again.

Raw bytes, frame delimiters, CRLF line endings, comments, and multi-line `data:`
fields are never reconstructed during replay. The bytes consumed by prefetch are
the bytes later delivered to the existing stream parser. Prefetch stores each
complete raw network chunk in the replay FIFO and scans a separate accumulated
view with a cursor. It waits when a frame delimiter is split across chunks. If a
chunk contains the first semantic frame plus later frames or a partial later
frame, every trailing byte remains in the FIFO and is replayed in its original
order.

### First semantic event classifier

Add a protocol-level helper that classifies exactly one complete upstream SSE
frame using the existing protocol parser and error semantics. It must:

- Ignore comment-only frames for the recovery decision.
- Recognize upstream error events without provider-specific string matching.
- Distinguish a normal semantic event from an upstream error.
- Preserve existing decode, limit, incomplete, idle-timeout, and max-duration
  categories when a usable first event cannot be produced.
- Stop after the first semantic event even when a network chunk contains several
  frames.

The helper is observational. It does not transform or consume the replay copy of
the frame.

### Routing integration

Only `SsePassThrough` responses with an SSE content type enter first-event
prefetch. Aggregated SSE already reports protocol failures to the routing loop,
and JSON responses retain their existing path.

When prefetch returns an upstream-classified server error, the existing
`should_retry_without_stream` branch changes the attempt mode to `Json` and
retries the same upstream and key once. A failed JSON attempt follows the normal
key and candidate fallback policy. A successful JSON response is synthesized
back into the downstream's requested SSE protocol.

Recovery eligibility follows the existing category policy:

| Category | Status | Recover as JSON before output |
| --- | ---: | --- |
| `upstream_stream_error_event` | 502 | Yes |
| `upstream_stream_decode_error`, `upstream_stream_limit_exceeded`, `upstream_stream_incomplete` | 502 | Yes |
| `stream_upstream_read_error`, `stream_upstream_body_decode_error` | 502 | No behavior change |
| `stream_upstream_timeout`, `stream_idle_timeout`, `stream_max_duration` | 504 | No |
| `stream_client_cancelled`, `stream_incomplete_close`, `stream_interrupted` | 499 | No |

The SSE-to-JSON transition budget is one per upstream key. If the JSON attempt
fails with an error that permits key rotation, the next key starts with a fresh
attempt mode selected from the downstream request and route capabilities. A new
upstream candidate does the same. The finite key and candidate sets therefore
bound all recovery attempts; the mode is never left as `Json` accidentally when
moving to a new key or candidate.

## Data Flow

1. The downstream sends a streaming Chat Completions or Responses request.
2. The gateway dispatches an SSE attempt to the selected upstream.
3. The upstream returns successful HTTP headers and an SSE body.
4. The replayable reader buffers raw chunks until the first semantic frame is
   classifiable. Gateway comment keepalives continue to keep the downstream
   connection active during this wait.
5. If the first semantic event is normal, the gateway commits to streaming and
   replays all prefetched bytes before reading the remaining body.
6. If the first semantic event is an error, the gateway returns the classified
   error to the routing loop before any model output is exposed.
7. The routing loop retries once with upstream streaming disabled.
8. Success is synthesized as downstream SSE. Failure continues through existing
   key and upstream candidate selection.

After step 5, any later failure remains an in-stream structured error. The
gateway never retries after normal semantic output because doing so could
duplicate text, reasoning, or tool calls.

Prefetch remains inside the request future already selected against downstream
channel closure; it must not run as a detached task. If the downstream closes
before the recovery decision, dropping that future cancels the in-flight body
read, the pre-header cancellation context records 499 and releases resources,
and no JSON retry starts. Cancellation winning a race with the first-event error
always takes precedence over recovery.

## Error And Resource Semantics

- A first-event upstream error retains `upstream_stream_error_event` and an
  internal 502 classification before recovery is attempted.
- Upstream read/decode failures remain 502 categories.
- Upstream idle and maximum-duration failures remain 504 categories.
- Downstream cancellation remains 499 and does not mark the upstream unhealthy.
- Every abandoned SSE attempt releases its upstream slot before the JSON retry.
- The downstream concurrency slot remains owned by the logical request across
  the bounded retry and is released exactly once.
- Usage history contains one terminal record for the logical request, not one
  record per internal attempt.
- Logs may record request ID, upstream ID, attempt modes, error category, attempt
  number, and whether recovery occurred. They must not record secrets, prompts,
  responses, reasoning, tool arguments, or tool results.

## Compatibility Constraints

- Codex Responses SSE ordering and terminal events remain unchanged.
- OpenCode Chat Completions SSE framing remains unchanged.
- `[DONE]`, CRLF framing, SSE comments, and multi-line `data:` fields remain
  byte-faithful on the normal pass-through path.
- A normal first event must not be duplicated or omitted.
- Tool-call deltas and reasoning deltas commit the stream just like text output.
- No upstream or model is removed solely because this recovery path is used.

## Test Strategy

Implementation follows red-green-refactor.

The initial failing integration test will configure a mock upstream that returns
an upstream error as the first SSE semantic event for `stream: true`, then
returns a successful JSON response for `stream: false`. The test will assert:

- The downstream receives a complete valid SSE response rather than an error.
- The upstream receives exactly two attempts in `true`, then `false` order.
- The request body remains semantically identical apart from stream mode.
- One successful usage record is written.
- Upstream and downstream concurrency counters return to zero.

Additional coverage will verify:

- A normal first event is replayed once with LF, CRLF, comments, split chunks,
  and multi-line data.
- Later frames and partial-frame bytes in the same prefetched network chunk are
  replayed once and in order.
- Idle and maximum-duration clocks include prefetch time and are not reset at
  body handoff.
- A normal first event followed by an error is not retried.
- A first-event failure followed by JSON failure advances through the existing
  candidate policy without an unbounded retry.
- Downstream cancellation during prefetch records 499, releases slots, aborts
  the body read, and starts no JSON retry.
- Responses-to-Responses and Chat-to-Responses client paths preserve their
  existing public event shapes.

Verification includes focused gateway tests, protocol tests, the full locked
offline Rust suite, rustfmt, Clippy with warnings denied, and substantive Codex
and OpenCode smoke tasks against the retained common-model set. Smoke tasks will
not use greeting-only probes.

## Deployment And Acceptance

Build the binary on the host, copy it into the existing container image, and
replace only the gateway container. PostgreSQL and Redis are not recreated. The
downstream test key fingerprint is checked before and after deployment.

Acceptance requires:

- The retained common models remain listed and routable.
- Codex and OpenCode complete text and read-only tool tasks.
- A controlled first-event mock failure recovers through JSON exactly once.
- No new 499 classification appears for upstream-originated failures.
- Recovered first-event failures do not increase upstream failure health counts.
- Unrecovered mid-stream failures remain visible with their existing 502
  category and enough structural metadata for diagnosis.
