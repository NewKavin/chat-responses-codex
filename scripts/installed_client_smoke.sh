#!/usr/bin/env bash
set -euo pipefail
set +x

: "${BASE_URL:?BASE_URL is required}"
: "${DOWNSTREAM_KEY:?DOWNSTREAM_KEY is required}"
: "${MODEL_SLUG:?MODEL_SLUG is required}"

readonly DEFAULT_CODEX_VERSION="0.144.0"
readonly DEFAULT_OPENCODE_VERSION="1.17.9"
readonly DEFAULT_CLAUDE_CODE_VERSION="2.1.195"
readonly DEFAULT_HERMES_VERSION="0.14.0"
CLIENTS_JSON="${CLIENTS_JSON:-[\"codex\",\"opencode\",\"claude_code\",\"hermes\"]}"
CODEX_VERSION="${EXPECTED_CODEX_VERSION:-$DEFAULT_CODEX_VERSION}"
OPENCODE_VERSION="${EXPECTED_OPENCODE_VERSION:-$DEFAULT_OPENCODE_VERSION}"
CLAUDE_CODE_VERSION="${EXPECTED_CLAUDE_CODE_VERSION:-$DEFAULT_CLAUDE_CODE_VERSION}"
HERMES_VERSION="${EXPECTED_HERMES_VERSION:-$DEFAULT_HERMES_VERSION}"
readonly CODEX_VERSION OPENCODE_VERSION CLAUDE_CODE_VERSION HERMES_VERSION
CLIENT_TIMEOUT_SECONDS="${CLIENT_TIMEOUT_SECONDS:-240}"
readonly CLIENT_KILL_AFTER_SECONDS="2"

BASE_URL="${BASE_URL%/}"
API_BASE_URL="${BASE_URL}/v1"
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
OPENCODE_BIN=""
CLAUDE_CODE_BIN=""
HERMES_BIN=""
if client_enabled codex; then
  CODEX_BIN="$(resolve_client_executable codex codex)" || exit 1
fi
if client_enabled opencode; then
  OPENCODE_BIN="$(resolve_client_executable opencode opencode)" || exit 1
fi
if client_enabled claude_code; then
  CLAUDE_CODE_BIN="$(resolve_client_executable claude_code claude)" || exit 1
fi
if client_enabled hermes; then
  HERMES_BIN="$(resolve_client_executable hermes hermes)" || exit 1
fi
readonly CODEX_BIN OPENCODE_BIN CLAUDE_CODE_BIN HERMES_BIN

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
          (.message?.content?[]?.type?),
          .part?.type?
        ]
      | .[]
      | select(type == "string")
    ' "$output_file" 2>/dev/null || true
  } | grep -E '^[A-Za-z0-9_.:-]+$' | sort -u | paste -sd, -)"
  printf '%s' "${events:-final_output}"
}

record_case() {
  local client="$1"
  local task="$2"
  local expected_marker="$3"
  local output_file="$4"
  shift 4
  local started finished status duration events
  started="$(date +%s%3N)"
  set +e
  timeout --kill-after="${CLIENT_KILL_AFTER_SECONDS}s" "$CLIENT_TIMEOUT_SECONDS" "$@" >"$output_file" 2>&1
  status=$?
  set -e
  finished="$(date +%s%3N)"
  duration=$((finished - started))
  events="$(sanitized_event_types "$output_file")"

  if [[ "$status" -ne 0 ]] || ! grep -Fq "$expected_marker" "$output_file"; then
    printf 'client=%s task=%s exit=%s duration_ms=%s events=%s status=failed\n' \
      "$client" "$task" "$status" "$duration" "$events" >&2
    return 1
  fi
  printf 'client=%s task=%s exit=0 duration_ms=%s events=%s status=passed\n' \
    "$client" "$task" "$duration" "$events"
}

if client_enabled codex; then
  verify_version codex "$CODEX_VERSION" "$CODEX_BIN" --version
fi
if client_enabled opencode; then
  verify_version opencode "$OPENCODE_VERSION" "$OPENCODE_BIN" --version
