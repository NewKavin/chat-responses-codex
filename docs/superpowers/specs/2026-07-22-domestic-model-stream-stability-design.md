# Domestic Model Stream Stability Design

Date: 2026-07-22

## Goal

Make Codex requests reliable when the gateway translates Responses requests to
Chat Completions upstreams such as GLM, MiniMax, and DeepSeek. Preserve the July
tool, reasoning, routing, and terminal-event behavior while accepting common
OpenAI-compatible stream deviations whose semantics are unambiguous.

## Evidence And Regression Boundary

- The portal has emitted `wire_api = "responses"` since June 11, including the
  known-good June deployment. It is not the regression trigger.
- All currently enabled upstreams are Chat Completions routes. Every Codex
  request therefore depends on Responses-to-Chat translation and Chat stream
  canonicalization.
- Commit `537d95d8` on July 16 introduced `ChatStreamCanonicalizer` and the
  public `upstream_stream_error_event` classification. The June path translated
  many sparse or non-standard chunks without enforcing the new invariants.
- The public error currently combines explicit provider failures and local
  canonicalization failures, while discarding the safe static internal reason.
- Current live GLM streams conform and succeed. The internal deployment can use
  a different compatibility layer, so stabilization must be shape-based rather
  than provider-name or model-name based.

## Selected Approach

Keep the canonicalizer and make it tolerant only where the missing or unstable
wire detail can be normalized without changing model semantics. Add structured,
server-only diagnostics before converting the internal protocol error to the
existing generic public error.

This is preferred over reverting to the June stream path because the old path
does not preserve the current Responses, tool, reasoning, usage, and terminal
lifecycle contracts. It is preferred over client retries because replaying a
complete Codex request can duplicate tools and other non-idempotent work.

## Compatibility Policy

### Normalize

The canonicalizer will normalize these cases:

1. A choice with `delta: null` becomes `delta: {}`. It does not count as text,
   reasoning, or tool output.
2. After upstream identity is locked, later non-null `id`, `model`, or `created`
   values may drift. The canonicalizer keeps and emits the first stable identity
   instead of failing the stream.
3. A clean upstream EOF without an explicit terminal reason may synthesize a
   terminal event only for choices that already emitted usable semantics:
   - text or reasoning output becomes `finish_reason: "stop"`;
   - a function or tool call becomes `finish_reason: "tool_calls"`.

The synthesized terminal uses the same stable identity and empty delta as the
existing `[DONE]` recovery path. Usage already observed before EOF is emitted
after the synthesized terminal.

### Continue To Reject

The gateway will continue to reject:

- explicit SSE `event: error`, `response.failed`, or non-null error envelopes;
- invalid JSON and non-object event envelopes;
- non-array `choices` and non-object choice entries;
- EOF or `[DONE]` after only role, usage, comments, or empty deltas;
- unknown, contradictory, or repeated terminal reasons;
- streams exceeding configured framing or duration limits.

These cases either carry an explicit failure or do not contain enough evidence
to infer successful completion.

## Error Handling And Diagnostics

The downstream response remains generic and stable:

- HTTP 502 before downstream semantic output, or a typed Responses failure
  after the stream is committed;
- code/category `upstream_stream_error_event`;
- message `upstream SSE stream reported failure`.

Before that public mapping, the gateway records one structured warning for an
`InvalidUpstreamStream` with only safe structural fields:

- request ID;
- selected upstream ID and protocol;
- downstream endpoint;
- phase (`canonicalize_push`, `canonicalize_finish`, `aggregate_push`, or
  `aggregate_finish`);
- error kind;
- static internal reason.

No prompt, response text, reasoning, tool arguments, tool results, provider
message, authorization value, API key, or raw SSE body may be logged. Provider
error type/code extraction is deferred until it can be represented as bounded,
sanitized metadata without widening the protocol error model for this release.

## Retry And Timeout Policy

- Keep portal `stream_max_retries = 0`. The gateway owns bounded route and
  stream-to-JSON recovery; Codex must not replay a terminal request.
- Do not change `request_max_retries`, response-header timeout, or stream idle
  timeout for this error category.
- A first-event failure may still use the existing one-time stream-to-JSON
  recovery before candidate fallback.
- No retry is allowed after usable text, reasoning, or tool output has reached
  the client.

## Test Strategy

Implementation follows red-green-refactor.

Protocol tests will first demonstrate that the current code rejects:

- null delta normalization;
- later identity drift;
- text output followed by clean EOF without a terminal reason;
- tool output followed by clean EOF without a terminal reason.

Negative tests will prove that role/usage-only EOF, unknown terminal reasons,
explicit upstream errors, and malformed JSON remain failures. A gateway
integration test will combine null delta, identity drift, text output, and clean
EOF and assert a successful canonical stream with one synthesized terminal and
`[DONE]`.

Logging tests will capture tracing output and assert request/upstream/phase/reason
fields are present while seeded prompt, tool, provider-message, and key markers
are absent.

## Live Acceptance Matrix

Run models serially with Codex `wire_api = "responses"` and
`stream_max_retries = 0`:

| Model | Required cases |
| --- | --- |
| `glm-5.1` | text, read-only tool, reasoning, approximately 20k input, terminal lifecycle |
| `glm-5.2` | text, read-only tool, reasoning, approximately 20k input, terminal lifecycle |
| `MiniMax-M2.7` | text, read-only tool, terminal lifecycle; reasoning is not required |
| `deepseek-v4-pro` | text, read-only tool, reasoning, approximately 20k input, terminal lifecycle |
| `deepseek-v4-flash` | text and terminal lifecycle as a non-blocking backup route |

Each required case must return HTTP 200, produce the expected text or parseable
tool call, and terminate with `response.completed` followed by `[DONE]`. It must
not contain `response.failed`, an error event, or a naked EOF. Large-input tests
must show non-trivial prompt usage and retain a deterministic marker at the end
of the input.

## Deployment And Rollback

Build and test a candidate image without recreating PostgreSQL or Redis. Replace
only the gateway container and preserve the current state volumes. Run the live
matrix against the candidate before promoting it to the internal deployment.

The rollback boundary is the previous gateway image. Database schemas and
persisted route data are unchanged by this work, so rollback requires only the
gateway image. The first historical comparison point for diagnosis remains
`17fc712c` versus `537d95d8`; neither historical image is the intended production
rollback target.

## Acceptance Criteria

- New protocol and integration regression tests pass.
- Existing explicit-error, malformed-stream, first-event recovery, tool,
  reasoning, and Responses lifecycle tests remain green.
- Rust formatting and Clippy with warnings denied pass.
- The relevant full Rust test suite passes offline.
- Required live model cases pass serially through a portal-equivalent Codex
  configuration.
- Logs identify future canonicalization failures structurally without leaking
  request or provider content.
- The repository documents the tested model matrix and exact candidate image.
