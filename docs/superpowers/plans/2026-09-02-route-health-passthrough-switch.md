# 路由健康「透传模式」开关：实施计划

- 日期：2026-09-02
- 状态：**待实施**
- 开发分支：`feat/route-health-passthrough`（**从 `main` 拉出**）
- 触发事件：内网部署 main 后的现场故障，见 §1
- ⚠️ 并行开发中的分支：`feat/portal-oidc-v2`（另一人在做 Portal OIDC）。**冲突规避见 §6，必读。**

## 1. 背景：现场故障与根因

内网部署最新 main，用 glm5.2 打上游（**单个聚合网关**形态）。上游网关返 502，两次之后网关显示「路由已耗尽」，随后**不再向上游发送请求**，持续对下游 codex 返回 429。

运维诉求原话：「我想尽可能地去争取上游资源，你却给我拦截住了」。

**根因不是模型探测**（`AUTOMATIC_CAPABILITY_PROBES_ENABLED`、`UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED` 默认均为 `false`，未开启即不运行）。实际因果链，全部有代码依据：

| 步 | 行为 | 依据 |
| --- | --- | --- |
| 1 | 上游 502 归入瞬态失败类 | `is_common_mode_transient_class` |
| 2 | 路由进入 `Cooling`，时长 = `base << (step-1)` = 5s → 10s | `types.rs:100`(base=5)、`types.rs:110`(max_step=2) |
| 3 | 单请求内 2 次物理尝试打完重试预算 → 该请求 `upstream_routes_exhausted` | `route_exhaustion_retry_max_rounds`=3、`max_wait_ms`=30000 |
| 4 | 冷却窗口内，**所有**请求得 `Cooling` → 本地拒绝、零物理尝试 ← **主要拦截来源** | `route_health.rs:1006`、`:1039` |
| 5 | 冷却到期后转半开，**头 3 秒独占**（仅一个探针，其余得 `HalfOpenBusy`）；独占窗口过后其余请求会被放行 | `types.rs:102`(=3000)、`route_health.rs:1025`、`:1060` |
| 6 | `Cooling` / `HalfOpenBusy` 聚合为 `TerminalFailure::Temporary` → 下游 429/503，**零物理尝试** | `errors.rs:288` 起 |
| 7 | 放行的请求打上游仍 502 → 立即重新冷却 10 秒 → 回到第 4 步 | — |

净效果是一个占空比很低的循环：**每约 10 秒的冷却窗口里所有请求被本地拒绝**，冷却到期后只有很短的窗口能真实打上游，一次 502 又把窗口关上。从运维视角就是「网关不再向上游发送、一直 429」。

**注意主次**：拦截量的绝对主体是第 4 步的**冷却**（`cooldown_base` 5s → `max_step` 2 → 10s），半开独占窗口（3s）只是次要因素。`half_open_ttl`=300s 是**半开代次的存活时长，不是阻断时长** —— 独占窗口一过，并发请求即被放行（两个后端行为一致，见 `route_health_reserve.lua:53-56` 的注释）。这是「保护病态上游」的设计，与本次运维诉求正好相反。

**现场可先用配置缓解**（全部热改，无需重启，已核过校验边界；按影响从大到小排）：
1. `upstream_transient_route_cooldown_base_seconds` 5→**1**（主因）
2. `upstream_transient_route_cooldown_max_step` 2→**1**（主因，冷却不再升到 10s）
3. `upstream_route_half_open_exclusive_window_ms` 3000→**0**（取消探针串行化）
4. `upstream_common_mode_transient_threshold` 4→**0**（关瞬态共模熔断）
5. `upstream_route_health_half_open_ttl_seconds` 300→**5**（次要，仅缩短半开代次寿命）
本计划的目标是把这五项调参收敛成**一个语义明确的开关**。

## 2. 目标与非目标

### 目标

新增运行时设置 **`upstream_route_health_enforcement_enabled`**，默认 `true`（**完全保持现有行为**）。

置 `false` 时（透传模式）：

