# Client Compatibility Matrix Design

## Goal

Make the gateway easy to use with the main agent clients the user cares about:

- Codex
- Cline
- OpenCode
- Claude Code
- other mainstream OpenAI-compatible agents

The compatibility target is not "one config for every client". The target is:

1. the gateway exposes the right protocol surfaces
2. the portal generates correct copy-paste presets for each client family
3. the docs explain which clients use which surface
4. tests lock the generated presets and the protocol guarantees

## Current State

The repository already supports:

- OpenAI-compatible routes at `/v1/chat/completions`, `/v1/responses`, and `/v1/models`
- Anthropic-compatible routes at `/v1/messages` and `/v1/messages/count_tokens`
- portal presets for:
  - Codex
  - OpenCode
  - Claude Code

That means the backend already covers the main protocol families. The gap is mostly productization:

- Cline is not surfaced as a first-class preset
- the portal does not present a compatibility matrix
- the docs do not explicitly group clients by protocol family
- there is no single place that says "use this preset for this client"

## Design Principles

1. Keep the backend stable.
   - Do not add a client-specific protocol layer if the current OpenAI/Anthropic surfaces already work.
2. Make the portal the source of truth for copy-paste configs.
   - Users should not need to reverse-engineer whether a client wants OpenAI or Anthropic fields.
3. Prefer client-family presets over one-off client hacks.
   - OpenAI-compatible clients share one preset shape.
   - Anthropic-compatible clients share another preset shape.
4. Keep config generation deterministic.
   - The same inputs must always produce the same output text/JSON.

## Proposed Scope

### 1. Compatibility Matrix UI

Add a matrix or tabbed layout in the portal integration page that groups clients by protocol family:

- Codex
- OpenAI-compatible clients
  - Cline
  - OpenCode
  - other generic OpenAI-compatible tools
- Anthropic-compatible clients
  - Claude Code
  - other generic Anthropic-compatible tools

Each entry should clearly show:

- which gateway endpoint it uses
- which auth method it expects
- whether it uses `/v1/models`
- what the default model selection rule is

### 2. Preset Generators

Keep the current Codex/OpenCode/Claude Code generators, and add a named OpenAI-compatible preset for Cline.

The Cline preset should stay generic:

- base URL points at `<gateway_origin>/v1`
- auth uses the downstream key
- models come from live `/v1/models`

This makes Cline a first-class path in the portal without inventing a Cline-specific protocol shape.

### 3. Documentation Update

Update the main README and the integration guide so the recommended paths are obvious:

- Codex -> Codex preset
- Cline -> named OpenAI-compatible preset
- OpenCode -> OpenCode preset
- Claude Code -> Anthropic-compatible preset

The docs should also say:

- the gateway is OpenAI-compatible at the base surface
- the gateway also exposes Anthropic-compatible endpoints for Claude Code
- model names should come from the gateway's live `/v1/models`

### 4. Tests

Add or extend tests for:

- portal generator output for Cline
- existing Codex/OpenCode/Claude Code generators
- protocol routes that prove `/v1/models`, `/v1/chat/completions`, `/v1/responses`, and `/v1/messages` remain available

Tests should focus on contract shape, not screenshot-level UI detail.

## Architecture

### Backend

No new backend protocol surface is required for the first pass.

The existing contract is already sufficient:

- OpenAI-compatible clients use `/v1/chat/completions`, `/v1/responses`, and `/v1/models`
- Claude Code uses `/v1/messages`
- the gateway keeps the model routing and auth layer in one place

### Frontend

Extend the portal integration screen with a compatibility matrix, including a named Cline OpenAI-compatible preset.

The implementation should reuse the existing integration utility module instead of duplicating template logic inside the view.

### Docs

Keep the current Codex guide, but add a higher-level compatibility section that maps clients to protocol family and entry point.

## Behavior Details

### Codex

Codex keeps using:

- `~/.codex/config.toml`
- `~/.codex/model-catalog.json`
- `codex login --with-api-key`

Codex should continue to use `wire_api = "responses"` and the live gateway model catalog.

### OpenAI-Compatible Clients

Cline and other OpenAI-compatible clients should use:

- gateway base URL: `<gateway_origin>/v1`
- API key: the downstream key from the portal
- model list: the gateway's live `/v1/models`

The portal should present Cline as the named example for this preset, but the underlying configuration remains the shared OpenAI-compatible shape.

### Claude Code

Claude Code should continue to use:

- `ANTHROPIC_BASE_URL=<gateway_origin>/v1`
- downstream key as the API key/token
- gateway model discovery

If the chosen default model is not Claude-compatible, the portal should keep the custom model option path.

### OpenCode

OpenCode should continue to use its current provider block and the gateway's OpenAI-compatible base URL.

## Error Handling

The compatibility layer should fail closed:

- if the portal cannot fetch live models, it should show a clear "cannot generate config" state
- if a client preset cannot be derived, the UI should fall back to the generic compatible preset rather than inventing fields
- if a protocol route is unavailable, the docs should call that out explicitly instead of implying support

## Verification Plan

Minimum verification before merge:

- frontend build passes
- targeted frontend integration tests for generators pass
- backend protocol tests for `/v1/messages` and `/v1/models` still pass
- existing Codex/OpenCode/Claude Code preset tests still pass

## Non-Goals

- Adding a brand-new client-specific backend protocol
- Supporting non-standard proprietary client formats beyond the main compatibility families
- Changing upstream routing, model selection, or pricing logic
- Reworking auth storage or downstream key issuance

## Open Questions

None for the first pass. If a later client needs a unique file format or auth shape, add it as a follow-up preset only after the generic OpenAI-compatible path is proven insufficient.
