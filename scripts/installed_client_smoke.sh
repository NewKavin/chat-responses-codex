#!/usr/bin/env bash
set -euo pipefail
set +x

: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"

if [[ -n "${API_BASE_URL:-}" ]]; then
  : "${API_BASE_URL:?API_BASE_URL is required}"
  API_BASE_URL="${API_BASE_URL%/}"
  if [[ "$API_BASE_URL" != */v1 ]]; then
    API_BASE_URL="${API_BASE_URL}/v1"
  fi
  BASE_URL="${BASE_URL:-${API_BASE_URL%/v1}}"
else
  : "${BASE_URL:?BASE_URL is required}"
  BASE_URL="${BASE_URL%/}"
  API_BASE_URL="${BASE_URL}/v1"
fi

readonly DEFAULT_CODEX_VERSION="0.146.0"
readonly DEFAULT_CLINE_VERSION="0.0.13"
readonly DEFAULT_OPENCODE_VERSION="1.17.18"
readonly DEFAULT_CLAUDE_CODE_VERSION="2.1.195"
readonly DEFAULT_KILO_VERSION="7.4.20"
readonly DEFAULT_HERMES_VERSION="0.14.0"
CLIENTS="${CLIENTS:-}"
CODEX_VERSION="${EXPECTED_CODEX_VERSION:-$DEFAULT_CODEX_VERSION}"
CLINE_VERSION="${EXPECTED_CLINE_VERSION:-$DEFAULT_CLINE_VERSION}"
OPENCODE_VERSION="${EXPECTED_OPENCODE_VERSION:-$DEFAULT_OPENCODE_VERSION}"
CLAUDE_CODE_VERSION="${EXPECTED_CLAUDE_CODE_VERSION:-$DEFAULT_CLAUDE_CODE_VERSION}"
KILO_VERSION="${EXPECTED_KILO_VERSION:-$DEFAULT_KILO_VERSION}"
HERMES_VERSION="${EXPECTED_HERMES_VERSION:-$DEFAULT_HERMES_VERSION}"
readonly CODEX_VERSION CLINE_VERSION OPENCODE_VERSION CLAUDE_CODE_VERSION KILO_VERSION HERMES_VERSION
CLIENT_TIMEOUT_SECONDS="${CLIENT_TIMEOUT_SECONDS:-240}"
readonly CLIENT_KILL_AFTER_SECONDS="2"

umask 077
WORKDIR="$(mktemp -d)"
TASKDIR="$WORKDIR/workspace"
mkdir -p "$TASKDIR"

cleanup() {
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

for command in curl jq timeout readlink; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'client=%s status=missing_command\n' "$command" >&2
    exit 1
  fi
done

if [[ -n "$CLIENTS" ]]; then
  CLIENTS_JSON="$(jq -nc --arg raw "$CLIENTS" \
    '$raw | split(",") | map(gsub("^[[:space:]]+|[[:space:]]+$"; "")) | map(select(length > 0))')"
else
  CLIENTS_JSON="${CLIENTS_JSON:-[\"codex\",\"opencode\",\"claude_code\",\"hermes\"]}"
fi

if ! jq -e '
  type == "array" and length > 0
  and all(.[]; . as $client
    | type == "string"
    and (["codex", "cline", "opencode", "claude_code", "kilo", "hermes"] | index($client) != null))
' <<<"$CLIENTS_JSON" >/dev/null; then
  printf 'status=invalid_clients message=%s\n' 'unknown client in CLIENTS_JSON' >&2
  exit 1
fi

client_enabled() {
  jq -e --arg client "$1" 'index($client) != null' <<<"$CLIENTS_JSON" >/dev/null
}

CODEX_TASKS_JSON='[]'
if client_enabled codex; then
  CODEX_TASKS="${CODEX_TASKS:-text_task,read_only_tool_task,delegation}"
  CODEX_TASKS_JSON="$(jq -nc --arg raw "$CODEX_TASKS" '
    $raw
    | split(",")
    | map(gsub("^[[:space:]]+|[[:space:]]+$"; ""))
    | map(select(length > 0))
  ')"
  if ! jq -e '
    type == "array" and length > 0
    and all(.[]; ["text_task", "read_only_tool_task", "delegation"] | index(.) != null)
  ' <<<"$CODEX_TASKS_JSON" >/dev/null; then
    printf 'client=codex task=selection status=invalid_tasks\n' >&2
    exit 1
  fi
fi

codex_task_enabled() {
  jq -e --arg task "$1" 'index($task) != null' <<<"$CODEX_TASKS_JSON" >/dev/null
}

resolve_client_executable() {
  local client="$1"
  local command="$2"
  local resolved
  resolved="$(type -P "$command" || true)"
  if [[ -z "$resolved" ]]; then
    printf 'client=%s status=missing_command\n' "$client" >&2
    return 1
  fi
  resolved="$(readlink -f -- "$resolved")"
  if [[ ! -x "$resolved" ]]; then
    printf 'client=%s status=missing_command\n' "$client" >&2
    return 1
  fi
  printf '%s' "$resolved"
}

CODEX_BIN=""
CLINE_BIN=""
OPENCODE_BIN=""
CLAUDE_CODE_BIN=""
KILO_BIN=""
HERMES_BIN=""
if client_enabled codex; then
  CODEX_BIN="$(resolve_client_executable codex codex)" || exit 1
fi
if client_enabled cline; then
  CLINE_BIN="$(resolve_client_executable cline clite)" || exit 1
fi
if client_enabled opencode; then
  OPENCODE_BIN="$(resolve_client_executable opencode opencode)" || exit 1
fi
if client_enabled claude_code; then
  CLAUDE_CODE_BIN="$(resolve_client_executable claude_code claude)" || exit 1
fi
if client_enabled kilo; then
  KILO_BIN="$(resolve_client_executable kilo kilo)" || exit 1
fi
if client_enabled hermes; then
  HERMES_BIN="$(resolve_client_executable hermes hermes)" || exit 1
fi
readonly CODEX_BIN CLINE_BIN OPENCODE_BIN CLAUDE_CODE_BIN KILO_BIN HERMES_BIN

version_token() {
  grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n 1
}

verify_version() {
  local client="$1"
  local expected="$2"
  shift 2
  local actual
  actual="$("$@" 2>&1 | version_token)"
  if [[ "$actual" != "$expected" ]]; then
    printf 'client=%s expected_version=%s actual_version=%s status=version_mismatch\n' \
      "$client" "$expected" "${actual:-unknown}" >&2
    return 1
  fi
  printf 'client=%s version=%s status=version_verified\n' "$client" "$actual"
}

resolve_hermes_python() {
  local hermes_dir candidate shebang interpreter
  hermes_dir="${HERMES_BIN%/*}"
  for candidate in "$hermes_dir/python" "$hermes_dir/python3"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  IFS= read -r shebang <"$HERMES_BIN" || true
  if [[ "$shebang" == '#!'* ]]; then
    interpreter="${shebang#\#!}"
    interpreter="${interpreter%%[[:space:]]*}"
    case "${interpreter##*/}" in
      python | python[0-9]* | pypy | pypy[0-9]*)
        if [[ -x "$interpreter" ]]; then
          printf '%s\n' "$interpreter"
          return 0
        fi
        ;;
    esac
  fi
  return 1
}

