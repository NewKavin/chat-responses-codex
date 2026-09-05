# 下游模型分组功能实现文档

## 功能概述

为下游（Downstream）增加模型分组（Model Group）支持，允许管理员通过选择模型分组来批量授权模型，而不是手动配置每个模型。

## 实现内容

### 1. 后端改动

#### 1.1 数据结构修改

**文件**: `src/state/postgres.rs`

在 `DownstreamConfig` 结构体中新增字段：

```rust
pub struct DownstreamConfig {
    // ... 原有字段 ...
    
    /// 模型分组 ID（优先级高于 model_allowlist）
    /// 如果设置了此字段，将使用模型分组的权限
    /// 如果为 None 或查询失败，则回退到使用 model_allowlist
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_group_id: Option<Uuid>,
}
```

**优先级逻辑**:
1. 如果 `model_group_id` 存在且能成功查询到模型分组 → 使用模型分组的模型列表
2. 否则 → 回退到 `model_allowlist`（向后兼容）

#### 1.2 数据库迁移

**文件**: `migrations/2026-09-05-add-downstream-model-group-id.sql`

```sql
BEGIN;

-- 为 downstreams 表添加 model_group_id 字段
ALTER TABLE downstreams 
ADD COLUMN IF NOT EXISTS model_group_id UUID REFERENCES model_groups(id) ON DELETE SET NULL;

-- 添加索引以优化查询性能
CREATE INDEX IF NOT EXISTS idx_downstreams_model_group_id ON downstreams(model_group_id);

-- 添加注释
COMMENT ON COLUMN downstreams.model_group_id IS '模型分组ID，优先级高于model_allowlist。如果设置，将使用模型分组的权限';

COMMIT;
```

**执行方式**:
```bash
psql $DATABASE_URL -f migrations/2026-09-05-add-downstream-model-group-id.sql
```

**特性**:
- ✅ 幂等性：可以重复执行
- ✅ 无损：不影响现有数据
- ✅ 无停机：可以在线执行
- ✅ 回滚：`ON DELETE SET NULL` 确保删除模型分组时不会破坏下游

### 2. 前端改动

#### 2.1 类型定义

**文件**: `frontend/src/types/admin.ts`

```typescript
export interface DownstreamConfig {
  // ... 原有字段 ...
  
  /** 模型分组 ID（优先级高于 model_allowlist） */
  model_group_id?: string
}
```

#### 2.2 UI 改进

**文件**: `frontend/src/views/admin/Downstreams.vue`

**新增功能**:

1. **表格列显示** - 显示下游使用的模型授权方式：
   - 如果使用模型分组：显示 `<Layers icon> 模型分组名称 (N个模型)`
   - 如果使用模型白名单：显示 `手动配置 (N个模型)`

2. **表单编辑** - 支持两种模式切换：
   ```
   ┌─────────────────────────────────────────────┐
   │ 模型权限配置                                │
   │                                             │
   │ 选择方式：                                  │
   │ ○ 使用模型分组（推荐）                      │
   │ ● 手动配置模型列表                          │
   │                                             │
   │ [如果选择"使用模型分组"]                    │
   │ ┌─────────────────────────────────────────┐ │
   │ │ 选择模型分组： [下拉选择器         ▼]  │ │
   │ │                                         │ │
   │ │ 预览：该分组包含 23 个模型              │ │
   │ │ • gpt-4                                 │ │
   │ │ • gpt-4-turbo                           │ │
   │ │ • claude-3-opus                         │ │
   │ │ ...                                     │ │
   │ └─────────────────────────────────────────┘ │
   │                                             │
   │ [如果选择"手动配置模型列表"]                │
   │ ┌─────────────────────────────────────────┐ │
   │ │ ☑ gpt-4                                 │ │
   │ │ ☑ gpt-4-turbo                           │ │
   │ │ ☐ claude-3-opus                         │ │
   │ │ ...                                     │ │
   │ └─────────────────────────────────────────┘ │
   └─────────────────────────────────────────────┘
   ```

3. **智能切换逻辑**:
   - 编辑现有下游时，根据 `model_group_id` 是否存在自动选择模式
   - 切换模式时自动清空对方的字段（避免冲突）
   - 提交时只发送当前模式对应的字段

#### 2.3 新增的响应式变量

```typescript
// 模型分组相关
const modelGroups = ref<ModelGroup[]>([])
const loadingGroups = ref(false)

// 表单模式控制
const modelConfigMode = ref<'group' | 'manual'>('manual')
```

#### 2.4 新增的计算属性

```typescript
// 当前选中的模型分组
const selectedModelGroup = computed(() => {
  if (!form.model_group_id) return null
  return modelGroups.value.find(g => g.id === form.model_group_id)
})

// 模型分组中的模型列表（用于预览）
const groupModels = computed(() => {
  return selectedModelGroup.value?.models || []
})
```

#### 2.5 新增的方法

