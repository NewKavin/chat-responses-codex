# GLM-5.2 Continuation Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Subagents are read-only scouts/reviewers only; the primary agent owns every edit and final verification. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the strict Codex delegation smoke, complete the domestic-model and installed-client live matrix, re-run all release gates, rebuild with the repository packaging script, deploy only the gateway while preserving operator configuration, and push `main`.

**Architecture:** Keep Codex on the V1 collaboration transport for Chat-only domestic upstreams. The gateway rejects unreadable encrypted agent payloads instead of fabricating plaintext, maps `*-fast-preview` subagent aliases to their configured base models, and proves delegation through the installed Codex event stream. Capability publication remains evidence-gated; unsupported reasoning levels are not advertised.

**Tech Stack:** Rust, Axum, Tokio, Reqwest, PostgreSQL 15, Redis 7, Vue 3, TypeScript, Vitest, Docker Compose, Codex CLI 0.146.0, Claude Code 2.1.221, Cline CLI 0.0.13, Kilo Code CLI 7.4.20.

---

## 1. Workspace And Safety Baseline

Repository and branch:

```text
/home/kavin/projects/chat2Responses
main
```

Current committed state:

```text
HEAD: 1716e90 feat(capabilities): verify domestic reasoning and v1 delegation
main is 57 commits ahead of origin/main as of the handoff.
```

Current uncommitted work must be preserved:

```text
M scripts/installed_client_smoke.sh
M tests/scripts.rs
?? docs/superpowers/plans/2026-08-06-glm52-continuation-handoff.md
?? docs/superpowers/plans/2026-08-06-glm52-continuation-prompt.md
```

Never run `git reset`, `git checkout --`, or another command that discards these changes. Other development windows may add changes after this handoff; treat every unknown change as user-owned and work with it.

Mandatory operating rules:

- Every shell command must begin with `rtk `.
- Manual file edits must use `apply_patch`.
- Follow TDD for every behavior change: write a focused test, run it and observe the expected failure, add the minimal implementation, and run it green.
- Before each commit, request a read-only specification review and then a separate read-only code-quality review.
- Subagents must use `agent_type="default"` and `fork_turns="none"`; they do not edit files and are not reused.
- Never print or log API Keys, Authorization headers, upstream response bodies, billing fields, prompts, tool arguments, encrypted content, or raw installed-client event payloads.
- Do not hard-code an upstream concurrency limit. It may be 4, 6, or another dynamic value.
- `/dashboard/api/user/request-status` remains private, optional, and disabled by default.
- Do not change `/home/kavin/docker/chat-responses-codex` until repository tests and image construction pass.
- Preserve deployment passwords, Keys, volumes, Redis prefix, and `3000:3001` port mapping. Recreate only the gateway unless a database or Redis migration explicitly requires otherwise.

Read these files completely before editing:

```text
/home/kavin/projects/chat2Responses/AGENTS.md
/home/kavin/projects/chat2Responses/RTK.md
/home/kavin/projects/chat2Responses/docs/superpowers/plans/2026-08-01-account-concurrency-recovery-and-runtime-visibility.md
/home/kavin/projects/chat2Responses/docs/superpowers/plans/2026-08-06-glm52-continuation-handoff.md
```

Initial state checks:

```bash
rtk git status --short --branch
rtk git log -5 --oneline --decorate
rtk git diff -- scripts/installed_client_smoke.sh tests/scripts.rs
```

Expected: the two modified smoke files and these two handoff documents remain present. Stop only if an overlapping external edit makes the intended change impossible to identify.

## 2. Verified State At Handoff

The following facts have fresh evidence from this development sequence:

- `rtk cargo test --test scripts`: 39 passed.
- A real deployed Codex 0.146.0 `glm-5.2` delegation was run three times serially through `http://127.0.0.1:3000/v1` with the `test` downstream account.
- All three runs emitted sanitized event types containing `collab_tool_call`, `agent_message`, and `turn.completed`, and the smoke reported `status=verified`.
- Each run used one script invocation with no automatic retry. No Key or raw event payload was printed.
- The current image is `chat-responses-codex:latest`, digest `sha256:609fbe97898d5abc776e491dd94a8c84e60c6de730e39c8acfc0ca60c9f6cfca`.
- The deployed gateway, PostgreSQL, and Redis were healthy after the prior deployment; the gateway responds to `/healthz`.

