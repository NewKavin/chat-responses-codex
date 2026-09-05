# 下游模型分组功能 - 快速开始

## 功能简介

下游（Downstream）现在支持使用**模型分组**来批量授权模型，大幅提升配置效率。

### 对比：手动配置 vs 模型分组

| 操作场景 | 手动配置 | 使用模型分组 |
|----------|----------|--------------|
| **配置100个模型** | 需要100次点击 | 选择1个分组，1次点击 |
| **新模型上线** | 需要编辑20个下游，20次操作 | 在分组中添加1次，所有下游立即生效 |
| **模型下线** | 需要编辑20个下游 | 从分组中移除1次 |

---

## 🚀 部署步骤

### 1. 执行数据库迁移

```bash
# 确保设置了 DATABASE_URL 环境变量
export DATABASE_URL="postgresql://user:password@localhost/chat2responses"

# 执行迁移
psql $DATABASE_URL -f migrations/2026-09-05-add-downstream-model-group-id.sql
```

**预期输出**:
```
BEGIN
ALTER TABLE
CREATE INDEX
COMMENT
COMMIT
```

### 2. 重启服务

```bash
# 如果使用 systemd
systemctl restart chat2responses

# 或者如果手动运行
pkill chat2responses
./target/release/chat2responses
```

### 3. 验证部署

访问管理后台 → 下游管理，查看是否有"使用模型分组"选项。

---

## 📖 使用指南

### 方式 1: 创建新下游时使用模型分组

1. 进入 **管理后台 → 下游管理**
2. 点击 **创建下游**
3. 在 **模型权限配置** 部分：
   - 选择 **"使用模型分组（推荐）"**
   - 从下拉列表选择一个模型分组（如 "基础模型"）
   - 预览会显示该分组包含的所有模型
4. 填写其他必填项，点击 **创建**

### 方式 2: 将现有下游切换到模型分组

1. 进入 **管理后台 → 下游管理**
2. 点击要编辑的下游的 **编辑** 按钮
3. 在 **模型权限配置** 部分：
   - 切换到 **"使用模型分组（推荐）"**
   - 选择一个模型分组
4. 点击 **保存**

> ⚠️ **注意**: 切换模式后，原有的手动配置会被清空。

### 方式 3: 批量更新模型权限

**场景**: 你有20个下游都需要访问新上线的 `gpt-4o` 模型

**传统方式**（手动配置）:
1. 打开第1个下游 → 勾选 `gpt-4o` → 保存
2. 打开第2个下游 → 勾选 `gpt-4o` → 保存
3. ... 重复20次

**使用模型分组**:
1. 进入 **管理后台 → 模型分组管理**
2. 编辑 "基础模型" 分组
3. 勾选 `gpt-4o` → 保存
4. ✅ 所有使用该分组的下游立即获得 `gpt-4o` 访问权限

---

## 🔍 界面说明

### 下游列表中的显示

| 显示内容 | 含义 |
|----------|------|
| **🔲 基础模型 (23个模型)** | 使用模型分组 "基础模型"，包含23个模型 |
| **手动配置 (15个模型)** | 使用传统的手动配置方式 |

### 编辑表单中的两种模式

#### 模式 1: 使用模型分组（推荐）

```
○ 使用模型分组（推荐）
● 手动配置模型列表

┌─────────────────────────────────────┐
│ 选择模型分组： [基础模型        ▼] │
│                                     │
│ 预览：该分组包含 23 个模型          │
│ • gpt-4                             │
│ • gpt-4-turbo                       │
│ • claude-3-opus                     │
│ ...                                 │
└─────────────────────────────────────┘
```

#### 模式 2: 手动配置模型列表

```
● 使用模型分组（推荐）
○ 手动配置模型列表

┌─────────────────────────────────────┐
│ ☑ gpt-4                             │
│ ☑ gpt-4-turbo                       │
│ ☐ claude-3-opus                     │
│ ☐ gemini-pro                        │
│ ...                                 │
└─────────────────────────────────────┘
```

---

## ⚙️ 优先级规则

系统按以下优先级判断模型权限：

```
1. 检查 model_group_id 是否存在
   ↓
   [存在] → 查询模型分组的模型列表
   |         ↓
   |      [查询成功] → 使用模型分组的权限 ✅
   |         ↓
   |      [查询失败] → 回退到 model_allowlist ⚠️
   ↓
   [不存在] → 使用 model_allowlist
```

> 💡 **提示**: 删除模型分组后，使用该分组的下游会自动回退到空白名单（需要重新配置）。

---

## 🧪 测试验证

### 测试 1: 验证模型分组生效

```bash
# 1. 创建一个使用 "基础模型" 分组的下游
# 2. 使用该下游的 Key 发起请求

curl -X POST https://your-domain.com/v1/chat/completions \
  -H "Authorization: Bearer sk-downstream-xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello"}]
  }'

# 预期：成功（如果 gpt-4 在 "基础模型" 分组中）
```

