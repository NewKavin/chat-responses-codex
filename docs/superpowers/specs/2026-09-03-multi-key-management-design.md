# 多 Key 管理功能设计文档

**日期**: 2026-09-03  
**作者**: Claude (Opus 5)  
**状态**: 待审核

---

## 1. 概述

### 1.1 背景

当前系统中，OAuth 登录用户只能看到一个 downstream key，无法创建和管理多个 key。这限制了用户在多个环境（开发/测试/生产）或多个客户端（Codex/Cline/Claude Code）使用不同 key 的能力。

### 1.2 目标

- OAuth 登录用户可以创建和管理多个 downstream key
- 区分两种 key 类型：`key-`（支持登录）和 `sk-`（仅 API 调用）
- 每用户最多 10 个 key（两种类型总和）
- Key 可以命名、随时查看、轮换、删除
- 保持向后兼容，不影响现有登录和 OAuth 绑定流程

### 1.3 非目标

- 不支持 key 级别的配额限制（第二期功能）
- 不支持 key 过期时间设置（第二期功能）
- 不支持 key 级别的 IP 白名单（第二期功能）

---

## 2. 架构设计

### 2.1 Key 类型定义

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    /// key- 前缀：支持 Portal 登录 + API 调用
    LoginEnabled,
    /// sk- 前缀：仅支持 API 调用
    ApiOnly,
}

impl KeyType {
    pub fn from_prefix(plaintext: &str) -> Self {
        if plaintext.starts_with("key-") {
            KeyType::LoginEnabled
        } else {
            KeyType::ApiOnly
        }
    }
    
    pub fn prefix(&self) -> &'static str {
        match self {
            KeyType::LoginEnabled => "key",
            KeyType::ApiOnly => "sk",
        }
    }
}
```

**关键规则**：
- OAuth 首次登录自动创建 1 个 `key-` 类型的 downstream（向后兼容）
- 用户在 Portal 手动创建的 key 均为 `sk-` 类型
- 轮换 key 时保持前缀不变（`key-` → `key-`，`sk-` → `sk-`）
- `sk-` key 无法用于 Portal 登录（前后端双重校验）

### 2.2 数据模型变更

**数据库 Migration**：

```sql
-- 添加 label 列
ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS label TEXT;

-- 为现有记录设置默认 label
UPDATE portal_user_downstreams 
SET label = 'Default Key'
WHERE label IS NULL;

-- 添加约束
ALTER TABLE portal_user_downstreams 
ALTER COLUMN label SET NOT NULL,
ADD CONSTRAINT label_max_length CHECK (char_length(label) <= 100);

-- 添加索引（优化查询）
CREATE INDEX IF NOT EXISTS idx_portal_user_downstreams_user_id 
ON portal_user_downstreams(user_id);

-- 添加创建时间列（如果不存在）
ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ DEFAULT NOW();
```

**Rust 结构体**：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalDownstreamBinding {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: String,  // 新增字段
}

#[derive(Debug, Clone)]
pub struct PortalDownstreamBindingWithLabel {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: String,
    pub created_at: i64,  // Unix timestamp
}
```

---

## 3. API 设计

### 3.1 新增 API 端点

```rust
// src/server/gateway.rs
.route("/api/portal/keys", get(portal_list_keys))
.route("/api/portal/keys", post(portal_create_key))
.route("/api/portal/keys/:id", get(portal_get_key_detail))
.route("/api/portal/keys/:id/rotate", post(portal_rotate_key_v2))
.route("/api/portal/keys/:id/default", post(portal_set_default_key))
.route("/api/portal/keys/:id", delete(portal_delete_key))

// 保留旧接口（向后兼容）
.route("/api/portal/key", get(portal_get_key))           // 返回默认 key
.route("/api/portal/key/rotate", post(portal_rotate_key)) // 轮换默认 key
```

### 3.2 API 详细规格

#### GET /api/portal/keys

列出当前用户的所有 keys。

**请求**：
```
GET /api/portal/keys
Cookie: crc_portal_session=<session_id>
```

