# Leptos Admin Architecture Design

Date: 2026-05-21  
Status: Draft

## Context

`chat-responses-codex` is a Rust gateway that translates Chat Completions and Responses traffic, routes requests to multiple upstream providers, manages admin credentials, and exposes operator pages for day-to-day control.

The current repository is a single Rust/axum application with server-rendered HTML in `src/server.rs`. That is working well for the gateway, but the admin UI is now large enough that future feature work will be easier if the UI layer becomes a clearly separate concern.

The design goal is not to move business logic into the browser. The design goal is to create a structure that makes the admin UI easier to extend while preserving the gateway's protocol handling, routing, rate limiting, and fallback behavior.

## Goals

- Keep the public gateway behavior and route semantics stable.
- Introduce Leptos only as the admin and portal UI layer.
- Preserve the existing Rust backend as the source of truth for protocol conversion, routing, and policy enforcement.
- Make future UI changes isolated, testable, and low risk.
- Keep deployment simple enough that the service can still run as one backend process behind one port.
- Make it easy to add new operator screens later without coupling them to the gateway core.

## Non-Goals

- No SPA rewrite.
- No new client-side application that owns gateway decisions.
- No migration of protocol translation into the browser.
- No migration of upstream selection, rate limiting, or fallback logic into Leptos.
- No requirement to publish a separate frontend service.

## Decision

Use a Rust workspace with two primary crates:

1. `gateway-core`
   - Owns protocol translation, routing, keys, state, and persistence.
   - Contains the gateway's domain rules and tests.
2. `gateway-web`
   - Owns HTTP routing, login/session handling, and Leptos SSR rendering.
   - Contains the admin pages, portal pages, and shared UI shell.

This is the recommended structure because it gives a clean seam between the stable gateway core and the evolving operator UI. The UI can change quickly without forcing protocol or routing churn.

## Deployment Shape

The workspace split is an internal code organization change, not a requirement for separate services.

- The production runtime should still be one backend process.
- The runtime binary should live in `gateway-web`.
- `gateway-core` should be consumed as a library crate by the runtime.
- The deployment should still expose one public port and one admin/gateway surface unless a future change explicitly introduces more.

## Target Architecture

### Crate Boundaries

| Crate | Responsibilities | Must Not Own |
| --- | --- | --- |
| `gateway-core` | request translation, model routing, upstream selection, rate limiting, persistence, key generation, usage accounting | HTML rendering, browser behavior, page layout |
| `gateway-web` | HTTP handlers, auth/session cookies, SSR pages, forms, minimal progressive enhancement | protocol semantics, routing policy, token accounting |

### Module Shape

The target layout should look roughly like this:

```text
workspace/
  Cargo.toml
  crates/
    gateway-core/
      src/
        lib.rs
        protocol.rs
        routing.rs
        keys.rs
        state.rs
        state/postgres.rs
        state/postgres_scram.rs
    gateway-web/
      src/
        main.rs
        lib.rs
        http/
          mod.rs
          auth.rs
          routes.rs
        web/
          mod.rs
          shell.rs
          shared.rs
          features/
            dashboard.rs
            upstreams.rs
            downstreams.rs
            logs.rs
            portal.rs
            login.rs
```

The exact filenames can vary, but the separation should not: core logic stays in one crate, UI stays in another.

### Boundary Rules

- Gateway decisions must stay in `gateway-core`.
- UI code must only observe core state through explicit view models.
- Page rendering must not directly implement routing policy or fallback policy.
- Admin forms may submit to ordinary HTTP handlers, but the validation and state mutation must be owned by the backend.
- Shared data types may be reused across crates, but browser-facing pages should not depend on internal implementation details that are irrelevant to rendering.

## Request and Rendering Flow

### Gateway API Flow

1. A downstream client calls `/v1/chat/completions`, `/v1/responses`, or `/v1/models`.
2. `gateway-web` authenticates the request and chooses the correct handler.
3. `gateway-core` resolves the model, selects an upstream, applies quotas and concurrency rules, and performs any necessary protocol conversion.
4. The response is streamed or returned to the client exactly as the gateway policy determines.

This flow should remain the authoritative path for all model traffic.

### Admin UI Flow

1. A browser requests `/admin`, `/admin/upstreams`, `/admin/downstreams`, `/admin/logs`, or `/portal`.
2. `gateway-web` builds a server-side view model from core state.
3. Leptos renders the page on the server.
4. Forms post back to the same backend process.
5. The backend mutates core state and redirects or re-renders with validation feedback.

