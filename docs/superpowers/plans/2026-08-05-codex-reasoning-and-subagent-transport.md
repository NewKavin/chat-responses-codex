# Codex Reasoning And Subagent Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Codex subagent tasks and results readable through domestic-model Chat fallback, strengthen real delegation verification, and expose only verified `low`, `medium`, `high`, `xhigh`, and `max` reasoning choices with `medium` preferred.

**Architecture:** Publish a conservative V1 multi-agent contract in the gateway Codex catalog and reject encrypted `agent_message` conversion at the Responses-to-Chat boundary. Keep portal role files derived from one catalog selection, validate delegation with a runtime-only marker, and make reasoning controls evidence-driven with capability mapping before generic degradation.

**Tech Stack:** Rust, Axum, Serde JSON, Codex CLI 0.146.0, Bash/JQ, Vue 3, TypeScript, Vitest.

---

## File Structure

- `src/protocol.rs`: represent plaintext agent messages and return a dedicated error for encrypted Chat fallback.
- `src/server/gateway/stream.rs`: map the dedicated protocol error to a stable, content-free gateway response.
- `src/server/gateway.rs`: publish the Codex V1 model-level contract and verified reasoning levels.
- `src/server/gateway/compat.rs`: apply route-specific effort mappings before generic effort degradation.
- `tests/protocol.rs`: cover plaintext and encrypted `agent_message` shapes.
- `tests/gateway/capability_routing.rs`: cover V1 catalog and verified reasoning metadata.
- `tests/gateway/responses/fallback.rs`: prove encrypted input never reaches a Chat upstream and native Responses remains transparent.
- `frontend/src/utils/integration.ts`: expose the fixed five-effort selection contract and keep both role files synchronized.
- `frontend/src/views/portal/Integration.vue`: render the effort selector and V1 compatibility guidance.
- `frontend/tests/utils/integration.spec.ts`: cover catalog preservation, effort selection, and generated configuration.
- `frontend/tests/views/portal-integration.spec.ts`: cover visible controls and operator guidance.
- `templates/codex/config.toml.example`: share the live-catalog effort placeholder with the default-agent template.
- `docs/codex-integration-guide.md`: document V1 selection, new-session requirements, and reasoning evidence.
- `scripts/installed_client_smoke.sh`: verify one completed delegation and an unpredictable child result.
- `tests/scripts.rs`: exercise positive and negative smoke fixtures.

### Task 1: Publish The Conservative V1 Catalog Contract

**Files:**
- Modify: `tests/gateway/capability_routing.rs`
- Modify: `src/server/gateway.rs`

- [ ] **Step 1: Write the failing Codex catalog tests**

In `codex_catalog_deserializes_with_the_pinned_0_144_model_info_contract`, add:

```rust
assert_eq!(parsed.models[0].multi_agent_version.as_deref(), Some("v1"));
assert_eq!(catalog["models"][0]["multi_agent_version"], "v1");
```

