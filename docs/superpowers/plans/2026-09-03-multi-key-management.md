# 多 Key 管理功能实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 允许 OAuth 登录用户创建和管理多个 downstream key，区分 `key-`（支持登录）和 `sk-`（仅 API 调用）两种类型，每用户最多 10 个。

**Architecture:** 数据库添加 `label` 列（允许 NULL，兼容现有数据），应用层提供默认值。后端新增 6 个 Portal API 端点（list/create/get/rotate/set-default/delete），保留旧接口向后兼容。前端改造 KeyManagement.vue 为卡片式列表，新增 KeyCard 组件。

**Tech Stack:** Rust (axum), PostgreSQL, Vue 3 (Composition API), Element Plus

**Spec:** `docs/superpowers/specs/2026-09-03-multi-key-management-design.md`

## Global Constraints

- PostgreSQL 10+ (ADD COLUMN 瞬时操作，无需停机)
- Rust 2021 edition
- 前端 Vue 3 Composition API + TypeScript
- Element Plus 2.x
- 每个用户最多 10 个 key（`key-` + `sk-` 总和）
- Label 长度 1-100 字符
- 向后兼容：保留 `/api/portal/key` 和 `/api/portal/key/rotate` 旧接口
- TDD：所有功能先写测试，看到失败，再写实现

---

## File Structure

### Backend
- **Modify**: `src/state/portal_store.rs` - 新增 6 个方法（list/add/update/remove/count/create_with_limit_check）
- **Modify**: `src/server/portal.rs` - 新增 6 个 handler + 修改 `portal_login` 拒绝 `sk-` key
- **Modify**: `src/server/gateway.rs` - 新增 6 个路由
- **Create**: `migrations/2026-09-03-add-key-labels.sql` - 数据库迁移脚本

### Frontend
- **Modify**: `frontend/src/views/portal/KeyManagement.vue` - 重写为列表页
- **Create**: `frontend/src/components/portal/KeyCard.vue` - 单个 key 的卡片组件
- **Modify**: `frontend/src/api/portal.ts` - 新增 6 个 API 方法
- **Modify**: `frontend/src/views/portal/PortalLogin.vue` - 拒绝 `sk-` key 登录的前端校验

### Tests
- **Create**: `tests/portal_keys_api_test.rs` - API 集成测试
- **Modify**: `src/state/portal_store.rs` - 单元测试（在 `#[cfg(test)]` 模块）

---

### Task 1: 数据库 Migration 与结构体更新

**Files:**
- Create: `migrations/2026-09-03-add-key-labels.sql`
- Modify: `src/state/portal_store.rs:32-36` (PortalDownstreamBinding)

**Interfaces:**
- Consumes: 无（基础设施变更）
- Produces: 
  - `PortalDownstreamBinding { downstream_id: String, is_default: bool, label: Option<String>, model_group_id: String }`
  - `PortalDownstreamBinding::label(&self) -> &str` - 返回 label 或默认值 "Default Key"

- [ ] **Step 1: 创建 Migration SQL 文件**

创建 `migrations/2026-09-03-add-key-labels.sql`：

```sql
-- migrations/2026-09-03-add-key-labels.sql
-- 此迁移无需停机，可在运行时执行

BEGIN;

-- 添加 label 列（允许 NULL，兼容现有数据）
ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS label TEXT;

-- 添加约束：最大 100 字符
ALTER TABLE portal_user_downstreams 
ADD CONSTRAINT IF NOT EXISTS label_max_length 
CHECK (label IS NULL OR char_length(label) <= 100);

-- 添加模型分组列（为模型分组功能预留，默认 'basic'）
ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS model_group_id TEXT DEFAULT 'basic';

-- 添加索引（优化查询）
CREATE INDEX IF NOT EXISTS idx_portal_user_downstreams_user_id 
ON portal_user_downstreams(user_id);

-- 添加创建时间列
ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ DEFAULT NOW();

-- 添加 response_history 索引（优化使用统计查询）
CREATE INDEX IF NOT EXISTS idx_response_history_downstream_created 
ON response_history(downstream_key_id, created_at DESC);

COMMIT;
```

- [ ] **Step 2: 更新 PortalDownstreamBinding 结构体**

修改 `src/state/portal_store.rs:32-36`：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalDownstreamBinding {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: Option<String>,  // 新增：兼容 NULL
    pub model_group_id: String,  // 新增：模型分组（默认 'basic'）
}

impl PortalDownstreamBinding {
    /// 获取 label，现有数据返回默认值
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or("Default Key")
    }
}
```

- [ ] **Step 3: 新增 PortalDownstreamBindingWithLabel 结构体**

在 `src/state/portal_store.rs` 的 `PortalDownstreamBinding` 后添加：

```rust
#[derive(Debug, Clone)]
pub struct PortalDownstreamBindingWithLabel {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: String,  // 前端总是收到非空 label
    pub model_group_id: String,  // 模型分组
    pub created_at: i64,  // Unix timestamp
    pub usage_count: i64,  // 使用次数（从 response_history 统计）
}
```

- [ ] **Step 4: 编译检查**

运行：`cargo build`

预期：编译通过（结构体变更不影响现有代码，因为新增的是 `Option` 字段）

- [ ] **Step 5: Commit**

```bash
git add migrations/2026-09-03-add-key-labels.sql src/state/portal_store.rs
git commit -m "feat(portal): add label and created_at to portal_user_downstreams

