# OAuth 内部适配需求

**日期：** 2026-09-03  
**优先级：** 中（待当前 multi-key management 完成后实施）  
**状态：** 规划中

---

## 背景

公司内部使用的 OAuth 服务采用了非标准的实现方式，与标准 OAuth 2.0 规范存在差异。当前系统仅支持标准的 OAuth 流程，导致无法对接内部系统。

---

## 需求详情

### 1. UserInfo 端点调用方式适配

**当前实现（标准 OAuth 2.0）：**
```http
GET /userinfo HTTP/1.1
Host: oauth.provider.com
Authorization: Bearer {access_token}
```

**需要支持的内部方式：**
```http
POST /userinfo HTTP/1.1
Host: internal.oauth.company.com
Content-Type: application/json

{
  "access_token": "xxx",
  "client_id": "xxx",
  "scope": "openid profile email"
}
```

**适配要求：**
- 保留现有的 GET + Bearer 方式（向后兼容）
- 新增 POST + JSON body 方式支持
- 通过配置项控制使用哪种方式

---

### 2. Token 端点路径自定义

**当前实现（固定路径）：**
- Token 端点：`{base_url}/token`
- 硬编码在代码中

**需要支持：**
- 可配置的 token 端点路径
- 例如：`{base_url}/accesstoken`、`{base_url}/oauth/token` 等
- 通过配置项指定完整路径或相对路径

---

## 技术方案（初步）

### 配置项设计

在 `portal_oidc_*` 配置基础上新增：

```rust
// UserInfo 调用方式
PORTAL_OIDC_USERINFO_METHOD: "GET" | "POST"  // 默认 "GET"

// Token 端点路径（相对于 issuer）
PORTAL_OIDC_TOKEN_PATH: String  // 默认 "/token"
```

### 代码改动位置（估计）

**Backend (Rust):**
- `src/server/portal.rs` - OAuth 回调处理逻辑
- `src/state/portal_store.rs` 或新建 OAuth client 模块
- 适配 userinfo 请求构建逻辑
- 适配 token 端点 URL 构建逻辑

**配置：**
- 环境变量定义
- Admin settings 界面（如果需要运行时可配置）

---

## 实施计划

**待定 - 需要完成当前 multi-key management 功能后再详细规划**

**估计工作量：** 2-4 小时
- 配置项添加：30 分钟
- UserInfo 方法适配：1 小时
- Token 路径自定义：1 小时
- 测试和验证：1-2 小时

---

## 依赖关系

- ✅ 无依赖 - 可独立实施
- ⚠️ 优先级：在当前 multi-key management (Tasks 1-12) 完成后
- ⚠️ 建议：与模型分组功能 (Tasks 13-22) 并行或之后实施

---

## 验证方式

1. 配置为 POST 方式，验证 userinfo 请求正确发送 JSON body
2. 配置自定义 token 路径，验证 token 请求使用正确的端点
3. 保持默认配置，验证向后兼容性（标准 OAuth 流程仍正常工作）
4. 测试内部 OAuth 服务的完整登录流程

---

## 备注

- 此需求来自用户在 2026-09-03 提出
- 记录在 multi-key management 开发期间
- 不影响当前 SDD 执行，单独跟踪