The last complete pre-handoff repository gate, before the current two-file smoke edit, passed:

```text
cargo test --all-targets: 1300 passed, 68 ignored
Redis live serial tests: 65 passed
slow stream: 2 passed
disconnect regression: 4 passed
provider Retry-After: 3 passed
frontend tests: 217 passed
vue-tsc --noEmit: passed
frontend production build: passed
Clippy all targets/all features: passed
fmt, diff check, shell syntax, Compose config: passed
```

These results are historical evidence only. Re-run every final gate after the remaining edits.

## 3. Current Uncommitted Smoke Change

`scripts/installed_client_smoke.sh` currently adds:

- explicit modern Codex event validation for exactly one completed `spawn_agent` and one completed `wait`;
- ordering `spawn_agent -> wait -> final agent_message -> turn.completed`;
- a random prompt leak sentinel that must not appear in the final reply;
- one occurrence of the random file marker in the final message;
- a bounded final wrapper of at most 192 extra characters;
- safe mismatch reason codes without printing event bodies;
- stable diagnostic prefix:

```text
client=codex task=delegation status=delegation_result_mismatch reasons=...
```

`tests/scripts.rs` currently adds negative fixtures for missing `wait` and prompt-sentinel leakage, plus a positive bounded-wrapper fixture.

Focused tests already green for this state:

```bash
rtk bash -n scripts/installed_client_smoke.sh
rtk cargo test --test scripts installed_client_smoke_rejects_unproven_delegation -- --exact
rtk cargo test --test scripts
```

Do not revert this work. The next task tightens one remaining false-positive path.

## 4. Task 1: Remove Automatic Legacy Delegation Acceptance

**Files:**

- Modify: `tests/scripts.rs`
- Modify: `scripts/installed_client_smoke.sh`

Problem: `record_codex_delegation_case` still accepts one completed `collab_tool_call` with no `.item.tool` through a legacy branch. A real client whose event schema loses the tool name could therefore pass without proving both `spawn_agent` and `wait`.

- [ ] **Step 1: Add a focused failing fixture**

In `installed_client_smoke_rejects_unproven_delegation`, add a fixture named `unknown_legacy_collab` that emits:

```json
{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}
{"type":"item.completed","item":{"type":"agent_message","text":"the marker read from probe.txt"}}
{"type":"turn.completed"}
```

Add `unknown_legacy_collab` to the rejected modes. Keep the existing assertion on the stable `delegation_result_mismatch` prefix.

- [ ] **Step 2: Run RED**

```bash
rtk cargo test --test scripts installed_client_smoke_rejects_unproven_delegation -- --exact
```

Expected: FAIL because the current legacy branch accepts the unknown collaboration item.

- [ ] **Step 3: Make real smoke require the modern schema**

Delete `$legacy_collab_indexes` and the output-shape-based `else` acceptance. The success predicate must require all of these:

```text
exactly two named collab_tool_call items
exactly one tool=spawn_agent
exactly one tool=wait
both status=completed
spawn index < wait index < final agent_message index
the random marker occurs exactly once in the final text
the leak sentinel is absent from the final text
the final text is no more than marker length + 192 characters
a turn.completed event follows the final message
```

Update generic fake Codex fixtures in `tests/scripts.rs` to emit the same named `spawn_agent` and `wait` pair. Do not introduce a production environment switch that permits legacy acceptance. Offline fixtures should model the current client contract.

- [ ] **Step 4: Bind the proof to one terminal turn**

Add another negative fixture, `extra_turn`, with a completed spawn/wait turn followed by a second marker-bearing turn. Observe RED first. Then minimally require exactly one `turn.started` and exactly one `turn.completed` in the delegation command output, with:

```text
turn.started < spawn_agent < wait < final agent_message < turn.completed
```

The installed `codex exec --json --ephemeral` command is one turn; multiple turn boundaries are not required for this smoke.

- [ ] **Step 5: Run GREEN and regressions**

```bash
rtk bash -n scripts/installed_client_smoke.sh
rtk cargo test --test scripts installed_client_smoke_rejects_unproven_delegation -- --exact
rtk cargo test --test scripts
rtk git diff --check
```

