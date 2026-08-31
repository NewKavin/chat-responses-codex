# Portal OAuth/OIDC 登录认证接口实施计划（待执行）

**日期：** 2026-08-28（原始草案 2026-08-23，本次仅更新日期与下游衔接说明，技术内容未改）  
**范围：** Portal 用户登录；管理员账号登录暂不改造  
**状态：** 已实施并提交（2026-08-29）；第一阶段 T1–T6 全部落地  
**本文档是第一阶段。** 第二阶段（管理员配置模型分组 + 下游用户按分组自助创建 key）见
`docs/superpowers/plans/2026-08-28-portal-self-service-keys-and-model-groups.md`，它**强依赖**本文档的
`portal_users` / `portal_identities` / `portal_user_downstreams` / `portal_sessions` 四张表和服务端
session 授权模型——没有可信的"这个请求是哪个人发的"，自助建 key 就是无主资源。**必须先做完本文档再做第二阶段。**

> 衔接提示：本文档 §6.3 的 `portal_user_downstreams` 已经设计成**多对多**（含 `is_default`），
> 第二阶段的"一个用户持有多把 key"正是建立在这张表上——每把自助 key 就是一条新的
> `downstreams` 记录 + 一条绑定。实施本文档时**不要**把它简化成一对一。

## 1. Goal

为自助门户增加基于 OAuth 2.0 Authorization Code + PKCE 的登录能力，并优先支持
OIDC（OpenID Connect）身份认证。用户从 `/portal/login` 跳转到配置的身份提供商（IdP）
完成认证后，回到网关建立 Portal 会话，继续访问现有概览、配额、历史、密钥和 Playground
接口。

实现必须保持现有“工号 + 下游密钥”登录可用，并将外部身份、Portal 用户和下游资源授权
分离，不能因为 OAuth 登录成功就自动获得任意下游密钥或配额。

## 2. 现状与问题

### 2.1 现有认证链路

| 位置 | 现状 |
|------|------|
| `src/auth.rs:5-47` | 只有基于 `jwt_secret` 的 12 小时 JWT 生成与校验；Claims 仅有 `sub/iat/exp` |
| `src/server/portal.rs:44-96` | `POST /api/portal/login` 校验 `employee_id + key`，然后用 `employee_id` 作为 JWT `sub` |
| `src/server/portal.rs:620-655` | Portal API 从 Bearer JWT 或下游密钥解析下游 ID |
| `src/server/gateway.rs:1967-1977` 附近 | Portal 路由和管理员路由在同一个 axum Router 中注册 |
| `frontend/src/views/portal/PortalLogin.vue:1-100` | 只有工号/密钥表单，将 JWT 写入 `localStorage.portal_token` |
| `frontend/src/api/portal.ts:14-40` | 每次请求从 localStorage 注入 Bearer token，401 时清理本地 token |
| `frontend/src/router/index.ts:93-110` | Portal 路由只检查 `localStorage.portal_token`，未验证服务端会话 |
| `src/state/postgres.rs:1790` 起 | PostgreSQL 通过内嵌 `SCHEMA_SQL` 初始化和增量扩展表结构 |
| `src/state.rs:3024-3105` 附近 | 未设置 `DATABASE_URL` 时使用文件状态兼容模式；Postgres 是生产持久化路径 |

### 2.2 当前设计缺口

1. 没有 provider discovery、授权 URL、state、PKCE verifier、code exchange 或 OIDC ID Token 校验。
2. JWT 的 `sub` 直接等同于下游 ID，无法表达“一个外部身份绑定哪个 Portal 用户/下游资源”。
3. localStorage 持有长期 Bearer token，存在 XSS 后被窃取并复用的风险。
4. 授权状态如果只放进浏览器或 JWT，无法可靠做到一次性消费、过期和跨实例共享。
5. OAuth provider 的 access token 不应成为 Portal API 的登录凭证，也不应返回给前端。

