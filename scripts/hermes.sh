#!/usr/bin/env bash
# 启动 Hermes Agent，接入本地 chat-responses-codex 网关。
#
# 前置条件：
#   1. 网关已运行（默认 0.0.0.0:3001），可用 `./target/release/chat-responses-codex` 或 docker compose up 启动
#   2. 在网关后台「下游」创建下游 Key，明文填入项目根目录 .hermes.env 的 CHAT2RESPONSES_KEY
#
# 用法：
#   ./scripts/hermes.sh chat          # 交互式对话
#   ./scripts/hermes.sh -m gpt-5-codex chat   # 指定模型
#   ./scripts/hermes.sh --help
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VENV_DIR="$PROJECT_ROOT/.hermes-venv"

# 加载环境变量
if [[ -f "$PROJECT_ROOT/.hermes.env" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$PROJECT_ROOT/.hermes.env"
  set +a
fi

# 确保 hermes 可执行（venv + npm 桥接器）
export PATH="$VENV_DIR/bin:$PATH"
exec node "$PROJECT_ROOT/node_modules/hermes-agent/bin/hermes.js" "$@"