1. 路由健康**只记录、不阻断**。冷却时间、失败类别、连续失败计数、last_failure_* 等**照常写入**，管理界面的健康档案与统计**不受影响**（运维仍要能看到 502 的分布）。
2. `reserve()` **永不**返回 `Cooling` 或 `HalfOpenBusy`，每个请求都获得许可并真实发往上游。
3. 上游的 502 **原样透传**给下游，不再被聚合成本地 429/503。
4. 请求成功时，该路由上已记录的冷却**必须被清除**（见 §5 语义问题一）。

### 非目标（明确不做，不要顺手扩大）

- **不动本地并发闸门 / C3 队列 / 账号租约**。那是为遵守上游 `max_concurrency` 存在的，关掉会超发。它自己有独立开关（`upstream_local_gate_fast_fail_enabled`、`upstream_account_queue_enabled` 等），与本开关无关。
- 不改任何默认值。现有五项调参的默认值保持原样。
- 不动共模熔断（`upstream_common_mode_*`）。它是**请求内**的重放抑制，与跨请求的路由健康是两套机制；本开关不覆盖它。若透传模式下仍被共模熔断挡住，属另一个议题，先停下报告，不要在本次一并改。

## 3. 开关设计

| 项 | 值 |
| --- | --- |
| 键名 | `upstream_route_health_enforcement_enabled` |
| 类型 | `bool` |
| 默认 | `true`（保持现状） |
| 热改 | **是**，进 `IMMEDIATE_RUNTIME_SETTING_FIELDS` |
| env | `UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED` |
| 界面分组 | `routing` |
| 界面标签 | 建议「路由健康拦截」 |
| 界面描述 | 建议「关闭后路由健康只记录不阻断：冷却与半开状态照常统计，但不再拦截请求，每个请求都真实发往上游，上游错误原样透传。用于上游故障时最大化争取上游资源；默认开启。」 |

## 4. 实现位置

**有两份平行实现，都必须改** —— 这是本计划最容易漏的一点。只改 Rust 那份，则在启用 Redis 的部署上开关完全无效。

（现场是否启用 Redis 未经确认，请先看部署的 `REDIS_ENABLED`/`REDIS_URL`。但无论现场如何，两份实现都要改：否则这个开关会变成一个「换个部署形态就静默失效」的陷阱。）

### 4a. 本地后端：`src/state/route_health.rs`

1. `RouteHealthRegistry` 增加字段 `enforcement_enabled: bool`（默认 `true`）。
2. 热更新入口：`update_runtime_tuning()`（`route_health.rs:739`）**增加一个参数**传入该值。该函数已有 8 个参数、已被 `src/state.rs:3504` 调用，沿用同一条路径即可。
3. `reserve()`（`route_health.rs:993`）：`enforcement_enabled == false` 时，跳过 key 与 route 的 `Cooling` / `HalfOpenBusy` 四处提前返回（`:1006`–`:1065` 区间内），落到函数尾部正常构造 `Ready(HealthLease{..})`（`:1074`）。
4. `reserve_route_health_probe()`（`route_health.rs:1103`）：同样跳过其 `Cooling` / `HalfOpenBusy` 返回。透传模式下正常 `reserve()` 已不会失败，该「最后手段探针」路径基本不会被触发，但不要留下不一致的分支。
5. **`last_access` 等记账照旧更新** —— 跳过的只是「返回拒绝」，不是「跳过记录」。

**调用点无需改动**：`reserve()` 只是更多地返回 `Ready`，`RouteAvailability` 的三个分支匹配处不变。这是本设计被选中的原因之一。

### 4b. Redis 后端：Lua 脚本 + 参数传递

Redis 后端有一份**功能等价的 Lua 实现**，冷却/半开/独占窗口逻辑全在脚本里：

