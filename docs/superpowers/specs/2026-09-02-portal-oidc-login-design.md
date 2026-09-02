# Portal OIDC 登录（重写版）：设计方案

- 日期：2026-09-02
- 状态：**待实施**
- 开发分支：`feat/portal-oidc-v2`（从 `main` 拉出）
- 取代：`docs/superpowers/plans/2026-09-01-portal-oauth-consolidated.md`
- 被取代方案的代码：`feat/portal-oauth-login` 分支（**保留作参考，不合入**）

## 0. 为什么推翻上一版

上一版的结论是「先把 `feat/portal-oauth-login` 合回 main，不要重写」。该结论已被否决，改为**代码全弃、方向保留 OIDC、重新实现**。

旧分支的状态（仍然属实，供参考时判断）：8 个提交、31 文件、+5971/−74、含 `tests/portal_oauth.rs`(1528 行，带 mock IdP)、**落后 main 45 个提交**。它用 `openidconnect` + `oauth2` crate 走严格 OIDC（PKCE + id_token 验签 + nonce）。

**推翻它的技术理由（不只是「旧」）**：那套严格实现与内网要对接的 IdP 形态不匹配，见 §1。另外它有两个已知缺口：无任何 `/api/admin/portal*` 端点；10 个 `portal_oauth_*` 仅进 `AppConfig`、不进运行时设置（改开关要重启）。

## 1. 与 new-api 对齐：核实结论（本方案的硬约束）

需求方明确要求「和 new-api 的 OAuth 方案一致，别开发完内网不能用」。以下为**读源码核实**的结果，非文档转述。仓库已改名为 `QuantumNous/new-api`（原 `Calcium-Ion/new-api`，GitHub 301 重定向）。

来源：`oauth/oidc.go`、`oauth/generic.go`、`controller/oauth.go`、`controller/custom_oauth.go`、`model/custom_oauth_provider.go`。

| 环节 | new-api 实际做法 | 本方案采纳 |
| --- | --- | --- |
| **身份来源** | 用 access_token 调 **userinfo 端点**，取 `sub`/`email`/`name`/`preferred_username` | **采纳。这是最关键的一条。** |
| **id_token** | 收下但**不验签、不参与身份判定** | 采纳：可选验签，**默认关** |
| **PKCE** | **完全不发**（无 `code_verifier`/`code_challenge`） | 折中：做成开关，**默认开**（理由见 §5.1） |
| CSRF | 服务端 state 记录，TTL 10 分钟，一次性消费；有 `login`/`bind` 两种 intent | 采纳（含 bind intent，见 §4.3） |
| 客户端凭据位置 | 默认 form body；`auth_style`：0=auto / 1=params / 2=Basic 头 | 采纳，可配 |
| 端点配置 | 三端点手工可配 **且** 支持 well-known 自动填充 | **两条路都做**（见 §5.2） |
| 字段映射 | `user_id_field`（默认 `sub`，**支持 `data.user.id` 这类 JSON 路径**）、`email_field`、`username_field`、`display_name_field` | 采纳 |
| 硬性要求 | `sub` 与 `email` **均不得为空**，否则登录失败 | 采纳，并在错误信息里点名是哪个字段空 |
| scopes | 默认 `openid profile email` | 采纳为默认值，可配 |
| 会话 | 服务端 session + SID，支持 `RevokeUserSession` 撤销 | 采纳（见 §3、§4.4） |

**互操作性结论**：只要 IdP 能被 new-api 接上，就能被本方案接上——因为身份判定路径（authorization code → token → userinfo）完全相同，且比 new-api 多出的两项（PKCE、id_token 验签）都可关闭。

## 2. 范围

**本轮做**：
1. OIDC 登录（Keycloak / Authentik 等自建 IdP，走 discovery 或手工端点）
2. 管理员管理 Portal 用户（列表、启停、绑定/解绑 key、设默认 key）
3. 已有账号绑定 OIDC 身份（bind 流程，存量用户迁移的主路径）

