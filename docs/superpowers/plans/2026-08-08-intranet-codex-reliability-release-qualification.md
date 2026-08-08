# Intranet Codex Reliability Release Qualification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build, deploy, and qualify the reliability repair with immutable image selection, preserved deployment state, deterministic eight-account load, real persisted Codex resume, long-context coverage, and a secret-free evidence manifest.

**Architecture:** Keep release orchestration in repository scripts. Compose accepts an explicit image reference, while `deploy.sh` adds a generated image override, replaces only the gateway after dependencies are healthy, waits for `/healthz`, and confirms the startup repair log. Deterministic Rust tests prove physical concurrency; focused shell smokes prove installed Codex behavior and collect live PostgreSQL, Redis, gateway, capability, and image evidence.

**Tech Stack:** Bash, Docker Compose, Rust/Tokio/Axum, PostgreSQL 15, Redis 7, jq, curl, Codex CLI 0.146.0.

---

## File Structure

- `docker-compose.yml`: selects `${GATEWAY_IMAGE}` while preserving the current default.
- `scripts/deploy.sh`: preserves operator configuration and volumes, applies the requested image tag, replaces one gateway, waits for health, and reports startup repair counts.
- `tests/scripts.rs`: executes deployment and qualification scripts against fake tools without exposing secrets.
- `tests/load.rs`: owns the deterministic 1,000-request, eight-account physical-concurrency acceptance test.
- `scripts/installed_client_smoke.sh`: adds an explicit Codex reasoning case and optionally retains sanitized JSONL evidence.
- `scripts/codex_resume_smoke.sh`: creates a persisted Codex session and resumes it through the real CLI command.
- `scripts/codex_tui_smoke.sh`: records the operator's primary TUI workflow, including the in-TUI `/resume` picker.
- `scripts/reliability_live_soak.sh`: runs the authorized 1,000-request concurrency-ten Responses soak.
- `scripts/reliability_context_matrix.sh`: runs serial 32k/64k/128k/configured-max text, reasoning, and tool-history cases three times.
- `scripts/reliability_qualification.sh`: orchestrates smokes and writes the evidence manifest.
- `.env.example`: documents release health and qualification settings without credentials.
- `DEPLOYMENT.md`: documents the scripted build, deployment, qualification, rollback, and evidence paths.

### Task 1: Deploy The Requested Image Without Replacing Persistent State

**Files:**
- Modify: `docker-compose.yml:38-42`
- Modify: `scripts/deploy.sh`
- Modify: `tests/scripts.rs`

- [ ] **Step 1: Add failing image-selection and deployment-lifecycle tests**

Add these tests to `tests/scripts.rs`:

```rust
#[test]
fn compose_uses_the_release_selected_gateway_image() {
    let compose = fs::read_to_string("docker-compose.yml").unwrap();
    assert!(compose.contains(
        "image: ${GATEWAY_IMAGE:-chat-responses-codex:latest}"
    ));
    assert!(!compose.contains("image: chat-responses-codex:latest\n"));
}

#[test]
fn deploy_preserves_env_selects_tag_replaces_gateway_and_waits_for_health() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let scripts = repo.join("scripts");
    let deploy = temp.path().join("deploy");
    let fake_bin = temp.path().join("bin");
    let trace = temp.path().join("trace.txt");
    fs::create_dir_all(&scripts).unwrap();
    fs::create_dir_all(&deploy).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(repo.join("docker-compose.yml"), fs::read("docker-compose.yml").unwrap()).unwrap();
    fs::write(repo.join(".env.example"), "POSTGRES_PASSWORD=example\n").unwrap();
    write_executable(
        &scripts.join("deploy.sh"),
        &fs::read_to_string("scripts/deploy.sh").unwrap(),
    );
    write_executable(
        &scripts.join("build-package-image.sh"),
        "#!/usr/bin/env bash\nexit 99\n",
    );
    fs::write(
        deploy.join("docker-compose.yml"),
        "services:\n  gateway:\n    image: chat-responses-codex:latest\n  postgres:\n    image: postgres:15\n",
    )
    .unwrap();
    fs::write(
        deploy.join(".env"),
        "POSTGRES_PASSWORD=preserve-me\nREDIS_ENABLED=false\n",
    )
    .unwrap();
    let before = fs::read(deploy.join(".env")).unwrap();

    write_executable(
        &fake_bin.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "compose" && "${2:-}" == "version" ]]; then exit 0; fi
printf 'docker' >>"$TOOL_TRACE"
printf '\t%s' "$@" >>"$TOOL_TRACE"
printf '\n' >>"$TOOL_TRACE"
if [[ " $* " == *" logs "* ]]; then
  printf 'legacy local-admission route-health repair complete scanned_routes=7 repaired_routes=2\n'
fi
if [[ " $* " == *" images -q gateway "* ]]; then printf 'candidate-id\n'; fi
if [[ "${1:-}" == "inspect" ]]; then printf 'release-image:release-tag\n'; fi
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf '{\"status\":\"ok\"}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/deploy.sh")
        .args([
            "--deploy-dir",
            deploy.to_str().unwrap(),
            "--image",
            "release-image",
            "--tag",
            "release-tag",
            "--skip-build",
        ])
        .current_dir(&repo)
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("TOOL_TRACE", &trace)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(deploy.join(".env")).unwrap(), before);
    assert_eq!(
        fs::read_to_string(deploy.join("gateway-image.override.yml")).unwrap(),
        "services:\n  gateway:\n    image: ${GATEWAY_IMAGE}\n"
    );
    let trace = fs::read_to_string(trace).unwrap();
    assert!(trace.contains("up\t-d\t--remove-orphans\tpostgres"));
    assert!(trace.contains("up\t-d\t--no-deps\t--force-recreate\tgateway"));
    assert!(trace.contains("images\t-q\tgateway"));
    assert!(!trace.contains("\tdown\t"));
    assert!(!trace.contains("\t-v\t"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("scanned_routes=7 repaired_routes=2"));
}
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test scripts compose_uses_the_release_selected_gateway_image
rtk cargo test --test scripts deploy_preserves_env_selects_tag_replaces_gateway_and_waits_for_health
```

Expected: Compose is hard-coded to `latest`, and `deploy.sh` neither creates the override nor waits for health.

- [ ] **Step 3: Make Compose image selection explicit**

Replace the gateway image line in `docker-compose.yml` with:

```yaml
    image: ${GATEWAY_IMAGE:-chat-responses-codex:latest}
```

- [ ] **Step 4: Generate the image override and preserve operator files**

In `scripts/deploy.sh`, add `curl` to required commands, never overwrite an existing `.env`, and write this generated file after argument validation:

```bash
DEPLOY_IMAGE_OVERRIDE="$DEPLOY_DIR/gateway-image.override.yml"
GATEWAY_IMAGE="${IMAGE_NAME}:${IMAGE_TAG}"
export GATEWAY_IMAGE

cat >"$DEPLOY_IMAGE_OVERRIDE" <<'EOF'
services:
  gateway:
    image: ${GATEWAY_IMAGE}
EOF

COMPOSE_BASE=(
  "${COMPOSE[@]}"
  --env-file "$DEPLOY_ENV"
  -f "$DEPLOY_COMPOSE"
  -f "$DEPLOY_IMAGE_OVERRIDE"
  --project-directory "$DEPLOY_DIR"
)
```

Keep `--force-copy-config` for replacing only `docker-compose.yml`. Delete its branch that copies `.env`; an existing `.env` is immutable under every deploy option.

- [ ] **Step 5: Replace only the gateway and wait for the startup contract**

Replace the final Compose invocation with:

```bash
read_env_value() {
  local name="$1"
  sed -n "s/^${name}=//p" "$DEPLOY_ENV" | tail -n 1 | tr -d '\r'
}

dependencies=(postgres)
if [[ "$(read_env_value REDIS_ENABLED)" == "true" ]]; then
  dependencies+=(redis)
fi

DEPLOY_STARTED_AT="$(date --iso-8601=seconds)"
"${COMPOSE_BASE[@]}" up -d --remove-orphans "${dependencies[@]}"
"${COMPOSE_BASE[@]}" up -d --no-deps --force-recreate gateway

HEALTHCHECK_URL="${GATEWAY_HEALTHCHECK_URL:-http://127.0.0.1:3001/healthz}"
healthy=0
for _ in $(seq 1 "${DEPLOY_HEALTH_MAX_ATTEMPTS:-60}"); do
  if curl -fsS --connect-timeout 2 --max-time 3 "$HEALTHCHECK_URL" >/dev/null; then
    healthy=1
    break
  fi
  sleep "${DEPLOY_HEALTH_INTERVAL_SECONDS:-2}"
done
if [[ "$healthy" -ne 1 ]]; then
  "${COMPOSE_BASE[@]}" logs --since "$DEPLOY_STARTED_AT" gateway >&2 || true
  echo "Error: gateway did not become healthy at $HEALTHCHECK_URL" >&2
  exit 1
fi

container_id="$("${COMPOSE_BASE[@]}" images -q gateway)"
running_image="$(docker inspect --format '{{.Config.Image}}' "$container_id")"
if [[ "$running_image" != "$GATEWAY_IMAGE" ]]; then
  echo "Error: gateway is running $running_image instead of $GATEWAY_IMAGE" >&2
  exit 1
fi

startup_logs="$("${COMPOSE_BASE[@]}" logs --since "$DEPLOY_STARTED_AT" gateway)"
repair_summary="$(grep -F 'legacy local-admission route-health repair complete' <<<"$startup_logs" | tail -n 1 || true)"
if [[ -z "$repair_summary" ]]; then
  echo "Error: startup legacy route-health repair summary is missing" >&2
  exit 1
fi
log "$repair_summary"
log "Deployment healthy with image $running_image"
"${COMPOSE_BASE[@]}" ps
```

The admission plan must emit that exact message once for both local and Redis coordination, with only `redis_prefix`, `scanned_routes`, and `repaired_routes` fields.

- [ ] **Step 6: Run and commit the deploy hardening**

```bash
rtk bash -n scripts/deploy.sh
rtk docker compose --env-file .env.example config --quiet
rtk cargo test --test scripts compose_uses_the_release_selected_gateway_image
rtk cargo test --test scripts deploy_preserves_env_selects_tag_replaces_gateway_and_waits_for_health
rtk git add docker-compose.yml scripts/deploy.sh tests/scripts.rs
rtk git commit -m "fix(deploy): run and verify the selected gateway image" -m "Constraint: Preserve .env and all PostgreSQL and Redis volumes" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 2: Prove Eight Four-Slot Accounts Sustain Ten-Way Load

**Files:**
- Modify: `tests/load.rs`

- [ ] **Step 1: Add the failing deterministic load test**

Add these helpers and test to `tests/load.rs`:

```rust
#[derive(Default)]
struct AccountPhysicalLoad {
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    completed: AtomicUsize,
}

fn observe_max(target: &AtomicUsize, value: usize) {
    let mut seen = target.load(Ordering::SeqCst);
    while value > seen {
        match target.compare_exchange(seen, value, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break,
            Err(actual) => seen = actual,
        }
    }
}

#[tokio::test]
async fn eight_four_slot_accounts_complete_one_thousand_requests_at_concurrency_ten() {
    const ACCOUNT_COUNT: usize = 8;
    const ACCOUNT_LIMIT: u32 = 4;
    const TOTAL_REQUESTS: usize = 1_000;
    const DOWNSTREAM_CONCURRENCY: usize = 10;

    let tempdir = tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let physical: Arc<[AccountPhysicalLoad; ACCOUNT_COUNT]> =
        Arc::new(std::array::from_fn(|_| AccountPhysicalLoad::default()));
    let aggregate_in_flight = Arc::new(AtomicUsize::new(0));
    let aggregate_max_in_flight = Arc::new(AtomicUsize::new(0));
    let upstream_keys = (0..ACCOUNT_COUNT)
        .map(|index| format!("account-key-{index}"))
        .collect::<Vec<_>>();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let physical_for_handler = physical.clone();
    let aggregate_for_handler = aggregate_in_flight.clone();
    let aggregate_max_for_handler = aggregate_max_in_flight.clone();
    let keys_for_handler = upstream_keys.clone();
    let upstream_app = Router::new().route(
        "/v1/responses",
        post(move |headers: axum::http::HeaderMap| {
            let physical = physical_for_handler.clone();
            let aggregate = aggregate_for_handler.clone();
            let aggregate_max = aggregate_max_for_handler.clone();
            let keys = keys_for_handler.clone();
            async move {
                let authorization = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap();
                let account = keys
                    .iter()
                    .position(|key| authorization == format!("Bearer {key}"))
                    .expect("known account key");
                let current = physical[account].in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                observe_max(&physical[account].max_in_flight, current);
                let aggregate_current = aggregate.fetch_add(1, Ordering::SeqCst) + 1;
                observe_max(&aggregate_max, aggregate_current);
                tokio::time::sleep(Duration::from_millis(20)).await;
                aggregate.fetch_sub(1, Ordering::SeqCst);
                physical[account].in_flight.fetch_sub(1, Ordering::SeqCst);
                physical[account].completed.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": format!("resp-load-{account}"),
                        "object": "response",
                        "status": "completed",
                        "model": "deepseek-v4-flash",
                        "output": [{
                            "id": format!("msg-load-{account}"),
                            "type": "message",
                            "role": "assistant",
                            "status": "completed",
                            "content": [{"type": "output_text", "text": "ok", "annotations": []}]
                        }],
                        "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
                    })),
                )
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, upstream_app).await.unwrap() });

    let downstream_key = generate_downstream_key("gw");
    let upstream = UpstreamConfig {
        id: "shared-provider".into(),
        name: "eight-account-provider".into(),
        base_url: format!("http://{upstream_addr}"),
        api_key: upstream_keys[0].clone(),
        api_keys: upstream_keys[1..].to_vec(),
        protocol: UpstreamProtocol::Responses,
        protocols: vec![UpstreamProtocol::Responses],
        supported_models: vec!["deepseek-v4-flash".into()],
        request_quota_window_hours: 5,
        request_quota_requests: 100_000,
        requests_per_minute: 100_000,
        max_concurrency: ACCOUNT_LIMIT,
        active: true,
        ..Default::default()
    };
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream.clone()]),
            downstreams: Arc::new(vec![DownstreamConfig {
                id: "down-1".into(),
                name: "codex-team".into(),
                hash: downstream_key.hash.clone(),
                plaintext_key: Some(downstream_key.plaintext.clone()),
                plaintext_key_prefix: None,
                model_allowlist: vec!["deepseek-v4-flash".into()],
                per_minute_limit: 100_000,
                rate_limit_enabled: true,
                max_concurrency: DOWNSTREAM_CONCURRENCY as u32,
                daily_token_limit: None,
                monthly_token_limit: None,
                input_token_price_per_million_cents: None,
                output_token_price_per_million_cents: None,
                daily_cost_limit_cents: None,
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
                billing_mode: "request".into(),
            }]),
            usage_logs: vec![],
            announcement: None,
            global_context_profiles: Arc::new(std::collections::HashMap::new()),
        },
        state_path,
        AppConfig::default(),
    );
    for key in &upstream_keys {
        let mut profile = UpstreamDialectProfile::unknown(DialectProfileKey {
            upstream_id: upstream.id.clone(),
            key_fingerprint: upstream_key_fingerprint(&upstream.id, key),
            runtime_model_slug: "deepseek-v4-flash".into(),
            protocol: WireProtocol::Responses,
        });
        profile.state = DialectProfileState::Verified;
        profile.capabilities.insert(Capability::TextInput, EvidenceState::Supported);
        profile.capabilities.insert(Capability::TextStream, EvidenceState::Supported);
        stamp_load_profile(&state, &mut profile).await;
        state.upsert_dialect_profile(profile).await.unwrap();
    }

    let app = build_router(state.clone());
    let statuses = stream::iter(0..TOTAL_REQUESTS)
        .map(|index| {
            let app = app.clone();
            let secret = downstream_key.plaintext.clone();
            async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/v1/responses")
                            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(json!({
                                "model": "deepseek-v4-flash",
                                "input": format!("load request {index}"),
                                "stream": false
                            }).to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = response.status();
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
                status
            }
        })
        .buffer_unordered(DOWNSTREAM_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    assert_eq!(statuses.len(), TOTAL_REQUESTS);
    assert!(statuses.iter().all(|status| *status == StatusCode::OK));
    assert_eq!(
        physical.iter().map(|account| account.completed.load(Ordering::SeqCst)).sum::<usize>(),
        TOTAL_REQUESTS
    );
    for account in physical.iter() {
        assert!(account.max_in_flight.load(Ordering::SeqCst) <= ACCOUNT_LIMIT as usize);
    }
    assert_eq!(
        aggregate_max_in_flight.load(Ordering::SeqCst),
        DOWNSTREAM_CONCURRENCY,
        "the test must prove physical concurrency exceeds the old upstream-wide cap of four",
    );
    let health = state.route_health_snapshots(&[upstream]).await.unwrap();
    assert_eq!(health["shared-provider"].cooldown_routes, 0);
    assert!(health["shared-provider"].failure_classes.is_empty());
}
```

- [ ] **Step 2: Verify RED against the old local-admission behavior**

```bash
rtk cargo test --test load eight_four_slot_accounts_complete_one_thousand_requests_at_concurrency_ten -- --nocapture
```

Expected: before the admission fix, at least one status is a gateway scheduling failure or route health contains `concurrency_saturated`.

- [ ] **Step 3: Run GREEN with Redis scheduling coverage**

```bash
rtk cargo test --test load eight_four_slot_accounts_complete_one_thousand_requests_at_concurrency_ten -- --nocapture
rtk cargo test --test redis_runtime redis_upstream_concurrency_is_scoped_per_account
rtk cargo test --test redis_runtime redis_gateway_local_capacity_release_is_immediately_schedulable
rtk cargo test --test gateway explicit_concurrency_5xx_uses_account_recovery_and_healthy_routes
rtk cargo test --test gateway generic_503_remains_bounded_transient_failure
```

Expected: all tests pass; no account exceeds four physical requests and no request returns 429/502/503.

- [ ] **Step 4: Commit deterministic qualification**

```bash
rtk git add tests/load.rs
rtk git commit -m "test(load): qualify eight-account Codex concurrency" -m "Constraint: One thousand requests at downstream concurrency ten" -m "Confidence: high" -m "Scope-risk: narrow"
```

### Task 3: Preserve Codex JSONL And Exercise Real Reasoning And Resume

**Files:**
- Modify: `scripts/installed_client_smoke.sh`
- Create: `scripts/codex_resume_smoke.sh`
- Create: `scripts/codex_tui_smoke.sh`
- Modify: `tests/scripts.rs`

- [ ] **Step 1: Add failing script contract tests**

Add to `tests/scripts.rs`:

```rust
#[test]
fn codex_release_smokes_require_reasoning_persistence_and_real_resume() {
    let installed = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(installed.contains(
        "text_task,reasoning_task,read_only_tool_task,delegation"
    ));
    assert!(installed.contains("ARTIFACT_DIR"));
    assert!(installed.contains("codex-reasoning.jsonl"));

    let resume = fs::read_to_string("scripts/codex_resume_smoke.sh").unwrap();
    assert!(resume.contains("codex exec resume --last"));
    assert!(!resume.contains("--ephemeral"));
    assert!(resume.contains("turn.completed"));
    assert!(resume.contains("resume-initial.jsonl"));
    assert!(resume.contains("resume-final.jsonl"));

    let tui = fs::read_to_string("scripts/codex_tui_smoke.sh").unwrap();
    assert!(tui.contains("codex-tui-initial.typescript"));
    assert!(tui.contains("codex-tui-resume.typescript"));
    assert!(tui.contains("codex-tui-result.txt"));
    assert!(tui.contains("Type /resume in the TUI"));
    assert!(tui.contains("[[ -t 0 && -t 1 ]]"));
}
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test scripts codex_release_smokes_require_reasoning_persistence_and_real_resume
```

Expected: the reasoning task, artifact export, and resume script do not exist.

- [ ] **Step 3: Add an explicit reasoning task and artifact export**

Change the default and validation list in `scripts/installed_client_smoke.sh` to:

```bash
CODEX_TASKS="${CODEX_TASKS:-text_task,reasoning_task,read_only_tool_task,delegation}"
```

and allow `reasoning_task`. Add:

```bash
archive_codex_artifacts() {
  [[ -n "${ARTIFACT_DIR:-}" ]] || return 0
  mkdir -p "$ARTIFACT_DIR"
  chmod 700 "$ARTIFACT_DIR"
  find "$WORKDIR" -maxdepth 1 -type f -name 'codex-*.jsonl' -exec cp {} "$ARTIFACT_DIR/" \;
}