Extend the conservative allowlist-only catalog test to assert every entry has
`multi_agent_version == "v1"`. Add a mixed-protocol route case and assert it
also publishes V1, so route ranking cannot accidentally opt a model into V2.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
rtk cargo test --test gateway codex_catalog_deserializes_with_the_pinned_0_144_model_info_contract
rtk cargo test --test gateway codex_catalog_uses_the_complete_nonempty_downstream_allowlist
```

Expected: the typed field is `None` and JSON has no `multi_agent_version`.

- [ ] **Step 3: Add the minimal catalog field**

In each object built by `list_models_codex_format`, add:

```rust
"multi_agent_version": "v1",
```

Do not derive the value from a single catalog witness. The catalog is
model-level while runtime retries can select another protocol; V1 is valid for
both native Responses and Chat fallback.

- [ ] **Step 4: Run the focused tests and verify GREEN**

```bash
rtk cargo test --test gateway codex_catalog
rtk cargo test --test gateway catalog_witness
```

- [ ] **Step 5: Request specification and code-quality reviews**

Dispatch two fresh read-only review agents. The specification reviewer checks
the implementation against Section 1 of the design. After fixes, the quality
reviewer checks catalog compatibility, mixed-route behavior, and test gaps.

- [ ] **Step 6: Commit**

```bash
rtk git add src/server/gateway.rs tests/gateway/capability_routing.rs
rtk git commit -m "fix(codex): select plaintext v1 subagent transport"
```

### Task 2: Reject Encrypted Agent Messages At The Chat Boundary

**Files:**
- Modify: `tests/protocol.rs`
- Modify: `tests/gateway/responses/fallback.rs`
- Modify: `src/protocol.rs`
- Modify: `src/server/gateway/stream.rs`

- [ ] **Step 1: Replace placeholder-positive protocol tests with RED tests**

Add a dedicated expected variant:

```rust
assert_eq!(
    responses_request_to_chat_payload(&request),
    Err(ProtocolError::EncryptedAgentMessageUnsupported),
);
```

Cover content-part ciphertext, top-level ciphertext, `type` without a value,
visible text beside ciphertext, and response-output translation. Keep a
separate plaintext case proving top-level text and text parts still merge in
order as an assistant message. Remove every assertion that treats
`[encrypted content omitted]` as valid output.

- [ ] **Step 2: Add gateway RED tests**

In `tests/gateway/responses/fallback.rs`, add one Chat-only fixture whose
request contains an encrypted `agent_message`. Assert:

```rust
assert_eq!(response.status(), StatusCode::BAD_REQUEST);
assert_eq!(body["error"]["code"], "encrypted_agent_message_requires_responses_upstream");
assert_eq!(chat_upstream_hits.load(Ordering::SeqCst), 0);
assert!(!serialized_body.contains("opaque-ciphertext"));
```

Add a native Responses fixture and assert the captured upstream request still
contains the opaque encrypted value unchanged.

- [ ] **Step 3: Run tests and verify RED**

```bash
rtk cargo test --test protocol agent_message
rtk cargo test --test gateway encrypted_agent_message
```

Expected: protocol tests still receive placeholder content and the Chat
gateway test reaches the upstream or returns the generic invalid-payload code.

- [ ] **Step 4: Implement the dedicated protocol error**

Add to `ProtocolError`:

```rust
EncryptedAgentMessageUnsupported,
```

Its `Display` text is a fixed string with no payload data. Replace the
placeholder helper with a recursive predicate that returns the dedicated error
for any encrypted shape. Keep the existing plaintext extraction for all other
parts.

Map the new variant in `protocol_error_to_gateway` with:

```rust
GatewayError::classified(
    StatusCode::BAD_REQUEST,
    "encrypted subagent messages require a native Responses route; use the V1 catalog profile and start a new Codex session",
    "invalid_request_error",
    "encrypted_agent_message_requires_responses_upstream",
    "encrypted_agent_message_requires_responses_upstream",
    None,
    Some(json!({ "scope": "gateway" })),
)
```

- [ ] **Step 5: Run focused and protocol regression tests**

```bash
rtk cargo test --test protocol agent_message
rtk cargo test --test gateway encrypted_agent_message
rtk cargo test --test gateway responses_fallback
rtk cargo test --test gateway stream_lifecycle
```

- [ ] **Step 6: Request specification and code-quality reviews**

The specification review verifies plaintext preservation, native Responses
transparency, safe public errors, and zero upstream hits. The quality review
checks recursive shape handling, response translation, and absence of content
in traces/errors.

- [ ] **Step 7: Commit**

```bash
rtk git add src/protocol.rs src/server/gateway/stream.rs tests/protocol.rs tests/gateway/responses/fallback.rs
rtk git commit -m "fix(protocol): reject unreadable encrypted agent payloads"
```

### Task 3: Document And Generate A Consistent V1 Agent Profile

**Files:**
- Modify: `frontend/tests/utils/integration.spec.ts`
- Modify: `frontend/tests/views/portal-integration.spec.ts`
- Modify: `tests/templates.rs`
- Modify: `frontend/src/views/portal/Integration.vue`
- Modify: `templates/codex/config.toml.example`
- Modify: `docs/codex-integration-guide.md`

- [ ] **Step 1: Write failing portal and template tests**

Require the sanitized catalog to retain `multi_agent_version: 'v1'`. Require
the portal view and integration guide to contain `multi_agent_version`, `V1`,
`multi_agent_v2`, and the new-session instruction. Require both static TOML
templates to use `<reasoning_effort_from_live_catalog>`.

- [ ] **Step 2: Run and verify RED**

```bash
rtk npm --prefix frontend test -- tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
rtk cargo test --test templates codex
```

- [ ] **Step 3: Add the minimal guidance and placeholder fix**

Add a warning below the current agent-limit text explaining that the live model
catalog selects V1 plaintext delegation, users must not enable
`multi_agent_v2`, and changed catalogs require a new session. Change the main
static template from literal `none` to the same catalog placeholder used by
`agents/default.toml.example`. Document the catalog precedence and sticky
session behavior without instructing users to hand-edit the version field.

- [ ] **Step 4: Run frontend and template tests**

```bash
rtk npm --prefix frontend test -- tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk cargo test --test templates codex
```

- [ ] **Step 5: Request specification and code-quality reviews, then commit**

```bash
rtk git add frontend/src/views/portal/Integration.vue frontend/tests/utils/integration.spec.ts frontend/tests/views/portal-integration.spec.ts templates/codex/config.toml.example docs/codex-integration-guide.md tests/templates.rs
rtk git commit -m "docs(codex): explain v1 subagent compatibility"
```

### Task 4: Make The Delegation Smoke Prove Task And Result Delivery

**Files:**
- Modify: `tests/scripts.rs`
- Modify: `scripts/installed_client_smoke.sh`

- [ ] **Step 1: Write failing positive and negative script fixtures**

Change fake Codex output to use the runtime `probe.txt` value in a completed
agent message:

```json
{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}
{"type":"item.completed","item":{"type":"agent_message","text":"<runtime marker>"}}
{"type":"turn.completed"}
```

Add a negative fixture that emits one completed collaboration call, a completed
turn, and the old fixed marker. Assert the smoke exits non-zero with
`status=delegation_result_mismatch`.

- [ ] **Step 2: Run and verify RED**

```bash
rtk cargo test --test scripts installed_client_smoke_uses_portal_codex_profile_and_checks_delegation
rtk cargo test --test scripts installed_client_smoke_can_run_only_the_codex_delegation_task
rtk cargo test --test scripts installed_client_smoke_rejects_unproven_delegation
```

Expected: the old verifier accepts the fixed marker and does not inspect
collaboration status or final agent text.

- [ ] **Step 3: Implement structural JSONL verification**

Reuse `READ_MARKER` as the delegation result. The delegation prompt must not
contain its value; it names only `probe.txt`. Replace the type-only check with
one `jq -Rne` expression requiring exactly one completed `collab_tool_call`, a
completed agent message equal to `$expected_marker`, and `turn.completed`.
Report only event types and bounded status.

- [ ] **Step 4: Run script tests and shell syntax checks**

```bash
rtk cargo test --test scripts installed_client_smoke
rtk bash -n scripts/installed_client_smoke.sh
```

- [ ] **Step 5: Request specification and code-quality reviews, then commit**

```bash
rtk git add scripts/installed_client_smoke.sh tests/scripts.rs
rtk git commit -m "test(codex): prove delegated task result delivery"
```

### Task 5: Publish Verified Reasoning Levels And Correct Mapping Order

**Files:**
- Modify: `tests/gateway/capability_routing.rs`
- Modify: `tests/gateway/responses/fallback.rs`
- Modify: `src/server/gateway.rs`
- Modify: `src/server/gateway/compat.rs`
- Modify: `src/server/gateway/upstream.rs`

- [ ] **Step 1: Write failing reasoning catalog tests**

Create policy/profile intersections for `low`, `medium`, `high`, `xhigh`,
`max`, and `minimal`. Assert the Codex catalog returns exactly the first five,
filters `minimal`, and defaults to `medium`. Keep a no-evidence case that
returns only `none`.

- [ ] **Step 2: Write the mapping-order RED test**

Configure `xhigh -> upstream-xhigh` and `max -> upstream-max` in the resolved
effort map for a Chat route. Send each canonical value and assert the captured
Chat payload contains the mapped upstream value, not generic `high`.

- [ ] **Step 3: Run and verify RED**

```bash
rtk cargo test --test gateway codex_catalog_advertises_only_verified_reasoning_levels
rtk cargo test --test gateway mapped_reasoning_effort_precedes_generic_normalization
```

- [ ] **Step 4: Filter the catalog and reorder compatibility mapping**

Change the public order to:

```rust
const CODEX_REASONING_EFFORT_ORDER: [&str; 5] =
    ["low", "medium", "high", "xhigh", "max"];