```typescript
// 加载模型分组列表
const loadModelGroups = async () => {
  loadingGroups.value = true
  try {
    const resp = await adminApi.get<{ groups: ModelGroup[] }>('/model-groups')
    modelGroups.value = resp.data.groups
  } catch (err) {
    console.error('加载模型分组失败:', err)
    ElMessage.error('加载模型分组失败')
  } finally {
    loadingGroups.value = false
  }
}

// 获取下游的模型来源显示
const getModelSource = (downstream: DownstreamConfig) => {
  if (downstream.model_group_id) {
    const group = modelGroups.value.find(g => g.id === downstream.model_group_id)
    return {
      type: 'group' as const,
      name: group?.name || '未知分组',
      count: group?.models.length || 0
    }
  }
  return {
    type: 'manual' as const,
    count: downstream.model_allowlist?.length || 0
  }
}
```

### 3. 数据流

#### 3.1 创建/编辑下游的流程

```
用户操作
  ↓
选择"使用模型分组"
  ↓
从下拉列表选择分组
  ↓
form.model_group_id = <选中的分组ID>
form.model_allowlist = undefined  // 清空手动配置
  ↓
提交表单
  ↓
后端保存 DownstreamConfig { model_group_id: Some(uuid), model_allowlist: None }
  ↓
写入数据库 downstreams 表
```

#### 3.2 权限校验流程

```
请求到达
  ↓
提取 downstream_id
  ↓
查询 DownstreamConfig
  ↓
判断 model_group_id 是否存在
  ↓
  [存在] → 查询 model_groups 表获取 models 列表
  |         ↓
  |      [查询成功] → 使用模型分组的模型列表
  |         ↓
  |      [查询失败] → 回退到 model_allowlist
  ↓
  [不存在] → 直接使用 model_allowlist
  ↓
检查请求的模型是否在允许列表中
  ↓
返回结果
```

### 4. 优势

#### 4.1 对比手动配置模型

| 维度 | 手动配置 | 使用模型分组 |
|------|----------|--------------|
| **配置效率** | 需要逐个勾选，如100个模型需100次点击 | 选择一个分组，1次点击 |
| **批量更新** | 需要逐个下游修改 | 修改模型分组，所有关联下游立即生效 |
| **权限一致性** | 容易出现配置不一致 | 分组保证权限一致 |
| **新模型上线** | 需要逐个下游添加 | 在模型分组中添加一次 |
| **审计追踪** | 难以追踪变更历史 | 分组变更有统一记录 |

#### 4.2 实际场景举例

**场景 1: 新模型上线**

- **手动配置**: 需要编辑 20 个下游，每个下游勾选新模型 → 20 次操作
- **模型分组**: 在"基础模型"分组中添加新模型 → 1 次操作，所有使用该分组的下游立即获得权限

**场景 2: 模型下线**

- **手动配置**: 需要编辑 20 个下游，每个下游取消勾选 → 20 次操作
- **模型分组**: 从分组中移除模型 → 1 次操作

**场景 3: 权限审计**

- **手动配置**: 需要逐个查看每个下游的配置
- **模型分组**: 查看分组配置即可，一目了然

### 5. 向后兼容性

#### 5.1 数据兼容

- ✅ 现有的下游配置（使用 `model_allowlist`）完全不受影响
- ✅ 数据库迁移不修改任何现有数据，只添加新列
- ✅ `model_group_id` 字段为 `Option<Uuid>`，默认为 `None`

#### 5.2 代码兼容

- ✅ 后端优先判断 `model_group_id`，不存在时回退到 `model_allowlist`
- ✅ 前端表单支持两种模式，编辑现有下游时自动识别模式
- ✅ API 接口无变化，只是新增可选字段

#### 5.3 回滚安全

如果需要回滚此功能：

1. **后端回滚**:
   ```bash
   git revert <commit-hash>
   cargo build --release
   ```

2. **数据库回滚**:
   ```sql
   BEGIN;
   -- 将使用模型分组的下游迁移回手动配置（可选）
   UPDATE downstreams 
   SET model_allowlist = (
     SELECT COALESCE(json_agg(model), '[]'::json)
     FROM model_groups mg, json_array_elements_text(mg.models) model
     WHERE mg.id = downstreams.model_group_id
   )
   WHERE model_group_id IS NOT NULL;
   
   -- 删除 model_group_id 列
   ALTER TABLE downstreams DROP COLUMN model_group_id;
   COMMIT;
   ```

3. **前端回滚**:
   ```bash
   git revert <commit-hash>
   npm run build
   ```

### 6. 测试计划

#### 6.1 单元测试

- [ ] 测试 `DownstreamConfig` 序列化/反序列化
- [ ] 测试 `model_group_id` 优先级逻辑
- [ ] 测试回退到 `model_allowlist` 的场景

#### 6.2 集成测试

**测试用例 1: 创建使用模型分组的下游**
```
1. 创建一个模型分组 "基础模型"，包含 gpt-4, claude-3-opus
2. 创建一个下游，选择 "基础模型" 分组
3. 验证：
   - downstream.model_group_id 正确
   - downstream.model_allowlist 为空
   - 请求 gpt-4 通过
   - 请求 gpt-3.5-turbo 被拒绝
```

