use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static MATRIX_SCRIPT_LOCK: Mutex<()> = Mutex::new(());

fn write_executable(path: &std::path::Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn write_hermes_mcp_driver(path: &std::path::Path) {
    write_executable(
        path,
        r#"#!/usr/bin/env node
import fs from 'node:fs'
import readline from 'node:readline'
import { spawn } from 'node:child_process'

const config = fs.readFileSync(`${process.env.HERMES_HOME}/config.yaml`, 'utf8')
const command = config.match(/^\s+command: "([^"]+)"/m)?.[1]
const server = config.match(/^\s+args: \["([^"]+)"\]/m)?.[1]
const env = { ...process.env }
for (const match of config.matchAll(/^\s{6}([A-Z_]+): "([^"]*)"$/gm)) env[match[1]] = match[2]
if (!command || !server) process.exit(2)

const child = spawn(command, [server], { env, stdio: ['pipe', 'pipe', 'inherit'] })
const lines = readline.createInterface({ input: child.stdout, crlfDelay: Infinity })
const pending = new Map()
let nextId = 1
lines.on('line', line => {
  try {
    const message = JSON.parse(line)
    if (message.id != null && pending.has(message.id)) {
      pending.get(message.id)(message)
      pending.delete(message.id)
    }
  } catch {}
})
const request = (method, params) => new Promise(resolve => {
  const id = nextId++
  pending.set(id, resolve)
  child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
})
const deadline = setTimeout(() => { child.kill(); process.exit(4) }, 5000)
await request('initialize', {
  protocolVersion: '2025-06-18',
  capabilities: {},
  clientInfo: { name: 'fake-hermes', version: '1.0.0' }
})
child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized', params: {} })}\n`)
await request('tools/list', {})
const result = await request('tools/call', { name: 'lookup', arguments: {} })
const text = result?.result?.content?.find(item => item.type === 'text')?.text
clearTimeout(deadline)
if (!text) process.exit(3)
process.stdout.write(`${text}\n`)
if (process.env.HERMES_DRIVER_HANG === '1') {
  fs.writeFileSync(process.env.HERMES_DRIVER_PID_FILE, `${process.pid}\n${child.pid}\n`)
  process.on('SIGTERM', () => {})
  setInterval(() => {}, 1000)
} else {
  child.kill()
}
"#,
    );
}

fn write_hermes_python_launcher(fake_bin: &std::path::Path) {
    let target = fake_bin.join("python3.12");
    write_executable(
        &target,
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "$(basename "$0")" != "python" ]]; then
  printf 'venv interpreter symlink was resolved away\n' >&2
  exit 97
fi
if [[ -n "${HERMES_PYTHON_PREFLIGHT_MARKER:-}" ]]; then
  printf checked >"$HERMES_PYTHON_PREFLIGHT_MARKER"
fi
exec python3 -S "$@"
"#,
    );
    std::os::unix::fs::symlink(&target, fake_bin.join("python")).unwrap();
}

fn write_fake_mcp_package(root: &std::path::Path) -> std::path::PathBuf {
    let pythonpath = root.join("hermes-pythonpath");
    let package = pythonpath.join("mcp");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("__init__.py"), "# isolated smoke dependency\n").unwrap();
    pythonpath
}

fn write_prerequisite_smoke_clients(fake_bin: &std::path::Path) {
    let fake_client = r#"#!/usr/bin/env bash
set -euo pipefail
client="$(basename "$0")"
if [[ "${1:-}" == "--version" ]]; then
  case "$client" in
    codex) printf 'codex-cli 0.146.0\n' ;;
    opencode) printf '1.17.18\n' ;;
    claude) printf '2.1.195 (Claude Code)\n' ;;
    hermes) printf 'Hermes Agent v0.14.0\n' ;;
  esac
  exit 0
fi
printf '%s\n' "$client" >>"$MODEL_TASK_MARKER"
args=" $* "
if [[ "$client" == "codex" && "$args" == *" login "* ]]; then
  exit 0
elif [[ "$client" == "codex" && "$args" == *"Delegate exactly one"* ]]; then
  marker="$(<probe.txt)"
  printf '{"type":"turn.started"}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
  jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
elif [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
elif [[ "$client" == "hermes" ]]; then
  node "$HERMES_MCP_DRIVER"
else
  cat probe.txt
fi
if [[ "$client" == "codex" ]]; then
  printf '{"type":"turn.completed"}\n'
fi
"#;
    for client in ["codex", "opencode", "claude", "hermes"] {
        write_executable(&fake_bin.join(client), fake_client);
    }
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );
    write_hermes_python_launcher(fake_bin);
}

fn matrix_cell(client_family: &str) -> Value {
    json!({
        "model_slug": "opaque/exposed-slug",
        "client_family": client_family,
        "status": "passed",
        "http_status": 200,
        "duration_ms": 1,
        "optional_downgrades": [],
        "check_results": [{"id": "text_stream", "passed": true}]
    })
}

fn run_compatibility_matrix(response: &Value) -> Output {
    run_compatibility_matrix_with_trace(response, false, "sentinel-admin-password").0
}

fn run_compatibility_matrix_with_trace(
    response: &Value,
    trace: bool,
    admin_password: &str,
) -> (Output, String) {
    let _guard = MATRIX_SCRIPT_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    let response_path = temp.path().join("matrix-response.json");
    fs::write(&response_path, serde_json::to_vec(response).unwrap()).unwrap();
    let env_path = temp.path().join("matrix.env");
    fs::write(&env_path, format!("ADMIN_PASSWORD={admin_password}\n")).unwrap();
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case " $* " in
  *"/api/admin/login "*) printf '{"token":"fake-admin-token"}\n' ;;
  *"/api/admin/troubleshooting/matrix/run "*) cat "$MATRIX_RESPONSE_FILE" ;;
  *) exit 2 ;;
esac
"#,
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let mut command = Command::new("bash");
    if trace {
        command.arg("-x");
    }
    let output = command
        .arg("scripts/compatibility_matrix.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("ENV_FILE", env_path)
        .env("MATRIX_RESPONSE_FILE", response_path)
        .output()
        .unwrap();
    let saved_response = fs::read_to_string("/tmp/compatibility-matrix.json").unwrap_or_default();
    (output, saved_response)
}

#[test]
fn portal_playground_status_stdout_is_not_polluted_by_logs() {
    let script = fs::read_to_string("scripts/portal_playground_e2e.sh")
        .expect("read portal playground e2e script");

    for name in ["log_info", "log_pass", "log_warn", "log_fail"] {
        let marker = format!("{name}() {{");
        let start = script
            .find(&marker)
            .unwrap_or_else(|| panic!("{name} function exists"));
        let rest = &script[start..];
        let end = rest.find("\n}").unwrap_or(rest.len());
        let function_body = &rest[..end];

        assert!(
            function_body.contains(">&2"),
            "{name} must write to stderr so command substitution can reserve stdout for status codes"
        );
    }
}

#[test]
fn portal_playground_e2e_never_rotates_or_prints_downstream_keys() {
    let script = fs::read_to_string("scripts/portal_playground_e2e.sh").unwrap();
    assert!(script.contains(": \"${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}\""));
    assert!(!script.contains("/rotate"));
    assert!(!script.contains("rotate_downstream_key"));
    assert!(!script.contains("ADMIN_PASSWORD"));
    assert!(script.contains("set +x"));
}

#[test]
fn portal_playground_e2e_uses_live_models_without_hardcoded_candidates() {
    let script = fs::read_to_string("scripts/portal_playground_e2e.sh").unwrap();
    assert!(script.contains("$BASE_URL/v1/models"));
    assert!(!script.contains("extra_default"));
    assert!(!script.contains("deepseek-chat"));
}

#[test]
fn compatibility_matrix_script_defaults_to_four_clients_and_semantic_jq_failures() {
    let script = fs::read_to_string("scripts/compatibility_matrix.sh")
        .expect("read compatibility matrix script");

    assert!(script.contains("CLIENTS_JSON"));
    assert!(script.contains("claude_code"));
    assert!(script.contains("jq -e"));
    for evidence in [
        "runtime_model_slug",
        "probe_version",
        "optional_downgrades",
        "selected_upstream_name",
        "selected_upstream_protocol",
    ] {
        assert!(
            script.contains(evidence),
            "missing matrix evidence {evidence}"
        );
    }
}

