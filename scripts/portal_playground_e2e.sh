#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
ADMIN_USER="${ADMIN_USER:-admin}"
DOWNSTREAM_ID="${DOWNSTREAM_ID:-test}"
TIMEOUT_SEC="${TIMEOUT_SEC:-60}"
DOTENV_PATH="${DOTENV_PATH:-${HOME}/docker/chat-responses-codex/.env}"

action_status=0

need_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "[FAIL] missing dependency: $cmd"
    return 1
  fi
}

print_section() {
  echo "================ $1 ================"
}

log_info() {
  echo "[INFO] $*"
}

log_pass() {
  echo "[PASS] $*"
}

log_warn() {
  echo "[WARN] $*"
}

log_fail() {
  echo "[FAIL] $*"
  action_status=1
}

require_or_exit() {
  if ! "$@"; then
    action_status=1
    return 1
  fi
}

now_ms() {
  date +%s%3N
}

ADMIN_PASSWORD=$(awk -F= '$1=="ADMIN_PASSWORD" {print $2}' "$DOTENV_PATH")
if [[ -z "${ADMIN_PASSWORD:-}" ]]; then
  echo "[FAIL] 未在 $DOTENV_PATH 读取到 ADMIN_PASSWORD"
  exit 1
fi

need_cmd curl || exit 1
need_cmd jq || exit 1

healthz() {
  local body_file
  body_file=$(mktemp)
  local code
  code=$(curl -ks -m 30 -o "$body_file" -w '%{http_code}' "$BASE_URL/healthz")
  if [[ "$code" == "200" ]]; then
    log_pass "HEALTHZ 200 body=$(cat "$body_file")"
  else
    log_fail "HEALTHZ status=$code"
  fi
  rm -f "$body_file"
}

admin_login() {
  local body_file
  body_file=$(mktemp)
  local code
  code=$(curl -ks -m 30 -o "$body_file" -w '%{http_code}' \
    -X POST "$BASE_URL/api/admin/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASSWORD\"}")
  if [[ "$code" != "200" ]]; then
    log_fail "ADMIN_LOGIN status=$code body=$(cat \"$body_file\")"
    rm -f "$body_file"
    return 1
  fi

  ADMIN_TOKEN=$(jq -r '.token // empty' "$body_file")
  rm -f "$body_file"
  if [[ -z "$ADMIN_TOKEN" || "$ADMIN_TOKEN" == "null" ]]; then
    log_fail "ADMIN_LOGIN success but token missing"
    return 1
  fi

  log_pass "ADMIN_LOGIN 200"
}

rotate_downstream_key() {
  local body_file
  body_file=$(mktemp)
  local code
  code=$(curl -ks -m 30 -o "$body_file" -w '%{http_code}' \
    -X POST "$BASE_URL/api/admin/downstreams/$DOWNSTREAM_ID/rotate" \
    -H "Authorization: Bearer $ADMIN_TOKEN")
  if [[ "$code" != "200" && "$code" != "201" ]]; then
    log_fail "PORTAL_KEY_ROTATE status=$code body=$(cat \"$body_file\")"
    rm -f "$body_file"
    return 1
  fi

  PORTAL_KEY=$(jq -r '.plaintext_key // empty' "$body_file")
  rm -f "$body_file"
  if [[ -z "$PORTAL_KEY" || "$PORTAL_KEY" == "null" ]]; then
    log_fail "PORTAL_KEY_ROTATE success but plaintext_key missing"
    return 1
  fi
  log_pass "PORTAL_KEY_ROTATE ${code}"
}

portal_login() {
  local body_file
  body_file=$(mktemp)
  local code
  code=$(curl -ks -m 30 -o "$body_file" -w '%{http_code}' \
    -X POST "$BASE_URL/api/portal/login" \
    -H 'Content-Type: application/json' \
    -d "{\"employee_id\":\"$DOWNSTREAM_ID\",\"key\":\"$PORTAL_KEY\"}")
  if [[ "$code" != "200" ]]; then
    log_fail "PORTAL_LOGIN status=$code body=$(cat \"$body_file\")"
    rm -f "$body_file"
    return 1
  fi
  PORTAL_TOKEN=$(jq -r '.token // empty' "$body_file")
  rm -f "$body_file"
  if [[ -z "$PORTAL_TOKEN" || "$PORTAL_TOKEN" == "null" ]]; then
    log_fail "PORTAL_LOGIN success but token missing"
    return 1
  fi
  log_pass "PORTAL_LOGIN 200"
}