fi
if client_enabled claude_code; then
  verify_version claude_code "$CLAUDE_CODE_VERSION" "$CLAUDE_CODE_BIN" --version
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
TEXT_PROMPT="Reply with exactly ${TEXT_MARKER}."
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
  mkdir -p "$CODEX_HOME_DIR"
  curl -fsS "$API_BASE_URL/models?client_version=$CODEX_VERSION" \
    -H "Authorization: Bearer $DOWNSTREAM_KEY" >"$CODEX_HOME_DIR/model-catalog.json"
  jq -e '.models | type == "array"' "$CODEX_HOME_DIR/model-catalog.json" >/dev/null
  MODEL_TOML="$(jq -Rn --arg value "$MODEL_SLUG" '$value')"
  API_BASE_TOML="$(jq -Rn --arg value "$API_BASE_URL" '$value')"
  cat >"$CODEX_HOME_DIR/config.toml" <<EOF
model_provider = "gateway"
model = $MODEL_TOML
review_model = $MODEL_TOML
disable_response_storage = true
model_catalog_json = "model-catalog.json"
web_search = "disabled"

[model_providers.gateway]
name = "chat-responses-gateway"
base_url = $API_BASE_TOML
wire_api = "responses"
env_key = "CHAT2RESPONSES_KEY"
EOF

  record_case codex text_task "$TEXT_MARKER" "$WORKDIR/codex-text.jsonl" \
    env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
    "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
    --cd "$TASKDIR" --model "$MODEL_SLUG" "$TEXT_PROMPT"
  record_case codex read_only_tool_task "$READ_MARKER" "$WORKDIR/codex-tool.jsonl" \
    env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
    "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
    --cd "$TASKDIR" --model "$MODEL_SLUG" "$READ_FILE_PROMPT"
fi

if client_enabled opencode; then
  OPENCODE_CONFIG_FILE="$WORKDIR/opencode.json"
  jq -n \
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
  }' >"$OPENCODE_CONFIG_FILE"

  record_case opencode text_task "$TEXT_MARKER" "$WORKDIR/opencode-text.jsonl" \
    env OPENCODE_CONFIG="$OPENCODE_CONFIG_FILE" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
    "$OPENCODE_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
    "$TEXT_PROMPT"
  record_case opencode read_only_tool_task "$READ_MARKER" "$WORKDIR/opencode-tool.jsonl" \
    env OPENCODE_CONFIG="$OPENCODE_CONFIG_FILE" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
    "$OPENCODE_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
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
  cat >"$WORKDIR/namespace-server.mjs" <<'EOF'
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
          inputSchema: { type: 'object', properties: {}, additionalProperties: false }
        }]
      }
    })
  } else if (request.method === 'tools/call') {
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
  cat >>"$CODEX_HOME_DIR/config.toml" <<EOF

[mcp_servers.smoke_namespace]
command = $NAMESPACE_COMMAND_TOML
args = [$NAMESPACE_SERVER_TOML]
env = { NAMESPACE_MARKER = $NAMESPACE_MARKER_TOML }
EOF
  record_case codex namespace_lookup "$NAMESPACE_MARKER" "$WORKDIR/codex-namespace.jsonl" \
    env CODEX_HOME="$CODEX_HOME_DIR" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
    "$CODEX_BIN" exec --json --ephemeral --skip-git-repo-check --sandbox read-only \
    --cd "$TASKDIR" --model "$MODEL_SLUG" \
    'Use the mcp__smoke_namespace__lookup tool exactly once. Reply with exactly the tool result.'
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
        env OPENCODE_CONFIG="$OPENCODE_CONFIG_FILE" CHAT2RESPONSES_KEY="$DOWNSTREAM_KEY" \
        "$OPENCODE_BIN" run --pure --format json --dir "$TASKDIR" --model "gateway/$MODEL_SLUG" \
        --file "$ATTACHMENT_FILE" \
        "Inspect the attached file, then reply with exactly ${ATTACHMENT_MARKER}."
    else
      printf 'client=opencode task=attachment status=protocol_matrix_covered\n'
    fi
  fi
  if client_enabled claude_code; then
    printf 'client=claude_code task=attachment status=protocol_matrix_covered\n'
  fi
  if client_enabled hermes; then
    printf 'client=hermes task=attachment status=protocol_matrix_covered\n'
  fi
else
  for client in codex opencode claude_code hermes; do
    if client_enabled "$client"; then
      printf 'client=%s task=attachment status=protocol_matrix_covered\n' "$client"
    fi
  done
fi
