# Codex Reasoning And Subagent Transport Design

## Context

The gateway serves Codex 0.146.0 through the Responses API while most deployed
domestic-model routes use Chat Completions upstreams. The main Codex turn works
with models such as GLM and DeepSeek, but delegated turns have failed for two
different reasons:

1. Existing local `agents/default.toml` files can pin a different model than
   the portal-generated main configuration. The deployed log shows repeated
   `gpt-5.6-sol` model-admission failures before any upstream request.
2. Codex Multi-Agent V2 encrypts `spawn_agent.message`, `send_message.message`,
   and `followup_task.message`. The child receives a visible `agent_message`
   envelope while the actual payload is carried in `encrypted_content`. A
   Responses-to-Chat adapter has no key with which to decrypt that payload.

The current converter replaces encrypted parts with
`[encrypted content omitted]`. This avoids the old invalid-payload response but
silently removes the task, so it cannot be considered a compatibility fix.

The live Codex catalog also reports `none` for every deployed domestic model.
The catalog does this intentionally when runtime capability evidence does not
prove `ReasoningOutput`, a reasoning control field, and a non-empty effort map.
The portal therefore has no verified configurable effort to offer even though
operators want `low`, `medium`, `high`, `xhigh`, and `max`, with `medium` as the
normal default.

## Goals

1. Make new Codex sessions use a subagent protocol that Chat fallback can carry
   without encrypted task loss.
2. Preserve all visible plaintext `agent_message` content and reject encrypted
   Chat fallback deterministically instead of silently replacing it.
3. Keep native Responses forwarding byte-for-byte compatible with encrypted
   content when the selected upstream supports that protocol.
4. Keep the main and default-agent model and reasoning effort synchronized in
   portal-generated configuration.
5. Prove real child-task delivery and child-result delivery in the installed
   Codex smoke test.
6. Offer portal reasoning choices `low`, `medium`, `high`, `xhigh`, and `max`.
   Do not offer `minimal`. Prefer `medium` when the live catalog verifies it.
7. Publish only reasoning levels backed by current route evidence. Do not infer
   `xhigh` or `max` from a model family name.

## Non-goals

- The gateway will not decrypt Codex V2 payloads or log their ciphertext.
- The gateway will not send ciphertext to a Chat model as ordinary content.
- The gateway will not modify Codex itself or invent a local encryption key.
- The gateway will not globally claim that all domestic models support every
  reasoning level.
- The portal will not enable the Codex `multi_agent_v2` feature.
- No API key, authorization header, prompt, tool argument, upstream body,
  billing field, or delegated payload may enter logs or diagnostic responses.

## Design

### 1. Codex multi-agent version contract

Every model entry produced by the gateway's Codex catalog includes:

```json
{"multi_agent_version":"v1"}
```

V1 is the gateway-wide conservative contract. It carries spawn tasks and later
messages as ordinary text and works for both Chat fallback and native Responses
routes. The gateway does not advertise V2 until a future capability proves that
every selectable route for a model can preserve the encrypted protocol. This
avoids a mixed-route failure where a model-level catalog selects V2 from one
Responses witness and a later retry lands on a Chat route.

Codex gives an explicit V2 feature override and an existing session's stored
version precedence over the catalog. Documentation therefore instructs users
to keep only `features.multi_agent = true`, not enable `multi_agent_v2`, replace
the live catalog, and start a new session after this change.

### 2. Encrypted `agent_message` boundary

The Responses-to-Chat converter accepts an `agent_message` only when all of its
content is representable plaintext. It preserves `input_text`, `output_text`,
top-level text, order, and the existing assistant-history merge behavior.

The converter returns a dedicated protocol error when any of these forms is
present:

- a content part with `type = "encrypted_content"`;
- a non-null `encrypted_content` field on a content part;
- a non-null top-level `encrypted_content` field.

It never emits a placeholder and never includes the ciphertext in the error.
The gateway maps the dedicated error to a stable 400 compatibility code such
as `encrypted_agent_message_requires_responses_upstream`. The public message
states that the current Chat fallback cannot represent encrypted subagent
payloads and that a V1/new session is required. It does not expose item data.

Responses-to-Responses dispatch keeps using the existing native payload path,
so the new converter error is reached only when translation to Chat is needed.
The same rule applies to Responses output translated for a Chat downstream:
visible plaintext remains supported; encrypted output is rejected rather than
misrepresented.

### 3. Portal and static guidance