**响应（200 OK）**：
```json
{
  "keys": [
    {
      "downstream_id": "ds_abc123",
      "label": "MacBook Pro 开发环境",
      "key_type": "LoginEnabled",
      "prefix": "key-",
      "is_default": true,
      "created_at": 1725350400,
      "last_used_at": 1725436800,
      "usage_last_7days": 1234
    },
    {
      "downstream_id": "ds_xyz789",
      "label": "生产服务器",
      "key_type": "ApiOnly",
      "prefix": "sk-",
      "is_default": false,
      "created_at": 1725264000,
      "last_used_at": null,
      "usage_last_7days": 0
    }
  ],
  "total": 2,
  "limit": 10
}
```

**字段说明**：
- `last_used_at`: 最近一次 API 调用时间（从 `response_history` 表查询），可选
- `usage_last_7days`: 最近 7 天的调用次数，可选

#### POST /api/portal/keys

创建新 key（`sk-` 前缀）。

**请求**：
```json
{
  "label": "测试环境 Key"
}
```

**验证规则**：
- `label` 必填，1-100 字符
- 当前用户 key 总数 < 10

**响应（200 OK）**：
```json
{
  "downstream_id": "ds_new123",
  "label": "测试环境 Key",
  "plaintext_key": "sk-AbCdEf1234567890",
  "key_type": "ApiOnly",
  "created_at": 1725523200
}
```

**响应（403 Forbidden - 超过限额）**：
```json
{
  "error": {
    "code": "key_limit_exceeded",
    "message": "You have reached the maximum of 10 keys per user"
  }
}
```

**响应（400 Bad Request - label 无效）**：
```json
{
  "error": {
    "code": "invalid_label",
    "message": "Label must be between 1 and 100 characters"
  }
}
```

#### GET /api/portal/keys/:id

获取某个 key 的详细信息（包含完整 plaintext）。

**请求**：
```
GET /api/portal/keys/ds_abc123
Cookie: crc_portal_session=<session_id>
```

**响应（200 OK）**：
```json
{
  "downstream_id": "ds_abc123",
  "label": "MacBook Pro 开发环境",
  "plaintext_key": "key-XyZ9876543210",
  "key_type": "LoginEnabled",
  "is_default": true,
  "created_at": 1725350400,
  "last_used_at": 1725436800,
  "usage_last_30days": 5678
}
```

**响应（404 Not Found）**：
```json
{
  "error": {
    "code": "key_not_found",
    "message": "Key not found or access denied"
  }
}
```

#### POST /api/portal/keys/:id/rotate

轮换某个 key（前缀保持不变）。

**请求**：
```json
{
  "label": "MacBook Pro 开发环境（已轮换）"  // 可选，更新 label
}
```

**响应（200 OK）**：
```json
{
  "downstream_id": "ds_abc123",
  "label": "MacBook Pro 开发环境（已轮换）",
  "plaintext_key": "key-NewRotated123",
  "key_type": "LoginEnabled",
  "rotated_at": 1725609600
}
```

#### POST /api/portal/keys/:id/default

设置某个 key 为默认。

**请求**：
```
POST /api/portal/keys/ds_xyz789/default
Cookie: crc_portal_session=<session_id>
```

**响应（200 OK）**：
```json
{
  "downstream_id": "ds_xyz789",
  "is_default": true
}
```

**副作用**：其他 key 的 `is_default` 自动设为 `false`。

#### DELETE /api/portal/keys/:id

删除某个 key（受限制：不能删除最后一个）。

**请求**：
```
DELETE /api/portal/keys/ds_xyz789
Cookie: crc_portal_session=<session_id>
```

**响应（200 OK）**：
```json
{
  "deleted": true,
  "downstream_id": "ds_xyz789"
}
```

**响应（403 Forbidden - 最后一个 key）**：
```json
{
  "error": {
    "code": "cannot_delete_last_key",
    "message": "Cannot delete the last key. You must have at least one key.",
    "remaining_keys": 1
  }
}
```

**响应（包含影响范围警告）**：