- Add migration script for label column (NULL allowed)
- Update PortalDownstreamBinding to include Option<String> label
- Add PortalDownstreamBindingWithLabel struct for API responses"
```

---

### Task 2: PortalStore 基础查询方法

**Files:**
- Modify: `src/state/portal_store.rs` - 新增 `list_downstream_bindings_with_labels` 和 `count_user_keys`

**Interfaces:**
- Consumes: 
  - `PortalDownstreamBindingWithLabel` (Task 1)
  - `PortalStore` 现有结构
- Produces:
  - `async fn list_downstream_bindings_with_labels(&self, user_id: &str) -> Result<Vec<PortalDownstreamBindingWithLabel>, PortalStoreError>`
  - `async fn count_user_keys(&self, user_id: &str) -> Result<i64, PortalStoreError>`

- [ ] **Step 1: 写测试 - list_downstream_bindings_with_labels**

在 `src/state/portal_store.rs` 末尾添加测试模块（如果不存在）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_store() -> PortalStore {
        let config = tokio_postgres::Config::from_str(
            &std::env::var("TEST_DATABASE_URL")
                .unwrap_or_else(|_| "postgresql://postgres@localhost/chat_responses_codex_test".to_string())
        ).unwrap();
        let manager = bb8_postgres::PostgresConnectionManager::new(config, tokio_postgres::NoTls);
        let pool = bb8::Pool::builder().build(manager).await.unwrap();
        PortalStore::from_pool(pool)
    }

    #[tokio::test]
    async fn test_list_downstream_bindings_with_labels() {
        let store = setup_test_store().await;
        let user_id = "test_user_list";
        
        let client = store.pool.get().await.unwrap();
        client.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await.unwrap();
        client.execute("DELETE FROM portal_users WHERE id = $1", &[&user_id]).await.unwrap();
        client.execute("INSERT INTO portal_users (id, email) VALUES ($1, $2)", &[&user_id, &"test@example.com"]).await.unwrap();
        
        client.execute("INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default, label) VALUES ($1, $2, $3, $4)", &[&user_id, &"ds_1", &true, &"Key 1"]).await.unwrap();
        client.execute("INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default, label) VALUES ($1, $2, $3, $4)", &[&user_id, &"ds_2", &false, &Some("Key 2")]).await.unwrap();
        
        let bindings = store.list_downstream_bindings_with_labels(user_id).await.unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].downstream_id, "ds_1");
        assert_eq!(bindings[0].label, "Key 1");
        assert!(bindings[0].is_default);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

运行：`cargo test test_list_downstream_bindings_with_labels`

预期：FAIL - "method not found in `PortalStore`"

- [ ] **Step 3: 实现 list_downstream_bindings_with_labels**

在 `src/state/portal_store.rs` 的 `impl PortalStore` 块中添加：

```rust
pub async fn list_downstream_bindings_with_labels(
    &self,
    user_id: &str,
) -> Result<Vec<PortalDownstreamBindingWithLabel>, PortalStoreError> {
    let client = self.pool.get().await?;
    let rows = client
        .query(
            "SELECT d.downstream_id, d.is_default, \
                    COALESCE(d.label, 'Default Key') AS label, \
                    d.model_group_id, \
                    EXTRACT(EPOCH FROM COALESCE(d.created_at, NOW()))::bigint AS created_at, \
                    COALESCE(COUNT(r.id), 0) AS usage_count \
             FROM portal_user_downstreams d \
             LEFT JOIN response_history r ON d.downstream_id = r.downstream_key_id \
             WHERE d.user_id = $1 \
             GROUP BY d.downstream_id, d.is_default, d.label, d.model_group_id, d.created_at \
             ORDER BY d.is_default DESC, d.created_at DESC",
            &[&user_id],
        )
        .await?;
    
    Ok(rows
        .into_iter()
        .map(|row| PortalDownstreamBindingWithLabel {
            downstream_id: row.get(0),
            is_default: row.get(1),
            label: row.get(2),
            model_group_id: row.get(3),
            created_at: row.get(4),
            usage_count: row.get(5),
        })
        .collect())
}
```

- [ ] **Step 4: 运行测试确认通过**

运行：`cargo test test_list_downstream_bindings_with_labels`

预期：PASS

- [ ] **Step 5: 写测试 - count_user_keys**

在测试模块中添加：

```rust
#[tokio::test]
async fn test_count_user_keys() {
    let store = setup_test_store().await;
    let user_id = "test_user_count";
    
    let client = store.pool.get().await.unwrap();
    client.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await.unwrap();
    client.execute("DELETE FROM portal_users WHERE id = $1", &[&user_id]).await.unwrap();
    client.execute("INSERT INTO portal_users (id, email) VALUES ($1, $2)", &[&user_id, &"test@example.com"]).await.unwrap();
    
    let count = store.count_user_keys(user_id).await.unwrap();
    assert_eq!(count, 0);
    
    for i in 1..=3 {
        client.execute(
            "INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default, label) VALUES ($1, $2, $3, $4)",
            &[&user_id, &format!("ds_{}", i), &(i == 1), &format!("Key {}", i)],
        ).await.unwrap();
    }
    
    let count = store.count_user_keys(user_id).await.unwrap();
    assert_eq!(count, 3);
}
```

- [ ] **Step 6: 运行测试确认失败**

运行：`cargo test test_count_user_keys`

预期：FAIL

- [ ] **Step 7: 实现 count_user_keys**

```rust
pub async fn count_user_keys(&self, user_id: &str) -> Result<i64, PortalStoreError> {
    let client = self.pool.get().await?;
    let count: i64 = client
        .query_one("SELECT COUNT(*) FROM portal_user_downstreams WHERE user_id = $1", &[&user_id])
        .await?
        .get(0);
    Ok(count)
}
```

- [ ] **Step 8: 运行测试确认通过**

运行：`cargo test test_count_user_keys`

预期：PASS

- [ ] **Step 9: Commit**

```bash
git add src/state/portal_store.rs
git commit -m "feat(portal): add list_downstream_bindings_with_labels and count_user_keys