#[test]
fn compatibility_matrix_rejects_empty_cells() {
    let output = run_compatibility_matrix(&json!({
        "run_id": "run-empty",
        "downstream_id": "test",
        "summary": {"passed": 0, "warning": 0, "failed": 0},
        "cells": []
    }));

    assert!(
        !output.status.success(),
        "empty matrix cells must fail the semantic gate\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn compatibility_matrix_requires_every_requested_client_for_each_model() {
    let output = run_compatibility_matrix(&json!({
        "run_id": "run-missing-client",
        "downstream_id": "test",
        "summary": {"passed": 3, "warning": 0, "failed": 0},
        "cells": [
            matrix_cell("codex"),
            matrix_cell("opencode"),
            matrix_cell("claude_code")
        ]
    }));

    assert!(
        !output.status.success(),
        "a model missing a requested client cell must fail the semantic gate\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn compatibility_matrix_accepts_complete_passing_client_cells() {
    let output = run_compatibility_matrix(&json!({
        "run_id": "run-complete",
        "downstream_id": "test",
        "summary": {"passed": 4, "warning": 0, "failed": 0},
        "cells": [
            matrix_cell("codex"),
            matrix_cell("opencode"),
            matrix_cell("claude_code"),
            matrix_cell("hermes")
        ]
    }));

    assert!(
        output.status.success(),
        "complete matrix should pass\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn deployment_scripts_disable_xtrace_before_reading_secrets() {
    for (path, first_secret) in [
        ("scripts/compatibility_matrix.sh", "ADMIN_PASSWORD="),
        ("scripts/render_live_capabilities.sh", "ADMIN_TOKEN"),
        ("scripts/installed_client_smoke.sh", "DOWNSTREAM_KEY"),
    ] {
        let script = fs::read_to_string(path).unwrap();
        let disable_xtrace = script
            .find("set +x")
            .unwrap_or_else(|| panic!("{path} must explicitly disable inherited xtrace"));
        let secret_handling = script.find(first_secret).unwrap();
        assert!(
            disable_xtrace < secret_handling,
            "{path} disables xtrace only after handling {first_secret}"
        );
    }
}

#[test]
fn redis_runtime_smoke_is_isolated_and_secret_safe() {
    let script = fs::read_to_string("scripts/redis_runtime_smoke.sh")
        .expect("Redis runtime smoke script should exist");

    for marker in [
        "set -euo pipefail",
        "set +x",
        "openssl rand",
        "SMOKE_PREFIX",
        "STATE_DIR",
        "chmod 0777 \"$STATE_DIR\"",
        "-v \"$STATE_DIR:/data\"",
        "docker network create",
        "gateway-a",
        "gateway-b",
        "trap cleanup EXIT",
        "3301",
        "3302",
        "route_health.cooldown_routes",
        "runtime_state.cooldown_remaining",
        "upstream_routes_exhausted",
    ] {
        assert!(
            script.contains(marker),
            "smoke script should contain `{marker}`"
        );
    }

    assert!(!script.contains("set -x"));
    assert!(!script.contains("16379"));
    assert!(!script.contains("docker volume create"));
    assert!(!script.contains("docker ps"));
    assert!(!script.contains("docker container prune"));
    assert!(!script.contains("docker network prune"));
    assert!(!script.contains("docker volume prune"));
    assert!(!script.contains("echo \"$ADMIN_PASSWORD\""));
    assert!(!script.contains("echo \"$DOWNSTREAM_KEY\""));
    assert!(script.contains(r#"(WORKDIR / "hold.started").write_text("started")"#));
    assert!(!script.contains(r#"(WORKDIR / "hold.started").touch()"#));
    assert!(script.contains("rate_limit_enabled: true"));
    assert!(!script.contains("rate_limit_enabled: false"));

    let redis_run = script
        .find("--name \"$REDIS_CONTAINER\"")
        .expect("smoke should start its exact Redis container");
    let redis_tail = &script[redis_run..];
    let redis_run_end = redis_tail
        .find("\ndocker run -d")
        .unwrap_or(redis_tail.len());
    let redis_block = &redis_tail[..redis_run_end];
    assert!(!redis_block.contains("\n  -p "));
    assert!(!redis_block.contains("--publish"));

    let disable_xtrace = script.find("set +x").unwrap();
    let first_secret = script.find("ADMIN_PASSWORD=").unwrap();
    assert!(disable_xtrace < first_secret);

    let plan = fs::read_to_string(
        "docs/superpowers/plans/2026-07-29-optional-redis-runtime-coordination.md",
    )
    .unwrap();
    assert!(plan.contains("deterministically delayed mock upstream"));
}

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
    let compose_source = fs::read_to_string(repo_root.join("docker-compose.yml")).unwrap();
    assert!(compose_source.contains("max-size: \"50m\""));
    assert!(compose_source.contains("max-file: \"5\""));
    assert!(compose_source.contains("LOG_ROTATION: ${LOG_ROTATION:-daily}"));
    assert!(compose_source.contains("LOG_ROTATION_MAX_FILES: ${LOG_ROTATION_MAX_FILES:-14}"));
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

#[test]
fn deploy_waits_for_container_health_before_reporting_success() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = temp.path().join("repo");
    let scripts_dir = repo_root.join("scripts");
    let frontend_dir = repo_root.join("frontend");
    let fake_bin = temp.path().join("bin");
    let deploy_dir = temp.path().join("deploy");
    let trace = temp.path().join("deploy-trace.txt");
    let curl_trace = temp.path().join("curl-trace.txt");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::create_dir_all(&frontend_dir).unwrap();
    fs::create_dir(&fake_bin).unwrap();

    write_executable(
        &scripts_dir.join("deploy.sh"),
        &fs::read_to_string("scripts/deploy.sh").unwrap(),
    );
    fs::copy("docker-compose.yml", repo_root.join("docker-compose.yml")).unwrap();
    fs::copy(".env.example", repo_root.join(".env.example")).unwrap();

    write_executable(
        &fake_bin.join("npm"),
        r#"#!/usr/bin/env bash
set -euo pipefail
mkdir -p frontend/dist/assets
printf 'console.log("key_concurrency")\n' >frontend/dist/assets/Upstreams-test.js
"#,
    );
    write_executable(
        &fake_bin.join("cargo"),
        r#"#!/usr/bin/env bash
set -euo pipefail
mkdir -p target/release
printf '#!/usr/bin/env bash\nexit 0\n' >target/release/chat-responses-codex
chmod +x target/release/chat-responses-codex
"#,
    );
    write_executable(
        &fake_bin.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "compose" && "${2:-}" == "version" ]]; then
  exit 0
fi
if [[ "${1:-}" == "inspect" ]]; then
  printf 'healthy'
  printf 'docker' >>"$DEPLOY_TRACE"
  printf '\t%s' "$@" >>"$DEPLOY_TRACE"
  printf '\n' >>"$DEPLOY_TRACE"
  exit 0
fi
if [[ "${1:-}" == "compose" ]]; then
  printf 'docker' >>"$DEPLOY_TRACE"
  printf '\t%s' "$@" >>"$DEPLOY_TRACE"
  printf '\n' >>"$DEPLOY_TRACE"
  exit 0
fi
printf 'unexpected docker invocation:' >&2
printf ' %s' "$@" >&2
printf '\n' >&2
exit 92
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'curl' >>"$CURL_TRACE"
printf '\t%s' "$@" >>"$CURL_TRACE"
printf '\n' >>"$CURL_TRACE"
url="${@: -1}"
path="/${url#*://*/}"
case "$path" in
  /healthz)
    printf 'ok'
    ;;
  /)
    printf '<html><script src="assets/index-main.js"></script></html>'
    ;;
  /assets/index-main.js)
    printf 'window.__assets__=["assets/Upstreams-test.js"]'
    ;;
  /assets/Upstreams-test.js)
    printf 'console.log("key_concurrency")\n'
    ;;
  *)
    printf 'unexpected curl url: %s\n' "$url" >&2
    exit 94
    ;;
esac
"#,
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/deploy.sh")
        .arg("--deploy-dir")
        .arg(&deploy_dir)
        .arg("--skip-build")
        .current_dir(&repo_root)
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("DEPLOY_TRACE", &trace)
        .env("CURL_TRACE", &curl_trace)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "deploy fixture failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let trace_text = fs::read_to_string(&trace).unwrap();
    let compose_up = trace_text.find("up\t-d\t--remove-orphans").unwrap();
    let compose_ps = trace_text.find("\tps").unwrap();
    assert!(compose_up < compose_ps, "compose ps must follow compose up");

    let curl_trace = fs::read_to_string(&curl_trace).unwrap_or_default();
    assert!(
        curl_trace.contains("healthz"),
        "deployed gateway /healthz was never polled"
    );
    assert!(
        curl_trace.contains("assets/Upstreams-test.js"),
        "deployed Upstreams chunk was never fetched"
    );
}

#[test]
fn compatibility_matrix_xtrace_does_not_leak_admin_password() {
    let secret = "matrix-xtrace-secret-sentinel";
    let response = json!({
        "run_id": "run-xtrace",
        "downstream_id": "test",
        "summary": {"passed": 4, "warning": 0, "failed": 0},
        "cells": [
            matrix_cell("codex"),
            matrix_cell("opencode"),
            matrix_cell("claude_code"),
            matrix_cell("hermes")
        ]
    });
    let (output, saved_response) = run_compatibility_matrix_with_trace(&response, true, secret);

    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    assert!(!saved_response.contains(secret));
}