## 3. Architecture

### 3.1 推荐协议与开源实现

采用 **OIDC Authorization Code + PKCE（S256）** 作为首选实现：

- 使用 Rust `openidconnect` crate 完成 issuer discovery、JWKS 获取、ID Token 签名/issuer/aud/nonce
  校验及标准 claims 解析。
- 使用 `oauth2` crate 的 authorization-code client 处理授权 URL、state、PKCE 和 code exchange；
  如果 `openidconnect` 暴露的 client 已覆盖所需能力，可以统一使用其封装，避免两套 client 并存。
- HTTP 传输复用项目已有 `reqwest`/rustls，不自定义 token endpoint 的 HTTP 协议实现。
- 不引入会改变现有路由和 session 模型的完整认证框架；协议层使用成熟 crate，业务授权仍由项目自身控制。

纯 OAuth 2.0 provider（没有 OIDC `id_token`）作为兼容模式：调用 user-info endpoint，要求配置
`user_info_url` 和稳定的 subject claim；不能只用 email 作为唯一身份键。

### 3.2 认证与授权边界

认证分为四层：

1. **Provider 身份：** `(issuer, subject)` 是外部身份唯一键；email、name 只作为展示或人工绑定辅助字段。
2. **Portal 用户：** 内部稳定 UUID，记录 provider、subject、展示名、email、启停状态和最后登录时间。
3. **资源绑定：** Portal 用户显式绑定一个或多个现有 `downstreams.id`；现有 Portal 页面默认使用用户
   当前选择的下游资源，第一期若只支持一个绑定则 API 仍按数组模型设计。
4. **会话：** 服务端生成随机 session id，浏览器只收到 HttpOnly Cookie；Portal API 从 session 解析内部
   用户和当前 downstream，而不是信任客户端提交的 employee_id。

OAuth 登录成功但没有资源绑定时，应建立“已认证、未授权”的短暂状态并返回明确的 `403 portal_access_not_granted`，
不得自动创建下游密钥或把 provider claim 直接当成 downstream id。

### 3.3 持久化策略

PostgreSQL 为生产必需后端，新增表通过 `src/state/postgres.rs` 的 schema migration 机制创建。

文件兼容模式有两种可接受选择，实施模型必须在代码中明确其一，不能静默回退：

- **推荐：** OAuth 端点在文件模式返回 `503 oauth_requires_durable_store`，因为多实例/重启会破坏 state、PKCE
  和 session 的安全语义；现有工号/密钥登录不受影响。
- **可选：** 若确实需要单实例开发模式，增加进程内 TTL store，并在启动日志和响应中明确这是开发能力；该 store
  不能被宣称为生产 OAuth session store，也不能用于多实例部署。

## 4. Configuration contract

配置读取遵循 `src/main.rs` 现有环境变量模式，并在 `AppConfig` 中保留结构化字段。敏感值不写入日志、
`Debug` 输出、admin API 响应或前端 bundle。

建议配置项：

| 环境变量 | 必填 | 含义 |
|----------|------|------|
| `PORTAL_OAUTH_ENABLED` | 否 | 默认 `false`；未启用时不注册/不暴露可用 provider |
| `PORTAL_OAUTH_ISSUER_URL` | 启用 OIDC 时是 | IdP issuer，必须是 HTTPS（本地测试可显式允许 localhost） |
| `PORTAL_OAUTH_CLIENT_ID` | 是 | OAuth client id |
| `PORTAL_OAUTH_CLIENT_SECRET` | 是 | confidential client secret；仅服务端使用 |
| `PORTAL_OAUTH_REDIRECT_URL` | 是 | 完整 callback URL，必须与 provider 注册值严格一致 |
| `PORTAL_OAUTH_SCOPES` | 否 | 默认 `openid profile email`；只申请实际需要的 scope |
| `PORTAL_OAUTH_USERINFO_URL` | 纯 OAuth 模式是 | user-info endpoint |
| `PORTAL_OAUTH_ALLOWED_EMAIL_DOMAINS` | 否 | 可选域名白名单；不能替代 subject 绑定 |
| `PORTAL_OAUTH_AUTO_PROVISION` | 否 | 默认 `false`；是否允许首次见到合法 subject 创建 Portal 用户 |
| `PORTAL_SESSION_TTL_SECONDS` | 否 | 默认 12 小时，限制在合理范围内 |
| `PORTAL_OAUTH_STATE_TTL_SECONDS` | 否 | 默认 5 分钟 |
| `PORTAL_COOKIE_SECURE` | 否 | 默认生产为 `true`；HTTP 本地开发须显式关闭 |

