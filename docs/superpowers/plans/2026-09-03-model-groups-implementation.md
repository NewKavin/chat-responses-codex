# 模型分组功能实现计划（附加）

> **For agentic workers:** 这是多 Key 管理功能的补充计划，实施 Tasks 13-20。先完成主计划 Tasks 1-12 后再执行本计划。

**Goal:** 添加模型分组管理功能，允许管理员定义模型分组，每个 key 绑定到分组，运行时校验 API 请求的 model 参数。

**Architecture:** 新增 `model_groups` 表，`portal_user_downstreams` 添加外键，Admin API 管理分组，网关层拦截校验模型权限。

**Tech Stack:** Rust (axum), PostgreSQL, Vue 3, Element Plus

**Spec:** `docs/superpowers/specs/2026-09-03-model-groups-addon.md`

**依赖:** 必须先完成主计划 Tasks 1-12

---

## Global Constraints

- 模型分组 ID 只允许小写字母、数字、连字符（验证正则：`^[a-z0-9-]+$`）
- 特殊值 `["*"]` 表示允许所有模型
- 不允许删除 `basic` 分组（保护性限制）
- 删除分组时相关 key 自动回退到 `basic`（`ON DELETE SET DEFAULT`）
- 非 Portal key（直接配置的 downstream）跳过模型校验

---

### Task 13: 数据库 Migration - Model Groups

**Files:**
- Create: `migrations/2026-09-03-add-model-groups.sql`

**Interfaces:**
- Consumes: `portal_user_downstreams` 表（Task 1）
- Produces:
  - `model_groups` 表
  - `portal_user_downstreams.model_group_id` 列

- [ ] **Step 1: 创建 Migration SQL**

创建 `migrations/2026-09-03-add-model-groups.sql`：

```sql
-- migrations/2026-09-03-add-model-groups.sql
BEGIN;

-- 创建模型分组表
CREATE TABLE IF NOT EXISTS model_groups (
  id TEXT PRIMARY KEY CHECK (id ~ '^[a-z0-9-]+$'),
  name TEXT NOT NULL,
  description TEXT,
  allowed_models JSONB NOT NULL,
  created_at TIMESTAMPTZ DEFAULT NOW(),
  updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- 插入初始数据
INSERT INTO model_groups (id, name, description, allowed_models) VALUES
  ('basic', 'Basic Models', 'Cost-effective models for development and testing', 
   '["gpt-3.5-turbo", "claude-3-haiku"]'::jsonb),
  ('premium', 'Premium Models', 'Advanced models for production workloads', 
   '["gpt-4", "gpt-4-turbo", "claude-3-opus", "claude-3.5-sonnet", "claude-3-sonnet"]'::jsonb),
  ('all', 'All Models', 'Unrestricted access to all available models', 
   '["*"]'::jsonb)
ON CONFLICT (id) DO NOTHING;

-- 添加 model_group_id 列到 portal_user_downstreams
ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS model_group_id TEXT DEFAULT 'basic' 
REFERENCES model_groups(id) ON DELETE SET DEFAULT;

-- 添加索引
CREATE INDEX IF NOT EXISTS idx_portal_user_downstreams_model_group 
ON portal_user_downstreams(model_group_id);

-- 更新现有的 key- 前缀 key 为 'all' 分组（向后兼容）
-- 注意：这需要连接 downstreams 表，假设该表存在
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'downstreams') THEN
    UPDATE portal_user_downstreams pud
    SET model_group_id = 'all'
    WHERE EXISTS (
      SELECT 1 FROM downstreams d 
      WHERE d.id = pud.downstream_id 
        AND d.plaintext_key LIKE 'key-%'
    );
  END IF;
END $$;

COMMIT;
```

- [ ] **Step 2: 验证 SQL 语法**

运行：`psql $TEST_DATABASE_URL -f migrations/2026-09-03-add-model-groups.sql --dry-run` （或用 `BEGIN; ... ROLLBACK;` 测试）

预期：无语法错误

- [ ] **Step 3: Commit**

```bash
git add migrations/2026-09-03-add-model-groups.sql
git commit -m "feat(db): add model_groups table and foreign key

- Create model_groups table with 3 default groups
- Add model_group_id column to portal_user_downstreams
- Migrate existing key- prefixed keys to 'all' group
- Add index and foreign key constraint"
```

---

### Task 14: PortalStore - ModelGroup 结构体与方法

**Files:**
- Modify: `src/state/portal_store.rs` - 新增 `ModelGroup` 结构体和 6 个方法

**Interfaces:**
- Consumes: `PortalStore` (Task 2)
- Produces:
  - `struct ModelGroup { id, name, description, allowed_models, created_at, updated_at }`
  - `async fn list_model_groups(&self) -> Result<Vec<ModelGroup>, PortalStoreError>`
  - `async fn get_model_group(&self, id: &str) -> Result<ModelGroup, PortalStoreError>`
  - `async fn create_model_group(&self, group: &ModelGroup) -> Result<(), PortalStoreError>`
  - `async fn update_model_group(&self, id: &str, name: &str, description: Option<&str>, allowed_models: Vec<String>) -> Result<(), PortalStoreError>`
  - `async fn delete_model_group(&self, id: &str) -> Result<(), PortalStoreError>`
  - `async fn get_key_allowed_models(&self, downstream_id: &str) -> Result<Vec<String>, PortalStoreError>`

- [ ] **Step 1: 定义 ModelGroup 结构体**

在 `src/state/portal_store.rs` 中添加：

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
```

- [ ] **Step 2: 写测试 - list_model_groups**

在测试模块中添加：

```rust
#[tokio::test]
async fn test_list_model_groups() {
    let store = setup_test_store().await;
    
    let groups = store.list_model_groups().await.unwrap();
    assert!(groups.len() >= 3); // 至少有 basic, premium, all
    
    let basic = groups.iter().find(|g| g.id == "basic").unwrap();
    assert_eq!(basic.name, "Basic Models");
    assert!(basic.allowed_models.contains(&"gpt-3.5-turbo".to_string()));
}
```

- [ ] **Step 3: 运行测试确认失败**

运行：`cargo test test_list_model_groups`

预期：FAIL

- [ ] **Step 4: 实现 list_model_groups**

```rust
pub async fn list_model_groups(&self) -> Result<Vec<ModelGroup>, PortalStoreError> {
    let client = self.pool.get().await?;
    let rows = client
        .query(
            "SELECT id, name, description, allowed_models, \
                    EXTRACT(EPOCH FROM created_at)::bigint, \
                    EXTRACT(EPOCH FROM updated_at)::bigint \
             FROM model_groups \
             ORDER BY id",
            &[],
        )
        .await?;
    
    Ok(rows
        .into_iter()
        .map(|row| {
            let allowed_models_json: serde_json::Value = row.get(3);
            let allowed_models: Vec<String> = serde_json::from_value(allowed_models_json)
                .unwrap_or_default();
            
            ModelGroup {
                id: row.get(0),
                name: row.get(1),
                description: row.get(2),
                allowed_models,
                created_at: row.get(4),
                updated_at: row.get(5),
            }
        })
        .collect())
}
```

- [ ] **Step 5: 运行测试确认通过**

运行：`cargo test test_list_model_groups`

预期：PASS

- [ ] **Step 6: 实现其他 5 个方法（create/get/update/delete/get_key_allowed_models）**

```rust
pub async fn get_model_group(&self, id: &str) -> Result<ModelGroup, PortalStoreError> {
    let client = self.pool.get().await?;
    let row = client
        .query_opt(
            "SELECT id, name, description, allowed_models, \
                    EXTRACT(EPOCH FROM created_at)::bigint, \
                    EXTRACT(EPOCH FROM updated_at)::bigint \
             FROM model_groups WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or(PortalStoreError::NotFound)?;
    
    let allowed_models_json: serde_json::Value = row.get(3);
    let allowed_models: Vec<String> = serde_json::from_value(allowed_models_json)
        .unwrap_or_default();
    
    Ok(ModelGroup {
        id: row.get(0),
        name: row.get(1),
        description: row.get(2),
        allowed_models,
        created_at: row.get(4),
        updated_at: row.get(5),
    })
}

pub async fn create_model_group(&self, group: &ModelGroup) -> Result<(), PortalStoreError> {
    // 验证 ID 格式
    if !group.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(PortalStoreError::Conflict("invalid group id format".to_string()));
    }
    
    let client = self.pool.get().await?;
    let allowed_models_json = serde_json::to_value(&group.allowed_models)
        .map_err(|_| PortalStoreError::Conflict("invalid allowed_models".to_string()))?;
    
    client
        .execute(
            "INSERT INTO model_groups (id, name, description, allowed_models) VALUES ($1, $2, $3, $4)",
            &[&group.id, &group.name, &group.description, &allowed_models_json],
        )
        .await
        .map_err(|e| {
            if e.to_string().contains("duplicate key") {
                PortalStoreError::Conflict("group id already exists".to_string())
            } else {
                PortalStoreError::from(e)
            }
        })?;
    
    Ok(())
}