sanitized_event_types() {
  local output_file="$1"
  local events
  events="$({
    jq -Rr '
      fromjson?
      | [
          .type?,
          .event?.type?,
          .message?.type?,
          .item?.type?,
          (.message?.content?[]?.type?),
          .part?.type?
        ]
      | .[]
      | select(type == "string")
      | . as $event_type
      | select([
          "thread.started",
          "turn.started",
          "turn.completed",
          "turn.failed",
          "item.started",
          "item.updated",
          "item.completed",
          "agent_message",
          "reasoning",
          "command_execution",
          "file_change",
          "mcp_tool_call",
          "collab_tool_call",
          "web_search",
          "todo_list",
          "step_start",
          "text",
          "step_finish",
          "error"
        ] | index($event_type))
    ' "$output_file" 2>/dev/null || true
  } | sort -u | head -n 16 | paste -sd, -)"
  printf '%s' "${events:-final_output}"
}

classify_failure_category() {
  local output_file="$1"
  local has_http_400="false"
  if jq -Rne '
    [inputs | fromjson?] as $events
    | any($events[]?;
        [.status_code?, .status?, .response?.status_code?, .response?.status?]
        | any(.[]?; (if type == "number" then . else tonumber? end) == 400)
      )
  ' "$output_file" >/dev/null 2>&1 \
    || grep -Eiq '^HTTP[[:space:]]+400([[:space:]]|$)' "$output_file"; then
    has_http_400="true"
  fi
  if jq -Rne '
      [inputs | fromjson?] as $events
      | any($events[]?;
          [.status_code?, .status?, .response?.status_code?, .response?.status?]
          | any(.[]?; (if type == "number" then . else tonumber? end) == 401)
        )
    ' "$output_file" >/dev/null 2>&1 \
    || grep -Eiq '^HTTP[[:space:]]+401([[:space:]]|$)' "$output_file"; then
    printf 'authentication'
  elif grep -Eiq 'agent[_ -]?profile|unsupported[^[:alnum:]]+(model|reasoning)|reasoning(_effort)?[^[:alnum:]]+(unsupported|invalid|unknown)' "$output_file"; then
    printf 'agent_profile'
  elif [[ "$has_http_400" == "true" ]] && jq -Rne '
    [inputs | fromjson?] as $events
    | any($events[]?;
        [
          .error?.category?, .error?.code?, .category?, .code?,
          .response?.error?.category?, .response?.error?.code?,
          .item?.error?.category?, .item?.error?.code?
        ]
        | any(.[]?; . == "gateway_protocol_capability_unsupported" or . == "gateway_protocol_semantic_invalid")
      )
  ' "$output_file" >/dev/null 2>&1; then
    printf 'protocol'
  elif jq -Rne '
    [inputs | fromjson?] as $events
    | any($events[]?;
        [.status_code?, .status?, .response?.status_code?, .response?.status?]
        | any(.[]?; (if type == "number" then . else tonumber? end) == 502
          or (if type == "number" then . else tonumber? end) == 503)
      )
  ' "$output_file" >/dev/null 2>&1 \
    || grep -Eiq '^HTTP[[:space:]]+(502|503)([[:space:]]|$)' "$output_file"; then
    printf 'upstream_availability'
  elif jq -Rne '
    [inputs | fromjson?] as $events
    | [range(0; ($events | length)) as $index
      | select(
          ($events[$index]
            | ([.status_code?, .status?, .response?.status_code?, .response?.status?]
              | any(.[]?; (if type == "number" then . else tonumber? end) == 499)))
          and ($events[$index]
            | ([
                .error?.category?, .error?.code?, .category?, .code?,
                .response?.error?.category?, .response?.error?.code?,
                .item?.error?.category?, .item?.error?.code?
              ] | any(.[]?; . == "stream_client_cancelled")))
        )
      | select(any($events[0:$index][]?;
          (.type? // .event?.type?)
            | . == "response.created"
              or . == "response.in_progress"
              or . == "response.output_text.delta"
              or . == "turn.started"
              or . == "item.started"
        ))
    ] | length > 0
  ' "$output_file" >/dev/null 2>&1; then
    printf 'client_cancelled'
  else
    printf 'unknown'
  fi
}

record_case() {
  local client="$1"
  local task="$2"
  local expected_marker="$3"
  local output_file="$4"
  shift 4
  local started finished status duration events category
  started="$(date +%s%3N)"
  set +e
  timeout --kill-after="${CLIENT_KILL_AFTER_SECONDS}s" "$CLIENT_TIMEOUT_SECONDS" "$@" >"$output_file" 2>&1
  status=$?
  set -e
  finished="$(date +%s%3N)"
  duration=$((finished - started))
  events="$(sanitized_event_types "$output_file")"
  category="unknown"
  if [[ "$client" == "codex" ]]; then
    category="$(classify_failure_category "$output_file")"
  fi

  if [[ "$status" -ne 0 ]] || ! grep -Fq "$expected_marker" "$output_file"; then
    printf 'client=%s task=%s exit=%s duration_ms=%s events=%s category=%s status=failed\n' \
      "$client" "$task" "$status" "$duration" "$events" "$category" >&2
    return 1
  fi
  printf 'client=%s task=%s exit=0 duration_ms=%s events=%s status=passed\n' \
    "$client" "$task" "$duration" "$events"
}

record_codex_case() {
  local output_file="$4"
  if ! record_case "$@"; then
    return 1
  fi
  if jq -Rne '[inputs | fromjson?] | any(.[]; .type == "turn.completed")' \
    "$output_file" >/dev/null 2>&1; then
    return 0
  fi
  # Plain-text fake clients used by offline tests have no structured event
  # stream; real --json Codex output is rejected unless it has turn.completed.
  if grep -Eq '^[[:space:]]*(\{|\[)' "$output_file"; then
    printf 'client=codex task=%s status=missing_turn_completed\n' "$2" >&2
    return 1
  fi
}

record_codex_delegation_case() {
  local expected_marker="$3"
  local output_file="$4"
  if ! record_codex_case "$1" "$2" "" "$4" "${@:5}" >/dev/null; then
    return 1
  fi
  local events
  events="$(sanitized_event_types "$output_file")"
  if ! jq -Rne --arg expected_marker "$expected_marker" '
    [inputs | fromjson?] as $events
    | [range(0; $events | length) as $index
        | select(
            $events[$index].type == "item.completed"
            and $events[$index].item.type == "collab_tool_call"
            and $events[$index].item.status == "completed"
          )
        | $index
      ] as $collab_indexes
    | [range(0; $events | length) as $index
        | select(
            $events[$index].type == "item.completed"
            and $events[$index].item.type == "agent_message"
          )
        | $index
      ] as $message_indexes
    | ($collab_indexes | length) == 1
      and ($message_indexes | length) >= 1
      and $collab_indexes[0] < $message_indexes[-1]
      and $events[$message_indexes[-1]].item.text == $expected_marker
      and ([range($message_indexes[-1] + 1; $events | length) as $index
        | select($events[$index].type == "turn.completed")
      ] | length) >= 1
  ' "$output_file" >/dev/null 2>&1; then
    printf 'client=codex task=delegation status=delegation_result_mismatch\n' >&2
    return 1
  fi
  printf 'client=codex task=delegation events=%s status=verified\n' "$events"
}

verify_codex_namespace_case() {
  local output_file="$1"
  local proof_file="$2"
  local expected_marker="$3"
  local call_count tool_name server_proof_status jsonl_status

  call_count="$(wc -l <"$proof_file" | tr -d '[:space:]')"
  tool_name="$(head -n 1 "$proof_file" 2>/dev/null || true)"
  server_proof_status="invalid"
  if [[ "$call_count" == "1" && "$tool_name" == "lookup" ]]; then
    server_proof_status="verified"
  fi

  jsonl_status="invalid"
  if jq -Rne --arg marker "$expected_marker" '
    [inputs | fromjson?] as $events
    | ([$events[]
        | select(
            .type == "item.completed"
            and .item.type == "mcp_tool_call"
            and .item.server == "smoke_namespace"
            and .item.tool == "lookup"
            and .item.status == "completed"
            and any(.item.result.content[]?; .type == "text" and .text == $marker)
          )
      ] | length) == 1
      and ([$events[]
        | select(
            .type == "item.completed"
            and .item.type == "agent_message"
            and .item.text == $marker
          )
      ] | length) >= 1
      and any($events[]; .type == "turn.completed")
  ' "$output_file" >/dev/null 2>&1; then
    jsonl_status="verified"
  fi

  if [[ "$server_proof_status" != "verified" || "$jsonl_status" != "verified" ]]; then
    printf 'client=codex task=namespace_proof calls=%s tool=%s jsonl=%s status=failed\n' \
      "$call_count" "$([[ "$tool_name" == "lookup" ]] && printf 'lookup' || printf 'unexpected')" \
      "$jsonl_status" >&2
    return 1
  fi
  printf 'client=codex task=namespace_proof calls=1 tool=lookup jsonl=verified status=verified\n'
}

if client_enabled codex; then
  verify_version codex "$CODEX_VERSION" "$CODEX_BIN" --version
fi
if client_enabled cline; then
  verify_version cline "$CLINE_VERSION" "$CLINE_BIN" --version
fi
if client_enabled opencode; then
  verify_version opencode "$OPENCODE_VERSION" "$OPENCODE_BIN" --version
fi
if client_enabled claude_code; then
  verify_version claude_code "$CLAUDE_CODE_VERSION" "$CLAUDE_CODE_BIN" --version
fi
if client_enabled kilo; then
  verify_version kilo "$KILO_VERSION" "$KILO_BIN" --version
fi
if client_enabled hermes; then
  verify_version hermes "$HERMES_VERSION" "$HERMES_BIN" --version
fi

HERMES_PYTHON_BIN=""
HERMES_MCP_PYTHONPATH="${HERMES_PYTHONPATH-${PYTHONPATH-}}"
if client_enabled hermes; then
  if ! HERMES_PYTHON_BIN="$(resolve_hermes_python)"; then
    printf 'client=hermes task=mcp_preflight prerequisite=missing_python status=prerequisite_failed\n' >&2
    exit 1
  fi
  if ! env PYTHONPATH="$HERMES_MCP_PYTHONPATH" \
    "$HERMES_PYTHON_BIN" -c 'import mcp' >/dev/null 2>&1; then
    printf 'client=hermes task=mcp_preflight prerequisite=python_mcp_extra status=prerequisite_failed\n' >&2
    exit 1
  fi
  printf 'client=hermes task=mcp_preflight prerequisite=python_mcp_extra status=verified\n'
fi
readonly HERMES_PYTHON_BIN HERMES_MCP_PYTHONPATH

TEXT_MARKER="CLIENT_TEXT_SMOKE_OK"
READ_MARKER="read-only-$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
printf '%s\n' "$READ_MARKER" >"$TASKDIR/probe.txt"
TEXT_PROMPT="Analyze a protocol converter that must preserve unknown JSON fields across request and response translation. In two concise sentences, explain one compatibility risk and one mitigation. End with exactly ${TEXT_MARKER} on its own line."
READ_FILE_PROMPT='Read probe.txt using one available read-only filesystem tool. Reply with exactly the file contents.'
HERMES_READ_PROMPT='Call the mcp_smoke_readonly_lookup tool exactly once. Do not use any other tool. Reply with exactly the returned text.'

if [[ -n "${ATTACHMENT_FILE:-}" ]]; then
  if [[ ! -f "$ATTACHMENT_FILE" ]]; then
    printf 'task=attachment status=missing_file\n' >&2
    exit 1
  fi
  ATTACHMENT_FILE="$(readlink -f "$ATTACHMENT_FILE")"
fi

cd "$TASKDIR"

if client_enabled codex; then
  CODEX_HOME_DIR="$WORKDIR/codex-home"
  mkdir -p "$CODEX_HOME_DIR/agents"
  mkdir -p "$CODEX_HOME_DIR"
  curl -fsS --connect-timeout 5 --max-time 30 \
    "$API_BASE_URL/models?format=codex&client_version=$CODEX_VERSION" \
    -H "Authorization: Bearer $DOWNSTREAM_KEY" >"$CODEX_HOME_DIR/model-catalog.json"
  jq -e '.models | type == "array"' "$CODEX_HOME_DIR/model-catalog.json" >/dev/null
  MODEL_TOML="$(jq -Rn --arg value "$MODEL_SLUG" '$value')"
  if ! MODEL_REASONING_EFFORT="$(jq -er --arg model "$MODEL_SLUG" '
    [ .models[]? | select((.slug // .id // "") == $model) ] as $matches
    | if ($matches | length) != 1 then
        error("selected model is missing from the live catalog")
      elif ($matches[0].default_reasoning_level? | type) != "string" then
        error("selected model has no reasoning metadata")
      else
        $matches[0].default_reasoning_level
      end
  ' "$CODEX_HOME_DIR/model-catalog.json" 2>/dev/null)" \
    || [[ ! "$MODEL_REASONING_EFFORT" =~ ^[A-Za-z0-9_-]+$ ]]; then
    printf 'client=codex task=agent_profile category=agent_profile status=failed\n' >&2
    exit 1
  fi
  MODEL_REASONING_TOML="$(jq -Rn --arg value "$MODEL_REASONING_EFFORT" '$value')"
  API_BASE_TOML="$(jq -Rn --arg value "$API_BASE_URL" '$value')"
  cat >"$CODEX_HOME_DIR/config.toml" <<EOF
model_provider = "gateway"
model = $MODEL_TOML
review_model = $MODEL_TOML
model_reasoning_effort = $MODEL_REASONING_TOML
model_catalog_json = "model-catalog.json"
web_search = "disabled"

[features]
skill_mcp_dependency_install = true
tool_suggest = true
multi_agent = true

[agents]
max_threads = 4
max_depth = 2

[model_providers.gateway]
name = "chat-responses-gateway"
base_url = $API_BASE_TOML
wire_api = "responses"
requires_openai_auth = true
stream_idle_timeout_ms = 3600000
stream_max_retries = 2
EOF
  cat >"$CODEX_HOME_DIR/agents/default.toml" <<EOF
name = "default"
description = "General-purpose read-only exploration subagent."
model = $MODEL_TOML
model_reasoning_effort = $MODEL_REASONING_TOML

[features]
image_generation = false
EOF
  if ! grep -Fq "model = $MODEL_TOML" "$CODEX_HOME_DIR/config.toml" \
    || ! grep -Fq "model = $MODEL_TOML" "$CODEX_HOME_DIR/agents/default.toml" \
    || ! grep -Fq "model_reasoning_effort = $MODEL_REASONING_TOML" "$CODEX_HOME_DIR/config.toml" \
    || ! grep -Fq "model_reasoning_effort = $MODEL_REASONING_TOML" "$CODEX_HOME_DIR/agents/default.toml"; then
    printf 'client=codex task=agent_profile category=agent_profile status=failed\n' >&2
    exit 1
  fi
  if [[ "${CODEX_SKIP_LOGIN:-0}" == "1" ]]; then
    printf 'client=codex task=authentication category=authentication status=skipped\n'
  elif ! printf '%s' "$DOWNSTREAM_KEY" \
    | env CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" login --with-api-key \
      >"$WORKDIR/codex-login.log" 2>&1; then
    LOGIN_CATEGORY="$(classify_failure_category "$WORKDIR/codex-login.log")"
    printf 'client=codex task=authentication category=%s status=failed\n' \
      "$LOGIN_CATEGORY" >&2
    exit 1
  else
    printf 'client=codex task=authentication category=authentication status=verified\n'
  fi

  if codex_task_enabled text_task; then
    record_codex_case codex text_task "$TEXT_MARKER" "$WORKDIR/codex-text.jsonl" \
      env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
      "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
      --cd "$TASKDIR" --model "$MODEL_SLUG" "$TEXT_PROMPT"
  fi
  if codex_task_enabled read_only_tool_task; then
    record_codex_case codex read_only_tool_task "$READ_MARKER" "$WORKDIR/codex-tool.jsonl" \
      env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
      "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
      --cd "$TASKDIR" --model "$MODEL_SLUG" "$READ_FILE_PROMPT"
  fi
  if codex_task_enabled delegation; then
    CODEX_DELEGATION_PROMPT='Delegate exactly one read-only subagent to read probe.txt and return its exact contents. Do not read probe.txt yourself. After the subagent finishes, reply with exactly the subagent result.'
    record_codex_delegation_case codex delegation "$READ_MARKER" "$WORKDIR/codex-delegation.jsonl" \
      env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
      "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
      --cd "$TASKDIR" --model "$MODEL_SLUG" "$CODEX_DELEGATION_PROMPT"
  fi
fi

if client_enabled cline; then
  CLINE_HOME_DIR="$WORKDIR/cline-home"
  CLINE_DATA_DIR="$WORKDIR/cline-data"
  CLINE_CONFIG_DIR="$CLINE_DATA_DIR/settings"
  mkdir -p "$CLINE_HOME_DIR" "$CLINE_CONFIG_DIR"
  CLINE_UPDATED_AT="$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
  jq -nc \
    --arg api_key "$DOWNSTREAM_KEY" \
    --arg base_url "$API_BASE_URL" \
    --arg model "$MODEL_SLUG" \
    --arg updated_at "$CLINE_UPDATED_AT" \
    '{
      version: 1,
      lastUsedProvider: "openai-native",
      providers: {
        "openai-native": {
          settings: {
            provider: "openai-native",
            apiKey: $api_key,
            model: $model,
            baseUrl: $base_url
          },
          updatedAt: $updated_at,
          tokenSource: "manual"
        }
      }
    }' >"$CLINE_CONFIG_DIR/providers.json"
  CLINE_ENV=(
    HOME="$CLINE_HOME_DIR"
    CLINE_DATA_DIR="$CLINE_DATA_DIR"
  )

  record_case cline text_task "$TEXT_MARKER" "$WORKDIR/cline-text.jsonl" \
    env "${CLINE_ENV[@]}" \
    "$CLINE_BIN" --json --plan --auto-approve true --thinking none \
    --cwd "$TASKDIR" --data-dir "$CLINE_DATA_DIR" --config "$CLINE_CONFIG_DIR" \
    --provider openai-native --model "$MODEL_SLUG" "$TEXT_PROMPT"
  record_case cline read_only_tool_task "$READ_MARKER" "$WORKDIR/cline-tool.jsonl" \
    env "${CLINE_ENV[@]}" \
    "$CLINE_BIN" --json --plan --auto-approve true --thinking none \
    --cwd "$TASKDIR" --data-dir "$CLINE_DATA_DIR" --config "$CLINE_CONFIG_DIR" \
    --provider openai-native --model "$MODEL_SLUG" "$READ_FILE_PROMPT"
fi

if client_enabled opencode; then
  OPENCODE_CONFIG_CONTENT="$(jq -nc \
    --arg base_url "$API_BASE_URL" \
    --arg model "$MODEL_SLUG" \
    '{
    "$schema": "https://opencode.ai/config.json",
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
  OPENCODE_ENV=(
    OPENCODE_CONFIG_CONTENT="$OPENCODE_CONFIG_CONTENT"
    OPENCODE_DISABLE_PROJECT_CONFIG=1
    OPENCODE_DISABLE_AUTOUPDATE=1
    XDG_DATA_HOME="$OPENCODE_XDG/data"
    XDG_CONFIG_HOME="$OPENCODE_XDG/config"
    XDG_STATE_HOME="$OPENCODE_XDG/state"
    XDG_CACHE_HOME="$OPENCODE_XDG/cache"
    CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY"
  )

  record_case opencode text_task "$TEXT_MARKER" "$WORKDIR/opencode-text.jsonl" \
    env "${OPENCODE_ENV[@]}" \
    "$OPENCODE_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
    "$TEXT_PROMPT"
  record_case opencode read_only_tool_task "$READ_MARKER" "$WORKDIR/opencode-tool.jsonl" \
    env "${OPENCODE_ENV[@]}" \
    "$OPENCODE_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
    "$READ_FILE_PROMPT"
fi

if client_enabled kilo; then
  KILO_CONFIG_CONTENT="$(jq -nc \
    --arg base_url "$API_BASE_URL" \
    --arg model "$MODEL_SLUG" \
    '{
      "$schema": "https://app.kilo.ai/config.json",
      model: ("gateway/" + $model),
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
  KILO_HOME_DIR="$WORKDIR/kilo-home"
  KILO_XDG="$WORKDIR/kilo-xdg"
  mkdir -p "$KILO_HOME_DIR" "$KILO_XDG"/{data,config,state,cache}
  KILO_ENV=(
    HOME="$KILO_HOME_DIR"
    KILO_CONFIG_CONTENT="$KILO_CONFIG_CONTENT"
    KILO_DISABLE_PROJECT_CONFIG=1
    KILO_DISABLE_AUTOUPDATE=1
    XDG_DATA_HOME="$KILO_XDG/data"
    XDG_CONFIG_HOME="$KILO_XDG/config"
    XDG_STATE_HOME="$KILO_XDG/state"
    XDG_CACHE_HOME="$KILO_XDG/cache"
    CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY"
  )

  record_case kilo text_task "$TEXT_MARKER" "$WORKDIR/kilo-text.jsonl" \
    env "${KILO_ENV[@]}" \
    "$KILO_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
    "$TEXT_PROMPT"
  record_case kilo read_only_tool_task "$READ_MARKER" "$WORKDIR/kilo-tool.jsonl" \
    env "${KILO_ENV[@]}" \
    "$KILO_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
    "$READ_FILE_PROMPT"
fi

if client_enabled claude_code; then
  mkdir -p "$WORKDIR/claude-home"
  CLAUDE_ENV=(
    CLAUDE_CONFIG_DIR="$WORKDIR/claude-home"
    ANTHROPIC_BASE_URL="$BASE_URL"
    ANTHROPIC_API_KEY="$DOWNSTREAM_KEY"
    ANTHROPIC_AUTH_TOKEN="$DOWNSTREAM_KEY"
    ANTHROPIC_DEFAULT_OPUS_MODEL="$MODEL_SLUG"
    ANTHROPIC_DEFAULT_SONNET_MODEL="$MODEL_SLUG"
    ANTHROPIC_DEFAULT_HAIKU_MODEL="$MODEL_SLUG"
  )
  record_case claude_code text_task "$TEXT_MARKER" "$WORKDIR/claude-text.jsonl" \
    env "${CLAUDE_ENV[@]}" "$CLAUDE_CODE_BIN" -p "$TEXT_PROMPT" --bare --verbose \
    --no-session-persistence --output-format stream-json --model "$MODEL_SLUG" --tools ""
  record_case claude_code read_only_tool_task "$READ_MARKER" "$WORKDIR/claude-tool.jsonl" \
    env "${CLAUDE_ENV[@]}" "$CLAUDE_CODE_BIN" -p "$READ_FILE_PROMPT" --bare --verbose \
    --no-session-persistence --output-format stream-json --model "$MODEL_SLUG" \
    --tools Read --allowedTools Read --permission-mode dontAsk
fi

if client_enabled hermes; then
HERMES_HOME_DIR="$WORKDIR/hermes-home"
mkdir -p "$HERMES_HOME_DIR"
if ! command -v node >/dev/null 2>&1; then
  printf 'client=hermes task=read_only_tool_task status=missing_node\n' >&2
  exit 1
fi
HERMES_NODE_COMMAND="$(command -v node)"
HERMES_MCP_SERVER="$WORKDIR/hermes-readonly-server.mjs"
HERMES_MCP_PROOF_FILE="$WORKDIR/hermes-mcp-proof.log"
# Hermes 0.14 exposes read-only MCP tools through config-level allowlisting;
# its single-shot mode bypasses approvals and is intentionally not used here.
cat >"$HERMES_MCP_SERVER" <<'EOF'
import fs from 'node:fs'
import readline from 'node:readline'

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
const send = value => process.stdout.write(`${JSON.stringify(value)}\n`)

for await (const line of lines) {
  let request
  try { request = JSON.parse(line) } catch { continue }
  if (request.id == null) continue
  if (request.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: request.id,
      result: {
        protocolVersion: request.params?.protocolVersion ?? '2025-06-18',
        capabilities: { tools: {} },
        serverInfo: { name: 'smoke-readonly', version: '1.0.0' }
      }
    })
  } else if (request.method === 'tools/list') {
    send({
      jsonrpc: '2.0',
      id: request.id,
      result: {
        tools: [{
          name: 'lookup',
          description: 'Return the read-only smoke value.',
          inputSchema: { type: 'object', properties: {}, additionalProperties: false }
        }]
      }
    })
  } else if (request.method === 'tools/call' && request.params?.name === 'lookup') {
    fs.appendFileSync(process.env.HERMES_MCP_PROOF_FILE, 'lookup\n', { encoding: 'utf8', mode: 0o600 })
    send({
      jsonrpc: '2.0',
      id: request.id,
      result: { content: [{ type: 'text', text: process.env.HERMES_READ_MARKER }] }
    })
  } else if (request.method === 'tools/call') {
    send({
      jsonrpc: '2.0',
      id: request.id,
      error: { code: -32602, message: 'only the read-only lookup tool is available' }
    })
  } else {
    send({ jsonrpc: '2.0', id: request.id, result: {} })
  }
}
EOF
cat >"$HERMES_HOME_DIR/config.yaml" <<'EOF'
model:
  provider: custom
  default: "__MODEL_SLUG__"
  base_url: "__API_BASE_URL__"
  api_key: "${CHAT2RESPONSES_KEY}"
max_turns: 12
EOF
sed -i \
  -e "s|__MODEL_SLUG__|$MODEL_SLUG|g" \
  -e "s|__API_BASE_URL__|$API_BASE_URL|g" \
  "$HERMES_HOME_DIR/config.yaml"
cat >>"$HERMES_HOME_DIR/config.yaml" <<EOF
mcp_servers:
  smoke_readonly:
    command: "$HERMES_NODE_COMMAND"
    args: ["$HERMES_MCP_SERVER"]
    env:
      HERMES_READ_MARKER: "$READ_MARKER"
      HERMES_MCP_PROOF_FILE: "$HERMES_MCP_PROOF_FILE"
    tools:
      include: [lookup]
      resources: false
      prompts: false
EOF

record_case hermes text_task "$TEXT_MARKER" "$WORKDIR/hermes-text.txt" \
  env HERMES_HOME="$HERMES_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
  PYTHONPATH="$HERMES_MCP_PYTHONPATH" \
  "$HERMES_BIN" chat --query "$TEXT_PROMPT" --quiet --model "$MODEL_SLUG" --provider custom --toolsets safe
record_case hermes read_only_tool_task "$READ_MARKER" "$WORKDIR/hermes-tool.txt" \
  env HERMES_HOME="$HERMES_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
  PYTHONPATH="$HERMES_MCP_PYTHONPATH" \
  "$HERMES_BIN" chat --query "$HERMES_READ_PROMPT" --quiet --model "$MODEL_SLUG" \
  --provider custom --toolsets safe,smoke_readonly
HERMES_MCP_CALL_COUNT="$(wc -l <"$HERMES_MCP_PROOF_FILE" 2>/dev/null || printf '0')"
HERMES_MCP_TOOL_NAME="$(head -n 1 "$HERMES_MCP_PROOF_FILE" 2>/dev/null || true)"
if [[ "$HERMES_MCP_CALL_COUNT" != "1" || "$HERMES_MCP_TOOL_NAME" != "lookup" ]]; then
  printf 'client=hermes task=read_only_tool_proof calls=%s tool=%s status=failed\n' \
    "$HERMES_MCP_CALL_COUNT" "${HERMES_MCP_TOOL_NAME:-none}" >&2
  exit 1
fi
printf 'client=hermes task=read_only_tool_proof calls=1 tool=lookup status=verified\n'
fi

if client_enabled codex && [[ "${CODEX_NAMESPACE_TEST:-0}" == "1" ]]; then
  if ! command -v node >/dev/null 2>&1; then
    printf 'client=codex task=namespace_lookup status=missing_node\n' >&2
    exit 1
  fi
  NAMESPACE_MARKER="namespace-$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
  NAMESPACE_MCP_PROOF_FILE="$WORKDIR/codex-namespace-proof.log"
  : >"$NAMESPACE_MCP_PROOF_FILE"
  cat >"$WORKDIR/namespace-server.mjs" <<'EOF'
import fs from 'node:fs'
import readline from 'node:readline'

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
const send = value => process.stdout.write(`${JSON.stringify(value)}\n`)

for await (const line of lines) {
  let request
  try { request = JSON.parse(line) } catch { continue }
  if (request.id == null) continue
  if (request.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: request.id,
      result: {
        protocolVersion: request.params?.protocolVersion ?? '2025-06-18',
        capabilities: { tools: {} },
        serverInfo: { name: 'gateway-namespace-smoke', version: '1.0.0' }
      }
    })
  } else if (request.method === 'tools/list') {
    send({
      jsonrpc: '2.0',
      id: request.id,
      result: {
        tools: [{
          name: 'lookup',
          description: 'Return the namespace smoke value.',
          inputSchema: { type: 'object', properties: {}, additionalProperties: false },
          annotations: {
            readOnlyHint: true,
            destructiveHint: false,
            openWorldHint: false
          }
        }]
      }
    })
  } else if (request.method === 'tools/call') {
    fs.appendFileSync(
      process.env.NAMESPACE_MCP_PROOF_FILE,
      `${String(request.params?.name ?? '')}\n`,
      { encoding: 'utf8', mode: 0o600 }
    )
    send({
      jsonrpc: '2.0',
      id: request.id,
      result: { content: [{ type: 'text', text: process.env.NAMESPACE_MARKER }] }
    })
  } else {
    send({ jsonrpc: '2.0', id: request.id, result: {} })
  }
}
EOF
  NAMESPACE_COMMAND_TOML="$(jq -Rn --arg value "$(command -v node)" '$value')"
  NAMESPACE_SERVER_TOML="$(jq -Rn --arg value "$WORKDIR/namespace-server.mjs" '$value')"
  NAMESPACE_MARKER_TOML="$(jq -Rn --arg value "$NAMESPACE_MARKER" '$value')"
  NAMESPACE_MCP_PROOF_TOML="$(jq -Rn --arg value "$NAMESPACE_MCP_PROOF_FILE" '$value')"
  cat >>"$CODEX_HOME_DIR/config.toml" <<EOF