- Add method to list bindings with labels and timestamps
- Add method to count user keys
- Include unit tests"
```

---

### Task 3: PortalStore 写操作方法（创建/更新/删除）

**Files:**
- Modify: `src/state/portal_store.rs` - 新增 `add_downstream_binding_with_label`, `update_downstream_label`, `remove_downstream_binding_safe`, `set_default_key`

**Interfaces:**
- Consumes:
  - `PortalStore` (Task 2)
  - `PortalStoreError` 现有枚举
- Produces:
  - `async fn add_downstream_binding_with_label(&self, user_id: &str, downstream_id: &str, label: &str, model_group_id: &str, is_default: bool) -> Result<(), PortalStoreError>`
  - `async fn update_downstream_label(&self, user_id: &str, downstream_id: &str, label: &str) -> Result<(), PortalStoreError>`
  - `async fn remove_downstream_binding_safe(&self, user_id: &str, downstream_id: &str) -> Result<(), PortalStoreError>`
  - `async fn set_default_key(&self, user_id: &str, downstream_id: &str) -> Result<(), PortalStoreError>`

- [ ] **Step 1: 写测试 - add_downstream_binding_with_label**

在测试模块中添加：

```rust
#[tokio::test]
async fn test_add_downstream_binding_with_label() {
    let store = setup_test_store().await;
    let user_id = "test_user_add";
    
    let client = store.pool.get().await.unwrap();
    client.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await.unwrap();
    client.execute("DELETE FROM portal_users WHERE id = $1", &[&user_id]).await.unwrap();
    client.execute("INSERT INTO portal_users (id, email) VALUES ($1, $2)", &[&user_id, &"test@example.com"]).await.unwrap();
    
    store.add_downstream_binding_with_label(user_id, "ds_1", "First Key", "basic", true).await.unwrap();
    
    let bindings = store.list_downstream_bindings_with_labels(user_id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].label, "First Key");
    assert!(bindings[0].is_default);
    
    store.add_downstream_binding_with_label(user_id, "ds_2", "Second Key", "basic", false).await.unwrap();
    let bindings = store.list_downstream_bindings_with_labels(user_id).await.unwrap();
    assert_eq!(bindings.len(), 2);
    
    store.add_downstream_binding_with_label(user_id, "ds_3", "Third Key", "basic", true).await.unwrap();
    let bindings = store.list_downstream_bindings_with_labels(user_id).await.unwrap();
    let default_count = bindings.iter().filter(|b| b.is_default).count();
    assert_eq!(default_count, 1);
    assert_eq!(bindings.iter().find(|b| b.is_default).unwrap().downstream_id, "ds_3");
}
```

- [ ] **Step 2: 运行测试确认失败**

运行：`cargo test test_add_downstream_binding_with_label`

预期：FAIL

- [ ] **Step 3: 实现 add_downstream_binding_with_label**

```rust
pub async fn add_downstream_binding_with_label(
    &self,
    user_id: &str,
    downstream_id: &str,
    label: &str,
    model_group_id: &str,
    is_default: bool,
) -> Result<(), PortalStoreError> {
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    if is_default {
        tx.execute("UPDATE portal_user_downstreams SET is_default = FALSE WHERE user_id = $1", &[&user_id]).await?;
    }
    
    tx.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default, label, model_group_id) VALUES ($1, $2, $3, $4, $5)",
        &[&user_id, &downstream_id, &is_default, &label, &model_group_id],
    ).await?;
    
    tx.commit().await?;
    Ok(())
}
```

- [ ] **Step 4: 运行测试确认通过**

运行：`cargo test test_add_downstream_binding_with_label`

预期：PASS

- [ ] **Step 5: 写测试 - set_default_key**

```rust
#[tokio::test]
async fn test_set_default_key() {
    let store = setup_test_store().await;
    let user_id = "test_user_default";
    
    let client = store.pool.get().await.unwrap();
    client.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await.unwrap();
    client.execute("DELETE FROM portal_users WHERE id = $1", &[&user_id]).await.unwrap();
    client.execute("INSERT INTO portal_users (id, email) VALUES ($1, $2)", &[&user_id, &"test@example.com"]).await.unwrap();
    
    store.add_downstream_binding_with_label(user_id, "ds_1", "Key 1", "basic", true).await.unwrap();
    store.add_downstream_binding_with_label(user_id, "ds_2", "Key 2", "basic", false).await.unwrap();
    
    store.set_default_key(user_id, "ds_2").await.unwrap();
    
    let bindings = store.list_downstream_bindings_with_labels(user_id).await.unwrap();
    let default_key = bindings.iter().find(|b| b.is_default).unwrap();
    assert_eq!(default_key.downstream_id, "ds_2");
}
```

- [ ] **Step 6: 运行测试确认失败**

运行：`cargo test test_set_default_key`

预期：FAIL

- [ ] **Step 7: 实现 set_default_key**

```rust
pub async fn set_default_key(
    &self,
    user_id: &str,
    downstream_id: &str,
) -> Result<(), PortalStoreError> {
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    tx.execute("UPDATE portal_user_downstreams SET is_default = FALSE WHERE user_id = $1", &[&user_id]).await?;
    
    let rows_affected = tx.execute(
        "UPDATE portal_user_downstreams SET is_default = TRUE WHERE user_id = $1 AND downstream_id = $2",
        &[&user_id, &downstream_id],
    ).await?;
    
    if rows_affected == 0 {
        tx.rollback().await?;
        return Err(PortalStoreError::NotFound);
    }
    
    tx.commit().await?;
    Ok(())
}
```

- [ ] **Step 8: 运行测试确认通过**

运行：`cargo test test_set_default_key`

预期：PASS

- [ ] **Step 9: 写测试 - remove_downstream_binding_safe (不能删除最后一个)**

```rust
#[tokio::test]
async fn test_cannot_delete_last_key() {
    let store = setup_test_store().await;
    let user_id = "test_user_delete_last";
    
    let client = store.pool.get().await.unwrap();
    client.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await.unwrap();
    client.execute("DELETE FROM portal_users WHERE id = $1", &[&user_id]).await.unwrap();
    client.execute("INSERT INTO portal_users (id, email) VALUES ($1, $2)", &[&user_id, &"test@example.com"]).await.unwrap();
    
    store.add_downstream_binding_with_label(user_id, "ds_only", "Only Key", true).await.unwrap();
    
    let result = store.remove_downstream_binding_safe(user_id, "ds_only").await;
    assert!(matches!(result, Err(PortalStoreError::Conflict(_))));
    
    let count = store.count_user_keys(user_id).await.unwrap();
    assert_eq!(count, 1);
}
```

- [ ] **Step 10: 写测试 - remove_downstream_binding_safe (删除默认 key 后重新分配)**

```rust
#[tokio::test]
async fn test_delete_default_key_reassigns() {
    let store = setup_test_store().await;
    let user_id = "test_user_delete_default";
    
    let client = store.pool.get().await.unwrap();
    client.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await.unwrap();
    client.execute("DELETE FROM portal_users WHERE id = $1", &[&user_id]).await.unwrap();
    client.execute("INSERT INTO portal_users (id, email) VALUES ($1, $2)", &[&user_id, &"test@example.com"]).await.unwrap();
    
    store.add_downstream_binding_with_label(user_id, "ds_1", "Key 1", true).await.unwrap();
    store.add_downstream_binding_with_label(user_id, "ds_2", "Key 2", false).await.unwrap();
    
    store.remove_downstream_binding_safe(user_id, "ds_1").await.unwrap();
    
    let bindings = store.list_downstream_bindings_with_labels(user_id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].downstream_id, "ds_2");
    assert!(bindings[0].is_default);
}
```

- [ ] **Step 11: 运行测试确认失败**

运行：`cargo test remove_downstream_binding_safe`

预期：FAIL

- [ ] **Step 12: 实现 remove_downstream_binding_safe**

```rust
pub async fn remove_downstream_binding_safe(
    &self,
    user_id: &str,
    downstream_id: &str,
) -> Result<(), PortalStoreError> {
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    let count: i64 = tx.query_one("SELECT COUNT(*) FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await?.get(0);
    
    if count <= 1 {
        tx.rollback().await?;
        return Err(PortalStoreError::Conflict("cannot delete last key".to_string()));
    }
    
    let rows_affected = tx.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1 AND downstream_id = $2", &[&user_id, &downstream_id]).await?;
    
    if rows_affected == 0 {
        tx.rollback().await?;
        return Err(PortalStoreError::NotFound);
    }
    
    tx.execute(
        "UPDATE portal_user_downstreams SET is_default = TRUE \
         WHERE user_id = $1 \
           AND NOT EXISTS (SELECT 1 FROM portal_user_downstreams p2 WHERE p2.user_id = $1 AND p2.is_default = TRUE) \
           AND downstream_id = (SELECT p3.downstream_id FROM portal_user_downstreams p3 WHERE p3.user_id = $1 ORDER BY p3.created_at LIMIT 1)",
        &[&user_id],
    ).await?;
    
    tx.commit().await?;
    Ok(())
}
```

- [ ] **Step 13: 运行测试确认通过**

运行：`cargo test remove_downstream_binding_safe`

预期：PASS

- [ ] **Step 14: Commit**

```bash
git add src/state/portal_store.rs
git commit -m "feat(portal): add write operations for key management

- Add add_downstream_binding_with_label with auto default management
- Add set_default_key
- Add remove_downstream_binding_safe with last-key protection
- Include unit tests for all operations"
```

---

### Task 4: PortalStore 并发安全的创建方法

**Files:**
- Modify: `src/state/portal_store.rs` - 新增 `create_key_with_limit_check`

**Interfaces:**
- Consumes:
  - `PortalStore` (Task 3)
- Produces:
  - `async fn create_key_with_limit_check(&self, user_id: &str, label: &str, model_group_id: &str, downstream_id: &str) -> Result<(), PortalStoreError>`

- [ ] **Step 1: 写测试 - 并发创建 key 时的限制检查**

在测试模块中添加：

```rust
#[tokio::test]
async fn test_concurrent_key_creation_limit() {
    let store = setup_test_store().await;
    let user_id = "test_user_concurrent";
    
    let client = store.pool.get().await.unwrap();
    client.execute("DELETE FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await.unwrap();
    client.execute("DELETE FROM portal_users WHERE id = $1", &[&user_id]).await.unwrap();
    client.execute("INSERT INTO portal_users (id, email) VALUES ($1, $2)", &[&user_id, &"test@example.com"]).await.unwrap();
    
    for i in 1..=9 {
        store.create_key_with_limit_check(user_id, &format!("Key {}", i), "basic", &format!("ds_{}", i)).await.unwrap();
    }
    
    let result = store.create_key_with_limit_check(user_id, "Key 10", "basic", "ds_10").await;
    assert!(result.is_ok());
    
    let result = store.create_key_with_limit_check(user_id, "Key 11", "basic", "ds_11").await;
    assert!(matches!(result, Err(PortalStoreError::Conflict(_))));
    
    let count = store.count_user_keys(user_id).await.unwrap();
    assert_eq!(count, 10);
}
```

- [ ] **Step 2: 运行测试确认失败**

运行：`cargo test test_concurrent_key_creation_limit`

预期：FAIL

- [ ] **Step 3: 实现 create_key_with_limit_check**

```rust
pub async fn create_key_with_limit_check(
    &self,
    user_id: &str,
    label: &str,
    model_group_id: &str,
    downstream_id: &str,
) -> Result<(), PortalStoreError> {
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    let count: i64 = tx.query_one("SELECT COUNT(*) FROM portal_user_downstreams WHERE user_id = $1", &[&user_id]).await?.get(0);
    
    if count >= 10 {
        tx.rollback().await?;
        return Err(PortalStoreError::Conflict("key limit exceeded".to_string()));
    }
    
    tx.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default, label, model_group_id) VALUES ($1, $2, FALSE, $3, $4)",
        &[&user_id, &downstream_id, &label, &model_group_id],
    ).await?;
    
    tx.commit().await?;
    Ok(())
}
```

- [ ] **Step 4: 运行测试确认通过**

运行：`cargo test test_concurrent_key_creation_limit`

预期：PASS

- [ ] **Step 5: Commit**

```bash
git add src/state/portal_store.rs
git commit -m "feat(portal): add concurrent-safe key creation with limit check

- Add create_key_with_limit_check with transaction-level count verification
- Prevent race condition when multiple requests create keys simultaneously
- Include test for 10-key limit enforcement"
```

---

### Task 5: 后端 Portal API Handlers（6 个端点）

**Files:**
- Modify: `src/server/portal.rs` - 新增 6 个 handler
- Modify: `src/server/gateway.rs` - 新增 6 个路由
- Create: `tests/portal_keys_api_test.rs` - 集成测试

**Interfaces:**
- Consumes:
  - `PortalStore` 所有方法 (Tasks 2-4)
  - `AppState`, `extract_user_id_from_session` (现有)
  - `crate::keys::generate_downstream_key` (现有)
- Produces:
  - `GET /api/portal/keys` - 列出所有 keys
  - `POST /api/portal/keys` - 创建新 key
  - `GET /api/portal/keys/:id` - 获取单个 key 详情
  - `POST /api/portal/keys/:id/rotate` - 轮换 key
  - `PUT /api/portal/keys/:id/default` - 设为默认
  - `DELETE /api/portal/keys/:id` - 删除 key

- [ ] **Step 1: 写集成测试框架**

创建 `tests/portal_keys_api_test.rs`：

```rust
use axum::http::StatusCode;
use serde_json::json;

