# Codex, OpenCode, And Portal Live Acceptance

## Deployment

- Verified at: `2026-07-17T10:46:29+08:00`
- Deployed commit: `868e023`
- Runtime and local-image binary SHA-256: `24b3738fa7cadb772d62244ce60e054cecc22a28cc152257ef85379703db37f1`
- Gateway container: running and healthy with restart count 0 after binary replacement
- PostgreSQL and Redis: retained existing containers and state
- Downstream credential digest: identical before and after installed-client smoke
- Exact clients: Codex `0.144.4`, OpenCode `1.17.18`

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
| `MiniMax-M2.7` | pass | pass after client retry | pass | pass | retained with operational note |
| `qwen3.6-plus` | pass | pass | pass | pass | retained |

All five focused models completed the substantive text and read-only tool tasks
with both installed clients. The one-hour acceptance window contained 13 HTTP
200 records for Kimi, 8 for GLM, 8 for DeepSeek, 8 for MiniMax, and 7 for Qwen.

MiniMax also produced one HTTP 502 `upstream_stream_error_event` on the Codex
Responses path after usable output had begun. The gateway did not replay that
committed stream; the client retried and completed. Qwen produced one HTTP 499
`stream_incomplete_close` on the OpenCode Chat Completions path after usable
output. This was a downstream close, not an upstream failure misclassified as
499, and the client task completed. No other focused error categories were
recorded, and no model was removed.

## Responses Compatibility Fix

- Required Responses usage defaults are present for cached and reasoning token
  details while existing upstream detail fields remain intact.
- Native Responses SSE preserves comments, event names, IDs, retry values,
  metadata-only frames, terminal frames, multi-line data, and CRLF delimiters.
- Native frames parsed from one upstream chunk are coalesced before downstream
  delivery to avoid an unnecessary chunking behavior change.

Verification:

- `protocol`: 74 passed
- `gateway`: 261 passed
- `compatibility_semantics`: 32 passed
- full Rust workspace: 908 passed, 3 ignored, 47 suites
- standalone `gateway-core`: 13 passed
- full rustfmt check: passed
- Clippy with all targets, all features, and warnings denied: passed
- `git diff --check`: passed

The previously failing
`stream_only_recovery_at_capacity_preserves_ordinary_candidate_fallback` test
was traced to repeated Argon2 validation of unchanged downstream keys during
upstream failure/success persistence. The request-path fix is deployed and the
original 2-second regression test now passes. Direct hash updates still clear a
stored plaintext value when it no longer matches the authoritative hash.

## First Semantic Event Recovery

- Successful SSE pass-through responses prefetch through the first semantic
  event with one replayable raw-chunk reader and one continuous watchdog.
- An initial upstream protocol error retries the same key once with upstream
  streaming disabled, then follows the existing key and candidate fallback
  policy if the JSON attempt fails.
- A normal first event commits the stream. Later errors remain structured 502
  events and are never replayed.
- Comments, CRLF delimiters, multi-line data, same-chunk trailing frames, and
  split frame delimiters are replayed byte-for-byte and exactly once.
- Cancellation during prefetch records one 499, releases both concurrency
  guards, starts no JSON retry, and does not change upstream health.

Controlled verification covered Chat and Responses recovery with exact upstream
attempt order `[stream, json]`, no retry after normal output, cancellation during
prefetch, and bounded candidate fallback `[first stream, first json, second
stream]`. The live common-model run did not encounter an initial error eligible
for this recovery path; its one MiniMax stream error occurred after usable
output and therefore correctly remained unrecovered.

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