[mcp_servers.smoke_namespace]
command = $NAMESPACE_COMMAND_TOML
args = [$NAMESPACE_SERVER_TOML]
env = { NAMESPACE_MARKER = $NAMESPACE_MARKER_TOML, NAMESPACE_MCP_PROOF_FILE = $NAMESPACE_MCP_PROOF_TOML }
EOF
  record_case codex namespace_lookup "$NAMESPACE_MARKER" "$WORKDIR/codex-namespace.jsonl" \
    env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
    "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
    --cd "$TASKDIR" --model "$MODEL_SLUG" \
    'Call the lookup member in the mcp__smoke_namespace namespace exactly once. Do not answer from memory. Reply with exactly the returned text.'
  verify_codex_namespace_case \
    "$WORKDIR/codex-namespace.jsonl" "$NAMESPACE_MCP_PROOF_FILE" "$NAMESPACE_MARKER"
fi

if [[ -n "${ATTACHMENT_FILE:-}" ]]; then
  if client_enabled codex; then
    if "$CODEX_BIN" exec --help 2>&1 | grep -q -- '--image'; then
      ATTACHMENT_MARKER="CODEX_ATTACHMENT_SMOKE_OK"
      record_case codex attachment "$ATTACHMENT_MARKER" "$WORKDIR/codex-attachment.jsonl" \
        env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
        "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
        --cd "$TASKDIR" --model "$MODEL_SLUG" --image "$ATTACHMENT_FILE" \
        "Inspect the attached file, then reply with exactly ${ATTACHMENT_MARKER}."
    else
      printf 'client=codex task=attachment status=protocol_matrix_covered\n'
    fi
  fi
  if client_enabled opencode; then
    if "$OPENCODE_BIN" run --help 2>&1 | grep -q -- '--file'; then
      ATTACHMENT_MARKER="OPENCODE_ATTACHMENT_SMOKE_OK"
      record_case opencode attachment "$ATTACHMENT_MARKER" "$WORKDIR/opencode-attachment.jsonl" \
        env "${OPENCODE_ENV[@]}" \
        "$OPENCODE_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
        --file "$ATTACHMENT_FILE" \
        "Inspect the attached file, then reply with exactly ${ATTACHMENT_MARKER}."
    else
      printf 'client=opencode task=attachment status=protocol_matrix_covered\n'
    fi
  fi
  if client_enabled cline; then
    printf 'client=cline task=attachment status=protocol_matrix_covered\n'
  fi
  if client_enabled claude_code; then
    printf 'client=claude_code task=attachment status=protocol_matrix_covered\n'
  fi
  if client_enabled kilo; then
    printf 'client=kilo task=attachment status=protocol_matrix_covered\n'
  fi
  if client_enabled hermes; then
    printf 'client=hermes task=attachment status=protocol_matrix_covered\n'
  fi
else
  for client in codex cline opencode claude_code kilo hermes; do
    if client_enabled "$client"; then
      printf 'client=%s task=attachment status=protocol_matrix_covered\n' "$client"
    fi
  done
fi