**本轮不做**（后续另开一轮）：用户自助创建/吊销 key、模型分组、分组倍率、邀请码、额度体系。

**保持不变**：现有「工号 + key」登录（`src/server/portal.rs:50` `portal_login`）原样保留，两套并存，不下线。现有 10 条 `/api/portal/*` 接口的行为不变。

## 3. 数据模型

4 张表，建表沿用现有方式：`src/state/postgres.rs` 内的 `CREATE TABLE IF NOT EXISTS` + `schema_migrations` 版本号（当前最高版本见该文件 `:1818` 起），**不引入 sqlx / 迁移框架**。

```
portal_users
  id              TEXT PRIMARY KEY        -- 内部 uuid
  email           TEXT NOT NULL
  display_name    TEXT
  username        TEXT
  disabled        BOOLEAN NOT NULL DEFAULT FALSE
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
  last_login_at   TIMESTAMPTZ
  UNIQUE(email)

portal_identities                          -- 一个用户可绑多个 IdP 身份
  provider        TEXT NOT NULL            -- 目前恒为 'oidc'，预留多 provider
  subject         TEXT NOT NULL            -- IdP 的 sub（按 user_id_field 取值）
  user_id         TEXT NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
  PRIMARY KEY(provider, subject)

portal_user_downstreams                    -- 用户 ↔ 现有 key（downstreams 表）多对多
  user_id         TEXT NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE
  downstream_id   TEXT NOT NULL
  is_default      BOOLEAN NOT NULL DEFAULT FALSE
  PRIMARY KEY(user_id, downstream_id)

portal_sessions
  sid             TEXT PRIMARY KEY         -- 随机 256bit，仅存哈希见下
  user_id         TEXT NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
  expires_at      TIMESTAMPTZ NOT NULL
  last_seen_at    TIMESTAMPTZ
  user_agent      TEXT
  ip              TEXT
```

**约束与注意**：
- `portal_sessions.sid` 存**哈希值**（SHA-256），Cookie 里才是原值——数据库泄露不等于会话被接管。
- **不新增字段到 `downstreams` 表**：绑定关系放在独立的 `portal_user_downstreams`，避免动网关热路径读的结构。
- OIDC 的 `state` 记录（对齐 new-api 的 `AuthFlow`）不落库，用进程内 `Mutex<HashMap>` + TTL 10 分钟即可；网关目前是**单实例聚合形态**，多实例部署时再迁移到 Redis（在文档中写明这一限制）。

**存储后端**：Postgres-only。未配置 `DATABASE_URL`（文件模式）时，所有 OIDC 端点返回 `503` + `oidc_requires_durable_store`，**不静默回退**；工号+key 登录与其余 portal 接口不受影响。

## 4. 流程

### 4.1 登录（login intent）

```
GET /api/portal/oidc/start?intent=login
  → 生成 state（进程内，TTL 10min，一次性）
  → 302 到 authorization_endpoint
       ?response_type=code&client_id=..&redirect_uri=..&scope=..&state=..
       [&code_challenge=..&code_challenge_method=S256]   ← PKCE 开关打开时才带

GET /api/portal/oidc/callback?code=..&state=..
  1. 校验并消费 state（不匹配/过期/已用 → 400）
  2. POST token_endpoint（auth_style 决定凭据放 body 还是 Basic 头）
  3. GET userinfo_endpoint，Authorization: Bearer <access_token>
  4. 按字段映射取 subject / email / username / display_name
     sub 或 email 为空 → 400，错误信息指明哪个字段空
  5. 邮箱域名白名单（含子域匹配）不通过 → 403 portal_email_domain_not_allowed
  6. 查 portal_identities：
       命中   → 取 user
       未命中 → 若 portal_oidc_registration_enabled = false
                   → 403 portal_registration_disabled（明确告知需管理员开通或先绑定）
                 否则新建 portal_users + portal_identities
  7. user.disabled → 403 portal_user_disabled
  8. 用户无任何 downstream 绑定 → 403 portal_access_not_granted（绝不自动发 key）
  9. 建 session，写 HttpOnly Cookie，302 回 /portal
```

