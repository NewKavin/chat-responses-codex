# Portal OAuth 登录 + 自助 Key：整合实施方案

- 日期：2026-09-01
- 状态：**待执行**
- 整合来源：
  - `docs/superpowers/plans/2026-08-28-portal-oauth-login.md`（第一阶段，OAuth 登录）
  - `docs/superpowers/plans/2026-08-28-portal-self-service-keys-and-model-groups.md`（第二阶段，模型分组 + 自助 key）
- 目标形态（用户需求原话）：支撑登录、用户自助创建 key、管理员管理用户；
  OAuth 支持**关闭/开启登录**，不能随便一个人就能登录注册；参考 new-api 的开源项目形态。

## 0. 先纠正一处事实：第一阶段代码不在 main 上

原方案 1 的状态行写「已实施并提交（2026-08-29）；第一阶段 T1–T6 全部落地」。
**这句在 `main` 视角下是错的**，会误导实施者以为可以直接从第二阶段开始。实测：

| 核查项 | 结果 |
| --- | --- |
| `grep -rln "oauth" src/` on `main` | **零命中** |
| `Cargo.toml` 的 `openidconnect` / `oauth2` 依赖 | **不存在** |
| `portal_users` / `portal_identities` / `portal_sessions` 表 | **不存在** |
| `git log --all --grep=oauth` | 8 个提交，`a1240314`..`b71dd4cb` |
| 这些提交所在分支 | **`feat/portal-oauth-login`**，未合入 main |

所以「搞了一半」的准确含义是：**第一阶段代码已完整实现，但滞留在分支上；第二阶段只有方案、无代码。**

分支状态（这是本方案最重要的输入）：

| 指标 | 值 |
| --- | --- |
| 领先 main | 8 个提交 |
| **落后 main** | **45 个提交**（分叉点 `004d9d2b`） |
| 改动规模 | 31 文件，+5971 / −74 |
| 新增后端模块 | `src/portal_auth.rs`(516) `src/portal_oauth.rs`(609) `src/server/portal_oauth.rs`(772) `src/state/portal_store.rs`(188) |
| 新增测试 | `tests/portal_oauth.rs`(1528) |
| 文本冲突 | **无**（`git merge-tree --write-tree` RC=0） |

## 1. 结论先说：先合分支，不要重写

第一阶段那 6000 行是已实现且带 1528 行集成测试（含 mock IdP）的完整功能，
**重写它没有任何收益**。本方案的第一件事就是把 `feat/portal-oauth-login` 接回 main。

文本层面可自动合并，但有 4 个文件双方都改过，属于「自动合并成功、语义仍需人看」：

| 文件 | main 侧改动（我方近期） | 分支侧改动 |
| --- | --- | --- |
| `src/state/types.rs` | E4.3 三个队列设置 | 10 个 `portal_oauth_*` 配置 |
| `src/main.rs` | E4.3 env 读取 | OAuth env 读取 |
| `frontend/src/types/index.ts` | E4.3 三个设置类型 | OAuth session 类型 |
| `frontend/src/router/index.ts` | 未改 | Portal 路由守卫改造 |

前三个是**同一结构体的不同字段**，互不覆盖，合并后编译即可验证。

## 2. 第一阶段已实现的能力（核实过，不是照抄文档）

三层准入控制都已落地，正对应你「不能随便一个人就能登录注册」的要求：

| 控制点 | 位置 | 默认值 | 作用 |
| --- | --- | --- | --- |
| 总开关 | `DEFAULT_PORTAL_OAUTH_ENABLED`（`types.rs:151`） | **`false`** | OAuth 登录默认关闭，不配不开 |
| 邮箱域名白名单 | `allowed_email_domain()`（`server/portal_oauth.rs:590`） | 空=不限 | 只放行指定域名（支持子域） |
| 自动开户 | `default_portal_oauth_auto_provision()` | **`false`** | 认证通过 ≠ 自动建账号 |
| 未授权响应 | `portal_access_not_granted`（`:411,523,687`） | — | 已认证但无绑定时明确拒绝，不自动发 key |

协议层用 `openidconnect` + `oauth2` crate（Authorization Code + PKCE/S256），
session 走服务端存储 + HttpOnly Cookie，access token 不下发前端。
四张表：`portal_users` / `portal_identities` / `portal_user_downstreams`（多对多，含 `is_default`）/ `portal_sessions`。

分支实际新增的路由**只有 4 条，全在用户侧**（核实自 `src/server/gateway.rs` 的 diff）：

