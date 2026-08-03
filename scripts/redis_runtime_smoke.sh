#!/usr/bin/env bash
set -euo pipefail
set +x

umask 077

GATEWAY_IMAGE="${GATEWAY_IMAGE:-chat-responses-codex:latest}"
REDIS_IMAGE="${REDIS_IMAGE:-redis:7-alpine}"
MOCK_IMAGE="${MOCK_IMAGE:-python:3.12-alpine}"
GATEWAY_A_PORT="${GATEWAY_A_PORT:-3301}"
GATEWAY_B_PORT="${GATEWAY_B_PORT:-3302}"
HOLD_SECONDS="${HOLD_SECONDS:-8}"
AUTHORIZED_CAPACITY_REQUESTS="${AUTHORIZED_CAPACITY_REQUESTS:-}"

if [[ -n "$AUTHORIZED_CAPACITY_REQUESTS" ]] &&
  ! [[ "$AUTHORIZED_CAPACITY_REQUESTS" =~ ^[1-9][0-9]*$ ]]; then
  printf '[FAIL] AUTHORIZED_CAPACITY_REQUESTS must be a positive integer\n' >&2
  exit 1
fi
DOWNSTREAM_MAX_CONCURRENCY=1
if [[ -n "$AUTHORIZED_CAPACITY_REQUESTS" ]]; then
  DOWNSTREAM_MAX_CONCURRENCY=$((AUTHORIZED_CAPACITY_REQUESTS + 1))
fi

SMOKE_NONCE="$(openssl rand -hex 8)"
SMOKE_PREFIX="chat2responses-redis-smoke-${SMOKE_NONCE}"
NETWORK_NAME="${SMOKE_PREFIX}-network"
REDIS_CONTAINER="${SMOKE_PREFIX}-redis"
MOCK_CONTAINER="${SMOKE_PREFIX}-mock-upstream"
GATEWAY_A_CONTAINER="${SMOKE_PREFIX}-gateway-a"
GATEWAY_B_CONTAINER="${SMOKE_PREFIX}-gateway-b"

ADMIN_PASSWORD="$(openssl rand -hex 24)"
JWT_SECRET="$(openssl rand -hex 32)"
UPSTREAM_KEY="$(openssl rand -hex 24)"

WORKDIR="$(mktemp -d)"
MOCK_DIR="${WORKDIR}/mock"
STATE_DIR="${WORKDIR}/state-a"
STATE_SNAPSHOT="${WORKDIR}/state-b.json"
GATEWAY_ENV="${WORKDIR}/gateway.env"
BACKGROUND_PID=""
GATEWAY_B_WAITER_PID=""

log_info() {
  printf '[INFO] %s\n' "$*" >&2
}

log_pass() {
  printf '[PASS] %s\n' "$*" >&2
}

fail() {
  printf '[FAIL] %s\n' "$*" >&2
  exit 1
}

cleanup() {
  local status=$?
  trap - EXIT

  if [[ -n "$BACKGROUND_PID" ]]; then
    kill "$BACKGROUND_PID" >/dev/null 2>&1 || true
    wait "$BACKGROUND_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "$GATEWAY_B_WAITER_PID" ]]; then
    kill "$GATEWAY_B_WAITER_PID" >/dev/null 2>&1 || true
    wait "$GATEWAY_B_WAITER_PID" >/dev/null 2>&1 || true
  fi

  docker rm -f \
    "$GATEWAY_B_CONTAINER" \
    "$GATEWAY_A_CONTAINER" \
    "$MOCK_CONTAINER" \
    "$REDIS_CONTAINER" >/dev/null 2>&1 || true
  docker network rm "$NETWORK_NAME" >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
  exit "$status"
}

need_cmd() {
  local command_name="$1"
  command -v "$command_name" >/dev/null 2>&1 \
    || fail "missing dependency: $command_name"
}

wait_for_http() {
  local url="$1"
  local label="$2"
  local attempt

  for ((attempt = 1; attempt <= 60; attempt++)); do
    if curl -fsS --max-time 2 "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  fail "$label did not become healthy"
}

