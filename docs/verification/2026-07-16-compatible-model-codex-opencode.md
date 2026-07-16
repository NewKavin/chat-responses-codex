# Codex, OpenCode, And Portal Live Acceptance

## Deployment

- Verified at: `2026-07-17T03:39:39+08:00`
- Deployed commit: `5a69acd`
- Runtime and local-image binary SHA-256: `80fb6f2d0528a39397575a5f719018db7ed2040830190b07760d25f3ef125f71`
- Gateway container: running and healthy after binary replacement
- PostgreSQL and Redis: retained existing containers and state
- Exact clients: Codex `0.144.0`, OpenCode `1.17.9`

## Qualification

- Active upstreams qualified: 3
- Retained models: 82
- Full models: 1
- Adapted models: 66
- Removed models: 39
- Operational failures retained for later retry: 15
- Final downstream catalog: 82 models

The domestic-model upstream retained 19 models: 1 full, 17 adapted, 1
operational failure, and 0 removed. The focused live matrix followed the user
scope and tested one commonly used model from each requested family rather than
rerunning all 82 models.

## Focused Client Matrix

| Model | Codex text | Codex read tool | OpenCode text | OpenCode read tool | Result |
| --- | --- | --- | --- | --- | --- |
| `kimi-k2.5` | pass | pass | pass | pass | retained |
| `glm-5.2` | pass | pass | pass | pass | retained |
| `deepseek-v4-flash` | pass | pass | pass | pass | retained |
| `MiniMax-M2.7` | pass after client retry | pass after client retry | pass | pass after client retry | retained with operational note |
| `qwen3.6-plus` | pass | pass | pass | pass | retained |

The original `kimi-k2.5` Codex failure was reproduced before the fix as a
strict Responses usage parse failure. After deployment, both Codex tasks
completed without reconnect exhaustion. The 10-minute Kimi log window contained
11 HTTP 200 records, no error categories, and no 499 records.

GLM, DeepSeek, and Qwen produced only HTTP 200 records in the focused window.
MiniMax produced 7 HTTP 200 records and 3 HTTP 502 records classified as
`upstream_stream_error_event`; no 499 was recorded. Both installed clients
recovered and completed their tasks. A low-load direct tool probe and four
concurrent direct tool probes all returned HTTP 200 with a tool delta and a
terminal event, so the model remains usable and was not removed. The observed
502s are retained as transient upstream operational evidence, not treated as a
protocol incompatibility.

## Responses Compatibility Fix

- Required Responses usage defaults are present for cached and reasoning token
  details while existing upstream detail fields remain intact.
- Native Responses SSE preserves comments, event names, IDs, retry values,
  metadata-only frames, terminal frames, multi-line data, and CRLF delimiters.
- Native frames parsed from one upstream chunk are coalesced before downstream
  delivery to avoid an unnecessary chunking behavior change.

Verification:

- `protocol`: 74 passed
- `gateway`: 255 passed
- full Rust workspace: 903 passed, 3 ignored, 47 suites
- standalone `gateway-core`: 8 passed
- touched-file rustfmt check: passed
- `git diff --check`: passed

The previously failing
`stream_only_recovery_at_capacity_preserves_ordinary_candidate_fallback` test
was traced to repeated Argon2 validation of unchanged downstream keys during
upstream failure/success persistence. The request-path fix is deployed and the
original 2-second regression test now passes. Direct hash updates still clear a
stored plaintext value when it no longer matches the authoritative hash.

## Portal Playground

- Non-mutating API E2E: HTTP 200, meaningful stream frames present, terminal
  event present, and no error category.
- Downstream credential digest was identical before and after the E2E.
- Browser E2E with `glm-5.2`: login passed, playground route passed, live model
  loaded, request completed, assistant message rendered, and no page errors.
- Browser E2E with the default `MiniMax-M2.7` selection observed one transient
  upstream stream error consistent with the MiniMax operational evidence above.
- Mobile audit at 375 px: the fixed 220 px portal navigation leaves about 99 px
  for the playground, confirming a responsive-layout defect for the UI track.

Static review also found that Markdown output is rendered through `v-html`
without sanitization and that stream completion does not explicitly flush its
`TextDecoder`. These findings require a separate reviewed UI design and are not
silently mixed into the protocol compatibility commit.

## Safety

- No downstream or upstream credentials were written to this document.
- No request text, response text, reasoning text, or tool arguments/results were
  retained.
- Temporary diagnostic files were permission-restricted or removed after use.
