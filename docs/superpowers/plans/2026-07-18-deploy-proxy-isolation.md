# Deploy Proxy Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent `scripts/deploy.sh` and its Docker children from inheriting host proxy variables while preserving the existing build and deployment flow.

**Architecture:** Clear the six standard uppercase and lowercase HTTP, HTTPS, and ALL proxy variables once at script entry. Exercise the public deploy script through a fake Docker executable so the regression test observes both child-process environment and the unchanged `docker build` argument vector.

**Tech Stack:** Bash, Rust integration tests, Docker, Cargo.

---

### Task 1: Capture the failing deploy proxy contract

**Files:**
- Modify: `tests/scripts.rs:315`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Add a behavioral regression test after `deployment_scripts_disable_xtrace_before_reading_secrets`**

Add this complete test:

```rust
#[test]
fn deploy_clears_proxy_environment_without_changing_build_command() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let deploy_dir = temp.path().join("deploy");
    let proxy_capture = temp.path().join("proxy-capture.txt");
    let args_capture = temp.path().join("args-capture.txt");
    fs::create_dir(&fake_bin).unwrap();

    write_executable(
        &fake_bin.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "compose" && "${2:-}" == "version" ]]; then
  exit 0
fi
if [[ "${1:-}" == "build" ]]; then
  for name in HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; do
    if [[ -v "$name" ]]; then
      printf '%s=set\n' "$name"
    else
      printf '%s=unset\n' "$name"
    fi
  done >"$DOCKER_PROXY_CAPTURE"
  printf '%s\n' "$@" >"$DOCKER_ARGS_CAPTURE"
  exit 0
fi
exit 0
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
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("DOCKER_PROXY_CAPTURE", &proxy_capture)
        .env("DOCKER_ARGS_CAPTURE", &args_capture)
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
    let repo_root = std::env::current_dir()
        .unwrap()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let captured_args = fs::read_to_string(&args_capture).unwrap();
    assert_eq!(
        captured_args.lines().collect::<Vec<_>>(),
        vec![
            "build",
            "-t",
            "proxy-test-image:proxy-test-tag",
            repo_root.as_str(),
        ]
    );
    assert_eq!(
        fs::read_to_string(&proxy_capture).unwrap(),
        concat!(
            "HTTP_PROXY=unset\n",
            "HTTPS_PROXY=unset\n",
            "ALL_PROXY=unset\n",
            "http_proxy=unset\n",
            "https_proxy=unset\n",
            "all_proxy=unset\n",
        )
    );
}
```

- [ ] **Step 2: Run the focused test and verify the proxy assertion fails**

Run:

```bash
rtk cargo test --test scripts deploy_clears_proxy_environment_without_changing_build_command -- --exact
```

Expected: FAIL at the proxy capture assertion. The unchanged build-argument assertion passes first, and the actual proxy capture reports all six variables as `set`.

### Task 2: Clear proxies at deploy entry

**Files:**
- Modify: `scripts/deploy.sh:2`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Add the minimal environment isolation immediately after strict mode**

Change the script header to exactly:

```bash
#!/usr/bin/env bash
set -euo pipefail

unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
```

Do not wrap or edit the existing `docker build` command and do not add build args or a new option.

- [ ] **Step 2: Check Bash syntax**

Run:

```bash
rtk bash -n scripts/deploy.sh
```

Expected: exit code 0 with no output.

- [ ] **Step 3: Re-run the focused test and verify it passes**

Run:

```bash
rtk cargo test --test scripts deploy_clears_proxy_environment_without_changing_build_command -- --exact
```

Expected: PASS. The fake Docker child sees all six proxy variables unset and receives `build -t proxy-test-image:proxy-test-tag <repo-root>`.

- [ ] **Step 4: Run the complete script test binary**

Run:

```bash
rtk cargo test --test scripts
```

Expected: all `tests/scripts.rs` tests pass.

- [ ] **Step 5: Commit the test and implementation together**

Run:

```bash
rtk git add scripts/deploy.sh tests/scripts.rs
rtk git commit -m "fix(deploy): disable inherited proxy environment" -m "Clear standard uppercase and lowercase proxy variables before Docker detection and build while preserving the existing deploy command flow." -m "Constraint: Keep the existing Docker build and compose commands unchanged" -m "Confidence: high" -m "Scope-risk: narrow"
```

### Task 3: Verify the complete build path

**Files:**
- Verify: `scripts/deploy.sh`
- Verify: `Dockerfile`
- Verify: `chat-responses-codex:latest`

- [ ] **Step 1: Run the full Rust test suite**

Run:

```bash
rtk cargo test
```

Expected: exit code 0 with no failed suites.

- [ ] **Step 2: Remove any stale temporary acceptance directory**

Run:

```bash
rtk rm -rf /tmp/chat2responses-deploy-proxy-verification
```

Expected: the fixed, task-specific temporary path is absent.

- [ ] **Step 3: Build the production image through the real deploy script without starting services**

Run:

```bash
rtk bash scripts/deploy.sh --skip-start --deploy-dir /tmp/chat2responses-deploy-proxy-verification --image chat-responses-codex --tag latest
```

Expected: exit code 0. Docker completes the existing multi-stage frontend and Rust release build and tags `chat-responses-codex:latest`; Compose is not started.

- [ ] **Step 4: Inspect the resulting image**

Run:

```bash
rtk docker image inspect chat-responses-codex:latest --format '{{.Id}} {{json .RepoTags}}'
```

Expected: exit code 0 and output containing `chat-responses-codex:latest`.

- [ ] **Step 5: Remove only the temporary deployment files**

Run:

```bash
rtk rm -rf /tmp/chat2responses-deploy-proxy-verification
```

Expected: temporary copied Compose and env files are removed. Keep `chat-responses-codex:latest`.

- [ ] **Step 6: Verify repository cleanliness and final commit**

Run:

```bash
rtk git status --short --branch
rtk git show --stat --oneline --summary HEAD
```

Expected: `main` has no unstaged or untracked files, and HEAD is the deploy proxy-isolation implementation commit.