The portal continues to derive one selected model and one live-catalog effort,
then writes them to both `config.toml` and `agents/default.toml`. The Codex setup
panel adds a short compatibility notice:

- agent protocol version comes from `model-catalog.json`;
- Chat-compatible routes use V1 plaintext delegation;
- do not enable `multi_agent_v2`;
- regenerate both configuration files and start a new session after changing
  the model or catalog.

The static templates use the same reasoning-effort placeholder in both files
instead of leaving the main template at literal `none`.

### 4. Real delegation proof

The installed-client smoke writes an unpredictable marker to a read-only probe
file. The parent prompt contains only the file path and asks exactly one child
to read it and return its exact value. The parent must return that value.

The verifier parses JSONL structurally and requires:

1. exactly one completed collaboration tool call;
2. a completed final `agent_message` whose text exactly equals the unpredictable
   marker;
3. a completed turn;
4. matching main/default model and effort configuration.

A fixture containing a collaboration event and the old fixed prompt marker but
not the runtime file marker must fail. Smoke output reports only bounded event
types, counts, and status; it does not print the delegated prompt or result.

### 5. Reasoning catalog and portal selection

The configurable portal list is fixed to:

```text
low, medium, high, xhigh, max
```

`minimal` is filtered even if a future upstream advertises it. For a selected
model, options absent from the live catalog are disabled. `none` remains an
internal compatibility state for models without verified reasoning controls,
not a user-selectable strength. When `medium` is verified it is the default;
otherwise the catalog's verified default is used, and configuration generation
must not invent a stronger value.

Catalog publication remains evidence-driven. A level is published only when
the selected route resolves `ReasoningOutput`, a control field, and an effort
map entry accepted by the current dialect profile. Capability probes may add
`xhigh` and `max` only after real upstream acceptance is observed.

For Chat fallback, capability mapping runs before generic compatibility
normalization. A configured canonical `xhigh` or `max` mapping must be allowed
to reach its upstream-specific value. Only an unmapped value uses the generic
fallback (`xhigh` or `max` to `high`; unknown or `none` omitted). Native
Responses routes preserve the requested canonical effort.

### 6. Runtime validation of domestic models

Validation temporarily enables one configured upstream/model route at a time,
uses the existing test downstream credential, and restores the original active
state after every probe. The matrix covers the configured GLM, DeepSeek, Kimi,
Qwen, and MiniMax slugs without hard-coding a concurrency limit.

Each model is tested serially for its advertised effort levels and for one V1
delegation. Failure updates capability evidence conservatively and leaves the
level disabled. Tests record only model slug, HTTP status class, bounded error
category, event type, and pass/fail state.

## Error handling

- Chat fallback plus encrypted task: safe 400 compatibility error, no upstream
  request, no content logging.
- Existing V2 session: same explicit error; operator starts a new session.
- Main/default profile mismatch: installed smoke fails as `agent_profile`.
- Catalog missing or malformed effort metadata: portal disables generation and
  smoke fails without guessing.
- Capability probe failure: the affected effort remains unpublished; ordinary
  model traffic and unrelated routes continue.

## Testing strategy

Implementation follows red-green-refactor in independent commits:

1. Add catalog tests requiring `multi_agent_version = "v1"` for every exposed
   Codex model, including unmatched allowlist entries.
2. Replace placeholder-positive protocol tests with failing tests that require
   a dedicated error for every encrypted shape while retaining plaintext
   `agent_message` conversion.
3. Add gateway tests proving encrypted input fails before the Chat upstream is
   hit and native Responses forwarding preserves the field.
4. Add portal/template tests for V1 guidance and synchronized role files.
5. Add smoke-script positive and negative fixtures using a runtime marker.
6. Add reasoning catalog, mapping-order, and five-option portal tests.
7. Run focused Rust/frontend/script tests, then all-target Rust tests, Redis
   serial tests, slow-stream/disconnect regressions, clippy, frontend tests,
   type checking, production build, and Compose validation.
8. Build the image before modifying the deployment directory. After deployment,
   run serial real Codex delegation against the configured domestic models and
   verify gateway health.

## Rollout

The repository changes are completed and verified before the Docker deployment
directory is touched. Deployment preserves existing credentials, volumes,
Redis prefix, and ports. Portal users regenerate `config.toml`,
`model-catalog.json`, and `agents/default.toml`, then start a new Codex session.
Rollback restores the previous image; no persisted schema change is required.