| 位置 | 要做的事 |
| --- | --- |
| `src/state/redis_runtime/route_health_reserve.lua` | 冷却提前返回在 `:30-32`（返回码 `'1'`=cooling）、半开忙碌在 `:41-42`（返回码 `'2'`）。两处按新 ARGV 跳过 |
| `src/state/redis_runtime/route_health_probe.lua` | 同样的 cooling / busy 提前返回，一并处理 |
| `src/state/redis_runtime.rs:1783` `reserve_route_health()` | 现有 5 个 `.arg(...)`（`lease_id`、ttl、half_open_ttl_ms、legacy 阈值、exclusive_window_ms）后**追加一个 ARGV** 传开关 |
| `src/state/redis_runtime.rs:1747` `reserve_route_health_probe()` | 同上 |
| `tuning_snapshot()` | 增加 `route_health_enforcement_enabled` 字段，与其余 tuning 值同路径下发（搜 `route_health_half_open_exclusive_window_ms` 找到该结构与它的刷新路径，照抄） |

**两个后端的语义必须完全一致**：同一组测试应能分别在本地后端与 Redis 后端（`TEST_REDIS_URL` + `#[ignore]` 套件）下验证同样的行为。§8 的第 3、4、5、6 条至少要在 Redis 侧各有一条对应测试。

## 5. 两个必须用测试定夺的语义问题

不要凭直觉选，**先写测试表达期望的行为，再实现**。

### 语义问题一：透传成功后要不要清冷却？

**要清。** 路由正在冷却、透传模式放行、请求成功 → 说明路由已恢复，冷却必须被清除，否则一旦运维把开关切回 `true`，会立刻被一段陈旧冷却挡住。

难点：`HealthLease` 尾部的 `half_open` 字段是 `key_generation.is_some() || route_generation.is_some()`（`route_health.rs:1079`），而 `reserve_expired_half_open_*` 在冷却**未到期**时不会发放 generation，所以透传放行拿到的是一个 `half_open == false` 的普通租约，走的不是半开成功路径 —— **默认不会清冷却**。

实现者需要选一条路并用测试钉住：给 `HealthLease` 加一个 `bypassed: bool` 标记，在完成路径上按「成功即清冷却」处理；或在透传模式下强制发放 half-open generation。两种都可以，**但必须有测试证明「冷却中 → 透传成功 → 冷却已清除」**。

### 语义问题二：透传失败后要不要继续升冷却？

**要继续记录**，且允许步进升级。理由：开关关闭期间健康档案仍要真实反映上游状态，否则运维切回 `true` 的瞬间会因为「档案是干净的」而毫无保护。

但要注意别把冷却升到荒谬的量级 —— 现有的 `ROUTE_HALF_OPEN_FAILURE_STEP_CAP` 与 A1 请求内抑制逻辑应继续生效。用测试确认：透传模式下连续失败 N 次，冷却值仍受既有上限约束。

## 6. 与 `feat/portal-oidc-v2` 的冲突规避（必读）

`feat/portal-oidc-v2` 正在新增 6 个 `portal_oidc_*` 运行时设置，与本计划**踩在同一套接线点上**。已核对其改动范围（`git diff --stat main..feat/portal-oidc-v2`）：

**它没有碰 `src/state/route_health.rs`** —— 本计划的主实现文件零冲突。

重叠文件与处置方式：

| 文件 | 重叠情况 | 怎么做 |
| --- | --- | --- |
| `src/state/types.rs` | 双方都加常量 / `AppConfig` 字段 / `default_*()` | 不同字段，自动合并通常成功 |
| `src/state/runtime_settings.rs` | 双方都往 `IMMEDIATE_...FIELDS` 加键、都加结构体字段 | **见下方「插入位置」** |
| `src/state.rs` | 双方都加 `pub use` 再导出 | 不同符号，通常自动合并 |
| `src/main.rs` | 双方都加 env 读取与 `AppConfig` 字面量 | 同上 |
| `frontend/src/types/index.ts` | 双方都加 TS 字段 | 同上 |
| `frontend/src/utils/runtimeSettings.ts` | 双方都加描述符 | **见下方「插入位置」** |
| `tests/runtime_settings.rs` | **同一行**计数断言 | **必然冲突，见下方** |
| `tests/admin_runtime_settings.rs` | **同一行**计数断言 | **必然冲突，见下方** |
| `frontend/.../runtimeSettings.spec.ts` | **同一行**计数断言 + fixture + expectedKeys | **必然冲突，见下方** |