如果 key 在最近 7 天有调用记录，响应体包含 `usage_last_7days` 字段：

```json
{
  "deleted": true,
  "downstream_id": "ds_xyz789",
  "impact": {
    "last_used_at": 1725436800,
    "usage_last_7days": 1234
  }
}
```

前端根据 `impact.usage_last_7days > 0` 显示警告弹窗，用户二次确认后删除。

---

## 4. 后端实现

### 4.1 PortalStore 新增方法

```rust
impl PortalStore {
    /// 列出用户的所有 downstream bindings（带 label 和创建时间）
    pub async fn list_downstream_bindings_with_labels(
        &self,
        user_id: &str,
    ) -> Result<Vec<PortalDownstreamBindingWithLabel>, PortalStoreError>;

    /// 创建新的 downstream binding（带 label）
    pub async fn add_downstream_binding_with_label(
        &self,
        user_id: &str,
        downstream_id: &str,
        label: &str,
        is_default: bool,
    ) -> Result<(), PortalStoreError>;

    /// 更新 binding 的 label
    pub async fn update_downstream_label(
        &self,
        user_id: &str,
        downstream_id: &str,
        label: &str,
    ) -> Result<(), PortalStoreError>;

    /// 删除 downstream binding（带保护检查：不能删除最后一个）
    pub async fn remove_downstream_binding_safe(
        &self,
        user_id: &str,
        downstream_id: &str,
    ) -> Result<(), PortalStoreError>;

    /// 统计用户的 key 数量
    pub async fn count_user_keys(&self, user_id: &str) -> Result<i64, PortalStoreError>;

    /// 在事务内创建 key（并发安全，事务内重新检查数量限制）
    pub async fn create_key_with_limit_check(
        &self,
        user_id: &str,
        label: &str,
        downstream: DownstreamConfig,
    ) -> Result<(), PortalStoreError>;
}
```

### 4.2 AppState 新增方法

```rust
impl AppState {
    /// 获取 key 的使用统计（从 response_history 表）
    pub async fn get_key_usage_stats(
        &self,
        downstream_id: &str,
    ) -> Result<KeyUsageStats, io::Error>;
}

pub struct KeyUsageStats {
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub usage_last_7days: i64,
    pub usage_last_30days: i64,
}
```

**SQL 查询**：

```sql
SELECT 
    MAX(created_at) AS last_used_at,
    COUNT(*) FILTER (WHERE created_at > NOW() - INTERVAL '7 days') AS usage_last_7days,
    COUNT(*) FILTER (WHERE created_at > NOW() - INTERVAL '30 days') AS usage_last_30days
FROM response_history
WHERE downstream_key_id = $1
```

### 4.3 Portal Login 修改

**拒绝 `sk-` key 登录**：

```rust
// src/server/portal.rs

pub(super) async fn portal_login(
    State(state): State<AppState>,
    Json(payload): Json<PortalLoginRequest>,
) -> impl IntoResponse {
    // 新增：检查 key 类型
    if payload.key.starts_with("sk-") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": {
                    "code": "invalid_key_type",
                    "message": "API-only keys (sk-) cannot be used for Portal login. Please use a login-enabled key (key-) instead."
                }
            })),
        )
            .into_response();
    }
    
    // 继续原有验证逻辑...
}
```

### 4.4 并发安全处理

**场景：两个请求同时创建第 10 和第 11 个 key**

**解决方案**：在事务内重新检查数量限制

```rust
pub async fn create_key_with_limit_check(
    &self,
    user_id: &str,
    label: &str,
    downstream: DownstreamConfig,
) -> Result<(), PortalStoreError> {
    let mut client = self.pool.get().await?;
    let tx = client.transaction().await?;
    
    // 在事务内再次检查数量
    let count: i64 = tx
        .query_one(
            "SELECT COUNT(*) FROM portal_user_downstreams WHERE user_id = $1",
            &[&user_id],
        )
        .await?
        .get(0);
    
    if count >= 10 {
        tx.rollback().await?;
        return Err(PortalStoreError::Conflict("key limit exceeded".to_string()));
    }
    
    // 插入 binding
    tx.execute(
        "INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default, label) \
         VALUES ($1, $2, FALSE, $3)",
        &[&user_id, &downstream.id, &label],
    )
    .await?;
    
    tx.commit().await?;
    Ok(())
}
```