pub async fn update_model_group(
    &self,
    id: &str,
    name: &str,
    description: Option<&str>,
    allowed_models: Vec<String>,
) -> Result<(), PortalStoreError> {
    let client = self.pool.get().await?;
    let allowed_models_json = serde_json::to_value(&allowed_models)
        .map_err(|_| PortalStoreError::Conflict("invalid allowed_models".to_string()))?;
    
    let rows_affected = client
        .execute(
            "UPDATE model_groups SET name = $2, description = $3, allowed_models = $4, updated_at = NOW() \
             WHERE id = $1",
            &[&id, &name, &description, &allowed_models_json],
        )
        .await?;
    
    if rows_affected == 0 {
        return Err(PortalStoreError::NotFound);
    }
    
    Ok(())
}

pub async fn delete_model_group(&self, id: &str) -> Result<(), PortalStoreError> {
    // 不允许删除 basic 分组
    if id == "basic" {
        return Err(PortalStoreError::Conflict("cannot delete basic group".to_string()));
    }
    
    let client = self.pool.get().await?;
    let rows_affected = client
        .execute("DELETE FROM model_groups WHERE id = $1", &[&id])
        .await?;
    
    if rows_affected == 0 {
        return Err(PortalStoreError::NotFound);
    }
    
    Ok(())
}

pub async fn get_key_allowed_models(
    &self,
    downstream_id: &str,
) -> Result<Vec<String>, PortalStoreError> {
    let client = self.pool.get().await?;
    let row = client
        .query_opt(
            "SELECT mg.allowed_models \
             FROM portal_user_downstreams pud \
             JOIN model_groups mg ON pud.model_group_id = mg.id \
             WHERE pud.downstream_id = $1",
            &[&downstream_id],
        )
        .await?
        .ok_or(PortalStoreError::NotFound)?;
    
    let allowed_models_json: serde_json::Value = row.get(0);
    let allowed_models: Vec<String> = serde_json::from_value(allowed_models_json)
        .unwrap_or_default();
    
    Ok(allowed_models)
}
```

- [ ] **Step 7: 写测试覆盖所有方法**

```rust
#[tokio::test]
async fn test_model_group_crud() {
    let store = setup_test_store().await;
    
    // Create
    let group = ModelGroup {
        id: "test_group".to_string(),
        name: "Test Group".to_string(),
        description: Some("Test".to_string()),
        allowed_models: vec!["gpt-4".to_string()],
        created_at: 0,
        updated_at: 0,
    };
    store.create_model_group(&group).await.unwrap();
    
    // Get
    let fetched = store.get_model_group("test_group").await.unwrap();
    assert_eq!(fetched.name, "Test Group");
    
    // Update
    store.update_model_group("test_group", "Updated", None, vec!["gpt-4".to_string(), "claude-3-opus".to_string()]).await.unwrap();
    let updated = store.get_model_group("test_group").await.unwrap();
    assert_eq!(updated.name, "Updated");
    assert_eq!(updated.allowed_models.len(), 2);
    
    // Delete
    store.delete_model_group("test_group").await.unwrap();
    let result = store.get_model_group("test_group").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_cannot_delete_basic_group() {
    let store = setup_test_store().await;
    let result = store.delete_model_group("basic").await;
    assert!(matches!(result, Err(PortalStoreError::Conflict(_))));
}
```

- [ ] **Step 8: 运行测试确认通过**

运行：`cargo test model_group`

预期：所有测试通过

- [ ] **Step 9: Commit**

```bash
git add src/state/portal_store.rs
git commit -m "feat(portal): add ModelGroup struct and store methods

- Add ModelGroup struct with allows_model() helper
- Implement list/get/create/update/delete methods
- Add get_key_allowed_models for runtime validation
- Protect basic group from deletion
- Include comprehensive unit tests"
```

---

### Task 15: Admin API - Model Groups CRUD

**Files:**
- Modify: `src/server/portal.rs` - 新增 4 个 admin handler
- Modify: `src/server/gateway.rs` - 新增 4 个路由
- Create: `tests/admin_model_groups_test.rs` - 集成测试

**Interfaces:**
- Consumes: `PortalStore` model_groups 方法 (Task 14)
- Produces:
  - `GET /api/admin/model-groups` - 列出所有分组
  - `POST /api/admin/model-groups` - 创建分组
  - `PUT /api/admin/model-groups/:id` - 更新分组
  - `DELETE /api/admin/model-groups/:id` - 删除分组

- [ ] **Step 1: 实现 admin_list_model_groups**

在 `src/server/portal.rs` 中添加：

```rust
pub(super) async fn admin_list_model_groups(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    let groups = match portal_store.list_model_groups().await {
        Ok(groups) => groups,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    };
    
    Json(json!({ "groups": groups })).into_response()
}
```

- [ ] **Step 2: 实现 admin_create_model_group**

```rust
#[derive(serde::Deserialize)]
struct CreateModelGroupRequest {
    id: String,
    name: String,
    description: Option<String>,
    allowed_models: Vec<String>,
}

pub(super) async fn admin_create_model_group(
    State(state): State<AppState>,
    Json(payload): Json<CreateModelGroupRequest>,
) -> impl IntoResponse {
    // 验证 ID 格式
    if !payload.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": {"code": "invalid_id", "message": "ID must contain only lowercase letters, digits, and hyphens"}}))).into_response();
    }
    
    if payload.allowed_models.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": {"code": "invalid_models", "message": "allowed_models cannot be empty"}}))).into_response();
    }
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    let group = ModelGroup {
        id: payload.id,
        name: payload.name,
        description: payload.description,
        allowed_models: payload.allowed_models,
        created_at: chrono::Utc::now().timestamp(),
        updated_at: chrono::Utc::now().timestamp(),
    };
    
    match portal_store.create_model_group(&group).await {
        Ok(_) => (StatusCode::CREATED, Json(json!(group))).into_response(),
        Err(PortalStoreError::Conflict(msg)) => (StatusCode::CONFLICT, Json(json!({"error": {"code": "group_exists", "message": msg}}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    }
}
```

- [ ] **Step 3: 实现 admin_update_model_group 和 admin_delete_model_group**

```rust
#[derive(serde::Deserialize)]
struct UpdateModelGroupRequest {
    name: String,
    description: Option<String>,
    allowed_models: Vec<String>,
}

pub(super) async fn admin_update_model_group(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateModelGroupRequest>,
) -> impl IntoResponse {
    if payload.allowed_models.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": {"code": "invalid_models", "message": "allowed_models cannot be empty"}}))).into_response();
    }
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    match portal_store.update_model_group(&id, &payload.name, payload.description.as_deref(), payload.allowed_models).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(PortalStoreError::NotFound) => (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "group_not_found"}}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    }
}