```

Filter all other keys. In Chat payload preparation, apply
`apply_resolved_reasoning_effort` to the original canonical value first. Only
when no capability mapping applies should generic normalization degrade
`xhigh|max` to `high` and omit `none` or unknown values.

- [ ] **Step 5: Run reasoning and fallback regressions**

```bash
rtk cargo test --test gateway reasoning
rtk cargo test --test gateway responses_fallback
rtk cargo test --test capability_resolver effort_map
```

- [ ] **Step 6: Request specification and code-quality reviews, then commit**

```bash
rtk git add src/server/gateway.rs src/server/gateway/compat.rs src/server/gateway/upstream.rs tests/gateway/capability_routing.rs tests/gateway/responses/fallback.rs
rtk git commit -m "feat(codex): expose verified reasoning strengths"
```

### Task 6: Add The Five-Level Portal Selector

**Files:**
- Modify: `frontend/tests/utils/integration.spec.ts`
- Modify: `frontend/tests/views/portal-integration.spec.ts`
- Modify: `frontend/src/utils/integration.ts`
- Modify: `frontend/src/views/portal/Integration.vue`

- [ ] **Step 1: Write failing selector tests**

Assert the fixed option order is `low/medium/high/xhigh/max`, `minimal` and
`none` are not selectable, unsupported options are disabled, and a verified
`medium` is selected by default. Assert changing model resets the selection to
that model's verified default and both generated TOML files receive the same
value.

- [ ] **Step 2: Run and verify RED**

```bash
rtk npm --prefix frontend test -- tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
```

- [ ] **Step 3: Implement selection state and UI**

Add a compact `el-select` beside the model selector. Derive supported values
from the selected catalog entry, render all five known values, and set
`:disabled` for absent levels. When only `none` is available, show an explicit
unavailable state and keep generation on the catalog's conservative value;
never send an unverified strength.

- [ ] **Step 4: Run frontend gates**

```bash
rtk npm --prefix frontend test -- tests/utils/integration.spec.ts tests/views/portal-integration.spec.ts
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk npm --prefix frontend run build
```

- [ ] **Step 5: Request specification and code-quality reviews, then commit**

```bash
rtk git add frontend/src/utils/integration.ts frontend/src/views/portal/Integration.vue frontend/tests/utils/integration.spec.ts frontend/tests/views/portal-integration.spec.ts
rtk git commit -m "feat(portal): configure verified Codex reasoning effort"
```

### Task 7: Live Validation, Full Gates, Image Build, And Deployment

**Files:**
- Modify only if tests require: capability deployment data and smoke scripts
- Modify after every repository gate and image build pass:
  `/home/kavin/docker/chat-responses-codex`

- [ ] **Step 1: Run focused transport verification**

```bash
rtk cargo fmt --check
rtk cargo test --test protocol agent_message
rtk cargo test --test gateway codex_catalog
rtk cargo test --test gateway encrypted_agent_message
rtk cargo test --test scripts installed_client_smoke
```

- [ ] **Step 2: Run the repository gates**

```bash
rtk cargo test --all-targets
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime -- --test-threads=1
rtk cargo test --test gateway 'slow_stream::'
rtk cargo test --test gateway stream_disconnect_releases_runtime_state
rtk cargo test --test gateway provider_retry_after
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk npm --prefix frontend test
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk npm --prefix frontend run build
rtk docker compose config
rtk git diff --check
```

- [ ] **Step 3: Build with the repository build script**

Run the repository's local compile, runtime-image, and export pipeline:

```bash
rtk scripts/build-package-image.sh --image chat-responses-codex --tag latest --output chat-responses-codex-latest.tar
rtk docker image inspect chat-responses-codex:latest
```

Require both commands to exit zero. Do not modify the deployment directory
before the image exists and its inspect command succeeds.

- [ ] **Step 4: Run serial domestic-model probes**

For each currently configured GLM, DeepSeek, Kimi, Qwen, and MiniMax slug,
temporarily enable one route, run the installed Codex delegation task with the
test downstream account, and restore its original active state immediately.
Probe only catalog-advertised reasoning levels. Do not print credentials,
prompts, tool arguments, results, upstream bodies, or billing fields.

- [ ] **Step 5: Deploy and health-check**

Preserve existing deployment passwords, keys, volumes, Redis prefix, and port
configuration. Validate Compose, replace the service with the verified image,
check container health and `/health`, then rerun the serial Codex delegation
smoke against `glm-5.2` and `deepseek-v4-flash`.

- [ ] **Step 6: Final review and push**

Run one final independent specification review and one code-quality review over
the complete commit range. Fix findings, rerun affected gates, merge into
`main` if needed, and push only after the deployed health and real delegation
checks pass.
