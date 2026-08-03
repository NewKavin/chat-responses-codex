#!/usr/bin/env bash
set -euo pipefail
set +x

: "${API_BASE_URL:?API_BASE_URL is required}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"
: "${ADMIN_LOG_URL:?ADMIN_LOG_URL is required}"
: "${ADMIN_TOKEN:?ADMIN_TOKEN is required}"

readonly CODEX_BIN="${CODEX_BIN:-codex}"
readonly CODEX_IDLE_TIMEOUT_MS=3600000
readonly CLIENT_KILL_AFTER_SECONDS="${CLIENT_KILL_AFTER_SECONDS:-30}"
readonly TEST_DAY="${TEST_DAY:-$(date +%F)}"
readonly DELAYED_OUTPUT_SECONDS="${DELAYED_OUTPUT_SECONDS:-3600}"
readonly CLIENT_TIMEOUT_SECONDS="${CLIENT_TIMEOUT_SECONDS:-$((DELAYED_OUTPUT_SECONDS + 300))}"
readonly OUTER_TIMEOUT_SECONDS="${OUTER_TIMEOUT_SECONDS:-$((CLIENT_TIMEOUT_SECONDS + 300))}"

for duration in "$DELAYED_OUTPUT_SECONDS" "$CLIENT_TIMEOUT_SECONDS" "$OUTER_TIMEOUT_SECONDS"; do
  [[ "$duration" =~ ^[0-9]+$ ]] || {
    printf 'status=invalid_duration\n' >&2
    exit 1
  }
done
(( OUTER_TIMEOUT_SECONDS > CLIENT_TIMEOUT_SECONDS )) || {
  printf 'status=outer_timeout_must_exceed_client_timeout\n' >&2
  exit 1
}

umask 077
WORKDIR="$(mktemp -d)"
CODEX_HOME_DIR="$WORKDIR/codex-home"
EVENT_LOG="$WORKDIR/events.jsonl"
CATALOG_FILE="$WORKDIR/model-catalog.json"
LOG_RESPONSE="$WORKDIR/logs.json"
mkdir -p "$CODEX_HOME_DIR"

cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "${CODEX_PID:-}" ]]; then
    kill "$CODEX_PID" >/dev/null 2>&1 || true
    wait "$CODEX_PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$WORKDIR"
  exit "$status"
}
trap cleanup EXIT

for command in curl jq timeout date mktemp; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'status=missing_command command=%s\n' "$command" >&2
    exit 1
  }
done
command -v "$CODEX_BIN" >/dev/null 2>&1 || {
  printf 'status=missing_command command=codex\n' >&2
  exit 1
}

API_BASE_URL="${API_BASE_URL%/}"
MODEL_TOML="$(jq -Rn --arg value "$MODEL_SLUG" '$value')"
API_BASE_TOML="$(jq -Rn --arg value "$API_BASE_URL" '$value')"

curl -fsS --connect-timeout 5 --max-time 30 \
  "$API_BASE_URL/models?client_version=0.146.0" \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" >"$CATALOG_FILE"
jq -e '.models | type == "array" and length > 0' "$CATALOG_FILE" >/dev/null

cat >"$CODEX_HOME_DIR/config.toml" <<EOF
model_provider = "gateway"
model = $MODEL_TOML
review_model = $MODEL_TOML
model_catalog_json = "model-catalog.json"
web_search = "disabled"

[model_providers.gateway]
name = "chat-responses-gateway"
base_url = $API_BASE_TOML
wire_api = "responses"
env_key = "CHAT2RESPONSES_KEY"
stream_idle_timeout_ms = 3600000
stream_max_retries = 2
EOF
cp "$CATALOG_FILE" "$CODEX_HOME_DIR/model-catalog.json"

TEXT_PROMPT="Wait at least ${DELAYED_OUTPUT_SECONDS} seconds before producing one short sentence explaining why a gateway must preserve response event ordering. End with the exact marker DELAYED_OUTPUT_SMOKE_OK on its own line."
set +e
timeout --kill-after="${CLIENT_KILL_AFTER_SECONDS}s" "$OUTER_TIMEOUT_SECONDS" \
  timeout --kill-after="${CLIENT_KILL_AFTER_SECONDS}s" "$CLIENT_TIMEOUT_SECONDS" \
  env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
  "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
  --cd "$WORKDIR" --model "$MODEL_SLUG" "$TEXT_PROMPT" >"$EVENT_LOG" 2>&1 &
CODEX_PID=$!
wait "$CODEX_PID"
CODEX_STATUS=$?
set -e
[[ "$CODEX_STATUS" -eq 0 ]] || {
  printf 'status=codex_failed exit=%s\n' "$CODEX_STATUS" >&2
  exit 1
}

jq -e -s 'any(.[]; .type == "turn.completed")' "$EVENT_LOG" >/dev/null || {
  printf 'status=missing_turn_completed\n' >&2
  exit 1
}

curl -fsS --connect-timeout 5 --max-time 30 --get "$ADMIN_LOG_URL" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  --data-urlencode "day=$TEST_DAY" \
  --data-urlencode "model=$MODEL_SLUG" >"$LOG_RESPONSE"

REQUEST_ID="${REQUEST_ID:-$(jq -Rr '
  fromjson?
  | select(.type == "response.created" or .type == "response.completed")
  | (.response.id? // .id? // empty)
' "$EVENT_LOG" | head -n 1)}"
: "${REQUEST_ID:?REQUEST_ID is required; set it to the gateway request id when the client omits one}"
matched_usage_rows="$(jq -r --arg request_id "$REQUEST_ID" \
  '[.logs[] | select(.request_id == $request_id)] | length' "$LOG_RESPONSE")"
[[ "$matched_usage_rows" =~ ^[1-9][0-9]*$ ]] || {
  printf 'status=missing_matching_usage_row\n' >&2
  exit 1
}
jq -e --arg request_id "$REQUEST_ID" \
  '[.logs[] | select(.request_id == $request_id and (.status_code == 499 or .status_code == 502 or .status_code == 503))] | length == 0' \
  "$LOG_RESPONSE" >/dev/null

printf 'status=passed event=turn.completed day=%s\n' "$TEST_DAY"
