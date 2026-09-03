# OAuth 内部适配功能开发提示词

**复制以下内容到新的 Claude 会话中（在 main 分支环境）：**

---

我需要为现有的 Portal OAuth 登录功能添加对公司内部非标准 OAuth 实现的支持。

## 背景

当前系统支持标准 OAuth 2.0 流程，但公司内部 OAuth 服务使用了非标准实现：

1. **UserInfo 端点**：使用 POST + JSON body，而不是标准的 GET + Authorization: Bearer
2. **Token 端点路径**：使用自定义路径如 `/accesstoken`，而不是固定的 `/token`

## 需求

### 1. UserInfo 端点适配

**当前实现（标准）：**
```http
GET /userinfo HTTP/1.1
Authorization: Bearer {access_token}
```

**需要支持（内部）：**
```http
POST /userinfo HTTP/1.1
Content-Type: application/json

{
  "access_token": "xxx",
  "client_id": "xxx",
  "scope": "openid profile email"
}
```

**要求：**
- 保留现有 GET + Bearer 方式（向后兼容）
- 新增配置项控制使用哪种方式
- 建议配置项：`PORTAL_OIDC_USERINFO_METHOD` (值: "GET" 或 "POST"，默认 "GET")

### 2. Token 端点路径自定义

**当前实现：**
- 固定使用 `{issuer}/token`

**需要支持：**
- 可配置的 token 端点路径
- 建议配置项：`PORTAL_OIDC_TOKEN_PATH` (默认 "/token")
- 支持自定义路径如 `/accesstoken`、`/oauth/token` 等

## 技术要求

1. **TDD 开发**：先写测试，看到失败，再实现
2. **向后兼容**：默认配置下使用标准 OAuth 2.0 方式
3. **配置管理**：
   - 添加新的环境变量
   - 如果有 Admin Settings UI，也需要在界面中暴露这些配置
4. **代码位置**：
   - Portal OAuth 相关代码应该在 `src/server/portal.rs` 或相关模块
   - 查找现有的 `PORTAL_OIDC_*` 配置项，跟随相同模式

## 实现步骤建议

1. **调研现有代码**
   - 找到当前 OAuth token 和 userinfo 请求的实现位置
   - 了解现有的 `PORTAL_OIDC_*` 配置项是如何定义和使用的

2. **添加配置项**
   - 定义新的环境变量
   - 添加到配置结构体
   - 提供默认值确保向后兼容

3. **实现 UserInfo 方法适配**
   - 根据 `PORTAL_OIDC_USERINFO_METHOD` 配置选择请求方式
   - POST 方式需要构建 JSON body

4. **实现 Token 路径自定义**
   - 使用 `PORTAL_OIDC_TOKEN_PATH` 配置构建完整的 token URL
   - 替换硬编码的 "/token" 路径

5. **测试**
   - 单元测试：验证 URL 构建逻辑
   - 集成测试（如果可能）：模拟内部 OAuth 服务响应
   - 手动测试：使用实际的内部 OAuth 服务验证

## 验证清单

- [ ] 默认配置下，OAuth 流程与之前完全相同（向后兼容）
- [ ] 配置为 POST 方式后，userinfo 请求正确发送 JSON body（包含 access_token、client_id、scope）
- [ ] 配置自定义 token 路径后，token 请求使用正确的端点
- [ ] 所有测试通过
- [ ] 代码通过 `cargo clippy` 检查
- [ ] 内部 OAuth 服务完整登录流程测试通过

## 参考信息

- 当前项目使用 Rust + axum 框架
- PostgreSQL 作为数据库
- 遵循项目的 TDD 规范（CLAUDE.md 中定义）
- 配置管理可能涉及 `src/state/settings.rs` 或类似文件

---

**开始前请先：**
1. 确认在 main 分支：`git branch --show-current`
2. 拉取最新代码：`git pull origin main`
3. 调研现有 Portal OAuth 代码实现
4. 按照 TDD 流程开发（RED → GREEN → REFACTOR）

如果遇到问题或需要更具体的指导，请随时询问！
