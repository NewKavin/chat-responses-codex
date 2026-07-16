#!/usr/bin/env bash
set -euo pipefail
set +x

OUTPUT="templates/capabilities/current-deployment.rendered.json"
IMPORT=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      OUTPUT="$2"
      shift 2
      ;;
    --import)
      IMPORT=1
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

: "${QWEN_VLM_SLUG:?QWEN_VLM_SLUG is required}"
: "${IMAGE_FIXTURE_URL:?IMAGE_FIXTURE_URL is required}"
: "${IMAGE_FIXTURE_EXPECTED_LABEL:?IMAGE_FIXTURE_EXPECTED_LABEL is required}"

jq \
  --arg qwen "$QWEN_VLM_SLUG" \
  --arg image_url "$IMAGE_FIXTURE_URL" \
  --arg image_label "$IMAGE_FIXTURE_EXPECTED_LABEL" \
  '.compatibility_expectations += [{
      "id": "selected-qwen-vlm",
      "selector": {"exposed_model": $qwen},
      "bundles": ["agent_core", "image_agent"],
      "client_profiles": ["codex", "opencode", "claude_code", "hermes"],
      "permitted_optional_downgrades": ["optional_image_detail"],
      "https_image_fixture": {
        "url": $image_url,
        "expected_label": $image_label
      }
    }]' \
  templates/capabilities/current-deployment.example.json > "$OUTPUT"

if [[ "$IMPORT" == "1" ]]; then
  : "${BASE_URL:?BASE_URL is required for --import}"
  : "${ADMIN_TOKEN:?ADMIN_TOKEN is required for --import}"
  curl -fsS "$BASE_URL/api/admin/capabilities/import" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H 'Content-Type: application/json' \
    --data-binary @"$OUTPUT" >/dev/null
fi