如需要多个 provider，首期可只实现一个默认 provider，但内部数据模型的 `provider` 字段必须保留，接口路径
使用 `/api/portal/oauth/{provider}/...`，避免后续把单 provider 假设写死到表结构和 session claims。

## 5. HTTP API contract

所有 OAuth 端点都在 `/api/portal` 下；callback 是浏览器跳转端点，不接受前端 JS 代理。错误体沿用项目已有
`{"error":{"message","code"}}` 形状，禁止返回 provider 原始响应正文、client secret、access token、refresh token、
ID token 原文或 PKCE verifier。

### 5.1 `GET /api/portal/oauth/{provider}/start`

行为：

1. 校验 provider 已启用且配置完整。
2. 生成至少 32 bytes 的随机 `state`、随机 PKCE verifier 和对应 S256 challenge；`nonce` 也必须随机生成。
3. 只将 state 的哈希、verifier 的加密/受保护值、provider、创建时间、过期时间、归因后的安全 return path
   写入 `oauth_login_attempts`，原始 state 只放授权 URL。
4. 生成 provider authorization URL，固定使用 response type `code`、PKCE S256、state、nonce、client id、redirect URI
   和最小 scopes。
5. 返回 `302` 到 provider；禁止使用用户传入的任意 `redirect_uri`。

允许一个相对站内 `return_to`，必须只接受 hash route 或以 `/` 开头且不包含 scheme/host 的路径；非法值统一回落
   `/portal`，防止 open redirect。

### 5.2 `GET /api/portal/oauth/{provider}/callback`

处理顺序必须固定：

1. 先处理 provider 返回的 `error`，清理对应 attempt，并跳转错误页；错误描述不原样回显给用户。
2. 要求 `code` 和 `state`，对 state 做 constant-time 比较；查询 attempt 时必须要求未消费、未过期且 provider 相同。
3. 原子消费 attempt，确保 callback 重放只能失败一次；并发 callback 不能重复创建 session。
4. 使用保存的 PKCE verifier、原始 redirect URI 和 provider 配置交换 code；限制请求超时、响应体大小和重定向策略。
5. 校验 OIDC ID Token：签名算法白名单、issuer、audience/client id、nonce、`exp`、`iat` 时钟偏差；JWKS 缓存必须有
   TTL 和失败处理。纯 OAuth 模式则从 user-info 读取稳定 subject，并校验 HTTPS、响应结构和超时。
6. 用 `(provider, issuer, subject)` 查找或按配置创建 Portal 用户，刷新安全的 display/email 元数据；不能用 email 覆盖 subject。
7. 判断 Portal 用户是否有 active downstream 绑定；无绑定时跳转 `/portal/login?oauth_error=access_not_granted`，同时记录不含敏感 claim 的审计事件。
8. 创建服务端 Portal session，设置 `HttpOnly`、`SameSite=Lax`、正确 `Path`、生产 `Secure` Cookie；session id 必须是
   高熵随机不透明值，数据库只保存哈希。
9. 清理一次性登录 attempt，跳转安全的站内 `return_to`，默认 `/portal`。

### 5.3 `GET /api/portal/session`

返回当前 Portal 会话的最小信息：

