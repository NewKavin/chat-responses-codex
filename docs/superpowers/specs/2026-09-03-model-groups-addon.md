# 模型分组功能补充设计

**日期**: 2026-09-03  
**基于**: 多 Key 管理功能设计  
**状态**: 待审核

---

## 1. 概述

### 1.1 目标

在多 Key 管理的基础上，添加模型分组功能，允许：
- 管理员定义模型分组（basic/premium/all 等）
- 每个 key 绑定到一个模型分组
- 运行时校验 API 请求的 model 参数是否在 key 的分组白名单内
- 运行时动态修改分组配置，无需重启服务

### 1.2 非目标

- 不支持用户级别的模型限制（所有 key 通过分组管理）
- 不支持按费用或请求次数的配额控制
- 不支持模型别名或映射

---

## 2. 数据模型

### 2.1 数据库 Schema

```sql
-- 模型分组定义表
CREATE TABLE model_groups (
  id TEXT PRIMARY KEY,           -- "basic", "premium", "all"
  name TEXT NOT NULL,            -- "Basic Models"
  description TEXT,              -- "Basic models for development"
  allowed_models JSONB NOT NULL, -- ["gpt-3.5-turbo", "claude-3-haiku"] 或 ["*"]
  created_at TIMESTAMPTZ DEFAULT NOW(),
  updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- 初始数据
INSERT INTO model_groups (id, name, description, allowed_models) VALUES
  ('basic', 'Basic Models', 'Cost-effective models for development and testing', 
   '["gpt-3.5-turbo", "claude-3-haiku"]'),
  ('premium', 'Premium Models', 'Advanced models for production workloads', 
   '["gpt-4", "gpt-4-turbo", "claude-3-opus", "claude-3.5-sonnet", "claude-3-sonnet"]'),
  ('all', 'All Models', 'Unrestricted access to all available models', 
   '["*"]');

-- portal_user_downstreams 添加外键
ALTER TABLE portal_user_downstreams 
ADD COLUMN model_group_id TEXT DEFAULT 'basic' 
REFERENCES model_groups(id) ON DELETE SET DEFAULT;

-- 添加索引
CREATE INDEX idx_portal_user_downstreams_model_group 
ON portal_user_downstreams(model_group_id);
```

**设计决策**：
- `allowed_models` 使用 JSONB 存储，支持灵活查询
- 特殊值 `["*"]` 表示允许所有模型
- 默认分组为 `basic`（最保守）
- `ON DELETE SET DEFAULT` 确保删除分组时 key 不会失效

