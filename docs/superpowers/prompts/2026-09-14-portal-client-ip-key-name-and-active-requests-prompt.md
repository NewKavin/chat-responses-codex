# 交接提示词：门户最近请求显示客户端 IP 与 Key 名称，概览页显示在途请求

请在仓库 `/home/kavin/projects/chat2Responses`（Rust/Axum 网关，分支 `main`）中，严格按照计划文档执行开发：

`docs/superpowers/plans/2026-09-14-portal-client-ip-key-name-and-active-requests.md`

## 背景（详见计划文档第 1、2 节）

管理台已经能看到用量日志和在途请求的客户端 IP（`3591bbda`..`3fa2cca8` 四个提交）。现在要把同样的信息带到客户端门户：

1. 门户"最近请求"每行加"Key"名称和"客户端 IP"两列；同时修一个现成的不一致——`/api/portal/usage-history` 一直只看 Bearer 的默认 Key，忽略用户在门户里显式选择的 Key，要改成和 `/api/portal/overview` 一样接受 `downstream_id` 并走 `resolve_portal_downstream_scope`。
2. 新增 `GET /api/portal/active-requests`，只返回当前作用域 Key 的在途请求，字段经门户结构体裁剪；概览页"下游并发状态"下方加"在途请求"面板，用 `useQuietRefresh` 按接口返回的间隔（默认 2 秒）轮询。

## 工作规则

- **强制 TDD**：每个任务先写/改测试，运行并亲眼看到失败，再写最小实现，再跑绿，再提交。计划里每个 Task 的 Step 已经按 RED → GREEN → COMMIT 排好，逐步打勾执行。
- 所有命令加 `rtk` 前缀：`rtk cargo test ...`、`rtk cargo clippy --all-targets -- -D warnings`、`rtk git add/commit`。
- 3 个任务按顺序做，每个任务单独 commit，commit message 已写在计划里。
- 计划中的"Global Constraints"一节是硬约束，特别是：门户接口不暴露 `upstream_id`/`upstream_name`/`error_message`/token 数；在途请求只能看当前作用域 Key 自己的；`UsageLogQuery` 保持单 `downstream_id`，不做多 Key 合并；不改管理台。
- 计划里引用的行号来自当前 `main`（`3fa2cca8`），动手前先 `rtk grep` 核对位置。`ActiveGatewayRequestSnapshot` 的 `phase`/`queue_position` 类型以 `src/state.rs` 源码为准。
- Task 1 里对 `resolve_portal_downstream_scope` 的放宽（显式 `downstream_id` 等于 Bearer 默认 Key 时直接放行）会同时影响 overview/quota，改完必须跑整个 `rtk cargo test --test portal_api` 确认既有测试不受影响。
- 前端两个新 spec 的 mock 列表按计划写；如果挂载时还依赖其它 `portalApi` 方法或全局对象，把它们补进 mock，不要为了让测试跑通改业务代码。
- 遇到与计划不一致的代码现状，先读代码确认，再按计划意图调整；不要猜着改。同一方向失败两次就停下来汇报，不要继续小修小补。
- 完成后回填计划文档末尾"完成状态"表（commit 号 + ✅），并逐项确认"验收清单"。

## 交付物

1. 3 个 commit（历史接口 / 在途接口 / 门户前端）。
2. `rtk cargo clippy --all-targets -- -D warnings` 与 `rtk cargo test` 全绿；前端 `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run` 全绿。
3. 计划文档"完成状态"表已回填。
4. 最后给我一段简短汇报：每个任务改了哪些文件、新增/改名的测试名、以及是否有偏离计划的地方及原因。