```json
{
  "authenticated": true,
  "user": { "id": "internal-uuid", "display_name": "...", "email": "..." },
  "downstreams": [{ "id": "downstream-1", "name": "..." }],
  "current_downstream_id": "downstream-1",
  "expires_at": 1770000000
}
```

未登录返回 `401 portal_session_required`；已登录但没有绑定返回 `403 portal_access_not_granted`。不要返回 provider token
或完整身份 claims。

### 5.4 `POST /api/portal/logout`

撤销当前服务端 session（删除或标记 revoked），清除 Cookie，并幂等返回 `204`。不能只让前端删除 localStorage。

### 5.5 资源选择接口（需要多绑定时）

若一期允许一个用户绑定多个 downstream，增加：

- `POST /api/portal/session/downstream`，body `{ "downstream_id": "..." }`；只允许选择自己的 active 绑定。
- session 内保存当前 downstream，服务端所有 Portal API 以 session 值为准；不能接受任意 `employee_id` 作为授权依据。

## 6. Data model and migration

在 `src/state/postgres.rs` 的 `SCHEMA_SQL` 中增加版本化 migration（不要修改既有 migration 的语义）。建议表：

### 6.1 `portal_users`

- `id UUID PRIMARY KEY`
- `status TEXT NOT NULL`，至少 `active/disabled`
- `display_name TEXT NOT NULL DEFAULT ''`
- `email TEXT NULL`
- `created_at BIGINT NOT NULL`
- `last_login_at BIGINT NULL`
- `metadata JSONB NULL`，只保存经白名单筛选的非敏感展示字段

### 6.2 `portal_identities`

- `id UUID PRIMARY KEY`
- `portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE`
- `provider TEXT NOT NULL`
- `issuer TEXT NOT NULL`
- `subject TEXT NOT NULL`
- `email TEXT NULL`
- `created_at BIGINT NOT NULL`
- `last_seen_at BIGINT NOT NULL`
- 唯一索引 `(provider, issuer, subject)`

禁止把 email 作为唯一键；provider/issuer 变更必须视为不同身份，除非有显式管理员合并流程。

### 6.3 `portal_user_downstreams`

- `portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE`
- `downstream_id TEXT NOT NULL REFERENCES downstreams(id) ON DELETE CASCADE`
- `is_default BOOLEAN NOT NULL DEFAULT FALSE`
- `created_at BIGINT NOT NULL`
- 主键 `(portal_user_id, downstream_id)`

### 6.4 `oauth_login_attempts`

- `state_hash BYTEA PRIMARY KEY`
- `provider TEXT NOT NULL`
- `issuer TEXT NOT NULL`
- `pkce_verifier_ciphertext BYTEA NOT NULL`（使用服务端密钥保护，不能明文持久化）
- `nonce_hash BYTEA NOT NULL`
- `return_to TEXT NOT NULL`
- `created_at BIGINT NOT NULL`
- `expires_at BIGINT NOT NULL`
- `consumed_at BIGINT NULL`

增加 `(expires_at)` 索引和定期/请求触发的过期清理；state 必须一次性消费。

### 6.5 `portal_sessions`

- `session_hash BYTEA PRIMARY KEY`
- `portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE`
- `current_downstream_id TEXT NULL REFERENCES downstreams(id) ON DELETE SET NULL`
- `created_at BIGINT NOT NULL`
- `expires_at BIGINT NOT NULL`
- `last_seen_at BIGINT NOT NULL`
- `revoked_at BIGINT NULL`
- `user_agent_hash BYTEA NULL`、`ip_hash BYTEA NULL`（仅用于审计/异常检测，不能存原始值，按保留策略清理）

为 `expires_at`、`portal_user_id` 建索引。session cookie 只包含随机 id，不包含可篡改的 user/downstream 信息。

