#!/usr/bin/env bash
set -euo pipefail
set +x

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
DOWNSTREAM_ID="${DOWNSTREAM_ID:-test}"
CLIENTS_JSON="${CLIENTS_JSON:-[\"codex\",\"opencode\",\"claude_code\",\"hermes\"]}"
ENV_FILE="${ENV_FILE:-$HOME/docker/chat-responses-codex/.env}"
OUTPUT_FILE="/tmp/compatibility-matrix.json"

if [[ ! -f "$ENV_FILE" ]]; then
  echo "ENV_FILE not found: $ENV_FILE" >&2
  exit 1
fi

ADMIN_PASSWORD="$(
  grep '^ADMIN_PASSWORD=' "$ENV_FILE" \
    | head -n 1 \
    | cut -d= -f2- \
    | tr -d '\r'
)"

if [[ -z "$ADMIN_PASSWORD" ]]; then
  echo "ADMIN_PASSWORD missing in $ENV_FILE" >&2
  exit 1
fi

LOGIN_PAYLOAD="$(
  jq -nc \
    --arg username "admin" \
    --arg password "$ADMIN_PASSWORD" \
    '{username: $username, password: $password}'
)"

REQUEST_PAYLOAD="$(
  jq -nc \
    --arg downstream_id "$DOWNSTREAM_ID" \
    --argjson client_profiles "$CLIENTS_JSON" \
    '{downstream_id: $downstream_id, client_profiles: $client_profiles}'
)"

TOKEN="$(
  curl -fsS "$BASE_URL/api/admin/login" \
    -H 'Content-Type: application/json' \
    --data-binary "$LOGIN_PAYLOAD" \
    | jq -r '.token'
)"

if [[ -z "$TOKEN" || "$TOKEN" == "null" ]]; then
  echo "admin login did not return a token" >&2
  exit 1
fi

umask 077
curl -fsS "$BASE_URL/api/admin/troubleshooting/matrix/run" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  --data-binary "$REQUEST_PAYLOAD" \
  >"$OUTPUT_FILE"

jq -e --argjson required_clients "$CLIENTS_JSON" '
  (($required_clients | type) == "array" and ($required_clients | length) > 0)
  and ((.cells | type) == "array" and (.cells | length) > 0)
  and .summary.failed == 0
  and ([
    .cells
    | group_by(.model_slug)[]
    | ([.[].client_family] | unique) as $observed_clients
    | ($required_clients
       | all(. as $client | ($observed_clients | index($client)) != null))
  ] | all)
  and (all(.cells[];
    ((.check_results | type) == "array" and (.check_results | length) > 0)
    and all(.check_results[]; .passed == true)
    and (
      ((.optional_downgrades // []) | length) == 0
      or any(.check_results[];
        .id == "optional_downgrades" and .passed == true)
    )
  ))
' "$OUTPUT_FILE" >/dev/null

jq -r '
      "run_id=\(.run_id)",
      "downstream_id=\(.downstream_id)",
      "passed=\(.summary.passed) warning=\(.summary.warning) failed=\(.summary.failed)",
      (.cells[] |
        "model=\(.model_slug)\tclient=\(.client_family)\tstatus=\(.status)\thttp=\(.http_status)\t" +
        "runtime=\(.runtime_model_slug // .model_slug)\tupstream_id=\(.selected_upstream_id // "-")\t" +
        "upstream_name=\(.selected_upstream_name // "-")\tprotocol=\(.selected_upstream_protocol // "-")\t" +
        "profile=\(.profile_state // "unknown")\tprobe_version=\(.probe_version // 0)\t" +
        "transition=\(.protocol_transition // "native")\tadapters=\((.adapter_set // []) | join(","))\t" +
        "retry=\(.dialect_retry_count // 0)\tfallback=\(.fallback_stage // "-")\t" +
        "first_event_ms=\(.first_meaningful_event_ms // 0)\tduration_ms=\(.duration_ms)"
      )
    ' "$OUTPUT_FILE"
