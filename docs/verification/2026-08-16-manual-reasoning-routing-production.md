# Manual Reasoning and Responses Routing Production Verification

Verified at `2026-08-16T23:42:10+08:00`.

## Release Identity

- Branch: `feat/manual-reasoning-routing`
- Deployed source revision: `84503f7`
- Runtime image: `chat-responses-codex:manual-reasoning-84503f7`
- Runtime image ID: `sha256:fe5da17f41cc53a08ce52674fe44b3667a82e70af965c4ae38b6b2bbf6bf17be`
- Binary SHA-256: `69ad8ad371000d9ac54b535a99252e7867e4ad750dcd3b245919a65751eb9a26`
- Rollback image: `chat-responses-codex:pre-manual-reasoning-20260816`
- Rollback image ID: `sha256:7b88ebe291c100d3122a1b592e6ef10f72c80ddc7df6fc3329b5955629bc9063`

The release binary was compiled in a Debian Bookworm Rust 1 container. Its
runtime linkage was checked inside the release image; `libgcc_s`, `libm`,
`libc`, and the ELF loader all resolved. Image entrypoint, user, workdir, and
healthcheck match the rollback image.

## Automated Verification

- `cargo test --all-targets --all-features`: 1640 passed, 83 ignored, 0 failed
  across 58 suites.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `npm test`: 33 suites and 260 tests passed.
- `npm run type-check`: passed.
- `npm run build`: passed with Vite 8.0.16.
- `cargo fmt --all -- --check`: failed against the repository-wide formatting
  baseline. The current rustfmt version proposes broad churn across shared and
  unrelated files, so no bulk formatting was applied.

## Deployment Verification

Only the Compose `gateway` service was recreated. PostgreSQL and Redis were not
restarted. The running container reports:

- Image ID `sha256:fe5da17f41cc53a08ce52674fe44b3667a82e70af965c4ae38b6b2bbf6bf17be`.
- Status `running`, health `healthy`, restart count `0`.
- `GET http://127.0.0.1:3000/healthz` returned `200 ok`.
- The 173 log lines emitted since deployment contain 0 `WARN`, 0 `ERROR`, 0
  panic, and 0 `gateway_no_routable_upstream` mentions.
- PostgreSQL contains 0 business requests, 0 business errors, and 0 no-route
  errors after deployment. A live generation request was deliberately not
  issued because it would consume production routing capacity and the user had
  already removed the Responses protocol setting.

The embedded production frontend was also checked through the running server.
The lazy-loaded bundles return HTTP 200 and contain the reasoning override API,
the mapping-status API, the `manual effective` UI state, and authoritative
mapping reason codes.

## Production Read-Only Acceptance

Authenticated read-only admin calls returned HTTP 200 without exposing or
persisting credentials:

- Mapping status: 63 mappings, 52 effective, 11 inactive.
- Inactive reasons: 5 inactive upstreams and 6 mappings with no eligible route.
- Route totals: 58 configured, 52 eligible, 13 unverified.
- Capability discovery: 20 models and 63 exact routes.
- Reasoning sources: 53 baseline routes and 10 probe-derived routes.
- Managed manual overrides: 0 before the user makes an edit.
- Capability document: schema 1, revision 8, 11 policies, 0 route overrides.

The current `gpt-5.6-sol` production configuration exposes seven Chat
Completions routes and no Responses route, which matches the user's decision to
remove the Responses setting after the old failure. This deployment did not
silently re-enable or rewrite that production protocol configuration.

## Incident Evidence and Root Cause

PostgreSQL contains 72 matching historical failures between
`2026-08-15 22:40:12+08:00` and `2026-08-15 23:57:03+08:00`. They all used
`/v1/responses`, model `gpt-5.6-sol`, and reasoning effort `max`. The old error
claimed the model was not configured while listing the same model as available.

The old routing decision treated an active upstream that declared Responses and
the model name as sufficient. It selected the protocol before checking the exact
key/model/protocol capability route. The release now builds the exact-route
capability cache first and chooses among an eligible Responses route, an
eligible Chat fallback, or an explicit unavailable result. Regression coverage
includes a Responses route that declares the model but rejects the required
capability while an eligible Chat route succeeds.

This is consistent with the official OpenAI migration guide: Responses uses a
distinct `/v1/responses` request, output, tool, and streaming contract rather
than being a model-name-only switch:
<https://developers.openai.com/api/docs/guides/migrate-to-responses>.

## Other Operational Findings

- The 96 `upstream_routes_exhausted` events in the prior 24-hour window were a
  single burst around `2026-08-16 00:00+08:00`: 55 for
  `deepseek-v4-flash/high` and 41 for `gpt-5.6-sol/max`. The latest event was at
  00:20. Diagnostics identify upstream capacity, transient 502/503 responses,
  concurrency saturation, and one credential-rejected route. This is an
  upstream health/capacity issue, not the no-routable routing bug.
- A read-only model probe observed 13 healthy and 4 offline channels. Credential
  cleanup and offline-channel review are worthwhile, but were not applied as
  part of this release because they change production routing inputs.
- A separate Compose project at `/home/kavin/docker/nginx` is restart-looping.
  Its config references `new-api-protocol-bridge:3000`, but no such container is
  present on `sub2api-network`; restart count was 1229. This is independent of
  the healthy `chat-responses-codex` Compose stack and was left unchanged.

## Rollback

If rollback is required, restore the preserved image and recreate only the
gateway service from `/home/kavin/docker/chat-responses-codex`:

```bash
rtk docker tag chat-responses-codex:pre-manual-reasoning-20260816 chat-responses-codex:latest
rtk docker compose up -d --no-deps --force-recreate gateway
```