在 `StateStore` trait、`FileStateStore` 和 `PostgresStateStore` 之间定义明确的 OAuth/session repository 接口。推荐把
OAuth 读写操作封装成 `PortalAuthStore` trait，由 `AppState` 持有，而不是把 SQL 散落在 handler 中。

## 7. Backend implementation tasks

其他模型按以下顺序实现，每一步先补测试再写实现：

### T1. 抽离认证领域模型和密钥工具

涉及：`src/auth.rs`、`src/state/types.rs`、必要时新增 `src/portal_auth.rs`。

- 保留 `generate_admin_token`/`verify_admin_token` 的现有行为，避免影响管理员 API。
- 新增安全随机 bytes、SHA-256 state/session hash、PKCE S256、constant-time compare、时间窗口校验工具。
- 新增不可序列化到日志的 `PortalSession`、`OAuthLoginAttempt`、`PortalIdentity` 等内部类型。
- 明确错误枚举，不把 provider 原始错误字符串直接透传给客户端。
- 使用独立 session cookie 认证；不要复用“看到 `eyJ` 就按 JWT 解析”的旧 Portal 分支作为 OAuth session 方案。

### T2. Provider 配置和 OIDC/OAuth client

涉及：`src/main.rs`、`src/state/types.rs`、新文件 `src/portal_oauth.rs`（或仓库认可的等价模块）、`Cargo.toml`。

- 增加配置解析、默认值、生产校验和启动时脱敏日志。
- provider discovery、JWKS、token endpoint、userinfo endpoint 必须设置超时和大小限制。
- 只允许 HTTPS；localhost 测试通过显式开发开关放行。
- 只接受配置的 redirect URL，不能从请求头或 query 动态拼接。
- 依赖版本固定并更新 lockfile；优先采用成熟 crate 的校验 API，不手写 JWT/JWK 验证。

### T3. Postgres schema/repository

涉及：`src/state/postgres.rs`、`src/state/store.rs`、`src/state.rs`、必要的 `src/state/file_store.rs`。

- 增加单独 migration version，保证重复启动幂等。
- 提供 attempt 原子消费、identity upsert、绑定查询、session 创建/查询/撤销、过期清理。
- 所有 SQL 使用参数绑定；事务覆盖“查找/创建用户 + 更新 identity + 创建 session”关键链路。
- 文件模式按 §3.3 的选择实现；不得让 OAuth state/session 在无告警情况下丢失。

### T4. Axum OAuth/Portal handlers

涉及：`src/server/portal.rs`、`src/server/gateway.rs`、新测试辅助模块。

- 注册 start/callback/session/logout 和可选 downstream selection 路由。
- 使用 cookie extractor/response headers 正确设置和清除 Cookie。
- 统一 `401/403/400/502/503` 错误 code；provider 失败日志包含 request id/provider/error class，不含 token、code、claims 原文。
- 将现有 Portal API 的 `extract_downstream_id_from_bearer` 改造成“优先服务端 session，兼容旧 Bearer JWT/下游密钥”的分层解析；旧登录端点的兼容期和弃用策略写入文档。
- 服务端从 session 查询绑定，不接受客户端 employee_id 作为授权事实。

### T5. Frontend login/session flow

涉及：`frontend/src/views/portal/PortalLogin.vue`、`frontend/src/api/portal.ts`、`frontend/src/router/index.ts`、
必要时 `frontend/src/views/portal/Portal.vue` 和新 composable/store。

- 登录页展示配置的 OAuth provider 按钮，点击只跳转 `/api/portal/oauth/{provider}/start`。
- callback 由后端完成 code exchange；前端只处理安全的 `oauth_error` 短码并调用 `/portal/session`。
- 改用 Cookie session；不把 OAuth access token、ID token、session id 写入 localStorage。
- 路由 guard 使用 `/portal/session` 的状态（首屏可显示 loading），不能把“localStorage 有字符串”当认证结论。
- 现有工号/密钥登录保留，成功后也应迁移到同一服务端 session 或明确记录兼容路径，避免两套 Portal 授权语义长期分叉。
- logout 调用后端 `/portal/logout`，再清理旧兼容 token 和本地展示信息。

