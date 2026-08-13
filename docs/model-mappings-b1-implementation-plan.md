# 模型映射管理 UI - 方案 B-1 实现计划

## 📋 项目概述

**目标**：重构模型映射功能，从全局映射改为按上游隔离的独立映射。

**原则**：严格 TDD 开发
1. 先写测试用例
2. 测试失败（红灯）
3. 实现功能
4. 测试通过（绿灯）
5. 重构优化

---

## 🎯 需求定义

### 用户故事
作为管理员，我希望能够为每个上游账号独立配置模型映射，使得：
- 不同上游的相同模型可以映射成不同的下游名称
- 配置互不影响，清晰直观
- 可以快速查看和编辑某个上游的所有映射

### 数据模型设计

#### 后端数据结构
```rust
pub struct UpstreamModelMapping {
    pub upstream_id: String,
    pub mappings: Vec<ModelMapping>,
}

pub struct ModelMapping {
    pub upstream_model: String,   // 上游原始模型名
    pub downstream_model: String,  // 下游显示名称
}
```

#### API 设计
```
GET  /api/admin/upstreams/{id}/model-mappings
  → 返回该上游的所有映射

PUT  /api/admin/upstreams/{id}/model-mappings
  → 更新该上游的映射
  请求体: { mappings: [{upstream_model, downstream_model}] }

GET  /api/admin/model-mappings/all
  → 返回所有上游的映射（用于初始加载）
```

#### 数据库设计
```sql
CREATE TABLE upstream_model_mappings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    upstream_id UUID NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
    upstream_model TEXT NOT NULL,
    downstream_model TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(upstream_id, upstream_model)
);

CREATE INDEX idx_upstream_model_mappings_upstream_id 
    ON upstream_model_mappings(upstream_id);
```

---

## 📝 第一阶段：编写测试用例

### 后端测试用例

#### 测试文件：`tests/model_mappings_test.rs`

```rust
#[cfg(test)]
mod model_mappings_tests {
    use super::*;

    #[tokio::test]
    async fn test_get_upstream_mappings_empty() {
        // 获取一个没有配置映射的上游
        // 预期：返回空数组
    }

    #[tokio::test]
    async fn test_create_upstream_mappings() {
        // 为上游 A 创建映射
        // 预期：保存成功，可以读取
    }

    #[tokio::test]
    async fn test_update_upstream_mappings() {
        // 更新上游 A 的映射
        // 预期：旧映射被替换
    }

    #[tokio::test]
    async fn test_different_upstreams_independent() {
        // 上游 A 的 gpt-4 → gpt-4-a
        // 上游 B 的 gpt-4 → gpt-4-b
        // 预期：两者互不影响
    }

    #[tokio::test]
    async fn test_duplicate_upstream_model_rejected() {
        // 尝试为同一上游的同一模型创建两个映射
        // 预期：返回错误
    }

    #[tokio::test]
    async fn test_delete_upstream_cascades() {
        // 删除上游账号
        // 预期：其映射配置也被删除
    }

    #[tokio::test]
    async fn test_get_all_mappings() {
        // 获取所有上游的映射
        // 预期：按上游 ID 分组返回
    }
}
```

### 前端测试用例

#### 测试文件：`frontend/tests/model-mappings-per-upstream.spec.md`

