# Configurable None Reasoning Effort Design

Date: 2026-08-17

## Status

Approved by the user. The user accepted the recommended representation and
explicitly required `none` to be the first item in every canonical ordering.

## Goal

Make `none` a first-class manually configurable reasoning effort without
changing the existing meaning of an empty selection or weakening exact-route
capability semantics. Make model-summary editing the primary batch workflow so
an operator does not have to repeat the same selection route by route.

The canonical configurable vocabulary is:

`none / low / medium / high / xhigh / max`

The official OpenAI Responses reasoning guide documents `none` as one of the
model-dependent values accepted by `reasoning.effort`. Availability remains
model-dependent, so this feature is an operator declaration for a specific
route rather than a claim that every upstream model supports `none`.

## Selected Representation

`none` is an ordinary, non-empty member of a route's supported effort set. It
may be selected alone or together with any other configured effort. A managed
route override stores it through the existing fields:

```json
{
  "reasoning_control_field": "reasoning_effort",
  "effort_map": {
    "none": "none"
  }
}
```

No boolean disable flag, sentinel object, or new capability field is added.
The existing request adapter already reads the selected canonical effort from
`effort_map` and writes the mapped value to the configured upstream field, so
`none` follows the same data path as every other effort.

An empty `levels` array keeps its current administrative meaning: remove the
admin-managed override for the selected scope. It never means “configure
`none`.” This distinction remains visible in the UI because `none` is a
checkbox while clearing is a separate action.

## Backend API And Persistence

`PUT /api/admin/capabilities/reasoning-overrides` accepts all six canonical
values. It rejects any value outside that set atomically with the existing
error code `capability_reasoning_override_invalid_level`. The validation
message lists the complete accepted vocabulary in canonical order:

`levels must contain only none, low, medium, high, xhigh, or max`

Normalization removes duplicates and emits values in this fixed order:

`none`, `low`, `medium`, `high`, `xhigh`, `max`

For a non-empty selection, the managed override continues to mark
`ReasoningOutput` supported and maps each selected canonical value to the same
JSON string. Selecting only `none` therefore persists an override; it does not
enter the clear branch. Route identity validation, scope expansion,
configuration compilation, persistence, revision handling, and hot ArcSwap
replacement are unchanged.

## Discovery And Codex Catalog

Capability discovery retains, sorts, and deduplicates `none` in route-level
`accepted_reasoning_levels` and model-level `verified_reasoning_levels`. The
model union contains one `none` even when multiple current routes declare it.
Probe evidence and manual declarations remain distinguishable through the
existing `reasoning_source` and `managed_reasoning_override` fields.

The Codex catalog uses the same six-value ordering. A configured `none` entry
is emitted once and before all non-`none` entries. Default selection follows
these rules:

1. use `high` when it is available;
2. otherwise use the first available non-`none` effort in canonical order;
3. use `none` when it is the only effective effort;
4. retain the conservative singleton `none` fallback when no effective effort
   metadata exists.

This preserves existing defaults when an operator adds `none` as an optional
choice. `supports_reasoning_summaries` is true only when at least one effective
non-`none` effort exists. A route configured with only `none`, like the
conservative fallback, does not advertise reasoning summaries.

## Frontend

The `ReasoningEffortLevel` type and the shared
`REASONING_EFFORT_LEVELS` constant gain `none` as their first member. The
existing checkbox group renders directly from that constant, so operators can
select `none` alone or alongside other efforts. Normalization preserves the
six-value canonical order and removes unknown or duplicate values.

The save button remains disabled only when the selection is empty. Selecting
`none` enables save. The separate “clear manual setting” action continues to
send an empty array, preserving the difference between a configured `none`
and no managed override.

### Model-summary batch editing

The model-summary table gains an edit action. Opening it uses the model's
current union of effective levels as the initial selection, shows the number
of current exact routes that will be affected, and fixes the scope to
`model_routes`. Saving is an explicit whole-set replacement: every current
route for that exposed model receives the selected set. This is the primary
workflow for operators who want one reasoning vocabulary across all upstream
keys and protocols for a model.

The frontend reuses one current route as the request identity required by the
existing API. The server still resolves and validates that identity before it
atomically enumerates all current model routes. If the representative route
became stale between discovery and save, the existing stable 4xx response is
shown and discovery is refreshed; the client does not silently retry against
another route.

The exact-route table retains its edit action for intentional per-route
differences. A dialog opened from an exact route defaults to `route`; a dialog
opened from model summary is fixed to `model_routes` and does not show
representative-route upstream or protocol details as though they described
the whole model. Model-summary clear removes admin-managed overrides from all
current model routes and leaves probe and preset evidence intact.

## Errors And Compatibility

- Unknown levels fail before any route or configuration mutation.
- Existing stored five-level overrides deserialize unchanged.
- Existing clients that never send `none` observe the same behavior and
  ordering for their selected levels.
- The API response shape, persistence schema, discovery schema, and Codex
  catalog schema do not change.
- Upstream acceptance is operator-owned. Configuring `none` does not rewrite
  probe evidence and does not assert universal model support.

## Test-Driven Implementation

Implementation follows red-green TDD in these focused slices:

1. Extend the admin override integration test first so an unsorted,
   duplicate-containing selection with `none` must persist
   `"none": "none"` and return `none` first in discovery. Verify the test
   fails before changing the backend allowlist.
2. Add an invalid-level assertion for the complete six-level error message,
   then update backend validation and normalization.
3. Add discovery and Codex catalog expectations for deduplication, canonical
   ordering, default selection, and summary support with both mixed efforts
   and `none` alone. Verify failures before extending the canonical catalog
   vocabulary and metadata rules.
4. Update the frontend utility test first to require
   `none / low / medium / high / xhigh / max` and mixed-input normalization.
   Verify failure before changing the TypeScript union and shared constant.
5. Extend the admin UI source test to require `none` in the shared selector
   vocabulary, a model-summary batch edit action fixed to `model_routes`, an
   affected-route count, an exact-route edit path, and the separate empty-array
   clear path. Verify the source test fails before editing the Vue component.
6. Run focused Rust and frontend tests, then the broader Rust suite, frontend
   unit tests, TypeScript checks, and production builds before deployment.

## Deployment And Observation

Deployment uses the existing project release path only after verification.
Post-deploy checks must confirm health, open the capability discovery endpoint,
and verify that a non-destructive managed override can expose `none` in the
admin discovery response and Codex catalog without a restart. Production
upstream protocols and unrelated route configuration are not changed for this
verification.

## Non-Goals

- Adding `minimal` or any other new configurable effort.
- Automatically probing `none` on every route.
- Treating a manual `none` declaration as probe verification.
- Adding a separately configurable Codex default effort.
- Changing the meaning of an empty level set.
- Refactoring the broader capability resolver or request adapter.
