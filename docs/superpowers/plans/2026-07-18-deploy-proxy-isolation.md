# Deploy Local Build Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile the frontend and Rust service on the host, then let deploy package only the local release binary into a non-root runtime image without inheriting host proxy variables.

**Architecture:** `scripts/deploy.sh` delegates its build branch to the existing `scripts/build-package-image.sh` with tar export disabled. A temporary-repository integration test runs the real scripts behind fake npm, Cargo, and Docker commands to verify local build order, proxy isolation, image arguments, and non-root runtime Dockerfile behavior.

**Tech Stack:** Bash, Rust integration tests, npm, Cargo, Docker.

---

### Task 1: Capture the local-build deploy contract

**Files:**
- Modify: `tests/scripts.rs:334`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Replace the existing deploy proxy test with a temporary-repository workflow test**

Replace `deploy_clears_proxy_environment_without_changing_build_command` with this complete test:

```rust
#[test]
fn deploy_builds_local_artifacts_before_packaging_runtime_image() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    let scripts_dir = repo_root.join("scripts");
    let frontend_dir = repo_root.join("frontend");
    let fake_bin = temp.path().join("bin");
    let deploy_dir = temp.path().join("deploy");
    let tool_trace = temp.path().join("tool-trace.txt");
    let runtime_dockerfile = temp.path().join("runtime.Dockerfile");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::create_dir_all(&frontend_dir).unwrap();
    fs::create_dir(&fake_bin).unwrap();

    for script in ["deploy.sh", "build-package-image.sh"] {
        write_executable(
            &scripts_dir.join(script),
            &fs::read_to_string(format!("scripts/{script}")).unwrap(),
        );
    }
    fs::copy("docker-compose.yml", repo_root.join("docker-compose.yml")).unwrap();
    fs::copy(".env.example", repo_root.join(".env.example")).unwrap();
    fs::copy(
        "frontend/package-lock.json",
        frontend_dir.join("package-lock.json"),
    )
    .unwrap();

    write_executable(
        &fake_bin.join("npm"),
        r#"#!/usr/bin/env bash
set -euo pipefail
for name in HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; do
  if [[ -v "$name" ]]; then
    printf 'npm inherited %s\n' "$name" >&2
    exit 90
  fi
done
printf 'npm' >>"$TOOL_TRACE"
printf '\t%s' "$@" >>"$TOOL_TRACE"
printf '\n' >>"$TOOL_TRACE"
"#,
    );
    write_executable(
        &fake_bin.join("cargo"),
        r#"#!/usr/bin/env bash
set -euo pipefail
for name in HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; do
  if [[ -v "$name" ]]; then
    printf 'cargo inherited %s\n' "$name" >&2
    exit 90
  fi
done
printf 'cargo' >>"$TOOL_TRACE"
printf '\t%s' "$@" >>"$TOOL_TRACE"
printf '\n' >>"$TOOL_TRACE"
mkdir -p target/release
printf '#!/usr/bin/env bash\nexit 0\n' >target/release/chat-responses-codex
chmod +x target/release/chat-responses-codex
"#,
    );
    write_executable(
        &fake_bin.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
for name in HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; do
  if [[ -v "$name" ]]; then
    printf 'docker inherited %s\n' "$name" >&2
    exit 90
  fi
done
if [[ "${1:-}" == "compose" && "${2:-}" == "version" ]]; then
  exit 0
fi
if [[ "${1:-}" == "build" ]]; then
  context="${@: -1}"
  if [[ ! -x "$context/chat-responses-codex" ]]; then
    printf 'runtime context missing local binary: %s\n' "$context" >&2
    exit 91
  fi
  cp "$context/Dockerfile" "$RUNTIME_DOCKERFILE_CAPTURE"
  printf 'docker' >>"$TOOL_TRACE"
  printf '\t%s' "$@" >>"$TOOL_TRACE"
  printf '\n' >>"$TOOL_TRACE"
  exit 0
fi
printf 'unexpected docker invocation:' >&2
printf ' %s' "$@" >&2
printf '\n' >&2
exit 92
"#,
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/deploy.sh")
        .arg("--deploy-dir")
        .arg(&deploy_dir)
        .arg("--image")
        .arg("proxy-test-image")
        .arg("--tag")
        .arg("proxy-test-tag")
        .arg("--skip-start")
        .current_dir(&repo_root)
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("TOOL_TRACE", &tool_trace)
        .env("RUNTIME_DOCKERFILE_CAPTURE", &runtime_dockerfile)
        .env("HTTP_PROXY", "http://proxy.invalid:8080")
        .env("HTTPS_PROXY", "http://proxy.invalid:8080")
        .env("ALL_PROXY", "socks5://proxy.invalid:1080")
        .env("http_proxy", "http://proxy.invalid:8080")
        .env("https_proxy", "http://proxy.invalid:8080")
        .env("all_proxy", "socks5://proxy.invalid:1080")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "deploy fixture failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let trace = fs::read_to_string(&tool_trace).unwrap();
    let npm_ci = trace
        .find("npm\t--prefix\tfrontend\tci\n")
        .expect("local npm ci invocation");
    let npm_build = trace
        .find("npm\t--prefix\tfrontend\trun\tbuild\n")
        .expect("local frontend build invocation");
    let cargo_build = trace
        .find("cargo\tbuild\t--release\n")
        .expect("local Cargo release build invocation");
    let docker_build = trace
        .find("docker\tbuild\t-t\tproxy-test-image:proxy-test-tag\t")
        .expect("runtime image build invocation");
    assert!(npm_ci < npm_build && npm_build < cargo_build && cargo_build < docker_build);

    let runtime = fs::read_to_string(&runtime_dockerfile).unwrap();
    assert!(runtime.starts_with("FROM debian:bookworm-slim\n"));
    assert!(!runtime.contains("FROM node:"));
    assert!(!runtime.contains("FROM rust:"));
    assert!(runtime.contains("useradd --system --uid 10001"));
    assert!(runtime.contains("HEALTHCHECK --interval=30s"));
    assert!(runtime.contains("USER app"));
}
```