### 2.2 Rust 结构体

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelGroup {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub allowed_models: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ModelGroup {
    /// 检查模型是否在允许列表中
    pub fn allows_model(&self, model: &str) -> bool {
        self.allowed_models.contains(&"*".to_string()) 
            || self.allowed_models.contains(&model.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct PortalDownstreamBindingWithLabel {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: String,
    pub created_at: i64,
    pub model_group_id: String,  // 新增
}
```

---

## 3. API 设计

### 3.1 Admin API（新增）

#### 3.1.1 列出所有模型分组

```
GET /api/admin/model-groups
```

**响应**：

```json
{
  "groups": [
    {
      "id": "basic",
      "name": "Basic Models",
      "description": "Cost-effective models for development and testing",
      "allowed_models": ["gpt-3.5-turbo", "claude-3-haiku"],
      "created_at": 1725350400,
      "updated_at": 1725350400
    },
    {
      "id": "premium",
      "name": "Premium Models",
      "description": "Advanced models for production workloads",
      "allowed_models": ["gpt-4", "claude-3-opus", "claude-3.5-sonnet"],
      "created_at": 1725350400,
      "updated_at": 1725350400
    }
  ]
}
```

#### 3.1.2 创建模型分组

```
POST /api/admin/model-groups
```

**请求**：

```json
{
  "id": "experimental",
  "name": "Experimental Models",
  "description": "Beta and experimental models",
  "allowed_models": ["gpt-4-turbo-preview", "claude-3-opus-20240229"]
}
```

**响应**：201 Created

**错误码**：
- `400` - id 格式错误（只允许小写字母、数字、连字符）
- `409` - id 已存在

#### 3.1.3 更新模型分组

```
PUT /api/admin/model-groups/:id
```

**请求**：

```json
{
  "name": "Premium Models (Updated)",
  "description": "...",
  "allowed_models": ["gpt-4", "claude-3-opus", "claude-3.5-sonnet", "claude-fable-5"]
}
```

**响应**：204 No Content

**注意**：不允许修改 `id`

#### 3.1.4 删除模型分组

```
DELETE /api/admin/model-groups/:id
```

**响应**：204 No Content

**行为**：
- 所有使用该分组的 key 自动回退到 `basic` 分组（`ON DELETE SET DEFAULT`）
- 不允许删除 `basic` 分组（保护性限制）

### 3.2 Portal API 修改

#### 3.2.1 创建 Key（新增参数）

```
POST /api/portal/keys
```

**请求**：

```json
{
  "label": "Production Key",
  "model_group_id": "premium"  // 新增，可选，默认 "basic"
}
```

**响应**：

```json
{
  "downstream_id": "ds_abc123",
  "label": "Production Key",
  "plaintext_key": "sk-xxx",
  "key_type": "ApiOnly",
  "model_group_id": "premium",
  "created_at": 1725350400
}
```

**错误码**：
- `404` - model_group_id 不存在

#### 3.2.2 列出 Keys（响应增强）

```
GET /api/portal/keys
```

**响应**：

```json
{
  "keys": [
    {
      "downstream_id": "ds_abc123",
      "label": "Production Key",
      "key_type": "LoginEnabled",
      "prefix": "key-",
      "is_default": true,
      "model_group_id": "premium",
      "model_group_name": "Premium Models",
      "created_at": 1725350400
    }
  ],
  "total": 1,
  "limit": 10
}
```

#### 3.2.3 更新 Key 的模型分组（新增）

```
PUT /api/portal/keys/:id/model-group
```

**请求**：

```json
{
  "model_group_id": "all"
}
```

**响应**：204 No Content

---

## 4. 运行时校验

### 4.1 网关拦截

在 `src/server/gateway.rs` 中，所有 `/v1/*` 请求都需要经过模型校验中间件：

```rust
async fn validate_model_access(
    downstream_id: &str,
    requested_model: &str,
    state: &AppState,
) -> Result<(), (StatusCode, Json<Value>)> {
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return Ok(()), // 非 Portal 模式，跳过校验
    };
    
    // 获取 key 的模型分组
    let model_group_id = match portal_store.get_key_model_group(downstream_id).await {
        Ok(id) => id,
        Err(_) => return Ok(()), // key 不存在于 portal_user_downstreams（可能是直接配置的 downstream），跳过
    };
    
    // 获取分组的允许模型列表
    let allowed_models = match portal_store.get_model_group_allowed_models(&model_group_id).await {
        Ok(models) => models,
        Err(_) => {
            // 分组不存在（数据不一致），拒绝请求
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "code": "model_group_not_found",
                        "message": "The model group for this key no longer exists"
                    }
                })),
            ));
        }
    };
    
    // 特殊值 "*" 表示允许所有模型
    if allowed_models.contains(&"*".to_string()) {
        return Ok(());
    }
    
    // 检查请求的模型是否在允许列表中
    if !allowed_models.iter().any(|m| m == requested_model) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": {
                    "code": "model_not_allowed",
                    "message": format!(
                        "Model '{}' is not allowed for this key. Allowed models: {}",
                        requested_model,
                        allowed_models.join(", ")
                    )
                }
            })),
        ));
    }
    
    Ok(())
}

// 在 proxy_request handler 中调用
pub async fn proxy_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // ... 现有的 key 验证逻辑 ...
    
    // 提取 model 参数
    let requested_model = match extract_model_from_body(&body) {
        Ok(model) => model,
        Err(err) => return err.into_response(),
    };
    
    // 模型权限校验
    if let Err(err) = validate_model_access(&downstream_id, &requested_model, &state).await {
        return err.into_response();
    }
    
    // ... 继续代理请求 ...
}
```

### 4.2 模型提取逻辑

```rust
fn extract_model_from_body(body: &Bytes) -> Result<String, (StatusCode, Json<Value>)> {
    let json: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"code": "invalid_json", "message": "Request body is not valid JSON"}})),
            ));
        }
    };
    
    let model = json.get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"code": "missing_model", "message": "Request must include a 'model' field"}})),
            )
        })?;
    
    Ok(model.to_string())
}
```

---

## 5. 前端设计

### 5.1 Admin 页面（新增）

**路由**: `/admin/model-groups`

**权限**: 仅管理员可访问（通过 Portal 用户的 `is_admin` 字段控制）

**功能**：
1. 列出所有模型分组（表格）
2. 创建新分组（对话框）
3. 编辑分组的模型列表（对话框）
4. 删除分组（确认对话框）

**UI 草图**：

```
+-------------------------------------------------------------+
| Model Groups                                    [+ New Group]|
+-------------------------------------------------------------+
| ID       | Name              | Allowed Models           | ... |
|----------|-------------------|--------------------------|-----|
| basic    | Basic Models      | gpt-3.5-turbo, ...       | Edit|
| premium  | Premium Models    | gpt-4, claude-3-opus...  | Edit|
| all      | All Models        | * (all)                  | Edit|
+-------------------------------------------------------------+
```

### 5.2 Key Management 页面修改

**创建 Key 对话框新增字段**：

```vue
<el-form-item label="Model Group">
  <el-select v-model="newKey.model_group_id">
    <el-option 
      v-for="group in modelGroups" 
      :key="group.id" 
      :label="group.name" 
      :value="group.id"
    >
      <div class="group-option">
        <span class="group-name">{{ group.name }}</span>
        <span class="group-models">
          {{ group.allowed_models.includes('*') ? 'All models' : group.allowed_models.join(', ') }}
        </span>
      </div>
    </el-option>
  </el-select>
  <el-text type="info" size="small">
    Determines which models this key can access
  </el-text>
</el-form-item>
```

**KeyCard 组件新增显示**：

```vue
<div class="info-row">
  <span class="info-label">Model Group:</span>
  <el-tag type="primary" size="small">{{ keyInfo.model_group_name }}</el-tag>
</div>
```

**新增操作：修改模型分组**：

```vue
<el-dropdown-item @click="emit('changeModelGroup')">
  <el-icon><Switch /></el-icon>
  Change Model Group
</el-dropdown-item>
```

---

## 6. 向后兼容

### 6.1 Migration 策略

```sql
-- Step 1: 创建 model_groups 表并插入初始数据
CREATE TABLE model_groups (...);
INSERT INTO model_groups ...;

-- Step 2: 添加 model_group_id 列（默认 'basic'）
ALTER TABLE portal_user_downstreams 
ADD COLUMN model_group_id TEXT DEFAULT 'basic' 
REFERENCES model_groups(id) ON DELETE SET DEFAULT;

-- Step 3: 更新现有的 key- 前缀 key 为 'all' 分组（保持旧行为）
UPDATE portal_user_downstreams
SET model_group_id = 'all'
WHERE downstream_id IN (
  SELECT d.id 
  FROM downstreams d 
  WHERE d.plaintext_key LIKE 'key-%'
);
```

**理由**：
- 现有的 `key-` key 在 migration 前没有模型限制
- 设为 `all` 分组保持向后兼容
- 新创建的 key 默认为 `basic` 分组（更安全）

### 6.2 非 Portal key 的处理

对于直接在 `config.toml` 中配置的 downstream（不在 `portal_user_downstreams` 表中）：
- 跳过模型校验（保持现有行为）
- 或配置文件添加 `model_group_id` 字段（可选功能）

---

## 7. 测试计划

### 7.1 单元测试

- [ ] `ModelGroup::allows_model()` 测试（包含 `*` 特殊值）
- [ ] `PortalStore::get_key_model_group()` 测试
- [ ] `PortalStore::get_model_group_allowed_models()` 测试

### 7.2 集成测试

- [ ] 创建 key 指定分组
- [ ] 更新 key 的分组
- [ ] Admin API CRUD 操作
- [ ] 网关拦截：允许的模型通过，不允许的模型被拒绝
- [ ] 删除分组后 key 回退到 `basic`

### 7.3 端到端测试

- [ ] Admin 创建新分组，用户用该分组创建 key，调用 API 验证权限
- [ ] 修改分组的模型列表，验证实时生效（无需重启）
- [ ] 删除分组，验证相关 key 回退到 `basic`

---

## 8. 实施顺序

1. **Task A: 数据库 Migration** - 创建 `model_groups` 表，添加 `model_group_id` 列
2. **Task B: PortalStore 方法** - 新增 6 个方法（CRUD model_groups + 查询）
3. **Task C: Admin API** - 4 个 handler（list/create/update/delete）
4. **Task D: 网关拦截** - 模型校验中间件
5. **Task E: Portal API 修改** - 创建/列出 key 时包含 model_group_id
6. **Task F: 前端 Admin 页面** - ModelGroupManagement.vue
7. **Task G: 前端 Portal 页面修改** - KeyManagement 添加分组选择
8. **Task H: 测试与验证** - 端到端测试

---

## 9. 未来扩展

- **模型别名**：允许定义 `gpt-4-latest` → `gpt-4-turbo` 映射
- **费用控制**：每个分组设置每月费用上限
- **用户级别分组**：允许某些用户创建特殊分组
- **审计日志**：记录模型调用历史（谁用了哪个模型）
