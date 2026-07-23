# Domestic Model Stream Stability Verification

Verification date: 2026-07-23

## Candidate

- Branch: `fix/domestic-stream-stability`
- Commits: `563f701`, `eec1fb5`, `d061db5`
- Image: `chat2responses-candidate:d061db5`
- Image ID/digest:
  `sha256:a170ec7872e3a3e1c064e6faa87a24c5f820e2d8b6341a11a475d73717a9e51f`
- Export: `/tmp/chat2responses-candidate-d061db5.tar`
- Export SHA-256:
  `8f7512280e9c92c3c9f872842c3a2a936a64a12b13b6c8d59b1bab1df4f0e27a`
- Container binary SHA-256:
  `a2e54b3b4eb920beab10a64d8c69c32b4f4c21fb0a62174b5c393712d5f57985`
- Isolated token-probe gateway: `http://127.0.0.1:3381`
- Client: Codex `0.144.6`, Responses wire API, `stream_max_retries = 0`
- The package image was built with `scripts/build-package-image.sh`. Candidate
  state, network ports, and mock upstream were isolated; production containers,
  databases, Redis instances, and volumes were not changed.

## Automated Gates

- Protocol: 82 passed
- Chat streaming: 43 passed
- Responses streaming: 8 passed
- Responses lifecycle: 14 passed
- Full offline Rust suite: 1032 passed, 3 ignored across 51 suites
- Full frontend suite: 174 passed across 27 test files
- Formatting and `git diff --check`: passed
- Clippy with warnings denied: 7 pre-existing errors and 1 warning remain in
  unrelated modules; no new finding is attributable to this change.

## Background Token Use

The final image was started in file-backed mode with one active mock upstream.
The second startup intentionally omitted both probe environment variables. Its
startup log reported the packaged defaults:

```text
automatic_capability_probes_enabled=false
upstream_model_key_sync_interval_seconds=0
```

Startup, health checks, and an upstream configuration change produced zero
requests to the mock upstream. Restarting the final image with the persisted
upstream also left the request counts unchanged. This covers both startup
reconciliation and configuration-change scheduling with the default settings.

| Observation point | Mock request counters |
| --- | --- |
| After startup and upstream creation | `{}` |
| After explicit model discovery | `{"GET /v1/models":1}` |
| After explicit manual capability probe | `{"GET /v1/models":1,"POST /v1/chat/completions":10}` |
| After restart with packaged defaults | `{"GET /v1/models":1,"POST /v1/chat/completions":10}` |

An explicit model discovery action produced exactly one
`GET /v1/models` and no inference POST. This endpoint lists models and does not
generate inference tokens, although it is still an upstream HTTP request.

An explicit manual capability probe returned `202` and produced 10
`POST /v1/chat/completions` requests for the current probe plan. The exact
number of requests is plan-dependent, but the distinction is not: manual
capability probes and the admin “真实验证并应用” action send real inference
requests and consume tokens on a real upstream. They remain available only as
explicit administrator actions and are not suppressed by the automatic-probe
setting. The mock retained only method/path counters; it did not retain keys,
headers, or request bodies.

## Client Matrix

The streaming cases were run serially through the `eec1fb5` candidate gateway
with the portal configuration described above. Only status, timing, token
counts, and event categories were retained. The final `d061db5` commit changes
probe scheduling, defaults, documentation, and admin warnings; it does not
change request or stream translation.

| Model | Text | Read-only tool | Approx. 25k input | Result |
| --- | --- | --- | --- | --- |
| `glm-5.1` | pass | pass | 25,641 input tokens, pass | pass |
| `glm-5.2` | pass | pass | 25,648 input tokens, pass | pass |
| `MiniMax-M2.7` | pass | pass | not required | pass |
| `deepseek-v4-pro` | pass | pass | 27,972 input tokens, 18.2s, pass | pass |
| `deepseek-v4-flash` | pass | pass | not required | pass |

The long-input rerun used a synthetic compatibility corpus because the source
PPT was supplied in another environment and is not present in this workspace.
It verifies request size and streaming behavior, not PPT extraction quality.

## Timeout Evidence

The candidate was first run with the code default
`UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS = 30`, then restarted in isolation
with only that setting changed to `120`. The long-input DeepSeek request passed
under the 120-second setting, but the result does not establish that header
timeout was the cause of the historical failure.

The candidate database contains three historical `stream_upstream_timeout`
records: 47,575 ms, 77,466 ms, and 242,963 ms. All have zero usage because the
stream failed before usage was parsed. The fixed 28-character body error and
the source classification indicate a reqwest response-body transport/decode
failure after headers, not the gateway's 30-second header timer or its
1,800-second idle watchdog. The same upstream routes have successful requests
before and after those records.

Recommended deployment starting point for a slow first token is:

```text
UPSTREAM_RESPONSE_HEADER_TIMEOUT_SECONDS=120
UPSTREAM_STREAM_IDLE_TIMEOUT_SECONDS=1800
UPSTREAM_STREAM_KEEPALIVE_INTERVAL_SECONDS=10
UPSTREAM_STREAM_MAX_DURATION_SECONDS=86400
```

Increase the header value only when the upstream's first response headers can
legitimately take longer; do not use it to hide response-body transport
failures. Keep `stream_max_retries = 0` so a committed Codex request is not
replayed.

## Compatibility Scope

The July regression was caused by strict Chat stream canonicalization. The
candidate normalizes `delta: null`, keeps the first valid stream identity when
later values drift, and synthesizes EOF terminal events only after observable
text, reasoning, or tool output. Explicit SSE errors, malformed envelopes, and
ambiguous role/usage-only EOFs remain failures. Structural diagnostics record
request ID, selected route, protocol, endpoint, phase, and a static reason
without recording prompts, response text, tool data, provider messages, or
credentials.