```markdown
# 按上游隔离的模型映射 - 测试用例

## TC-1: 查看上游映射列表
1. 选择上游账号 A
2. 查看该上游的模型映射表格

**预期**：
- 显示上游 A 的所有模型映射
- 每行显示：上游模型名 → 下游模型名
- 有「添加映射」按钮

## TC-2: 为上游添加映射
1. 选择上游账号 A
2. 点击「添加映射」
3. 选择上游模型：gpt-4
4. 输入下游名称：gpt-4-premium
5. 保存

**预期**：
- 映射保存成功
- 表格中显示新映射
- 只影响上游 A，不影响其他上游

## TC-3: 不同上游的相同模型独立映射
1. 上游 A：gpt-4 → gpt-4-premium
2. 上游 B：gpt-4 → gpt-4-standard
3. 保存

**预期**：
- 两个映射都保存成功
- 互不影响
- 可以分别查看和编辑

## TC-4: 编辑映射
1. 选择上游 A
2. 点击某条映射的「编辑」
3. 修改下游名称
4. 保存

**预期**：
- 映射更新成功
- 上游模型名不可修改（只能修改下游名称）

## TC-5: 删除映射
1. 选择上游 A
2. 点击某条映射的「删除」
3. 确认删除

**预期**：
- 映射被删除
- 该模型恢复使用原始名称（无映射）

## TC-6: 快速添加（从上游模型列表）
1. 选择上游 A
2. 查看其支持的模型列表
3. 点击某个模型的「配置映射」
4. 自动填充上游模型名
5. 输入下游名称并保存

**预期**：
- 映射创建成功
- 减少手动输入错误

## TC-7: 重复映射检测
1. 为上游 A 的 gpt-4 创建映射
2. 再次尝试为上游 A 的 gpt-4 创建映射

**预期**：
- 提示错误：该模型已有映射
- 建议编辑现有映射

## TC-8: 切换上游账号
1. 查看上游 A 的映射
2. 切换到上游 B

**预期**：
- 显示上游 B 的映射
- 数据正确切换，无残留

## TC-9: 空映射状态
1. 选择一个没有配置映射的上游

**预期**：
- 显示空状态提示
- 提示「该上游暂无模型映射，所有模型使用原始名称」
- 显示「添加映射」按钮

## TC-10: 删除上游账号后
1. 为上游 A 配置映射
2. 删除上游 A 账号

**预期**：
- 上游被删除
- 其映射配置也被级联删除
```

---

## 🔧 第二阶段：后端实现

### 步骤 2.1：数据库迁移
**文件**：`migrations/XXXX_create_upstream_model_mappings.sql`

```sql
-- 创建映射表
-- 运行后测试：表结构正确，索引创建成功

-- 验证方法：
-- \d upstream_model_mappings
-- \di (查看索引)
```

### 步骤 2.2：数据模型
**文件**：`src/models/upstream_model_mapping.rs`

```rust
// 定义数据结构
// 实现序列化/反序列化
// 添加验证逻辑

// 测试：
// - 结构序列化正确
// - 字段验证有效
```

### 步骤 2.3：数据库操作层
**文件**：`src/db/upstream_model_mappings.rs`

```rust
// 实现 CRUD 操作：
// - get_upstream_mappings(pool, upstream_id)
// - set_upstream_mappings(pool, upstream_id, mappings)
// - get_all_mappings(pool)
// - delete_by_upstream_id(pool, upstream_id)

// 测试：运行 model_mappings_test.rs
// 预期：所有测试通过（绿灯）
```

### 步骤 2.4：API 路由
**文件**：`src/server/admin.rs`

```rust
// 添加路由处理函数：
// - admin_get_upstream_model_mappings
// - admin_update_upstream_model_mappings
// - admin_get_all_model_mappings

// 注册路由：
// GET  /api/admin/upstreams/:id/model-mappings
// PUT  /api/admin/upstreams/:id/model-mappings
// GET  /api/admin/model-mappings/all

// 测试：curl 验证 API
```

### 步骤 2.5：集成到路由解析
**文件**：`src/routing/model_resolver.rs`

```rust
// 修改模型解析逻辑：
// 1. 查询该上游的映射配置
// 2. 如果有映射，使用下游名称
// 3. 如果无映射，使用原始名称

// 测试：
// - 下游请求 gpt-4-premium → 路由到上游 A 的 gpt-4
// - 下游请求 gpt-4-standard → 路由到上游 B 的 gpt-4
```

---

## 🎨 第三阶段：前端实现

### 步骤 3.1：类型定义
**文件**：`frontend/src/types/index.ts`

```typescript
export interface ModelMapping {
  upstream_model: string
  downstream_model: string
}

export interface UpstreamModelMappings {
  upstream_id: string
  upstream_name: string
  mappings: ModelMapping[]
}
```

### 步骤 3.2：API 方法
**文件**：`frontend/src/api/admin.ts`

```typescript
// 添加方法：
// - getUpstreamModelMappings(upstreamId)
// - updateUpstreamModelMappings(upstreamId, mappings)
// - getAllModelMappings()
```

### 步骤 3.3：UI 组件
**文件**：`frontend/src/views/admin/ModelMappings.vue`