| 路由 | 方法 |
| --- | --- |
| `/api/portal/oauth/{provider}/start` | GET |
| `/api/portal/oauth/{provider}/callback` | GET |
| `/api/portal/session` | GET |
| `/api/portal/logout` | POST |

另有内置速率限制 `PortalOauthLimiter::new(300, 60)`（防授权端点被刷）。

**关键缺口：没有任何 `/api/admin/portal*` 端点** —— 即管理员无法管理 Portal 用户。这是 P1 的依据。

## 3. 与你要求不一致的一处：OAuth 配置没进网关设置

你明确要求过「参数开关不要写死，要做到网关设置中」。但实测分支的 10 个 `portal_oauth_*`
**只进 `AppConfig`，不进运行时设置** —— 证据是分支没碰
`tests/runtime_settings.rs` / `tests/admin_runtime_settings.rs` 的计数断言。

后果：**OAuth 开关只能改环境变量 + 重启，不能在管理界面热改。**

这不是 bug（第一阶段方案本来就这么设计的），但和你的要求冲突，所以列为本方案的 P2 任务。
注意区分哪些该热改、哪些不该：

| 配置 | 是否该进运行时设置 | 理由 |
| --- | --- | --- |
| `portal_oauth_enabled` | **应该** | 「支持关闭/开启登录」是你的明确需求，出事要能立刻关 |
| `portal_oauth_auto_provision` | **应该** | 控制能否自助注册，属日常运营开关 |
| `portal_oauth_allowed_email_domains` | **应该** | 加减部门域名不该重启 |
| `portal_session_ttl_seconds` | 应该 | 纯数值策略 |
| `portal_oauth_client_secret` | **不应该** | 密钥不进可读设置接口，留在 env |
| `portal_oauth_issuer_url` / `client_id` / `redirect_url` / `userinfo_url` | 不应该 | 换 IdP 是部署变更，且要重建 OIDC client |

## 4. 第二阶段（自助 key + 模型分组）：只有方案，无代码

强依赖第一阶段那四张表和服务端 session —— 没有可信的「这个请求是谁发的」，
自助建 key 就是无主资源。所以**顺序不可颠倒**。

方案 2 的核心设计（已读过，判断是合理的，保留）：

- **一把 key = 一条 `downstreams` 记录**，复用现有数据面，不改网关热路径；
- **容量按「人」聚合而非按 key**（方案 2 §3.3 自称最关键的一条）——
  否则用户建 10 把 key 就能拿到 10 倍并发，配额形同虚设；
- **存量用户认领流程**（§3.5a）——首次 OAuth 登录且无绑定时，引导用户输入现有「工号 + key」，
  复用 `state.downstream_for_secret()` + `downstream.id == employee_id` 校验（`src/server/portal.rs:50-76`，
  **注意原方案写的 `:52-76` 行号已偏移，实际函数从 50 行开始**）确认其确实持有该 key，再建绑定；
- **数据面零影响**是业务连续性的基石，新增字段全部 `#[serde(default)]`。

## 5. 任务分解

### P0 — 把 `feat/portal-oauth-login` 接回 main（最优先，阻塞其余全部）

分支落后 45 个提交，所以**先在分支上 rebase/merge main，在分支上跑通全量，再合回 main**，
不要直接把分支合进 main 然后在 main 上救火。

1. 新建工作分支 `feat/portal-oauth-integration`，基于当前 `main`；
2. 把 `feat/portal-oauth-login` 合入该分支（文本无冲突，但需处理下述语义点）；
3. 补 `Cargo.toml` 的 `openidconnect` / `oauth2` 依赖（main 上没有，锁定确切版本，不用开放区间）；
4. 编译 + 跑全量。**基线：main 当前是 62 套件 / 1855 passed / 0 failed / 99 ignored**；
   合入后套件数会变成 63（多 `tests/portal_oauth.rs`），passed 数应显著上升；
5. 逐个确认这 4 处语义合并点：
   - `src/state/types.rs`：E4.3 三设置与 10 个 `portal_oauth_*` 应同时存在；
   - `src/main.rs`：两侧 env 读取都在；
   - `frontend/src/types/index.ts`：`RuntimeSettings` 里 E4.3 三字段仍在（**前端有字段数断言 79，
     缺字段会导致 `vue-tsc` 失败**——这个坑我这轮踩过一次，见 `c74d21bc`）；
   - `frontend/src/router/index.ts`：Portal 守卫改为服务端 session 校验后，不能破坏管理员路由。

**验证纪律**：`cargo fmt --check`（只 `-p chat-responses-codex`，不用 `--all`）、
`cargo clippy --all-targets`、`cargo test` 各跑一次独立记录退出码，不用 `&&` 串联。
前端另跑 `npx vue-tsc` 和 `npx vitest run`（**这两项 `cargo test` 覆盖不到**）。