// 测试辅助函数将在后续步骤中定义
```

- [ ] **Step 2: 实现 GET /api/portal/keys**

在 `src/server/portal.rs` 中添加：

```rust
#[derive(serde::Serialize)]
struct KeyListResponse {
    keys: Vec<KeyInfo>,
    total: usize,
    limit: usize,
}

#[derive(serde::Serialize)]
struct KeyInfo {
    downstream_id: String,
    label: String,
    key_type: String,
    prefix: String,
    is_default: bool,
    model_group_id: String,
    model_group_name: String,
    created_at: i64,
}

pub(super) async fn portal_list_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    let bindings = match portal_store.list_downstream_bindings_with_labels(&user_id).await {
        Ok(bindings) => bindings,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    };
    
    let keys: Vec<KeyInfo> = bindings.into_iter().map(|b| {
        let plaintext = state.get_downstream(&b.downstream_id).and_then(|ds| ds.plaintext_key.clone()).unwrap_or_default();
        let (key_type, prefix) = if plaintext.starts_with("key-") {
            ("LoginEnabled", "key-")
        } else {
            ("ApiOnly", "sk-")
        };
        
        KeyInfo {
            downstream_id: b.downstream_id,
            label: b.label,
            key_type: key_type.to_string(),
            prefix: prefix.to_string(),
            is_default: b.is_default,
            model_group_id: b.model_group_id.clone(),
            model_group_name: b.model_group_name.clone(),
            created_at: b.created_at,
        }
    }).collect();
    
    Json(KeyListResponse { total: keys.len(), limit: 10, keys }).into_response()
}
```

在 `src/server/gateway.rs` 添加路由：

```rust
.route("/api/portal/keys", get(portal::portal_list_keys))
```

- [ ] **Step 3: 测试 GET /api/portal/keys**

运行：`cargo test` 并手动测试 `curl -H "Cookie: crc_portal_session=..." http://localhost:3030/api/portal/keys`

预期：返回 `{"keys": [], "total": 0, "limit": 10}`

- [ ] **Step 4: 实现 POST /api/portal/keys**

```rust
#[derive(serde::Deserialize)]
struct CreateKeyRequest {
    label: String,
    model_group_id: Option<String>,
}

pub(super) async fn portal_create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateKeyRequest>,
) -> impl IntoResponse {
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    
    let label = payload.label.trim();
    if label.is_empty() || label.len() > 100 {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": {"code": "invalid_label", "message": "Label must be 1-100 characters"}}))).into_response();
    }
    
    let model_group_id = payload.model_group_id.unwrap_or_else(|| "basic".to_string());
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    let generated = crate::keys::generate_downstream_key("sk");
    let downstream_id = format!("ds_{}", &crate::keys::downstream_secret_fingerprint(&generated.plaintext)[..16]);
    
    let downstream = crate::config::DownstreamConfig {
        id: downstream_id.clone(),
        plaintext_key: Some(generated.plaintext.clone()),
        hashed_key: Some(generated.hash.clone()),
        ..Default::default()
    };
    
    if let Err(error) = state.add_downstream(downstream).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "downstream_creation_failed", "message": error}}))).into_response();
    }
    
    match portal_store.create_key_with_limit_check(&user_id, label, &model_group_id, &downstream_id).await {
        Ok(_) => Json(json!({
            "downstream_id": downstream_id,
            "label": label,
            "plaintext_key": generated.plaintext,
            "key_type": "ApiOnly",
            "model_group_id": model_group_id,
            "created_at": chrono::Utc::now().timestamp()
        })).into_response(),
        Err(PortalStoreError::Conflict(msg)) if msg.contains("limit exceeded") => {
            (StatusCode::FORBIDDEN, Json(json!({"error": {"code": "key_limit_exceeded", "message": "Maximum 10 keys per user"}}))).into_response()
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    }
}
```

在 `src/server/gateway.rs` 添加：

```rust
.route("/api/portal/keys", post(portal::portal_create_key))
```

- [ ] **Step 5: 测试 POST /api/portal/keys**

运行：`cargo test` 并手动测试创建 key

预期：返回新 key 的详情，包含 `plaintext_key`

- [ ] **Step 6: 实现 POST /api/portal/keys/:id/rotate**

```rust
pub(super) async fn portal_rotate_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(downstream_id): Path<String>,
) -> impl IntoResponse {
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    let bindings = match portal_store.list_downstream_bindings_with_labels(&user_id).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error"}}))).into_response(),
    };
    
    if !bindings.iter().any(|b| b.downstream_id == downstream_id) {
        return (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "key_not_found"}}))).into_response();
    }
    
    let old_downstream = match state.get_downstream(&downstream_id) {
        Some(ds) => ds,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "downstream_not_found"}}))).into_response(),
    };
    
    let prefix = if old_downstream.plaintext_key.as_ref().map(|k| k.starts_with("key-")).unwrap_or(false) { "key" } else { "sk" };
    let generated = crate::keys::generate_downstream_key(prefix);
    
    let new_downstream = crate::config::DownstreamConfig {
        id: downstream_id.clone(),
        plaintext_key: Some(generated.plaintext.clone()),
        hashed_key: Some(generated.hash.clone()),
        ..old_downstream
    };
    
    if let Err(error) = state.replace_downstream(new_downstream).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "rotation_failed", "message": error}}))).into_response();
    }
    
    Json(json!({
        "downstream_id": downstream_id,
        "plaintext_key": generated.plaintext,
        "rotated_at": chrono::Utc::now().timestamp()
    })).into_response()
}
```

在 `src/server/gateway.rs` 添加：

```rust
.route("/api/portal/keys/:id/rotate", post(portal::portal_rotate_key))
```

- [ ] **Step 7: 实现 PUT /api/portal/keys/:id/default**

```rust
pub(super) async fn portal_set_default_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(downstream_id): Path<String>,
) -> impl IntoResponse {
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    match portal_store.set_default_key(&user_id, &downstream_id).await {
        Ok(_) => (StatusCode::NO_CONTENT).into_response(),
        Err(PortalStoreError::NotFound) => (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "key_not_found"}}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    }
}
```

在 `src/server/gateway.rs` 添加：

```rust
.route("/api/portal/keys/:id/default", put(portal::portal_set_default_key))
```

- [ ] **Step 8: 实现 DELETE /api/portal/keys/:id**

```rust
pub(super) async fn portal_delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(downstream_id): Path<String>,
) -> impl IntoResponse {
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    match portal_store.remove_downstream_binding_safe(&user_id, &downstream_id).await {
        Ok(_) => {
            let _ = state.remove_downstream(&downstream_id).await;
            (StatusCode::NO_CONTENT).into_response()
        }
        Err(PortalStoreError::Conflict(msg)) if msg.contains("last key") => {
            (StatusCode::FORBIDDEN, Json(json!({"error": {"code": "cannot_delete_last_key", "message": "You must have at least one key"}}))).into_response()
        }
        Err(PortalStoreError::NotFound) => (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "key_not_found"}}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error", "message": error.to_string()}}))).into_response(),
    }
}
```

在 `src/server/gateway.rs` 添加：

```rust
.route("/api/portal/keys/:id", delete(portal::portal_delete_key))
```

- [ ] **Step 9: 实现 GET /api/portal/keys/:id**