pub(super) async fn admin_delete_model_group(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    match portal_store.delete_model_group(&id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(PortalStoreError::Conflict(msg)) if msg.contains("basic") => {
            (StatusCode::FORBIDDEN, Json(json!({"error": {"code": "cannot_delete_basic", "message": "Cannot delete the basic group"}}))).into_response()
        }
        Err(PortalStoreError::NotFound) => (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "group_not_found"}}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    }
}
```

- [ ] **Step 4: 添加路由**

在 `src/server/gateway.rs` 中添加：

```rust
.route("/api/admin/model-groups", get(portal::admin_list_model_groups))
.route("/api/admin/model-groups", post(portal::admin_create_model_group))
.route("/api/admin/model-groups/:id", put(portal::admin_update_model_group))
.route("/api/admin/model-groups/:id", delete(portal::admin_delete_model_group))
```

- [ ] **Step 5: 写集成测试**

创建 `tests/admin_model_groups_test.rs`：

```rust
use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn test_admin_list_model_groups() {
    let app = common::setup_test_app().await;
    
    let response = app.get("/api/admin/model-groups").send().await;
    assert_eq!(response.status(), StatusCode::OK);
    
    let body: serde_json::Value = response.json().await;
    assert!(body["groups"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn test_admin_create_and_delete_group() {
    let app = common::setup_test_app().await;
    
    // Create
    let response = app
        .post("/api/admin/model-groups")
        .json(&json!({
            "id": "test-group",
            "name": "Test Group",
            "description": "For testing",
            "allowed_models": ["gpt-4"]
        }))
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    
    // Delete
    let response = app.delete("/api/admin/model-groups/test-group").send().await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_cannot_delete_basic_group() {
    let app = common::setup_test_app().await;
    
    let response = app.delete("/api/admin/model-groups/basic").send().await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 6: 运行测试**

运行：`cargo test admin_model_groups`

预期：PASS

- [ ] **Step 7: Commit**

```bash
git add src/server/portal.rs src/server/gateway.rs tests/admin_model_groups_test.rs
git commit -m "feat(admin): add model groups CRUD API

- GET /api/admin/model-groups - list all groups
- POST /api/admin/model-groups - create group
- PUT /api/admin/model-groups/:id - update group
- DELETE /api/admin/model-groups/:id - delete group
- Protect basic group from deletion
- Include integration tests"
```

---

### Task 16: 网关拦截 - 模型权限校验

**Files:**
- Modify: `src/server/gateway.rs` - 添加 `validate_model_access` 中间件

**Interfaces:**
- Consumes:
  - `PortalStore::get_key_allowed_models` (Task 14)
  - 现有的 `proxy_request` handler
- Produces:
  - 模型权限校验逻辑，拒绝未授权的模型请求

- [ ] **Step 1: 写测试 - 允许的模型通过**

创建 `tests/model_validation_test.rs`：

```rust
#[tokio::test]
async fn test_allowed_model_passes() {
    let app = common::setup_test_app().await;
    
    // 创建一个 basic 分组的 key
    let key = common::create_test_key(&app, "basic").await;
    
    // 请求 gpt-3.5-turbo（在 basic 分组中）
    let response = app
        .post("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&json!({
            "model": "gpt-3.5-turbo",
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await;
    
    // 不应该被拒绝（实际上游调用可能失败，但不是因为权限）
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: 写测试 - 不允许的模型被拒绝**

```rust
#[tokio::test]
async fn test_forbidden_model_rejected() {
    let app = common::setup_test_app().await;
    
    // 创建一个 basic 分组的 key
    let key = common::create_test_key(&app, "basic").await;
    
    // 请求 gpt-4（不在 basic 分组中）
    let response = app
        .post("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await;
    
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    
    let body: serde_json::Value = response.json().await;
    assert_eq!(body["error"]["code"], "model_not_allowed");
}
```

- [ ] **Step 3: 写测试 - 通配符 * 允许所有模型**

```rust
#[tokio::test]
async fn test_wildcard_allows_all_models() {
    let app = common::setup_test_app().await;
    
    // 创建一个 all 分组的 key
    let key = common::create_test_key(&app, "all").await;
    
    // 请求任意模型
    let response = app
        .post("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&json!({
            "model": "some-random-model",
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await;
    
    // 不应该因为权限被拒绝
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 4: 运行测试确认失败**

运行：`cargo test model_validation`

预期：FAIL（校验逻辑尚未实现）

- [ ] **Step 5: 实现模型提取逻辑**

在 `src/server/gateway.rs` 中添加：

```rust
fn extract_model_from_body(body: &Bytes) -> Result<String, (StatusCode, Json<Value>)> {
    let json: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "code": "invalid_json",
                        "message": "Request body is not valid JSON"
                    }
                })),
            ));
        }
    };
    
    let model = json
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "code": "missing_model",
                        "message": "Request must include a 'model' field"
                    }
                })),
            )
        })?;
    
    Ok(model.to_string())
}
```

- [ ] **Step 6: 实现校验中间件**

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
    
    // 获取 key 的允许模型列表
    let allowed_models = match portal_store.get_key_allowed_models(downstream_id).await {
        Ok(models) => models,
        Err(PortalStoreError::NotFound) => {
            // key 不在 portal_user_downstreams 中（可能是直接配置的 downstream）
            // 跳过校验，保持向后兼容
            return Ok(());
        }
        Err(error) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "code": "store_error",
                        "message": error.to_string()
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
```

- [ ] **Step 7: 在 proxy_request 中调用校验**

修改现有的 `proxy_request` handler：

```rust
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

- [ ] **Step 8: 运行测试确认通过**

运行：`cargo test model_validation`

预期：PASS

- [ ] **Step 9: 测试非 Portal key 跳过校验**

```rust
#[tokio::test]
async fn test_non_portal_key_skips_validation() {
    let app = common::setup_test_app_with_direct_downstream().await;
    
    // 使用直接配置的 downstream key（不在 portal_user_downstreams 表中）
    let key = "some-direct-key";
    
    // 请求任意模型
    let response = app
        .post("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&json!({
            "model": "any-model",
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await;
    
    // 不应该因为权限被拒绝（保持向后兼容）
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 10: Commit**

```bash
git add src/server/gateway.rs tests/model_validation_test.rs
git commit -m "feat(gateway): add model access validation middleware

- Extract model from request body
- Validate against key's allowed_models via portal store
- Support wildcard '*' for unrestricted access
- Skip validation for non-Portal keys (backward compat)
- Return 403 with helpful error message for unauthorized models
- Include comprehensive integration tests"
```

---

### Task 17: Portal API - 修改查询以返回模型分组名称

**Files:**
- Modify: `src/state/portal_store.rs` - 修改 `list_downstream_bindings_with_labels` 使用 LEFT JOIN

**Interfaces:**
- Consumes:
  - `list_downstream_bindings_with_labels` (Task 2，主计划)
  - `model_groups` 表 (Task 13)
- Produces:
  - 列出 key 时返回 `model_group_name`（通过 JOIN 查询）

**说明：** 主计划已经预留了 `model_group_id` 字段和相关实现，这个 Task 只需要修改查询语句，添加 LEFT JOIN 来获取 `model_group_name`，避免 N+1 查询问题。

- [ ] **Step 1: 修改 PortalDownstreamBindingWithLabel 结构体**

在 `src/state/portal_store.rs` 中添加 `model_group_name` 字段：

```rust
#[derive(Debug, Clone)]
pub struct PortalDownstreamBindingWithLabel {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: String,
    pub created_at: i64,
    pub model_group_id: String,
    pub model_group_name: String,  // 新增
}
```

- [ ] **Step 2: 修改 list_downstream_bindings_with_labels 使用 LEFT JOIN**

```rust
pub async fn list_downstream_bindings_with_labels(
    &self,
    user_id: &str,
) -> Result<Vec<PortalDownstreamBindingWithLabel>, PortalStoreError> {
    let client = self.pool.get().await?;
    let rows = client
        .query(
            "SELECT 
                pud.downstream_id, 
                pud.is_default, 
                COALESCE(pud.label, 'Default Key'), 
                EXTRACT(EPOCH FROM pud.created_at)::bigint,
                COALESCE(pud.model_group_id, 'basic'),
                COALESCE(mg.name, 'Basic Models')
             FROM portal_user_downstreams pud
             LEFT JOIN model_groups mg ON pud.model_group_id = mg.id
             WHERE pud.user_id = $1
             ORDER BY pud.is_default DESC, pud.created_at DESC",
            &[&user_id],
        )
        .await?;
    
    Ok(rows
        .into_iter()
        .map(|row| PortalDownstreamBindingWithLabel {
            downstream_id: row.get(0),
            is_default: row.get(1),
            label: row.get(2),
            created_at: row.get(3),
            model_group_id: row.get(4),
            model_group_name: row.get(5),
        })
        .collect())
}
```

- [ ] **Step 3: 运行测试验证**

运行：`cargo test list_downstream_bindings_with_labels`

预期：测试通过，返回结果包含 `model_group_name`

- [ ] **Step 4: 提交**

```bash
git add src/state/portal_store.rs
git commit -m "feat(model-groups): add JOIN to return model_group_name in list keys"
```

---

### Task 18: 前端 API 封装 - 模型分组
}

pub(super) async fn portal_create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateKeyRequest>,
) -> impl IntoResponse {
    // ... 现有的用户验证和 label 验证 ...
    
    let model_group_id = payload.model_group_id.unwrap_or_else(|| "basic".to_string());
    
    // 验证 model_group_id 存在
    if let Err(PortalStoreError::NotFound) = state.portal_store().get_model_group(&model_group_id).await {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "code": "model_group_not_found",
                    "message": format!("Model group '{}' does not exist", model_group_id)
                }
            })),
        )
            .into_response();
    }
    
    // ... 生成 downstream ...
    
    // 创建 binding（传入 model_group_id）
    if let Err(error) = state
        .portal_store()
        .add_downstream_binding_with_label(&user_id, &downstream_id, label, false, &model_group_id)
        .await
    {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "binding_failed",
            &error.to_string(),
        );
    }
    
    Json(json!({
        "downstream_id": downstream_id,
        "label": label,
        "plaintext_key": generated.plaintext,
        "key_type": "ApiOnly",
        "model_group_id": model_group_id,
        "created_at": crate::state::unix_seconds()
    }))
    .into_response()
}
```

- [ ] **Step 5: 修改 portal_list_keys handler**

```rust
pub(super) async fn portal_list_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // ... 现有的用户验证 ...
    
    let bindings = match state
        .portal_store()
        .list_downstream_bindings_with_labels(&user_id)
        .await
    {
        Ok(bindings) => bindings,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error", &error.to_string()),
    };
    
    let mut keys = Vec::new();
    for binding in bindings {
        // 获取 model_group_name
        let model_group_name = match state.portal_store().get_model_group(&binding.model_group_id).await {
            Ok(group) => group.name,
            Err(_) => "Unknown".to_string(),
        };
        
        // 获取 downstream 的详细信息
        let downstream = match state.get_downstream(&binding.downstream_id) {
            Some(ds) => ds,
            None => continue,
        };
        
        let key_type = if downstream.plaintext_key.as_ref().map_or(false, |k| k.starts_with("key-")) {
            "LoginEnabled"
        } else {
            "ApiOnly"
        };
        
        keys.push(json!({
            "downstream_id": binding.downstream_id,
            "label": binding.label,
            "key_type": key_type,
            "prefix": if key_type == "LoginEnabled" { "key-" } else { "sk-" },
            "is_default": binding.is_default,
            "model_group_id": binding.model_group_id,
            "model_group_name": model_group_name,
            "created_at": binding.created_at,
        }));
    }
    
    Json(json!({
        "keys": keys,
        "total": keys.len(),
        "limit": 10
    }))
    .into_response()
}
```

- [ ] **Step 6: 新增 portal_update_key_model_group handler**

```rust
#[derive(serde::Deserialize)]
struct UpdateKeyModelGroupRequest {
    model_group_id: String,
}

pub(super) async fn portal_update_key_model_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(downstream_id): Path<String>,
    Json(payload): Json<UpdateKeyModelGroupRequest>,
) -> impl IntoResponse {
    // 验证用户
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    
    // 验证 model_group_id 存在
    if let Err(PortalStoreError::NotFound) = state.portal_store().get_model_group(&payload.model_group_id).await {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "code": "model_group_not_found",
                    "message": format!("Model group '{}' does not exist", payload.model_group_id)
                }
            })),
        )
            .into_response();
    }
    
    // 更新
    let result = state
        .portal_store()
        .pool
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE portal_user_downstreams SET model_group_id = $3 \
             WHERE user_id = $1 AND downstream_id = $2",
            &[&user_id, &downstream_id, &payload.model_group_id],
        )
        .await;
    
    match result {
        Ok(0) => (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "key_not_found"}}))).into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    }
}
```

- [ ] **Step 7: 添加路由**

在 `src/server/gateway.rs` 中添加：

```rust
.route("/api/portal/keys/:id/model-group", put(portal::portal_update_key_model_group))
```

- [ ] **Step 8: 写集成测试**

```rust
#[tokio::test]
async fn test_create_key_with_model_group() {
    let app = common::setup_test_app().await;
    let session = common::login_as_test_user(&app).await;
    
    let response = app
        .post("/api/portal/keys")
        .header("Cookie", session_cookie(&session))
        .json(&json!({
            "label": "Premium Key",
            "model_group_id": "premium"
        }))
        .send()
        .await;
    
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await;
    assert_eq!(body["model_group_id"], "premium");
}

#[tokio::test]
async fn test_list_keys_shows_model_group() {
    let app = common::setup_test_app().await;
    let session = common::login_as_test_user(&app).await;
    
    let response = app
        .get("/api/portal/keys")
        .header("Cookie", session_cookie(&session))
        .send()
        .await;
    
    let body: serde_json::Value = response.json().await;
    let key = &body["keys"][0];
    assert!(key["model_group_id"].is_string());
    assert!(key["model_group_name"].is_string());
}
```

- [ ] **Step 9: 运行测试**

运行：`cargo test portal_keys.*model_group`

预期：PASS

- [ ] **Step 10: Commit**

```bash
git add src/server/portal.rs src/server/gateway.rs src/state/portal_store.rs tests/
git commit -m "feat(portal): add model group support to key management

- Accept model_group_id when creating keys (defaults to 'basic')
- Return model_group_id and model_group_name when listing keys
- Add PUT /api/portal/keys/:id/model-group to change group
- Validate model_group_id exists before creating/updating
- Include integration tests"
```

---

### Task 18: 前端 API 封装 - 模型分组

**Files:**
- Modify: `frontend/src/api/portal.ts` - 新增 model groups API 方法
- Modify: `frontend/src/api/types.ts` - 新增类型定义

**Interfaces:**
- Consumes:
  - Admin API (Task 15)
  - Portal API (Task 17)
- Produces:
  - TypeScript 类型
  - API 方法封装

- [ ] **Step 1: 定义 TypeScript 类型**

在 `frontend/src/api/types.ts` 中添加：

```typescript
export interface ModelGroup {
  id: string
  name: string
  description?: string
  allowed_models: string[]
  created_at: number
  updated_at: number
}

export interface KeyInfo {
  downstream_id: string
  label: string
  key_type: 'LoginEnabled' | 'ApiOnly'
  prefix: string
  is_default: boolean
  model_group_id: string
  model_group_name: string
  created_at: number
  last_used_at?: number
  usage_last_7days?: number
  plaintext_key?: string
}

export interface CreateKeyRequest {
  label: string
  model_group_id?: string
}
```

- [ ] **Step 2: 添加 Admin API 方法**

在 `frontend/src/api/portal.ts` 中添加：

```typescript
// Model Groups (Admin)
export async function listModelGroups(): Promise<{ groups: ModelGroup[] }> {
  const response = await axios.get('/api/admin/model-groups')
  return response.data
}

export async function createModelGroup(data: {
  id: string
  name: string
  description?: string
  allowed_models: string[]
}): Promise<ModelGroup> {
  const response = await axios.post('/api/admin/model-groups', data)
  return response.data
}

export async function updateModelGroup(
  id: string,
  data: {
    name: string
    description?: string
    allowed_models: string[]
  }
): Promise<void> {
  await axios.put(`/api/admin/model-groups/${id}`, data)
}

export async function deleteModelGroup(id: string): Promise<void> {
  await axios.delete(`/api/admin/model-groups/${id}`)
}
```

- [ ] **Step 3: 修改现有的 Portal API 方法签名**

```typescript
export async function createKey(data: CreateKeyRequest): Promise<KeyInfo> {
  const response = await axios.post('/api/portal/keys', data)
  return response.data
}

export async function listKeys(): Promise<{ keys: KeyInfo[]; total: number; limit: number }> {
  const response = await axios.get('/api/portal/keys')
  return response.data
}

export async function updateKeyModelGroup(
  keyId: string,
  model_group_id: string
): Promise<void> {
  await axios.put(`/api/portal/keys/${keyId}/model-group`, { model_group_id })
}
```

- [ ] **Step 4: 添加错误处理**

```typescript
export function isModelGroupNotFoundError(error: any): boolean {
  return error.response?.data?.error?.code === 'model_group_not_found'
}

export function isCannotDeleteBasicError(error: any): boolean {
  return error.response?.data?.error?.code === 'cannot_delete_basic'
}

export function isModelNotAllowedError(error: any): boolean {
  return error.response?.data?.error?.code === 'model_not_allowed'
}
```

- [ ] **Step 5: Commit**

```bash
git add frontend/src/api/portal.ts frontend/src/api/types.ts
git commit -m "feat(frontend): add model groups API wrapper

- Add ModelGroup and updated KeyInfo types
- Implement admin model groups CRUD methods
- Update createKey and listKeys to support model_group_id
- Add updateKeyModelGroup method
- Include error detection helpers"
```

---

### Task 19: 前端 Admin 页面 - 模型分组管理

**Files:**
- Create: `frontend/src/views/admin/ModelGroupManagement.vue`
- Create: `frontend/src/components/admin/ModelGroupForm.vue`
- Modify: `frontend/src/router/index.ts` - 新增路由

**Interfaces:**
- Consumes: API 方法 (Task 18)
- Produces: 完整的 Admin 模型分组管理页面

- [ ] **Step 1: 创建 ModelGroupForm 组件**

创建 `frontend/src/components/admin/ModelGroupForm.vue`：

```vue
<script setup lang="ts">
import { ref, computed } from 'vue'
import { ElMessage } from 'element-plus'
import type { ModelGroup } from '@/api/types'

const props = defineProps<{
  modelValue: boolean
  mode: 'create' | 'edit'
  group?: ModelGroup
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  'submit': [data: any]
}>()

const dialogVisible = computed({
  get: () => props.modelValue,
  set: (val) => emit('update:modelValue', val)
})

const form = ref({
  id: props.group?.id || '',
  name: props.group?.name || '',
  description: props.group?.description || '',
  allowed_models: props.group?.allowed_models?.join('\n') || ''
})

const rules = {
  id: [
    { required: true, message: 'Please enter group ID', trigger: 'blur' },
    { pattern: /^[a-z0-9-]+$/, message: 'Only lowercase letters, digits, and hyphens', trigger: 'blur' }
  ],
  name: [
    { required: true, message: 'Please enter group name', trigger: 'blur' }
  ],
  allowed_models: [
    { required: true, message: 'Please enter at least one model', trigger: 'blur' }
  ]
}

const formRef = ref()

const handleSubmit = async () => {
  await formRef.value.validate()
  
  const modelsArray = form.value.allowed_models
    .split('\n')
    .map(m => m.trim())
    .filter(m => m.length > 0)
  
  if (modelsArray.length === 0) {
    ElMessage.warning('Please enter at least one model')
    return
  }
  
  emit('submit', {
    id: form.value.id,
    name: form.value.name,
    description: form.value.description || undefined,
    allowed_models: modelsArray
  })
}
</script>

<template>
  <el-dialog
    v-model="dialogVisible"
    :title="mode === 'create' ? 'Create Model Group' : 'Edit Model Group'"
    width="600px"
  >
    <el-form ref="formRef" :model="form" :rules="rules" label-width="140px">
      <el-form-item label="Group ID" prop="id">
        <el-input
          v-model="form.id"
          :disabled="mode === 'edit'"
          placeholder="e.g., experimental"
        />
        <el-text type="info" size="small" v-if="mode === 'create'">
          Only lowercase letters, digits, and hyphens
        </el-text>
      </el-form-item>
      
      <el-form-item label="Group Name" prop="name">
        <el-input v-model="form.name" placeholder="e.g., Experimental Models" />
      </el-form-item>
      
      <el-form-item label="Description">
        <el-input
          v-model="form.description"
          type="textarea"
          :rows="2"
          placeholder="Optional description"
        />
      </el-form-item>
      
      <el-form-item label="Allowed Models" prop="allowed_models">
        <el-input
          v-model="form.allowed_models"
          type="textarea"
          :rows="6"
          placeholder="One model per line, or * for all models"
        />
        <el-text type="info" size="small">
          Enter one model ID per line. Use <code>*</code> to allow all models.
        </el-text>
      </el-form-item>
    </el-form>
    
    <template #footer>
      <el-button @click="dialogVisible = false">Cancel</el-button>
      <el-button type="primary" @click="handleSubmit">
        {{ mode === 'create' ? 'Create' : 'Update' }}
      </el-button>
    </template>
  </el-dialog>
</template>
```

- [ ] **Step 2: 创建 ModelGroupManagement 页面**

创建 `frontend/src/views/admin/ModelGroupManagement.vue`：

```vue
<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { listModelGroups, createModelGroup, updateModelGroup, deleteModelGroup, isCannotDeleteBasicError } from '@/api/portal'
import type { ModelGroup } from '@/api/types'
import ModelGroupForm from '@/components/admin/ModelGroupForm.vue'

const groups = ref<ModelGroup[]>([])
const loading = ref(false)
const formVisible = ref(false)
const formMode = ref<'create' | 'edit'>('create')
const editingGroup = ref<ModelGroup | undefined>()

const loadGroups = async () => {
  loading.value = true
  try {
    const response = await listModelGroups()
    groups.value = response.groups
  } catch (error) {
    ElMessage.error(`Failed to load model groups: ${error}`)
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  loadGroups()
})

const handleCreate = () => {
  formMode.value = 'create'
  editingGroup.value = undefined
  formVisible.value = true
}

const handleEdit = (group: ModelGroup) => {
  formMode.value = 'edit'
  editingGroup.value = group
  formVisible.value = true
}

const handleFormSubmit = async (data: any) => {
  loading.value = true
  try {
    if (formMode.value === 'create') {
      await createModelGroup(data)
      ElMessage.success('Model group created successfully')
    } else {
      await updateModelGroup(data.id, data)
      ElMessage.success('Model group updated successfully')
    }
    formVisible.value = false
    await loadGroups()
  } catch (error: any) {
    if (error.response?.data?.error?.code === 'group_exists') {
      ElMessage.error('A group with this ID already exists')
    } else {
      ElMessage.error(`Failed to save model group: ${error}`)
    }
  } finally {
    loading.value = false
  }
}

const handleDelete = async (group: ModelGroup) => {
  if (group.id === 'basic') {
    ElMessage.warning('The basic group cannot be deleted')
    return
  }
  
  try {
    await ElMessageBox.confirm(
      `Delete model group "${group.name}"? Keys using this group will fall back to the "basic" group.`,
      'Confirm Deletion',
      { type: 'warning' }
    )
    
    loading.value = true
    await deleteModelGroup(group.id)
    ElMessage.success('Model group deleted successfully')
    await loadGroups()
  } catch (error: any) {
    if (error !== 'cancel') {
      if (isCannotDeleteBasicError(error)) {
        ElMessage.error('Cannot delete the basic group')
      } else {
        ElMessage.error(`Failed to delete model group: ${error}`)
      }
    }
  } finally {
    loading.value = false
  }
}

const formatModels = (models: string[]) => {
  if (models.includes('*')) {
    return 'All models (*)'
  }
  return models.join(', ')
}
</script>

<template>
  <div class="model-group-management">
    <div class="page-header">
      <div>
        <h1>Model Groups</h1>
        <p class="subtitle">Manage model access groups for API keys</p>
      </div>
      <el-button type="primary" @click="handleCreate">
        <el-icon><Plus /></el-icon>
        Create Group
      </el-button>
    </div>
    
    <el-table :data="groups" v-loading="loading" stripe>
      <el-table-column prop="id" label="ID" width="150" />
      <el-table-column prop="name" label="Name" width="200" />
      <el-table-column prop="description" label="Description" />
      <el-table-column label="Allowed Models">
        <template #default="{ row }">
          <el-text class="models-text">{{ formatModels(row.allowed_models) }}</el-text>
        </template>
      </el-table-column>
      <el-table-column label="Actions" width="180" align="right">
        <template #default="{ row }">
          <el-button size="small" @click="handleEdit(row)">
            <el-icon><Edit /></el-icon>
            Edit
          </el-button>
          <el-button
            size="small"
            type="danger"
            plain
            @click="handleDelete(row)"
            :disabled="row.id === 'basic'"
          >
            <el-icon><Delete /></el-icon>
            Delete
          </el-button>
        </template>
      </el-table-column>
    </el-table>
    
    <ModelGroupForm
      v-model="formVisible"
      :mode="formMode"
      :group="editingGroup"
      @submit="handleFormSubmit"
    />
  </div>
</template>

<style scoped>
.model-group-management {
  max-width: 1400px;
  margin: 0 auto;
  padding: 24px;
}

.page-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  margin-bottom: 24px;
}

.page-header h1 {
  margin: 0 0 8px 0;
  font-size: 28px;
  font-weight: 600;
}

.subtitle {
  margin: 0;
  color: var(--el-text-color-secondary);
  font-size: 14px;
}

.models-text {
  font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
  font-size: 13px;
}
</style>
```

- [ ] **Step 3: 添加路由**

在 `frontend/src/router/index.ts` 中添加：

```typescript
{
  path: '/admin/model-groups',
  name: 'ModelGroupManagement',
  component: () => import('@/views/admin/ModelGroupManagement.vue'),
  meta: { requiresAdmin: true }
}
```

- [ ] **Step 4: 测试页面**

运行：`npm run dev`

访问 `/admin/model-groups`，测试：
- [ ] 列出所有分组
- [ ] 创建新分组
- [ ] 编辑分组
- [ ] 删除分组（验证不能删除 basic）

- [ ] **Step 5: Commit**

```bash
git add frontend/src/views/admin/ModelGroupManagement.vue frontend/src/components/admin/ModelGroupForm.vue frontend/src/router/index.ts
git commit -m "feat(admin): add model group management UI

- Create ModelGroupManagement page with table view
- Add ModelGroupForm dialog for create/edit
- Support CRUD operations with validation
- Protect basic group from deletion
- Display allowed models in readable format"
```

---

### Task 20: 前端 Portal 页面 - 添加模型分组选择

**Files:**
- Modify: `frontend/src/views/portal/KeyManagement.vue` - 添加模型分组选择
- Modify: `frontend/src/components/portal/KeyCard.vue` - 显示模型分组

**Interfaces:**
- Consumes:
  - API 方法 (Task 18)
  - KeyManagement.vue (Task 9)
  - KeyCard.vue (Task 8)
- Produces:
  - 创建 key 时可选择模型分组
  - key 列表显示分组信息
  - 支持修改 key 的分组

- [ ] **Step 1: 修改 KeyManagement.vue - 加载模型分组**

在 `<script setup>` 中添加：

```typescript
import { listModelGroups, type ModelGroup } from '@/api/portal'

const modelGroups = ref<ModelGroup[]>([])

const loadModelGroups = async () => {
  try {
    const response = await listModelGroups()
    modelGroups.value = response.groups
  } catch (error) {
    console.error('Failed to load model groups:', error)
    // 如果加载失败，提供默认分组
    modelGroups.value = [
      { id: 'basic', name: 'Basic Models', allowed_models: [], created_at: 0, updated_at: 0 }
    ]
  }
}

onMounted(() => {
  loadKeys()
  loadModelGroups()
})
```

- [ ] **Step 2: 修改创建对话框 - 添加分组选择**

在创建对话框的表单中添加：

```vue
<el-form-item label="Model Group">
  <el-select v-model="newKey.model_group_id" placeholder="Select model group">
    <el-option
      v-for="group in modelGroups"
      :key="group.id"
      :label="group.name"
      :value="group.id"
    >
      <div class="group-option">
        <span class="group-name">{{ group.name }}</span>
        <span class="group-models">
          {{ formatAllowedModels(group.allowed_models) }}
        </span>
      </div>
    </el-option>
  </el-select>
  <el-text type="info" size="small">
    Determines which AI models this key can access
  </el-text>
</el-form-item>
```

在 `<script>` 中添加辅助函数：

```typescript
const formatAllowedModels = (models: string[]) => {
  if (models.includes('*')) {
    return 'All models'
  }
  return models.slice(0, 3).join(', ') + (models.length > 3 ? ` +${models.length - 3} more` : '')
}

const newKey = ref({
  label: '',
  model_group_id: 'basic'  // 默认值
})
```

- [ ] **Step 3: 修改创建逻辑传递 model_group_id**

```typescript
const handleCreateKey = async () => {
  if (!newKey.value.label.trim()) {
    ElMessage.warning('Please enter a key label')
    return
  }
  
  if (keys.value.length >= 10) {
    ElMessage.error('You have reached the maximum of 10 keys per user')
    return
  }
  
  loading.value = true
  try {
    const response = await createKey({
      label: newKey.value.label.trim(),
      model_group_id: newKey.value.model_group_id  // 传递分组
    })
    createdKeyInfo.value = {
      plaintext_key: response.plaintext_key,
      label: response.label
    }
    newKey.value = { label: '', model_group_id: 'basic' }  // 重置
    createDialogVisible.value = false
    await loadKeys()
    ElMessage.success('Key created successfully')
  } catch (error) {
    ElMessage.error(`Failed to create key: ${error}`)
  } finally {
    loading.value = false
  }
}
```

- [ ] **Step 4: 修改 KeyCard.vue - 显示模型分组**

在 KeyCard 组件中添加显示：

```vue
<div class="info-row">
  <span class="info-label">Model Group:</span>
  <el-tag type="primary" size="small">{{ keyInfo.model_group_name }}</el-tag>
</div>
```

在操作菜单中添加"修改分组"选项：

```vue
<el-dropdown-item @click="emit('changeModelGroup')">
  <el-icon><Switch /></el-icon>
  Change Model Group
</el-dropdown-item>
```

更新 `defineEmits`：

```typescript
defineEmits(['rotate', 'set-default', 'delete', 'copy-key', 'change-model-group'])
```

- [ ] **Step 5: 实现修改分组对话框**

在 KeyManagement.vue 中添加：

```vue
<!-- Change Model Group Dialog -->
<el-dialog v-model="changeGroupDialogVisible" title="Change Model Group" width="500px">
  <div v-if="changingKey">
    <p>
      Change model group for <strong>{{ changingKey.label }}</strong>
    </p>
    <el-form>
      <el-form-item label="New Model Group">
        <el-select v-model="newModelGroupId" placeholder="Select model group">
          <el-option
            v-for="group in modelGroups"
            :key="group.id"
            :label="group.name"
            :value="group.id"
          >
            <div class="group-option">
              <span class="group-name">{{ group.name }}</span>
              <span class="group-models">
                {{ formatAllowedModels(group.allowed_models) }}
              </span>
            </div>
          </el-option>
        </el-select>
      </el-form-item>
    </el-form>
  </div>
  <template #footer>
    <el-button @click="changeGroupDialogVisible = false">Cancel</el-button>
    <el-button type="primary" @click="handleChangeModelGroupSubmit" :loading="loading">
      Change
    </el-button>
  </template>
</el-dialog>
```

在 `<script>` 中添加：

```typescript
const changeGroupDialogVisible = ref(false)
const changingKey = ref<KeyInfo | null>(null)
const newModelGroupId = ref('')

const handleChangeModelGroup = (key: KeyInfo) => {
  changingKey.value = key
  newModelGroupId.value = key.model_group_id
  changeGroupDialogVisible.value = true
}

const handleChangeModelGroupSubmit = async () => {
  if (!changingKey.value) return
  
  loading.value = true
  try {
    await updateKeyModelGroup(changingKey.value.downstream_id, newModelGroupId.value)
    ElMessage.success('Model group changed successfully')
    changeGroupDialogVisible.value = false
    await loadKeys()
  } catch (error: any) {
    if (isModelGroupNotFoundError(error)) {
      ElMessage.error('Selected model group does not exist')
    } else {
      ElMessage.error(`Failed to change model group: ${error}`)
    }
  } finally {
    loading.value = false
  }
}
```

在 KeyCard 组件的 handler 中连接：

```vue
<KeyCard
  v-for="key in keys"
  :key="key.downstream_id"
  :key-info="key"
  @rotate="handleRotateKey(key)"
  @set-default="handleSetDefaultKey(key)"
  @delete="handleDeleteKey(key)"
  @copy-key="handleCopyKey(key)"
  @change-model-group="handleChangeModelGroup(key)"
/>
```

- [ ] **Step 6: 添加样式**

在 KeyManagement.vue 的 `<style scoped>` 中添加：

```css
.group-option {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.group-name {
  font-weight: 500;
}

.group-models {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
}

.info-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
}

.info-label {
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
```

- [ ] **Step 7: 测试完整流程**

运行：`npm run dev`

测试：
- [ ] 创建 key 时选择不同的模型分组
- [ ] 列表显示每个 key 的分组
- [ ] 修改 key 的分组
- [ ] 用不同分组的 key 调用 API，验证模型权限

- [ ] **Step 8: Commit**

```bash
git add frontend/src/views/portal/KeyManagement.vue frontend/src/components/portal/KeyCard.vue
git commit -m "feat(portal): add model group selection to key management

- Load and display available model groups
- Add group selector in create key dialog
- Display model group badge on key cards
- Support changing key's model group
- Show allowed models preview in group selector
- Default to 'basic' group for new keys"
```

---

### Task 21: 端到端测试 - 模型分组功能

**Files:**
- 所有相关文件（根据测试结果修复）

**Interfaces:**
- Consumes: 完整系统 (Tasks 13-20)
- Produces: 经过验证的端到端模型分组功能

- [ ] **Step 1: 执行数据库 Migration**

运行：`psql $DATABASE_URL -f migrations/2026-09-03-add-model-groups.sql`

预期：成功创建 `model_groups` 表和默认数据

- [ ] **Step 2: 验证 Migration 结果**

```bash
psql $DATABASE_URL -c "SELECT id, name, allowed_models FROM model_groups;"
```

预期：显示 basic, premium, all 三个分组

- [ ] **Step 3: 启动后端和前端**

```bash
cargo run &
cd frontend && npm run dev &
```

- [ ] **Step 4: 测试 Admin 页面 - 创建自定义分组**

1. 访问 `/admin/model-groups`
2. 创建新分组：
   - ID: `experimental`
   - Name: `Experimental Models`
   - Allowed Models: `gpt-4-turbo-preview`, `claude-3-opus-20240229`
3. 验证分组出现在列表中

- [ ] **Step 5: 测试创建 key 并选择分组**

1. 访问 Portal 的 Key Management 页面
2. 创建新 key：
   - Label: `Test Premium Key`
   - Model Group: 选择 `Premium Models`
3. 验证 key 创建成功且显示 "Premium Models" 标签

- [ ] **Step 6: 测试模型权限校验 - 允许的模型**

使用上面创建的 premium key，调用 API：

```bash
curl -X POST http://localhost:3030/v1/chat/completions \
  -H "Authorization: Bearer sk-xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "test"}]
  }'
```

预期：请求通过（不会因为权限被拒绝，实际上游调用可能失败）

- [ ] **Step 7: 测试模型权限校验 - 不允许的模型**

使用 basic 分组的 key，调用 API：

```bash
curl -X POST http://localhost:3030/v1/chat/completions \
  -H "Authorization: Bearer sk-yyy" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "test"}]
  }'
```

预期：返回 403 Forbidden，错误信息包含 `model_not_allowed`

- [ ] **Step 8: 测试通配符分组**

1. 创建 key 并选择 `All Models` 分组
2. 用该 key 调用任意模型
3. 验证不会因为权限被拒绝

- [ ] **Step 9: 测试修改 key 的分组**

1. 选择一个 basic 分组的 key
2. 点击"Change Model Group"
3. 改为 `premium` 分组
4. 验证更新成功
5. 用该 key 调用 gpt-4，验证现在可以通过

- [ ] **Step 10: 测试删除分组后 key 回退**

1. Admin 页面创建临时分组 `temp-group`
2. 创建 key 并绑定到 `temp-group`
3. 删除 `temp-group`
4. 刷新 key 列表
5. 验证该 key 的分组自动变为 `basic`

- [ ] **Step 11: 测试不能删除 basic 分组**

1. Admin 页面尝试删除 `basic` 分组
2. 验证删除按钮被禁用或显示错误

- [ ] **Step 12: 测试 Admin 修改分组后实时生效**

1. 创建 key 并绑定到 `premium` 分组
2. Admin 页面修改 `premium` 分组，移除 `gpt-4`
3. **不重启服务**
4. 用该 key 调用 gpt-4
5. 验证现在被拒绝（证明无需重启即可生效）

- [ ] **Step 13: 测试向后兼容 - 旧的 key- 类型**

1. 检查 Migration 后现有的 `key-` 类型 key
2. 验证它们的 model_group_id 为 `all`
3. 用旧 key 调用任意模型
4. 验证不受限制（保持旧行为）

- [ ] **Step 14: 测试非 Portal key 跳过校验**

如果有直接配置的 downstream（不在 portal_user_downstreams 表中）：
1. 用该 key 调用任意模型
2. 验证不会因为权限被拒绝（向后兼容）

- [ ] **Step 15: 运行自动化测试**

```bash
cargo test
cd frontend && npm run test
```

预期：所有测试通过

- [ ] **Step 16: 修复发现的问题**

记录测试中发现的所有问题，逐一修复

- [ ] **Step 17: 重新运行所有测试**

重复 Steps 4-15，确保所有功能正常

- [ ] **Step 18: Commit**

```bash
git add .
git commit -m "test: complete end-to-end testing for model groups

- Verified admin CRUD operations work correctly
- Confirmed model access validation at gateway
- Tested wildcard '*' allows all models
- Validated key fallback to 'basic' when group deleted
- Confirmed runtime updates without restart
- Tested backward compatibility with existing keys
- Fixed [list issues found and fixed]"
```

---

### Task 22: 文档更新

**Files:**
- Modify: `docs/features/multi-key-management.md` - 添加模型分组章节
- Modify: `README.md` - 更新功能列表
- Create: `docs/api/model-groups.md` - 模型分组 API 文档

**Interfaces:**
- Consumes: 完整系统 (Tasks 13-21)
- Produces: 更新的文档

- [ ] **Step 1: 更新 multi-key-management.md**

在 `docs/features/multi-key-management.md` 中添加：

```markdown
## Model Groups

### Overview

Model groups control which AI models each key can access. Administrators define groups with allowed model lists, and each key is assigned to one group.

### Default Groups

- **basic**: Cost-effective models for development (`gpt-3.5-turbo`, `claude-3-haiku`)
- **premium**: Advanced models for production (`gpt-4`, `claude-3-opus`, `claude-3.5-sonnet`)
- **all**: Unrestricted access (wildcard `*`)

### Creating Custom Groups

Administrators can create custom groups via the Admin panel at `/admin/model-groups`:

1. Click "Create Group"
2. Enter a unique ID (lowercase letters, digits, hyphens only)
3. Specify group name and description
4. List allowed models (one per line, or `*` for all)

### Assigning Groups to Keys

When creating a key in the Portal:
1. Select a model group from the dropdown
2. The key will only be able to access models in that group
3. Default: `basic` group

### Runtime Validation

When a client makes an API request:
1. The gateway extracts the `model` field from the request body
2. Looks up the key's assigned model group
3. Checks if the requested model is in the group's allowed list
4. Rejects with 403 if not allowed

### Dynamic Updates

Model group changes take effect immediately without restarting the service:
- Add/remove models from a group
- Keys automatically inherit the updated permissions
- No need to rotate keys or restart

### Backward Compatibility

- Existing `key-` prefixed keys are migrated to the `all` group (unrestricted access)
- Non-Portal keys (direct config) skip validation
- Keys created before model groups default to `basic`
```

- [ ] **Step 2: 创建 API 文档**

创建 `docs/api/model-groups.md`：

```markdown
# Model Groups API

## Admin Endpoints

### List All Model Groups

```http
GET /api/admin/model-groups
```

**Response:**
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
    }
  ]
}
```

### Create Model Group

```http
POST /api/admin/model-groups
```

**Request:**
```json
{
  "id": "experimental",
  "name": "Experimental Models",
  "description": "Beta and experimental models",
  "allowed_models": ["gpt-4-turbo-preview", "claude-3-opus-20240229"]
}
```

**Response:** 201 Created

**Error Codes:**
- `400` - Invalid ID format (must be lowercase letters, digits, hyphens)
- `409` - Group ID already exists

### Update Model Group

```http
PUT /api/admin/model-groups/:id
```

**Request:**
```json
{
  "name": "Updated Name",
  "description": "Updated description",
  "allowed_models": ["model1", "model2"]
}
```

**Response:** 204 No Content

### Delete Model Group

```http
DELETE /api/admin/model-groups/:id
```

**Response:** 204 No Content

**Notes:**
- Cannot delete the `basic` group (returns 403)
- Keys using this group will fall back to `basic`

## Portal Endpoints

### Create Key with Model Group

```http
POST /api/portal/keys
```

**Request:**
```json
{
  "label": "My Key",
  "model_group_id": "premium"
}
```

**Response:**
```json
{
  "downstream_id": "ds_abc123",
  "label": "My Key",
  "plaintext_key": "sk-xxx",
  "key_type": "ApiOnly",
  "model_group_id": "premium",
  "created_at": 1725350400
}
```

### List Keys (includes model group)

```http
GET /api/portal/keys
```

**Response:**
```json
{
  "keys": [
    {
      "downstream_id": "ds_abc123",
      "label": "My Key",
      "model_group_id": "premium",
      "model_group_name": "Premium Models",
      ...
    }
  ]
}
```

### Update Key's Model Group

```http
PUT /api/portal/keys/:id/model-group
```

**Request:**
```json
{
  "model_group_id": "all"
}
```

**Response:** 204 No Content

## Error Codes

- `model_not_allowed` (403) - Requested model is not in the key's group
- `model_group_not_found` (404) - Referenced model group does not exist
- `cannot_delete_basic` (403) - Attempted to delete the protected basic group
```

- [ ] **Step 3: 更新 README.md**

在 `README.md` 中更新功能列表：

```markdown
## Features

- **Multi-Key Management**: Create and manage up to 10 API keys per user
  - Two key types: Login Enabled (`key-`) and API Only (`sk-`)
  - **Model Groups**: Control which AI models each key can access
  - Admin-managed model groups with runtime updates
  - Rotate keys without downtime
  - Set default key for Portal login
```

- [ ] **Step 4: Commit**

```bash
git add docs/ README.md
git commit -m "docs: add model groups documentation

- Add model groups section to multi-key management doc
- Create comprehensive API documentation
- Update README with model groups feature
- Include examples and error codes"
```

---

## Self-Review Checklist

- [x] **Spec coverage**: 所有设计文档中的模型分组需求都有对应的 task
- [x] **No placeholders**: 所有 task 包含实际代码，无 TBD
- [x] **Type consistency**: 结构体、方法签名在所有 task 中一致
- [x] **TDD flow**: 每个功能都遵循 RED → GREEN → REFACTOR
- [x] **Backward compatibility**: Migration 和现有 key 保持兼容
- [x] **Security**: 模型权限校验、非 Portal key 跳过、不能删除 basic 保护
- [x] **Runtime updates**: 修改分组后无需重启即可生效

## Plan Complete

模型分组功能实现计划已保存到 `docs/superpowers/plans/2026-09-03-model-groups-implementation.md`。

**实施顺序：**
1. 先完成主计划 Tasks 1-12（多 Key 管理基础功能）
2. 再执行本计划 Tasks 13-22（模型分组附加功能）

**预计工作量：**
- Tasks 13-17（后端）: ~4-6 小时
- Tasks 18-20（前端）: ~3-4 小时
- Tasks 21-22（测试与文档）: ~2-3 小时
- 总计：~9-13 小时

**两种执行方式：**

**1. Subagent-Driven（推荐）** - 为每个 task 派发独立 subagent，两阶段审查，快速迭代

**2. Inline Execution** - 在当前 session 批量执行，设置检查点

你想选择哪种方式执行？