### P1 — 管理员管理用户（你的明确需求，第一阶段缺）

第一阶段只做了登录与绑定，**没有管理员侧的用户管理界面**。需要：

- 列出 Portal 用户（来源 provider、subject、email、启停、最后登录）；
- 启用/禁用某用户（禁用后 session 立即失效，不能只挡新登录）；
- 查看/编辑某用户的下游绑定（`portal_user_downstreams`，含切换 `is_default`）；
- 手工为用户建立绑定（`auto_provision=false` 时这是唯一入路，**必须有，否则没人能进来**）。

最后一条是 Day-1 阻塞项：默认不自动开户，若管理员又无法手工绑定，则所有人都被拒在门外。

### P2 — 把该热改的 OAuth 开关接进运行时设置（§3）

按 §3 的表格，只把 4 项接进运行时设置，密钥与 issuer 类留在 env。
接线点共 10 处，照 E4.3 的现成路径抄（见
`docs/superpowers/plans/2026-08-31-local-slot-gate-false-429.md` §10.2 的清单）。

注意：改运行时设置字段数会同时打破**后端**（`tests/runtime_settings.rs`、
`tests/admin_runtime_settings.rs`）和**前端**（`runtimeSettings.spec.ts` 的 79/66 断言）三处计数断言。
小数类设置在前端描述符里必须写 `integer: false`，否则管理界面会拒掉自己的默认值（`c74d21bc` 修过一次）。

### P3 — 第二阶段：模型分组 + 自助 key

依赖 P0 完成。按方案 2 的 S 系列任务推进，其中两条不可省：
- **容量按人聚合**（方案 2 §3.3）；
- **存量用户认领流程**（§3.5a），否则老用户会在切换后全部登录失败。

## 6. 风险

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **分支落后 45 个提交** | 文本虽无冲突，但 45 个提交里含 SSE decode、E4.3 等实质改动，语义面可能有交互 | P0 要求在分支上先合 main 并跑通全量，再合回；不要在 main 上救火 |
| **文件模式（无 PG）行为** | 方案 1 §3.3 要求 OAuth 端点在文件模式返回 `503 oauth_requires_durable_store`，不静默回退 | 合并后要实测：不设 `DATABASE_URL` 时 OAuth 端点必须 503，且**工号/密钥登录不受影响** |
| **默认不自动开户导致无人可登录** | `auto_provision=false` 且管理员无手工绑定入口 = 全员被拒 | P1 的手工绑定是 Day-1 阻塞项，不能挪到 P3 |
| **禁用用户只挡新登录** | 若禁用不清 session，已登录用户仍能用 | P1 明确要求禁用即失效现有 session |
| **前端断言与类型是隐藏地雷** | `cargo test` 不覆盖前端；字段缺失会让 `vue-tsc` 直接失败 | 每次改设置都要跑 `npx vue-tsc` + `npx vitest run` |
| **旧登录方式何时下线** | 两套登录并存期间，`portal_login` 仍签发 `generate_admin_token`（`src/server/portal.rs`），与 OAuth 的 session 模型并存 | 保持并存，不在本方案内下线；下线需单独评估 |

**回滚**：P0 是分支合并，`git revert` 或直接弃用整合分支即可，main 不受影响。
OAuth 总开关默认 `false`，即使代码合入 main，未配置时功能不激活 —— 这是天然的安全边界。

## 7. 需要你确认的点

1. **IdP 是什么** —— 内网自建（Keycloak / Authentik / 钉钉 / 企业微信 / LDAP 网关）还是别的？
   这决定走 OIDC discovery 还是纯 OAuth2 + userinfo 兼容模式（方案 1 §3.1 两条路都留了）。
2. **旧的「工号 + key」登录要不要保留** —— 方案 1 的设计是保留并存。若要下线需单独评估。
3. **new-api 的哪些行为要对齐** —— 你提到参考 new-api。它的用户体系较重（邀请码、分组倍率、
   令牌额度等）。本方案目前只覆盖你明说的「登录 + 自助建 key + 管理员管人」，
   若还要邀请码/额度倍率等，需要另加任务。

## 8. 执行顺序

```
P0（合分支，阻塞一切）
  └─ P1（管理员管人，Day-1 必需）
       ├─ P2（OAuth 开关进运行时设置，你的明确要求）
       └─ P3（第二阶段：模型分组 + 自助 key）
```

P2 与 P3 可并行；P1 不能后置，否则功能上线即无人可登录。