#[test]
fn render_live_capabilities_is_exact_and_imports_only_when_requested() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    let curl_marker = temp.path().join("curl-called");
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf called >\"$CURL_MARKER\"\nprintf '{}\\n'\n",
    );
    let inherited_path = std::env::var("PATH").unwrap();
    let path = format!("{}:{inherited_path}", fake_bin.display());
    let secret = "render-xtrace-secret-sentinel";
    let rendered_path = temp.path().join("rendered.json");

    let imported = Command::new("bash")
        .arg("-x")
        .arg("scripts/render_live_capabilities.sh")
        .arg("--output")
        .arg(&rendered_path)
        .arg("--import")
        .env("PATH", &path)
        .env("QWEN_VLM_SLUG", "opaque/qwen-vlm")
        .env("IMAGE_FIXTURE_URL", "https://fixture.invalid/image.png")
        .env("IMAGE_FIXTURE_EXPECTED_LABEL", "high-contrast-square")
        .env("BASE_URL", "https://gateway.invalid")
        .env("ADMIN_TOKEN", secret)
        .env("CURL_MARKER", &curl_marker)
        .output()
        .unwrap();
    let rendered = fs::read_to_string(&rendered_path).unwrap();

    assert!(
        imported.status.success(),
        "render/import failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&imported.stdout),
        String::from_utf8_lossy(&imported.stderr)
    );
    assert!(
        curl_marker.exists(),
        "--import must call the admin endpoint"
    );
    assert!(!String::from_utf8_lossy(&imported.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&imported.stderr).contains(secret));
    assert!(!rendered.contains(secret));

    let document: Value = serde_json::from_str(&rendered).unwrap();
    let qwen = document["compatibility_expectations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|expectation| expectation["id"] == "selected-qwen-vlm")
        .unwrap();
    assert_eq!(
        qwen,
        &json!({
            "id": "selected-qwen-vlm",
            "selector": {"exposed_model": "opaque/qwen-vlm"},
            "bundles": ["agent_core", "image_agent"],
            "client_profiles": ["codex", "opencode", "claude_code", "hermes"],
            "permitted_optional_downgrades": ["optional_image_detail"],
            "https_image_fixture": {
                "url": "https://fixture.invalid/image.png",
                "expected_label": "high-contrast-square"
            }
        })
    );

    fs::remove_file(&curl_marker).unwrap();
    let rendered_without_import = temp.path().join("rendered-without-import.json");
    let local_only = Command::new("bash")
        .arg("scripts/render_live_capabilities.sh")
        .arg("--output")
        .arg(&rendered_without_import)
        .env("PATH", &path)
        .env("QWEN_VLM_SLUG", "opaque/qwen-vlm")
        .env("IMAGE_FIXTURE_URL", "https://fixture.invalid/image.png")
        .env("IMAGE_FIXTURE_EXPECTED_LABEL", "high-contrast-square")
        .env("BASE_URL", "https://gateway.invalid")
        .env("ADMIN_TOKEN", secret)
        .env("CURL_MARKER", &curl_marker)
        .output()
        .unwrap();
    assert!(local_only.status.success());
    assert!(
        !curl_marker.exists(),
        "rendering without --import must not call curl"
    );

    for empty_name in [
        "QWEN_VLM_SLUG",
        "IMAGE_FIXTURE_URL",
        "IMAGE_FIXTURE_EXPECTED_LABEL",
    ] {
        let invalid_output = temp.path().join(format!("invalid-{empty_name}.json"));
        let mut command = Command::new("bash");
        command
            .arg("scripts/render_live_capabilities.sh")
            .arg("--output")
            .arg(invalid_output)
            .env("PATH", &path)
            .env("QWEN_VLM_SLUG", "opaque/qwen-vlm")
            .env("IMAGE_FIXTURE_URL", "https://fixture.invalid/image.png")
            .env("IMAGE_FIXTURE_EXPECTED_LABEL", "high-contrast-square")
            .env(empty_name, "");
        assert!(
            !command.output().unwrap().status.success(),
            "empty {empty_name} must fail"
        );
    }
}

#[test]
fn installed_client_smoke_xtrace_does_not_leak_downstream_key() {
    let secret = "smoke-xtrace-secret-sentinel";
    let output = Command::new("bash")
        .arg("-x")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", "/usr/bin:/bin")
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", secret)
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .output()
        .unwrap();

    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
}

#[test]
fn installed_client_smoke_reports_missing_hermes_mcp_extra_before_model_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let empty_pythonpath = temp.path().join("empty-pythonpath");
    let model_task_marker = temp.path().join("model-task-started");
    let hermes_mcp_driver = temp.path().join("hermes-mcp-driver.mjs");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&empty_pythonpath).unwrap();
    write_hermes_mcp_driver(&hermes_mcp_driver);
    write_prerequisite_smoke_clients(&fake_bin);

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("HERMES_PYTHONPATH", &empty_pythonpath)
        .env("MODEL_TASK_MARKER", &model_task_marker)
        .env("HERMES_MCP_DRIVER", &hermes_mcp_driver)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "missing Hermes mcp extra must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "client=hermes task=mcp_preflight prerequisite=python_mcp_extra status=prerequisite_failed"
        ),
        "missing mcp extra must be reported as a prerequisite failure\nstderr:\n{stderr}"
    );
    assert!(
        !model_task_marker.exists(),
        "no model task may start after a failed Hermes MCP preflight"
    );
    assert!(
        !stderr.contains("task=read_only_tool_proof"),
        "missing dependency must not be mislabeled as a tool-call failure\nstderr:\n{stderr}"
    );
}

#[test]
fn installed_client_smoke_honors_hermes_pythonpath_for_mcp_preflight() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let model_task_marker = temp.path().join("model-task-started");
    let preflight_marker = temp.path().join("python-preflight-ran");
    let hermes_mcp_driver = temp.path().join("hermes-mcp-driver.mjs");
    fs::create_dir(&fake_bin).unwrap();
    write_hermes_mcp_driver(&hermes_mcp_driver);
    write_prerequisite_smoke_clients(&fake_bin);
    let pythonpath = write_fake_mcp_package(temp.path());

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("HERMES_PYTHONPATH", &pythonpath)
        .env("HERMES_PYTHON_PREFLIGHT_MARKER", &preflight_marker)
        .env("MODEL_TASK_MARKER", &model_task_marker)
        .env("HERMES_MCP_DRIVER", &hermes_mcp_driver)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "isolated Hermes mcp extra should allow the smoke workflow\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        preflight_marker.exists(),
        "Hermes Python preflight did not run"
    );
    assert!(
        model_task_marker.exists(),
        "smoke model tasks did not continue"
    );
}

#[test]
fn installed_client_smoke_requires_a_real_hermes_read_only_tool_call() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    write_hermes_python_launcher(&fake_bin);
    let hermes_pythonpath = write_fake_mcp_package(temp.path());
    let hermes_mcp_driver = temp.path().join("hermes-mcp-driver.mjs");
    write_hermes_mcp_driver(&hermes_mcp_driver);
    let fake_client = r#"#!/usr/bin/env bash
set -euo pipefail
client="$(basename "$0")"
if [[ "${1:-}" == "--version" ]]; then
  case "$client" in
    codex) printf 'codex-cli 0.146.0\n' ;;
    opencode) printf '1.17.18\n' ;;
    claude) printf '2.1.195 (Claude Code)\n' ;;
    hermes) printf 'Hermes Agent v0.14.0\n' ;;
  esac
  exit 0
fi
args=" $* "
if [[ "$client" == "codex" && "$args" == *" login "* ]]; then
  exit 0
elif [[ "$client" == "codex" && "$args" == *"Delegate exactly one"* ]]; then
  marker="$(<probe.txt)"
  printf '{"type":"turn.started"}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
  jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
