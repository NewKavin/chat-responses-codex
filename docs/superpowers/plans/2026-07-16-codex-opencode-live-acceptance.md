# Codex And OpenCode Live Acceptance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Validate every final `test` downstream model with exact Codex and OpenCode clients, correct the OpenCode smoke isolation bug, converge configuration without deleting useful adapted models, and record sanitized live evidence.

**Architecture:** Keep one installed-client runner but allow an explicit client subset. Isolate every client from user configuration, especially OpenCode through `OPENCODE_CONFIG_CONTENT` and temporary XDG directories. Apply backend qualification first, run exact clients per exposed model, exclude only version-verified basic-text failures, and treat tool-only failures as capability downgrades rather than model deletion.

**Tech Stack:** Bash, jq, curl, Codex CLI 0.144.0, OpenCode 1.17.9, Rust script contract tests, Docker Compose.

---

## File Map

- Modify `scripts/installed_client_smoke.sh`: selectable clients and fully isolated OpenCode configuration.
- Modify `tests/scripts.rs`: subset, version, isolation, and secret-safety contracts.
- Create `docs/verification/2026-07-16-compatible-model-codex-opencode.md`: sanitized qualification, playground, client, and final-gate evidence.
- Use existing `scripts/deploy.sh`, `scripts/portal_playground_e2e.sh`, and admin APIs; no credential values are written to repository files.

### Task 1: Make Installed-Client Selection Explicit

**Files:**
- Modify: `scripts/installed_client_smoke.sh`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Write failing client-selection contract tests**

Add:

```rust
#[test]
fn installed_client_smoke_accepts_a_validated_client_subset() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains("CLIENTS_JSON=\"${CLIENTS_JSON:-[\\\"codex\\\",\\\"opencode\\\",\\\"claude_code\\\",\\\"hermes\\\"]}\""));
    assert!(script.contains("client_enabled()"));
    assert!(script.contains("jq -e --arg client"));
    assert!(script.contains("unknown client in CLIENTS_JSON"));
}

#[test]
fn installed_client_smoke_keeps_exact_primary_versions() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains("DEFAULT_CODEX_VERSION=\"0.144.0\""));
    assert!(script.contains("DEFAULT_OPENCODE_VERSION=\"1.17.9\""));
}
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk cargo test --test scripts installed_client_smoke -- --nocapture`

Expected: FAIL because client selection is not implemented.

- [ ] **Step 3: Add validated selection helpers**

Near the version constants add:

```bash
CLIENTS_JSON="${CLIENTS_JSON:-[\"codex\",\"opencode\",\"claude_code\",\"hermes\"]}"

if ! jq -e '
  type == "array" and length > 0
  and all(.[]; . as $client
    | type == "string"
    and (["codex", "opencode", "claude_code", "hermes"] | index($client) != null))
' <<<"$CLIENTS_JSON" >/dev/null; then
  printf 'status=invalid_clients message=%s\n' 'unknown client in CLIENTS_JSON' >&2
  exit 1
fi

client_enabled() {
  jq -e --arg client "$1" 'index($client) != null' <<<"$CLIENTS_JSON" >/dev/null
}
```

Resolve and verify binaries only for enabled clients. Run Hermes Python/MCP
preflight only when Hermes is enabled. Guard each client task block with
`if client_enabled <name>; then ... fi`.

- [ ] **Step 4: Run focused tests and syntax check**

Run: `rtk cargo test --test scripts installed_client_smoke -- --nocapture`

Expected: PASS.

Run: `rtk bash -n scripts/installed_client_smoke.sh`

Expected: exit 0.

- [ ] **Step 5: Commit selectable clients**

```bash
rtk git add scripts/installed_client_smoke.sh tests/scripts.rs
rtk git commit -m "test(clients): allow focused compatibility smoke"
```

### Task 2: Isolate OpenCode From User And Project Configuration

**Files:**
- Modify: `scripts/installed_client_smoke.sh`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Write the failing isolation contract**

Add:

```rust
#[test]
fn opencode_smoke_uses_inline_config_and_temporary_xdg_paths() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains("OPENCODE_CONFIG_CONTENT=\"$OPENCODE_CONFIG_CONTENT\""));
    assert!(script.contains("OPENCODE_DISABLE_PROJECT_CONFIG=1"));
    assert!(script.contains("OPENCODE_DISABLE_AUTOUPDATE=1"));
    for name in ["XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
        assert!(script.contains(name), "missing {name}");
    }
    assert!(!script.contains("OPENCODE_CONFIG=\"$OPENCODE_CONFIG_FILE\""));
}
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk cargo test --test scripts opencode_smoke -- --nocapture`

Expected: FAIL because the script passes an explicit file but still permits merged user/project configuration.

- [ ] **Step 3: Replace file configuration with isolated inline content**

Build the existing JSON object into a variable instead of a file:

```bash
OPENCODE_CONFIG_CONTENT="$(jq -nc \
  --arg base_url "$API_BASE_URL" \
  --arg model "$MODEL_SLUG" \
  '{
    model: ("gateway/" + $model),
    small_model: ("gateway/" + $model),
    provider: {
      gateway: {
        npm: "@ai-sdk/openai-compatible",
        name: "Chat Responses Gateway",
        options: {baseURL: $base_url, apiKey: "{env:CHAT2RESPONSES_KEY}"},
        models: {($model): {name: $model}}
      }
    },
    permission: {"*": "deny", read: "allow"}
  }')"
OPENCODE_XDG="$WORKDIR/opencode-xdg"
mkdir -p "$OPENCODE_XDG"/{data,config,state,cache}
```

For both OpenCode commands pass:

```bash
env \
  OPENCODE_CONFIG_CONTENT="$OPENCODE_CONFIG_CONTENT" \
  OPENCODE_DISABLE_PROJECT_CONFIG=1 \
  OPENCODE_DISABLE_AUTOUPDATE=1 \
  XDG_DATA_HOME="$OPENCODE_XDG/data" \
  XDG_CONFIG_HOME="$OPENCODE_XDG/config" \
  XDG_STATE_HOME="$OPENCODE_XDG/state" \
  XDG_CACHE_HOME="$OPENCODE_XDG/cache" \
  CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
  "$OPENCODE_BIN" run --pure --format json ...
```

Do not add `--dangerously-skip-permissions`; the read-only permission is explicit.

- [ ] **Step 4: Run static and exact-version live smoke**

Run: `rtk cargo test --test scripts opencode_smoke -- --nocapture`

Expected: PASS.

With the key supplied through the shell environment, run:

```bash
rtk env \
  PATH="/tmp/chat2responses-client-pins/node_modules/.bin:$PATH" \
  BASE_URL=http://127.0.0.1:3000 \
  DOWNSTREAM_KEY="$DOWNSTREAM_KEY" \
  MODEL_SLUG="Qwen/Qwen3-235B-A22B" \
  CLIENTS_JSON='["opencode"]' \
  scripts/installed_client_smoke.sh
```

Expected: OpenCode 1.17.9 version, text task, and read-only tool task all report `status=passed`; key is absent from output.

- [ ] **Step 5: Commit OpenCode isolation**

```bash
rtk git add scripts/installed_client_smoke.sh tests/scripts.rs
rtk git commit -m "fix(clients): isolate opencode compatibility smoke"
```

### Task 3: Deploy The Implemented Workspace Without Resetting Live State

**Files:**
- No source changes expected.

- [ ] **Step 1: Record the pre-deploy state safely**

Run: `rtk git rev-parse HEAD`

Expected: one commit SHA for the deployment evidence.

Run: `rtk docker ps --format '{{.Names}} {{.Status}}'`

Expected: gateway, PostgreSQL, and Redis containers are present. Do not print `.env` or database credentials.

- [ ] **Step 2: Deploy through the repository script**

Run: `rtk bash scripts/deploy.sh`

Expected: build/deploy exits 0 without recreating or clearing PostgreSQL state.

- [ ] **Step 3: Verify health and the existing downstream**

Run: `rtk curl -fsS http://127.0.0.1:3000/healthz`

Expected: `ok`.

Use admin login in a non-echoing shell and assert downstream `test` exists with a non-empty plaintext key. Print only `downstream=test key=present`.

### Task 4: Qualify And Atomically Apply Live Models

**Files:**
- Evidence only: `/tmp/chat2responses-qualification.json`

- [ ] **Step 1: Call the qualification endpoint**

In a `set +x` shell, obtain an admin token and call:

```json
{
  "apply": true,
  "upstream_ids": [],
  "downstream_id": "test",
  "excluded_models": []
}
```

Save the response with mode 600 at `/tmp/chat2responses-qualification.json`.

- [ ] **Step 2: Validate the safety and availability summary**

Run:

```bash
rtk jq -e '
  .applied == true
  and .summary.retained_models > 0
  and ((.evidence | tostring | test("Bearer |sk-[A-Za-z0-9]")) | not)
' /tmp/chat2responses-qualification.json
```

Expected: `true`.

- [ ] **Step 3: Fetch the final exposed catalog**

Using the current `test` key without printing it, save `/v1/models` to
`/tmp/chat2responses-final-models.json` with mode 600. Assert every returned ID
is present in the qualification retained set and the list is non-empty.

### Task 5: Run Codex And OpenCode On Every Exposed Model

**Files:**
- Evidence only: `/tmp/chat2responses-client-results.jsonl`

- [ ] **Step 1: Install exact clients in a temporary prefix**

Run:

```bash
rtk npm install --prefix /tmp/chat2responses-client-pins \
  @openai/codex@0.144.0 opencode-ai@1.17.9 --no-audit --no-fund
```

Expected: exit 0.

Run:

```bash
rtk bash -lc 'export PATH=/tmp/chat2responses-client-pins/node_modules/.bin:$PATH; codex --version; opencode --version'
```

Expected: `codex-cli 0.144.0` and `1.17.9`.

