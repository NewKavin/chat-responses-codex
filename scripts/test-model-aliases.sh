#!/usr/bin/env bash
# 模型映射 UI 功能测试脚本
set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost:3000}"
ADMIN_TOKEN="${ADMIN_TOKEN:-}"

echo "========================================"
echo "模型映射 UI 功能测试"
echo "========================================"
echo ""

# 检查是否提供了 token
if [[ -z "$ADMIN_TOKEN" ]]; then
  echo "❌ 错误: 请设置 ADMIN_TOKEN 环境变量"
  echo ""
  echo "使用方法:"
  echo "  export ADMIN_TOKEN='your-admin-token'"
  echo "  ./test-model-aliases.sh"
  echo ""
  echo "或者:"
  echo "  ADMIN_TOKEN='your-token' ./test-model-aliases.sh"
  exit 1
fi

echo "🔍 测试环境:"
echo "  - Base URL: $BASE_URL"
echo "  - Admin Token: ${ADMIN_TOKEN:0:10}..."
echo ""

# 测试 1: 获取当前映射规则
echo "📋 测试 1: 获取当前映射规则"
RESPONSE=$(curl -s -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$BASE_URL/api/admin/model-aliases")
echo "响应: $RESPONSE"

# 检查是否成功
if echo "$RESPONSE" | grep -q "model_aliases"; then
  echo "✅ 测试 1 通过: API 响应正常"
else
  echo "❌ 测试 1 失败: API 响应异常"
  exit 1
fi
echo ""

# 测试 2: 获取上游列表
echo "📋 测试 2: 获取上游列表"
UPSTREAMS=$(curl -s -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$BASE_URL/api/admin/upstreams")
echo "上游数量: $(echo "$UPSTREAMS" | grep -o '"id"' | wc -l)"

if echo "$UPSTREAMS" | grep -q '"id"'; then
  echo "✅ 测试 2 通过: 上游列表获取成功"
else
  echo "⚠️  测试 2 警告: 没有上游账号，跳过后续测试"
  exit 0
fi
echo ""

# 测试 3: 添加一个映射规则
echo "📋 测试 3: 添加映射规则"
TEST_RULE='{
  "model_aliases": [
    {
      "canonical": "test-model-v1",
      "aliases": ["test-model", "TestModel"]
    }
  ]
}'

ADD_RESPONSE=$(curl -s -X PUT \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d "$TEST_RULE" \
  "$BASE_URL/api/admin/model-aliases")

echo "响应: $ADD_RESPONSE"

if echo "$ADD_RESPONSE" | grep -q "success\|ok"; then
  echo "✅ 测试 3 通过: 规则添加成功"
else
  echo "❌ 测试 3 失败: 规则添加失败"
  exit 1
fi
echo ""

# 测试 4: 验证规则已保存
echo "📋 测试 4: 验证规则已保存"
VERIFY=$(curl -s -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$BASE_URL/api/admin/model-aliases")

if echo "$VERIFY" | grep -q "test-model-v1"; then
  echo "✅ 测试 4 通过: 规则保存成功"
else
  echo "❌ 测试 4 失败: 规则未保存"
  exit 1
fi
echo ""

# 测试 5: 清理测试数据
echo "📋 测试 5: 清理测试数据"
CLEANUP='{
  "model_aliases": []
}'

CLEANUP_RESPONSE=$(curl -s -X PUT \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d "$CLEANUP" \
  "$BASE_URL/api/admin/model-aliases")

if echo "$CLEANUP_RESPONSE" | grep -q "success\|ok"; then
  echo "✅ 测试 5 通过: 清理成功"
else
  echo "⚠️  测试 5 警告: 清理可能失败（不影响主要功能）"
fi
echo ""

echo "========================================"
echo "✅ 所有测试通过！"
echo "========================================"
echo ""
echo "📝 手动测试步骤:"
echo "1. 打开浏览器访问: $BASE_URL"
echo "2. 登录管理后台"
echo "3. 进入: 资源管理 > 模型映射"
echo "4. 选择一个上游账号"
echo "5. 点击模型列表中的「快速添加」"
echo "6. 编辑规范名称和别名"
echo "7. 保存并验证"
echo ""