wait_for_file() {
  local path="$1"
  local label="$2"
  local attempt

  for ((attempt = 1; attempt <= 100; attempt++)); do
    if [[ -s "$path" ]]; then
      return 0
    fi
    sleep 0.1
  done
  fail "$label was not observed"
}

assert_status() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  [[ "$actual" == "$expected" ]] \
    || fail "$label returned HTTP $actual; expected $expected"
}

for dependency in docker curl jq openssl; do
  need_cmd "$dependency"
done

curl() {
  command curl --connect-timeout 5 --max-time "${SMOKE_CURL_MAX_TIME_SECONDS:-60}" "$@"
}

docker image inspect "$GATEWAY_IMAGE" >/dev/null 2>&1 \
  || fail "gateway image is unavailable: $GATEWAY_IMAGE"

mkdir -p "$MOCK_DIR" "$STATE_DIR"
# WORKDIR remains private; this mount point must be writable by image UID 10001.
chmod 0777 "$STATE_DIR"
cat >"${MOCK_DIR}/server.py" <<'PY'
import json
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


WORKDIR = Path("/work")
HOLD_SECONDS = int(os.environ.get("HOLD_SECONDS", "8"))
COUNTER_LOCK = threading.Lock()


def increment_counter(name: str) -> int:
    path = WORKDIR / name
    with COUNTER_LOCK:
        current = int(path.read_text() or "0") if path.exists() else 0
        current += 1
        path.write_text(str(current))
        return current


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send_json(self, status: int, payload: dict, headers=None) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        if self.path == "/healthz":
            self.send_json(200, {"status": "ok"})
        else:
            self.send_json(404, {"error": {"message": "not found"}})

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        try:
            request = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            request = {}

        message_text = json.dumps(request.get("messages", []))
        if "hold" in message_text:
            increment_counter("hold.hits")
            (WORKDIR / "hold.started").write_text("started")
            time.sleep(HOLD_SECONDS)
            self.send_json(
                200,
                {
                    "id": "chatcmpl-redis-smoke",
                    "object": "chat.completion",
                    "created": int(time.time()),
                    "model": request.get("model", "smoke-model"),
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": "HOLD_OK"},
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2,
                    },
                },
            )
            return

        if "authorized-capacity" in message_text:
            increment_counter("capacity.hits")
            self.send_json(
                429,
                {"error": {"message": "concurrency limit reached"}},
                {"Retry-After": "1"},
            )
            return

        if "cooldown" in message_text:
            increment_counter("cooldown.hits")
            self.send_json(
                429,
                {"error": {"message": "rate limit exceeded"}},
                {"Retry-After": "30"},
            )
            return

        self.send_json(400, {"error": {"message": "unknown smoke request"}})

    def log_message(self, _format: str, *_args) -> None:
        return


ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
PY

cat >"$GATEWAY_ENV" <<EOF
BIND_ADDR=0.0.0.0:3001
STATE_PATH=/data/state.json
DATABASE_URL=
LOG_PATH=/tmp/chat2responses-redis-smoke.log
ADMIN_USERNAME=smoke-admin
ADMIN_PASSWORD=$ADMIN_PASSWORD
JWT_SECRET=$JWT_SECRET
APP_NAME=chat2responses-redis-smoke
REDIS_ENABLED=true
REDIS_URL=redis://redis:6379
REDIS_KEY_PREFIX=$SMOKE_PREFIX
AUTOMATIC_CAPABILITY_PROBES_ENABLED=false
UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED=false
UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=0
UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED=false
UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED=false
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=0
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=1
UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS=60000
UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS=32
UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS=100,200,400,800,1000,2000
UPSTREAM_HEDGE_ENABLED=false
UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS=0
EOF

trap cleanup EXIT

log_info "creating isolated Docker resources"
docker network create "$NETWORK_NAME" >/dev/null

docker run -d \
  --name "$REDIS_CONTAINER" \
  --network "$NETWORK_NAME" \
  --network-alias redis \
  "$REDIS_IMAGE" >/dev/null

docker run -d \
  --name "$MOCK_CONTAINER" \
  --network "$NETWORK_NAME" \
  --network-alias mock-upstream \
  -e HOLD_SECONDS="$HOLD_SECONDS" \
  -v "$MOCK_DIR:/work" \
  "$MOCK_IMAGE" \
  python /work/server.py >/dev/null