elif [[ "$client" == "hermes" && "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
      printf 'CLIENT_TEXT_SMOKE_OK\n'
elif [[ "$client" == "hermes" ]]; then
  if [[ "${HERMES_SKIP_MCP_PROOF:-0}" == "1" ]]; then
    cat probe.txt
    exit 0
  fi
  if [[ -f "${HERMES_HOME:-}/config.yaml" ]] \
     && grep -q '^mcp_servers:' "${HERMES_HOME}/config.yaml" \
     && grep -q 'include:.*lookup' "${HERMES_HOME}/config.yaml" \
     && grep -q 'resources: false' "${HERMES_HOME}/config.yaml" \
     && grep -q 'prompts: false' "${HERMES_HOME}/config.yaml" \
     && ! grep -Eq 'terminal|write_file|patch|search_files' "${HERMES_HOME}/config.yaml"; then
    node "$HERMES_MCP_DRIVER"
  else
    printf 'Hermes read-only MCP tool was not configured\n' >&2
    exit 42
  fi
elif [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
else
  cat probe.txt
fi
if [[ "$client" == "codex" ]]; then
  printf '{"type":"turn.completed"}\n'
fi
"#;
    for client in ["codex", "opencode", "claude", "hermes"] {
        write_executable(&fake_bin.join(client), fake_client);
    }
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("HERMES_PYTHONPATH", &hermes_pythonpath)
        .env("HERMES_MCP_DRIVER", &hermes_mcp_driver)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(
            "client=hermes task=read_only_tool_proof calls=1 tool=lookup status=verified"
        ),
        "Hermes read-only task must prove one server-observed lookup call"
    );

    let missing_proof = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("HERMES_PYTHONPATH", &hermes_pythonpath)
        .env("HERMES_MCP_DRIVER", &hermes_mcp_driver)
        .env("HERMES_SKIP_MCP_PROOF", "1")
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();
    assert!(
        !missing_proof.status.success(),
        "Hermes marker output without a server-observed proof must fail"
    );
    assert!(
        String::from_utf8_lossy(&missing_proof.stderr)
            .contains("client=hermes task=read_only_tool_proof calls=0 tool=none status=failed"),
        "missing proof failure must identify the absent lookup call\nstderr:\n{}",
        String::from_utf8_lossy(&missing_proof.stderr)
    );
}

#[test]
fn installed_client_smoke_script_pins_defaults_and_allows_explicit_expected_versions() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh")
        .expect("read installed client smoke script");

    for fixed_pin in [
        "readonly DEFAULT_CODEX_VERSION=\"0.146.0\"",
        "readonly DEFAULT_CLINE_VERSION=\"0.0.13\"",
        "readonly DEFAULT_OPENCODE_VERSION=\"1.17.18\"",
        "readonly DEFAULT_CLAUDE_CODE_VERSION=\"2.1.195\"",
        "readonly DEFAULT_KILO_VERSION=\"7.4.20\"",
        "readonly DEFAULT_HERMES_VERSION=\"0.14.0\"",
    ] {
        assert!(
            script.contains(fixed_pin),
            "missing immutable client pin {fixed_pin}"
        );
    }
    for explicit_override in [
        "EXPECTED_CODEX_VERSION:-$DEFAULT_CODEX_VERSION",
        "EXPECTED_CLINE_VERSION:-$DEFAULT_CLINE_VERSION",
        "EXPECTED_OPENCODE_VERSION:-$DEFAULT_OPENCODE_VERSION",
        "EXPECTED_CLAUDE_CODE_VERSION:-$DEFAULT_CLAUDE_CODE_VERSION",
        "EXPECTED_KILO_VERSION:-$DEFAULT_KILO_VERSION",
        "EXPECTED_HERMES_VERSION:-$DEFAULT_HERMES_VERSION",
    ] {
        assert!(
            script.contains(explicit_override),
            "missing explicit tested-version override {explicit_override}"
        );
    }
    for command in [
        "\"$CODEX_BIN\" exec",
        "\"$CLINE_BIN\" --json",
        "\"$OPENCODE_BIN\" run",
        "\"$CLAUDE_CODE_BIN\" -p",
        "\"$KILO_BIN\" run",
        "\"$HERMES_BIN\" chat",
    ] {
        assert!(
            script.contains(command),
            "missing real client command {command}"
        );
    }
    assert!(script.contains("text_task"));
    assert!(script.contains("read_only_tool_task"));
    assert!(script.contains("mcp_smoke_readonly_lookup"));
    assert!(script.contains("tools:"));
    assert!(script.contains("include: [lookup]"));
    assert!(script.contains("resources: false"));
    assert!(script.contains("prompts: false"));
    assert!(!script.contains("hermes --oneshot"));
    assert!(script.contains("\"$CLAUDE_CODE_BIN\" -p \"$TEXT_PROMPT\""));
    assert!(script.contains("\"$CLAUDE_CODE_BIN\" -p \"$READ_FILE_PROMPT\""));
    assert!(script.contains("CLAUDE_CONFIG_DIR=\"$WORKDIR/claude-home\""));
    assert!(script.contains("web_search = \"disabled\""));
    assert!(script.contains("stream_max_retries = 2"));
    assert!(!script.contains("disable_response_storage"));
    for client in [
        "codex",
        "cline",
        "opencode",
        "claude_code",
        "kilo",
        "hermes",
    ] {
        assert!(
            script.contains(&format!(
                "client={client} task=attachment status=protocol_matrix_covered"
            )),
            "missing explicit attachment evidence for {client}"
        );
    }
    assert!(!script.contains("echo \"$DOWNSTREAM_KEY\""));
}

#[test]
fn installed_client_smoke_uses_portal_codex_profile_and_checks_delegation() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh")
        .expect("read installed client smoke script");

    for required in [
        "[features]",
        "multi_agent = true",
        "multi_agent_v2 = false",
        "[agents]",
        "requires_openai_auth = true",
        "login --with-api-key",
        "$CODEX_HOME_DIR/agents/default.toml",
        "model_reasoning_effort",
        "developer_instructions",
        "collab_tool_call",
        "spawn_agent",
        "wait",
        "turn.completed",
    ] {
        assert!(script.contains(required), "smoke script missing {required}");
    }
    assert!(
        !script.contains("CODEX_DELEGATION_MARKER"),
        "the delegation prompt must not contain its expected runtime result"
    );
    assert!(script.contains("delegation_result_mismatch"));
    for instruction in [
        "Call spawn_agent exactly once",
        "Call wait exactly once for that child, even if it has already finished",
        "Your final reply must contain only the child result returned by wait",
    ] {
        assert!(
            script.contains(instruction),
            "delegation prompt is missing unambiguous instruction: {instruction}"
        );
    }
    assert!(
        !script.contains("Do not spawn, wait, or message any additional agents"),
        "the delegation prompt must not ambiguously discourage the required wait"
    );
    assert!(
        script.contains("agent_profile") && script.contains("authentication"),
        "smoke script must classify profile and authentication failures"
    );
    assert!(!script.contains("cat \"$WORKDIR/codex-namespace.jsonl\""));
}

#[test]
fn installed_client_smoke_logs_in_inside_isolated_codex_home() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let smoke_tmp = temp.path().join("tmp");
    let login_home = temp.path().join("login-home");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&smoke_tmp).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf 'codex-cli 0.146.0\n'
  exit 0
fi
if [[ "${1:-}" == "login" ]]; then
  printf '%s' "${CODEX_HOME:-}" >"$CODEX_LOGIN_HOME_MARKER"
  exit 0
fi
args=" $* "
if [[ "$args" == *"Delegate exactly one"* ]]; then
  marker="$(<probe.txt)"
  jq -nc '{type:"turn.started"}'
  jq -nc '{type:"item.completed",item:{type:"collab_tool_call",tool:"spawn_agent",status:"completed"}}'
  jq -nc '{type:"item.completed",item:{type:"collab_tool_call",tool:"wait",status:"completed"}}'
  jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
elif [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
else
  cat probe.txt
fi
printf '{"type":"turn.completed"}\n'
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("TMPDIR", &smoke_tmp)
        .env_remove("CODEX_HOME")
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("CLIENTS_JSON", r#"["codex"]"#)
        .env("CODEX_LOGIN_HOME_MARKER", &login_home)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "isolated Codex smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let captured_home = fs::read_to_string(login_home).unwrap();
    assert!(
        captured_home.starts_with(smoke_tmp.to_str().unwrap()),
        "login must receive a temporary CODEX_HOME, got {captured_home:?}"
    );
}

#[test]
fn installed_client_smoke_can_run_only_the_codex_delegation_task() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let smoke_tmp = temp.path().join("tmp");
    let execution_marker = temp.path().join("codex-executions");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&smoke_tmp).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf 'codex-cli 0.146.0\n'
  exit 0
fi
if [[ "${1:-}" == "login" ]]; then
  exit 0
fi
printf 'exec\n' >>"$CODEX_EXECUTION_MARKER"
args=" $* "
if [[ "$args" == *"Delegate exactly one"* ]]; then
  marker="$(<probe.txt)"
  printf '{"type":"turn.started"}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
  jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
elif [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
else
  cat probe.txt
fi
printf '{"type":"turn.completed"}\n'
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("TMPDIR", &smoke_tmp)
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("CLIENTS_JSON", r#"["codex"]"#)
        .env("CODEX_TASKS", "delegation")
        .env("CODEX_EXECUTION_MARKER", &execution_marker)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "delegation-only Codex smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(execution_marker)
            .unwrap()
            .lines()
            .count(),
        1,
        "delegation-only smoke must execute Codex exactly once"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("client=codex task=delegation"));
    assert!(!stdout.contains("client=codex task=text_task"));
    assert!(!stdout.contains("client=codex task=read_only_tool_task"));
}

#[test]
fn installed_client_smoke_rejects_unproven_delegation() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let smoke_tmp = temp.path().join("tmp");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&smoke_tmp).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf 'codex-cli 0.146.0\n'
  exit 0
fi
if [[ "${1:-}" == "login" ]]; then
  exit 0
