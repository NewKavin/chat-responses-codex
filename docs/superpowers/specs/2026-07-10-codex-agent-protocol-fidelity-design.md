# Codex Agent Protocol Fidelity Design

## Summary

`chat-responses-codex` must let Codex, OpenCode, Claude Code, and Hermes use
one gateway URL, one downstream key, and an exposed model slug without knowing
which provider or wire protocol serves that model. The gateway owns protocol
adaptation and model-specific request normalization.

The primary deployment target is a third-party hosted or self-deployed API, not
the model vendor's official endpoint. The gateway must preserve the Codex agent
loop when an OpenAI Responses request is routed to an upstream that implements
only Chat Completions, implements a restricted subset of Responses, or differs
from the vendor's documented request dialect. The initial acceptance targets
are the exact configured slugs `glm-5.2`, `deepseek-v4-flash`,
`MiniMax/MiniMax-M2.5`, `MiniMax/MiniMax-M2.7`,
`moonshotai/Kimi-K2.5`, and `moonshotai/kimi-k2.6`.

The existing gateway is a usable compatibility beta, not yet a high-fidelity
agent gateway. Basic routing, text streaming, standard function calls, and
pairwise conversion are established. Codex namespace tools, reasoning replay
through tool loops, truthful capability advertisement, semantic matrix checks,
and measured converter latency are not yet production-complete.

This design keeps the existing pairwise converters. It adds a capability
resolver, reversible tool adapters, reasoning continuity, semantic diagnostics,
truthful client presets, and evidence-based model profiles. It does not add a
large universal intermediate representation.

## Evidence Standard

Protocol behavior is not inferred from field names. Three evidence lanes are
kept separate:

1. Downstream client contracts come from exact client source and sanitized
   captures from the installed version.
2. Model semantics come from the model vendor's current official API reference
   or OpenAPI document.
3. Upstream wire syntax comes from a repeatable live probe against the exact
   configured third-party upstream, model slug, and protocol. For wire syntax,
   this observed result overrides vendor documentation.

A feature beyond the conservative common subset is treated as unsupported until
it has positive evidence for the exact route. Live probes run in diagnostics,
background setup, or acceptance workflows, never on the normal request path.
Source URLs, client versions, upstream IDs, exact slugs, probe versions, and
timestamps are retained so a later client, relay, or model release cannot
silently redefine an existing profile.

### Verified client contracts

The design was checked on 2026-07-10 against:

- Codex CLI `0.144.0`, source tag `rust-v0.144.0`, commit
  `767822446c7a594caa19609ca435281a9ec67e0d`.
- OpenCode `1.17.9`, source tag `v1.17.9`, commit
  `5c23e88419c4743b9be42cea132f2fb1e6cb63ff`.
- Claude Code CLI `2.1.195`; the public repository does not contain the core
  client implementation, so its wire behavior was verified by sanitized local
  capture and the official Anthropic Messages reference. Public repository tag
  `v2.1.205`, commit `be02c39841a59e2ac1f35ac12285def02acdbb5a`,
  was used for custom-gateway release notes.
- Hermes Agent `0.14.0`, installed source commit
  `43e566f77eaf01293086eb7cb99a21e240d60634`.

Relevant authoritative sources:

- [OpenAI Codex source](https://github.com/openai/codex/tree/rust-v0.144.0)
- [OpenAI Responses reference](https://developers.openai.com/api/reference/resources/responses/)
- [Anthropic Messages reference](https://platform.claude.com/docs/en/api/messages)
- [Anthropic streaming events](https://platform.claude.com/docs/en/build-with-claude/streaming)
- [Anthropic tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools)
- [OpenCode source](https://github.com/anomalyco/opencode/tree/v1.17.9)
- [Claude Code public repository](https://github.com/anthropics/claude-code)
- [Hermes Agent source](https://github.com/NousResearch/hermes-agent)

Verified model-provider sources:

- [DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode),
  [tool calls](https://api-docs.deepseek.com/guides/tool_calls), and
  [Anthropic compatibility](https://api-docs.deepseek.com/guides/anthropic_api)
- [GLM 5.2](https://docs.bigmodel.cn/cn/guide/models/text/glm-5.2),
  [thinking mode](https://docs.bigmodel.cn/cn/guide/capabilities/thinking-mode),
  [function calling](https://docs.bigmodel.cn/cn/guide/capabilities/function-calling),
  and [GLM 5.2 migration](https://docs.bigmodel.cn/cn/guide/start/migrate-to-glm-new)
- [MiniMax Responses](https://platform.minimax.io/docs/api-reference/responses-create),
  [Chat Completions](https://platform.minimax.io/docs/api-reference/text-chat-openai),
  [Messages](https://platform.minimax.io/docs/api-reference/text-chat-anthropic),
  and [Codex setup](https://platform.minimax.io/docs/token-plan/codex)
- [Kimi Chat Completions](https://platform.kimi.ai/docs/api/chat),
  [Kimi K2.6](https://platform.kimi.ai/docs/guide/kimi-k2-6-quickstart),
  [thinking models](https://platform.kimi.ai/docs/guide/use-kimi-k2-thinking-model),
  and [Codex integration](https://platform.kimi.ai/docs/guide/codex-kimi)

### Verified Codex behavior

A real Codex `0.144.0` request contained eight top-level function tools, two
namespace tools, and one hosted `web_search` tool. The namespaces were
`multi_agent_v1` and `mcp__openaiDeveloperDocs`. Codex source confirms:

- `exec_command`, `write_stdin`, and the legacy `shell_command` are ordinary
  function tools selected by the model catalog and feature configuration.
- `apply_patch` is a custom/freeform tool only when
  `apply_patch_tool_type = "freeform"`.
- Namespace members are returned as Responses `function_call` items with the
  original member in `name` and the namespace in the optional `namespace`
  field. Codex routes using both fields.
- There is no basis for inventing a Responses `local_shell` adapter for the
  current Codex tool set.
- Generic custom providers enable namespace and hosted-search capabilities at
  the provider layer. Model metadata alone is not sufficient to suppress
  hosted web search.

A controlled Codex tool-loop capture returned a standard Responses reasoning
item followed by a function call. Codex included the reasoning item, function
call, and function-call output in its next request, preserving the reasoning
text. This validates standard Responses reasoning items as the reversible
carrier for chat-provider `reasoning_content`.

### Verified Claude Code behavior

An installed Claude Code `2.1.195` request to a custom base URL used
`POST /v1/messages?beta=true`, `anthropic-version: 2023-06-01`, and an SSE
response. In bare mode it sent the `Bash`, `Edit`, and `Read` client tools plus:

- `thinking: {"type":"adaptive"}`
- `output_config: {"effort":"high"}`
- `context_management.edits`
- `max_tokens: 32000`
- a system content array and user content blocks

A controlled `Read` round trip confirmed that Claude Code replays the assistant
`tool_use` block and a user `tool_result` block linked by the same ID. The
required stream order is `message_start`, content block events, `message_delta`,
and `message_stop`. `ping` and in-stream `error` events are legal.

### Verified OpenCode and Hermes behavior

OpenCode custom providers default to `@ai-sdk/openai-compatible` and use Chat
Completions. Installed OpenCode `1.17.9` sent `stream: true`,
`stream_options.include_usage: true`, standard function tools, and standard
assistant/tool continuation messages.

Installed Hermes `0.14.0` uses the OpenAI SDK Chat Completions stream, requests
`stream_options.include_usage`, incrementally assembles indexed tool-call
arguments, and replays assistant `tool_calls` followed by `role: "tool"`
messages with `tool_call_id`.

## Current Maturity

| Area | Current level | Evidence and gap |
| --- | --- | --- |
| Routing and authentication | Established | Multiple upstream protocols, quotas, retries, and error categories exist. |
| Text JSON and SSE conversion | Established | Automated coverage and live text requests pass. |
| Standard function calls | Partial | Basic calls convert, but the matrix does not require an actual tool call and reasoning replay is missing. |
| Codex namespace/custom tools | Incomplete | Chat fallback currently strips namespace and custom tools. |
| Reasoning agent loops | Incomplete | Chat `reasoning_content` is not represented in Responses history. DeepSeek and Kimi document a 400 error when it is omitted after a thinking tool call. |
| Claude Code | Partial | The Messages adapter supports tools and thinking, but Claude Code is absent from the default batch matrix. |
| Capability metadata | Incomplete | The Codex catalog advertises optimistic fixed capabilities for every model. |
| Diagnostics | Partial | The current matrix treats HTTP 200 plus non-empty output as success and always reports a null fallback stage. |
| Performance | Unverified | Streaming is incremental, but gateway-added first meaningful byte latency has no acceptance measurement. |

The baseline workspace suite passes 466 tests with one ignored load test. A
live matrix run across five exposed models and Codex, OpenCode, and Hermes
reported 14 passes and one transient upstream authentication failure. This is
route-level evidence only; it is not proof of agent-loop fidelity.

## Goals

1. Preserve Codex instructions, text streaming, function tools, namespace
   tools, supported custom tools, tool outputs, reasoning continuity, and
   multi-turn replay across Responses-to-Chat and restricted-Responses routes.
2. Normalize the six exact target slugs according to their verified model
   semantics and the observed dialect of each configured third-party upstream.
3. Keep client configuration provider-agnostic: gateway URL, downstream key,
   and exposed model slug are sufficient.
4. Prefer a configured native Responses route when its declared capability
   subset can carry the request; otherwise adapt once to Chat Completions.
5. Make every advertised capability executable. Optional unsupported features
   may downgrade with diagnostics; required unsupported features fail before
   dispatch.
6. Validate Codex, OpenCode, Claude Code, and Hermes with protocol-semantic
   checks and installed-client smoke tests.
7. Keep healthy requests single-attempt and streaming, with less than 50 ms P95
   gateway-added first meaningful byte latency against a local mock upstream.

## Non-Goals

- Emulating provider-hosted web search, file search, computer use, or code
  execution when the selected upstream has no equivalent service.
- Exposing raw prompts, tool arguments, or credentials in compatibility logs.
- Hiding upstream authentication, quota, availability, or model-quality
  failures as converter success.
- Runtime capability probes on ordinary requests.
- Assuming a third-party API accepts vendor-specific fields because it serves a
  vendor model or uses an OpenAI-compatible path.
- Adding an Anthropic Messages upstream protocol in this iteration. The
  downstream Messages surface continues to use the existing Chat adapter.
- Replacing routing, quota, persistence, or all pairwise converters with a
  universal intermediate representation.

## Design Principles

### Preserve, adapt, downgrade, or reject

Every client feature has one explicit outcome:

1. Preserve it unchanged when the target protocol supports it.
2. Adapt it reversibly when both sides can express the same behavior.
3. Downgrade it only when it is optional, with a response diagnostic and usage
   log record.
4. Reject it before dispatch when it is required and no faithful mapping exists.

Silent removal is not an acceptable outcome.

### Model semantics and wire syntax are separate

An explicit exact-slug semantic registry determines the behavior the gateway
must try to preserve. The selected upstream, exact routed slug, and target
protocol determine the wire fields that can express it. For example, MiniMax
M2.x reasoning cannot be disabled, while one relay may accept
`reasoning_split` and another relay serving the same slug may reject it. The
resolver combines both facts before building the upstream payload. Vendor
documentation is never used as proof that a third-party endpoint accepts a
field.

### Native means capability-compatible, not endpoint-compatible

An upstream accepting `/v1/responses` does not imply full OpenAI Responses
support. MiniMax documents function tools but not namespace, custom, or hosted
tools. The gateway adapts unsupported Responses subsets before dispatch instead
of blindly passing them through.

## Architecture

### Two-layer capability model

Add two independent profile types in the gateway compatibility module:

1. `ModelSemanticProfile` is selected from an explicit registry entry for the
   exposed slug. It contains semantic ceilings and invariants such as context
   limit, whether reasoning can be disabled, meaningful effort levels, fixed
   sampling behavior, and whether a thinking tool loop requires reasoning
   replay.
2. `UpstreamDialectProfile` records what one configured route actually accepts
   and emits. Its identity is the tuple `(upstream_id, exact_model_slug,
   protocol)`, where `exact_model_slug` is the final slug sent after route
   resolution. A profile from one upstream, slug spelling, or protocol is never
   reused for another.

No hostname or provider-name classifier participates in wire decisions. In
particular, an OpenAI-looking base URL, an `/v1` suffix, or an
"OpenAI-compatible" label does not prove support for any optional field.

The request-time capability resolver receives:

- the downstream endpoint and requested feature set
- the exact exposed slug and its semantic profile
- the selected upstream ID, final exact slug, and protocol
- the persisted dialect profile or the conservative unprobed profile
- `strip_nonstandard_chat_fields`

It returns an immutable request profile containing the selected token-limit
field, reasoning controls and replay carrier, omitted sampling fields, tool and
streaming capabilities, permitted provider extensions, adapter set, catalog
capabilities, and downgrade policy. Lookup is bounded in-memory work and never
performs network I/O.

`strip_nonstandard_chat_fields` remains a hard administrator override. When it
is true, the resolver intersects even a verified dialect with the conservative
Chat subset and suppresses reasoning controls, `parallel_tool_calls`, GLM
`tool_stream`, MiniMax `reasoning_split`, and other optional extensions. A probe
can never turn those fields back on while the override is active. The override
does not delete a `reasoning_content` value already emitted by the model or its
required continuation replay; those are protocol state, not optional controls.

### Exact initial semantic registry

The first registry contains these entries; substring matching is not used:

| Exact exposed slug | Verified semantic behavior | Gateway preservation policy |
| --- | --- | --- |
| `deepseek-v4-flash` | DeepSeek V4: 1M context, 384K maximum output, function tools, thinking on by default, meaningful efforts `high`/`max`, and exact reasoning replay after thinking tool calls. | Map Codex `low`/`medium` to semantic `high` and `xhigh` to `max`; preserve reasoning bytes on every tool sub-turn; omit ineffective thinking-mode sampling controls. The dialect profile chooses the actual token and effort fields. |
| `glm-5.2` | GLM 5.2: 1M context, 128K maximum output, function tools, thinking on by default, efforts `high`/`max`, optional streamed tool arguments, and preserved thinking for coding agents. | Map lower Codex levels to semantic `high` and `xhigh` to `max`; preserve reasoning exactly. Emit `tool_stream` only when this exact route accepted it during probing. |
| `MiniMax/MiniMax-M2.5` and `MiniMax/MiniMax-M2.7` | MiniMax M2.x supports function tools and cannot disable reasoning. Official interfaces include Chat, restricted Responses, and Messages, with up to 200K output in the current reference. | Expose a fixed reasoning mode and never claim an off switch. Select Responses or Chat only from route evidence; emit separated reasoning only when the route accepted it. |
| `moonshotai/Kimi-K2.5` and `moonshotai/kimi-k2.6` | Kimi K2.5/K2.6 have 256K context, function and parallel tools, default-on but disableable thinking, fixed sampling parameters, and required reasoning replay in thinking tool loops. | Map Codex `none` to semantic thinking-off and other levels to thinking-on, omit fixed sampling fields, and replay reasoning on tool sub-turns. The dialect profile chooses the thinking and token-limit syntax. |

Routing remains case-sensitive and uses the exact configured slug. Semantic
lookup uses only the six explicit entries above after ASCII case normalization;
unknown aliases and future variants require a new explicit entry and do not
inherit behavior merely because their name contains `glm`, `deepseek`,
`minimax`, `kimi`, or a provider prefix.

Official limits are semantic ceilings, not claims about a relay. A route-level
configured limit can reduce them. If a third-party route has no configured or
verified limit, the catalog uses the existing conservative default rather than
inventing the official maximum.

### Persisted third-party dialect profiles

Persist dialect profiles in both file state and PostgreSQL. Each record contains:

- upstream ID, exact final model slug, and protocol
- a SHA-256 configuration fingerprint covering normalized base URL, enabled
  protocols, final slug, and compatibility override
- probe schema version, last attempt time, last successful time, and state
  `verified`, `partial`, `unsupported`, or `unknown`
- tri-state `supported`, `rejected`, or `unobserved` evidence for endpoint,
  field, tool-loop, reasoning-replay, and streaming capabilities
- accepted token-limit field and reasoning/thinking control vocabulary
- observed reasoning output/replay carrier and SSE terminal behavior
- sanitized evidence codes, HTTP status, and event-type summaries

Profiles never store API keys, headers, prompts, response text, reasoning text,
tool arguments, or tool results. Authentication, quota, timeout, and 5xx
failures leave capabilities `unobserved` and do not overwrite the last verified
profile with a false negative.

A profile is invalidated when the upstream base URL, enabled protocols, exact
model slug, compatibility override, or probe schema version changes. Removing
an upstream removes its profiles. Valid profiles become refresh candidates
after the configurable dialect-probe interval, default seven days. Recognized
dialect-field errors queue an earlier refresh. Ordinary traffic continues using
the last verified profile until a refresh has conclusive evidence.

### Dialect probe lifecycle

Creating or changing an active upstream queues a bounded background probe for
models exposed to at least one downstream. Model discovery queues newly exposed
exact slugs. Administrators can rerun the same probe from diagnostics. Normal
client requests never wait for or launch a probe.

The queue deduplicates the profile key, runs at most one probe per upstream and
two globally, and caps each completion at 64 output tokens. It uses an existing
key mapped to the exact model, is tagged `compatibility_probe`, and obeys the
upstream's concurrency, rate, and request-quota accounting. It does not consume
a downstream quota or alter normal route health counters. A 401, 403, 429,
timeout, or 5xx stops the remaining subprobes for that key; it records an
operational failure and leaves capability evidence unchanged.

The probe uses synthetic low-output requests and a fake
`gateway_compat_probe` function that is never executed outside the probe. It
tests, independently for each configured protocol:

1. Minimal non-streaming and streaming text, including endpoint availability
   and valid terminal behavior.
2. `max_tokens` versus `max_completion_tokens`, omitting a token limit when
   neither has positive evidence.
3. Accepted thinking/reasoning controls and effort values relevant to the exact
   semantic profile.
4. Function tool selection, complete arguments, assistant/tool continuation,
   and exact `reasoning_content`-style replay when reasoning is emitted.
5. Incremental indexed tool arguments, `parallel_tool_calls`, and
   `stream_options.include_usage`.
6. Model-specific candidates such as GLM `tool_stream` and MiniMax
   `reasoning_split`; these are never tested for unrelated slugs.
7. Restricted-Responses behavior, including whether standard functions work
   while namespace, custom, or hosted tools do not.

A successful HTTP status is insufficient. Positive evidence requires the
expected semantic output, linked call IDs, parseable arguments, valid SSE order,
and a successful synthetic continuation. A recognized field-level 400 is
negative evidence only for that field. A model that ignores forced tool choice
or returns plain text is marked tool-incompatible for that route.

Before a probe completes, Chat routes use only `model`, `messages`, `stream`,
standard function tools, and a token limit only when already configured.
Responses routes are treated as restricted and are not preferred over a viable
Chat route. The gateway omits optional reasoning controls, sampling extensions,
`parallel_tool_calls`, `stream_options`, and vendor extensions. Required tools
are never silently removed: the baseline function adapter is attempted, and an
upstream rejection remains a classified model/dialect failure. If the baseline
response contains the known `reasoning_content` carrier, the gateway preserves
and replays its decoded string value without modification even before the
background profile is complete.

### Capability-aware routing and catalog aggregation

Route selection filters candidates by the resolved request profile before
quota, priority, and health ordering. A route is eligible only if it can carry
or reversibly adapt every required client feature. Accepting an endpoint path is
not enough. A native Responses route that lacks required function or replay
semantics loses to a verified Chat route with the reversible adapters.

For each exposed model, Codex metadata is taken from one deterministic
"catalog witness" route: the highest-fidelity verified route, then the existing
priority and health order. This avoids intersecting all routes down to the
weakest relay and avoids advertising an impossible union assembled from
different routes. Requests using advertised capabilities are restricted to the
witness route or another route with a compatible superset. A Responses
continuation is pinned to the same dialect-profile identity unless an equivalent
profile can preserve its reasoning and tool registry exactly.

If no route is verified yet, the best active route becomes a provisional
catalog witness and advertises only the conservative subset. The model remains
visible while its background probe runs. Diagnostics label the witness
`unverified`; a completed profile replaces its metadata atomically without any
downstream configuration change.

### Bounded dialect correction retry

Healthy requests remain single-attempt. If an upstream rejects a request before
any response bytes with a recognized field-level 400, the gateway may make one
correction retry using an already known dialect alternative, such as switching
the token-limit field or removing an optional probed extension. The retry is
recorded and schedules profile refresh.

The correction path never removes instructions, required tools, tool choice,
tool results, call IDs, reasoning replay, or required structured output. It does
not retry authentication, quota, overload, arbitrary 4xx, or failures after SSE
has started. A request that would become semantically weaker fails with the
original classified error instead.

### Reversible tool adapter registry

Each dispatch builds a tool adapter registry after route selection and before
protocol conversion. The registry is stored with Responses history so
`previous_response_id` continuation uses the same identities.

#### Standard function tools

Standard function tools retain their public name, JSON Schema, call ID,
arguments, and result. Existing Chat and Responses conversions remain the base
path.

#### Namespace tools

When the target lacks namespace support, each namespace member becomes a flat
function tool. Mappings are assigned in sorted canonical-identity order. For
the identity bytes `kind + NUL + namespace + NUL + member`, the generated name
is `gw_<middle>_<digest>`:

- `<middle>` is the namespace and member joined with `__`, with every run of
  characters outside `[A-Za-z0-9_-]` replaced by `_`, trimmed, and shortened as
  needed; an empty middle becomes `tool`
- `<digest>` starts with the first 12 lowercase hex characters of SHA-256 over
  the identity bytes
- the full name contains only `[A-Za-z0-9_-]` and is at most 64 ASCII bytes
- on collision with a top-level or already generated name, the digest grows in
  four-character steps and the middle is shortened to retain the limit
- if the complete digest still collides, conversion fails before dispatch

The reserved `gw_` prefix does not prohibit a caller's top-level tool with that
prefix; the collision procedure handles it explicitly. The registry stores the
chosen name, so continuation does not recompute a different mapping after the
tool set changes.

The description is prefixed with the original namespace description. The
registry maps the generated name back to the original namespace, member name,
and tool kind.

Chat or restricted-Responses function calls are restored as Responses
`function_call` items with the original `name` and `namespace`. The call ID and
argument bytes are unchanged. The continuation maps the corresponding output
back to the generated upstream function without exposing the generated name to
Codex. A namespace-member `tool_choice` is mapped to the same generated name.

This adapter applies to Chat targets and to Responses targets that advertise
functions but not namespaces.

#### Custom/freeform tools

When the target supports only functions, a Responses custom tool becomes a
function with one required string property named `input`. Calls are restored as
`custom_tool_call` items with the original name, optional namespace, and raw
input. Outputs are restored as `custom_tool_call_output`.

The adapter exists for compatibility, but model metadata advertises
`apply_patch_tool_type = null` until a model/upstream pair passes the custom-tool
semantic probe. Codex then uses its verified function-shaped shell tools. This
avoids inviting a less reliable freeform path while retaining an explicit
adapter for callers that send one.

#### Hosted and unknown tools

The generated Codex preset sets `web_search = "disabled"`. Hosted
`web_search`, `file_search`, and `computer_use` tools are handled as follows:

- With `tool_choice: auto` or no explicit choice, remove the optional hosted
  tool, return a downgrade response header, and record a safe diagnostic.
- When explicitly selected or when required and no executable tool remains,
  return HTTP 400 before upstream dispatch.
- Unknown tool kinds return HTTP 400 because their semantics are not known.

No tool description, arguments, prompt text, or credential appears in the
diagnostic.

### Reasoning continuity

Chat-provider reasoning is a first-class protocol value, not display text to
discard. The initial reversible carrier is the probed `reasoning_content` field.
If a relay uses another carrier, it is not guessed from a successful text
response: that route remains ineligible for thinking tool loops until a
documented adapter and a positive replay probe exist.

For non-streaming Chat-to-Responses conversion:

1. Convert `message.reasoning_content` into a Responses `reasoning` output item
   with `reasoning_text` content.
2. Place it before the associated function calls.
3. Store the reasoning item in Responses history.

For streaming conversion:

1. Emit `response.output_item.added` for a reasoning item.
2. Forward incremental bytes with official
   `response.reasoning_text.delta` events.
3. Emit `response.reasoning_text.done` and the completed reasoning item before
   completing associated function calls.
4. Continue forwarding text and tool argument deltas without buffering the
   full response.

On the next Codex request, merge the reasoning item and following function calls
into one Chat assistant message containing the route profile's verified replay
carrier and `tool_calls`. For the initial target profiles the carrier is
`reasoning_content`. Preserve reasoning bytes exactly. This matches the verified
Codex replay behavior and the DeepSeek/Kimi documented tool-loop requirement.

Reasoning is never copied into ordinary assistant text, logs, or errors.

### Claude Code reasoning bridge

The Messages adapter uses the same semantic and dialect resolver rather than
discarding Claude Code extensions. The captured
`thinking: {"type":"adaptive"}` enables the target model's supported thinking
mode, and `output_config.effort` maps through the exact-slug semantic effort
mapping to the route's probed wire control. A model whose reasoning cannot be
disabled, such as MiniMax M2.x, remains truthfully fixed-on.

An assistant Messages `thinking` block immediately associated with `tool_use`
maps to the Chat assistant message's verified reasoning carrier and
`tool_calls`. The inverse conversion emits a Messages `thinking` block before
the linked `tool_use` blocks. Streaming follows the official order:
`content_block_start`, zero or more `thinking_delta` events, one
`signature_delta`, then `content_block_stop`.

Because the third-party Chat model cannot create an Anthropic signature, the
gateway emits an opaque gateway signature over the exact thinking string,
model, dialect-profile identity, and linked call IDs. Claude Code treats the
signature as opaque and replays it. The gateway verifies it before restoring
reasoning on the next request; an invalid or modified block receives an
Anthropic-shaped 400 before dispatch. Acceptance uses the installed Claude Code
client to prove that the opaque signature is accepted and replayed.

The captured `context_management` edit with `clear_thinking_20251015` and
`keep: "all"` is satisfied by retaining all thinking blocks. A future edit that
would change history is implemented explicitly or reported as an optional
downgrade; it is never silently applied with guessed semantics. Anthropic
`cache_control` blocks remain optional cache hints: they are forwarded only
when a route profile has an equivalent, otherwise diagnostics report their
removal without changing prompt text.

### Continuation and replay

Responses history stores:

- input and output items
- original tool definitions
- tool adapter registry version and deterministic mapping
- semantic and dialect profile identities, fingerprints, and probe versions
- reasoning items required for tool continuation
- actual fallback stage

`previous_response_id` replay retains reasoning and tool state at the
high-fidelity stage. If an upstream rejects replay, existing staged fallback may
reduce old tool state or compact history, but diagnostics must report the stage
that succeeded. A reduced request is a warning, not a high-fidelity pass.

### Truthful client metadata and presets

`GET /v1/models?client_version=...` and generated client configuration use the
same resolved capability data as dispatch. Per-model Codex metadata reports:

- verified context window
- supported and default reasoning levels
- reasoning-summary support
- parallel tool support
- input modalities
- shell and apply-patch tool types
- verbosity and structured-output support

The portal-generated Codex catalog consumes the gateway model list instead of
inventing fixed TypeScript capability flags. Catalog generation fails visibly
if the live model list cannot be fetched.

The Codex preset includes `web_search = "disabled"`. OpenCode continues to use
`@ai-sdk/openai-compatible`. Claude Code uses `ANTHROPIC_BASE_URL` and all model
aliases point to the selected gateway slug. Hermes uses the gateway Chat
Completions base URL and model slug.

## Error and Diagnostic Contract

Capability mismatches use HTTP 400 and category
`gateway_protocol_capability_unsupported`. The error envelope matches the
downstream endpoint: OpenAI shape for Responses/Chat and Anthropic shape for
Messages.

Optional reductions use the bounded header
`x-chat2responses-downgrade`, with values such as
`optional_tool:web_search`, plus matrix metadata rather than changing the
response body. The value is capped at 512 ASCII bytes. Diagnostics include only:

- model slug
- selected upstream ID and protocol
- protocol transition
- adapter types used
- removed optional tool kinds
- dialect profile state and probe version
- dialect correction retry count
- fallback stage
- error category

Upstream authentication, quota, overload, and availability failures keep their
existing categories.

## Compatibility Matrix

The default batch matrix contains four client profiles:

| Client | Endpoint | Required semantic checks |
| --- | --- | --- |
| Codex | `/v1/responses` | model metadata, text SSE lifecycle, function call, namespace restore, reasoning replay, tool-output continuation, and `previous_response_id` |
| OpenCode | `/v1/chat/completions` | model visibility, Chat SSE terminal chunk, function call, indexed argument assembly, and result continuation |
| Claude Code | `/v1/messages` | model visibility, exact Messages SSE order, adaptive-thinking/effort mapping, signed thinking block replay, `tool_use`, matching `tool_result`, and non-zero `count_tokens` |
| Hermes | `/v1/chat/completions` | model visibility, Chat SSE, function call, usage chunk handling, and result continuation |

Validators parse JSON and SSE and require:

- at least one meaningful text, reasoning, or tool delta
- valid item/content-block types and IDs
- parseable complete tool arguments
- the expected terminal event or finish reason
- a tool call when the check requested one
- matching tool call/output IDs on continuation
- preserved namespace and reasoning markers on dedicated deterministic probes
- usage when the upstream supplies it

A plain-text answer to a tool prompt is a model-compatibility failure. HTTP 200
with malformed or incomplete SSE is a protocol failure.

Each matrix cell reports selected upstream, exact final slug, dialect profile
state and probe version, protocol transition, adapter set, correction retry
count, actual fallback stage, check-level results, error category, first
meaningful event latency, and total duration.

## Live Acceptance Set

Extend the `test` downstream to expose these exact configured slugs:

- `glm-5.2`
- `deepseek-v4-flash`
- `MiniMax/MiniMax-M2.5`
- `MiniMax/MiniMax-M2.7`
- `moonshotai/Kimi-K2.5`
- `moonshotai/kimi-k2.6`

Existing Claude and Grok entries remain covered. Each slug is tested as routed;
case variants and provider-prefixed aliases do not inherit another slug's pass.

The deterministic API matrix is supplemented by installed-client smoke tests:

- Codex CLI `0.144.0`
- OpenCode `1.17.9`
- Claude Code `2.1.195`
- Hermes Agent `0.14.0`

Each client performs a text task and a safe read-only tool task. Codex also
performs a namespace-backed tool task when an MCP namespace is available.

## Performance Contract

Normal processing must satisfy:

- no runtime network capability probe
- no additional upstream attempt for a healthy request
- linear request conversion in message and tool payload size
- deterministic O(n) tool registry construction
- incremental text, reasoning, and tool-argument SSE emission
- no full-response aggregation before downstream emission
- bounded per-call adapter state
- less than 50 ms P95 gateway-added first meaningful byte latency against a
  local mock under the repository's 20-way concurrency load shape

The load test records direct mock latency and gateway latency separately before
and after the change in the same release build and environment.

## Testing Strategy

Implementation follows test-driven development. Required automated coverage:

- exact-slug semantic classification without substring inheritance
- dialect-profile persistence, fingerprint invalidation, and tri-state evidence
- probed token/reasoning field selection and conservative unprobed behavior
- `strip_nonstandard_chat_fields` as a hard profile intersection
- capability-aware route filtering and deterministic catalog witness selection
- bounded field-correction retry and prohibited semantic retry cases
- namespace name collision, length, character, and restore cases
- custom/freeform request, response, stream, and continuation round trips
- non-streaming and streaming reasoning-content round trips
- DeepSeek/Kimi multi-step reasoning plus tool continuation
- Claude adaptive-thinking/effort mapping, gateway signatures, signature
  rejection, and thinking-plus-tool replay
- restricted-Responses adaptation for MiniMax
- optional hosted-tool downgrade and required-tool rejection
- `previous_response_id` replay with registry and reasoning state
- semantic SSE validators and malformed/empty negative fixtures
- Claude Code default matrix coverage and token counting
- generated Codex catalog and `web_search = "disabled"`
- full Rust workspace and frontend suites

Sanitized client captures become versioned fixtures containing only protocol
structure and synthetic prompts/results.

## Rollout

1. Add semantic matrix validators and Claude Code coverage without changing
   production dispatch.
2. Add dialect-profile persistence, the bounded background probe, and
   conservative unprobed profiles.
3. Add the two-layer capability resolver, capability-aware route filtering, and
   replace optimistic model metadata with catalog-witness metadata.
4. Add namespace/custom adapters for Chat and restricted Responses targets.
5. Add reasoning-content conversion, streaming, and history replay.
6. Add bounded dialect correction, hosted-tool downgrade/error diagnostics, and
   truthful presets.
7. Expose the exact Kimi and MiniMax slugs to `test` and run the full
   model/client matrix.
8. Run installed-client smoke tests and the performance acceptance test.

No compatibility behavior ships based only on a higher matrix pass count. A
new pass must satisfy the semantic validator for that feature.

## Acceptance Criteria

The work is accepted when:

1. All repository and frontend tests pass.
2. The four-client matrix covers every model exposed by `test`.
3. Each of `glm-5.2`, `deepseek-v4-flash`, `MiniMax/MiniMax-M2.5`,
   `MiniMax/MiniMax-M2.7`, `moonshotai/Kimi-K2.5`, and
   `moonshotai/kimi-k2.6` has at least one verified exact-route dialect profile
   and completes Codex text streaming plus a supported tool loop without
   protocol errors.
4. A deterministic Codex namespace probe restores the original namespace and
   member name through both JSON and SSE paths.
5. DeepSeek and Kimi thinking tool loops replay exact reasoning content and do
   not receive the documented missing-reasoning 400 error.
6. Claude Code completes a Messages `thinking`/`tool_use`/`tool_result` loop,
   replays a valid gateway thinking signature, and receives the official SSE
   block and terminal sequence.
7. Optional hosted tools produce an observable downgrade; required hosted or
   unknown tools fail before upstream dispatch.
8. Changing an upstream URL, protocol set, exact slug, override, or probe
   version invalidates the old dialect profile, while auth/quota probe failures
   do not erase the last verified evidence.
9. Advertised capabilities match one catalog-witness profile, and routing never
   sends a request requiring them to a weaker route.
10. Healthy requests remain single-attempt and gateway-added P95 first
   meaningful byte latency remains below 50 ms.
11. Upstream auth/quota failures remain distinguishable from conversion and
    model-semantic failures.