---

## 5. 前端实现

### 5.1 页面布局

**改造 `KeyManagement.vue`**：

- **顶部**：页面标题 + "创建新密钥"按钮
- **中间**：Key 卡片列表（`KeyCard` 组件）
  - 每个卡片显示：图标、名称、类型标签（默认/支持登录/仅API）、key 预览、创建时间、最近使用、操作按钮
- **底部**：已使用 X / 10 个密钥

**KeyCard 组件**：

```vue
<div class="key-card" :class="{ 'is-default': keyData.is_default }">
  <div class="key-card-header">
    <div class="key-card-title">
      <Key v-if="keyData.key_type === 'LoginEnabled'" />
      <Shield v-else />
      <span>{{ keyData.label }}</span>
    </div>
    <div class="key-card-badges">
      <el-tag v-if="keyData.is_default" type="success">默认</el-tag>
      <el-tag v-if="keyData.key_type === 'LoginEnabled'" type="success">
        支持登录
      </el-tag>
      <el-tag v-else type="info">仅API</el-tag>
    </div>
  </div>
  
  <div class="key-card-preview">
    <code>{{ keyData.prefix }}-••••{{ keyData.plaintext_key.slice(-4) }}</code>
  </div>
  
  <div class="key-card-meta">
    <span>创建于 {{ formatDate(keyData.created_at) }}</span>
    <span v-if="keyData.last_used_at">
      · 最近使用: {{ formatRelativeTime(keyData.last_used_at) }}
    </span>
    <span v-else>· 从未使用</span>
  </div>
  
  <div class="key-card-actions">
    <el-button size="small" @click="$emit('view')">查看</el-button>
    <el-button size="small" @click="$emit('copy')">复制</el-button>
    <el-button size="small" @click="$emit('rotate')">轮换</el-button>
    <el-button v-if="!keyData.is_default" size="small" @click="$emit('set-default')">
      设为默认
    </el-button>
    <el-button size="small" type="danger" plain @click="$emit('delete')">
      删除
    </el-button>
  </div>
</div>
```

### 5.2 关键交互流程

#### 创建 Key

1. 用户点击"创建新密钥"
2. 前端检查 `keys.length >= 10`，如果是则提示"已达上限"并禁用按钮
3. 弹窗显示表单：输入名称（1-100 字符）
4. 提示："新创建的密钥以 `sk-` 开头，仅用于 API 调用，不支持 Portal 登录。"
5. 用户点击"创建" → 调用 `POST /api/portal/keys`
6. 后端返回新 key → 自动打开"查看密钥"对话框，显示完整 plaintext + 复制按钮
7. 刷新列表

#### 查看 Key

1. 用户点击某个 key 的"查看"按钮
2. 调用 `GET /api/portal/keys/:id`
3. 弹窗显示：
   - 名称
   - 类型标签（支持登录 / 仅 API）
   - 完整 plaintext（带复制按钮）
   - 警告："请妥善保管此密钥。如需撤销访问权限，请删除此密钥。"

#### 轮换 Key

1. 用户点击"轮换"按钮
2. 二次确认弹窗："确定要轮换密钥 'XXX' 吗？轮换后旧密钥将立即失效。"
3. 调用 `POST /api/portal/keys/:id/rotate`
4. 自动打开"查看密钥"对话框，显示新 key
5. 刷新列表

#### 删除 Key

1. 用户点击"删除"按钮
2. 前端先调用 `GET /api/portal/keys/:id` 获取使用统计
3. 如果 `usage_last_7days > 0`：
   - 弹窗警告："此密钥正在使用中，最近 7 天有 1234 次调用。删除后，使用此密钥的客户端将无法访问 API。"
4. 如果 `usage_last_7days == 0`：
   - 弹窗确认："确定要删除密钥 'XXX' 吗？删除后无法恢复。"