### T6. 运维文档和配置样例

涉及：`README.md`、`docker-compose.yml` 或样例 env 文件、必要时 `SECURITY.md`。

- 写清 provider 注册的 callback URL、反向代理 HTTPS、Cookie Secure、时钟同步、Postgres 要求和密钥轮换。
- 说明 `PORTAL_OAUTH_ENABLED=false` 时行为、无绑定用户的处理和旧登录兼容期。
- 不在仓库提交真实 client secret、真实 issuer 私钥或 token。

## 8. Security requirements

以下是强制验收项，不得作为“后续优化”：

1. state 至少 256 bits 熵、服务端保存 hash、单次消费、TTL 默认 5 分钟、constant-time 比较。
2. PKCE 只允许 S256；verifier 服务端加密保存或仅保存在受保护的短期存储中。
3. OIDC 必须校验 issuer、audience、nonce、签名算法、签名、`exp`/`iat`；禁止 `alg=none` 和不受限的算法回退。
4. callback 不允许 open redirect；`return_to` 只允许站内相对路径/hash route。
5. session id 不透明、高熵、数据库只保存 hash；Cookie `HttpOnly; SameSite=Lax; Secure`（生产）。
6. 登录、callback、session、logout 有合理的速率限制和超时；provider 失败要防止重试风暴。
7. 日志脱敏：不写 authorization code、state、verifier、nonce、access/refresh/ID token、email 全量和原始 provider body。
8. session 固定攻击防护：OAuth 回调成功后必须创建新 session，不能沿用登录前的 session id。
9. 账号停用、解绑 downstream、session revoke 的行为要在每次 Portal API 请求重新检查，不能只在登录时判断。
10. 邮箱域名白名单只是额外约束，不能替代 `(issuer, subject)` 唯一身份和显式资源绑定。

## 9. Test plan

### 9.1 Rust unit tests

- PKCE verifier/challenge 生成和 RFC 7636 S256 结果。
- state/session hash 不可逆、constant-time compare 的成功/失败/过期边界。
- issuer/aud/nonce/exp/iat/算法校验；错误 token、错误 issuer、错误 audience、过期 token、nonce 重放均拒绝。
- `return_to` 接受 `/portal`、`/#/portal` 等安全路径，拒绝 `https://evil.example`、`//evil.example`、反斜杠绕过和控制字符。
- claim 白名单与 subject 规则；同 email 不同 subject 不能合并身份。

### 9.2 Axum integration tests

新增 `tests/portal_oauth.rs`，使用本地 mock IdP（authorization、token、JWKS、userinfo）覆盖：

1. OAuth 未启用/配置不完整返回预期状态，不泄露配置细节。
2. start 生成正确 `state`、S256 challenge、nonce、scope、redirect URI，并保存 attempt。
3. callback 成功建立 session Cookie；浏览器随后访问 `/api/portal/session` 和现有 `/portal/overview` 成功。
4. 缺 code/state、state 错误、state 过期、state 重放、provider 不匹配均拒绝。
5. token endpoint 错误、JWKS 错误、签名/issuer/aud/nonce/过期错误均不建 session。
6. OAuth 用户无 downstream 绑定返回 `403 portal_access_not_granted`，不会创建 API key。
7. disabled 用户、disabled downstream、已撤销 session 不能访问现有 Portal API。
8. logout 撤销 session、清 Cookie，重复 logout 幂等。
9. 并发两次 callback 只有一次成功消费 attempt，不能产生两个有效 session。
10. 多用户/多 provider 同 email 场景按 subject 隔离；显式绑定才能访问正确 downstream。
11. 无 Postgres 文件模式符合 §3.3 约定，不出现重启后静默失效或跨实例误用。
12. 现有 `portal/login`、Portal Bearer JWT 兼容测试和管理员登录测试全部继续通过。