Expected: all 39 or more script tests pass, and the new negative fixtures fail only when run against the pre-fix validator.

## 5. Task 2: Re-Prove Real GLM Delegation After Tightening

**Files:** no repository edit expected.

Use the deployed `test` downstream account without printing its plaintext Key. Keep `set +x`; capture the value inside one shell process and pass it only through environment variables/stdin. Do not put the Key literal in the command line or final report.

- [ ] **Step 1: Run three serial `glm-5.2` delegation smokes**

Set:

```text
CLIENTS=codex
CODEX_TASKS=delegation
EXPECTED_CODEX_VERSION=0.146.0
API_BASE_URL=http://127.0.0.1:3000/v1
MODEL_SLUG=glm-5.2
CLIENT_TIMEOUT_SECONDS=300
```

Run `scripts/installed_client_smoke.sh` three times serially. Do not add automatic retry to the script. A failed run is evidence to diagnose, not a reason to silently issue a duplicate request.

- [ ] **Step 2: Diagnose any failure only from safe reason codes**

Allowed diagnostic output:

```text
spawn_count
wait_count
spawn_status
wait_status
message_count
marker_count
wrapper_too_long
prompt_sentinel
turn_completed
event_order
```

Do not print the JSONL file. If a new observable distinction is needed, add a bounded boolean/count reason code and first add a failing offline test.

- [ ] **Step 3: Run `glm-5.1` delegation**

Use the same command with `MODEL_SLUG=glm-5.1`. Its catalog reasoning level may remain `none`; V1 delegation still needs to work. If the upstream returns an operational 5xx, record only the status/category and route identity, then select another enabled route if available. Do not change capability evidence from one transient failure.

## 6. Task 3: Close The Capability Review Items

**Files:**

- Inspect: `src/server/gateway/capability_probe.rs`
- Inspect: `tests/capability_probe.rs`
- Inspect: `src/protocol.rs`
- Inspect: `scripts/codex_delayed_output_smoke.sh`
- Test/modify only if the stated assertion is absent.

Facts already established:

- Responses `reasoning_effort` probes correctly send `reasoning.effort` at `src/server/gateway/capability_probe.rs:1521-1545`.
- `tests/capability_probe.rs:258-317` already proves the nested request and absence of a top-level `reasoning_effort` field.
- `/v1/messages` is a downstream Claude compatibility surface, not a native upstream protocol. Production capability probe jobs support Chat Completions and Responses only. Do not invent a native Messages upstream implementation.
- `*-fast-preview` aliases are mapped to base slugs by `codex_subagent_base_model` and `canonical_route_model`; the focused gateway test for `glm-5.2-fast-preview -> glm-5.2` already passes.

- [ ] **Step 1: Re-run the established probe and alias tests**

```bash
rtk cargo test --test capability_probe responses_reasoning_control_probe_uses_nested_reasoning_effort -- --exact
rtk cargo test --test gateway responses::core::codex_subagent_fast_preview_model_uses_authorized_base_route -- --exact
```

- [ ] **Step 2: Verify every generated Codex config disables V2**

The portal generator, main template, installed-client smoke, delayed-output smoke, and documentation must all contain:

```toml
[features]
multi_agent_v2 = false
```

If `scripts/codex_delayed_output_smoke.sh` or another generator lacks the setting, add a source-level test in `tests/scripts.rs`, observe RED, add the setting, and run GREEN. Do not add `multi_agent_v2` to `agents/default.toml`; that file is an agent role profile, not the transport selector.

- [ ] **Step 3: Re-run protocol security regressions**

```bash
rtk cargo test --test gateway encrypted_agent_message
rtk cargo test --test templates codex
```

Expected: native Responses preserves supported encrypted data; Chat-only downgrade rejects unreadable encrypted agent messages with a safe 400 and never substitutes fake plaintext.

## 7. Task 4: Install Missing Clients In A Temporary Prefix

**Files:** no repository edit expected.

Current host state:

```text
codex 0.146.0: installed
claude 2.1.221: installed
clite: missing
kilo: missing
```

Exact packages:

```text
@cline/cli@0.0.13 -> clite
@kilocode/cli@7.4.20 -> kilo
```

Do not install the unrelated unscoped `clite` or `cline` packages.

- [ ] **Step 1: Install into an isolated temporary prefix**

```bash
rtk npm install --prefix /tmp/chat2responses-client-pins @cline/cli@0.0.13 @kilocode/cli@7.4.20 --no-audit --no-fund
```

- [ ] **Step 2: Verify versions without credentials**

```bash
rtk bash -lc 'export PATH=/tmp/chat2responses-client-pins/node_modules/.bin:$PATH; clite --version; kilo --version'
```

Expected: the first semantic version tokens are `0.0.13` and `7.4.20`.

## 8. Task 5: Run The Installed-Client Compatibility Matrix

**Files:** modify smoke support only through a new TDD cycle if a reproducible gateway/client contract defect is found.

Use the `test` downstream account and direct internal gateway. Run serially to avoid distorting scarce provider capacity.

- [ ] **Step 1: Run Claude Code with the installed version override**

```text
CLIENTS=claude_code
EXPECTED_CLAUDE_CODE_VERSION=2.1.221
API_BASE_URL=http://127.0.0.1:3000/v1
MODEL_SLUG=glm-5.2
CLIENT_TIMEOUT_SECONDS=300
```

Expected: text and read-only `Read` cases pass through downstream `/v1/messages` compatibility.

- [ ] **Step 2: Run Cline**

Prepend `/tmp/chat2responses-client-pins/node_modules/.bin` to `PATH` inside the smoke process and set:

```text
CLIENTS=cline
EXPECTED_CLINE_VERSION=0.0.13
MODEL_SLUG=glm-5.2
```

Expected: text and read-only tool cases pass.

- [ ] **Step 3: Run Kilo Code**

Use the same temporary `PATH` and set:

```text
CLIENTS=kilo
EXPECTED_KILO_VERSION=7.4.20
MODEL_SLUG=glm-5.2
```

Expected: text and read-only tool cases pass.

- [ ] **Step 4: Run Codex text/tool/delegation separately**

Use three invocations with exactly one `CODEX_TASKS` value each:

```text
text_task
read_only_tool_task
delegation
```

Expected: every invocation reaches `turn.completed`; delegation has one named spawn and one named wait. Do not treat OpenCode or Hermes as substitutes for Codex, Claude Code, Cline, or Kilo Code.

## 9. Task 6: Run The Domestic-Model Matrix

**Files:** no repository edit expected unless a reproducible translator defect has a red test.

Start from the live `/v1/models?format=codex&client_version=0.146.0` catalog through the `test` downstream account. Read only slug, reasoning metadata, and multi-agent capability. Do not print the Authorization header or full response.

Test these deployed slugs serially when routable:

```text
glm-5.2
glm-5.1
deepseek-v4-flash
kimi-k2.6
qwen3.7-plus
MiniMax-M3
```

Also test the active DeepSeek Pro, Qwen, Kimi, or MiniMax slug if the catalog uses a different exact casing/name. Never manufacture a model alias absent from the live catalog.

Minimum cases:

- `glm-5.2` and `glm-5.1`: Codex text, read-only tool, and delegation.
- DeepSeek: Codex text, read-only tool, and delegation on at least one healthy DeepSeek route.
- Kimi, Qwen, MiniMax: Codex text and read-only tool; run delegation when catalog advertises V1 multi-agent support.
- All families: `turn.completed`, no duplicate tool call, no logical 499/502/503 usage row for a successful smoke.

Capability rules:

- Portal default remains `medium` only when verified and advertised.
- Advertise only `low`, `medium`, and `high` where evidence exists.
- Do not infer `xhigh` or `max` from `high`.
- A model with unknown/failed probe evidence remains `none`; do not force reasoning metadata to satisfy a UI expectation.

Operational 429/5xx from one route is not automatically a translator bug. Select another enabled upstream route to avoid capacity/health noise, record only safe category/status metadata, and never retry a request after semantic output.

## 10. Task 7: Reviews, Commit, And Full Repository Gates

- [ ] **Step 1: Specification review**