5. 用户确认 → 调用 `DELETE /api/portal/keys/:id`
6. 如果后端返回 `cannot_delete_last_key` 错误 → 提示"不能删除最后一个密钥，至少保留一个"
7. 删除成功 → 刷新列表

### 5.3 API 客户端方法

```typescript
// src/api/portal.ts

export const portalApi = {
  // 现有方法...
  
  listKeys() {
    return axios.get('/api/portal/keys')
  },
  
  createKey(data: { label: string }) {
    return axios.post('/api/portal/keys', data)
  },
  
  getKeyDetail(id: string) {
    return axios.get(`/api/portal/keys/${id}`)
  },
  
  rotateKey(id: string, data?: { label?: string }) {
    return axios.post(`/api/portal/keys/${id}/rotate`, data)
  },
  
  setDefaultKey(id: string) {
    return axios.post(`/api/portal/keys/${id}/default`)
  },
  
  deleteKey(id: string) {
    return axios.delete(`/api/portal/keys/${id}`)
  },
}
```

### 5.4 Portal 登录表单修改

```vue
<!-- src/views/portal/PortalLogin.vue -->
<script setup lang="ts">
const handleLogin = async () => {
  // 前端校验：sk- key 无法登录
  if (form.key.startsWith('sk-')) {
    ElMessage.error('API 密钥（sk- 开头）不支持 Portal 登录，请使用 key- 开头的密钥')
    return
  }
  
  // 继续原有登录逻辑...
}
</script>
```

---

## 6. 错误处理

### 6.1 错误码汇总

| 场景 | 错误码 | HTTP 状态码 | 前端处理 |
|------|--------|------------|----------|
| 创建 key 时超过 10 个限额 | `key_limit_exceeded` | 403 | 禁用创建按钮 + 提示"已达上限" |
| 删除最后一个 key | `cannot_delete_last_key` | 403 | 提示"至少保留 1 个" |
| 删除不存在的 key | `key_not_found` | 404 | 提示"key 已被删除" |
| 操作别人的 key | `unauthorized` | 403 | Session 校验失败，跳转登录 |
| label 为空或超过 100 字符 | `invalid_label` | 400 | 表单校验拦截 + 后端拒绝 |
| 数据库连接失败 | `store_error` | 500 | 提示"系统错误，请稍后重试" |
| 尝试用 `sk-` key 登录 Portal | `invalid_key_type` | 403 | 提示"此 key 不支持 Portal 登录" |

### 6.2 边界情况

**场景 1：用户删除默认 key**

- **行为**：删除成功后，自动将最早创建的 key 设为默认
- **实现**：`remove_downstream_binding_safe()` 方法内执行 SQL：

```sql
UPDATE portal_user_downstreams SET is_default = TRUE
WHERE user_id = $1
  AND NOT EXISTS (SELECT 1 FROM portal_user_downstreams p2 WHERE p2.user_id = $1 AND p2.is_default = TRUE)
  AND downstream_id = (SELECT p3.downstream_id FROM portal_user_downstreams p3 WHERE p3.user_id = $1 ORDER BY p3.created_at LIMIT 1)
```

**场景 2：用户在两个浏览器标签页同时创建 key**

- **风险**：两个请求都通过了数量检查（9 个），同时创建第 10 和第 11 个
- **防护**：事务内重新检查数量，第 11 个请求被拒绝

**场景 3：用户轮换 key 的同时，客户端正在用旧 key 发起请求**

- **行为**：轮换后旧 key 立即失效，客户端收到 401
- **处理**：无需特殊处理，这是预期行为（文档说明）

---

## 7. 测试策略

### 7.1 单元测试

**PortalStore 测试**：

```rust
#[tokio::test]
async fn test_key_limit_enforcement() {
    // 创建 10 个 keys，第 11 个被拒绝
}

#[tokio::test]
async fn test_cannot_delete_last_key() {
    // 尝试删除唯一的 key，返回 Conflict 错误
}

#[tokio::test]
async fn test_delete_default_key_reassigns() {
    // 删除默认 key，另一个 key 自动成为默认
}

#[tokio::test]
async fn test_key_type_prefix_preserved_on_rotate() {
    // key- 轮换后仍是 key-，sk- 轮换后仍是 sk-
}
```

