# Agent Protocol Fidelity Verification

Verification date: 2026-07-13

## Automated Verification

- `cargo test --all-targets -- --nocapture`: `626 passed`, `3 ignored`
- `cargo test --test gateway -- --nocapture`: `162 passed`
- `cargo test --manifest-path crates/gateway-core/Cargo.toml --all-targets -- --nocapture`: `5 passed`
- `npm --prefix frontend exec vitest run`: `120 passed`
- `npm --prefix frontend run build`: passed
- `cargo fmt --all -- --check`: passed
- `cargo clippy --all-targets --all-features -- -D warnings`: passed
- `bash -n scripts/installed_client_smoke.sh`: passed
- `cargo test --release --test load load_gateway_first_meaningful_event -- --ignored --exact --nocapture`: passed

## Performance Evidence

- Text first meaningful event:
  - direct P95: `18 ms`
  - gateway P95: `19 ms`
  - gateway-added P95: `1 ms`
- Inline image first meaningful event:
  - direct P95: `5 ms`
  - gateway P95: `21 ms`
  - gateway-added P95: `16 ms`
- Both measured gateway-added P95 values are below the `50 ms` acceptance limit.

## Source-Backed Client Compatibility

- Codex catalog compatibility notes were checked against the official OpenAI Codex source tag
  `rust-v0.144.1`. That source evidence is **superseded context and is not accepted as exact
  installed-client evidence**: the smoke script pins Codex CLI `0.144.0`.
- A live installed-client run with the pinned Codex CLI `0.144.0` has not been completed in this
  environment. Codex text and read-only tool acceptance therefore remain **pending**, rather than
  being inferred from the `0.144.1` source notes.
- Codex deserializes `model_catalog_json` as `ModelsResponse<ModelInfo>` and requires
  `web_search_tool_type` to be a `WebSearchToolType` string. The gateway now emits the
  official default value `"text"`; it no longer emits an invalid JSON `null`.
- Chat `tool_choice: "auto"` is treated as automatic function use, not as the separate
  `ForcedToolChoice` capability. This matches Responses handling and permits clients such
  as OpenCode to use automatic tools on routes that reject named/required tool forcing.
- `parallel_tool_calls: true` requires verified parallel-tool evidence when the request
  actually contains tools. Catalog reasoning defaults and effort levels come from the
  selected route's resolved evidence; unknown controls are emitted as an empty level list
  with a `null` default, and unprobed reasoning summaries are not advertised.

## Installed Client Acceptance

The deployed gateway was exercised with OpenCode, Claude Code, and Hermes against a live
third-party Chat Completions route. Those clients each completed a text task and a real
read-only tool task. Codex `0.144.0` remains pending until its pinned binary is measured:

| Client | Version | Text | Read-only tool loop |
| --- | --- | --- | --- |
| Codex | `0.144.0` | pending (not measured) | pending (not measured) |
| OpenCode | `1.17.9` | passed | passed |
| Claude Code | `2.1.195` | passed | passed |
| Hermes | `0.14.0` | passed | passed |

Claude Code acceptance uses an isolated `CLAUDE_CONFIG_DIR`, so an existing user-level
provider configuration cannot redirect the smoke test. Its CLI prompt is placed before
the variadic `--tools` option so the prompt cannot be consumed as a tool name.

## Live Capability Evidence

Probe schema version `9` produced successful, current profiles for GLM and DeepSeek:

| Route | Function tools | Continuation | Reasoning replay | Forced choice | Indexed argument stream |
| --- | --- | --- | --- | --- | --- |
| GLM 5.2 deployment route | supported | supported | supported | rejected | rejected |
| DeepSeek deployment route | supported | supported | supported | rejected | rejected |

Current Qwen evidence is route-specific rather than inferred from the model name:

- The 235B text route supports function tools, continuation, indexed argument streaming,
  and reasoning replay; its current probe rejects forced selection and Data URL images.
- The selected Qwen VLM routes support function tools, forced selection, continuation,
  and indexed argument streaming, but the configured upstream did not prove image
  semantics and rejected reasoning replay.

MiniMax M2.5/M2.7 and Kimi K2.5/K2.6 could not produce positive current profiles because
the configured third-party upstream reported model/provider resource failures. These
routes remain unknown or rejected rather than being advertised as compatible.

## Strict Matrix Status

The strict four-client matrix is intentionally stronger than the installed-client smoke.
It requires forced function selection, linked continuation, indexed argument fragments,
reasoning replay, and image semantics where configured. It is not reported as passing:

- GLM and DeepSeek reject forced tool choice and indexed argument streaming even though
  their automatic tool and continuation loops work.
- The selected Qwen VLM route did not prove HTTPS or Data URL image semantics and rejected
  reasoning replay.
- A final matrix rerun was interrupted by third-party `429` rate limiting; no partial or
  empty result was promoted to acceptance evidence.

## Deployment Evidence

- Deployed gateway image: `sha256:7197ec96a57dbacddc44ce4f4f99a0870cfa5ecab3186d24319170a0987be5c9`
- Gateway container health: healthy
- PostgreSQL and Redis were not recreated during the upgrade.
- Preserved rollback image: `chat-responses-codex:rollback-20260712-0697e442a10d`
- Preserved pre-upgrade database backup:
  `/home/kavin/docker/chat-responses-codex/backups/pre-agent-fidelity-20260712.sql`

## Acceptance Mapping

| Criterion | Evidence |
| --- | --- |
| Four client protocol surfaces | `tests/troubleshooting.rs`, installed-client acceptance above |
| Codex namespace fidelity | `tests/gateway/responses/tools.rs`, `tests/compatibility_semantics.rs` |
| Claude signed thinking replay | `tests/gateway/claude.rs`, `tests/thinking_signature.rs` |
| Reasoning replay routing | `tests/gateway/responses/reasoning.rs`, `tests/gateway/claude.rs` |
| Image adaptation | `tests/gateway/images.rs` |
| Bounded dialect retry | `tests/gateway/dialect_retry.rs` |
| Capability persistence and admin | `tests/admin_capabilities.rs`, `tests/postgres_roundtrip.rs` |
| Truthful frontend presets | `frontend/tests/utils/integration.spec.ts`, `frontend/tests/api/admin.spec.ts` |
| Dynamic matrix semantics | `tests/troubleshooting.rs`, `tests/compatibility_semantics.rs` |
| Streaming latency contract | `tests/load.rs` |
| Vendor-agnostic production dispatch | `tests/generic_dispatch.rs`, `tests/capability_policy.rs` |