- [ ] **Step 2: Run the focused test and verify it fails on the old direct-Docker path**

Run:

```bash
rtk cargo test --test scripts deploy_builds_local_artifacts_before_packaging_runtime_image -- --exact
```

Expected: FAIL because the old deploy calls Docker with the repository as context before npm and Cargo create a local runtime artifact. The fake Docker reports `runtime context missing local binary`.

### Task 2: Delegate deploy builds to the local artifact helper

**Files:**
- Modify: `scripts/deploy.sh:132`
- Modify: `scripts/build-package-image.sh:165`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Replace only the direct Docker build call in deploy**

Change the build branch to:

```bash
if [[ "$SKIP_BUILD" -eq 0 ]]; then
  log "Building docker image ${IMAGE_NAME}:${IMAGE_TAG}"
  "$SCRIPT_DIR/build-package-image.sh" \
    --image "$IMAGE_NAME" \
    --tag "$IMAGE_TAG" \
    --skip-export
else
  log "Skip docker image build"
fi
```

Do not change config copying, `--skip-build`, `--skip-start`, or Compose commands.

- [ ] **Step 2: Make the helper runtime Dockerfile match the production non-root stage**

Replace the generated runtime Dockerfile heredoc with this complete content:

```dockerfile
FROM debian:bookworm-slim

WORKDIR /app
COPY chat-responses-codex /usr/local/bin/chat-responses-codex

RUN groupadd --system app \
    && useradd --system --uid 10001 --gid app --create-home --home-dir /home/app --shell /usr/sbin/nologin app \
    && mkdir -p /data /logs \
    && chown -R app:app /data /logs /home/app

ENV BIND_ADDR=0.0.0.0:3001
ENV STATE_PATH=/data/state.json
ENV LOG_PATH=/logs/chat-responses-codex.log
ENV APP_NAME=chat-responses-codex

VOLUME ["/data", "/logs"]
EXPOSE 3001

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/chat-responses-codex", "--healthcheck"]

USER app

ENTRYPOINT ["/usr/local/bin/chat-responses-codex"]
```

