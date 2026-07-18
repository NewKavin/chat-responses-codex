# Codex Reasoning Catalog Compatibility Design

## Problem

The gateway currently emits every Codex catalog entry with an empty
`supported_reasoning_levels` array, a `null` `default_reasoning_level`, and
`supports_reasoning_summaries: false`. The portal integration example separately
hard-codes `model_reasoning_effort = "high"`.

Codex CLI 0.144.5 accepts this JSON, so strict configuration validation does not
report an error. The model picker does not handle the metadata combination well:
it treats zero advertised efforts as a multi-step selection, converts the null
default to the single internal `none` effort, applies the model immediately, and
leaves the parent model picker waiting for a child picker completion. Repeated
Enter presses therefore produce repeated `Model changed to <model> default`
messages without closing the picker or presenting reasoning choices.

This is generated gateway metadata, not a syntax error in the user's
`~/.codex/config.toml`.

## Goals

- Make model selection complete normally in Codex CLI 0.144.5 and later.
- Expose selectable reasoning efforts only when the selected route has evidence
  that those controls are accepted.
- Keep models with unknown or unsupported reasoning controls usable without
  sending speculative upstream parameters.
- Make the portal's generated `config.toml` agree with its downloaded model
  catalog.
- Preserve the existing evidence-based route and capability model.

## Non-Goals

- Do not claim that reasoning output alone proves support for a configurable
  reasoning effort.
- Do not advertise `low`, `medium`, or `high` for every domestic model.
- Do not change Codex CLI source code or require a patched Codex binary.
- Do not change model probing prompts or synchronously probe models while a user
  downloads integration configuration.
- Do not edit a user's existing `~/.codex` files as part of deployment.

## Catalog Metadata Policy

Catalog metadata continues to come from the route selected by
`select_catalog_witness_entry`. The selected witness already contains the
resolved `reasoning_control_field`, canonical-to-upstream `effort_map`, reasoning
capability state, and reasoning carrier.

For each model, the gateway will derive a small reasoning metadata value before
constructing the Codex JSON entry.

### Verified configurable reasoning

A model advertises configurable reasoning only when all of these conditions are
true for the selected witness:

1. `ReasoningOutput` is supported.
2. `reasoning_control_field` is present.
3. `effort_map` contains at least one canonical Codex effort.

Advertised levels are the canonical keys accepted by the resolved effort map,
ordered using Codex's stable effort order (`minimal`, `low`, `medium`, `high`,
`xhigh`, `max`). Unknown custom keys follow in lexical order rather than being
dropped. Each level is emitted in Codex's required object form:

```json
{
  "effort": "medium",
  "description": "Balanced reasoning depth"
}
```

The default is `medium` when it is present. Otherwise the gateway chooses the
lowest advertised canonical effort, which is the conservative compatibility
default. `supports_reasoning_summaries` is enabled only for this verified branch
so Codex actually sends `reasoning.effort` on Responses requests.

### Unknown or unavailable controls

When the selected witness does not meet the verified conditions, the catalog
emits one explicit level:

```json
{
  "supported_reasoning_levels": [
    {
      "effort": "none",
      "description": "Do not request a configurable reasoning effort"
    }
  ],
  "default_reasoning_level": "none",
  "supports_reasoning_summaries": false
}
```

This single level is intentional. It prevents the Codex 0.144.5 zero-level
picker loop, keeps the model selectable, and preserves the current behavior of
omitting speculative reasoning controls.

## Portal Configuration

`CodexCatalogResponse` will expose typed fields needed to find a selected model's
default reasoning level. `buildCodexConfigToml` will receive that derived effort
instead of embedding `high`.

The portal chooses the effort from the primary model's
`default_reasoning_level`. If a future or malformed catalog omits it, the portal
uses `none`, matching the safe catalog fallback. It never guesses a stronger
effort from a model name.

The generated `model-catalog.json` remains the unmodified live catalog response,
so the config and catalog are produced from the same snapshot.

## Data Flow

1. Capability policy and asynchronous probes populate route profiles.
2. The resolver intersects configured effort mappings with accepted profile
   controls.
3. Catalog witness selection chooses the best executable route for a model.
4. The catalog metadata helper converts only the selected witness's resolved
   effort keys into Codex model metadata.
5. The portal downloads that catalog and derives the generated TOML default from
   the selected model entry.
6. Codex uses the advertised options in `/model`; the gateway maps a selected
   canonical effort through the same resolved route evidence during dispatch.

## Error Handling

- An empty or absent resolved effort map is not an error; it produces the single
  `none` fallback.
- Unsupported/custom canonical effort names are never allowed to make the
  catalog invalid. Non-empty names are serialized as Codex custom effort values.
- A catalog model whose default is absent or not a string makes the portal use
  `none`.
- Existing model visibility, context limits, witness ranking, and tool
  capability filtering remain unchanged.

## Testing

Backend tests will cover:

- unknown reasoning controls produce exactly one `none` level and default;
- a witness with verified mapped efforts advertises only those canonical keys;
- effort ordering and conservative default selection are deterministic;
- reasoning output without an accepted control still uses the `none` fallback;
- unrelated catalog metadata and witness selection remain unchanged.

Frontend tests will cover:

- generated TOML uses the selected catalog model's default effort;
- a missing/null default falls back to `none`;
- `high` is no longer hard-coded into the integration example;
- the downloaded live catalog remains unchanged.

Acceptance verification will use Codex CLI 0.144.5 with an isolated
`CODEX_HOME`: selecting an unknown-control model must close the picker after one
Enter, while a synthetic verified-control catalog must show its reasoning level
picker and persist the selected effort. No live model prompt is needed for this
TUI verification.

## Files In Scope

- `src/server/gateway.rs`: derive and serialize Codex reasoning metadata.
- `tests/gateway/capability_routing.rs`: backend catalog regressions.
- `frontend/src/utils/integration.ts`: typed catalog metadata and derived TOML
  effort.
- `frontend/src/views/portal/Integration.vue`: pass the selected model's catalog
  default into the TOML builder.
- `frontend/tests/utils/integration.spec.ts`: portal generation regressions.
- Codex integration documentation and templates that currently show a
  hard-coded reasoning effort, when those examples are generated or asserted by
  the affected tests.

## Deployment Compatibility

No database schema change is required. Existing capability profiles and
configuration remain valid. After deployment, users download a fresh
`model-catalog.json` and regenerate `config.toml`; existing catalogs continue to
parse but retain the Codex 0.144.5 picker behavior until replaced.