Dispatch a fresh read-only default subagent. Give it the current diff and this handoff requirements. It must report findings ordered Critical/Important/Minor with exact `file:line` evidence. Fix every Critical/Important item through a new red-green cycle.

- [ ] **Step 2: Code-quality review**

Dispatch a different fresh read-only default subagent. Review shell safety, false positives/negatives, test realism, secret handling, duplicate-request risk, and portability. Fix every Critical/Important item through TDD.

- [ ] **Step 3: Run focused release checks**

```bash
rtk bash -n scripts/installed_client_smoke.sh scripts/codex_delayed_output_smoke.sh scripts/redis_runtime_smoke.sh
rtk cargo test --test scripts
rtk cargo test --test templates
rtk cargo test --test capability_probe
rtk cargo test --test gateway slow_stream
rtk cargo test --test gateway stream_client_cancelled
rtk cargo test --test gateway upstream_concurrency_retry_after
```

- [ ] **Step 4: Run the full gate**

```bash
rtk cargo fmt --check
rtk cargo test --all-targets
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime -- --test-threads=1
rtk cargo test --test gateway slow_stream
rtk cargo test --test gateway stream_client_cancelled
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk npm --prefix frontend test
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk npm --prefix frontend run build
rtk docker compose config
rtk git diff --check
```

Read every exit status. Do not claim a gate passed based on historical results.

- [ ] **Step 5: Commit repository-owned changes**

Stage only files intentionally changed by this continuation. Expected files are initially:

```text
scripts/installed_client_smoke.sh
tests/scripts.rs
docs/superpowers/plans/2026-08-06-glm52-continuation-handoff.md
docs/superpowers/plans/2026-08-06-glm52-continuation-prompt.md
```

Include any additional TDD-backed file only if Task 3 found a real missing config assertion.

Suggested commit:

```bash
rtk git add scripts/installed_client_smoke.sh tests/scripts.rs docs/superpowers/plans/2026-08-06-glm52-continuation-handoff.md docs/superpowers/plans/2026-08-06-glm52-continuation-prompt.md
rtk git commit -m "test(codex): require complete delegation lifecycle"
```

Do not commit the external deployment directory.

## 11. Task 8: Build, Deploy, Health Check, And Push

Use the repository packaging script, not an ad hoc Docker build:

```bash
rtk scripts/build-package-image.sh --image chat-responses-codex --tag latest --output chat-responses-codex-latest.tar
```

- [ ] **Step 1: Inspect the built artifact**

Record the new image digest and tar size. Confirm the image starts with the validated Compose environment before changing deployment state.

- [ ] **Step 2: Preserve the current operator deployment**

Do not rewrite `/home/kavin/docker/chat-responses-codex/.env` or Compose unless a verified repository config change requires it. Preserve the current backup convention under `/home/kavin/docker/chat-responses-codex/backups`.

- [ ] **Step 3: Recreate only the gateway**

Validate config, load/use the new image, and force-recreate only service `gateway`. PostgreSQL and Redis should retain their running containers and volumes.

```bash
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml config
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml up -d --no-deps --force-recreate gateway
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml ps
rtk docker inspect --format '{{json .State.Health}}' chat-responses-codex
rtk curl -fsS http://127.0.0.1:3000/healthz
```

- [ ] **Step 4: Re-run the critical deployed smokes**

After deployment, run at minimum:

```text
glm-5.2 Codex text
glm-5.2 Codex delegation
glm-5.2 Claude Code text and Read
one healthy DeepSeek Codex text/delegation
```

Then run the full installed-client and domestic matrix from Tasks 5-6 if it was previously exercised only against the old image.

- [ ] **Step 5: Push `main`**

```bash
rtk git status --short --branch
rtk git log --oneline origin/main..main
rtk git push origin main
```

Push only after the new commit, full gate, image build, deployment health, and critical deployed smoke all pass.

## 12. Final Report Contract

The completion report must state:

- final commit SHA and remote push result;
- exact test counts for Rust, Redis, and frontend;
- installed-client versions and pass/fail per client;
- domestic model slug and safe result category per test;
- new image digest;
- deployment container health and `/healthz` result;
- any unavailable client/model caused by an external operational failure.

Never include credentials, prompts, tool arguments, raw response/event bodies, billing fields, or encrypted content in the report.