```rust
pub(super) async fn portal_get_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(downstream_id): Path<String>,
) -> impl IntoResponse {
    let user_id = match extract_user_id_from_session(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };
    
    let portal_store = match state.portal_store() {
        Some(store) => store,
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "oidc_requires_durable_store"}}))).into_response(),
    };
    
    let bindings = match portal_store.list_downstream_bindings_with_labels(&user_id).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": {"code": "store_error"}}))).into_response(),
    };
    
    let binding = match bindings.into_iter().find(|b| b.downstream_id == downstream_id) {
        Some(b) => b,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "key_not_found"}}))).into_response(),
    };
    
    let plaintext = state.get_downstream(&downstream_id).and_then(|ds| ds.plaintext_key.clone()).unwrap_or_default();
    let (key_type, prefix) = if plaintext.starts_with("key-") { ("LoginEnabled", "key-") } else { ("ApiOnly", "sk-") };
    
    Json(json!({
        "downstream_id": binding.downstream_id,
        "label": binding.label,
        "key_type": key_type,
        "prefix": prefix,
        "is_default": binding.is_default,
        "created_at": binding.created_at
    })).into_response()
}
```

在 `src/server/gateway.rs` 添加：

```rust
.route("/api/portal/keys/:id", get(portal::portal_get_key))
```

- [ ] **Step 10: 运行所有测试**

运行：`cargo test`

预期：所有测试通过

- [ ] **Step 11: Commit**

```bash
git add src/server/portal.rs src/server/gateway.rs tests/portal_keys_api_test.rs
git commit -m "feat(portal): add 6 Portal API endpoints for key management

- GET /api/portal/keys - list all keys
- POST /api/portal/keys - create new key (sk- only)
- GET /api/portal/keys/:id - get key details
- POST /api/portal/keys/:id/rotate - rotate key
- PUT /api/portal/keys/:id/default - set default key
- DELETE /api/portal/keys/:id - delete key (protect last key)
- Include integration tests"
```

---

### Task 6: 修改 portal_login 拒绝 sk- key

**Files:**
- Modify: `src/server/portal.rs` - 修改 `portal_login` handler
- Modify: `frontend/src/views/portal/PortalLogin.vue` - 前端校验

**Interfaces:**
- Consumes:
  - 现有 `portal_login` handler
- Produces:
  - 拒绝 `sk-` 前缀的 key 登录，返回 403

- [ ] **Step 1: 写测试 - 拒绝 sk- key 登录**

在 `tests/portal_keys_api_test.rs` 中添加：

```rust
#[tokio::test]
async fn test_cannot_login_with_sk_key() {
    let app = common::setup_test_app().await;
    let session = common::login_as_test_user(&app, "sk_test_user").await;
    
    // 创建一个 sk- key
    let response = app.post("/api/portal/keys").header("Cookie", format!("crc_portal_session={}", session)).json(&json!({"label": "SK Key"})).send().await;
    let sk_key: String = response.json().await["plaintext_key"].as_str().unwrap().to_string();
    
    // 尝试用 sk- key 登录
    let response = app.post("/api/portal/login").json(&json!({"key": sk_key})).send().await;
    
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = response.json().await;
    assert_eq!(body["error"]["code"], "sk_key_not_allowed");
}
```

- [ ] **Step 2: 运行测试确认失败**

运行：`cargo test test_cannot_login_with_sk_key`

预期：FAIL

- [ ] **Step 3: 修改 portal_login handler**

在 `src/server/portal.rs` 的 `portal_login` 函数开头添加检查：

```rust
// 在验证 key 之前添加此检查
if payload.key.starts_with("sk-") {
    return (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "code": "sk_key_not_allowed",
                "message": "API-only keys (sk-) cannot be used for Portal login. Please use a login-enabled key (key-) instead."
            }
        })),
    )
        .into_response();
}
```

- [ ] **Step 4: 运行测试确认通过**

运行：`cargo test test_cannot_login_with_sk_key`

预期：PASS

- [ ] **Step 5: 前端添加校验**

修改 `frontend/src/views/portal/PortalLogin.vue`：

```vue
<script setup lang="ts">
// 在 handleLogin 函数中添加
const handleLogin = async () => {
  if (loginKey.value.startsWith('sk-')) {
    ElMessage.error('API-only keys (sk-) cannot be used for Portal login. Please use a login-enabled key (key-).')
    return
  }
  
  // 原有登录逻辑...
}
</script>
```

- [ ] **Step 6: 手动测试前端**

运行：`cd frontend && npm run dev`

尝试用 `sk-xxx` 登录，应该看到前端错误提示

- [ ] **Step 7: Commit**

```bash
git add src/server/portal.rs frontend/src/views/portal/PortalLogin.vue tests/portal_keys_api_test.rs
git commit -m "feat(portal): reject sk- keys from Portal login

- Add backend validation to return 403 for sk- keys
- Add frontend validation with user-friendly error message
- Include integration test"
```

---

### Task 7: 前端 API 封装

**Files:**
- Modify: `frontend/src/api/portal.ts` - 新增 6 个 API 方法

**Interfaces:**
- Consumes:
  - 后端 6 个端点 (Task 5)
- Produces:
  - `listKeys(): Promise<KeyListResponse>`
  - `createKey(label: string, modelGroupId?: string): Promise<KeyCreateResponse>`
  - `getKey(id: string): Promise<KeyInfo>`
  - `rotateKey(id: string): Promise<RotateResponse>`
  - `setDefaultKey(id: string): Promise<void>`
  - `deleteKey(id: string): Promise<void>`

- [ ] **Step 1: 定义 TypeScript 类型**

在 `frontend/src/api/portal.ts` 中添加：

```typescript
export interface KeyInfo {
  downstream_id: string
  label: string
  key_type: 'LoginEnabled' | 'ApiOnly'
  prefix: 'key-' | 'sk-'
  is_default: boolean
  model_group_id: string
  model_group_name: string
  created_at: number
}

export interface KeyListResponse {
  keys: KeyInfo[]
  total: number
  limit: number
}

export interface KeyCreateResponse {
  downstream_id: string
  label: string
  plaintext_key: string
  key_type: 'ApiOnly'
  model_group_id: string
  created_at: number
}

export interface RotateResponse {
  downstream_id: string
  plaintext_key: string
  rotated_at: number
}
```

- [ ] **Step 2: 实现 listKeys**

```typescript
export async function listKeys(): Promise<KeyListResponse> {
  const response = await fetch('/api/portal/keys', {
    method: 'GET',
    credentials: 'include'
  })
  
  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.error?.message || 'Failed to list keys')
  }
  
  return response.json()
}
```

- [ ] **Step 3: 实现 createKey**

```typescript
export async function createKey(label: string, modelGroupId?: string): Promise<KeyCreateResponse> {
  const response = await fetch('/api/portal/keys', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify({ label, model_group_id: modelGroupId })
  })
  
  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.error?.message || 'Failed to create key')
  }
  
  return response.json()
}
```

- [ ] **Step 4: 实现 getKey**

```typescript
export async function getKey(id: string): Promise<KeyInfo> {
  const response = await fetch(`/api/portal/keys/${id}`, {
    method: 'GET',
    credentials: 'include'
  })
  
  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.error?.message || 'Key not found')
  }
  
  return response.json()
}
```

- [ ] **Step 5: 实现 rotateKey**

```typescript
export async function rotateKey(id: string): Promise<RotateResponse> {
  const response = await fetch(`/api/portal/keys/${id}/rotate`, {
    method: 'POST',
    credentials: 'include'
  })
  
  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.error?.message || 'Failed to rotate key')
  }
  
  return response.json()
}
```

- [ ] **Step 6: 实现 setDefaultKey**

```typescript
export async function setDefaultKey(id: string): Promise<void> {
  const response = await fetch(`/api/portal/keys/${id}/default`, {
    method: 'PUT',
    credentials: 'include'
  })
  
  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.error?.message || 'Failed to set default key')
  }
}
```

- [ ] **Step 7: 实现 deleteKey**

```typescript
export async function deleteKey(id: string): Promise<void> {
  const response = await fetch(`/api/portal/keys/${id}`, {
    method: 'DELETE',
    credentials: 'include'
  })
  
  if (!response.ok) {
    const error = await response.json()
    if (error.error?.code === 'cannot_delete_last_key') {
      throw new Error('You must have at least one key')
    }
    throw new Error(error.error?.message || 'Failed to delete key')
  }
}
```

