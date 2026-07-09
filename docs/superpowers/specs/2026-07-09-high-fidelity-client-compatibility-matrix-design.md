# High-Fidelity Client Compatibility Matrix Design

## Summary

Build a repeatable compatibility matrix for the `test` downstream that proves
every exposed model can be called through the three client families the user
cares about:

- Codex
- opencode
- Hermes

The first version should prioritize execution reliability over perfect protocol
fidelity, but only when the upstream forces a downgrade. The gateway should
preserve as much Responses semantics as possible on the first attempt, then
apply a staged fallback ladder for chat-only upstreams when those semantics are
rejected.

This design does not collapse model slugs into a canonical alias. If the
downstream currently exposes multiple slugs for the same model family, those
slugs remain separately visible and separately testable. Each slug must become
individually usable.

## Goals

- Make every model currently exposed to downstream `test` callable through:
  - Codex via `/v1/responses`
  - opencode via `/v1/chat/completions`
  - Hermes via the existing OpenAI-compatible chat path
- Preserve a higher-fidelity Responses-to-Chat fallback path for Codex whenever
  the selected upstream does not support native Responses.
- Keep standard `function` tools working whenever they can be represented on a
  chat-only upstream.
- Explicitly reject Responses-native built-in tools such as `web_search`,
  `file_search`, and `computer_use` when the selected upstream only supports
  Chat Completions.
- Add a repeatable compatibility matrix in the admin UI and a scriptable smoke
  path so regressions can be detected after future changes.

## Non-Goals

- Do not merge multiple model slugs into one canonical downstream model.
- Do not add a new upstream protocol beyond the existing `ChatCompletions` and
  `Responses` surfaces.
- Do not silently invent fake support for Responses-native built-in tools on
  chat-only upstreams.
- Do not redesign downstream auth, upstream management, or the model probe UI.
- Do not require provider-specific upstream configuration as a prerequisite for
  the first version.

## Current State

The current gateway already exposes all client-facing protocol surfaces needed
for the target clients:

- Codex uses `/v1/responses`
- opencode uses `/v1/chat/completions`
- Hermes uses the OpenAI-compatible chat path

The main gaps are in protocol-preserving fallback behavior rather than missing
routes.

Observed failure classes:

1. **Chat-only upstream fallback preserved too much Responses state**
   - Recent changes caused `Responses -> ChatCompletions` fallback to replay
     large `previous_response_id` histories, tool states, and tool outputs into
     chat-only upstreams.
   - This produced upstream rejections such as `CONTENT_LENGTH_EXCEEDS_THRESHOLD`
     and `TOOL_CONFIG_MISSING`, even though a small chat-only request to the
     same upstream would succeed.

2. **Third-party chat proxies reject some OpenAI/Responses extension fields**
   - At least one recent regression path involved `parallel_tool_calls`
     surviving long enough to trigger upstream 400s in chat-only proxies.

3. **Different slugs for the same family route to different upstreams**
   - Example: one active upstream exposes `deepseek-v4-flash`, while another
     exposes `deepseek-ai/deepseek-v4-flash`.
   - The first version must preserve both slugs instead of collapsing them, so
     compatibility must be validated per slug.

4. **Some upstreams are healthy for chat but unstable or semantically broken**
   - `claude-haiku-4-5-20251001` on one current chat-only upstream sometimes
     returns a syntactically valid success envelope with empty content and zero
     tokens.
   - That is an upstream behavior problem, but the gateway must still classify
     it clearly and avoid conflating it with protocol-conversion failures.

## Design Principles

1. **Preserve semantics first, degrade only on evidence**
   - The first attempt should retain as much Responses meaning as safely
     possible.
   - Fallback degradation must be driven by concrete upstream rejection signals,
     not by default pessimism.

2. **Client compatibility is judged at the gateway boundary**
   - The matrix should run real requests against the same `/v1/*` endpoints and
     downstream auth path used by live clients.
   - It should not bypass routing, quota checks, or stream handling.

3. **Per-slug compatibility, not per-family abstraction**
   - If `deepseek-v4-flash` works and `deepseek-ai/deepseek-v4-flash` does not,
     the matrix must report exactly that.
   - The system must not hide that difference by silently rewriting one slug to
     another for the first version.

4. **Unsupported built-in tool semantics must fail explicitly**
   - `web_search`, `file_search`, and `computer_use` cannot be faithfully
     mapped to chat-only upstreams.
   - The gateway should say so directly instead of silently stripping them.

## Client Matrix Scope

### Codex

Codex must be tested through `/v1/responses`.

Required checks:

- Basic non-stream request
- Basic stream request
- Standard `function` tool request
- Long history request
- `previous_response_id` continuation
- Stream continuation / replay scenario

Codex-only note:

- `previous_response_id` is specific to Responses semantics and should be
  tested only for Codex.

### opencode

opencode must be tested through `/v1/chat/completions`.

Required checks:

- Basic non-stream request
- Basic stream request
- Standard `function` tool request
- Long multi-turn history request
- Chat-equivalent replay behavior where applicable

### Hermes

Hermes should be treated as an OpenAI-compatible chat client for the first
version, following the existing `scripts/hermes.sh` path.

Required checks:

- Basic non-stream request
- Basic stream request
- Standard `function` tool request
- Long multi-turn history request

## Fallback Ladder

The fallback ladder applies only when:

- the downstream request is `Responses`
- the selected model has no active `Responses` upstream candidate
- the selected upstream path is therefore `ChatCompletions`

### Stage 0: High-Fidelity Attempt

Send the best available Responses-to-Chat translation, keeping:

- current-round user input
- `instructions` / system prompt
- standard `function` tools
- `tool_choice` when representable
- `reasoning_effort` when compatible
- `response_format` / JSON schema when representable
- `stream_options` fields that are known-safe
- replayed history and `previous_response_id` state when present

### Stage 1: Extension Cleanup

If the upstream rejects the request with a protocol/field-shaped 4xx, remove
high-risk extension fields first:

- `parallel_tool_calls`
- `stream_options.include_obfuscation`
- other Responses/OpenAI extension fields already classified as unsafe for
  third-party chat proxies

### Stage 2: Tool Replay Reduction

If the upstream still rejects the request with a tool- or schema-shaped 4xx,
keep standard `function` tools for the current turn when possible, but remove:

- replayed tool outputs
- replayed tool call state
- chat messages with `tool_call_id`
- replayed assistant tool-call blocks derived from `previous_response_id`

This stage should still preserve the current-turn user request and any safe
system prompt or prior plain-text conversation history.

### Stage 3: History Compaction

If the upstream still rejects the request with context-length or payload-size
4xx, compress the fallback payload down to the closest chat-equivalent request
that preserves execution:

- keep system/instructions if present
- keep only safe, plain-text user/assistant history needed for continuity
- drop `previous_response_id`
- drop replay-only Responses state

### Stage 4: Explicit Failure

If the upstream still rejects the request:

- return the real classified gateway failure
- record the fallback stage reached
- show the exact compatibility layer that failed in the matrix output

### Repeated Failure Memory

For each tuple of:

- downstream id
- client family
- model slug
- upstream id
- fallback stage

keep an in-memory “high-fidelity rejection” counter.

Policy:

- After the first failure, keep trying the higher-fidelity stage on later
  identical calls.
- Retry high-fidelity attempts up to 3 total identical failures.
- Once the same tuple has failed 3 times, later identical calls should skip the
  proven-bad higher-fidelity stage and start directly from the next lower stage.
- A successful request at any stage resets the failure memory for the skipped
  higher-fidelity path.

This keeps the system exploratory at first, but prevents repeatedly paying the
same protocol failure cost forever.

## Unsupported Built-In Tools

When the selected path falls back to chat-only upstreams:

- `function` tools remain supported when representable.
- `web_search`, `file_search`, and `computer_use` must return an explicit
  compatibility failure.

The error should explain:

- that the current model/upstream path only supports Chat Completions
- that the requested built-in Responses tool cannot be faithfully mapped
- that the user must choose another model/upstream or drop the built-in tool

## Diagnostic Entry Points

### Admin Matrix

Add an admin troubleshooting/compatibility matrix entry point that can:

- choose a downstream (defaulting to `test` for the initial use case)
- choose one or more client families
- run the compatibility matrix over all models currently exposed by that
  downstream

For each model/client combination, show:

- endpoint used
- upstream selected
- protocol path taken (`native` vs `responses_to_chat`)
- fallback stage reached
- final status (`passed`, `warning`, `failed`)
- gateway error category when failed
- concise summary and next action

### Scripted Smoke

Add a scriptable smoke entry point in the repo that:

- resolves the target downstream key from the local deployment
- fetches the live model list for that downstream
- runs the configured matrix for `codex`, `opencode`, and `hermes`
- emits machine-readable results plus a human-readable summary

This script is the regression harness for future changes and should be usable
without the admin UI.

## Result Model

Each matrix result should include:

- `client_family`
- `model_slug`
- `endpoint`
- `selected_upstream_id`
- `selected_upstream_name`
- `selected_upstream_protocol`
- `protocol_transition`
- `fallback_stage`
- `status`
- `http_status`
- `gateway_error_category`
- `summary`
- `details`
- `duration_ms`

Optional fields for debugging:

- `safe_payload_metrics`
  - message count
  - tool count
  - whether reasoning_effort was present
  - whether stream options were present
- `skipped_stages`
  - stages skipped because they previously failed 3 times for the same tuple

## Verification Plan

Required automated checks:

- `protocol.rs` unit tests for the new fallback-safe transformations
- `gateway` integration tests for:
  - high-fidelity fallback success
  - staged fallback after protocol-shaped 4xx
  - repeated failure memory skipping high-fidelity after 3 failures
  - explicit built-in tool failure on chat-only upstreams
  - per-slug matrix behavior without canonical slug collapse

Required runtime validation:

- `test` downstream matrix against all live model slugs
- spot-check at least:
  - one chat-only Claude-family slug
  - one chat-only DeepSeek-family slug
  - one model already known to work through Codex fallback

## Risks

- Preserving more Responses semantics for chat-only upstreams increases request
  complexity and can reintroduce 4xx payload failures if the fallback ladder is
  too permissive.
- Some upstreams may fluctuate between empty-success, 4xx, and 5xx behaviors;
  the matrix must distinguish provider instability from protocol incompatibility.
- Keeping multiple slugs visible means operators may still need to understand
  that similarly named models route differently.

## Open Questions

None for this version. The approved direction is:

- preserve separate slugs
- preserve high-fidelity semantics as far as feasible
- prioritize execution when forced to choose
- explicitly reject built-in Responses tools on chat-only upstreams
