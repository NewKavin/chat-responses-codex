# Claude Real Streaming Design

## Goal

Make `/v1/messages` use real upstream streaming instead of collecting a full chat response and synthesizing Anthropic SSE in the gateway.

## Scope

This design only covers downstream Claude-compatible `/v1/messages` requests with `"stream": true`.

In scope:
- `ChatCompletions` upstream -> `/v1/messages` real SSE adaptation
- Gateway-side translation from chat chunk SSE events to Anthropic Messages SSE events
- Preserving existing non-stream `/v1/messages` behavior
- Preserving existing request translation from Claude request JSON to chat payload JSON

Out of scope:
- New upstream routing policy
- New downstream endpoint shapes
- Reworking `/v1/responses`
- Reworking Claude JSON non-stream output
- Native `Responses` upstream -> `/v1/messages` direct adaptation beyond existing chat-stream-compatible dispatch results

## Problem

Current `/v1/messages` streaming is not real streaming. The gateway converts Claude input to a chat payload, deliberately avoids forwarding `stream: true`, waits for a full chat response, converts that full response into a Claude-style message, then emits synthetic SSE events from the final body.

That behavior has three concrete problems:

1. Downstream clients do not receive token-by-token or tool-argument incremental events.
2. Upstream streaming failure/latency semantics are hidden behind gateway synthesis.
3. The code explicitly rejects streamed dispatch results for Claude compatibility, so the transport cannot evolve incrementally.

## Existing Code Paths

- `claude_messages()` in `src/server/gateway.rs` translates Claude request JSON into a chat payload and dispatches it through `process_gateway_request()`.
- `claude_messages_to_chat_payload()` currently omits the downstream `stream` flag on purpose.
- `dispatch_claude_success()` only accepts `DispatchBody::Json` for `/v1/messages`; `DispatchBody::Stream` returns an error.
- `claude_message_to_sse_body()` synthesizes Anthropic SSE from a completed Claude message JSON body.
- Existing stream translation infrastructure already exists for:
  - `ChatCompletions` <-> `Responses`
  - streaming lifecycle, usage logging, timeout handling, and drop cleanup

## Chosen Approach

Add a dedicated streaming adapter for `/v1/messages` that consumes chat-completions SSE chunks and emits Anthropic Messages SSE events in real time.

This adapter will live in the gateway layer, not in the generic `StreamTranslator`, because:
- Anthropic `/v1/messages` is a downstream compatibility surface, not a peer upstream protocol already modeled in routing.
- The smallest safe change is to reuse the existing dispatch/routing machinery and only adapt the streamed body at Claude response dispatch time.
- We can support both native chat upstream streaming and any existing gateway-produced chat chunk stream without changing upstream selection.

## Alternatives Considered

### 1. Extend `StreamTranslator` with a Claude target protocol

Pros:
- Centralized translation abstraction
- Cleaner protocol taxonomy long-term

Cons:
- Requires promoting `/v1/messages` into the routing/translation protocol model
- Larger refactor for this repo’s current structure
- Higher regression risk for an otherwise narrow compatibility fix

Decision:
- Rejected for this change set

### 2. Prefer `Responses` upstream for Claude streaming and translate from there

Pros:
- Could align more directly with richer response-event semantics

Cons:
- Changes upstream selection behavior
- Adds uncertainty around tool-call semantics and provider support
- Moves multiple variables at once

Decision:
- Rejected for this change set

## Architecture

### Request path

`claude_messages_to_chat_payload()` will preserve `"stream": true` when the downstream request asks for streaming.

The request will continue to dispatch through the existing `process_gateway_request(..., EndpointKind::ChatCompletions)` path, so routing, auth, usage, retry, and timeout behavior stay centralized.

### Response path

`dispatch_claude_success()` will gain streamed-body support:

- `DispatchBody::Json` stays on the current non-stream conversion path
- `DispatchBody::Stream` will be adapted to Anthropic SSE when the downstream request asked for streaming

The streamed adapter will:
- parse upstream SSE frames
- consume `chat.completion.chunk` JSON events
- emit Anthropic named SSE events immediately
- treat upstream `[DONE]` as transport completion, not as a downstream emitted frame

### Streaming adapter state

Add a dedicated state machine in `src/server/gateway.rs` for Claude stream adaptation. It needs to track:

- `message_id`
- `model`
- `created_at`
- whether `message_start` has been emitted
- whether the assistant text block has started
- accumulated text block content state
- tool call state by index:
  - `id`
  - `name`
  - accumulated arguments
  - whether `content_block_start` has been emitted
- latest usage counters
- whether semantic completion has been observed
- lifecycle/logging cleanup context

### Event mapping

Input:
- chat SSE frames containing `chat.completion.chunk`
- upstream trailing `usage`
- upstream `[DONE]`

Output:
- `event: message_start`
- `event: content_block_start`
- `event: content_block_delta`
- `event: content_block_stop`
- `event: message_delta`
- `event: message_stop`

Mapping rules:

- First assistant role/content/tool signal emits `message_start`
- First text content opens a `text` content block
- Each text delta becomes `content_block_delta` with `text_delta`
- First tool-call fragment for a given tool index opens a `tool_use` content block
- Tool-call argument deltas become `content_block_delta` with `input_json_delta`
- Finish reason `tool_calls` maps to Claude `stop_reason: "tool_use"`
- Finish reason `stop` maps to Claude `stop_reason: "end_turn"`
- On semantic completion, any open blocks are closed before `message_delta`
- `message_delta` emits final stop reason and latest output token usage
- `message_stop` ends the downstream stream

The gateway will not emit `data: [DONE]` downstream for `/v1/messages`, because Anthropic Messages streaming uses named SSE events rather than OpenAI’s sentinel frame.

## Error Handling

- Invalid upstream SSE JSON stays an upstream read/decode failure and should continue using existing stream error cleanup.
- Unknown chat chunk payloads should be ignored if they do not carry actionable fields.
- If the stream completes without a terminal finish reason but does reach `[DONE]`, the adapter should finalize conservatively with `end_turn`.
- If the client disconnects after semantic completion but before upstream `[DONE]`, the drop path should count the stream as success, matching the existing Responses semantic-completion handling.

## Testing Strategy

### Update existing tests

In `tests/gateway/claude.rs`:
- existing stream tests should assert that the upstream chat request now includes `"stream": true`
- existing assertions that the gateway avoided upstream streaming should be removed or inverted

### Add new tests

Add coverage for:
- text streaming emits incremental Anthropic text events from chat chunk deltas
- tool-call streaming emits incremental `tool_use` block events from chat tool-call deltas
- no downstream `data: [DONE]` frame is emitted
- upstream stream completion is logged as success
- downstream disconnect after semantic completion is logged as success

### Regression boundaries

Non-stream `/v1/messages` JSON behavior must remain unchanged.

## Risks

The main risk is event-order mismatch between chat chunk semantics and Anthropic Messages SSE expectations. The implementation should stay conservative:
- one assistant text block unless future evidence requires multiple
- tool blocks keyed by chunk tool-call index
- explicit block close before message close

Another risk is mixing Claude downstream transport logic into a large gateway file. For this change, that is acceptable because it minimizes moving parts. If this area grows further, extraction into a focused module would be warranted later.

## Acceptance Criteria

- `/v1/messages` with `"stream": true` forwards a streamed upstream chat request
- downstream receives real-time Anthropic SSE events derived from chat chunks
- gateway no longer fabricates Claude stream output from a completed JSON response for the streaming path
- existing non-stream `/v1/messages` behavior remains green