### 7.2 API 集成测试

```rust
#[tokio::test]
async fn test_create_key_flow() {
    // POST /api/portal/keys，验证返回的 plaintext 以 sk- 开头
}

#[tokio::test]
async fn test_sk_key_cannot_login() {
    // 用 sk- key 调用 POST /api/portal/login，返回 403
}

#[tokio::test]
async fn test_list_keys_shows_usage_stats() {
    // 模拟 API 调用，验证 usage_last_7days 字段正确
}

#[tokio::test]
async fn test_concurrent_key_creation() {
    // 并发创建 10 个 key，验证只有 10 个成功
}
```

### 7.3 前端组件测试

```typescript
it('disables create button when limit reached', () => {
  // 10 个 key 时，创建按钮应该被禁用
})

it('shows login-enabled badge for key- prefix', () => {
  // key- 类型显示"支持登录"标签
})

it('shows usage warning when deleting active key', () => {
  // 删除有调用记录的 key 时显示警告
})
```

---

## 8. 部署与迁移

### 8.1 数据库 Migration

```sql
-- migrations/2026-09-03-add-key-labels.sql

BEGIN;

ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS label TEXT;

UPDATE portal_user_downstreams 
SET label = 'Default Key'
WHERE label IS NULL;

ALTER TABLE portal_user_downstreams 
ALTER COLUMN label SET NOT NULL,
ADD CONSTRAINT label_max_length CHECK (char_length(label) <= 100);

CREATE INDEX IF NOT EXISTS idx_portal_user_downstreams_user_id 
ON portal_user_downstreams(user_id);

ALTER TABLE portal_user_downstreams 
ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ DEFAULT NOW();

COMMIT;
```

### 8.2 部署步骤

1. **数据库迁移**（停机/低峰期）：
   ```bash
   psql -U postgres -d chat_responses_codex -f migrations/2026-09-03-add-key-labels.sql
   ```

2. **后端部署**：
   ```bash
   cargo build --release
   docker build -t chat-responses-codex:v0.2.0 .
   docker-compose up -d
   ```

3. **验证**：
   - 访问 `/portal/keys` 确认页面正常
   - 创建一个新 key，确认前缀为 `sk-`
   - 尝试用 `sk-` key 登录 Portal，确认被拒绝
   - 删除非最后一个 key，确认成功
   - 尝试删除最后一个 key，确认被阻止

### 8.3 回滚计划

如果部署后发现严重问题：

1. **回滚后端**：
   ```bash
   docker-compose down
   docker-compose up -d chat-responses-codex:v0.1.2
   ```

2. **数据库回滚**（仅在必要时）：
   ```sql
   ALTER TABLE portal_user_downstreams DROP COLUMN IF EXISTS label;
   ```

**注意**：移除 `label` 列不会影响旧版本运行（旧版本不读取此列），但新创建的 bindings 会丢失 label 信息。

3. **向后兼容性**：旧版本前端会继续调用 `/api/portal/key`（旧接口保留），功能不受影响。

---

## 9. 安全考虑

### 9.1 Key 类型隔离

- **`sk-` key 无法登录 Portal**：前端表单校验 + 后端 `portal_login` 接口校验
- **防止权限提升**：用户只能操作自己的 keys（Session 提取 user_id）

### 9.2 Plaintext 存储风险

**现状**：`plaintext_key` 已经存储在数据库中（当前设计）

**风险**：如果数据库泄露，所有 key 明文暴露

**缓解措施**：
- 数据库连接使用 SSL
- PostgreSQL 配置行级安全策略（RLS）
- 定期审计数据库访问日志

**未来改进（第二期）**：
- 考虑只存储 hash，用户创建/轮换时只显示一次 plaintext
- 权衡：用户体验下降（丢失 key 无法恢复）vs 安全性提升