for ((attempt = 1; attempt <= 60; attempt++)); do
  if docker exec "$REDIS_CONTAINER" redis-cli ping 2>/dev/null | grep -qx PONG; then
    break
  fi
  [[ "$attempt" -lt 60 ]] || fail "Redis did not become ready"
  sleep 1
done

for ((attempt = 1; attempt <= 60; attempt++)); do
  if docker exec "$MOCK_CONTAINER" python -c \
    'import urllib.request; urllib.request.urlopen("http://127.0.0.1:8080/healthz", timeout=1)' \
    >/dev/null 2>&1; then
    break
  fi
  [[ "$attempt" -lt 60 ]] || fail "mock upstream did not become ready"
  sleep 1
done

docker run -d \
  --name "$GATEWAY_A_CONTAINER" \
  --network "$NETWORK_NAME" \
  --network-alias gateway-a \
  --env-file "$GATEWAY_ENV" \
  -p "127.0.0.1:${GATEWAY_A_PORT}:3001" \
  -v "$STATE_DIR:/data" \
  "$GATEWAY_IMAGE" >/dev/null

wait_for_http "http://127.0.0.1:${GATEWAY_A_PORT}/healthz" "gateway A"

LOGIN_PAYLOAD="$(jq -nc \
  --arg username smoke-admin \
  --arg password "$ADMIN_PASSWORD" \
  '{username: $username, password: $password}')"
ADMIN_TOKEN="$(curl -fsS \
  "http://127.0.0.1:${GATEWAY_A_PORT}/api/admin/login" \
  -H 'Content-Type: application/json' \
  --data-binary "$LOGIN_PAYLOAD" \
  | jq -er '.token')"

UPSTREAM_PAYLOAD="$(jq -nc \
  --arg api_key "$UPSTREAM_KEY" \
  '{
    id: "smoke-upstream",
    name: "Redis smoke upstream",
    base_url: "http://mock-upstream:8080",
    api_key: $api_key,
    api_keys: [],
    api_key_models: [{api_key: $api_key, supported_models: ["smoke-model"]}],
    protocol: "ChatCompletions",
    protocols: ["ChatCompletions"],
    supported_models: ["smoke-model"],
    requests_per_minute: 1000,
    request_quota_window_hours: 24,
    request_quota_requests: 1000,
    max_concurrency: 10,
    active: true
  }')"
UPSTREAM_STATUS="$(curl -sS \
  -o "${WORKDIR}/upstream-create.json" \
  -w '%{http_code}' \
  -X POST "http://127.0.0.1:${GATEWAY_A_PORT}/api/admin/upstreams" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  --data-binary "$UPSTREAM_PAYLOAD")"
assert_status 201 "$UPSTREAM_STATUS" "upstream creation"

DOWNSTREAM_PAYLOAD="$(jq -nc \
  --argjson max_concurrency "$DOWNSTREAM_MAX_CONCURRENCY" \
  '{
    id: "smoke-downstream",
    name: "Redis smoke downstream",
    model_allowlist: ["smoke-model"],
    rate_limit_enabled: true,
    per_minute_limit: 1000,
    max_concurrency: $max_concurrency,
    active: true
  }')"
DOWNSTREAM_STATUS="$(curl -sS \
  -o "${WORKDIR}/downstream-create.json" \
  -w '%{http_code}' \
  -X POST "http://127.0.0.1:${GATEWAY_A_PORT}/api/admin/downstreams" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  --data-binary "$DOWNSTREAM_PAYLOAD")"
assert_status 201 "$DOWNSTREAM_STATUS" "downstream creation"
DOWNSTREAM_KEY="$(jq -er '.plaintext_key' "${WORKDIR}/downstream-create.json")"

docker exec "$GATEWAY_A_CONTAINER" test -s /data/state.json \
  || fail "gateway A did not persist file-backed state"
cp "${STATE_DIR}/state.json" "$STATE_SNAPSHOT"
chmod 0444 "$STATE_SNAPSHOT"

