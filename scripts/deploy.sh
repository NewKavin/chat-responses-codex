#!/usr/bin/env bash
set -euo pipefail

unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy

usage() {
  cat <<'EOF'
Usage:
  scripts/deploy.sh [options]

Options:
  -d, --deploy-dir <path>    Deployment directory (default: ~/docker/chat-responses-codex)
  -i, --image <name>         Docker image name (default: chat-responses-codex)
  -t, --tag <tag>            Docker image tag (default: latest)
      --skip-build           Skip docker image build step
      --skip-start           Skip compose up step
      --force-copy-config    Overwrite existing docker-compose.yml/.env in deploy dir
  -h, --help                 Show this help message
EOF
}

log() {
  printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"
}

need_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Error: required command '$cmd' not found" >&2
    exit 1
  fi
}

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DEPLOY_DIR="${HOME}/docker/chat-responses-codex"
IMAGE_NAME="chat-responses-codex"
IMAGE_TAG="latest"
SKIP_BUILD=0
SKIP_START=0
FORCE_COPY_CONFIG=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -d|--deploy-dir)
      DEPLOY_DIR="${2:-}"
      shift 2
      ;;
    -i|--image)
      IMAGE_NAME="${2:-}"
      shift 2
      ;;
    -t|--tag)
      IMAGE_TAG="${2:-}"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --skip-start)
      SKIP_START=1
      shift
      ;;
    --force-copy-config)
      FORCE_COPY_CONFIG=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Error: unknown option '$1'" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "${IMAGE_NAME}" || -z "${IMAGE_TAG}" ]]; then
  echo "Error: image name and tag must be non-empty" >&2
  exit 1
fi

need_cmd docker

if docker compose version >/dev/null 2>&1; then
  COMPOSE=(docker compose)
elif command -v docker-compose >/dev/null 2>&1; then
  COMPOSE=(docker-compose)
else
  echo "Error: docker compose plugin or docker-compose not found" >&2
  exit 1
fi

REPO_COMPOSE="$REPO_ROOT/docker-compose.yml"
REPO_ENV_EXAMPLE="$REPO_ROOT/.env.example"
DEPLOY_COMPOSE="$DEPLOY_DIR/docker-compose.yml"
DEPLOY_ENV="$DEPLOY_DIR/.env"

mkdir -p "$DEPLOY_DIR"

copy_or_check() {
  local src="$1"
  local dst="$2"
  if [[ ! -f "$src" ]]; then
    echo "Error: source file missing: $src" >&2
    exit 1
  fi
  if [[ -f "$dst" ]] && [[ "$FORCE_COPY_CONFIG" -eq 0 ]]; then
    return
  fi
  cp "$src" "$dst"
}

copy_or_check "$REPO_COMPOSE" "$DEPLOY_COMPOSE"

if [[ ! -f "$DEPLOY_ENV" ]]; then
  if [[ -f "$REPO_ENV_EXAMPLE" ]]; then
    cp "$REPO_ENV_EXAMPLE" "$DEPLOY_ENV"
    log "Created ${DEPLOY_ENV} from .env.example; edit APP secrets before start"
  else
    echo "Error: source env example missing: $REPO_ENV_EXAMPLE" >&2
    exit 1
  fi
elif [[ "$FORCE_COPY_CONFIG" -eq 1 ]]; then
  cp "$REPO_ENV_EXAMPLE" "$DEPLOY_ENV"
fi

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  log "Building docker image ${IMAGE_NAME}:${IMAGE_TAG}"
  "$SCRIPT_DIR/build-package-image.sh" \
    --image "$IMAGE_NAME" \
    --tag "$IMAGE_TAG" \
    --skip-export
else
  log "Skip docker image build"
fi

if [[ "$SKIP_START" -eq 1 ]]; then
  log "Skip deployment start"
  exit 0
fi

if [[ ! -f "$DEPLOY_COMPOSE" ]]; then
  echo "Error: compose file missing: $DEPLOY_COMPOSE" >&2
  exit 1
fi
if [[ ! -f "$DEPLOY_ENV" ]]; then
  echo "Error: env file missing: $DEPLOY_ENV" >&2
  exit 1
fi

log "Deploying with compose in ${DEPLOY_DIR}"
"${COMPOSE[@]}" --env-file "$DEPLOY_ENV" -f "$DEPLOY_COMPOSE" --project-directory "$DEPLOY_DIR" up -d --remove-orphans

wait_for_gateway_health() {
  local container="${GATEWAY_CONTAINER_NAME:-chat-responses-codex}"
  local port="${GATEWAY_HEALTH_PORT:-3000}"
  local health attempt

  for attempt in $(seq 1 30); do
    health=$(docker inspect --format '{{.State.Health.Status}}' "$container" 2>/dev/null || true)
    printf '[%s] Gateway health attempt %s/30: %s\n' \
      "$(date '+%Y-%m-%d %H:%M:%S')" "$attempt" "${health:-unknown}"
    if [[ "$health" == healthy ]] &&
       [[ "$(curl --noproxy '*' -fsS "http://127.0.0.1:${port}/healthz" 2>/dev/null || true)" == ok ]]; then
      return 0
    fi
    sleep 2
  done

  echo "Error: gateway failed health verification" >&2
  return 1
}

verify_deployed_frontend() {
  local port="${GATEWAY_HEALTH_PORT:-3000}"
  local index asset main upstream_asset

  index=$(curl --noproxy '*' -fsS "http://127.0.0.1:${port}/") || return 1
  asset=$(printf '%s' "$index" | grep -o 'assets/index-[^" ]*\.js' | head -n1)
  [[ -n "$asset" ]] || { echo "Error: deployed index has no main asset" >&2; return 1; }
  main=$(curl --noproxy '*' -fsS "http://127.0.0.1:${port}/${asset}") || return 1
  upstream_asset=$(printf '%s' "$main" | grep -o 'assets/Upstreams-[^" ]*\.js' | head -n1)
  [[ -n "$upstream_asset" ]] || { echo "Error: deployed main asset has no Upstreams chunk" >&2; return 1; }
  curl --noproxy '*' -fsS "http://127.0.0.1:${port}/${upstream_asset}" |
    grep -F -q 'key_concurrency' || {
      echo "Error: deployed Upstreams chunk is stale" >&2
      return 1
    }
}

wait_for_gateway_health
verify_deployed_frontend

log "Deployment finished"
"${COMPOSE[@]}" --project-directory "$DEPLOY_DIR" -f "$DEPLOY_COMPOSE" ps