portal_key() {
  local body_file
  body_file=$(mktemp)
  local code
  code=$(curl -ks -m 30 -o "$body_file" -w '%{http_code}' \
    -H "Authorization: Bearer $PORTAL_TOKEN" \
    "$BASE_URL/api/portal/key")
  if [[ "$code" != "200" ]]; then
    log_fail "PORTAL_KEY status=$code body=$(cat \"$body_file\")"
    rm -f "$body_file"
    return 1
  fi

  PORTAL_DOWNSTREAM_KEY=$(jq -r '.plaintext_key // empty' "$body_file")
  rm -f "$body_file"
  if [[ -z "$PORTAL_DOWNSTREAM_KEY" || "$PORTAL_DOWNSTREAM_KEY" == "null" ]]; then
    log_fail "PORTAL_KEY success but plaintext_key missing"
    return 1
  fi
  log_pass "PORTAL_KEY 200"
}

gateway_models() {
  local body_file
  body_file=$(mktemp)
  local code
  code=$(curl -ks -m 60 -o "$body_file" -w '%{http_code}' \
    -H "Authorization: Bearer $PORTAL_DOWNSTREAM_KEY" \
    "$BASE_URL/v1/models")
  if [[ "$code" != "200" ]]; then
    log_fail "GATEWAY_MODELS status=$code body=$(cat \"$body_file\")"
    rm -f "$body_file"
    return 1
  fi

  MODEL_COUNT=$(jq -r '.data | length // 0' "$body_file")
  mapfile -t MODEL_LIST < <(jq -r '.data[].id // empty' "$body_file" | awk 'NF' | head -n 5)
  rm -f "$body_file"
  if [[ "$MODEL_COUNT" == "0" ]]; then
    log_fail "GATEWAY_MODELS no models"
    return 1
  fi
  log_pass "GATEWAY_MODELS 200 count=$MODEL_COUNT"
}

chat_completion() {
  local candidate=()
  local extra_default
  extra_default=(
    "grok-4.20-fast"
    "deepseek-ai/DeepSeek-V4-Pro"
    "deepseek-chat"
    "GLM-5"
    "Qwen3.7-Plus"
  )

  if [[ "${#MODEL_LIST[@]}" -gt 0 ]]; then
    candidate+=("${MODEL_LIST[@]}")
  fi

  for item in "${extra_default[@]}"; do
    local found=0
    for existing in "${candidate[@]}"; do
      if [[ "$existing" == "$item" ]]; then
        found=1
        break
      fi
    done
    if [[ "$found" -eq 0 ]]; then
      candidate+=("$item")
    fi
  done

  local payload
  local code
  local body_file
  local assistant
  local error_body
  local model

  for model in "${candidate[@]}"; do
    payload=$(jq -nc --arg model "$model" '{model:$model,messages:[{role:"user",content:"请返回字符串 \"ok\""}],stream:false}')
    body_file=$(mktemp)
    local start end elapsed
    start=$(now_ms)
    code=$(curl -ks -m "$TIMEOUT_SEC" -o "$body_file" -w '%{http_code}' \
      -X POST "$BASE_URL/v1/chat/completions" \
      -H "Authorization: Bearer $PORTAL_DOWNSTREAM_KEY" \
      -H 'Content-Type: application/json' \
      -d "$payload")
    end=$(now_ms)
    elapsed=$((end - start))

    if [[ "$code" == "200" ]]; then
      assistant=$(jq -r '.choices[0].message.content // empty' "$body_file")
      log_pass "CHAT_COMPLETION status=200 model=$model elapsed=${elapsed}ms response_prefix=$(echo "$assistant" | head -c 30)"
      rm -f "$body_file"
      return 0
    fi

    error_body=$(cat "$body_file" | tr '\n' ' ' | head -c 120)
    log_warn "CHAT_COMPLETION model=$model status=$code elapsed=${elapsed}ms error=$error_body"
    rm -f "$body_file"
  done

  log_fail "CHAT_COMPLETION all candidates failed"
  return 1
}

main() {
  print_section "Portal Playground E2E"
  healthz
  require_or_exit admin_login
  require_or_exit rotate_downstream_key
  require_or_exit portal_login
  require_or_exit portal_key
  require_or_exit gateway_models
  require_or_exit chat_completion

  if [[ "$action_status" -eq 0 ]]; then
    log_pass "ENDPOINT_SMOKE_TESTS completed"
  else
    log_fail "ENDPOINT_SMOKE_TESTS failed"
  fi

  return "$action_status"
}

main "$@"
exit "$action_status"