This keeps the UI thin and makes the core behavior reusable for CLI, API, or future tooling.

## UI Component Model

The admin UI should be composed from reusable pieces instead of page-specific HTML blobs.

Recommended shared components:

- `AppShell`
- `NavRail`
- `TopBar`
- `SummaryCards`
- `DataTable`
- `StatusBadge`
- `ActionBar`
- `DrawerPanel`
- `FormField`
- `InlineHint`
- `SecretChip`
- `EmptyState`

Recommended page modules:

- `login`: admin login form and session bootstrap
- `dashboard`: service overview and counters
- `upstreams`: upstream list, edit/create, model mapping, quota controls
- `downstreams`: downstream list, secret handling, limits, expiry, allowlists
- `logs`: operational and audit logs
- `portal`: downstream self-service view

Each feature module should own its own form view model and validation helpers so that future pages can follow the same pattern.

## Migration Plan

### Phase 1: Extract the Core Boundary

- Freeze the semantics of `protocol`, `state`, `routing`, and `keys`.
- Move those modules into `gateway-core`.
- Keep tests for translation, routing, persistence, and rate limiting attached to the core crate.
- Do not change the gateway behavior during the move.

### Phase 2: Introduce `gateway-web`

- Create the backend crate that runs the HTTP server.
- Move route registration, login/session handling, and response rendering there.
- Keep the public routes unchanged.
- Continue serving the same gateway endpoints while the admin UI is reworked.

### Phase 3: Replace Server-String HTML With Leptos SSR

- Start with the login page and dashboard.
- Then migrate upstreams and downstreams.
- Then migrate logs and portal.
- Keep form submissions on the server.
- Add hydration only where it clearly improves operator experience.

### Phase 4: Remove Old Template Code

- Delete the old string-based HTML helpers after the Leptos pages fully replace them.
- Keep the gateway and persistence tests intact.
- Retain or update admin tests so they assert the new page structure and form behavior.

## Data Flow Rules

- Core data should be loaded and mutated by `gateway-core`.
- UI pages should receive view models, not raw internal state when a view model is enough.
- Secret values should only be exposed by backend-controlled render logic.
- Any model alias, quota, or limit configuration should still be validated server-side.
- Browser-side behavior must be optional and should never be required for correctness.

## Error Handling

- Validation errors should be handled by the backend and re-rendered in the same page context.
- Authorization failures should still produce the current redirect or unauthorized behavior.
- A page should be able to render useful error states without JavaScript.
- If Leptos hydration fails, the server-rendered HTML must still be usable.

## Testing Strategy

### Core Tests

- Keep protocol translation tests in the core crate.
- Keep gateway routing and quota tests in the core crate.
- Keep persistence round-trip tests in the core crate.
- Keep load and concurrency checks focused on gateway behavior, not UI behavior.

### Web Tests

- Add render checks for each SSR page.
- Verify login flow, redirect behavior, and session handling.
- Verify that admin forms still submit and update state correctly.
- Verify that the new shell preserves the operator workflow.

### Regression Rule

No migration step is complete unless the existing gateway endpoint tests still pass.

## Risks and Mitigations

### Risk: UI and Core Become Tightly Coupled Again

Mitigation:

- keep feature modules in `gateway-web`
- keep core types and policies in `gateway-core`
- use explicit view models and avoid leaking rendering concerns into core

### Risk: Leptos Adds Too Much Complexity Too Early

Mitigation:

- start with SSR only
- keep hydration optional and sparse
- avoid introducing a separate frontend build unless a later requirement justifies it

### Risk: Migration Breaks Gateway Semantics

Mitigation:

- preserve the current routes
- keep protocol tests untouched
- move UI first, not gateway logic

### Risk: Separate Crates Slow Down Small Changes

Mitigation:

- keep the public backend interfaces narrow
- group UI pages by feature
- prefer simple HTTP form posts over a large client-side state machine

## Success Criteria

The architecture is successful if all of the following are true:

- the gateway keeps its current protocol behavior
- the admin UI becomes easier to extend
- UI changes do not require changes to protocol logic
- the core crate can be tested independently
- the backend can still be deployed as one service on one port
- new operator pages can be added with minimal cross-cutting changes