### 规避手段一：从 main 拉分支，不要从 OIDC 分支拉

```bash
git fetch origin
git checkout -b feat/route-health-passthrough origin/main
```

**不要** `git merge feat/portal-oidc-v2`，**不要**在那条分支上开发。它有未提交的工作区改动，且 `src/portal_oidc.rs` 等文件在 main 上不存在。

### 规避手段二：插入位置刻意错开

OIDC 分支把新键**追加在列表末尾**。所以本计划的新键请**插在路由健康的同族条目旁边**（中部），不要追加到末尾：

- `IMMEDIATE_RUNTIME_SETTING_FIELDS`：紧邻 `"upstream_route_health_half_open_ttl_seconds"` / `"upstream_route_half_open_exclusive_window_ms"` 一组。
- `frontend/src/utils/runtimeSettings.ts`：紧邻 `upstream_route_health_half_open_ttl_seconds` 描述符（约 `:327`）。
- `expectedKeys`（前端 spec）：同样插在路由健康那一段。

这样两边的 diff hunk 相距足够远，git 能自动合并。**这既是冲突规避，也是语义正确的分组** —— 这个开关本来就属于路由健康。

### 规避手段三：计数断言不要手工猜数字

三处计数断言（后端 `tests/runtime_settings.rs`、`tests/admin_runtime_settings.rs`，前端 `runtimeSettings.spec.ts` 的 `toHaveLength` 与 immediate 计数）双方都会 +N，**同一行必然冲突**。

合并时的正确做法：**任取一侧，然后跑测试，用失败信息里报出的实际数字**。

```
assertion `left == right` failed
  left: 86      ← 用这个数
 right: 80
```

不要用「80 + 6 + 1」这种算式心算，两边都在动，算错就是一次无谓的红。

从 main 起步时的基线值（本计划 +1 设置后应各 +1）：
`tests/runtime_settings.rs` = **79**、`tests/admin_runtime_settings.rs` = **80**、
前端 `toHaveLength` = **80** / immediate = **67**。

### 规避手段四：谁先合谁后合

两条分支互不阻塞，可以并行开发、独立验证。**后合入 main 的那条**负责 rebase 并按手段三修计数断言。本分支改动小（预计 1 个设置 + 1 个文件的主实现 + 接线），**建议本分支先合**，让 OIDC 那条大改动只承担一次 rebase。

## 7. 接线点清单（9 处）

照 `docs/superpowers/plans/2026-08-31-local-slot-gate-false-429.md` §10.2 执行。`eb5bed62` 是一个完整现成范例（`upstream_account_queue_poll_interval_ms` 从常量到前端描述符的全链路），照抄其结构最省事：

```bash
git show eb5bed62
```

| 文件 | 内容 |
| --- | --- |
| `src/state/types.rs` | `DEFAULT_` 常量、`AppConfig` 字段 + `#[serde(default)]`、`Default` impl、`default_*()` |
| `src/state/runtime_settings.rs` | 键名清单（**中部插入**）、结构体字段、`from_config`、`apply`、（本项为 bool，无需 validate 范围） |
| `src/state.rs` | 常量 `pub use` 再导出 + `update_runtime_tuning` 调用处传入新参数 |
| `src/main.rs` | `use` 导入、`env_bool` 读取、`AppConfig` 字面量 |
| `src/state/route_health.rs` | **主实现**（§4） |
| `frontend/src/types/index.ts` | TS 字段 |
| `frontend/src/utils/runtimeSettings.ts` | 描述符（**中部插入**，`control: 'switch'`） |
| `tests/runtime_settings.rs` | 计数断言 79 → 80 |
| `tests/admin_runtime_settings.rs` | 计数断言 80 → 81 |
| 前端 `runtimeSettings.spec.ts` | 计数断言 80 → 81 / immediate 67 → 68、fixture、expectedKeys |

已知坑：前端字段缺失会让 `vue-tsc` 直接失败，而 `cargo test` 覆盖不到前端。

## 8. 测试清单

