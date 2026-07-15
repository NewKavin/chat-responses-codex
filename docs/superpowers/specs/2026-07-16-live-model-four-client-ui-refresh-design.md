# Live Model, Four-Client, And UI Refresh Design

## Summary

This iteration turns the existing compatibility beta into an auditable local
deployment result. It configures every model that can complete a real minimal
inference through a configured upstream, exercises those models through the
current downstream with Codex, OpenCode, Claude Code, and Hermes, removes the
portal troubleshooting product surface, and refreshes the shared console shell.

The protocol-fidelity design from 2026-07-10 remains authoritative for deeper
adapter work. This iteration does not claim that listing a model proves it is
runnable, and it does not mark a client compatible from an HTTP 200 alone.

## Product Decisions

- The four downstream clients are Codex, OpenCode, Claude Code, and Hermes.
- The admin troubleshooting center and compatibility matrix remain. They are
  the operational surface used to validate the four clients.
- The portal troubleshooting page, navigation item, API client methods, public
  backend routes, wrapper handlers, and portal-only tests are removed.
- The UI follows mature relay-console conventions represented by new-api and
  sub2api: compact navigation, neutral surfaces, dense tables, one restrained
  teal accent, clear state colors, and responsive layouts.
- The UI must not use gradients, glowing decoration, glass effects, oversized
  marketing headings, floating section cards, or purple/blue AI-style visuals.

## Live Model Qualification

Model qualification has two distinct phases:

1. Discovery calls the configured upstream's `/v1/models` endpoint per active
   key and records the advertised model slugs.
2. Execution sends a bounded, non-streaming text inference to the exact route
   and model. A model qualifies only when the request returns a successful,
   parseable protocol response with non-empty text or reasoning output.

The probe uses the upstream's configured protocol. Chat Completions routes use
`/v1/chat/completions`; Responses routes use `/v1/responses`. A route advertised
under both protocols is checked under both. Authentication, quota, timeout,
5xx, malformed responses, and empty model output remain explicit failures.

The resulting configuration is the union of successful exact model slugs for
each upstream. Per-key model mappings retain only models proven on that key.
Failed keys or failed model/protocol pairs are not silently treated as valid.
The evidence report stores only upstream ID, model slug, protocol, status,
latency, timestamp, and sanitized error category. It never stores keys,
prompts, response text, or raw error bodies.

## Four-Client Validation

The admin compatibility matrix defaults to this fixed client order:

1. Codex, using `/v1/responses`
2. OpenCode, using `/v1/chat/completions`
3. Claude Code, using `/v1/messages` and `/v1/messages/count_tokens`
4. Hermes, using `/v1/chat/completions`

When no model list is supplied, the matrix enumerates every model exposed by
the selected downstream. A cell passes only when the profile's required checks
pass. Claude Code is no longer rejected by matrix validation.

The deterministic matrix is followed by installed-client smoke tests. Each
real CLI is configured with only the gateway URL, current downstream key, and
an exposed model slug. Each performs one text task and one safe read-only task
where the client exposes a stable non-interactive workflow. Exact installed
versions, exit status, duration, and sanitized event types are recorded.

## Portal Troubleshooting Removal

Remove the complete portal-only surface:

- `/portal/troubleshooting` router child and sidebar/title entries
- `views/portal/Troubleshooting.vue`
- `portalApi.runTroubleshooting` and `getActiveTroubleshootingRequests`
- `/api/portal/troubleshooting/run` and
  `/api/portal/troubleshooting/active-requests`
- portal-only wrapper handlers and downstream-key extraction helper when it has
  no remaining callers
- portal-only frontend and Rust tests

Retain shared troubleshooting types, validators, runtime route capture, the
admin page, admin APIs, the admin compatibility matrix, and the shared runner.
Removed portal endpoints return 404.

## UI Direction

### Shared shell

Admin and portal shells use a 216-pixel desktop sidebar and a 56-pixel topbar.
The sidebar is a light neutral surface with grouped navigation, small icons,
quiet labels, and a teal active indicator. The topbar contains the current page
title and compact account/context actions. Main content uses a stable maximum
width where appropriate and predictable 20-24 pixel gutters.

Mobile layouts replace the fixed sidebar with a drawer opened by a menu icon.
Tables retain their desktop structure at wide widths and use horizontal scroll
or existing compact content at narrow widths; navigation never consumes most
of the viewport.

### Visual tokens

- Canvas: `#f6f7f8`
- Surface: `#ffffff`
- Strong text: `#17201d`
- Muted text: `#66716d`
- Border: `#dfe5e2`
- Accent: `#0f8f76`
- Accent soft surface: `#eaf6f2`
- Danger and warning retain conventional red and amber roles
- Card radius is 6-8 pixels; controls use established Element Plus sizing
- Shadows are reserved for drawers, dialogs, and menus, not ordinary sections

Create one global stylesheet for tokens and Element Plus refinements. Views
keep business-specific layout rules but consume shared tokens. The first pass
updates the admin shell, portal shell, both login screens, and common card,
table, form, drawer, and button treatments. It does not rewrite dashboard
charts or page business logic.

## Error Handling

- A failed discovery or inference probe cannot add a model to the live set.
- A matrix cell preserves upstream authentication, quota, availability,
  converter, and model-semantic categories.
- CLI installation or version mismatch is reported separately from gateway
  compatibility.
- UI API failures retain existing Element Plus error messaging and must not
  replace dense operational content with decorative empty states.

## Verification

- Rust tests prove the portal routes are gone and the admin routes remain.
- Frontend tests prove the portal route and API methods are gone while the
  admin troubleshooting route remains.
- Matrix tests prove the default order contains all four clients and Claude
  Code is accepted.
- Model qualification tests use local mock upstreams to prove advertised but
  failing models are excluded and successful per-key mappings are retained.
- Live evidence covers every configured active upstream and every model exposed
  by the selected downstream.
- Real Codex, OpenCode, Claude Code, and Hermes smoke commands exit zero.
- Frontend unit tests, type checking, production build, Rust formatting,
  targeted suites, and the full workspace test suite pass.
- Desktop and mobile screenshots verify the refreshed UI has no overlap,
  clipped navigation, gradients, or blank content.

## Acceptance Criteria

1. Every model retained in the live upstream configuration has a current real
   inference pass, and every advertised-but-failing model is excluded or
   explicitly reported as failed.
2. The current downstream exposes the qualified model union.
3. The four-client matrix covers every exposed model with no missing client
   profile.
4. All four installed clients complete the required smoke workflow through the
   current downstream.
5. The portal troubleshooting route, UI, API, and backend surface no longer
   exist; admin troubleshooting remains usable.
6. The refreshed admin and portal shells are responsive, operationally dense,
   visually consistent, and free of AI-style decoration.
7. Verification evidence is sanitized and the completed changes are committed
   directly on `main`.