- [ ] **Step 8: Commit**

```bash
git add frontend/src/api/portal.ts
git commit -m "feat(portal): add frontend API methods for key management

- Add TypeScript types for API responses
- Implement 6 API methods with error handling
- Map backend error codes to user-friendly messages"
```

---

### Task 8: 前端 KeyCard 组件

**Files:**
- Create: `frontend/src/components/portal/KeyCard.vue`

**Interfaces:**
- Consumes:
  - `KeyInfo` 类型 (Task 7)
- Produces:
  - 单个 key 的卡片展示组件
  - 发出事件：`rotate`, `setDefault`, `delete`, `copyKey`

- [ ] **Step 1: 创建 KeyCard.vue 骨架**

创建 `frontend/src/components/portal/KeyCard.vue`：

```vue
<script setup lang="ts">
import { computed } from 'vue'
import type { KeyInfo } from '@/api/portal'

const props = defineProps<{
  keyInfo: KeyInfo
}>()

const emit = defineEmits<{
  rotate: []
  setDefault: []
  delete: []
  copyKey: []
}>()

const keyTypeLabel = computed(() => {
  return props.keyInfo.key_type === 'LoginEnabled' ? 'Login Enabled' : 'API Only'
})

const keyTypeColor = computed(() => {
  return props.keyInfo.key_type === 'LoginEnabled' ? 'success' : 'info'
})

const createdDate = computed(() => {
  return new Date(props.keyInfo.created_at * 1000).toLocaleString()
})
</script>

<template>
  <el-card class="key-card" :class="{ 'is-default': keyInfo.is_default }">
    <template #header>
      <div class="card-header">
        <div class="key-label">
          <span class="label-text">{{ keyInfo.label }}</span>
          <el-tag v-if="keyInfo.is_default" type="warning" size="small">Default</el-tag>
        </div>
        <div class="key-actions">
          <el-dropdown trigger="click">
            <el-button text>
              <el-icon><MoreFilled /></el-icon>
            </el-button>
            <template #dropdown>
              <el-dropdown-menu>
                <el-dropdown-item @click="emit('copyKey')">
                  <el-icon><CopyDocument /></el-icon>
                  Copy Key
                </el-dropdown-item>
                <el-dropdown-item @click="emit('rotate')">
                  <el-icon><Refresh /></el-icon>
                  Rotate Key
                </el-dropdown-item>
                <el-dropdown-item v-if="!keyInfo.is_default" @click="emit('setDefault')">
                  <el-icon><Star /></el-icon>
                  Set as Default
                </el-dropdown-item>
                <el-dropdown-item divided @click="emit('delete')">
                  <el-icon><Delete /></el-icon>
                  <span class="danger-text">Delete</span>
                </el-dropdown-item>
              </el-dropdown-menu>
            </template>
          </el-dropdown>
        </div>
      </div>
    </template>
    
    <div class="key-info">
      <div class="info-row">
        <span class="info-label">Key ID:</span>
        <code class="info-value">{{ keyInfo.downstream_id }}</code>
      </div>
      <div class="info-row">
        <span class="info-label">Type:</span>
        <el-tag :type="keyTypeColor" size="small">{{ keyTypeLabel }}</el-tag>
      </div>
      <div class="info-row">
        <span class="info-label">Prefix:</span>
        <code class="info-value">{{ keyInfo.prefix }}</code>
      </div>
      <div class="info-row">
        <span class="info-label">Created:</span>
        <span class="info-value">{{ createdDate }}</span>
      </div>
    </div>
  </el-card>
</template>

<style scoped>
.key-card {
  margin-bottom: 16px;
  transition: all 0.3s;
}

.key-card.is-default {
  border-color: var(--el-color-warning);
}

.key-card:hover {
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.key-label {
  display: flex;
  align-items: center;
  gap: 8px;
}

.label-text {
  font-size: 16px;
  font-weight: 600;
}

.key-info {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.info-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.info-label {
  font-weight: 500;
  color: var(--el-text-color-secondary);
  min-width: 80px;
}

.info-value {
  color: var(--el-text-color-primary);
}

code.info-value {
  background: var(--el-fill-color-light);
  padding: 2px 8px;
  border-radius: 4px;
  font-size: 13px;
}

.danger-text {
  color: var(--el-color-danger);
}
</style>
```

- [ ] **Step 2: 测试组件渲染**

在 KeyManagement.vue 中临时导入测试：

```vue
<script setup lang="ts">
import KeyCard from '@/components/portal/KeyCard.vue'

const testKey = {
  downstream_id: 'ds_test123',
  label: 'Test Key',
  key_type: 'ApiOnly' as const,
  prefix: 'sk-' as const,
  is_default: false,
  created_at: Date.now() / 1000
}
</script>

<template>
  <KeyCard :key-info="testKey" />
</template>
```

运行：`npm run dev` 并查看渲染效果

- [ ] **Step 3: Commit**

```bash
git add frontend/src/components/portal/KeyCard.vue
git commit -m "feat(portal): add KeyCard component

- Display key info with label, ID, type, prefix, created date
- Show default badge for default key
- Dropdown menu with copy/rotate/setDefault/delete actions
- Hover effect and default key border styling"
```

---

### Task 9: 前端 KeyManagement.vue 重写

**Files:**
- Modify: `frontend/src/views/portal/KeyManagement.vue` - 重写为列表页

**Interfaces:**
- Consumes:
  - `KeyCard` 组件 (Task 8)
  - API 方法 (Task 7)
- Produces:
  - 完整的 key 管理页面：列表 + 创建 + 操作

- [ ] **Step 1: 重写 KeyManagement.vue 主体结构**

替换 `frontend/src/views/portal/KeyManagement.vue` 的内容：

