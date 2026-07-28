# Portal Codex Recommendation Optimization Design

## Goal

Make the portal's generated Codex setup safer and better matched to the deployed
GLM capacity, while keeping existing Codex clients and standard OpenAI-compatible
model discovery working unchanged.

The same change also corrects the initial usage-chart ranges requested for the
portal and administrator views.

## Decisions

- Codex catalog compaction begins at 80% of the model context window instead of
  95%.
- The portal requests the Codex catalog with `format=codex`, not a hard-coded
  Codex version.
- Real Codex requests containing `client_version` remain supported.
- Generated Codex configuration uses `max_threads = 4`, `max_depth = 2`, and
  `stream_max_retries = 8`.
- The portal lets the user select the model used for `model` and `review_model`.
- The generated login command prompts for the downstream key instead of
  embedding the plaintext key in shell history.
- The portal token usage chart defaults to 7 days.
- The administrator dashboard defaults to 1 day.

## Codex Catalog Request

Extend the `/v1/models` query contract with an optional `format` field.

- `format=codex` returns the existing Codex `{"models": [...]}` catalog.
- Any present `client_version` also returns that catalog for backward
  compatibility with Codex clients.
- Requests without either selector keep the standard OpenAI-compatible
  `{"object":"list","data":[...]}` response.
- Unknown `format` values do not opt into Codex output.

The portal will request `/v1/models?format=codex`. No browser-side Codex version
guessing or version input is introduced because the backend catalog is not
currently version-dependent.

## Context Compaction

Set each generated Codex model catalog entry's
`effective_context_window_percent` to 80. This remains model-relative, so
switching between models with different context windows continues to use the
correct absolute threshold. Do not add a global
`model_auto_compact_token_limit`, which would override that model-relative
behavior.

Portal copy, templates, guides, and assertions that describe 95% compaction
will be updated to 80%.

## Portal Codex Configuration

Add a model selector to the Codex integration section using the live,
allowlist-filtered model list already available in the page state.

- Initial selection remains the first usage-ranked model to preserve current
  behavior.
- The UI labels that default as the historically most-used model rather than an
  unqualified recommendation.
- Selecting another model immediately updates `model`, `review_model`, and
  `model_reasoning_effort` from that model's live catalog metadata.
- The full sanitized live catalog remains unchanged and includes all allowed
  models.
- The selection is page-local and is not persisted to gateway state or browser
  storage.

Generated defaults become:

```toml
[agents]
max_threads = 4
max_depth = 2

[model_providers.gateway]
stream_max_retries = 8
```

The retry count remains eight as requested. Reducing agent concurrency aligns
the recommendation with the deployed upstream concurrency of four without
changing gateway quota enforcement.

## Secure Login Command

Generate a shell snippet that reads the downstream key without echoing it,
passes it to `codex login --with-api-key`, and unsets the temporary variable:

```bash
read -rsp 'Gateway downstream key: ' CHAT2RESPONSES_DOWNSTREAM_KEY
printf '\n'
printf '%s' "$CHAT2RESPONSES_DOWNSTREAM_KEY" | codex login --with-api-key
unset CHAT2RESPONSES_DOWNSTREAM_KEY
```

The portal key remains necessary for fetching the live catalog, but its value
will no longer appear in the generated command.

## Usage Chart Defaults

- Initialize the portal usage-history range to `7d`.
- Initialize the administrator dashboard range and its empty analytics state to
  `1d`.
- Preserve all existing range controls, custom filters, API parameters, and
  user-triggered refresh behavior.

## Error Handling

- A missing or empty Codex catalog continues to block generated configuration.
- Model selection falls back to the current usage-ranked primary model when no
  valid explicit selection exists.
- If catalog refresh removes the selected model, selection returns to the new
  usage-ranked primary model.
- Standard `/v1/models` callers remain unaffected by the new `format` query.

## Testing

Implementation will follow test-driven development.

1. Add backend tests that fail until `format=codex` works, unknown formats stay
   standard, `client_version` remains compatible, and catalog entries report
   80%.
2. Update frontend generator tests first for `4/2/8`, secure login output, model
   selection, selected-model reasoning metadata, and the absence of a literal
   portal key in the command.
3. Update portal view tests first to require `format=codex`, reject the fixed
   `0.144.6` query, and cover the model selector.
4. Add or update view tests for portal `7d` and administrator `1d` defaults.
5. Update template and documentation consistency tests for the new defaults and
   80% copy.
6. Run targeted frontend and backend suites, then full frontend build, Rust
   tests, strict Clippy, release build, deployment, and live smoke tests.

## Non-Goals

- No change to GLM routing, tool adaptation, hedging, or retry implementation.
- No persistence of a user's portal model selection.
- No removal of `client_version` support.
- No automatic Codex CLI upgrade or mutation of the user's local Codex config.
- No change to administrator log-filter defaults outside the dashboard chart.
