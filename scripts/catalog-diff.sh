#!/usr/bin/env bash
# catalog-diff.sh —— 阶段 2 端到端验收：对比迁移前后每个在用 key 的 Codex 模型目录。
#
# 用法：
#   scripts/catalog-diff.sh snapshot <dir>    # 为每个在用 key 拉取 format=codex 目录存到 <dir>
#   scripts/catalog-diff.sh diff <before> <after> [report]   # 逐 key 对比，输出差异报告
#
# 依赖：psql、curl、jq 或 python3（任选其一），读取部署目录 .env 获得管理员凭据。
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
ENV_FILE="${ENV_FILE:-$HOME/docker/chat-responses-codex/.env}"

# 从 .env 提取值（兼容 KEY="value" 或 KEY=value）
env_get() {
  local key="$1"
  grep -E "^${key}=" "$ENV_FILE" | head -1 | cut -d= -f2- | tr -d '"' | tr -d "'"
}

# 获取管理员 token
admin_token() {
  local user pass
  user="$(env_get ADMIN_USERNAME)"; pass="$(env_get ADMIN_PASSWORD)"
  [ -n "$user" ] && [ -n "$pass" ] || { echo "ADMIN_USERNAME/ADMIN_PASSWORD missing in $ENV_FILE" >&2; exit 1; }
  curl -fsS -X POST "$BASE_URL/api/admin/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$user\",\"password\":\"$pass\"}" |
    python3 -c 'import sys,json; print(json.load(sys.stdin)["token"])'
}

# 列出所有在用下游（plaintext_key 非空、active）——直连数据库取明文 key
list_keys() {
  local pgurl
  pgurl="$(env_get DATABASE_URL)"
  [ -n "$pgurl" ] || { echo "DATABASE_URL missing in $ENV_FILE" >&2; exit 1; }
  psql "$pgurl" -At -F $'\t' -c \
    "SELECT d.id, d.plaintext_key, COALESCE(NULLIF(d.name,''), d.id) FROM downstreams d WHERE d.active AND d.plaintext_key IS NOT NULL AND d.plaintext_key <> '' ORDER BY d.id;"
}

# 拉取一个 key 的 codex 目录（slug 一行一个，排序后输出）
fetch_catalog() {
  local key="$1"
  curl -fsS -H "Authorization: Bearer $key" "$BASE_URL/v1/models?format=codex" |
    python3 -c 'import sys,json
data=json.load(sys.stdin)
print("\n".join(sorted(m["slug"] for m in data.get("models", []))))'
}

cmd="${1:-}"; shift || true
case "$cmd" in
  snapshot)
    outdir="$1"
    mkdir -p "$outdir"
    list_keys | while IFS=$'\t' read -r id key name; do
      fetch_catalog "$key" > "$outdir/${name}.txt" 2>/dev/null || echo "  (fetch failed: $name)" >&2
      echo "  saved ${name}.txt ($(wc -l < "$outdir/${name}.txt") slugs)"
    done
    ;;
  diff)
    before="$1"; after="$2"; report="${3:-/tmp/catalog-diff-report.txt}"
    : > "$report"
    rc=0
    for file in "$before"/*.txt; do
      name="$(basename "$file")"
      if [ ! -f "$after/$name" ]; then
        echo "MISSING  $name (only in before)" >> "$report"; rc=1; continue
      fi
      if ! diff -u "$file" "$after/$name" > /tmp/.catalog-diff-one.txt 2>&1; then
        echo "DIFF     $name" >> "$report"
        sed 's/^/    /' /tmp/.catalog-diff-one.txt >> "$report"
        rc=1
      else
        echo "OK       $name ($(wc -l < "$file") slugs unchanged)" >> "$report"
      fi
    done
    for file in "$after"/*.txt; do
      name="$(basename "$file")"
      if [ ! -f "$before/$name" ]; then
        echo "NEW      $name (only in after)" >> "$report"; rc=1
      fi
    done
    echo "==== report: $report ===="
    cat "$report"
    [ "$rc" -eq 0 ] || { echo "CATALOG DIFFS FOUND" >&2; exit 1; }
    ;;
  *)
    echo "usage: $0 {snapshot <dir>|diff <before> <after> [report]}" >&2
    exit 2
    ;;
esac
