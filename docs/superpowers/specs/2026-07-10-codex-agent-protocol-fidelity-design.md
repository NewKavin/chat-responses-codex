# Codex Agent Protocol Fidelity Design

## Summary

`chat-responses-codex` must let Codex, OpenCode, Claude Code, and Hermes use
one gateway URL, one downstream key, and an exposed model slug without knowing
which provider or wire protocol serves that model. The gateway owns protocol
adaptation and capability-driven request normalization.

The primary deployment target is a third-party hosted or self-deployed API, not
the model vendor's official endpoint. The gateway must preserve the Codex agent
loop when an OpenAI Responses request is routed to an upstream that implements
only Chat Completions, implements a restricted subset of Responses, or differs
from the vendor's documented request dialect.

The architecture is model-agnostic. Production code contains no model slug,
vendor-name classifier, model-family enum, or model-specific request branch.
Capabilities and semantic constraints are data that can be probed, configured,
exported, and imported. The current deployment emphasizes GLM, DeepSeek,
MiniMax, Kimi, and one configured Qwen vision-language model, but these names
are acceptance data rather than gateway logic. A different deployment can use
an entirely different model set without recompiling.

The existing gateway is a usable compatibility beta, not yet a high-fidelity
agent gateway. Basic routing, text streaming, standard function calls, and
pairwise conversion are established. Codex namespace tools, reasoning replay
through tool loops, truthful capability advertisement, semantic matrix checks,
and measured converter latency are not yet production-complete.

This design keeps the existing pairwise converters. It adds a generic capability
schema, external semantic policies, a dialect-probe engine, reversible tool and
image adapters, reasoning continuity, semantic diagnostics, and truthful client
presets. It does not add a large universal intermediate representation.

## Evidence Standard

Protocol behavior is not inferred from field names. Three evidence lanes are
kept separate:

1. Downstream client contracts come from exact client source and sanitized
   captures from the installed version.
2. Model semantics used by a deployment policy come from the model vendor's
   current official API reference or OpenAPI document. They are not compiled
   into request conversion code.
3. Upstream wire syntax comes from a repeatable live probe against the exact
   configured third-party upstream, model slug, and protocol. For wire syntax,
   this observed result overrides vendor documentation.