docker run -d \
  --name "$GATEWAY_B_CONTAINER" \
  --network "$NETWORK_NAME" \
  --network-alias gateway-b \
  --env-file "$GATEWAY_ENV" \
  -p "127.0.0.1:${GATEWAY_B_PORT}:3001" \
  -v "$STATE_SNAPSHOT:/data/state.json:ro" \
  "$GATEWAY_IMAGE" >/dev/null

wait_for_http "http://127.0.0.1:${GATEWAY_B_PORT}/healthz" "gateway B"
GATEWAY_B_ADMIN_TOKEN="$(curl -fsS \
  "http://127.0.0.1:${GATEWAY_B_PORT}/api/admin/login" \
  -H 'Content-Type: application/json' \
  --data-binary "$LOGIN_PAYLOAD" \
  | jq -er '.token')"

HOLD_PAYLOAD='{"model":"smoke-model","messages":[{"role":"user","content":"hold"}]}'
curl -sS --max-time "$((HOLD_SECONDS + 20))" \
  -o "${WORKDIR}/gateway-a-hold.json" \
  -w '%{http_code}' \
  -X POST "http://127.0.0.1:${GATEWAY_A_PORT}/v1/chat/completions" \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" \
  -H 'Content-Type: application/json' \
  --data-binary "$HOLD_PAYLOAD" \
  >"${WORKDIR}/gateway-a-hold.status" &
BACKGROUND_PID=$!

wait_for_file "${MOCK_DIR}/hold.started" "gateway A upstream hold"

CAPACITY_PIDS=()
if [[ -n "$AUTHORIZED_CAPACITY_REQUESTS" ]]; then
  CAPACITY_PAYLOAD='{"model":"smoke-model","messages":[{"role":"user","content":"authorized-capacity"}]}'
  for ((request = 1; request <= AUTHORIZED_CAPACITY_REQUESTS; request++)); do
    curl -sS --max-time "$((HOLD_SECONDS + 20))" \
      -o "${WORKDIR}/capacity-${request}.json" \
      -w '%{http_code}' \
      -X POST "http://127.0.0.1:${GATEWAY_B_PORT}/v1/chat/completions" \
      -H "Authorization: Bearer $DOWNSTREAM_KEY" \
      -H 'Content-Type: application/json' \
      --data-binary "$CAPACITY_PAYLOAD" \
      >"${WORKDIR}/capacity-${request}.status" &
    CAPACITY_PIDS+=("$!")
  done
fi

curl -sS --max-time "$((HOLD_SECONDS + 20))" \
  -o "${WORKDIR}/gateway-b-hold.json" \
  -w '%{http_code}' \
  -X POST "http://127.0.0.1:${GATEWAY_B_PORT}/v1/chat/completions" \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" \
  -H 'Content-Type: application/json' \
  --data-binary "$HOLD_PAYLOAD" \
  >"${WORKDIR}/gateway-b-hold.status" &
GATEWAY_B_WAITER_PID=$!

RUNTIME_READY=0
for ((attempt = 1; attempt <= 30; attempt++)); do
  if curl -fsS --max-time 5 \
    "http://127.0.0.1:${GATEWAY_B_PORT}/api/admin/downstreams/runtime" \
    -H "Authorization: Bearer $GATEWAY_B_ADMIN_TOKEN" \
    >"${WORKDIR}/gateway-b-downstream-runtime.json" &&
    jq -e '
      .items[]
      | select(.downstream_id == "smoke-downstream")
      | .concurrency.available == true
        and (.concurrency.admitted >= 1)
        and (.concurrency.waiting_upstream >= 1)
    ' "${WORKDIR}/gateway-b-downstream-runtime.json" >/dev/null; then
    RUNTIME_READY=1
    break
  fi
  sleep 0.2
done
[[ "$RUNTIME_READY" -eq 1 ]] || fail "runtime endpoint did not expose an upstream waiter"
log_pass "Redis runtime endpoint exposed admitted and waiting_upstream counts"

for pid in "${CAPACITY_PIDS[@]}"; do
  wait "$pid" || true
done