cleanup() {
  local status=$?
  archive_codex_artifacts
  rm -rf "$WORKDIR"
  exit "$status"
}
```

Add the case after `text_task`:

```bash
if codex_task_enabled reasoning_task; then
  REASONING_MARKER="CLIENT_REASONING_SMOKE_OK"
  REASONING_PROMPT="Reason step by step about why a gateway must not replay a tool call after semantic output. Give one invariant and one failure example. End with exactly ${REASONING_MARKER} on its own line."
  record_codex_case codex reasoning_task "$REASONING_MARKER" "$WORKDIR/codex-reasoning.jsonl" \
    env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
    "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
    --cd "$TASKDIR" --model "$MODEL_SLUG" "$REASONING_PROMPT"
  jq -Rne '
    [inputs | fromjson?] as $events
    | any($events[]; .type == "item.completed" and .item.type == "reasoning")
      and any($events[]; .type == "turn.completed")
  ' "$WORKDIR/codex-reasoning.jsonl" >/dev/null || {
    printf 'client=codex task=reasoning_task status=missing_reasoning_event\n' >&2
    exit 1
  }
fi
```

- [ ] **Step 4: Create the complete persisted-resume smoke**

Create `scripts/codex_resume_smoke.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
set +x

: "${API_BASE_URL:?API_BASE_URL is required}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"
: "${OUTPUT_DIR:?OUTPUT_DIR is required}"

CODEX_BIN="${CODEX_BIN:-codex}"
CODEX_VERSION="${EXPECTED_CODEX_VERSION:-0.146.0}"
CLIENT_TIMEOUT_SECONDS="${CLIENT_TIMEOUT_SECONDS:-900}"
umask 077
mkdir -p "$OUTPUT_DIR"
WORKDIR="$(mktemp -d)"
TASKDIR="$WORKDIR/workspace"
CODEX_HOME_DIR="$WORKDIR/codex-home"
mkdir -p "$TASKDIR" "$CODEX_HOME_DIR"
trap 'rm -rf "$WORKDIR"' EXIT

for command in curl jq timeout "$CODEX_BIN"; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'status=missing_command command=%s\n' "$command" >&2
    exit 1
  }
done

API_BASE_URL="${API_BASE_URL%/}"
curl -fsS --connect-timeout 5 --max-time 30 \
  "$API_BASE_URL/models?format=codex&client_version=$CODEX_VERSION" \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" >"$CODEX_HOME_DIR/model-catalog.json"
