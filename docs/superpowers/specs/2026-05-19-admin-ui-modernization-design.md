# Admin UI Modernization Design

Date: 2026-05-19  
Status: Draft

## Context

`chat2responses-gateway` currently serves its admin UI directly from the Rust/axum app in `src/server.rs`. The existing interface is functional, but it is visually closer to a utility panel than a modern control plane.

The target look and interaction model is a SaaS-style admin console similar to the screenshot the user provided:

- left navigation rail
- compact top bar with app/page context
- card-based summary strip
- soft background gradients and rounded surfaces
- table-centric management views
- action-heavy rows with copy, view, edit, regenerate, and delete controls

The immediate priority is the admin shell and the downstream key page. The final target is that the same shell and visual language are reused across the entire admin area.

## Goals

- Replace the current admin chrome with a modern control-panel layout.
- Keep the implementation server-rendered; do not introduce a separate frontend build system.
- Make downstream records fully manageable in place from the list page.
- Persist downstream plaintext secrets so they can be viewed and copied later.
- Support secret regeneration without editing the secret directly.
- Support deleting an entire downstream record.
- Allow downstream records to have no expiry time.
- Reuse the same shell and design system across dashboard, upstreams, downstreams, logs, and portal.

## Non-Goals

- No React/Vite/SPA rewrite.
- No change to the upstream routing logic beyond what is needed for the admin UI.
- No encryption-at-rest scheme for stored plaintext secrets in this task.
- No redesign of the public API surface.

## Delivery Shape

This work ships in two phases:

1. Phase 1 modernizes the admin shell and fully reworks the downstream key page.
2. Phase 2 applies the same shell, spacing, typography, and component language to the rest of the admin pages so the whole area feels consistent.

The UI should look complete after Phase 1 on the downstream page, but the final architecture is the unified Phase 2 shape.

## Information Architecture

### Global Shell

The admin area uses a persistent two-column layout:

- left sidebar for navigation
- top bar for page title, subtitle, and quick status
- main content area with summary cards, tables, and forms

The shell is reused across all admin pages. The sidebar highlights the active section.

### Downstream Page

The downstream page becomes the most interactive management screen:

- list/table view for all downstream records
- toolbar with search and status filters
- create button in the page header
- right-side drawer for create/edit details
- row actions for view/copy/edit
- destructive actions for regenerate and delete

The user never leaves the list context to edit a downstream record.

## Data Model

### DownstreamConfig

The downstream record needs to store both the auth hash and the plaintext secret:

- `hash` remains the verification source for gateway requests.
- `plaintext_key` stores the current plaintext secret for later viewing and copying.
- `plaintext_key` is optional during migration so older persisted states can still deserialize.

Recommended shape:

- `plaintext_key: Option<String>`
- `#[serde(default)]` on the field so legacy JSON loads cleanly

Behavior:

- New downstream keys always store both `hash` and `plaintext_key`.
- Editing metadata does not change the secret.
- Regenerating a secret replaces both `hash` and `plaintext_key`.
- Legacy records with no plaintext secret remain usable, but the UI should show them as migrated/legacy until they are regenerated.

### Expiry Semantics

`expires_at: Option<u64>` already supports unlimited lifetime:

- `None` means the record never expires.
- `Some(unix_seconds)` means the record expires at that timestamp.

The UI should present this as a clear “永不过期” choice instead of requiring a Unix timestamp in the common case.

## UI Behavior

### Admin Shell

The shell should visually match the provided screenshot:

- pale or light-toned content area with a subtle gradient background
- rounded cards and tables
- small pill badges for status/model/metadata
- compact top bar with app title and current section
- left nav with clear active state
- responsive behavior so the sidebar collapses gracefully on narrow screens

The shell should be implemented in the existing server-rendered HTML, with inline CSS and only minimal vanilla JS where necessary.

### Downstream List

The downstream list should show:

- name
- masked secret preview
- supported models
- limits
- expiry state
- active/inactive state
- actions

The secret preview is hidden by default and rendered as a masked chip. The row provides:

- `查看` to reveal the secret in place
- `复制` to copy the plaintext secret
- `编辑` to open the drawer

If a record has no persisted plaintext secret because it predates the migration, the UI should indicate that it is legacy and offer regeneration.

### Right Drawer

Create/edit should happen in a right-side drawer or side panel that stays on the list page.

Drawer contents:

- record name
- model allowlist
- per-minute limit
- daily token limit
- monthly token limit
- IP allowlist
- expiry mode and timestamp, with “永不过期” as the default friendly choice
- active toggle
- secret section

Secret section rules:

- the secret is read-only
- the default presentation is masked
- the user can reveal it
- the user can copy it
- the user can regenerate it

Delete action:

- the drawer includes a destructive delete action
- the user must confirm before deleting
- deleting removes the entire record from persisted state

Regenerate action:

- regenerating creates a fresh plaintext secret
- the new secret immediately replaces the old one
- the old secret is no longer valid
- the drawer should stay open and show the new secret after regeneration

### Search and Filter

The downstream list should include a simple toolbar:

- search by name or secret fragment
- filter by active/inactive state
- optional quick filter for unlimited vs expiring records

This can be server-rendered query state; no client-side table library is required.

## Routes and Flows

The implementation can keep the current router style and extend it with dedicated downstream management actions:

- `GET /admin/downstreams`
- `GET /admin/downstreams/new`
- `GET /admin/downstreams/{id}/edit`
- `POST /admin/downstreams`
- `POST /admin/downstreams/{id}`
- `POST /admin/downstreams/{id}/rotate`
- `POST /admin/downstreams/{id}/delete`
- `POST /admin/downstreams/{id}/toggle`

Recommended flow:

1. `GET /admin/downstreams` renders the list and empty state.
2. `GET /admin/downstreams/new` renders the same page with the create drawer open.
3. `GET /admin/downstreams/{id}/edit` renders the same page with the edit drawer open.
4. `POST /admin/downstreams` creates a new record and stores both secret and hash.
5. `POST /admin/downstreams/{id}` updates metadata only.
6. `POST /admin/downstreams/{id}/rotate` regenerates the secret and hash.
7. `POST /admin/downstreams/{id}/delete` removes the record entirely.

The page should return HTML responses after each action so the user stays in the same visual flow.

## Component Breakdown

### AdminChrome

Responsible for the persistent shell:

- sidebar navigation
- top bar
- content padding and background
- responsive layout behavior

### SummaryCards

Responsible for the headline metrics shown at the top of the page:

- total downstreams
- active downstreams
- unlimited downstreams
- expiring downstreams

### DownstreamTable

Responsible for the list view:

- row rendering
- masked secret chips
- status pills
- row actions

### DownstreamDrawer

Responsible for create/edit:

- the form fields
- the secret display and copy/reveal controls
- regenerate and delete actions
- validation messages and notices

### SecretControls

Responsible for secret-specific interaction:

- masked default display
- reveal toggle
- copy to clipboard
- regenerate confirmation

## Migration Plan

The migration needs to preserve older persisted states:

- adding `plaintext_key: Option<String>` must not break JSON deserialization
- older records with only `hash` should still load
- those legacy records should remain authenticatable by hash
- the UI should make it clear that legacy records need regeneration before the plaintext can be viewed/copied later

When a legacy record is edited and saved, the plaintext secret should remain `None` unless the user explicitly regenerates it.

## Error Handling

The UI should surface clear, localized notices for:

- invalid form input
- missing or unknown downstream record
- duplicate or invalid models list input
- failed secret regeneration
- failed delete/update persistence

Important rule:

- plaintext secrets must not be written into tracing logs or error logs
- the secret may appear in the HTML UI and persisted JSON as requested, but it should not leak into runtime logs

## Testing Strategy

Add or extend tests for:

- loading legacy persisted state that lacks `plaintext_key`
- creating a downstream record and persisting both hash and plaintext secret
- editing downstream metadata without changing the secret
- regenerating a secret and verifying the old secret is replaced
- deleting a downstream record
- unlimited expiry handling when `expires_at` is omitted
- HTML rendering for the masked secret chip and drawer actions

Recommended request-level coverage:

- create downstream
- edit downstream
- rotate downstream secret
- delete downstream
- verify the list page still renders correctly after each operation

## Acceptance Criteria

- The admin area uses a modern console-style shell instead of the current plain utility look.
- The downstream page has a right-side drawer for create/edit and does not force the user out of the list.
- Downstream records support full metadata editing.
- The secret is stored in plaintext for later view/copy use, while the hash remains the auth source.
- The secret is hidden by default, but viewable and copyable.
- Secret regeneration works and replaces the persisted secret.
- Delete removes the whole downstream record.
- Records can be configured to never expire.
- The final shell and visual language can be reused across all admin pages.

