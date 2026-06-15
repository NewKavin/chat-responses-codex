#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/build-package-image.sh [options]

Build flow (local-only source compile):
  1) Build frontend locally
  2) Build Rust binary locally
  3) Build runtime image by copying local binary
  4) Export image tar package

Options:
  -i, --image <name>    Docker image name (default: chat-responses-codex)
  -t, --tag <tag>       Docker image tag (default: latest)
  -o, --output <file>   Output tar file path (default: <image>-<tag>.tar)
      --skip-npm-install
                        Skip npm dependency install step
      --skip-frontend-build
                        Skip frontend build step
      --skip-backend-build
                        Skip backend release build step
      --skip-image-build
                        Skip docker image build step
      --skip-export
                        Skip docker image export step
  -h, --help            Show this help message

Examples:
  scripts/build-package-image.sh
  scripts/build-package-image.sh --image chat-responses-codex --tag v0.1.2
  scripts/build-package-image.sh -i my/gateway -t prod -o /tmp/gateway-prod.tar
EOF
}

log() {
  printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"
}

ensure_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: required command '$1' not found in PATH" >&2
    exit 1
  fi
}

IMAGE_NAME="chat-responses-codex"
IMAGE_TAG="latest"
OUTPUT_TAR=""
SKIP_NPM_INSTALL=0
SKIP_FRONTEND_BUILD=0
SKIP_BACKEND_BUILD=0
SKIP_IMAGE_BUILD=0
SKIP_EXPORT=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -i|--image)
      IMAGE_NAME="${2:-}"
      shift 2
      ;;
    -t|--tag)
      IMAGE_TAG="${2:-}"
      shift 2
      ;;
    -o|--output)
      OUTPUT_TAR="${2:-}"
      shift 2
      ;;
    --skip-npm-install)
      SKIP_NPM_INSTALL=1
      shift
      ;;
    --skip-frontend-build)
      SKIP_FRONTEND_BUILD=1
      shift
      ;;
    --skip-backend-build)
      SKIP_BACKEND_BUILD=1
      shift
      ;;
    --skip-image-build)
      SKIP_IMAGE_BUILD=1
      shift
      ;;
    --skip-export)
      SKIP_EXPORT=1
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

if [[ -z "$IMAGE_NAME" || -z "$IMAGE_TAG" ]]; then
  echo "Error: image name and tag must not be empty" >&2
  exit 1
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

if [[ -z "$OUTPUT_TAR" ]]; then
  OUTPUT_TAR="${IMAGE_NAME//\//-}-${IMAGE_TAG}.tar"
fi
OUTPUT_TAR="${OUTPUT_TAR//:/-}"

log "Repository root: $REPO_ROOT"
log "Image: ${IMAGE_NAME}:${IMAGE_TAG}"
log "Tar output: ${OUTPUT_TAR}"

ensure_command cargo
ensure_command docker
if [[ "$SKIP_FRONTEND_BUILD" -eq 0 || "$SKIP_NPM_INSTALL" -eq 0 ]]; then
  ensure_command npm
fi

if [[ "$SKIP_NPM_INSTALL" -eq 0 ]]; then
  log "Installing frontend dependencies..."
  if [[ -f frontend/package-lock.json ]]; then
    npm --prefix frontend ci
  else
    npm --prefix frontend install
  fi
else
  log "Skipping npm dependency install"
fi

if [[ "$SKIP_FRONTEND_BUILD" -eq 0 ]]; then
  log "Building frontend..."
  npm --prefix frontend run build
else
  log "Skipping frontend build"
fi

if [[ "$SKIP_BACKEND_BUILD" -eq 0 ]]; then
  log "Building backend (release)..."
  cargo build --release
else
  log "Skipping backend release build"
fi

if [[ "$SKIP_IMAGE_BUILD" -eq 0 ]]; then
  BINARY_PATH="target/release/chat-responses-codex"
  if [[ ! -x "$BINARY_PATH" ]]; then
    echo "Error: binary not found or not executable: $BINARY_PATH" >&2
    echo "Hint: run without --skip-backend-build, or build it first with 'cargo build --release'" >&2
    exit 1
  fi

  CONTEXT_DIR="$(mktemp -d /tmp/chat-responses-codex-image-context.XXXXXX)"
  trap 'rm -rf "$CONTEXT_DIR"' EXIT

  cp "$BINARY_PATH" "$CONTEXT_DIR/chat-responses-codex"
  cat > "$CONTEXT_DIR/Dockerfile" <<'EOF'
FROM debian:bookworm-slim

WORKDIR /app
COPY chat-responses-codex /usr/local/bin/chat-responses-codex

ENV BIND_ADDR=0.0.0.0:3001
ENV STATE_PATH=/data/state.json
ENV LOG_PATH=/logs/chat-responses-codex.log
ENV APP_NAME=chat-responses-codex

VOLUME ["/data", "/logs"]
EXPOSE 3001

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/chat-responses-codex", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/chat-responses-codex"]
EOF

  log "Building docker image (copy local binary only)..."
  docker build -t "${IMAGE_NAME}:${IMAGE_TAG}" "$CONTEXT_DIR"
else
  log "Skipping docker image build"
fi

if [[ "$SKIP_EXPORT" -eq 0 ]]; then
  log "Exporting docker image to tar..."
  docker save -o "$OUTPUT_TAR" "${IMAGE_NAME}:${IMAGE_TAG}"
  if [[ -f "$OUTPUT_TAR" ]]; then
    log "Export finished: $(du -h "$OUTPUT_TAR" | awk '{print $1}') $OUTPUT_TAR"
  fi
else
  log "Skipping docker image export"
fi

log "Done."
