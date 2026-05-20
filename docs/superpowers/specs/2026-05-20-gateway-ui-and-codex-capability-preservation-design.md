# Gateway UI and Codex Capability Preservation Design

Date: 2026-05-20  
Status: Draft

## Context

`chat-responses-codex` is not just a conversion helper. It is the full gateway between Codex and multiple upstream model providers, with admin pages, downstream keys, routing, usage logging, and protocol translation.

The user goal is to improve the UI and keep Codex capability intact. That means this work must not accidentally weaken:

- model routing by slug
- `Chat Completions` and `Responses` conversion
- streaming event translation
- tool calling compatibility
- upstream alias mapping
- downstream key management and self-service portal behavior
- logs and operational visibility

This design locks those behaviors as non-negotiable.

## Goals

- Keep the current public routes and gateway behavior.
- Apply the selected UI direction A: a balanced control desk with clear hierarchy, tables, drawer-based editing, and strong operator readability.
- Keep Codex config examples and model catalog aligned with the gateway's routing contract.
- Make model aliases, upstream protocol type, and model availability visible in the UI.
- Keep downstream secret reveal/copy/rotate/delete flows intact.
- Keep portal and logs useful for day-to-day operations.
- Preserve current tool-call and streaming behavior unless a regression test proves an equivalent replacement.

## Non-Goals

- No SPA rewrite.
- No new frontend build system.
- No auth model redesign.
- No endpoint renames.
- No silent protocol narrowing.
- No deletion of legacy compatibility branches or example files unless they are proven dead and replaced by tests.

## Chosen UI Direction

Direction A is the target:

- light, warm background
- neutral card surfaces
- persistent left navigation
- compact top bar
- summary cards for key counts and usage
- dense but readable tables
- right-side drawer for create/edit/secret actions
- minimal decoration, but enough visual structure to feel like a real control desk

The shell should be reused across dashboard, upstreams, downstreams, logs, and portal so the admin area feels cohesive instead of stitched together.

## System Boundaries

### `src/protocol.rs`

Owns payload transformation only.

- Chat to Responses request conversion
- Responses to Chat request conversion
- Chat response to Responses response conversion
- Responses response to Chat response conversion
- streaming event translation for tool calls and text deltas

This module should not absorb routing, state, or UI responsibilities.

### `src/state.rs`

Owns runtime state and gateway policy.

- upstream and downstream configuration
- model alias resolution
- route selection
- request windows and usage logs
- upstream model discovery
- downstream key verification

### `src/server.rs`

Owns HTTP routes and HTML rendering.

- admin dashboard
- upstreams page
- downstreams page
- logs page
- portal page
- protocol dispatch

The server module can improve presentation, but it must not change the meaning of the protocol layer or routing layer without explicit tests.

### Codex Integration Files

These remain the canonical contract for Codex setup:

- `codex-config.toml.example`
- `codex-model-catalog.json`
- `gateway-state.example.json`

Any UI or docs change must keep these aligned with the active routing behavior.

## UI Decisions

### Dashboard

- show total upstreams and downstreams
- show active vs inactive state
- show recent usage and operational health
- keep the summary tiles compact and scannable

### Upstreams

- show protocol type
- show active models
- show alias mappings clearly
- keep `fetch current models` visible and usable
- keep edit/delete/toggle actions in place

### Downstreams

- show masked secret preview by default
- keep reveal, copy, rotate, delete, and toggle actions
- show allowlist, limits, expiry, and usage in the list context
- use a drawer instead of forcing a separate edit screen

### Logs

- show requested model and upstream model side by side
- show prompt, completion, total tokens, status, latency, and request id
- keep the page useful for debugging routing or compatibility issues

### Portal

- show what the downstream key can access
- show recent usage for that downstream
- keep the same shell language as the rest of the admin area

## Codex and Tool-Call Preservation

The gateway must continue to support Codex without making its working set smaller or less capable.

Requirements:

- preserve both chat-to-responses and responses-to-chat conversions
- preserve stream translation for tool calls and function calls
- preserve `model` and `review_model` slug handling in Codex-facing docs and examples
- preserve upstream model alias mapping so the on-wire model name can differ from the user-facing slug
- keep unsupported tool semantics explicit rather than silently changing behavior in a way that breaks current flows
- keep `Responses` and `Chat Completions` routing visible so users can see why a model is or is not compatible

If a future change wants to tighten tool semantics or alter model compatibility rules, it must come with regression tests first.

## Verification

The implementation must be verified with:

- protocol tests for request and response conversion
- gateway tests for end-to-end forwarding, alias routing, and stream translation
- admin tests for the shell, downstream secret actions, and upstream editing
- UI smoke checks for the dashboard, upstreams, downstreams, logs, and portal pages

If a change touches tool calling or streaming, the relevant tests must be updated before claiming success.

## Rollout

- No API renames.
- No required data migration.
- No new frontend build step.
- UI changes land behind the existing server-rendered routes.
- Preserve the current gateway contract while the shell is modernized.