fi
marker="$(<probe.txt)"
case "$FAKE_DELEGATION_MODE" in
  fixed_marker)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}\n'
    printf '{"type":"item.completed","item":{"type":"agent_message","text":"CODEX_DELEGATION_MARKER"}}\n'
    printf '{"type":"turn.completed"}\n'
    ;;
  wrong_final)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"item.completed","item":{"type":"agent_message","text":"wrong-final-result"}}\n'
    printf '{"type":"turn.completed"}\n'
    ;;
  nonzero)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"turn.completed"}\n'
    exit 7
    ;;
  no_collab)
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"turn.completed"}\n'
    ;;
  unknown_legacy_collab)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"turn.completed"}\n'
    ;;
  duplicate_collab)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}\n'
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"turn.completed"}\n'
    ;;
  failed_collab)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"failed"}}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"turn.completed"}\n'
    ;;
  early_turn_completed)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","status":"completed"}}\n'
    printf '{"type":"turn.completed"}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    ;;
  extra_turn)
    printf '{"type":"turn.started"}\n'
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
    printf '{"type":"item.completed","item":{"type":"agent_message","text":"delegated-turn-finished"}}\n'
    printf '{"type":"turn.completed"}\n'
    printf '{"type":"turn.started"}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"turn.completed"}\n'
    ;;
  wrapped_final)
    printf '{"type":"turn.started"}\n'
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:("Child result:\n" + $marker + "\nDone.")}}'
    printf '{"type":"turn.completed"}\n'
    ;;
  missing_wait)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
    jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
    printf '{"type":"turn.completed"}\n'
    ;;
  leaked_prompt)
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
    printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
    jq -nc --arg marker "$marker" --arg sentinel "${CODEX_DELEGATION_LEAK_SENTINEL:-missing-sentinel}" \
      '{type:"item.completed",item:{type:"agent_message",text:($marker + "\n" + $sentinel)}}'
    printf '{"type":"turn.completed"}\n'
    ;;
esac
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let run = |mode: &str| {
        Command::new("bash")
            .arg("scripts/installed_client_smoke.sh")
            .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
            .env("TMPDIR", &smoke_tmp)
            .env("BASE_URL", "https://gateway.invalid")
            .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
            .env("MODEL_SLUG", "opaque/exposed-slug")
            .env("CLIENTS_JSON", r#"["codex"]"#)
            .env("CODEX_TASKS", "delegation")
            .env("FAKE_DELEGATION_MODE", mode)
            .env("CLIENT_TIMEOUT_SECONDS", "5")
            .output()
            .unwrap()
    };

    let output = run("nonzero");
    assert!(
        !output.status.success(),
        "a non-zero Codex exit must fail even when its JSONL looks complete"
    );

    for mode in [
        "fixed_marker",
        "wrong_final",
        "no_collab",
        "unknown_legacy_collab",
        "duplicate_collab",
        "failed_collab",
        "early_turn_completed",
        "extra_turn",
        "missing_wait",
        "leaked_prompt",
    ] {
        let output = run(mode);
        assert!(
            !output.status.success(),
            "unproven delegation mode {mode} must fail"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("client=codex task=delegation status=delegation_result_mismatch"),
            "unexpected stderr for {mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output = run("wrapped_final");
    assert!(
        output.status.success(),
        "a bounded final wrapper around the exact child result must pass\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn installed_client_smoke_never_prints_runtime_marker_as_an_event_type() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let smoke_tmp = temp.path().join("tmp");
    let marker_capture = temp.path().join("runtime-marker");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&smoke_tmp).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then printf 'codex-cli 0.146.0\n'; exit 0; fi
if [[ "${1:-}" == "login" ]]; then exit 0; fi
marker="$(<probe.txt)"
printf '%s' "$marker" >"$MARKER_CAPTURE_FILE"
printf '{"type":"turn.started"}\n'
printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
jq -nc --arg marker "$marker" --arg secret 'SENSITIVE_PROMPT_SENTINEL' \
  '{type:"item.completed",item:{type:$marker,prompt:$secret,arguments:{task:$secret}}}'
for index in $(seq 1 32); do
  jq -nc --arg event_type "untrusted.$index" '{type:$event_type}'
done
jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
printf '{"type":"turn.completed"}\n'
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("TMPDIR", &smoke_tmp)
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("CLIENTS_JSON", r#"["codex"]"#)
        .env("CODEX_TASKS", "delegation")
        .env("MARKER_CAPTURE_FILE", &marker_capture)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(output.status.success());
    let marker = fs::read_to_string(marker_capture).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(&marker));
    assert!(!stderr.contains(&marker));
    assert!(!stdout.contains("SENSITIVE_PROMPT_SENTINEL"));
    assert!(!stderr.contains("SENSITIVE_PROMPT_SENTINEL"));
    assert!(!stdout.contains("untrusted."));
    let verified = stdout
        .lines()
        .find(|line| line.contains("task=delegation") && line.contains("status=verified"))
        .unwrap();
    let event_list = verified
        .split_once(" events=")
        .and_then(|(_, suffix)| suffix.split_once(" status=").map(|(events, _)| events))
        .unwrap();
    assert!(event_list.split(',').count() <= 16);
}

#[test]
fn installed_client_smoke_rejects_non_v1_selected_model_before_codex_exec() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let smoke_tmp = temp.path().join("tmp");
    let execution_marker = temp.path().join("codex-executed");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&smoke_tmp).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then printf 'codex-cli 0.146.0\n'; exit 0; fi
if [[ "${1:-}" == "login" ]]; then exit 0; fi
printf 'executed\n' >"$CODEX_EXECUTION_MARKER"
marker="$(<probe.txt)"
printf '{"type":"turn.started"}\n'
printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
printf '{"type":"turn.completed"}\n'
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ -n "${FAKE_MULTI_AGENT_VERSION:-}" ]]; then
  jq -nc --arg version "$FAKE_MULTI_AGENT_VERSION" \
    '{models:[{slug:"opaque/exposed-slug",default_reasoning_level:"none",multi_agent_version:$version}]}'
else
  printf '{"models":[{"slug":"opaque/exposed-slug","default_reasoning_level":"none"}]}\n'
fi
"#,
    );

    let inherited_path = std::env::var("PATH").unwrap();
    for version in [None, Some("v2")] {
        let _ = fs::remove_file(&execution_marker);
        let mut command = Command::new("bash");
        command
            .arg("scripts/installed_client_smoke.sh")
            .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
            .env("TMPDIR", &smoke_tmp)
            .env("BASE_URL", "https://gateway.invalid")
            .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
            .env("MODEL_SLUG", "opaque/exposed-slug")
            .env("CLIENTS_JSON", r#"["codex"]"#)
            .env("CODEX_TASKS", "delegation")
            .env("CLIENT_TIMEOUT_SECONDS", "5")
            .env("CODEX_EXECUTION_MARKER", &execution_marker);
        if let Some(version) = version {
            command.env("FAKE_MULTI_AGENT_VERSION", version);
        } else {
            command.env_remove("FAKE_MULTI_AGENT_VERSION");
        }
        let output = command.output().unwrap();

        assert!(
            !output.status.success(),
            "selected model transport {:?} must be rejected",
            version
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(
                "client=codex task=agent_transport category=agent_transport status=failed"
            ),
            "unexpected stderr for transport {:?}: {}",
            version,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !execution_marker.exists(),
            "Codex exec must not start for transport {:?}",
            version
        );
    }
}

#[test]
fn installed_client_smoke_rejects_model_missing_from_live_catalog() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let execution_marker = temp.path().join("codex-executed");
    fs::create_dir(&fake_bin).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf 'codex-cli 0.146.0\n'
  exit 0
fi
printf executed >"$CODEX_EXECUTION_MARKER"
exit 0
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("CLIENTS_JSON", r#"["codex"]"#)
        .env("CODEX_EXECUTION_MARKER", &execution_marker)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(!output.status.success(), "missing catalog model must fail");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("client=codex task=agent_profile category=agent_profile status=failed"),
        "missing catalog model must be classified as agent_profile\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !execution_marker.exists(),
        "Codex must not run when its selected catalog model is unavailable"
    );
}

#[test]
fn installed_client_smoke_uses_bounded_codex_failure_categories() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf 'codex-cli 0.146.0\n'
  exit 0
fi
if [[ "${1:-}" == "login" ]]; then
  exit 0
fi
args=" $* "
if [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  case "$FAKE_CODEX_FAILURE" in
    generic_400)
      printf '{"type":"error","status":400,"error":{"category":"invalid_request_error"}}\n'
      ;;
    protocol_400)
      printf '{"type":"error","status":400,"error":{"category":"gateway_protocol_capability_unsupported"}}\n'
      ;;
    protocol_200)
      printf '{"type":"error","status":200,"error":{"category":"gateway_protocol_capability_unsupported"}}\n'
      ;;
    protocol_text_400)
      printf '{"type":"error","message":"retry after 400 seconds","error":{"category":"gateway_protocol_capability_unsupported"}}\n'
      ;;
    generic_499)
      printf '{"type":"error","status_code":499,"error":{"category":"request_cancelled"}}\n'
      ;;
    stream_499)
      printf '{"type":"response.created"}\n'
      printf '{"type":"error","status_code":499,"error":{"category":"stream_client_cancelled"}}\n'
      ;;
    stream_499_before)
      printf '{"type":"error","status_code":499,"error":{"category":"stream_client_cancelled"}}\n'
      printf '{"type":"response.created"}\n'
      ;;
  esac
  exit 1