### 9.3 Frontend tests

更新/新增 `frontend/tests/views/portal-ui.spec.ts`、`frontend/tests/views/portal-integration.spec.ts`、
`frontend/tests/api/portal.spec.ts`：

- 登录页显示 OAuth 按钮且跳转到后端 start endpoint。
- 不从 OAuth callback URL 读取或保存 token；只调用 session API。
- session loading、未登录、无资源授权、401、logout 和错误短码显示正确。
- 路由 guard 不因伪造 localStorage 值放行 Portal 页面。
- 旧工号/密钥登录 UI 仍可用。

### 9.4 Verification commands

实现模型完成后至少运行：

```bash
cargo fmt --check
cargo test --test portal_oauth
cargo test --test portal_api --test portal_flow --test admin
cargo test
cd frontend && npm run type-check && npm test
```

若环境没有 Postgres，必须明确报告跳过的集成测试，并用临时 Postgres 服务或 CI service 完成 schema/repository 验证；
不能把“编译通过”当成 OAuth 功能完成。

## 10. Acceptance criteria

只有满足以下全部条件才能标记完成：

- 配置一个标准 OIDC provider 后，用户能从 Portal 登录页完成授权并进入 Portal。
- callback 只接受一次，state、PKCE、nonce、issuer、audience、签名和时间校验均有测试证据。
- 浏览器端没有 OAuth token 或明文 session id；现有 Portal API 使用服务端 session 完成授权。
- 身份没有显式 downstream 绑定时明确拒绝，不能越权访问其他用户资源。
- 重启/多实例场景下 Postgres session 和 attempt 仍有效且不会重复消费。
- 旧工号/密钥 Portal 登录及管理员登录不回归。
- 日志、错误响应、配置输出中没有敏感认证材料。
- README/SECURITY 中记录部署、反向代理、Cookie、迁移、密钥轮换和回滚方式。

## 11. Rollback and rollout

1. 默认关闭 `PORTAL_OAUTH_ENABLED`，先只部署 schema 和未启用代码。
2. 在测试/预发布 provider 上验证 callback、session、绑定和日志脱敏，再按 provider 开启。
3. 关闭开关可停止新 OAuth 登录；已经建立的 session 按 TTL 或显式 revoke 处理。
4. 数据库表采用 additive migration，不删除现有 downstream 或登录字段；回滚代码时保留新表以避免破坏既有登录。
5. 如发现 session/绑定错误，先关闭 OAuth、撤销相关 sessions，再修复代码；不得直接清空 `portal_users` 或 `portal_identities`。

## 12. Non-goals and decisions

- 本期不把管理员用户名/密码登录改成 OAuth，也不修改 `/api/admin/login` 的 JWT 契约。
- 本期不实现 provider 管理后台；provider 通过受保护的部署环境配置，后续可独立增加 admin UI。
- 本期不把 OAuth access token 用于调用上游模型；Portal OAuth 只证明 Portal 用户身份。
- 不采用“前端拿 authorization code 再交给后端”的方案，避免 code、client secret 和 PKCE 生命周期落到浏览器业务代码。
- 不采用无 state 的隐式流（Implicit Grant），不采用 Resource Owner Password Credentials。
- 不用 email 单独自动映射 downstream；如产品确实需要自动 provisioning，必须另行设计管理员批准、域名验证和审计策略。



---

## 实施回填（Implementation Log）

> 第一阶段 T1–T6 在 `feat/portal-oauth-login` 分支完成并合并到 main 前按序提交。
> 每个提交均可独立编译 + 测试通过。

### 任务回填