### 4.2 会话校验

新增 `resolve_portal_session()`：读 Cookie → 查 `portal_sessions`（未过期）→ 查 `portal_users`（未禁用）→ 取默认绑定的 `downstream_id`。

接入点是 `src/server/portal.rs:620` 的 **`extract_downstream_id_from_bearer()`**（该函数是所有 portal 接口取身份的唯一入口，现有两条分支：`eyJ` 开头走 `verify_admin_token`、否则当作裸 downstream key 查 `downstream_for_secret`）。改造方式：**在这两条分支之前先试 Cookie 会话**，命中则返回默认绑定的 downstream id。注意该函数签名只收 `&HeaderMap`，Cookie 也从 headers 里取，签名无需改动。这样现有 10 条 portal 接口与前端页面**零改动**即可在 OIDC 会话下工作。

### 4.3 绑定已有账号（bind intent）—— 存量用户迁移主路径

用户先用现有「工号 + key」登录 Portal，页面上点「绑定 OIDC 账号」：

```
GET /api/portal/oidc/start?intent=bind   （需已登录：带旧 JWT 或已有会话）
  → state 记录里记下「要绑到哪个 downstream_id」
callback 时：
  → 拿到 (provider, subject)
  → 若该身份已绑到别人 → 409 portal_identity_already_bound
  → 否则：建/复用 portal_users，写 portal_identities，
          并把该 downstream_id 写入 portal_user_downstreams（is_default=true）
```

**这条路径的价值**：`portal_oidc_registration_enabled` 可以一直保持关闭，存量用户仍能自助完成迁移，管理员不必逐个手工录入。管理侧手工绑定（§4.5）作为兜底保留。

### 4.4 禁用即失效

管理员禁用用户时，**同一事务内删除该用户全部 `portal_sessions`**。语义对齐 new-api 的 `RevokeUserSession`。仅置 `disabled=true` 而不清 session 属于实现缺陷，验收必须覆盖。