### 9.3 并发创建防护

事务内重新检查数量限制，防止超过 10 个的竞态条件。

---

## 10. 文档更新

### 10.1 用户文档

**README.md** - "Portal 功能"章节：

```markdown
### 多 Key 管理

OAuth 登录后，您可以创建和管理多个 API key：

- **key- 前缀**：支持 Portal 登录 + API 调用（OAuth 首次登录自动创建）
- **sk- 前缀**：仅支持 API 调用（用户手动创建）
- 每个用户最多 10 个 key
- 可以为 key 命名、查看、轮换、删除
- 至少保留 1 个 key（防止自锁）

访问 `/portal/keys` 管理您的密钥。
```

**INTEGRATION.md** - 客户端集成指南：

```markdown
## Key 类型说明

chat-responses-codex 支持两种 key 类型：

| 前缀 | 用途 | 获取方式 |
|------|------|----------|
| `key-` | API 调用 + Portal 登录 | OAuth 首次登录自动生成 |
| `sk-` | 仅 API 调用 | Portal 手动创建 |

**客户端配置**：使用任意前缀的 key 均可访问 API。

**Portal 登录**：只能使用 `key-` 前缀的 key。
```

### 10.2 管理员文档

**DEPLOYMENT.md** - 部署指南：

```markdown
## 升级到 v0.2.0（多 Key 管理）

1. 执行数据库迁移：
   ```bash
   psql -U postgres -d chat_responses_codex -f migrations/2026-09-03-add-key-labels.sql
   ```

2. 升级后端镜像到 v0.2.0

3. 验证：访问 `/portal/keys` 确认功能正常
```

**SECURITY.md** - 安全说明：

```markdown
## Key 类型安全

- `sk-` 前缀的 key 无法用于 Portal 登录，降低了 key 泄露的风险
- 建议：生产 API 调用使用 `sk-` key，仅在需要登录时使用 `key-` key
```

---

## 11. 未来扩展

### 11.1 第二期功能（可选）

- **Key 过期时间**：创建时设置 TTL（如 90 天后自动失效）
- **Key 级别配额**：单个 key 的每月调用次数限制
- **IP 白名单**：限制 key 只能从特定 IP 访问
- **Key 使用审计**：详细的调用日志（时间、IP、请求路径、响应状态）

### 11.2 安全增强（可选）

- **Plaintext 只显示一次**：创建/轮换后只显示一次，之后只能看到 hash
- **Key 权限范围**：限制 key 只能访问特定 upstream 或 model

---

## 12. 总结

本设计实现了完整的多 Key 管理功能，核心要点：

1. **两种 key 类型**：`key-`（登录 + API）和 `sk-`（仅 API），前缀保持不变
2. **数量限制**：每用户最多 10 个 key，事务内并发安全检查
3. **删除保护**：至少保留 1 个 key，删除前显示影响范围
4. **向后兼容**：保留旧 API，OAuth 登录流程不变
5. **前端体验**：卡片式列表，创建后立即显示新 key，支持随时查看

---

**附录 A：相关文件清单**

**后端**：
- `src/state/portal_store.rs` - 新增 6 个方法
- `src/server/portal.rs` - 新增 6 个 handler + 修改 `portal_login`
- `src/server/gateway.rs` - 新增 6 个路由
- `src/keys.rs` - 无修改（复用现有 `generate_downstream_key`）
- `migrations/2026-09-03-add-key-labels.sql` - 数据库迁移

**前端**：
- `frontend/src/views/portal/KeyManagement.vue` - 完全重写
- `frontend/src/components/portal/KeyCard.vue` - 新增组件
- `frontend/src/api/portal.ts` - 新增 6 个方法
- `frontend/src/views/portal/PortalLogin.vue` - 新增 `sk-` key 校验

**文档**：
- `README.md` - "Portal 功能"章节
- `INTEGRATION.md` - Key 类型说明
- `DEPLOYMENT.md` - 升级步骤
- `SECURITY.md` - Key 类型安全说明

---

**设计文档完成日期**: 2026-09-03
