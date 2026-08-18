# Portal Codex None Selection Design

Date: 2026-08-17

## Status

Approved through the user's standing instruction to apply the recommended
decision. The user explicitly requires `none` to appear first and to be
selectable when the live model catalog publishes it.

## Problem

The gateway already publishes manually configured `none` support in
`supported_reasoning_levels`, and the Codex configuration generators already
write the selected effort into both `~/.codex/config.toml` and
`~/.codex/agents/default.toml`. The portal uses a separate five-value constant,
however, so it filters `none` out while resolving the live catalog. A
`none`-only model therefore renders an empty disabled selector even though the
generated configuration internally falls back to `none`.

## Selected Design

Extend the portal Codex selection vocabulary to the same canonical order used
by capability discovery:

`none / low / medium / high / xhigh / max`

`none` remains catalog-gated like every other effort. When the selected model
publishes it, the option is enabled and appears first. A model that publishes
only `none` has an enabled selector containing that single usable choice. A
model with mixed levels keeps the existing default preference for `high`, but
the user can explicitly switch back to `none`.

The selected value continues through the existing computed state into both
Codex generators. Selecting `none` therefore renders and copies:

```toml
model_reasoning_effort = "none"
```

No backend metadata, route override, upstream protocol, or model mapping is
changed. The model catalog remains the source of truth for which options are
enabled.

## Alternatives Rejected

Omitting `model_reasoning_effort` for `none` is not selected because Codex can
then fall back to the catalog's non-none default, commonly `high`; omission is
not an explicit reset. Making `none` unconditionally available is also not
selected because it would bypass the portal's existing verified-capability
contract.

## Tests

Update the integration utility tests first so they fail until:

1. the canonical portal list starts with `none`;
2. a `none`-only catalog yields one enabled `none` option and remains
   configurable;
3. mixed catalogs expose `none` first while preserving `high` as the default;
4. parent and default-agent TOML both contain the explicitly selected `none`.

Run the focused Vitest file, the portal integration source test, frontend type
checking, and the production frontend build before deployment. Deployment
acceptance must inspect the rebuilt asset, gateway health, and fresh container
logs without mutating production capability data.

## Non-Goals

- Changing the backend's reasoning default policy.
- Adding `minimal`, `ultra`, or other effort values.
- Combining the four Codex setup artifacts into one clipboard payload.
- Modifying production probe results or manual reasoning overrides.
