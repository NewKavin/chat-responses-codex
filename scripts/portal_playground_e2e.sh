#!/usr/bin/env bash
set -euo pipefail
set +x

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
TIMEOUT_SEC="${TIMEOUT_SEC:-60}"

action_status=0
temp_dir=""

need_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "[FAIL] missing dependency: $cmd" >&2
    return 1
  fi
}

log_info() {
  echo "[INFO] $*" >&2
}

log_pass() {
  echo "[PASS] $*" >&2
}

log_warn() {
  echo "[WARN] $*" >&2
}

log_fail() {
  echo "[FAIL] $*" >&2
  action_status=1
}

now_ms() {
  date +%s%3N
}

cleanup() {
  if [[ -n "$temp_dir" ]]; then
    rm -rf "$temp_dir"
  fi
}

fetch_live_models() {
  local body_file="$temp_dir/models.json"
  local code
  local start
  local end
  local elapsed

  start=$(now_ms)
  code=$(curl -ksS -m "$TIMEOUT_SEC" -o "$body_file" -w '%{http_code}' \
    -H "Authorization: Bearer $DOWNSTREAM_KEY" \
    "$BASE_URL/v1/models")
  end=$(now_ms)
  elapsed=$((end - start))

  if [[ "$code" != "200" ]]; then
    log_fail "model=catalog status=$code duration_ms=$elapsed meaningful_frames=0 error_category=http_$code terminal_present=false"
    return 1
  fi

  if ! jq -e '.data | type == "array"' "$body_file" >/dev/null 2>&1; then
    log_fail "model=catalog status=$code duration_ms=$elapsed meaningful_frames=0 error_category=invalid_catalog terminal_present=false"
    return 1
  fi

  mapfile -t MODEL_LIST < <(
    jq -r '.data[]?.id | select(type == "string")' "$body_file" | awk 'NF && !seen[$0]++'
  )
  if [[ "${#MODEL_LIST[@]}" -eq 0 ]]; then
    log_fail "model=catalog status=$code duration_ms=$elapsed meaningful_frames=0 error_category=empty_catalog terminal_present=false"
    return 1
  fi
}

inspect_stream() {
  local body_file="$1"
  local line
  local data
  local category

  MEANINGFUL_FRAMES=0
  ERROR_CATEGORY="none"
  TERMINAL_PRESENT="false"

  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    [[ "$line" == data:* ]] || continue
    data="${line#data:}"
    data="${data#"${data%%[![:space:]]*}"}"

    if [[ "$data" == "[DONE]" ]]; then
      TERMINAL_PRESENT="true"
      continue
    fi

    category=$(jq -r '.error.category // .category // .error.code // .error.type // empty' \
      <<<"$data" 2>/dev/null || true)
    if [[ -n "$category" ]]; then
      ERROR_CATEGORY="$category"
    fi

    if jq -e '
      ((((.choices // [])[0].delta.content // "") | type == "string" and length > 0) or
       (((.choices // [])[0].delta.reasoning_content // "") | type == "string" and length > 0))
    ' <<<"$data" >/dev/null 2>&1; then
      MEANINGFUL_FRAMES=$((MEANINGFUL_FRAMES + 1))
    fi

    if jq -e '((.choices // [])[0].finish_reason // "") != ""' \
      <<<"$data" >/dev/null 2>&1; then
      TERMINAL_PRESENT="true"
    fi
  done <"$body_file"
}

smoke_model() {
  local model="$1"
  local safe_name="$2"
  local body_file="$temp_dir/chat-$safe_name.sse"
  local payload
  local code
  local start
  local end
  local elapsed

  payload="$(jq -nc --arg model "$model" '{
    model: $model,
    messages: [{role:"user",content:"Reply with exactly PLAYGROUND_OK"}],
    stream: true
  }')"

  start=$(now_ms)
  code=$(curl -ksS -m "$TIMEOUT_SEC" -o "$body_file" -w '%{http_code}' \
    -X POST "$BASE_URL/v1/chat/completions" \
    -H "Authorization: Bearer $DOWNSTREAM_KEY" \
    -H 'Content-Type: application/json' \
    -d "$payload")
  end=$(now_ms)
  elapsed=$((end - start))

  inspect_stream "$body_file"
  if [[ "$code" == "200" && "$MEANINGFUL_FRAMES" -gt 0 && "$TERMINAL_PRESENT" == "true" && "$ERROR_CATEGORY" == "none" ]]; then
    log_pass "model=$model status=$code duration_ms=$elapsed meaningful_frames=$MEANINGFUL_FRAMES error_category=$ERROR_CATEGORY terminal_present=$TERMINAL_PRESENT"
    return 0
  fi

  if [[ "$ERROR_CATEGORY" == "none" ]]; then
    if [[ "$code" != "200" ]]; then
      ERROR_CATEGORY="http_$code"
    elif [[ "$MEANINGFUL_FRAMES" -eq 0 ]]; then
      ERROR_CATEGORY="empty_stream"
    else
      ERROR_CATEGORY="missing_terminal"
    fi
  fi
  log_warn "model=$model status=$code duration_ms=$elapsed meaningful_frames=$MEANINGFUL_FRAMES error_category=$ERROR_CATEGORY terminal_present=$TERMINAL_PRESENT"
  return 1
}

main() {
  need_cmd curl || return 1
  need_cmd jq || return 1
  need_cmd awk || return 1
  need_cmd date || return 1

  temp_dir=$(mktemp -d)
  trap cleanup EXIT

  fetch_live_models || return 1

  local index=0
  local model
  for model in "${MODEL_LIST[@]}"; do
    index=$((index + 1))
    if smoke_model "$model" "$index"; then
      return 0
    fi
  done

  log_fail "model=all-live-models status=failed duration_ms=0 meaningful_frames=0 error_category=no_playable_model terminal_present=false"
  return "$action_status"
}

main "$@"