严格 TDD：先写测试、**亲眼看到失败且失败原因是功能缺失**，再实现。

**默认值回归（最重要，证明没改现有行为）**
1. 开关默认 `true` 时，冷却中的路由仍返回 `Cooling` —— 现有相关测试必须全部保持绿。
2. 开关默认 `true` 时，半开独占窗口内第二个调用者仍得 `HalfOpenBusy`。

**透传模式行为**
3. 开关 `false` + 路由正在冷却 → `reserve()` 返回 `Ready`，**不是** `Cooling`。
4. 开关 `false` + 半开独占窗口内 → 第二、第三个并发调用者**都**返回 `Ready`，不出现 `HalfOpenBusy`。
5. 开关 `false` + 失败 → 冷却与失败类别**照常写入**（读健康档案/快照断言，证明「只是不阻断」而非「不记录」）。
6. 开关 `false` + 冷却中透传 + 请求成功 → **冷却被清除**（§5 语义问题一）。
7. 开关 `false` + 连续失败 → 冷却仍受既有步进上限约束，不无限膨胀（§5 语义问题二）。
8. 热改生效：运行中把开关从 `true` 改为 `false`，**不重启**，冷却中的路由立刻变为可用（走 `update_runtime_tuning` 路径断言）。

**端到端（有 mock 上游的网关级测试）**
9. 上游持续返 502、开关 `false` → 下游收到的是**上游的 502**，不是本地聚合的 429/503；且 `upstream_attempted_count > 0`。
10. 同上但开关 `true` → 维持现有行为（下游 429/503、`upstream_attempted_count == 0`），证明开关两侧行为确实不同。

第 9、10 条是这个功能的验收核心：**它们直接对应运维的原始诉求**。

## 9. 验证纪律

五条各跑一次、**独立记录退出码**，不要用 `&&` 串联掩盖失败：

```
cargo fmt -p chat-responses-codex -- --check
cargo clippy --all-targets
cargo test
cd frontend && npx vue-tsc --noEmit -p tsconfig.json
cd frontend && npx vitest run
```

基线：`main` 当前 **62 套件 / 1858 passed / 0 failed / 102 ignored**；前端 **273 passed**。合入后 passed 应上升，failed 必须为 0。

涉及 Redis 的回归（本计划不碰 Redis，但路由健康在 Redis 后端下也有实现，**必须跑**）：

```
docker run -d --rm -p 16399:6379 redis:7-alpine
TEST_REDIS_URL=redis://127.0.0.1:16399 cargo test --test redis_runtime -- --ignored
```

基线 99 passed。⚠️ 该套件在**并行**下有**既有**偶发失败（约 1/4 概率，每次挂的测试不同，与本计划无关）。若出现失败，先用干净树（`git stash`）跑同样命令对照，确认是既有抖动再继续，**不要**误判成自己引入的。

路由健康在 Redis 后端**确实另有一份 Lua 实现**（已核实，见 §4b），透传语义必须两个后端一致，否则内网启用 Redis 后开关就是空的。Redis 侧测试不是可选项。

## 10. 风险

| 风险 | 处置 |
| --- | --- |
| **透传模式打爆上游** | 这是运维明确要的行为（争取上游资源）。默认 `true` 即天然安全边界；文档写明「仅在上游故障且希望最大化重试压力时开启」 |
| 陈旧冷却在切回 `true` 时立刻生效 | §5 语义问题一要求成功即清冷却，并有测试 |
| **Redis 后端漏改（最高概率的失败方式）** | 已确认存在第二份 Lua 实现，§4b 列了全部位置；Redis 侧必须有独立测试，否则现场开关无效 |
| 与 OIDC 分支的计数断言冲突 | §6 手段三：跑测试取实际数字，不心算 |
| 误以为本开关能解决并发闸门的 429 | §2 非目标已写明；诊断时先看 429 响应体的 `upstream_attempted_count` 与 `local_gate_rejected_count` 区分两条路径 |

**回滚**：开关默认 `true`，代码合入后不配置即完全保持现有行为。整体回滚直接弃用本分支，`main` 不受影响。