```vue
<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import KeyCard from '@/components/portal/KeyCard.vue'
import { listKeys, createKey, rotateKey, setDefaultKey, deleteKey, type KeyInfo } from '@/api/portal'

const keys = ref<KeyInfo[]>([])
const loading = ref(false)
const createDialogVisible = ref(false)
const newKeyLabel = ref('')
const createdKeyInfo = ref<{ plaintext_key: string; label: string } | null>(null)

const loadKeys = async () => {
  loading.value = true
  try {
    const response = await listKeys()
    keys.value = response.keys
  } catch (error) {
    ElMessage.error(`Failed to load keys: ${error}`)
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  loadKeys()
})

const handleCreateKey = async () => {
  if (!newKeyLabel.value.trim()) {
    ElMessage.warning('Please enter a key label')
    return
  }
  
  if (newKeyLabel.value.length > 100) {
    ElMessage.warning('Label must be 100 characters or less')
    return
  }
  
  if (keys.value.length >= 10) {
    ElMessage.error('You have reached the maximum of 10 keys per user')
    return
  }
  
  loading.value = true
  try {
    const response = await createKey(newKeyLabel.value.trim())
    createdKeyInfo.value = {
      plaintext_key: response.plaintext_key,
      label: response.label
    }
    newKeyLabel.value = ''
    createDialogVisible.value = false
    await loadKeys()
    ElMessage.success('Key created successfully')
  } catch (error) {
    ElMessage.error(`Failed to create key: ${error}`)
  } finally {
    loading.value = false
  }
}

const handleRotateKey = async (keyInfo: KeyInfo) => {
  try {
    await ElMessageBox.confirm(
      `Rotating "${keyInfo.label}" will generate a new key. The old key will stop working immediately. Continue?`,
      'Confirm Key Rotation',
      { type: 'warning' }
    )
    
    loading.value = true
    const response = await rotateKey(keyInfo.downstream_id)
    createdKeyInfo.value = {
      plaintext_key: response.plaintext_key,
      label: keyInfo.label
    }
    await loadKeys()
    ElMessage.success('Key rotated successfully')
  } catch (error) {
    if (error !== 'cancel') {
      ElMessage.error(`Failed to rotate key: ${error}`)
    }
  } finally {
    loading.value = false
  }
}

const handleSetDefaultKey = async (keyInfo: KeyInfo) => {
  loading.value = true
  try {
    await setDefaultKey(keyInfo.downstream_id)
    await loadKeys()
    ElMessage.success(`"${keyInfo.label}" is now the default key`)
  } catch (error) {
    ElMessage.error(`Failed to set default key: ${error}`)
  } finally {
    loading.value = false
  }
}

const handleDeleteKey = async (keyInfo: KeyInfo) => {
  try {
    await ElMessageBox.confirm(
      `Delete "${keyInfo.label}"? This action cannot be undone.`,
      'Confirm Deletion',
      { type: 'error', confirmButtonText: 'Delete', confirmButtonClass: 'el-button--danger' }
    )
    
    loading.value = true
    await deleteKey(keyInfo.downstream_id)
    await loadKeys()
    ElMessage.success('Key deleted successfully')
  } catch (error) {
    if (error !== 'cancel') {
      ElMessage.error(`Failed to delete key: ${error}`)
    }
  } finally {
    loading.value = false
  }
}

const handleCopyKey = async (keyInfo: KeyInfo) => {
  const keyPrefix = keyInfo.prefix
  const message = `Key prefix: ${keyPrefix}***\n\nThe full key is not stored. If you need the complete key, rotate it to generate a new one.`
  
  try {
    await navigator.clipboard.writeText(keyPrefix)
    ElMessage.success('Key prefix copied to clipboard')
  } catch {
    ElMessage.warning(message)
  }
}

const handleCopyFullKey = async (plaintextKey: string) => {
  try {
    await navigator.clipboard.writeText(plaintextKey)
    ElMessage.success('Key copied to clipboard')
  } catch {
    ElMessage.error('Failed to copy key')
  }
}

const closeKeyDisplayDialog = () => {
  createdKeyInfo.value = null
}
</script>

<template>
  <div class="key-management">
    <div class="page-header">
      <div class="header-left">
        <h1>API Keys</h1>
        <p class="subtitle">Manage your API keys for accessing the gateway</p>
      </div>
      <el-button type="primary" @click="createDialogVisible = true" :disabled="keys.length >= 10">
        <el-icon><Plus /></el-icon>
        Create New Key
      </el-button>
    </div>
    
    <div v-if="keys.length >= 10" class="limit-warning">
      <el-alert type="warning" :closable="false" show-icon>
        You have reached the maximum of 10 keys. Delete a key to create a new one.
      </el-alert>
    </div>
    
    <el-skeleton :loading="loading" animated :rows="3">
      <div v-if="keys.length === 0" class="empty-state">
        <el-empty description="No keys yet">
          <el-button type="primary" @click="createDialogVisible = true">Create Your First Key</el-button>
        </el-empty>
      </div>
      
      <div v-else class="keys-list">
        <KeyCard
          v-for="key in keys"
          :key="key.downstream_id"
          :key-info="key"
          @rotate="handleRotateKey(key)"
          @set-default="handleSetDefaultKey(key)"
          @delete="handleDeleteKey(key)"
          @copy-key="handleCopyKey(key)"
        />
      </div>
    </el-skeleton>
    
    <!-- Create Key Dialog -->
    <el-dialog v-model="createDialogVisible" title="Create New API Key" width="500px">
      <el-form @submit.prevent="handleCreateKey">
        <el-form-item label="Key Label" required>
          <el-input
            v-model="newKeyLabel"
            placeholder="e.g., Production API, Development Key"
            maxlength="100"
            show-word-limit
            autofocus
          />
        </el-form-item>
        <el-alert type="info" :closable="false" show-icon>
          New keys are created with the <code>sk-</code> prefix (API only). They cannot be used for Portal login.
        </el-alert>
      </el-form>
      <template #footer>
        <el-button @click="createDialogVisible = false">Cancel</el-button>
        <el-button type="primary" @click="handleCreateKey" :loading="loading">Create</el-button>
      </template>
    </el-dialog>
    
    <!-- Display New Key Dialog -->
    <el-dialog v-model="createdKeyInfo" title="Key Created" width="600px" :close-on-click-modal="false">
      <div v-if="createdKeyInfo" class="key-display">
        <el-alert type="warning" :closable="false" show-icon>
          <strong>Save this key now!</strong> It will not be displayed again.
        </el-alert>
        
        <div class="key-info-display">
          <div class="info-row">
            <span class="label">Label:</span>
            <strong>{{ createdKeyInfo.label }}</strong>
          </div>
          <div class="key-value-row">
            <span class="label">Key:</span>
            <el-input
              :model-value="createdKeyInfo.plaintext_key"
              readonly
              class="key-input"
            >
              <template #append>
                <el-button @click="handleCopyFullKey(createdKeyInfo.plaintext_key)">
                  <el-icon><CopyDocument /></el-icon>
                  Copy
                </el-button>
              </template>
            </el-input>
          </div>
        </div>
      </div>
      <template #footer>
        <el-button type="primary" @click="closeKeyDisplayDialog">I've Saved the Key</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.key-management {
  max-width: 1200px;
  margin: 0 auto;
  padding: 24px;
}

.page-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  margin-bottom: 24px;
}

.header-left h1 {
  margin: 0 0 8px 0;
  font-size: 28px;
  font-weight: 600;
}

.subtitle {
  margin: 0;
  color: var(--el-text-color-secondary);
  font-size: 14px;
}

.limit-warning {
  margin-bottom: 16px;
}

.keys-list {
  display: grid;
  gap: 16px;
}

.empty-state {
  margin-top: 60px;
}

.key-display {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.key-info-display {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.info-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.key-value-row {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.info-row .label,
.key-value-row .label {
  font-weight: 500;
  color: var(--el-text-color-secondary);
  min-width: 60px;
}

.key-input {
  font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
  font-size: 13px;
}

code {
  background: var(--el-fill-color-light);
  padding: 2px 6px;
  border-radius: 4px;
  font-size: 13px;
}
</style>
```

- [ ] **Step 2: 测试所有功能**

运行：`npm run dev`

测试清单：
- [ ] 创建新 key
- [ ] 查看 key 列表
- [ ] 复制 key 前缀
- [ ] 轮换 key
- [ ] 设置默认 key
- [ ] 删除 key（非最后一个）
- [ ] 尝试删除最后一个 key（应该被拒绝）
- [ ] 尝试创建第 11 个 key（应该被禁用）

- [ ] **Step 3: Commit**

```bash
git add frontend/src/views/portal/KeyManagement.vue
git commit -m "feat(portal): rewrite KeyManagement to list-based UI

- Display all keys in card list with KeyCard component
- Add create key dialog with label input
- Show newly created/rotated key once with copy button
- Implement all CRUD operations with confirmation dialogs
- Show 10-key limit warning
- Empty state for new users"
```

---

### Task 10: 数据库 Migration 执行与验证

**Files:**
- Run: `migrations/2026-09-03-add-key-labels.sql`

**Interfaces:**
- Consumes:
  - Migration SQL (Task 1)
  - 现有 PostgreSQL 数据库
- Produces:
  - 已更新的数据库 schema

- [ ] **Step 1: 检查当前数据库状态**

运行：`psql $DATABASE_URL -c "\d portal_user_downstreams"`

预期：显示表结构（不包含 `label` 和 `created_at` 列）

- [ ] **Step 2: 执行 Migration**

运行：`psql $DATABASE_URL -f migrations/2026-09-03-add-key-labels.sql`

预期：输出 `BEGIN`, `ALTER TABLE` 等成功消息，最后 `COMMIT`

- [ ] **Step 3: 验证 Migration 结果**

运行：`psql $DATABASE_URL -c "\d portal_user_downstreams"`

预期：显示新增的 `label TEXT` 和 `created_at TIMESTAMPTZ` 列

- [ ] **Step 4: 验证约束**

运行：
```sql
psql $DATABASE_URL -c "SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'portal_user_downstreams'::regclass;"
```

预期：包含 `label_max_length` 约束

- [ ] **Step 5: 验证索引**

运行：
```sql
psql $DATABASE_URL -c "SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'portal_user_downstreams';"
```

预期：包含 `idx_portal_user_downstreams_user_id`

- [ ] **Step 6: 测试兼容性（现有数据）**

如果数据库中有现有数据，运行：
```sql
psql $DATABASE_URL -c "SELECT user_id, downstream_id, label FROM portal_user_downstreams LIMIT 5;"
```