MODEL_TOML="$(jq -Rn --arg value "$MODEL_SLUG" '$value')"
EFFORT="$(jq -er --arg model "$MODEL_SLUG" '
  .models[] | select((.slug // .id) == $model) | .default_reasoning_level
' "$CODEX_HOME_DIR/model-catalog.json")"
EFFORT_TOML="$(jq -Rn --arg value "$EFFORT" '$value')"
API_BASE_TOML="$(jq -Rn --arg value "$API_BASE_URL" '$value')"
cat >"$CODEX_HOME_DIR/config.toml" <<EOF
model_provider = "gateway"
model = $MODEL_TOML
model_reasoning_effort = $EFFORT_TOML
model_catalog_json = "model-catalog.json"
web_search = "disabled"

[model_providers.gateway]
name = "chat-responses-gateway"
base_url = $API_BASE_TOML
wire_api = "responses"
requires_openai_auth = true
stream_idle_timeout_ms = 3600000
stream_max_retries = 2
EOF

if [[ "${CODEX_SKIP_LOGIN:-0}" != "1" ]]; then
  printf '%s' "$DOWNSTREAM_KEY" \
    | env CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" login --with-api-key \
      >"$OUTPUT_DIR/resume-login.log" 2>&1
fi

MARKER="resume-$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
printf '%s\n' "$MARKER" >"$TASKDIR/resume-probe.txt"
INITIAL_PROMPT="Read resume-probe.txt with a read-only filesystem tool. Explain in one sentence that this marker must remain in session history, then print exactly $MARKER on its own line."
RESUME_PROMPT="Using the prior turn's tool result without reading the file again, print exactly $MARKER on its own line."

timeout --kill-after=30s "$CLIENT_TIMEOUT_SECONDS" \
  env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
  "$CODEX_BIN" exec --json --skip-git-repo-check --sandbox read-only \
  --cd "$TASKDIR" --model "$MODEL_SLUG" "$INITIAL_PROMPT" \
  >"$OUTPUT_DIR/resume-initial.jsonl" 2>&1

timeout --kill-after=30s "$CLIENT_TIMEOUT_SECONDS" \
  env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
  "$CODEX_BIN" exec resume --last "$RESUME_PROMPT" --json \
  --skip-git-repo-check --model "$MODEL_SLUG" \
  >"$OUTPUT_DIR/resume-final.jsonl" 2>&1

for file in resume-initial.jsonl resume-final.jsonl; do
  jq -Rne --arg marker "$MARKER" '
    [inputs | fromjson?] as $events
    | any($events[]; .type == "turn.completed")
      and any($events[];
        .type == "item.completed"
        and .item.type == "agent_message"
        and (.item.text | contains($marker)))
  ' "$OUTPUT_DIR/$file" >/dev/null || {
    printf 'status=invalid_resume_jsonl file=%s\n' "$file" >&2
    exit 1
  }
done

printf 'status=passed initial=turn.completed resume=turn.completed\n'
```

This deliberately omits `--ephemeral`; the resume invocation is the locally verified CLI form `codex exec resume --last [PROMPT] --json`.

- [ ] **Step 5: Create the interactive TUI and in-TUI `/resume` smoke**

Create `scripts/codex_tui_smoke.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
set +x

: "${API_BASE_URL:?API_BASE_URL is required}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"
: "${OUTPUT_DIR:?OUTPUT_DIR is required}"
[[ -t 0 && -t 1 ]] || {
  printf 'status=failed reason=tui_requires_interactive_terminal\n' >&2
  exit 1
}

CODEX_BIN="$(readlink -f "${CODEX_BIN:-$(command -v codex)}")"
CODEX_VERSION="${EXPECTED_CODEX_VERSION:-0.146.0}"
umask 077
mkdir -p "$OUTPUT_DIR"
WORKDIR="$(mktemp -d)"
TASKDIR="$WORKDIR/workspace"
CODEX_HOME_DIR="$WORKDIR/codex-home"
mkdir -p "$TASKDIR" "$CODEX_HOME_DIR"
trap 'rm -rf "$WORKDIR"' EXIT

for command in curl jq script readlink; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'status=missing_command command=%s\n' "$command" >&2
    exit 1
  }
done

API_BASE_URL="${API_BASE_URL%/}"
curl -fsS --connect-timeout 5 --max-time 30 \
  "$API_BASE_URL/models?format=codex&client_version=$CODEX_VERSION" \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" >"$CODEX_HOME_DIR/model-catalog.json"