fi
exit 1
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let run = |failure: &str| {
        Command::new("bash")
            .arg("scripts/installed_client_smoke.sh")
            .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
            .env("BASE_URL", "https://gateway.invalid")
            .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
            .env("MODEL_SLUG", "opaque/exposed-slug")
            .env("CLIENTS_JSON", r#"["codex"]"#)
            .env("CODEX_SKIP_LOGIN", "1")
            .env("FAKE_CODEX_FAILURE", failure)
            .env("CLIENT_TIMEOUT_SECONDS", "5")
            .output()
            .unwrap()
    };

    for (failure, category) in [
        ("generic_400", "unknown"),
        ("generic_499", "unknown"),
        ("protocol_200", "unknown"),
        ("protocol_text_400", "unknown"),
        ("protocol_400", "protocol"),
        ("stream_499", "client_cancelled"),
        ("stream_499_before", "unknown"),
    ] {
        let output = run(failure);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "failure mode {failure} must fail");
        assert!(
            stderr.contains(&format!("category={category}")),
            "failure mode {failure} should be category {category}, stderr:\n{stderr}"
        );
    }
}

#[test]
fn installed_client_smoke_accepts_a_validated_client_subset() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains(
        "CLIENTS_JSON=\"${CLIENTS_JSON:-[\\\"codex\\\",\\\"opencode\\\",\\\"claude_code\\\",\\\"hermes\\\"]}\""
    ));
    assert!(script.contains("client_enabled()"));
    assert!(script.contains("jq -e --arg client"));
    assert!(script.contains("unknown client in CLIENTS_JSON"));
    assert!(script
        .contains("[\"codex\", \"cline\", \"opencode\", \"claude_code\", \"kilo\", \"hermes\"]"));
}

#[test]
fn installed_client_smoke_executes_isolated_cline_and_kilo_clients() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let smoke_tmp = temp.path().join("tmp");
    let unexpected_curl = temp.path().join("unexpected-curl");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&smoke_tmp).unwrap();

    let fake_client = r#"#!/usr/bin/env bash
set -euo pipefail
client="$(basename "$0")"
if [[ "${1:-}" == "--version" ]]; then
  case "$client" in
    clite) printf '0.0.13\n' ;;
    kilo) printf '7.4.20\n' ;;
  esac
  exit 0
fi
args=" $* "
if [[ "$client" == "clite" ]]; then
  [[ "$args" == *' --json '* ]]
  [[ "$args" == *' --plan '* ]]
  [[ "$args" == *' --provider openai-native '* ]]
  [[ "$args" == *' --model opaque/exposed-slug '* ]]
  [[ "$args" == *" --cwd $PWD "* ]]
  jq -e '
    .lastUsedProvider == "openai-native"
    and .providers["openai-native"].settings.model == "opaque/exposed-slug"
    and .providers["openai-native"].settings.baseUrl == "https://gateway.invalid/v1"
    and (.providers["openai-native"].updatedAt | type == "string" and length > 0)
  ' "$CLINE_DATA_DIR/settings/providers.json" >/dev/null
elif [[ "$client" == "kilo" ]]; then
  [[ "$args" == *' run '* ]]
  [[ "$args" == *' --pure '* ]]
  [[ "$args" == *' --format json '* ]]
  [[ "$args" == *' --model gateway/opaque/exposed-slug '* ]]
  [[ "$args" == *" --dir $PWD "* ]]
  [[ "$HOME" == "$TMPDIR/"* ]]
  jq -e '
    .model == "gateway/opaque/exposed-slug"
    and .permission["*"] == "deny"
    and .permission.read == "allow"
  ' <<<"$KILO_CONFIG_CONTENT" >/dev/null
fi
if [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  result='CLIENT_TEXT_SMOKE_OK'
else
  result="$(cat probe.txt)"
fi
if [[ "$client" == "kilo" ]]; then
  jq -nc '{type: "step_start", authorization: "Bearer sentinel-downstream-key"}'
  jq -nc --arg text "$result" '{
    type: "text",
    text: $text,
    response_body: "sentinel-upstream-response-body",
    prompt: "sentinel-task-payload",
    tool_arguments: "sentinel-tool-arguments"
  }'
  jq -nc '{type: "step_finish"}'
else
  printf '%s\n' "$result"
fi
"#;
    for client in ["clite", "kilo"] {
        write_executable(&fake_bin.join(client), fake_client);
    }
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf called >\"$UNEXPECTED_CURL\"\nexit 97\n",
    );

    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("TMPDIR", &smoke_tmp)
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("CLIENTS_JSON", r#"["cline","kilo"]"#)
        .env("UNEXPECTED_CURL", &unexpected_curl)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Cline/Kilo smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !unexpected_curl.exists(),
        "Cline/Kilo-only smoke entered the Codex catalog block"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for client in ["cline", "kilo"] {
        assert!(stdout.contains(&format!("client={client} task=text_task")));
        assert!(stdout.contains(&format!("client={client} task=read_only_tool_task")));
    }
    for task in ["text_task", "read_only_tool_task"] {
        let evidence = stdout
            .lines()
            .find(|line| line.contains(&format!("client=kilo task={task} ")))
            .unwrap_or_else(|| panic!("missing Kilo {task} evidence:\n{stdout}"));
        assert!(
            evidence.contains("events=step_finish,step_start,text"),
            "Kilo {task} evidence did not preserve safe event types: {evidence}"
        );
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for sensitive in [
        "sentinel-downstream-key",
        "sentinel-upstream-response-body",
        "sentinel-task-payload",
        "sentinel-tool-arguments",
        "CLIENT_TEXT_SMOKE_OK",
        "read-only-",
    ] {
        assert!(
            !combined.contains(sensitive),
            "Cline/Kilo evidence leaked sensitive marker {sensitive}"
        );
    }
    for skipped in [
        "client=codex",
        "client=opencode",
        "client=claude_code",
        "client=hermes",
    ] {
        assert!(
            !stdout.contains(skipped),
            "disabled client produced evidence: {skipped}"
        );
    }
}

#[test]
fn installed_client_smoke_executes_only_selected_clients() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let unexpected_curl = temp.path().join("unexpected-curl");
    fs::create_dir(&fake_bin).unwrap();
    write_executable(
        &fake_bin.join("opencode"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf '1.17.18\n'
elif [[ " $* " == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
else
  cat probe.txt
fi
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf called >\"$UNEXPECTED_CURL\"\nexit 97\n",
    );

    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("CLIENTS_JSON", r#"["opencode"]"#)
        .env("UNEXPECTED_CURL", &unexpected_curl)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "OpenCode-only smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !unexpected_curl.exists(),
        "OpenCode-only smoke entered the Codex catalog block"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("client=opencode task=text_task"));
    assert!(stdout.contains("client=opencode task=read_only_tool_task"));
    for skipped in ["client=codex", "client=claude_code", "client=hermes"] {
        assert!(
            !stdout.contains(skipped),
            "disabled client produced smoke evidence: {skipped}\n{stdout}"
        );
    }
}

#[test]
fn installed_client_smoke_uses_a_substantive_text_task() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    let text_prompt = script
        .lines()
        .find(|line| line.starts_with("TEXT_PROMPT="))
        .expect("TEXT_PROMPT assignment");

    assert!(text_prompt.contains("protocol converter"));
    assert!(text_prompt.contains("two concise sentences"));
    assert!(!text_prompt.contains("Reply with exactly ${TEXT_MARKER}."));
    assert!(!text_prompt.to_ascii_lowercase().contains("hello"));
    assert!(!text_prompt.contains("你好"));
}

#[test]
fn opencode_smoke_uses_inline_config_and_temporary_xdg_paths() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains("OPENCODE_CONFIG_CONTENT=\"$OPENCODE_CONFIG_CONTENT\""));
    assert!(script.contains("OPENCODE_DISABLE_PROJECT_CONFIG=1"));
    assert!(script.contains("OPENCODE_DISABLE_AUTOUPDATE=1"));
    for name in [
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ] {
        assert!(script.contains(name), "missing {name}");
    }
    assert!(!script.contains("OPENCODE_CONFIG=\"$OPENCODE_CONFIG_FILE\""));
}

#[test]
fn installed_client_smoke_keeps_exact_primary_versions() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains("DEFAULT_CODEX_VERSION=\"0.146.0\""));
    assert!(script.contains("DEFAULT_OPENCODE_VERSION=\"1.17.18\""));
}

#[test]
fn installed_codex_smoke_tracks_current_client_and_idle_budget() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains("DEFAULT_CODEX_VERSION=\"0.146.0\""));
    assert!(script.contains("stream_idle_timeout_ms = 3600000"));
}