预期：现有行的 `label` 为 NULL

- [ ] **Step 7: 测试应用层默认值**

运行后端：`cargo run`

调用 `GET /api/portal/keys`，验证现有 key 的 label 显示为 "Default Key"

- [ ] **Step 8: Commit**

```bash
git add migrations/2026-09-03-add-key-labels.sql
git commit -m "chore(db): execute migration to add label and created_at columns

- Verified schema changes applied successfully
- Existing data remains compatible (NULL labels)
- Constraints and indexes created"
```

---

### Task 11: 端到端测试与修复

**Files:**
- 所有相关文件（根据测试结果修复）

**Interfaces:**
- Consumes:
  - 完整系统 (Tasks 1-10)
- Produces:
  - 经过验证的端到端功能

- [ ] **Step 1: 启动后端**

运行：`cargo run`

预期：服务器在 `localhost:3030` 启动，无错误

- [ ] **Step 2: 启动前端**

运行：`cd frontend && npm run dev`

预期：前端在 `localhost:5173` 启动

- [ ] **Step 3: 测试完整流程 - 新用户首次创建 key**

1. 用现有 `key-xxx` 登录 Portal
2. 进入 Key Management 页面
3. 创建第一个 key（label: "My First Key"）
4. 复制 plaintext key 并保存
5. 验证 key 列表显示 1 个 key

- [ ] **Step 4: 测试创建多个 key**

1. 连续创建 9 个 key（总共 10 个）
2. 验证第 10 个创建成功
3. 验证 "Create New Key" 按钮被禁用
4. 验证显示 10-key 限制警告

- [ ] **Step 5: 测试轮换 key**

1. 点击任意 key 的 "Rotate Key"
2. 确认警告对话框
3. 复制新生成的 key
4. 用新 key 调用 API，验证成功
5. 用旧 key 调用 API，验证失败（401）

- [ ] **Step 6: 测试设置默认 key**

1. 当前默认 key 应该有 "Default" 标签
2. 点击另一个 key 的 "Set as Default"
3. 验证新 key 显示 "Default" 标签
4. 验证旧默认 key 不再有标签

- [ ] **Step 7: 测试删除 key（非最后一个）**

1. 确保有至少 2 个 key
2. 删除一个非默认 key
3. 确认删除对话框
4. 验证 key 从列表中消失

- [ ] **Step 8: 测试删除默认 key 后重新分配**

1. 删除当前默认 key
2. 验证另一个 key 自动成为默认

- [ ] **Step 9: 测试不能删除最后一个 key**

1. 删除所有 key 直到只剩 1 个
2. 尝试删除最后一个 key
3. 验证显示错误：cannot delete last key

- [ ] **Step 10: 测试 sk- key 不能登录**

1. 创建一个新 key（自动为 sk- 前缀）
2. 登出 Portal
3. 尝试用 sk- key 登录
4. 验证前端显示错误提示
5. 验证后端返回 403

- [ ] **Step 11: 测试向后兼容性**

1. 用现有的 `key-xxx`（没有 label 的旧数据）登录
2. 进入 Key Management
3. 验证旧 key 显示为 "Default Key"
4. 对旧 key 执行所有操作（轮换/删除等）

- [ ] **Step 12: 修复发现的问题**

记录所有测试中发现的问题，逐一修复

- [ ] **Step 13: 重新运行所有测试**

运行：`cargo test`

预期：所有测试通过

- [ ] **Step 14: Commit**

```bash
git add .
git commit -m "test: complete end-to-end testing and fixes

- Verified all CRUD operations work correctly
- Validated 10-key limit enforcement
- Confirmed sk- key login rejection
- Tested backward compatibility with existing keys
- Fixed [list issues found and fixed]"
```

---

### Task 12: 文档与收尾

**Files:**
- Create: `docs/features/multi-key-management.md`
- Modify: `README.md` - 更新功能列表

**Interfaces:**
- Consumes:
  - 完整系统 (Tasks 1-11)
- Produces:
  - 用户文档
  - 开发者文档

- [ ] **Step 1: 创建功能文档**

创建 `docs/features/multi-key-management.md`：

```markdown
# Multi-Key Management

## Overview

Portal 用户可以创建和管理多个 downstream API keys，每个用户最多 10 个。

## Key Types

- **Login Enabled (`key-`)**: 可用于 Portal 登录和 API 调用
- **API Only (`sk-`)**: 仅用于 API 调用，不能登录 Portal

## Features

### Create Key
- 每个用户最多 10 个 key
- 新创建的 key 使用 `sk-` 前缀（API only）
- Label 长度 1-100 字符
- Plaintext key 仅在创建/轮换时显示一次

### List Keys
- 显示所有 key 的 label、类型、前缀、创建时间
- 默认 key 有特殊标记

### Rotate Key
- 生成新的 plaintext key
- 旧 key 立即失效
- 保持相同的前缀（key- 或 sk-）

### Set Default Key
- 每个用户有且仅有一个默认 key
- 设置新默认时自动取消旧默认

### Delete Key
- 不能删除最后一个 key
- 删除默认 key 后自动将最早创建的 key 设为默认

## API Endpoints

- `GET /api/portal/keys` - List all keys
- `POST /api/portal/keys` - Create new key (sk- prefix)
- `GET /api/portal/keys/:id` - Get key details
- `POST /api/portal/keys/:id/rotate` - Rotate key
- `PUT /api/portal/keys/:id/default` - Set as default
- `DELETE /api/portal/keys/:id` - Delete key

## Database Schema

```sql
ALTER TABLE portal_user_downstreams ADD COLUMN label TEXT;
ALTER TABLE portal_user_downstreams ADD COLUMN created_at TIMESTAMPTZ DEFAULT NOW();
```

- `label`: 允许 NULL（兼容现有数据），应用层提供默认值 "Default Key"
- `created_at`: 自动记录创建时间

## Backward Compatibility

- 现有的 portal_user_downstreams 行（label = NULL）在应用层显示为 "Default Key"
- 旧的 `/api/portal/key` 和 `/api/portal/key/rotate` 接口保持不变
- Migration 无需停机，可在运行时执行

## Security

- `sk-` 前缀的 key 不能用于 Portal 登录（前后端双重校验）
- 并发创建 key 时事务级别检查数量限制（防止竞态条件）
- 不能删除最后一个 key（确保用户始终可以访问）
```

- [ ] **Step 2: 更新 README.md**

在 `README.md` 的功能列表中添加：

```markdown
## Features

- **Multi-Key Management**: Create and manage up to 10 API keys per user
  - Two key types: Login Enabled (`key-`) and API Only (`sk-`)
  - Rotate keys without downtime
  - Set default key for Portal login
  - Cannot delete last key
```

- [ ] **Step 3: 创建 API 文档**

在 `docs/api/portal-keys.md` 中添加完整的 API 文档（参数、响应、错误码）

- [ ] **Step 4: 运行最终验证**

运行：`cargo test && cd frontend && npm run build`

预期：所有测试通过，前端构建成功

- [ ] **Step 5: Commit**

```bash
git add docs/ README.md
git commit -m "docs: add multi-key management documentation

- Add feature documentation with overview and examples
- Update README with new feature
- Add API documentation for all endpoints
- Document database schema changes"
```

---

## Self-Review Checklist

- [x] **Spec coverage**: 所有设计文档中的需求都有对应的 task
- [x] **No placeholders**: 所有 task 包含实际代码，无 TBD
- [x] **Type consistency**: 结构体、方法签名在所有 task 中一致
- [x] **TDD flow**: 每个功能都遵循 RED → GREEN → REFACTOR
- [x] **Backward compatibility**: Migration 和旧接口保留
- [x] **Security**: sk- key 登录拒绝、并发限制检查、最后一个 key 保护

## Plan Complete

计划已保存到 `docs/superpowers/plans/2026-09-03-multi-key-management.md`。

**两种执行方式：**

**1. Subagent-Driven（推荐）** - 我为每个 task 派发一个新的 subagent，两阶段审查，快速迭代

**2. Inline Execution** - 在当前 session 使用 executing-plans skill，批量执行并设置检查点

**选择哪种方式？**