- [ ] **Step 3: Check both Bash scripts**

Run:

```bash
rtk bash -n scripts/deploy.sh
rtk bash -n scripts/build-package-image.sh
```

Expected: both commands exit 0 with no output.

- [ ] **Step 4: Re-run the focused workflow test**

Run:

```bash
rtk cargo test --test scripts deploy_builds_local_artifacts_before_packaging_runtime_image -- --exact
```

Expected: PASS. The trace proves npm ci, frontend build, Cargo release build, and runtime Docker build occur in order; all fake child tools reject inherited proxies; the captured Dockerfile uses `USER app`.

- [ ] **Step 5: Run the complete script test binary and formatting checks**

Run:

```bash
rtk cargo test --test scripts
rtk rustfmt --edition 2021 --check tests/scripts.rs
rtk git diff --check
```

Expected: all script tests pass, rustfmt reports no diff, and Git finds no whitespace errors.

- [ ] **Step 6: Commit the revised workflow**

Run:

```bash
rtk git add scripts/deploy.sh scripts/build-package-image.sh tests/scripts.rs
rtk git commit -m "fix(deploy): build image from local artifacts" -m "Delegate deploy builds to the existing host npm/Cargo pipeline, then package only the release binary in a non-root runtime image." -m "Constraint: Preserve proxy isolation and existing Compose behavior" -m "Rejected: Docker multi-stage compilation | daemon proxy failure and unnecessary toolchain rebuilds" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 3: Verify the real local build and image

**Files:**
- Verify: `scripts/deploy.sh`
- Verify: `scripts/build-package-image.sh`
- Verify: `chat-responses-codex:latest`

- [ ] **Step 1: Run the full Rust suite**

Run:

```bash
rtk cargo test
```

Expected: exit code 0 with no failed suites.

- [ ] **Step 2: Remove the fixed temporary deploy directory**

Run:

```bash
rtk rm -rf /tmp/chat2responses-deploy-local-build-verification
```

Expected: the task-specific temporary path is absent.

- [ ] **Step 3: Run the real host build through deploy without starting Compose**

Run:

```bash
rtk bash scripts/deploy.sh --skip-start --deploy-dir /tmp/chat2responses-deploy-local-build-verification --image chat-responses-codex --tag latest
```

Expected: exit code 0. Logs show local frontend dependency installation/build, local Cargo release build, `Building docker image (copy local binary only)`, and `Skipping image export`; no Node or Rust builder stage runs inside Docker.

- [ ] **Step 4: Inspect the resulting image contract**

Run:

```bash
rtk docker image inspect chat-responses-codex:latest --format '{{.Id}} user={{.Config.User}} tags={{json .RepoTags}} health={{json .Config.Healthcheck.Test}}'
```

Expected: output contains `user=app`, `chat-responses-codex:latest`, and the binary `--healthcheck` command.

- [ ] **Step 5: Confirm current containers were not restarted**

Run:

```bash
rtk docker ps --format '{{.ID}} {{.Names}} {{.Status}}'
```

Expected: existing gateway, PostgreSQL, and Redis containers remain running; deploy used `--skip-start` and did not recreate them.

- [ ] **Step 6: Remove temporary deploy files and verify repository state**

Run:

```bash
rtk rm -rf /tmp/chat2responses-deploy-local-build-verification
rtk git status --short --branch
rtk git show --stat --oneline --summary HEAD
```

Expected: only the temporary copied Compose/env files are removed, `chat-responses-codex:latest` remains, the repository is clean, and HEAD is the local-build workflow commit.