A feature beyond the conservative common subset is treated as unsupported until
it has positive evidence for the exact route or an explicit administrator
override. Live probes run in diagnostics, background setup, or acceptance
workflows, never on the normal request path. Source URLs, client versions,
upstream IDs, exact slugs, probe versions, and timestamps are retained so a
later client, relay, or model release cannot silently redefine an existing
profile.

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
- [Qwen3-VL source and cookbooks](https://github.com/QwenLM/Qwen3-VL) and the
  [Alibaba Model Studio Qwen-VL API](https://help.aliyun.com/zh/model-studio/developer-reference/qwen-vl-api)

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

Codex source defines image content as Responses `input_image` with a string
`image_url` and optional `detail`. Local attachments become Data URLs, while
remote image URLs remain URLs. Source tests confirm that mixed input and image
items are retained in request history.

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

The official Messages schema represents image content as an `image` block whose
source is either `{type: "url", url}` or
`{type: "base64", media_type, data}`. Image support in this design follows that
schema; it is not inferred from the text-only local capture.

### Verified OpenCode and Hermes behavior

OpenCode custom providers default to `@ai-sdk/openai-compatible` and use Chat
Completions. Installed OpenCode `1.17.9` sent `stream: true`,
`stream_options.include_usage: true`, standard function tools, and standard
assistant/tool continuation messages.

Installed Hermes `0.14.0` uses the OpenAI SDK Chat Completions stream, requests
`stream_options.include_usage`, incrementally assembles indexed tool-call
arguments, and replays assistant `tool_calls` followed by `role: "tool"`
messages with `tool_call_id`.

OpenCode source lowers image media to Chat `image_url` and Responses
`input_image`, including MIME-qualified Data URLs. Hermes' native vision path
uses OpenAI-style `image_url` parts with Data URLs. These source contracts define
the image matrix; installed-client attachment smoke tests remain conditional on
the public CLI workflow.

## Current Maturity

| Area | Current level | Evidence and gap |
| --- | --- | --- |
| Routing and authentication | Established | Multiple upstream protocols, quotas, retries, and error categories exist. |
| Text JSON and SSE conversion | Established | Automated coverage and live text requests pass. |
| Standard function calls | Partial | Basic calls convert, but the matrix does not require an actual tool call and reasoning replay is missing. |
| Codex namespace/custom tools | Incomplete | Chat fallback currently strips namespace and custom tools. |
| Reasoning agent loops | Incomplete | Chat `reasoning_content` is not represented in Responses history. DeepSeek and Kimi document a 400 error when it is omitted after a thinking tool call. |
| Image input | Partial | Chat/Responses pairwise conversion recognizes basic image URLs, but nested detail, Messages images, capability advertisement, route filtering, and semantic tests are incomplete. |
| Claude Code | Partial | The Messages adapter supports tools and thinking, but Claude Code is absent from the default batch matrix. |
| Capability metadata | Incomplete | The Codex catalog advertises optimistic fixed capabilities for every model. |
| Diagnostics | Partial | The current matrix treats HTTP 200 plus non-empty output as success and always reports a null fallback stage. |
| Performance | Unverified | Streaming is incremental, but gateway-added first meaningful byte latency has no acceptance measurement. |

The baseline workspace suite passes 466 tests with one ignored load test. A
live matrix run across five exposed models and Codex, OpenCode, and Hermes
reported 14 passes and one transient upstream authentication failure. This is
route-level evidence only; it is not proof of agent-loop fidelity.

## Goals

1. Preserve instructions, text and image input, text streaming, function tools,
   namespace tools, supported custom tools, tool outputs, reasoning continuity,
   and multi-turn replay across Responses, Chat, and Messages routes.
2. Support arbitrary model slugs through one generic capability schema. The
   current GLM, DeepSeek, MiniMax, Kimi, and Qwen targets are live acceptance
   data, not compiled behavior.
3. Keep production dispatch free of model-name and provider-hostname
   classification.
4. Keep client configuration provider-agnostic: gateway URL, downstream key,
   and exposed model slug are sufficient.
5. Prefer a configured native Responses route when its declared capability
   subset can carry the request; otherwise adapt once to Chat Completions.
6. Make every advertised capability executable. Optional unsupported features
   may downgrade with diagnostics; required unsupported features fail before
   dispatch.
7. Validate Codex, OpenCode, Claude Code, and Hermes with protocol-semantic
   checks and installed-client smoke tests.
8. Keep healthy requests single-attempt and streaming, with less than 50 ms P95
   gateway-added first meaningful byte latency against a local mock upstream.

## Non-Goals

- Emulating provider-hosted web search, file search, computer use, or code
  execution when the selected upstream has no equivalent service.
- Exposing raw prompts, tool arguments, image URLs/data, or credentials in
  compatibility logs.
- Hiding upstream authentication, quota, availability, or model-quality
  failures as converter success.
- Runtime capability probes on ordinary requests.
- Assuming a third-party API accepts vendor-specific fields because it serves a
  vendor model or uses an OpenAI-compatible path.
- Downloading remote input images inside the gateway, converting images to
  text, or silently discarding an unsupported image.
- Video, audio, image generation, or cross-provider file-store emulation in the
  initial multimodal scope.
- Shipping active model-name policies or vendor-specific model branches in the
  application binary.
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

An external capability policy describes semantic constraints that cannot be
safely inferred from one response, such as fixed-on reasoning or a documented
context ceiling. The selected upstream, routed slug, and target protocol
determine the wire fields that can express those semantics. The resolver
combines both data sources before building the upstream payload. Vendor
documentation is never used as proof that a third-party endpoint accepts a
field, and a model name is never used as an implicit policy selector unless an
administrator configured that selector.

### Native means capability-compatible, not endpoint-compatible

An upstream accepting `/v1/responses` does not imply full OpenAI Responses
support. A restricted implementation may support functions without namespace,
custom, or hosted tools. The gateway adapts unsupported Responses subsets
before dispatch instead of blindly passing them through.

## Architecture

### Generic capability layers

Add three independent, model-agnostic data types:

1. `CapabilityPolicy` contains administrator-authored semantic constraints,
   mappings, and probe candidates for facts that cannot be represented by the
   conservative baseline. It is external JSON data, not a Rust model registry,
   and it does not by itself prove wire support.
2. `UpstreamDialectProfile` records what one configured route actually accepts
   and emits. Its identity is `(upstream_id, exact_model_slug, protocol)`, where
   the slug is the final runtime value sent upstream. The tuple is a data key,
   not a model classifier.
3. `ResolvedCapabilities` is the immutable request-time intersection used by
   routing, conversion, model metadata, and diagnostics.

The production binary contains no known model slugs, vendor substrings, or
provider-specific model enums. No hostname participates in capability
decisions. An OpenAI-looking base URL, an `/v1` suffix, or an
"OpenAI-compatible" label proves only the configured endpoint location.

The generic schema covers:

| Dimension | Representative values |
| --- | --- |
| Protocol | Chat Completions, Responses, downstream Messages |
| Input modalities | text, image; HTTPS URL and Base64/Data URL image sources |
| Tools | function, namespace, custom/freeform, hosted, parallel calls, forced choice, continuation |
| Reasoning | off/optional/fixed-on, effort vocabulary and mapping, output carrier, replay requirement |
| Generation controls | accepted token-limit field, configured ceilings, sampling policy, structured output |
| Streaming | text, reasoning, indexed tool arguments, usage chunks, terminal event behavior |
| Extensions | declarative request patches, response predicates, and dependencies supplied by policy data |

Policies are persisted in file state and PostgreSQL and can be imported or
exported as versioned JSON. Selectors are administrator data and may target an
exact exposed slug, a final routed slug, an upstream ID, a protocol, a
user-defined tag, or an explicit glob. More-specific selectors win; equal
specificity and priority with conflicting values makes the bundle invalid.
There are no active built-in selectors. Moving the project to another
deployment therefore requires configuration changes, not source changes.

Policy loading is atomic. Unknown schema versions, unknown required keys,
invalid enums, or ambiguous selectors reject the candidate bundle and retain
the last valid in-memory version. Normal requests use the in-memory snapshot
and never read policy files.

Declarative extensions are intentionally not plugins. They cannot change the
upstream URL, headers, credentials, model, prompt/input/messages, tools, stream
mode, or media payloads. A request patch may set only unprotected optional body
paths declared in the bundle. A response predicate uses bounded JSON/SSE paths
and comparisons; it cannot contain executable code, templates, network calls,
or filesystem access.

The resolver receives the downstream feature set, runtime model slugs, selected
route identity, applicable external policies, persisted dialect evidence, and
administrator overrides. Values resolve in this order:

1. explicit administrator route override
2. conclusive live dialect evidence for wire acceptance
3. external semantic policy for facts the probe cannot establish
4. conservative protocol baseline

Absence of probe evidence does not negate a required semantic invariant. A
direct conflict between conclusive wire evidence and a required policy fails
closed and is reported for administrator resolution.

The resolver returns token-limit syntax, reasoning controls and replay carrier,
sampling omissions, modality and tool capabilities, streaming behavior,
declarative extensions, adapter set, catalog metadata, and downgrade policy.
Lookup is bounded in-memory work with no I/O.

The existing `strip_nonstandard_chat_fields` setting remains as a legacy hard
override represented internally as a generic capability intersection. It
suppresses optional request extensions but never removes reasoning state, tool
state, images, or other required continuation content already present.

Official model limits can be recorded in external policy as semantic ceilings,
but they are not claims about a relay. A route-level configured or verified
limit can reduce them. Without either value, the catalog uses the existing
conservative default rather than inferring from the model name.

### Persisted third-party dialect profiles

Persist dialect profiles in both file state and PostgreSQL. Each record contains:

- upstream ID, exact final model slug, and protocol
- a SHA-256 configuration fingerprint covering normalized base URL, enabled
  protocols, final slug, and compatibility override
- probe schema version, last attempt time, last successful time, and state
  `verified`, `partial`, `unsupported`, or `unknown`
- a generic tri-state capability map using `supported`, `rejected`, or
  `unobserved` evidence for endpoint, field, modality, tool-loop,
  reasoning-replay, and streaming behavior
- accepted token-limit field and reasoning/thinking control vocabulary
- observed image source forms, reasoning output/replay carrier, and SSE terminal
  behavior
- sanitized evidence codes, HTTP status, and event-type summaries

Profiles never store API keys, headers, prompts, response text, reasoning text,
image URLs/data, tool arguments, or tool results. Authentication, quota,
timeout, and 5xx failures leave capabilities `unobserved` and do not overwrite
the last verified profile with a false negative.

A profile is invalidated when the upstream base URL, enabled protocols, runtime
slug, relevant route override, or probe schema version changes. A policy change
invalidates resolved-capability caches but retains still-valid raw probe
evidence. Removing an upstream removes its profiles. Valid profiles become
refresh candidates after the configurable dialect-probe interval, default seven
days. Recognized dialect-field errors queue an earlier refresh. Ordinary
traffic continues using the last verified profile until a refresh has
conclusive evidence.

### Dialect probe lifecycle

Creating or changing an active upstream queues a bounded background probe for
runtime models exposed to at least one downstream. Model discovery queues newly
exposed slugs without classifying their names. Administrators can rerun the same
probe from diagnostics. Normal client requests never wait for or launch a
probe.

The queue deduplicates the profile key, runs at most one probe per upstream and
two globally, and caps each completion at 64 output tokens. It uses an existing
key mapped to the exact model, is tagged `compatibility_probe`, and obeys the
upstream's concurrency, rate, and request-quota accounting. It does not consume
a downstream quota or alter normal route health counters. A 401, 403, 429,
timeout, or 5xx stops the remaining subprobes for that key; it records an
operational failure and leaves capability evidence unchanged.

The engine owns protocol-level cases only. Optional extension cases are generic
data objects containing a request patch, prerequisites, and a response
predicate; policy bundles can add them without adding code or naming a model in
the binary. The probe uses synthetic low-output requests and a fake
`gateway_compat_probe` function that is never executed outside the probe. It
tests, independently for each configured protocol:

1. Minimal non-streaming and streaming text, including endpoint availability
   and valid terminal behavior.
2. `max_tokens` versus `max_completion_tokens`, omitting a token limit when
   neither has positive evidence.
3. Thinking/reasoning controls and effort values declared as candidates by the
   applicable policy.
4. Function tool selection, complete arguments, assistant/tool continuation,
   and exact `reasoning_content`-style replay when reasoning is emitted.
5. Incremental indexed tool arguments, `parallel_tool_calls`, and
   `stream_options.include_usage`.
6. Base64/Data URL image input, an administrator-configured HTTPS image fixture,
   mixed text/image ordering, and image-informed function selection.
7. Declarative extension cases supplied by policy. Fields such as
   `tool_stream` or `reasoning_split` are data in a deployment bundle, not
   model branches in the probe engine.
8. Restricted-Responses behavior, including whether standard functions work
   while namespace, custom, or hosted tools do not.

A successful HTTP status is insufficient. Positive evidence requires the
expected semantic output, linked call IDs, parseable arguments, valid SSE order,
and a successful synthetic continuation. A recognized field-level 400 is
negative evidence only for that field. A model that ignores forced tool choice
or returns plain text is marked tool-incompatible for that route. An image pass
requires a structured answer derived from a high-contrast fixture whose answer
is absent from the prompt; accepting an image-shaped request alone is not
positive multimodal evidence.

Before a probe completes, Chat routes use only `model`, `messages`, `stream`,
standard function tools, and a token limit only when already configured.
Responses routes are treated as restricted and are not preferred over a viable
Chat route. The gateway omits optional reasoning controls, sampling extensions,
`parallel_tool_calls`, `stream_options`, and vendor extensions. Required tools
are never silently removed: the baseline function adapter is attempted, and an
upstream rejection remains a classified model/dialect failure. If the baseline
response contains the known `reasoning_content` carrier, the gateway preserves
and replays its decoded string value without modification even before the
background profile is complete. Image input is not part of the unprobed
baseline; an image request waits for no probe and is routed only to a route with
positive evidence or an explicit administrator override.

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
Until adapter semantics and tool-registry compatibility have a durable persisted
equivalence identity, a missing or failed exact continuation profile fails closed;
a capability superset alone never authorizes continuation failover.

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
tool results, call IDs, reasoning replay, images, or required structured output.
It does not retry authentication, quota, overload, arbitrary 4xx, or failures
after SSE has started. A request that would become semantically weaker fails
with the original classified error instead.

### Request data flow

Each request follows one capability-driven path:

1. Parse the downstream payload into `RequestedFeatures` without flattening the
   protocol-native message body. Required features include modalities, tool
   kinds and choice, reasoning continuation, structured output, and stream
   semantics.
2. Resolve generic capabilities for every candidate route from the in-memory
   policy and dialect snapshots.
3. Remove candidates that cannot preserve or reversibly adapt every required
   feature, then apply existing quota, priority, affinity, and health ordering.
4. Build only the selected route's tool, modality, and reasoning adapters and
   perform the existing pairwise protocol conversion.
5. Translate upstream JSON or SSE incrementally and attach downgrade and route
   diagnostics.
6. Persist the resolved-capability fingerprint and reversible adapter state for
   continuation.

No large universal message representation is introduced. `RequestedFeatures`
contains capability flags and identities, not copied prompt, image, or tool
payloads.

### Input modality adapter

The initial generic multimodal capability is image input. The pairwise adapters
map only structurally equivalent forms:

- Responses `input_image` with `image_url`
- Chat content `image_url` with nested `url` and optional `detail`
- Messages `image` with a supported URL or Base64 `source`

HTTPS URLs are passed through without fetching. Base64/Data URLs retain their
media type and encoded data without image transcoding. Mixed text/image order
is stable, and `detail` is preserved only when the selected dialect accepts it;
otherwise it is an observable optional downgrade. Request body limits remain
the bound for inline data.

The gateway does not resolve DNS, fetch a remote image, upload to a provider
file store, or replace an image with generated text. A cross-provider `file_id`
is rejected before dispatch unless a native route has positive evidence that
the same identifier is usable. An unsupported required image makes a route
ineligible and produces `gateway_protocol_capability_unsupported` if no route
remains. Video, audio, and generated media are left for future capability types.

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
`apply_patch_tool_type = null` until the selected route passes the generic
custom-tool semantic probe. Codex then uses its verified function-shaped shell tools. This
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
into one Chat assistant message containing the route profile's registered and
verified replay carrier plus `tool_calls`. The initial carrier adapter supports
the documented Chat `reasoning_content` field, independent of model name.
Preserve the decoded reasoning string exactly. This matches verified Codex
replay behavior and documented thinking tool-loop requirements.

Reasoning is never copied into ordinary assistant text, logs, or errors.

### Claude Code reasoning bridge

The Messages adapter uses the same semantic and dialect resolver rather than
discarding Claude Code extensions. The captured
`thinking: {"type":"adaptive"}` enables the target model's supported thinking
mode, and `output_config.effort` maps through the resolved generic effort policy
to the route's probed wire control. A policy declaring fixed-on reasoning
remains truthfully fixed-on.

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
- capability-policy and dialect-profile identities, fingerprints, and versions
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

The existing administration and troubleshooting surfaces expose each resolved
capability, its source (`override`, `probe`, `policy`, or `baseline`), profile
age, conflicts, and probe state. They support policy and expectation
import/export plus manual probe refresh. None of these controls are required in
Codex, OpenCode, Claude Code, or Hermes configuration.

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
- removed optional tool kinds or modality hints
- capability-policy ID, schema version, and digest
- dialect profile state and probe version
- dialect correction retry count
- fallback stage
- error category

Upstream authentication, quota, overload, and availability failures keep their
existing categories.

## Compatibility Matrix

The default batch matrix contains four client profiles and enumerates models
from the selected downstream at runtime:

| Client | Endpoint | Required semantic checks |
| --- | --- | --- |
| Codex | `/v1/responses` | model metadata, text SSE lifecycle, `input_image` when required, function call, namespace restore, reasoning replay, tool-output continuation, and `previous_response_id` |
| OpenCode | `/v1/chat/completions` | model visibility, Chat SSE terminal chunk, `image_url` when required, function call, indexed argument assembly, and result continuation |
| Claude Code | `/v1/messages` | model visibility, exact Messages SSE order, `image.source` when required, adaptive-thinking/effort mapping, signed thinking replay, `tool_use`, matching `tool_result`, and non-zero `count_tokens` |
| Hermes | `/v1/chat/completions` | model visibility, Chat SSE, protocol-level image conversion when required, function call, usage chunk handling, and result continuation |

Validators parse JSON and SSE and require:

- at least one meaningful text, reasoning, or tool delta
- valid item/content-block types and IDs
- parseable complete tool arguments
- the expected terminal event or finish reason
- a tool call when the check requested one
- matching tool call/output IDs on continuation
- preserved namespace and reasoning markers on dedicated deterministic probes
- preserved image media type, source, ordering, and optional detail
- an image-derived structured tool call when image capability is required
- usage when the upstream supplies it

A plain-text answer to a tool prompt is a model-compatibility failure. HTTP 200
with malformed or incomplete SSE is a protocol failure.

Each matrix cell reports selected upstream, exact final slug, dialect profile
state and probe version, protocol transition, adapter set, correction retry
count, actual fallback stage, check-level results, error category, first
meaningful event latency, and total duration.

## Dynamic Live Acceptance

Add a persisted, importable `compatibility_expectations` collection. Each entry
contains an administrator selector, required generic capability bundles,
client profiles, permitted optional downgrades, and an optional HTTPS image
fixture. The matrix reads this collection and the downstream model list at
runtime. It contains no compiled target list. Expectations are assertions for
diagnostics and acceptance only; they never grant a route capability, change
catalog metadata, or participate in production routing.

The reusable bundles are capability data:

- `agent_core`: text JSON/SSE, function selection, complete streamed arguments,
  linked result continuation, and valid usage/terminal behavior
- `reasoning_agent`: reasoning output plus exact replay through a tool loop
- `image_agent`: HTTPS and Data URL image input, stable mixed-content order,
  image-derived structured function selection, and text streaming

For the current deployment, rollout configuration will assign the appropriate
bundles to `glm-5.2`, `deepseek-v4-flash`, `MiniMax/MiniMax-M2.5`,
`MiniMax/MiniMax-M2.7`, `moonshotai/Kimi-K2.5`, and
`moonshotai/kimi-k2.6`. It also assigns `agent_core` and `image_agent` to one
administrator-selected Qwen vision-language slug. The Qwen slug is whatever the
third-party upstream actually exposes; changing it requires only data changes.
Existing Claude, Grok, and any future entries remain dynamically covered by
their configured or probed bundles.

An importable deployment policy, authored from the verified sources above,
records the current targets' reasoning invariants, effort vocabulary, sampling
rules, context ceilings, replay requirements, and declarative extension probe
cases. The bundle is current-deployment data and can be replaced wholesale when
the project is moved; it never becomes an active built-in default.

Route evidence remains exact even though policy selectors are configurable. A
case variant or provider-prefixed alias does not inherit another route's live
probe result.

The deterministic API matrix is supplemented by installed-client smoke tests:

- Codex CLI `0.144.0`
- OpenCode `1.17.9`
- Claude Code `2.1.195`
- Hermes Agent `0.14.0`

Each client performs a text task and a safe read-only tool task. Codex also
performs a namespace-backed tool task when an MCP namespace is available.
Image-capable installed clients perform an attachment task; clients without a
public attachment interface are covered by their protocol-level matrix instead
of an invented CLI workflow.

## Performance Contract

Normal processing must satisfy:

- no runtime network capability probe
- no additional upstream attempt for a healthy request
- linear request conversion in message and tool payload size
- linear image pass-through without remote fetch, transcoding, or a full Base64
  decode/re-encode cycle
- deterministic O(n) tool registry construction
- incremental text, reasoning, and tool-argument SSE emission
- no full-response aggregation before downstream emission
- bounded per-call adapter state
- less than 50 ms P95 gateway-added first meaningful byte latency against a
  local mock under the repository's 20-way concurrency load shape

The load test records direct mock latency and gateway latency separately before
and after the change in the same release build and environment. A bounded
inline-image fixture is measured separately so text-only performance cannot hide
an image-copying regression.

## Stream-Only Upstream Delivery Adaptation

Some third-party OpenAI-compatible routes accept text, reasoning, and function
tools only when the request uses SSE. The same exact route can return HTTP 200
with an empty JSON completion when `stream:false`. This is a wire-delivery
property, not a model-family property.

Add `NonStreamingResponse` to the generic route capability vocabulary. The
minimal non-stream probe verifies usable output rather than HTTP status alone,
while the existing stream probe independently verifies a meaningful delta and
terminal event. A route with `NonStreamingResponse=Rejected` and
`TextStream=Supported` uses an `SseAggregate` upstream attempt for a downstream
JSON request. The same direct aggregate mode is allowed when non-stream evidence
is unobserved but `TextStream=Supported` comes from a probe or exact override;
baseline stream assumptions are not sufficient evidence. The downstream still
receives the endpoint's ordinary JSON schema, so Codex, OpenCode, Claude Code,
and Hermes share one adapter.

The attempt modes are explicit:

- `Json`: request JSON and return JSON
- `SsePassThrough`: request SSE and forward/translate incrementally
- `SseAggregate`: request SSE, accumulate the source protocol, then perform the
  existing JSON protocol conversion

`SseAggregate` is allowed only for a non-stream downstream request. It must
never delay a downstream streaming request or change the first-event latency
contract above. It is recorded as the `stream_to_json` adapter and is not a
semantic downgrade.

When route evidence is still unknown, an empty JSON success can trigger one
same-route SSE recovery only when usage explicitly reports zero output tokens,
no content/reasoning/tool call exists, no stateful continuation is present, and
no hosted/computer tool can execute upstream. Missing usage is not zero. A
successful recovery atomically merges exact-route evidence under the capability
update lock; a failed recovery does not alter the profile. The route
configuration fingerprint and probe schema version invalidate learned evidence.
A bounded per-route singleflight elects one unknown-route recovery leader;
followers wait before making their first upstream attempt, then re-read the
profile and use the learned single-attempt mode. This cold-start serialization
is exact-route only and never applies after conclusive evidence exists.

Chat accumulation is indexed by choice and tool-call index and preserves text,
refusal, `reasoning_content`, legacy function calls, fragmented function names
and arguments, finish reasons, log probabilities, usage-only tail chunks, IDs,
model, timestamps, service tier, and system fingerprint. Responses accumulation
uses the complete response object from `response.completed` or
`response.incomplete`. Error/failed events and incomplete transport termination
remain upstream failures; partial output is never promoted to JSON success.

The SSE decoder accepts LF and CRLF delimiters, `data:` with or without a space,
multiple data lines, comments, arbitrary network chunk boundaries, and a final
EOF frame. Aggregation reuses the stream idle/max-duration watchdog, enforces
bounded frame and total bytes, and leaves upstream/downstream slot release to
the existing RAII guards.

## Testing Strategy

Implementation follows test-driven development. Required automated coverage:

- policy schema validation, selector precedence, atomic reload, import/export,
  and last-valid fallback
- arbitrary synthetic model slugs working without a model classifier
- dialect-profile persistence, fingerprint invalidation, and tri-state evidence
- generic declarative probe cases, token/reasoning field selection, and
  conservative unprobed behavior
- `strip_nonstandard_chat_fields` as a hard profile intersection
- capability-aware route filtering and deterministic catalog witness selection
- bounded field-correction retry and prohibited semantic retry cases
- Responses/Chat/Messages image URL and Data URL round trips, mixed ordering,
  detail downgrade, unsupported image rejection, and native-only `file_id`
- namespace name collision, length, character, and restore cases
- custom/freeform request, response, stream, and continuation round trips
- non-streaming and streaming reasoning-content round trips
- policy-required multi-step reasoning plus tool continuation
- Claude adaptive-thinking/effort mapping, gateway signatures, signature
  rejection, and thinking-plus-tool replay
- restricted-Responses adaptation using generic capability fixtures
- optional hosted-tool downgrade and required-tool rejection
- `previous_response_id` replay with registry and reasoning state
- semantic SSE validators and malformed/empty negative fixtures
- Claude Code default matrix coverage and token counting
- dynamic `compatibility_expectations` and image-agent semantic validators
- generated Codex catalog and `web_search = "disabled"`
- full Rust workspace and frontend suites

Sanitized client captures become versioned fixtures containing only protocol
structure and synthetic prompts/results.

## Rollout

1. Add semantic matrix validators, dynamic expectations, image fixtures, and
   Claude Code coverage without changing production dispatch.
2. Add the generic policy schema and persistence. Remove model-family,
   substring, and official-host classifiers from production normalization while
   preserving the legacy strict override as data.
3. Add dialect-profile persistence, generic declarative probes, image probes,
   and conservative unprobed profiles.
4. Add the generic capability resolver, capability-aware route filtering, and
   replace optimistic model metadata with catalog-witness metadata.
5. Add modality and namespace/custom adapters for Chat, Responses, and Messages
   targets.
6. Add reasoning-content conversion, streaming, and history replay.
7. Add bounded dialect correction, hosted-tool and modality downgrade/error
   diagnostics, and truthful presets.
8. Configure the current deployment's GLM, DeepSeek, MiniMax, Kimi, Qwen, Claude,
   and Grok expectations, then run the dynamic model/client matrix.
9. Run installed-client smoke tests and the performance acceptance test.

No compatibility behavior ships based only on a higher matrix pass count. A
new pass must satisfy the semantic validator for that feature.

## Acceptance Criteria

The work is accepted when:

1. All repository and frontend tests pass.
2. Production request normalization contains no model slug, vendor substring,
   provider hostname, or model-family dispatch. A new synthetic slug receives
   configured/probed capabilities without recompilation.
3. The four-client matrix dynamically covers every model exposed by `test` and
   enforces its configured expectation bundles.
4. The current six GLM, DeepSeek, MiniMax, and Kimi entries each have at least
   one verified route and complete their configured text, reasoning, and tool
   checks without protocol errors.
5. The administrator-selected Qwen vision-language entry completes HTTPS and
   Data URL image understanding, mixed text/image input, streaming text, and an
   image-derived tool loop through the gateway.
6. A deterministic Codex namespace probe restores the original namespace and
   member name through both JSON and SSE paths.
7. Every route whose policy requires thinking continuation replays the exact
   reasoning string and avoids missing-reasoning protocol errors.
8. Claude Code completes a Messages `thinking`/`tool_use`/`tool_result` loop,
   replays a valid gateway thinking signature, and receives the official SSE
   block and terminal sequence.
9. Optional hosted tools and image-detail hints produce observable downgrades;
   required hosted, unknown, or unsupported image inputs fail before upstream
   dispatch without losing content.
10. Changing an upstream URL, protocol set, runtime slug, override, or probe
   version invalidates the old dialect profile, while auth/quota probe failures
   do not erase the last verified evidence.
11. Advertised capabilities match one catalog-witness profile, and routing never
   sends a request requiring them to a weaker route.
12. Healthy requests remain single-attempt and gateway-added P95 first
   meaningful byte latency remains below 50 ms.
13. Upstream auth/quota failures remain distinguishable from conversion and
    model-semantic failures.