### 测试 2: 验证未授权模型被拒绝

```bash
curl -X POST https://your-domain.com/v1/chat/completions \
  -H "Authorization: Bearer sk-downstream-xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-3.5-turbo",
    "messages": [{"role": "user", "content": "Hello"}]
  }'

# 预期：403 Forbidden（如果 gpt-3.5-turbo 不在分组中）
```

### 测试 3: 验证模型分组更新立即生效

```bash
# 1. 在管理后台编辑 "基础模型" 分组，添加 gemini-pro
# 2. 立即使用下游的 Key 请求 gemini-pro

curl -X POST https://your-domain.com/v1/chat/completions \
  -H "Authorization: Bearer sk-downstream-xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-pro",
    "messages": [{"role": "user", "content": "Hello"}]
  }'

# 预期：立即成功（无需重启服务）
```

---

## 🔧 常见问题

### Q1: 现有的下游会受影响吗？

**A**: 不会。现有的下游使用 `model_allowlist`（手动配置），完全不受影响。只有主动切换到"使用模型分组"才会改变行为。

### Q2: 如果删除了模型分组会怎样？

**A**: 使用该模型分组的下游的 `model_group_id` 会被自动设为 `NULL`，权限回退到空白名单（需要重新配置）。

数据库设置了 `ON DELETE SET NULL`，确保不会破坏数据完整性。

### Q3: 可以同时使用模型分组和手动配置吗？

**A**: 不可以。系统按优先级判断：
- 如果 `model_group_id` 存在 → 只使用模型分组
- 如果 `model_group_id` 不存在 → 使用手动配置

前端表单会确保只设置其中一个。

### Q4: 模型分组查询失败会怎样？

**A**: 系统会自动回退到 `model_allowlist`。这是一个**优雅降级**机制，确保服务不会因为模型分组查询失败而完全不可用。

### Q5: 如何查看哪些下游使用了某个模型分组？

**A**: 目前可以通过 SQL 查询：

```sql
SELECT 
  d.id, 
  d.name, 
  mg.name AS group_name
FROM downstreams d
JOIN model_groups mg ON d.model_group_id = mg.id
WHERE mg.name = '基础模型';
```

未来版本可能会在前端添加"使用情况"统计。

---

## 📊 监控建议

### 关键指标

```sql
-- 模型分组使用率
SELECT 
  COUNT(CASE WHEN model_group_id IS NOT NULL THEN 1 END) AS using_group,
  COUNT(CASE WHEN model_group_id IS NULL THEN 1 END) AS using_manual,
  COUNT(*) AS total
FROM downstreams;
```

### 日志监控

关注以下日志：
- `"Failed to query model_group"` - 模型分组查询失败
- `"Downstream fallback to model_allowlist"` - 回退到手动配置

如果这些日志频繁出现，可能是：
1. 模型分组被删除了
2. 数据库查询性能问题
3. 数据库连接问题

---

## 🎯 推荐实践

### 1. 为不同场景创建模型分组

```
基础模型：gpt-4, gpt-4-turbo, claude-3-opus（用于普通用户）
高级模型：gpt-4o, claude-3.5-sonnet（用于付费用户）
开源模型：llama-3-70b, mixtral-8x7b（用于测试环境）
```

### 2. 定期审计模型分组

- 每月检查模型分组的使用情况
- 清理不再使用的模型分组
- 确保模型分组的命名清晰

### 3. 新模型上线流程

1. 先在测试环境的模型分组中添加新模型
2. 测试通过后，再添加到生产环境的模型分组
3. 所有使用该分组的下游立即获得访问权限

---

## 📚 相关文档

- [完整技术文档](DOWNSTREAM_MODEL_GROUP_FEATURE.md)
- [数据库迁移指南](docs/deployment-guide-multi-keys.md)
- [模型分组管理](docs/model-groups.md)

---

## 🚨 回滚指南

如果部署后发现问题，可以快速回滚：

```bash
# 1. 停止服务
systemctl stop chat2responses

# 2. 数据库回滚（将使用模型分组的下游恢复为空白名单）
psql $DATABASE_URL <<EOF
BEGIN;
UPDATE downstreams SET model_group_id = NULL WHERE model_group_id IS NOT NULL;
ALTER TABLE downstreams DROP COLUMN model_group_id;
COMMIT;
EOF

# 3. 代码回滚
git revert <commit-hash>
cargo build --release
cd frontend && npm run build

# 4. 重启服务
systemctl start chat2responses
```

---

**开发完成时间**: 2026-09-05  
**开发者**: Claude (Opus 5)  
**状态**: ✅ 编译通过，等待部署测试