- [ ] **Step 2: Run both clients for each final model**

Read model IDs from `/tmp/chat2responses-final-models.json`. For each model run
the installed-client script with:

```bash
CLIENTS_JSON='["codex","opencode"]'
```

Capture only client, version, model, task, exit status, duration, sanitized
event types, and pass/fail into the mode-600 JSONL file. Do not capture prompts,
outputs, tool arguments/results, or keys.

- [ ] **Step 3: Separate basic-text failures from tool-only failures**

Build two arrays:

- `text_failed_models`: either client's version-verified text task failed after reaching the gateway.
- `tool_downgraded_models`: text passed but a read-only tool loop failed.

Local install/config/prerequisite failures go to `infrastructure_failures` and
must not alter model configuration.

- [ ] **Step 4: Exclude only confirmed basic-text failures**

If `text_failed_models` is non-empty, call qualification again with
`excluded_models` set to that exact unique array and `apply:true`. The backend
zero-result guard must reject an exclusion that would remove the final model.

For tool-only failures, queue a capability probe for the selected route and
verify the live catalog does not advertise unsupported tool capability; retain
the text-usable model.

- [ ] **Step 5: Re-fetch and re-run the converged set**

Fetch `/v1/models` again and rerun Codex/OpenCode text tasks for every remaining
model. Expected: non-empty catalog and zero text failures.

### Task 6: Run The Non-Mutating Playground Smoke

**Files:**
- No repository changes expected.

- [ ] **Step 1: Hash the current key without printing it**

Compute an in-memory SHA-256 digest of `DOWNSTREAM_KEY` before the test; print
only the digest prefix if needed for comparison.

- [ ] **Step 2: Run playground E2E**

Run:

```bash
rtk env BASE_URL=http://127.0.0.1:3000 \
  DOWNSTREAM_KEY="$DOWNSTREAM_KEY" \
  scripts/portal_playground_e2e.sh
```

Expected: health, live model catalog, and at least one minimal streaming request pass.

- [ ] **Step 3: Prove credential stability**

Fetch the `test` downstream key again through the non-printing admin flow and
assert its SHA-256 digest equals the pre-test digest.

### Task 7: Record Sanitized Evidence

**Files:**
- Create: `docs/verification/2026-07-16-compatible-model-codex-opencode.md`

- [ ] **Step 1: Write the evidence document**

Record:

- deployed commit and timestamp
- count of active upstreams and retained/full/adapted/removed/operational models
- final exposed model slugs
- exact Codex/OpenCode versions
- per-model text/tool pass or capability downgrade
- playground model-count/intersection and E2E outcome
- qualification safety-guard outcome
- verification commands and aggregate counts

Do not include credentials, prompts, response/reasoning text, tool
arguments/results, upstream URLs, image URLs/data, or raw error bodies.

- [ ] **Step 2: Scan the evidence for secret-like material**

Run:

```bash
rtk rg -n -e 'Bearer[[:space:]]+[A-Za-z0-9._-]+' \
  -e 'sk-[A-Za-z0-9_-]{12,}' \
  docs/verification/2026-07-16-compatible-model-codex-opencode.md
```

Expected: no matches.

- [ ] **Step 3: Commit evidence**

```bash
rtk git add docs/verification/2026-07-16-compatible-model-codex-opencode.md
rtk git commit -m "test: verify live models with codex and opencode"
```

### Task 8: Full Completion Gate

**Files:**
- No source changes expected.

- [ ] **Step 1: Format and lint**

Run: `rtk cargo fmt --all -- --check`

Expected: exit 0.

Run: `rtk cargo clippy --locked --all-targets --all-features -- -D warnings`

Expected: no issues.

- [ ] **Step 2: Run all Rust tests**

Run: `rtk cargo test --locked --all-targets -- --nocapture`

Expected: all non-ignored tests pass.

Run: `rtk cargo test --manifest-path crates/gateway-core/Cargo.toml --all-targets -- --nocapture`

Expected: all shared-crate tests pass.

- [ ] **Step 3: Run frontend tests and build**

Run: `rtk npm --prefix frontend exec vitest run`

Expected: all frontend tests pass.

Run: `rtk npm --prefix frontend run build`

Expected: exit 0.

- [ ] **Step 4: Run script syntax checks**

Run:

```bash
rtk bash -n scripts/installed_client_smoke.sh scripts/portal_playground_e2e.sh
```

Expected: exit 0.

- [ ] **Step 5: Verify generic production routing**

Run:

```bash
rtk rg -n -i 'deepseek|minimax|moonshot|kimi|qwen|zhipu|glm|grok|sonnet|haiku' \
  src --glob '*.rs'
```

Expected: no model/provider classifier in production dispatch or qualification;
legitimate downstream protocol naming must be reviewed separately.

- [ ] **Step 6: Verify clean worktree and final commits**

Run: `rtk git status --short`

Expected: clean.

Run: `rtk git log -8 --oneline`

Expected: portal, qualification, client isolation, and evidence commits are visible.