**界面设计**：
```
┌─────────────────────────────────────────────┐
│ 模型映射管理                                 │
├─────────────────────────────────────────────┤
│ 选择上游账号: [下拉框: 上游 A ▼]  [刷新]    │
├─────────────────────────────────────────────┤
│ 上游支持的模型 (快速添加)                   │
│ ┌──────────────────────────────────────┐   │
│ │ • gpt-4              [配置映射]       │   │
│ │ • deepseek-chat      [配置映射]       │   │
│ │ • claude-3-opus      [配置映射]       │   │
│ └──────────────────────────────────────┘   │
├─────────────────────────────────────────────┤
│ 当前映射配置                    [添加映射]  │
│ ┌──────────────────────────────────────┐   │
│ │ 上游模型        下游模型      操作    │   │
│ ├──────────────────────────────────────┤   │
│ │ gpt-4      →   gpt-4-premium  [编辑]  │   │
│ │ deepseek   →   deepseek-v3    [删除]  │   │
│ └──────────────────────────────────────┘   │
│                                   [保存全部]│
└─────────────────────────────────────────────┘
```

### 步骤 3.4：路由配置
保持不变：`/admin/model-mappings`

---

## 🧪 第四阶段：测试验证

### 步骤 4.1：后端单元测试
```bash
cargo test model_mappings
```
**预期**：所有测试通过

### 步骤 4.2：前端类型检查
```bash
npm run type-check
```
**预期**：无类型错误

### 步骤 4.3：集成测试
运行测试脚本验证端到端流程

### 步骤 4.4：手动测试
按照 `model-mappings-per-upstream.spec.md` 逐条验证

---

## 📦 第五阶段：构建部署

### 步骤 5.1：前端构建
```bash
npm run build
```

### 步骤 5.2：后端编译
```bash
cargo build --release
```

### 步骤 5.3：数据库迁移
```bash
# 生产环境执行迁移
sqlx migrate run
```

### 步骤 5.4：Docker 部署
```bash
./scripts/deploy.sh
```

### 步骤 5.5：验证部署
```bash
curl http://localhost:3000/api/admin/model-mappings/all
```

---

## 📊 时间估算

| 阶段 | 任务 | 预计时间 |
|------|------|---------|
| 1 | 编写测试用例 | 20 分钟 |
| 2.1 | 数据库迁移 | 10 分钟 |
| 2.2 | 数据模型 | 15 分钟 |
| 2.3 | 数据库操作层 | 30 分钟 |
| 2.4 | API 路由 | 20 分钟 |
| 2.5 | 路由解析集成 | 15 分钟 |
| 3.1 | 前端类型定义 | 5 分钟 |
| 3.2 | 前端 API 方法 | 10 分钟 |
| 3.3 | 前端 UI 组件 | 40 分钟 |
| 3.4 | 路由配置 | 5 分钟 |
| 4 | 测试验证 | 20 分钟 |
| 5 | 构建部署 | 15 分钟 |
| **总计** | | **约 3 小时 25 分钟** |

---

## ✅ 验收标准

### 功能验收
- [ ] 可以为每个上游独立配置映射
- [ ] 不同上游的相同模型可以映射成不同名称
- [ ] 映射配置正确保存和读取
- [ ] 删除上游时级联删除映射
- [ ] 下游请求正确路由到对应上游

### 测试验收
- [ ] 所有后端单元测试通过
- [ ] 所有前端手动测试用例通过
- [ ] TypeScript 类型检查通过
- [ ] 前端构建成功
- [ ] 后端编译成功

### 部署验收
- [ ] 数据库迁移成功
- [ ] Docker 容器正常运行
- [ ] API 端点可访问
- [ ] 前端页面正常加载

---

## 🚨 风险和注意事项

### 数据迁移风险
- **问题**：如果生产环境已有全局映射配置
- **方案**：编写迁移脚本将全局映射转换为每个上游的独立映射

### 向后兼容
- **问题**：旧的全局映射 API 是否保留
- **方案**：保留旧 API 3 个版本，标记为 deprecated

### 性能考虑
- **问题**：每次路由都要查询映射表
- **方案**：添加内存缓存，配置更新时刷新缓存

---

## 📝 下一步行动

1. **确认计划**：你确认这个计划后，我开始执行
2. **第一阶段**：先编写所有测试用例（后端 + 前端）
3. **第二阶段**：实现后端，直到所有测试通过
4. **第三阶段**：实现前端 UI
5. **第四阶段**：测试验证
6. **第五阶段**：构建部署

---

**准备好开始了吗？请确认计划，我立即开始第一阶段！**