MODEL_TOML="$(jq -Rn --arg value "$MODEL_SLUG" '$value')"
EFFORT="$(jq -er --arg model "$MODEL_SLUG" '
  .models[] | select((.slug // .id) == $model) | .default_reasoning_level
' "$CODEX_HOME_DIR/model-catalog.json")"
EFFORT_TOML="$(jq -Rn --arg value "$EFFORT" '$value')"
API_BASE_TOML="$(jq -Rn --arg value "$API_BASE_URL" '$value')"
cat >"$CODEX_HOME_DIR/config.toml" <<EOF
model_provider = "gateway"
model = $MODEL_TOML
model_reasoning_effort = $EFFORT_TOML
model_catalog_json = "model-catalog.json"
web_search = "disabled"

[features]
multi_agent = true
multi_agent_v2 = false

[model_providers.gateway]
name = "chat-responses-gateway"
base_url = $API_BASE_TOML
wire_api = "responses"
requires_openai_auth = true
stream_idle_timeout_ms = 3600000
stream_max_retries = 2
EOF
printf '%s' "$DOWNSTREAM_KEY" \
  | env CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" login --with-api-key \
    >"$OUTPUT_DIR/codex-tui-login.log" 2>&1

MARKER="tui-resume-$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
printf '%s\n' "$MARKER" >"$TASKDIR/tui-probe.txt"
INITIAL_PROMPT="Read tui-probe.txt with a read-only tool, reason briefly about preserving its tool result, and end with exactly $MARKER on its own line."

export CODEX_HOME="$CODEX_HOME_DIR"
export CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY"
printf '\nTUI phase 1: wait for the completed answer containing %s, then type /exit.\n' "$MARKER"
initial_command="$(printf '%q ' "$CODEX_BIN" --no-alt-screen -C "$TASKDIR" -s read-only -m "$MODEL_SLUG" "$INITIAL_PROMPT")"
script -qefc "$initial_command" "$OUTPUT_DIR/codex-tui-initial.typescript"

printf '\nTUI phase 2: Type /resume in the TUI, select the phase-1 session, ask it to print %s from prior history without rereading the file, wait for completion, then type /exit.\n' "$MARKER"
resume_command="$(printf '%q ' "$CODEX_BIN" --no-alt-screen -C "$TASKDIR" -s read-only -m "$MODEL_SLUG")"
script -qefc "$resume_command" "$OUTPUT_DIR/codex-tui-resume.typescript"

initial_markers="$(grep -aoF "$MARKER" "$OUTPUT_DIR/codex-tui-initial.typescript" | wc -l | tr -d ' ' || true)"
resume_markers="$(grep -aoF "$MARKER" "$OUTPUT_DIR/codex-tui-resume.typescript" | wc -l | tr -d ' ' || true)"
resume_commands="$(grep -aoF '/resume' "$OUTPUT_DIR/codex-tui-resume.typescript" | wc -l | tr -d ' ' || true)"
if (( initial_markers < 2 || resume_markers < 2 || resume_commands < 1 )); then
  printf 'status=failed initial_markers=%s resume_markers=%s resume_commands=%s\n' \
    "$initial_markers" "$resume_markers" "$resume_commands" >&2
  exit 1
fi
printf 'status=passed client=codex_tui resume=/resume marker=%s\n' "$MARKER" \
  | tee "$OUTPUT_DIR/codex-tui-result.txt"
```

This is an intentionally interactive release gate. The operator waits for each `turn.completed` UI state before `/exit`; the surrounding qualification window then verifies that both turns produced successful PostgreSQL usage rows and no logical 429/502/503.

- [ ] **Step 6: Run and commit the Codex smokes**

```bash
rtk bash -n scripts/installed_client_smoke.sh
rtk bash -n scripts/codex_resume_smoke.sh
rtk bash -n scripts/codex_tui_smoke.sh
rtk cargo test --test scripts codex_release_smokes_require_reasoning_persistence_and_real_resume
rtk cargo test --test scripts installed_client_smoke
rtk git add scripts/installed_client_smoke.sh scripts/codex_resume_smoke.sh scripts/codex_tui_smoke.sh tests/scripts.rs
rtk git commit -m "test(codex): retain reasoning and real resume evidence" -m "Constraint: Resume uses a persisted CLI session, never a hand-built continuation" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 4: Add Authorized Live Soak And Context Qualification

**Files:**
- Create: `scripts/reliability_live_soak.sh`
- Create: `scripts/reliability_context_matrix.sh`
- Modify: `tests/scripts.rs`

- [ ] **Step 1: Add failing safety and matrix contract tests**

Add to `tests/scripts.rs`:

```rust
#[test]
fn reliability_live_scripts_are_bounded_secret_free_and_cover_required_tiers() {
    let soak = fs::read_to_string("scripts/reliability_live_soak.sh").unwrap();
    assert!(soak.contains("TOTAL_REQUESTS=\"${TOTAL_REQUESTS:-1000}\""));
    assert!(soak.contains("CONCURRENCY=\"${CONCURRENCY:-10}\""));
    assert!(soak.contains("--config \"$CURL_CONFIG\""));
    assert!(!soak.contains("Authorization: Bearer $DOWNSTREAM_KEY"));

    let matrix = fs::read_to_string("scripts/reliability_context_matrix.sh").unwrap();
    for tier in ["32000", "64000", "128000", "configured_max"] {
        assert!(matrix.contains(tier));
    }
    for scenario in ["text", "reasoning", "read_only_tool"] {
        assert!(matrix.contains(scenario));
    }
    assert!(matrix.contains("CONTEXT_RUNS=\"${CONTEXT_RUNS:-3}\""));
    assert!(matrix.contains("recommended_safe_context_tokens"));
}
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test scripts reliability_live_scripts_are_bounded_secret_free_and_cover_required_tiers
```

Expected: both scripts are missing.

- [ ] **Step 3: Create the bounded live soak**

Create `scripts/reliability_live_soak.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
set +x

: "${API_BASE_URL:?API_BASE_URL is required}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"
: "${OUTPUT_DIR:?OUTPUT_DIR is required}"
TOTAL_REQUESTS="${TOTAL_REQUESTS:-1000}"
CONCURRENCY="${CONCURRENCY:-10}"
[[ "$TOTAL_REQUESTS" =~ ^[1-9][0-9]*$ ]]
[[ "$CONCURRENCY" =~ ^[1-9][0-9]*$ ]]

umask 077
mkdir -p "$OUTPUT_DIR/responses" "$OUTPUT_DIR/status"
CURL_CONFIG="$OUTPUT_DIR/curl.conf"
PAYLOAD="$OUTPUT_DIR/request.json"
cat >"$CURL_CONFIG" <<EOF
silent
show-error
header = "Authorization: Bearer $DOWNSTREAM_KEY"
header = "Content-Type: application/json"
connect-timeout = 10
max-time = 900
EOF
chmod 600 "$CURL_CONFIG"
jq -nc --arg model "$MODEL_SLUG" '{
  model: $model,
  input: "Return exactly LIVE_SOAK_OK.",
  max_output_tokens: 32,
  stream: false
}' >"$PAYLOAD"

run_soak_request() {
  local index="$1"
  local code
  code="$(curl --config "$CURL_CONFIG" \
    --output "$OUTPUT_DIR/responses/$index.json" \
    --write-out '%{http_code}' \
    --data-binary "@$PAYLOAD" \
    "${API_BASE_URL%/}/responses")"
  printf '%s\n' "$code" >"$OUTPUT_DIR/status/$index"
}
export -f run_soak_request
export CURL_CONFIG PAYLOAD OUTPUT_DIR API_BASE_URL
seq 1 "$TOTAL_REQUESTS" | xargs -n 1 -P "$CONCURRENCY" bash -c 'run_soak_request "$1"' _

find "$OUTPUT_DIR/status" -type f -print0 \
  | sort -z \
  | xargs -0 cat >"$OUTPUT_DIR/http-statuses.txt"
status_count="$(wc -l <"$OUTPUT_DIR/http-statuses.txt" | tr -d ' ')"
ok_count="$(awk '$1 == 200 {count++} END {print count + 0}' "$OUTPUT_DIR/http-statuses.txt")"
if [[ "$status_count" != "$TOTAL_REQUESTS" || "$ok_count" != "$TOTAL_REQUESTS" ]]; then
  printf 'status=failed requests=%s http_200=%s\n' "$status_count" "$ok_count" >&2
  exit 1
fi
if ! jq -e -s 'all(.[]; .status == "completed")' "$OUTPUT_DIR"/responses/*.json >/dev/null; then
  printf 'status=failed reason=non_terminal_response\n' >&2
  exit 1
fi
rm -f "$CURL_CONFIG"
printf 'status=passed requests=%s concurrency=%s\n' "$TOTAL_REQUESTS" "$CONCURRENCY"
```

- [ ] **Step 4: Create the serial context matrix**

Create `scripts/reliability_context_matrix.sh` with this complete control flow. The repeated `token` fixture is measured again from returned usage; the configured limit is never inferred from byte length.

```bash
#!/usr/bin/env bash
set -euo pipefail
set +x

: "${API_BASE_URL:?API_BASE_URL is required}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"
: "${CONFIGURED_MAX_TOKENS:?CONFIGURED_MAX_TOKENS is required}"
: "${OUTPUT_DIR:?OUTPUT_DIR is required}"
CONTEXT_RUNS="${CONTEXT_RUNS:-3}"
CONTEXT_TIERS="${CONTEXT_TIERS:-32000,64000,128000,configured_max}"
REASONING_EFFORT="${REASONING_EFFORT:-high}"
[[ "$CONFIGURED_MAX_TOKENS" =~ ^[1-9][0-9]*$ ]]
[[ "$CONTEXT_RUNS" =~ ^[1-9][0-9]*$ ]]

umask 077
mkdir -p "$OUTPUT_DIR/cases"
CURL_CONFIG="$OUTPUT_DIR/curl.conf"
cat >"$CURL_CONFIG" <<EOF
silent
show-error
header = "Authorization: Bearer $DOWNSTREAM_KEY"
header = "Content-Type: application/json"
connect-timeout = 10
max-time = 3600
EOF
chmod 600 "$CURL_CONFIG"

resolve_tier() {
  if [[ "$1" == "configured_max" ]]; then
    printf '%s' "$CONFIGURED_MAX_TOKENS"
  else
    printf '%s' "$1"
  fi
}

request_json() {
  local body="$1" output="$2"
  local code
  code="$(curl --config "$CURL_CONFIG" --output "$output" --write-out '%{http_code}' \
    --data-binary "@$body" "${API_BASE_URL%/}/responses")" || return 1
  [[ "$code" == "200" ]] || return 1
  jq -e '.status == "completed"' "$output" >/dev/null
}

run_case() {
  local tier_name="$1" target="$2" scenario="$3" run="$4"
  local stem="$OUTPUT_DIR/cases/${tier_name}-${scenario}-${run}"
  local fixture="$stem.txt" body="$stem-request.json" response="$stem-response.json"
  local fixture_words=$(( target > 1024 ? target - 1024 : target / 2 ))
  jq -nr --argjson words "$fixture_words" '"token " * $words' >"$fixture"

  case "$scenario" in
    text)
      jq -nc --arg model "$MODEL_SLUG" --rawfile fixture "$fixture" '{
        model: $model,
        input: ($fixture + "\nReturn exactly CONTEXT_TEXT_OK."),
        max_output_tokens: 64,
        stream: false
      }' >"$body"
      request_json "$body" "$response"
      ;;
    reasoning)
      jq -nc --arg model "$MODEL_SLUG" --arg effort "$REASONING_EFFORT" \
        --rawfile fixture "$fixture" '{
          model: $model,
          input: ($fixture + "\nReason briefly, then return exactly CONTEXT_REASONING_OK."),
          reasoning: {effort: $effort},
          max_output_tokens: 128,
          stream: false
        }' >"$body"
      request_json "$body" "$response"
      ;;
    read_only_tool)
      jq -nc --arg model "$MODEL_SLUG" '{
        model: $model,
        input: "Call read_context_fixture exactly once.",
        tools: [{
          type: "function",
          name: "read_context_fixture",
          description: "Read the immutable context qualification fixture.",
          parameters: {type: "object", properties: {}, additionalProperties: false}
        }],
        tool_choice: {type: "function", name: "read_context_fixture"},
        stream: false
      }' >"$body"
      request_json "$body" "$stem-tool-call.json"
      response_id="$(jq -er '.id' "$stem-tool-call.json")"
      call_id="$(jq -er '.output[] | select(.type == "function_call") | .call_id' "$stem-tool-call.json")"
      jq -nc --arg model "$MODEL_SLUG" --arg previous "$response_id" \
        --arg call_id "$call_id" --rawfile fixture "$fixture" '{
          model: $model,
          previous_response_id: $previous,
          input: [{type: "function_call_output", call_id: $call_id, output: $fixture}],
          max_output_tokens: 64,
          stream: false
        }' >"$body"
      request_json "$body" "$response"
      ;;
    *) return 2 ;;
  esac
}

RESULTS="$OUTPUT_DIR/results.jsonl"
: >"$RESULTS"
IFS=',' read -r -a tier_names <<<"$CONTEXT_TIERS"
for tier_name in "${tier_names[@]}"; do
  target="$(resolve_tier "$tier_name")"
  for scenario in text reasoning read_only_tool; do
    for run in $(seq 1 "$CONTEXT_RUNS"); do
      if run_case "$tier_name" "$target" "$scenario" "$run"; then passed=true; else passed=false; fi
      jq -nc --arg tier "$tier_name" --argjson target "$target" \
        --arg scenario "$scenario" --argjson run "$run" --argjson passed "$passed" \
        '{tier: $tier, target_tokens: $target, scenario: $scenario, run: $run, passed: $passed}' \
        >>"$RESULTS"
    done
  done
done

jq -s --argjson runs "$CONTEXT_RUNS" '
  group_by(.target_tokens)
  | map({
      target_tokens: .[0].target_tokens,
      passed: (length == ($runs * 3) and all(.[]; .passed))
    })
  | sort_by(.target_tokens)
  | . as $tiers
  | {
      tiers: $tiers,
      recommended_safe_context_tokens: ([$tiers[] | select(.passed) | .target_tokens] | max // 0)
    }
' "$RESULTS" >"$OUTPUT_DIR/summary.json"

jq -e '.recommended_safe_context_tokens >= 32000' "$OUTPUT_DIR/summary.json" >/dev/null || {
  printf 'status=failed reason=minimum_32k_not_qualified\n' >&2
  exit 1
}
jq -e --argjson configured "$CONFIGURED_MAX_TOKENS" '
  any(.tiers[]; .target_tokens == $configured and .passed)
' "$OUTPUT_DIR/summary.json" >/dev/null || {
  printf 'status=failed reason=configured_max_not_qualified\n' >&2
  exit 1
}
rm -f "$CURL_CONFIG"
jq -r '"status=passed recommended_safe_context_tokens=\(.recommended_safe_context_tokens)"' \
  "$OUTPUT_DIR/summary.json"
```

A higher 64k or 128k failure remains recorded and does not erase a lower passing tier. Final release still requires the deployed `CONFIGURED_MAX_TOKENS` tier to pass three text, three reasoning, and three tool-history runs.

- [ ] **Step 5: Run and commit live qualification scripts**

```bash
rtk bash -n scripts/reliability_live_soak.sh
rtk bash -n scripts/reliability_context_matrix.sh
rtk cargo test --test scripts reliability_live_scripts_are_bounded_secret_free_and_cover_required_tiers
rtk git add scripts/reliability_live_soak.sh scripts/reliability_context_matrix.sh tests/scripts.rs
rtk git commit -m "test(reliability): add live load and context qualification" -m "Constraint: Live traffic requires operator-provided credentials" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 5: Orchestrate Evidence And Enforce Runtime Invariants

**Files:**
- Create: `scripts/reliability_qualification.sh`
- Modify: `tests/scripts.rs`

- [ ] **Step 1: Add a failing evidence-manifest contract test**

Add to `tests/scripts.rs`:

```rust
#[test]
fn reliability_qualification_indexes_all_twelve_acceptance_criteria() {
    let script = fs::read_to_string("scripts/reliability_qualification.sh").unwrap();
    for artifact in [
        "image-inspect.json",
        "gateway.log",
        "redis-invariants.json",
        "postgres-usage.tsv",
        "capability-discovery.json",
        "checksums.sha256",
        "manifest.json",
    ] {
        assert!(script.contains(artifact), "missing {artifact}");
    }
    for criterion in 1..=12 {
        assert!(script.contains(&format!("\"ac{criterion}\"")));
    }
    assert!(script.contains("codex_resume_smoke.sh"));
    assert!(script.contains("codex_tui_smoke.sh"));
    assert!(script.contains("codex_delayed_output_smoke.sh"));
    assert!(script.contains("reliability_context_matrix.sh"));
    assert!(script.contains("reliability_live_soak.sh"));
    assert!(script.contains("responses_continuation_503_fails_over_to_compatible_account"));
    assert!(script.contains("legacy_local_admission_route_health_is_repaired_selectively"));
    assert!(script.contains("enabled_deployment_bootstrap_replaces_only_revision_zero"));
    assert!(script.contains("qualification_requires_redis"));
    assert!(script.contains("usage_rows < MIN_USAGE_ROWS"));
}
```

- [ ] **Step 2: Verify RED**

```bash
rtk cargo test --test scripts reliability_qualification_indexes_all_twelve_acceptance_criteria
```

Expected: orchestrator is missing.

- [ ] **Step 3: Create the qualification orchestrator preflight and capability phase**

Create `scripts/reliability_qualification.sh`. Start with:

```bash
#!/usr/bin/env bash
set -euo pipefail
set +x

: "${DEPLOY_DIR:?DEPLOY_DIR is required}"
: "${API_BASE_URL:?API_BASE_URL is required}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"
: "${CONFIGURED_MAX_TOKENS:?CONFIGURED_MAX_TOKENS is required}"
OUTPUT_DIR="${OUTPUT_DIR:-artifacts/reliability-2026-08-08}"
MIN_USAGE_ROWS="${MIN_USAGE_ROWS:-1000}"
DEPLOY_ENV="$DEPLOY_DIR/.env"
DEPLOY_COMPOSE="$DEPLOY_DIR/docker-compose.yml"
[[ -f "$DEPLOY_ENV" && -f "$DEPLOY_COMPOSE" ]]
mkdir -p "$OUTPUT_DIR"
chmod 700 "$OUTPUT_DIR"
umask 077
RUN_START_EPOCH="$(date +%s)"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

for command in curl jq docker sha256sum codex; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'status=missing_command command=%s\n' "$command" >&2
    exit 1
  }
done
if docker compose version >/dev/null 2>&1; then
  COMPOSE=(docker compose)
else
  COMPOSE=(docker-compose)
fi
COMPOSE_BASE=("${COMPOSE[@]}" --env-file "$DEPLOY_ENV" -f "$DEPLOY_COMPOSE" --project-directory "$DEPLOY_DIR")

env_value() {
  sed -n "s/^$1=//p" "$DEPLOY_ENV" | tail -n 1 | tr -d '\r'
}
ADMIN_USERNAME="$(env_value ADMIN_USERNAME)"
ADMIN_USERNAME="${ADMIN_USERNAME:-admin}"
ADMIN_PASSWORD="$(env_value ADMIN_PASSWORD)"
REDIS_KEY_PREFIX="$(env_value REDIS_KEY_PREFIX)"
REDIS_KEY_PREFIX="${REDIS_KEY_PREFIX:-chat2responses}"
REDIS_ENABLED="$(env_value REDIS_ENABLED)"
POSTGRES_PASSWORD="$(env_value POSTGRES_PASSWORD)"
[[ "$MIN_USAGE_ROWS" =~ ^[1-9][0-9]*$ ]]
if [[ "$REDIS_ENABLED" != "true" ]]; then
  printf 'status=failed reason=qualification_requires_redis\n' >&2
  exit 1
fi

curl -fsS "${API_BASE_URL%/v1}/healthz" >"$OUTPUT_DIR/healthz.json"
ADMIN_TOKEN="$(curl -fsS "${API_BASE_URL%/v1}/api/admin/login" \
  -H 'Content-Type: application/json' \
  --data-binary "$(jq -nc --arg username "$ADMIN_USERNAME" --arg password "$ADMIN_PASSWORD" \
    '{username: $username, password: $password}')" | jq -er '.token')"

curl -fsS "${API_BASE_URL%/v1}/api/admin/capabilities/probe-all" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  --data-binary "$(jq -nc --arg model "$MODEL_SLUG" '{models: [$model]}')" \
  >"$OUTPUT_DIR/capability-probe-all.json"
PROBE_STARTED_AT="$(jq -er '.started_at' "$OUTPUT_DIR/capability-probe-all.json")"

probe_complete=0
for _ in $(seq 1 90); do
  curl -fsS "${API_BASE_URL%/v1}/api/admin/capabilities/discovery" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    >"$OUTPUT_DIR/capability-discovery.json"
  if jq -e --arg model "$MODEL_SLUG" --argjson started "$PROBE_STARTED_AT" '
    [.models[] | select(.exposed_model_slug == $model)] as $models
    | ($models | length) == 1
      and ($models[0].routes | length) > 0
      and all($models[0].routes[];
        .outcome != "pending"
        and ((.last_attempt_at // 0) >= $started or .outcome == "deferred"))
  ' "$OUTPUT_DIR/capability-discovery.json" >/dev/null; then
    probe_complete=1
    break
  fi
  sleep 1
done
if [[ "$probe_complete" -ne 1 ]]; then
  printf 'status=failed reason=capability_probe_batch_timed_out\n' >&2
  exit 1
fi
jq -e --arg model "$MODEL_SLUG" '
  ([.. | objects | has("key_fingerprint")] | all(. == false))
  and any(.models[];
    .exposed_model_slug == $model
    and (.verified_reasoning_levels | length) > 0)
' "$OUTPUT_DIR/capability-discovery.json" >/dev/null
jq -e '[.. | objects | has("key_fingerprint")] | all(. == false)' \
  "$OUTPUT_DIR/capability-probe-all.json" >/dev/null
```

- [ ] **Step 4: Add deterministic, Codex, soak, delayed-output, and context phases**

Continue the same file with:

```bash
cargo test --test load eight_four_slot_accounts_complete_one_thousand_requests_at_concurrency_ten \
  -- --nocapture >"$OUTPUT_DIR/deterministic-eight-account.log" 2>&1
cargo test --test gateway explicit_concurrency_5xx_uses_account_recovery_and_healthy_routes \
  >"$OUTPUT_DIR/deterministic-concurrency-502.log" 2>&1
cargo test --lib all_accounts_locally_full_then_release_without_route_cooldown \
  >"$OUTPUT_DIR/deterministic-all-full.log" 2>&1
cargo test --test gateway generic_503_remains_bounded_transient_failure \
  >"$OUTPUT_DIR/deterministic-generic-503.log" 2>&1
cargo test --test gateway responses_continuation_503_fails_over_to_compatible_account \
  >"$OUTPUT_DIR/deterministic-continuation-failover.log" 2>&1
cargo test --test redis_runtime legacy_local_admission_route_health_is_repaired_selectively \
  >"$OUTPUT_DIR/deterministic-legacy-repair.log" 2>&1
cargo test --test capability_state enabled_deployment_bootstrap_replaces_only_revision_zero \
  >"$OUTPUT_DIR/deterministic-capability-bootstrap.log" 2>&1

CLIENTS=codex ARTIFACT_DIR="$OUTPUT_DIR/codex" \
  BASE_URL="${API_BASE_URL%/v1}" API_BASE_URL="$API_BASE_URL" \
  DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" \
  "$SCRIPT_DIR/installed_client_smoke.sh" >"$OUTPUT_DIR/installed-codex.log" 2>&1

API_BASE_URL="$API_BASE_URL" DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" \
  OUTPUT_DIR="$OUTPUT_DIR/codex-resume" \
  "$SCRIPT_DIR/codex_resume_smoke.sh" >"$OUTPUT_DIR/codex-resume.log" 2>&1

API_BASE_URL="$API_BASE_URL" DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" \
  OUTPUT_DIR="$OUTPUT_DIR/codex-tui" \
  "$SCRIPT_DIR/codex_tui_smoke.sh"

API_BASE_URL="$API_BASE_URL" DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" \
  OUTPUT_DIR="$OUTPUT_DIR/live-soak" \
  "$SCRIPT_DIR/reliability_live_soak.sh" >"$OUTPUT_DIR/live-soak.log" 2>&1

API_BASE_URL="$API_BASE_URL" DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" \
  CONFIGURED_MAX_TOKENS="$CONFIGURED_MAX_TOKENS" OUTPUT_DIR="$OUTPUT_DIR/context" \
  "$SCRIPT_DIR/reliability_context_matrix.sh" >"$OUTPUT_DIR/context.log" 2>&1

API_BASE_URL="$API_BASE_URL" DOWNSTREAM_KEY="$DOWNSTREAM_KEY" MODEL_SLUG="$MODEL_SLUG" \
  ADMIN_LOG_URL="${API_BASE_URL%/v1}/api/admin/logs" ADMIN_TOKEN="$ADMIN_TOKEN" \
  DELAYED_OUTPUT_SECONDS="${DELAYED_OUTPUT_SECONDS:-3600}" \
  "$SCRIPT_DIR/codex_delayed_output_smoke.sh" >"$OUTPUT_DIR/delayed-output.log" 2>&1
```

Every deterministic test name above is created in the owning child plan. Do not weaken this phase to a hand-made `previous_response_id` request.

- [ ] **Step 5: Add sanitized PostgreSQL, Redis, image, and log evidence**

Continue with:

```bash
"${COMPOSE_BASE[@]}" exec -T -e PGPASSWORD="$POSTGRES_PASSWORD" postgres \
  psql -U chat_responses_codex -d chat_responses_codex -At -F $'\t' -c \
  "SELECT status_code, wire_status_code, COALESCE(error_category, ''), COUNT(*)
   FROM usage_logs
   WHERE created_at >= $RUN_START_EPOCH AND model = '$(sed "s/'/''/g" <<<"$MODEL_SLUG")'
   GROUP BY status_code, wire_status_code, error_category
   ORDER BY status_code, wire_status_code, error_category" \
  >"$OUTPUT_DIR/postgres-usage.tsv"
if awk -F '\t' '$1 == 429 || $1 == 502 || $1 == 503 {bad += $4} END {exit bad != 0}' \
  "$OUTPUT_DIR/postgres-usage.tsv"; then :; else
  echo "Error: logical 429/502/503 found in qualification window" >&2
  exit 1
fi
usage_rows="$(awk -F '\t' '{rows += $4} END {print rows + 0}' "$OUTPUT_DIR/postgres-usage.tsv")"
if (( usage_rows < MIN_USAGE_ROWS )); then
  printf 'Error: expected at least %s usage rows, found %s\n' \
    "$MIN_USAGE_ROWS" "$usage_rows" >&2
  exit 1
fi

ROUTE_INDEX="${REDIS_KEY_PREFIX}:v1:route-health:{route-health}:index:routes"
"${COMPOSE_BASE[@]}" exec -T redis redis-cli --json EVAL '
  local members = redis.call("ZRANGE", KEYS[1], 0, -1)
  local poisoned = 0
  local statusless_concurrency = 0
  local now = redis.call("TIME")
  local now_ms = (now[1] * 1000) + math.floor(now[2] / 1000)
  for _, key in ipairs(members) do
    if redis.call("HGET", key, "failure_class") == "concurrency_saturated"
        and not redis.call("HGET", key, "failure_status") then
      statusless_concurrency = statusless_concurrency + 1
      local until_ms = tonumber(redis.call("HGET", key, "cooldown_until_ms") or "0")
      if until_ms - now_ms > tonumber(ARGV[1]) then poisoned = poisoned + 1 end
    end
  end
  return {#members, statusless_concurrency, poisoned}
' 1 "$ROUTE_INDEX" 62000 >"$OUTPUT_DIR/redis-invariants.json"
jq -e '.[2] == 0' "$OUTPUT_DIR/redis-invariants.json" >/dev/null

gateway_id="$("${COMPOSE_BASE[@]}" images -q gateway)"
docker inspect "$gateway_id" \
  | jq '.[0] | {id: .Id, image: .Config.Image, image_digest: .Image, health: .State.Health.Status}' \
  >"$OUTPUT_DIR/image-inspect.json"
jq -e '.health == "healthy"' "$OUTPUT_DIR/image-inspect.json" >/dev/null
"${COMPOSE_BASE[@]}" logs --since "@$RUN_START_EPOCH" gateway \
  | sed -E 's/(Bearer|api[_-]?key=)[^[:space:]]+/\1[REDACTED]/Ig' \
  >"$OUTPUT_DIR/gateway.log"
if grep -RqsF '"key_fingerprint"' "$OUTPUT_DIR"; then
  printf 'Error: evidence contains a key_fingerprint field\n' >&2
  exit 1
fi
if grep -RqsF "$DOWNSTREAM_KEY" "$OUTPUT_DIR"; then
  printf 'Error: evidence contains the downstream credential\n' >&2
  exit 1
fi
```

The Redis artifact contains only counts. It does not expose route keys, fingerprints, leases, waiter identities, quotas, or credentials.

- [ ] **Step 6: Write a checksum manifest mapping all acceptance criteria**

Finish the script with:

```bash
find "$OUTPUT_DIR" -type f ! -name manifest.json ! -name checksums.sha256 -printf '%P\0' \
  | sort -z \
  | while IFS= read -r -d '' file; do
      (cd "$OUTPUT_DIR" && sha256sum "$file")
    done >"$OUTPUT_DIR/checksums.sha256"

jq -n \
  --arg created_at "$(date --iso-8601=seconds)" \
  --arg model "$MODEL_SLUG" \
  --arg image "$(jq -r '.image' "$OUTPUT_DIR/image-inspect.json")" \
  --arg digest "$(jq -r '.image_digest' "$OUTPUT_DIR/image-inspect.json")" \
  '{
    schema_version: 1,
    created_at: $created_at,
    model: $model,
    image: {reference: $image, digest: $digest},
    acceptance: [
      {id: "ac1", passed: true, evidence: ["deterministic-eight-account.log"]},
      {id: "ac2", passed: true, evidence: ["deterministic-concurrency-502.log"]},
      {id: "ac3", passed: true, evidence: ["deterministic-all-full.log"]},
      {id: "ac4", passed: true, evidence: ["deterministic-generic-503.log"]},
      {id: "ac5", passed: true, evidence: ["deterministic-continuation-failover.log", "codex-resume/resume-initial.jsonl", "codex-resume/resume-final.jsonl", "codex-tui/codex-tui-initial.typescript", "codex-tui/codex-tui-resume.typescript", "codex-tui/codex-tui-result.txt"]},
      {id: "ac6", passed: true, evidence: ["capability-discovery.json"]},
      {id: "ac7", passed: true, evidence: ["codex/", "codex-tui/", "context/summary.json", "postgres-usage.tsv"]},
      {id: "ac8", passed: true, evidence: ["delayed-output.log", "postgres-usage.tsv"]},
      {id: "ac9", passed: true, evidence: ["deterministic-legacy-repair.log", "gateway.log", "redis-invariants.json"]},
      {id: "ac10", passed: true, evidence: ["deterministic-capability-bootstrap.log", "capability-probe-all.json", "capability-discovery.json"]},
      {id: "ac11", passed: true, evidence: ["redis-invariants.json"]},
      {id: "ac12", passed: true, evidence: ["image-inspect.json", "gateway.log", "redis-invariants.json", "postgres-usage.tsv", "checksums.sha256"]}
    ]
  }' >"$OUTPUT_DIR/manifest.json"

jq -e '
  (.acceptance | length) == 12
  and all(.acceptance[]; .passed == true)
' "$OUTPUT_DIR/manifest.json" >/dev/null
printf 'status=passed manifest=%s/manifest.json\n' "$OUTPUT_DIR"
```

- [ ] **Step 7: Run and commit evidence orchestration**

```bash
rtk bash -n scripts/reliability_qualification.sh
rtk cargo test --test scripts reliability_qualification_indexes_all_twelve_acceptance_criteria
rtk git add scripts/reliability_qualification.sh tests/scripts.rs
rtk git commit -m "test(reliability): index complete release evidence" -m "Constraint: Evidence excludes credentials and route fingerprints" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 6: Document, Build, Deploy, And Qualify The Candidate

**Files:**
- Modify: `.env.example`
- Modify: `DEPLOYMENT.md`

- [ ] **Step 1: Document deploy health controls and context qualification input**

Add to `.env.example`:

```dotenv
# Repository deploy script health gate. Override only when the published gateway port differs.
GATEWAY_HEALTHCHECK_URL=http://127.0.0.1:3001/healthz
DEPLOY_HEALTH_MAX_ATTEMPTS=60
DEPLOY_HEALTH_INTERVAL_SECONDS=2
```

In `DEPLOYMENT.md`, add a release section containing the exact build, deploy, qualification, evidence, and rollback commands from Steps 3-6 below. State explicitly that `.env`, `postgres-data`, `redis-data`, `REDIS_KEY_PREFIX`, and a nonzero capability revision are preserved; `docker compose down`, `docker compose down -v`, and Redis flushes are prohibited release operations.

- [ ] **Step 2: Run the full pre-build gate**

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --all-targets --all-features
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run type-check
rtk npm --prefix frontend run build
rtk bash -n scripts/*.sh
rtk docker compose --env-file .env.example config --quiet
```

Expected: every command exits zero; Redis-dependent tests are executed in their existing serialized environment and are not silently skipped.

- [ ] **Step 3: Build the immutable candidate only with the repository script**

```bash
rtk scripts/build-package-image.sh \
  --image chat-responses-codex \
  --tag 2026-08-08-reliability.1 \
  --output artifacts/chat-responses-codex-2026-08-08-reliability.1.tar
rtk sha256sum artifacts/chat-responses-codex-2026-08-08-reliability.1.tar
rtk docker image inspect chat-responses-codex:2026-08-08-reliability.1
```

Expected: the versioned image and tar exist, and the image inspect reference is `chat-responses-codex:2026-08-08-reliability.1`.

- [ ] **Step 4: Deploy only with the repository script**

```bash
rtk scripts/deploy.sh \
  --deploy-dir /home/kavin/docker/chat-responses-codex \
  --image chat-responses-codex \
  --tag 2026-08-08-reliability.1
```

Expected: the script reports the selected image, healthy `/healthz`, and one legacy repair summary. It does not recreate PostgreSQL/Redis volumes or overwrite `.env`.

- [ ] **Step 5: Run the authorized qualification**

Set `DOWNSTREAM_KEY`, `MODEL_SLUG`, and the already deployed conservative `CONFIGURED_MAX_TOKENS` in the operator shell. Run this from a real terminal because it opens two Codex TUI phases and requires the operator to use `/resume`:

```bash
rtk scripts/reliability_qualification.sh
```

with:

```text
DEPLOY_DIR=/home/kavin/docker/chat-responses-codex
API_BASE_URL=http://127.0.0.1:3001/v1
OUTPUT_DIR=artifacts/reliability-2026-08-08
```

Expected: `artifacts/reliability-2026-08-08/manifest.json` lists `ac1` through `ac12` as passed, the configured-max context tier passes three consecutive runs for every scenario, PostgreSQL has zero logical 429/502/503 in the qualification window, and Redis reports zero poisoned routes.

- [ ] **Step 6: Verify evidence and commit release documentation**

```bash
rtk jq -e '(.acceptance | length) == 12 and all(.acceptance[]; .passed)' artifacts/reliability-2026-08-08/manifest.json
rtk sha256sum -c artifacts/reliability-2026-08-08/checksums.sha256
rtk git diff --check
rtk git add .env.example DEPLOYMENT.md
rtk git commit -m "docs(release): define reliable intranet Codex qualification" -m "Constraint: Build and deploy only through repository scripts" -m "Confidence: high" -m "Scope-risk: narrow"
```

- [ ] **Step 7: Keep rollback scripted and state-preserving**

Rollback uses the exact prior immutable tag recorded in the previous manifest. Load it into a nonempty shell variable, then use the same script:

```bash
rtk scripts/deploy.sh \
  --deploy-dir /home/kavin/docker/chat-responses-codex \
  --image chat-responses-codex \
  --tag "${PRIOR_IMAGE_TAG:?set PRIOR_IMAGE_TAG from the previous manifest}" \
  --skip-build
```

Never roll back by flushing Redis, deleting volumes, or copying an older `.env`. Because the prior image can reintroduce the local-cooldown defect, reduce traffic and monitor `legacy_local_admission_poisoned_routes` until the repaired image is restored.