#[test]
fn long_running_smoke_scripts_require_bounded_runtime_and_safe_observability() {
    let delayed = fs::read_to_string("scripts/codex_delayed_output_smoke.sh").unwrap();
    for required in [
        ": \"${API_BASE_URL:?API_BASE_URL is required}\"",
        ": \"${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}\"",
        ": \"${MODEL_SLUG:?MODEL_SLUG is required}\"",
        "mktemp -d",
        "trap cleanup EXIT",
        "stream_idle_timeout_ms = 3600000",
        "multi_agent_v2 = false",
        "turn.completed",
        "status_code == 499",
        "status_code == 502",
        "status_code == 503",
    ] {
        assert!(
            delayed.contains(required),
            "delayed smoke missing {required}"
        );
    }
    assert!(!delayed.contains("set -x"));
    assert!(!delayed.contains("echo \"$DOWNSTREAM_KEY\""));

    let redis = fs::read_to_string("scripts/redis_runtime_smoke.sh").unwrap();
    for required in [
        "/api/admin/downstreams/runtime",
        "AUTHORIZED_CAPACITY_REQUESTS",
        "waiting_upstream",
        "admitted",
        "available",
    ] {
        assert!(redis.contains(required), "Redis smoke missing {required}");
    }
    assert!(!redis.contains("provider_limit"));
}

#[test]
fn task13_smokes_match_the_serial_release_contract() {
    let installed = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(installed.contains(": \"${API_BASE_URL:?API_BASE_URL is required}\""));
    assert!(installed.contains("CLIENTS=\"${CLIENTS:-}\""));
    assert!(installed.contains("record_codex_case"));

    let delayed = fs::read_to_string("scripts/codex_delayed_output_smoke.sh").unwrap();
    assert!(delayed.contains(
        "timeout --kill-after=\"${CLIENT_KILL_AFTER_SECONDS}s\" \"$CLIENT_TIMEOUT_SECONDS\""
    ));
    assert!(delayed.contains("matched_usage_rows"));
    assert!(delayed.contains("REQUEST_ID is required"));

    let redis = fs::read_to_string("scripts/redis_runtime_smoke.sh").unwrap();
    assert!(redis.contains("GATEWAY_B_WAITER_PID"));
    assert!(redis.contains("runtime endpoint exposed admitted and waiting_upstream counts"));
}

#[test]
fn installed_client_smoke_only_requests_tools_that_each_client_has() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh")
        .expect("read installed client smoke script");

    assert!(script.contains("READ_FILE_PROMPT="));
    assert!(script.contains("HERMES_READ_PROMPT="));
    assert!(script.contains("record_codex_case codex read_only_tool_task"));
    assert!(script.contains("--model \"$MODEL_SLUG\" \"$READ_FILE_PROMPT\""));
    assert!(script.contains("record_case opencode read_only_tool_task"));
    assert!(script.contains("\"$READ_FILE_PROMPT\""));
    assert!(script.contains("record_case claude_code read_only_tool_task"));
    assert!(script.contains("\"$CLAUDE_CODE_BIN\" -p \"$READ_FILE_PROMPT\""));
    assert!(script.contains("record_case hermes read_only_tool_task"));
    assert!(script.contains("--query \"$HERMES_READ_PROMPT\""));
    assert!(script.contains("the lookup member in the mcp__smoke_namespace namespace"));
    assert!(!script.contains("mcp__smoke_namespace__lookup tool"));
}

#[test]
fn installed_client_smoke_requires_structured_codex_namespace_proof() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    write_executable(
        &fake_bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf 'codex-cli 0.146.0\n'
  exit 0
fi
args=" $* "
if [[ "$args" == *" login "* ]]; then
  exit 0
elif [[ "$args" == *"Delegate exactly one"* ]]; then
  marker="$(<probe.txt)"
  printf '{"type":"turn.started"}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
  jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
  printf '{"type":"turn.completed"}\n'
elif [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
elif grep -q '^\[mcp_servers\.smoke_namespace\]$' "$CODEX_HOME/config.toml"; then
  config="$(<"$CODEX_HOME/config.toml")"
  [[ "$config" =~ NAMESPACE_MARKER\ =\ \"([^\"]+)\" ]] || exit 91
  marker="${BASH_REMATCH[1]}"
  proof_file=""
  if [[ "$config" =~ NAMESPACE_MCP_PROOF_FILE\ =\ \"([^\"]+)\" ]]; then
    proof_file="${BASH_REMATCH[1]}"
  fi
  case "$FAKE_NAMESPACE_MODE" in
    marker_only)
      printf '%s\n' "$marker"
      ;;
    valid)
      printf 'lookup\n' >>"$proof_file"
      jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"mcp_tool_call",server:"smoke_namespace",tool:"lookup",status:"completed",result:{content:[{type:"text",text:$marker}]}}}'
      jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
      jq -nc '{type:"turn.completed"}'
      ;;
    duplicate)
      printf 'lookup\nlookup\n' >>"$proof_file"
      jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"mcp_tool_call",server:"smoke_namespace",tool:"lookup",status:"completed",result:{content:[{type:"text",text:$marker}]}}}'
      jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
      jq -nc '{type:"turn.completed"}'
      ;;
    wrong_tool)
      printf 'other\n' >>"$proof_file"
      jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"mcp_tool_call",server:"smoke_namespace",tool:"other",status:"completed",result:{content:[{type:"text",text:$marker}]}}}'
      jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
      jq -nc '{type:"turn.completed"}'
      ;;
  esac
else
  cat probe.txt
fi
"#,
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let run = |mode: &str| {
        Command::new("bash")
            .arg("scripts/installed_client_smoke.sh")
            .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
            .env("BASE_URL", "https://gateway.invalid")
            .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
            .env("MODEL_SLUG", "opaque/exposed-slug")
            .env("CLIENTS_JSON", r#"["codex"]"#)
            .env("CODEX_NAMESPACE_TEST", "1")
            .env("FAKE_NAMESPACE_MODE", mode)
            .env("CLIENT_TIMEOUT_SECONDS", "5")
            .output()
            .unwrap()
    };

    let marker_only = run("marker_only");
    assert!(
        !marker_only.status.success(),
        "a marker without a structured MCP event or server proof must fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&marker_only.stdout),
        String::from_utf8_lossy(&marker_only.stderr)
    );

    let valid = run("valid");
    assert!(
        valid.status.success(),
        "one matching MCP event and server call must pass\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&valid.stdout),
        String::from_utf8_lossy(&valid.stderr)
    );
    let stdout = String::from_utf8_lossy(&valid.stdout);
    assert!(stdout.contains(
        "client=codex task=namespace_proof calls=1 tool=lookup jsonl=verified status=verified"
    ));

    for mode in ["duplicate", "wrong_tool"] {
        let output = run(mode);
        assert!(
            !output.status.success(),
            "namespace proof mode {mode} must fail\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn protocol_fidelity_verification_does_not_accept_superseded_codex_evidence() {
    let document = fs::read_to_string("docs/verification/2026-07-10-agent-protocol-fidelity.md")
        .expect("read protocol fidelity verification document");
    let lower = document.to_ascii_lowercase();

    assert!(
        lower.contains("0.144.1") && lower.contains("superseded"),
        "the old Codex 0.144.1 source evidence must be marked superseded"
    );
    assert!(
        lower.contains("not accepted") && lower.contains("0.144.0") && lower.contains("pending"),
        "the pinned 0.144.0 Codex acceptance must remain pending until it is measured"
    );
    assert!(
        !document.contains("| Codex | `0.144.1` | passed | passed |"),
        "superseded Codex 0.144.1 evidence must not remain an accepted installed-client result"
    );
    assert!(
        !document.contains("Each client completed"),
        "the acceptance summary must not claim completion for pending Codex 0.144.0"
    );
}

#[test]
fn installed_client_smoke_uses_the_same_path_binary_for_version_and_execution() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    let execution_marker = temp.path().join("codex-executed");
    let hermes_mcp_driver = temp.path().join("hermes-mcp-driver.mjs");
    write_hermes_mcp_driver(&hermes_mcp_driver);

    let fake_client = r#"#!/usr/bin/env bash
set -euo pipefail
client="$(basename "$0")"
if [[ "${1:-}" == "--version" ]]; then
  case "$client" in
    codex) printf 'codex-cli 0.144.1\n' ;;
    opencode) printf '1.17.18\n' ;;
    claude) printf '2.1.195 (Claude Code)\n' ;;
    hermes) printf 'Hermes Agent v0.14.0\n' ;;
  esac
  exit 0
fi
args=" $* "
if [[ "$client" == "codex" ]]; then
  printf executed >"$CODEX_EXECUTION_MARKER"