| 任务 | 提交 | 状态 |
|------|------|------|
| T1 认证领域模型与密码学原语 | `a1240314` | ✅ |
| T2 Provider 配置与 OIDC/OAuth client | `031c35d3` | ✅ |
| T3 Postgres schema/repository | `b2b1e98c` | ✅ |
| T4 Axum OAuth/Portal handlers | `5ddfe2aa` | ✅ |
| T4 补丁：端到端测试套件 + error 302 统一 + hand-written INSERT NULL 参数 | `bd20838e` | ✅ |
| T5 前端登录/会话流（OAuth 按钮、session guard、logout） | `2e7de889` | ✅ |
| T6 运维文档与配置样例（README/DEPLOYMENT/SECURITY/.env/compose） | `fb494444` | ✅ |
| 验收补强：ID Token 五项字段校验 + 会话跨重载持久化的端到端证据（§9.2.5/§10） | `b71dd4c` | ✅ |

### 验证结果（2026-08-29 实测，逐步骤独立记录退出码）

| 步骤 | 结果 | 状态 |
|------|------|------|
| `cargo fmt --check` | RC=0 | ✅ |
| `cargo clippy --all-targets -- -D warnings` | RC=0 | ✅ |
| `cargo test --lib` | 277 passed / 0 failed | ✅ |
| `cargo test --test portal_oauth` | 17 passed / 0 failed | ✅ |
| `cargo test --test portal_api` | 39 passed | ✅ |
| `cargo test --test portal_flow` | 2 passed | ✅ |
| `cargo test --test admin` | 31 passed | ✅ |
| `cargo test --test postgres_store` | 3 passed（NULL 绑定修复后） | ✅ |
| `cargo test` 完整套件（`RUST_MIN_STACK=4194304`） | 1884 passed / 0 failed / 91 ignored，RC=0 | ✅ |
| `frontend npm run type-check` | RC=0 | ✅ |
| `frontend npm test` | 38 files / 294 passed | ✅ |
| `frontend npm run build` | RC=0 | ✅ |
| live Redis 套件（`--test redis_runtime -- --ignored`） | 未执行（本任务不涉及 Redis 后端；无 Redis 环境） | 未执行 |

> 验收补强：mock IdP 新增 `IdTokenMode` 故障注入（WrongIssuer/WrongAudience/Expired/WrongNonce/WrongSignature），端到端证明五项均拒绝且不建 session（§9.2.5「签名/issuer/aud/nonce/过期错误均不建 session」的字面证据）；`alg=none` 由 `openidconnect` 依据 discovery 声明的 `id_token_signing_alg_values_supported=["RS256"]` 拒绝，不在允许算法集合内即失败。另补 `session_survives_app_state_reload_restart_multi_instance` 覆盖 §10「重启/多实例」验收项。速率限制（§8.6）逻辑由 `PortalOauthLimiter` 单元测试 `limiter_allows_burst_then_denies` / `limiter_is_keyed_independently` 覆盖。

> 注：`tests/troubleshooting.rs` 的 `admin_compatibility_matrix_uses_gateway_protocol_selection_metadata`
> 在默认 2MB 测试线程栈下栈溢出（**预存在问题**，与本次 OAuth 改动无关）：stash 掉全部
> 未提交改动后在 HEAD 上仍复现，且 `RUST_MIN_STACK=3MB` 即通过——是深而非无限的递归，
> 非本任务引入。完整套件用 `RUST_MIN_STACK=4194304` 跑通。

### 现场验证表（由运维在内网部署后填写，实施方不填）

| 检查项 | 结果 |
|--------|------|
| 在 IdP 注册 callback URL 后，用户从 `/portal/login` 用“使用企业账号登录”完成授权并进入 Portal | |
| callback 重放/错误 state 均被拒绝，日志不含 code/token/claims 原文 | |
| 未绑定下游资源的身份返回 `403 portal_access_not_granted`，不创建密钥 | |
| 旧「工号 + 下游密钥」登录仍可用 | |
| logout 后重放会话 Cookie 访问 Portal API 返回 401 | |
| 重启/多实例下 session 与 attempt 行为符合预期 | |