**测试用例 2: 修改模型分组立即生效**
```
1. 下游 A 使用 "基础模型" 分组
2. 在 "基础模型" 中添加 gemini-pro
3. 验证：下游 A 立即可以访问 gemini-pro
```

**测试用例 3: 模型分组被删除后的回退**
```
1. 下游 A 使用 "测试分组"
2. 删除 "测试分组"（数据库 ON DELETE SET NULL）
3. 验证：
   - downstream.model_group_id 变为 null
   - 回退到使用 model_allowlist
```

**测试用例 4: 手动配置转为模型分组**
```
1. 下游 A 使用手动配置，model_allowlist = [gpt-4, gpt-4-turbo]
2. 编辑下游 A，切换到 "使用模型分组"，选择 "基础模型"
3. 验证：
   - downstream.model_group_id 正确
   - downstream.model_allowlist 被清空
```

**测试用例 5: 模型分组转为手动配置**
```
1. 下游 A 使用 "基础模型" 分组
2. 编辑下游 A，切换到 "手动配置模型列表"
3. 验证：
   - downstream.model_group_id 被清空
   - downstream.model_allowlist 正确
```

#### 6.3 前端 UI 测试

- [ ] 表格正确显示模型来源（分组名称 vs 手动配置）
- [ ] 表单模式切换正常
- [ ] 模型分组选择器加载正确
- [ ] 模型预览显示正确
- [ ] 切换模式时字段清空正确

#### 6.4 性能测试

- [ ] 100 个下游使用同一个模型分组，查询性能
- [ ] 模型分组包含 1000 个模型时的性能
- [ ] 模型分组被删除时的数据库性能（ON DELETE SET NULL）

### 7. 部署步骤

#### 7.1 生产环境部署

```bash
# 1. 备份数据库
pg_dump $DATABASE_URL > backup_$(date +%Y%m%d_%H%M%S).sql

# 2. 执行数据库迁移
psql $DATABASE_URL -f migrations/2026-09-05-add-downstream-model-group-id.sql

# 3. 编译后端
cargo build --release

# 4. 编译前端
cd frontend && npm run build

# 5. 重启服务
systemctl restart chat2responses

# 6. 验证部署
curl https://your-domain.com/health
```

#### 7.2 回滚步骤

```bash
# 1. 停止服务
systemctl stop chat2responses

# 2. 回滚代码
git revert <commit-hash>
cargo build --release
cd frontend && npm run build

# 3. 回滚数据库（可选）
psql $DATABASE_URL -c "ALTER TABLE downstreams DROP COLUMN model_group_id;"

# 4. 重启服务
systemctl start chat2responses
```

### 8. 监控与告警

#### 8.1 关键指标

- **模型分组使用率**: 使用模型分组的下游占比
  ```sql
  SELECT 
    COUNT(CASE WHEN model_group_id IS NOT NULL THEN 1 END)::float / COUNT(*) * 100 AS group_usage_percent
  FROM downstreams;
  ```

- **模型分组查询失败率**: 查询模型分组失败导致回退的次数
  ```
  监控日志: "Failed to query model_group"
  ```

#### 8.2 告警规则

- ⚠️ 模型分组查询失败率 > 5%
- ⚠️ 模型分组被频繁删除（1天内 > 5次）

### 9. 已知限制

1. **模型分组删除**: 删除模型分组会将关联的下游的 `model_group_id` 设为 NULL，需要手动重新配置
2. **循环依赖**: 模型分组不支持嵌套（不是限制，是设计决策）
3. **历史记录**: 模型分组的变更历史不会记录在下游的变更日志中

### 10. 未来改进

- [ ] 模型分组变更日志（记录分组内容的修改历史）
- [ ] 下游批量切换到模型分组
- [ ] 模型分组使用情况统计（哪些下游使用了此分组）
- [ ] 模型分组模板（预设常用的分组配置）

---

## 文件清单

### 后端修改
- ✅ `src/state/postgres.rs` - 新增 `model_group_id` 字段
- ✅ `migrations/2026-09-05-add-downstream-model-group-id.sql` - 数据库迁移

### 前端修改
- ✅ `frontend/src/types/admin.ts` - 新增类型定义
- ✅ `frontend/src/views/admin/Downstreams.vue` - UI 改进

### 文档
- ✅ `DOWNSTREAM_MODEL_GROUP_FEATURE.md` - 本文档

---

## 完成状态

- ✅ 后端代码修改
- ✅ 数据库迁移脚本
- ✅ 前端类型定义
- ✅ 前端 UI 实现
- ✅ 编译验证（前端）
- ⏳ 编译验证（后端）- 正在进行中
- ⏳ 数据库迁移执行 - 等待用户确认
- ⏳ 集成测试
- ⏳ 部署到生产环境

---

## 开发者笔记

本功能的核心设计思想是：

1. **优先级明确**: `model_group_id` > `model_allowlist`
2. **优雅降级**: 查询失败时自动回退
3. **向后兼容**: 不破坏现有配置
4. **用户友好**: 前端提供清晰的模式切换

实现时最重要的是保持**数据一致性**和**回退安全性**。