### 4.5 管理员接口

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/admin/portal/users` | 列表：email、display_name、provider/subject、disabled、last_login_at、绑定数；支持分页与关键字 |
| PATCH | `/api/admin/portal/users/{id}` | 启用/禁用（禁用即清 session） |
| GET | `/api/admin/portal/users/{id}/bindings` | 该用户的 key 绑定 |
| POST | `/api/admin/portal/users/{id}/bindings` | 新增绑定 `{downstream_id, is_default}`；`downstream_id` 必须已存在 |
| DELETE | `/api/admin/portal/users/{id}/bindings/{downstream_id}` | 解绑；解绑默认项后须保证剩余项里仍有且仅有一个默认 |

全部挂现有 `admin_auth_middleware`（用法见 `src/server/gateway.rs:2277` 附近的 `route_layer` 写法）。

**Day-1 阻塞项**：`portal_oidc_registration_enabled` 默认 false，若管理侧绑定接口缺席且 bind 流程未做，则无人能进入系统。这两条至少要有一条可用。

## 5. 配置

### 5.1 进运行时设置（可热改，管理界面可见）

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `portal_oidc_enabled` | `false` | 总开关。出事能立刻关 |
| `portal_oidc_registration_enabled` | `false` | **是否允许 OIDC 注册新用户**。关闭时，未绑定身份的人一律 403，只能走 §4.3 绑定或管理员开通 |
| `portal_oidc_allowed_email_domains` | 空（不限） | 逗号分隔；空=不限制；匹配含子域 |
| `portal_session_ttl_seconds` | `86400` | 会话有效期 |
| `portal_oidc_pkce_enabled` | `true` | 见下 |
| `portal_oidc_verify_id_token` | `false` | 存在 id_token 时是否验签；默认关以对齐 new-api |

**PKCE 默认开的理由**：Keycloak / Authentik 均支持 PKCE；对不要求 PKCE 的 IdP，多带 `code_challenge` 会被忽略，不影响成功率。因此「默认开 + 可一键关」比 new-api 的「完全不发」更安全，且互操作性不降低。现场若遇到拒绝，关掉即可，无需改代码。

### 5.2 留在环境变量（不进可读设置接口）

`PORTAL_OIDC_CLIENT_ID`、`PORTAL_OIDC_CLIENT_SECRET`、`PORTAL_OIDC_REDIRECT_URL`、
`PORTAL_OIDC_ISSUER_URL`（用于 discovery）、
`PORTAL_OIDC_AUTHORIZATION_ENDPOINT` / `PORTAL_OIDC_TOKEN_ENDPOINT` / `PORTAL_OIDC_USERINFO_ENDPOINT`（手工模式）、
`PORTAL_OIDC_SCOPES`（默认 `openid profile email`）、
`PORTAL_OIDC_AUTH_STYLE`（`auto` / `params` / `basic`，默认 `auto`）、
字段映射 `PORTAL_OIDC_USER_ID_FIELD`（默认 `sub`）/ `_EMAIL_FIELD` / `_USERNAME_FIELD` / `_DISPLAY_NAME_FIELD`。

**端点解析优先级**：三个端点若显式配置则直接用；否则用 `ISSUER_URL` 拉 `/.well-known/openid-configuration` 填充。**两条路都必须实现**——手工填是内网 IdP 不规范时的救命通道。discovery 失败时错误信息要包含拉取的 URL 与 HTTP 状态码。

字段映射支持 `a.b.c` JSON 路径（对齐 new-api 的 `data.user.id` 形态）。

## 6. 接线点清单

新增运行时设置字段必须同步改动以下位置，照 `docs/superpowers/plans/2026-08-31-local-slot-gate-false-429.md` §10.2 执行；`eb5bed62` 是一次完整的现成范例（一个设置从常量到前端描述符的全链路）：

| 文件 | 内容 |
| --- | --- |
| `src/state/types.rs` | `DEFAULT_` 常量、`AppConfig` 字段 + `#[serde(default)]`、`Default` impl、`default_*()` |
| `src/state/runtime_settings.rs` | 键名清单、结构体字段、`from_config`、`apply`、`validate` |
| `src/state.rs` | 常量 `pub use` 再导出 |
| `src/main.rs` | `use` 导入、`env_*` 读取、`AppConfig` 字面量 |
| `frontend/src/types/index.ts` | TS 类型字段 |
| `frontend/src/utils/runtimeSettings.ts` | 管理界面描述符（label/control/min/max/description） |
| `tests/runtime_settings.rs` | 计数断言（当前 **79**） |
| `tests/admin_runtime_settings.rs` | 计数断言（当前 **80**） |
| `frontend/src/utils/runtimeSettings.spec.ts` | 计数断言（当前 **80** / immediate **67**）、fixture、expectedKeys |

**两个已知的坑**：小数类设置在前端描述符里必须写 `integer: false`（`c74d21bc` 修过）；前端字段缺失会让 `vue-tsc` 直接失败，而 `cargo test` 覆盖不到前端。

## 7. 任务分解

严格 TDD：每个任务先写失败测试、**亲眼确认失败原因是功能缺失**，再实现。

