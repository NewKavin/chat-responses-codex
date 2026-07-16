# Protocol Compatibility

## Current Maturity

Semantic evidence, not HTTP reachability, defines these labels.

- **Verified**: JSON/SSE ordering, content, tool/reasoning continuation, and required evidence checks pass.
- **Partial**: the route has bounded optional downgrade evidence or only a subset of the requested bundle is observed.
- **Unsupported**: a required capability is rejected or semantic validation fails.
- **Unknown**: no fresh exact-route probe exists. Documentation alone does not promote this state.

| Area | Maturity | Evidence |
| --- | --- | --- |
| Codex Responses routing | Verified | `tests/compatibility_semantics.rs`, `tests/troubleshooting.rs`, `tests/gateway/responses/*` |
| OpenCode Chat routing | Verified | `tests/troubleshooting.rs`, `tests/gateway/chat/*` |
| Claude Code Messages adapter | Verified | `tests/gateway/claude.rs`, `tests/troubleshooting.rs` |
| Hermes Chat routing | Verified | `tests/compatibility_semantics.rs`, `tests/troubleshooting.rs` |
| Reasoning replay through tool loops | Verified | `tests/gateway/responses/reasoning.rs`, `tests/gateway/claude.rs` |
| Namespace tool preservation | Verified | `tests/gateway/responses/tools.rs`, `tests/compatibility_semantics.rs` |
| Image HTTPS/Data URL adaptation | Verified | `tests/gateway/images.rs` |
| Safe dialect correction retry | Verified | `tests/gateway/dialect_retry.rs` |
| Capability administration | Verified | `tests/admin_capabilities.rs` |
| Streaming first-event latency contract | Verified | `tests/load.rs` release ignored benchmark |
| Installed client execution | Pending live acceptance | `scripts/installed_client_smoke.sh`; requires the deployed gateway and pinned CLIs |
| Deployment model/client matrix | Pending live acceptance | `scripts/compatibility_matrix.sh`; recorded per exact runtime route |

## Rules

- Preserve, adapt, downgrade, or reject; never silently drop semantics.
- Third-party and self-hosted upstreams are the primary target.
- Exact-route probes override vendor docs for wire syntax.
- Policy semantics do not prove relay support; verified profiles do.
- Manual probes and capability imports are administrative actions, not request-path behavior.
- Optional hosted tools may be dropped only under auto choice when another executable tool remains; the gateway emits `optional_tool:<kind>`. Explicit or last-required hosted tools are rejected.
- Compatibility expectations are diagnostic assertions. They never grant routing capabilities or alter production profiles.
- The selected Qwen VLM expectation is rendered from `QWEN_VLM_SLUG`, `IMAGE_FIXTURE_URL`, and `IMAGE_FIXTURE_EXPECTED_LABEL`; no Qwen slug is compiled into routing code.

## Deployment Workflow

1. Import `templates/capabilities/current-deployment.example.json`, or render the Qwen VLM entry with `scripts/render_live_capabilities.sh` and import the result.
2. Queue exact-route probes from the admin troubleshooting page and inspect source/state/age before acceptance.
3. Run `scripts/compatibility_matrix.sh`; all required semantic checks must pass and every downgrade must be permitted.
4. Run `scripts/installed_client_smoke.sh` for each exposed deployment slug using the pinned Codex, OpenCode, Claude Code, and Hermes versions.
5. Record live results before changing a maturity label from pending/partial to verified.