fi
if [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
elif [[ "$client" == "hermes" ]]; then
  node "$HERMES_MCP_DRIVER"
else
  cat probe.txt
fi
"#;
    for client in ["codex", "opencode", "claude", "hermes"] {
        write_executable(&fake_bin.join(client), fake_client);
    }
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let path = format!("{}:{inherited_path}", fake_bin.display());
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"codex() {
  if [[ "${1:-}" == "--version" ]]; then
    printf 'codex-cli 0.146.0\n'
    return 0
  fi
  return 99
}
export -f codex
exec bash "$1""#,
        )
        .arg("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", &path)
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("CODEX_EXECUTION_MARKER", &execution_marker)
        .env("HERMES_MCP_DRIVER", &hermes_mcp_driver)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "PATH binary version mismatch must fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "client=codex expected_version=0.146.0 actual_version=0.144.1 status=version_mismatch"
        ),
        "version check must use the PATH binary\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !execution_marker.exists(),
        "a mismatched PATH binary must not execute a smoke task"
    );

    fs::remove_file(fake_bin.join("codex")).unwrap();
    let missing = Command::new("bash")
        .arg("-c")
        .arg("codex() { printf 'codex-cli 0.146.0\\n'; }; export -f codex; exec bash \"$1\"")
        .arg("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("client=codex status=missing_command"),
        "an exported function must not satisfy the PATH executable requirement\nstderr:\n{}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[test]
fn installed_client_smoke_force_kills_hung_clients_and_cleans_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let smoke_tmp = temp.path().join("tmp");
    fs::create_dir(&fake_bin).unwrap();
    fs::create_dir(&smoke_tmp).unwrap();
    write_hermes_python_launcher(&fake_bin);
    let hermes_pythonpath = write_fake_mcp_package(temp.path());
    let hermes_mcp_driver = temp.path().join("hermes-mcp-driver.mjs");
    let pid_file = temp.path().join("hermes-driver-pids");
    write_hermes_mcp_driver(&hermes_mcp_driver);

    let fake_client = r#"#!/usr/bin/env bash
set -euo pipefail
client="$(basename "$0")"
if [[ "${1:-}" == "--version" ]]; then
  case "$client" in
    codex) printf 'codex-cli 0.146.0\n' ;;
    opencode) printf '1.17.18\n' ;;
    claude) printf '2.1.195 (Claude Code)\n' ;;
    hermes) printf 'Hermes Agent v0.14.0\n' ;;
  esac
  exit 0
fi
args=" $* "
if [[ "$client" == "codex" && "$args" == *" login "* ]]; then
  exit 0
elif [[ "$client" == "codex" && "$args" == *"Delegate exactly one"* ]]; then
  marker="$(<probe.txt)"
  printf '{"type":"turn.started"}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
  jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
  printf '{"type":"turn.completed"}\n'
elif [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
elif [[ "$client" == "hermes" ]]; then
  exec node "$HERMES_MCP_DRIVER"
else
  cat probe.txt
fi
"#;
    for client in ["codex", "opencode", "claude", "hermes"] {
        write_executable(&fake_bin.join(client), fake_client);
    }
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let started = Instant::now();
    let output = Command::new("timeout")
        .args([
            "--kill-after=2",
            "7",
            "bash",
            "scripts/installed_client_smoke.sh",
        ])
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("TMPDIR", &smoke_tmp)
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("HERMES_PYTHONPATH", &hermes_pythonpath)
        .env("HERMES_MCP_DRIVER", &hermes_mcp_driver)
        .env("HERMES_DRIVER_HANG", "1")
        .env("HERMES_DRIVER_PID_FILE", &pid_file)
        .env("CLIENT_TIMEOUT_SECONDS", "1")
        .output()
        .unwrap();
    let elapsed = started.elapsed();

    assert!(!output.status.success(), "a timed-out smoke task must fail");
    assert!(
        elapsed < Duration::from_secs(6),
        "the smoke script did not force-kill the TERM-resistant client: {elapsed:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("client=hermes task=read_only_tool_task exit=137"),
        "timeout exit reporting was lost\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        smoke_tmp.read_dir().unwrap().next().is_none(),
        "the smoke WORKDIR was not removed after timeout"
    );

    let pids = fs::read_to_string(&pid_file).expect("hung driver records driver and MCP PIDs");
    for pid in pids.lines() {
        let process_path = std::path::Path::new("/proc").join(pid);
        for _ in 0..20 {
            if !process_path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            !process_path.exists(),
            "timed-out Hermes/MCP process {pid} survived cleanup"
        );
    }
}

#[test]
fn installed_client_smoke_keeps_fake_clients_inside_the_workspace() {
    let script = fs::read_to_string("scripts/installed_client_smoke.sh").unwrap();
    assert!(script.contains(r#""*": "deny""#));
    assert!(script.contains(r#"read: "allow""#));
    assert!(script.contains("record_case hermes read_only_tool_task"));
    assert!(
        !script.contains("client=hermes task=read_only_tool_task status=protocol_matrix_covered")
    );
    for forbidden in [
        "--dangerously-skip-permissions",
        "--ignore-rules",
        "--toolsets terminal",
        "--toolsets=terminal",
    ] {
        assert!(
            !script.contains(forbidden),
            "smoke script contains unsafe client argument: {forbidden}"
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    write_hermes_python_launcher(&fake_bin);
    let hermes_pythonpath = write_fake_mcp_package(temp.path());
    let capture = temp.path().join("client-args.txt");
    let outside_sentinel = temp.path().join("outside-workspace-sentinel");
    let hermes_mcp_driver = temp.path().join("hermes-mcp-driver.mjs");
    write_hermes_mcp_driver(&hermes_mcp_driver);

    let fake_client = r#"#!/usr/bin/env bash
set -euo pipefail
client="$(basename "$0")"
if [[ "${1:-}" == "--version" ]]; then
  case "$client" in
    codex) printf 'codex-cli 0.146.0\n' ;;
    opencode) printf '1.17.18\n' ;;
    claude) printf '2.1.195 (Claude Code)\n' ;;
    hermes) printf 'Hermes Agent v0.14.0\n' ;;
  esac
  exit 0
fi
printf '%s' "$client" >>"$CAPTURE_FILE"
printf '\t%q' "$@" >>"$CAPTURE_FILE"
printf '\n' >>"$CAPTURE_FILE"
args=" $* "
if [[ "$client" == "codex" && "$args" == *" login "* ]]; then
  exit 0
elif [[ "$client" == "codex" && "$args" == *"Delegate exactly one"* ]]; then
  marker="$(<probe.txt)"
  printf '{"type":"turn.started"}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"spawn_agent","status":"completed"}}\n'
  printf '{"type":"item.completed","item":{"type":"collab_tool_call","tool":"wait","status":"completed"}}\n'
  jq -nc --arg marker "$marker" '{type:"item.completed",item:{type:"agent_message",text:$marker}}'
  printf '{"type":"turn.completed"}\n'
fi
if [[ "$args" == *' --dangerously-skip-permissions '* \
   || "$args" == *' --ignore-rules '* \
   || "$args" == *' --toolsets terminal '* \
   || "$args" == *' --toolsets=terminal '* ]]; then
  printf 'unsafe invocation\n' >"$OUTSIDE_SENTINEL"
fi
if [[ "$args" == *CLIENT_TEXT_SMOKE_OK* ]]; then
  printf 'CLIENT_TEXT_SMOKE_OK\n'
elif [[ "$client" == "hermes" ]]; then
  node "$HERMES_MCP_DRIVER"
elif [[ -f probe.txt ]]; then
  cat probe.txt
else
  printf 'missing probe\n' >&2
  exit 1
fi
"#;
    for client in ["codex", "opencode", "claude", "hermes"] {
        write_executable(&fake_bin.join(client), fake_client);
    }
    write_executable(
        &fake_bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '{\"models\":[{\"slug\":\"opaque/exposed-slug\",\"default_reasoning_level\":\"none\",\"multi_agent_version\":\"v1\"}]}\\n'\n",
    );

    let inherited_path = std::env::var("PATH").unwrap();
    let output = Command::new("bash")
        .arg("scripts/installed_client_smoke.sh")
        .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
        .env("BASE_URL", "https://gateway.invalid")
        .env("DOWNSTREAM_KEY", "sentinel-downstream-key")
        .env("MODEL_SLUG", "opaque/exposed-slug")
        .env("HERMES_PYTHONPATH", &hermes_pythonpath)
        .env("CAPTURE_FILE", &capture)
        .env("OUTSIDE_SENTINEL", &outside_sentinel)
        .env("HERMES_MCP_DRIVER", &hermes_mcp_driver)
        .env("CLIENT_TIMEOUT_SECONDS", "5")
        .env("EXPECTED_CODEX_VERSION", "0.146.0")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !outside_sentinel.exists(),
        "unsafe client arguments allowed a fake client to escape its workspace"
    );
    let captured = fs::read_to_string(capture).unwrap();
    for forbidden in [
        "--dangerously-skip-permissions",
        "--ignore-rules",
        "--toolsets terminal",
        "--toolsets=terminal",
    ] {
        assert!(
            !captured.contains(forbidden),
            "captured unsafe client argument: {forbidden}\n{captured}"
        );
    }
}