- **T1** 表结构 + `PortalStore`（Postgres 实现 + 文件模式的 503 语义）
- **T2** OIDC 配置解析：discovery 与手工端点两条路、字段映射、auth_style、端点缺失时的清晰报错
- **T3** `/start` + `/callback`：state 生成与一次性消费、token 交换、userinfo 取值、错误分支（§4.1 的 9 步全覆盖）
- **T4** 会话层：建/查/删 session、Cookie 属性、`resolve_portal_identity` 接入（**验证现有 10 条 portal 接口零改动仍工作**）
- **T5** bind intent（§4.3）
- **T6** 管理员 5 条接口 + 禁用即清 session
- **T7** 6 个运行时设置接线（§6 全部 9 处 + 三处计数断言）
- **T8** 前端：登录页 OIDC 入口、Portal 内绑定入口、管理端用户管理页
- **T9** 文档：部署说明、Keycloak/Authentik 配置样例、`.env` 与 compose 片段

依赖：T1 → T2 → T3 → T4 →（T5、T6、T7 可并行）→ T8 → T9。

## 8. 测试清单

正向：discovery 成功、手工端点成功、登录建会话、二次登录复用身份、bind 绑定成功。

**反向（必须覆盖，不可省）**：
1. `portal_oidc_registration_enabled=false` + 新身份 → 403，且**未建任何用户记录**
2. 用户被禁用 → 存量 session **立即**失效（非仅挡新登录）
3. 邮箱域名不在白名单 → 403（并覆盖子域应放行的用例）
4. state 不匹配 / 过期 / 重放（同一 state 用两次）→ 400
5. userinfo 缺 `sub` 或缺 `email` → 400，错误指明缺哪个
6. 认证通过但无 key 绑定 → 403，**绝不自动发 key**
7. 文件模式（无 `DATABASE_URL`）→ OIDC 端点 503，**且工号+key 登录仍正常**
8. bind 时该身份已绑他人 → 409
9. `portal_oidc_enabled=false` → `/start` 404 或 403（不得泄露 IdP 地址）
10. 关闭 PKCE 开关后请求里不带 `code_challenge`；开启后带且为 S256

测试用 mock IdP（本地 axum server 起 authorization/token/userinfo 三个端点）。旧分支 `tests/portal_oauth.rs` 的 mock IdP 思路**可参考，代码不照搬**。

## 9. 风险

| 风险 | 处置 |
| --- | --- |
| 内网 IdP 不发 `email` | 登录直接失败且信息明确；部署文档要求在 IdP 侧放开 email scope/claim |
| IdP 只有非标端点、无 discovery | §5.2 的手工端点模式；必须实测一次手工路径 |
| state 存进程内，多实例部署会失效 | 当前为单实例聚合形态，可接受；**必须在部署文档中写明**，多实例时迁 Redis |
| 禁用用户只挡新登录 | §4.4 + 测试 2 |
| 前端断言与类型是隐藏地雷 | 每次改设置必跑 `npx vue-tsc` + `npx vitest run` |
| 误合旧分支 | `feat/portal-oauth-login` 仅作参考，任何情况下不合入 |

**回滚**：`portal_oidc_enabled` 默认 false，代码合入后未配置即不激活，这是天然安全边界。整体回滚直接弃用 `feat/portal-oidc-v2` 分支，`main` 不受影响。

## 10. 验证纪律

以下各跑一次、**独立记录退出码**，不要用 `&&` 串联掩盖失败：

```
cargo fmt -p chat-responses-codex -- --check
cargo clippy --all-targets
cargo test
cd frontend && npx vue-tsc --noEmit -p tsconfig.json
cd frontend && npx vitest run
```

基线：`main` 当前 **62 套件 / 1858 passed / 0 failed / 102 ignored**；前端 **273 passed**。合入后套件数与 passed 数应上升，failed 必须为 0。

需要真实 Redis 的测试基线（本方案不涉及，但改动若波及网关需回归）：
`docker run -d --rm -p 16399:6379 redis:7-alpine`，
`TEST_REDIS_URL=redis://127.0.0.1:16399 cargo test --test redis_runtime -- --ignored` → 99 passed。
注意该套件在并行下有**既有**偶发失败（约 1/4，每次挂的测试不同），与本方案无关，判断时以干净树对照。
