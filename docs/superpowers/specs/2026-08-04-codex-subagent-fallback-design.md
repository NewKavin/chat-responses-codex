# Codex Subagent Authentication And Fallback Design

## Context

Codex CLI 0.146.0 uses the Responses API for the main turn and can create
subagent threads when `features.multi_agent = true`. The portal-generated
configuration intentionally uses `requires_openai_auth = true`; every Codex
HTTP request, including a subagent request, must therefore have the downstream
Bearer credential installed with `codex login --with-api-key`.

The deployed gateway currently routes the configured models to Chat
Completions-only upstreams. It already flattens namespace and custom tool
definitions and restores function calls in responses, but a later Responses
request containing `custom_tool_call` or `custom_tool_call_output` is not fully
translated back to Chat tool messages. That can reject an otherwise valid
multi-agent, MCP, or freeform tool loop.

## Goals

1. Make authentication failures diagnosable without weakening downstream key
   enforcement or logging credentials.
2. Preserve Codex namespace and custom tool loops through the existing
   Responses-to-Chat fallback, including `previous_response_id` replay.
3. Preserve call IDs, tool order, namespace identity, and custom input bytes
   wherever the Chat wire format can represent them.
4. Keep unknown or semantically unrepresentable tool kinds as stable,
   capability-scoped errors instead of silently dropping them.
5. Verify both the pure converters and a real portal-configured `glm-5.2`
   Codex invocation.

## Non-goals

- Do not let the gateway infer or reuse a downstream key when a request lacks
  `Authorization`.
- Do not emulate hosted tools such as web search or computer use on a Chat
  upstream.
- Do not log prompts, tool arguments, upstream response bodies, API keys,
  authorization headers, or billing fields.
- Do not hard-code a model name or an upstream concurrency limit.

## Design

### Authentication contract

The portal configuration remains the source of truth:

```toml
requires_openai_auth = true
```

The generated login command remains the required setup step. The installed
client smoke path will use an isolated `CODEX_HOME`, explicitly run
`codex login --with-api-key`, and report only exit status, event type, and
bounded error category. A negative no-login check will assert that the result
is classified as authentication failure, so an operator can distinguish this
from protocol incompatibility.

The gateway continues to return ordinary `401` for missing or invalid
downstream credentials. No new credential-sharing endpoint or server-side
session is introduced.

### Responses-to-Chat tool continuation

`ToolAdapterRegistry` remains the single reversible mapping between the
downstream identity and the generated Chat function name. The request
converter receives the registry saved in `ResponseHistoryContext`:

- Responses `function_call` with a namespace maps to the registered flattened
  function name and becomes an assistant `tool_calls` entry.
- Responses `custom_tool_call` maps to a Chat function call using the registered
  name and arguments `{"input": <custom input>}`.
- Responses `function_call_output` remains a Chat `role: tool` message.
- Responses `custom_tool_call_output` is adapted to the same Chat tool message
  while retaining its call ID and output value.

When the current request includes a tool declaration, its deterministic
registry is used. When it is a continuation without declarations, the
registry deserialized from response history is reused. If no registry exists,
original function/custom names remain unchanged; no guessed namespace mapping
is created.

The Responses output converters and `ResponsesToChatState` accept the
representable custom call item and use the same registry. Unknown output item
types still return `ProtocolError::InvalidPayload` and are mapped to the
existing capability/invalid-response categories.

### Observability

Existing fallback downgrade metadata is extended only with bounded category
codes such as `unsupported_tools` or `tool_choice_dropped`. New code must not
include the tool name, namespace, input, or output in logs or error messages.

## Testing strategy

The implementation follows red-green-refactor:

1. Add protocol tests that currently fail for namespace/custom input and output
   replay, including multiple calls with stable IDs and order.
2. Add a Chat-only gateway two-turn test proving a custom call from the first
   response can be submitted as a custom output continuation.
3. Add Responses-to-Chat JSON/SSE tests for a representable custom output item
   and a rejection test for an unknown output type.
4. Run each focused test while red, implement the smallest converter/context
   changes, and rerun green.
5. Run the existing Rust protocol/gateway suites, clippy, and the portal
   frontend tests.
6. Run the real Codex smoke twice against `glm-5.2`: the logged-in portal
   configuration must emit `collab_tool_call` and `turn.completed`; the
   intentionally unauthenticated control must report only authentication
   failure and must not be mistaken for a protocol rejection.

## Rollout

No deployment-directory mutation occurs during implementation. After all
repository gates and the real smoke pass, build the image with the existing
package script, run the deployment health checks, then deploy while preserving
the existing credentials, volumes, Redis prefix, and ports.