if [[ -z "$AUTHORIZED_CAPACITY_REQUESTS" ]]; then
wait "$GATEWAY_B_WAITER_PID" || true
GATEWAY_B_WAITER_PID=""
GATEWAY_B_HOLD_STATUS="$(<"${WORKDIR}/gateway-b-hold.status")"
assert_status 429 "$GATEWAY_B_HOLD_STATUS" "gateway B shared concurrency admission"
jq -e '.error.code == "gateway_concurrency_full"' \
  "${WORKDIR}/gateway-b-hold.json" >/dev/null \
  || fail "gateway B did not return gateway_concurrency_full"

wait "$BACKGROUND_PID" || fail "gateway A hold request failed"
BACKGROUND_PID=""
GATEWAY_A_HOLD_STATUS="$(<"${WORKDIR}/gateway-a-hold.status")"
assert_status 200 "$GATEWAY_A_HOLD_STATUS" "gateway A hold request"
log_pass "gateway B enforced gateway A's downstream concurrency lease"
fi

if [[ -n "$AUTHORIZED_CAPACITY_REQUESTS" ]]; then
  wait "$GATEWAY_B_WAITER_PID" || true
  GATEWAY_B_WAITER_PID=""
  wait "$BACKGROUND_PID" || fail "gateway A hold request failed"
  BACKGROUND_PID=""
fi

COOLDOWN_PAYLOAD='{"model":"smoke-model","messages":[{"role":"user","content":"cooldown"}]}'
GATEWAY_A_COOLDOWN_STATUS="$(curl -sS --max-time 10 \
  -o "${WORKDIR}/gateway-a-cooldown.json" \
  -w '%{http_code}' \
  -X POST "http://127.0.0.1:${GATEWAY_A_PORT}/v1/chat/completions" \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" \
  -H 'Content-Type: application/json' \
  --data-binary "$COOLDOWN_PAYLOAD")"
assert_status 429 "$GATEWAY_A_COOLDOWN_STATUS" "gateway A cooldown request"
wait_for_file "${MOCK_DIR}/cooldown.hits" "gateway A route failure"
COOLDOWN_HITS_BEFORE="$(<"${MOCK_DIR}/cooldown.hits")"

GATEWAY_B_RUNTIME_STATUS="$(curl -sS --max-time 5 \
  -o "${WORKDIR}/gateway-b-runtime.json" \
  -w '%{http_code}' \
  "http://127.0.0.1:${GATEWAY_B_PORT}/api/admin/upstreams" \
  -H "Authorization: Bearer $GATEWAY_B_ADMIN_TOKEN")"
assert_status 200 "$GATEWAY_B_RUNTIME_STATUS" "gateway B runtime diagnostics"
jq -e '
  .[]
  | select(.id == "smoke-upstream")
  | (.route_health.cooldown_routes >= 1)
    and ((.route_health.earliest_retry_after_seconds // 0) >= 1)
    and ((.runtime_state.cooldown_remaining // 0) >= 1)
' "${WORKDIR}/gateway-b-runtime.json" >/dev/null \
  || fail "gateway B did not expose gateway A's shared route cooldown"

GATEWAY_B_COOLDOWN_STATUS="$(curl -sS --max-time 10 \
  -o "${WORKDIR}/gateway-b-cooldown.json" \
  -w '%{http_code}' \
  -X POST "http://127.0.0.1:${GATEWAY_B_PORT}/v1/chat/completions" \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" \
  -H 'Content-Type: application/json' \
  --data-binary "$COOLDOWN_PAYLOAD")"
assert_status 429 "$GATEWAY_B_COOLDOWN_STATUS" "gateway B shared route cooldown"
jq -e '
  .error.code == "upstream_routes_exhausted"
  and ((.error.details.retry_after_seconds // 0) >= 1)
' "${WORKDIR}/gateway-b-cooldown.json" >/dev/null \
  || fail "gateway B did not return the shared route cooldown contract"
COOLDOWN_HITS_AFTER="$(<"${MOCK_DIR}/cooldown.hits")"
[[ "$COOLDOWN_HITS_AFTER" == "$COOLDOWN_HITS_BEFORE" ]] \
  || fail "gateway B retried a route already cooled by gateway A"

log_pass "gateway B honored gateway A's route cooldown without hitting upstream"
log_pass "isolated Redis runtime coordination smoke completed"
