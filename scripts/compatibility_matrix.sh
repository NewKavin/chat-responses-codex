#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
DOWNSTREAM_ID="${DOWNSTREAM_ID:-test}"
CLIENTS_JSON="${CLIENTS_JSON:-[\"codex\",\"opencode\",\"hermes\"]}"
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

RAW_JSON="$(
  curl -fsS "$BASE_URL/api/admin/troubleshooting/matrix/run" \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    --data-binary "$REQUEST_PAYLOAD"
)"

printf '%s\n' "$RAW_JSON" | tee "$OUTPUT_FILE"
printf '%s\n' "$RAW_JSON" \
  | jq -r '
      "run_id=\(.run_id)",
      "downstream_id=\(.downstream_id)",
      "passed=\(.summary.passed) warning=\(.summary.warning) failed=\(.summary.failed)",
      (.cells[] | "\(.model_slug)\t\(.client_family)\t\(.status)\tHTTP \(.http_status)\t\(.summary)")
    '
