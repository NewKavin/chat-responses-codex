# 交接提示词：请求日志与在途请求记录客户端 IP

请在仓库 `/home/kavin/projects/chat2Responses`（Rust/Axum 网关，分支 `main`）中，严格按照计划文档执行开发：

`docs/superpowers/plans/2026-09-12-usage-log-client-ip.md`

## 背景（详见计划文档第 1、2 节）

内网多台机器共用一个下游 Key 跑 Codex，管理台"请求日志"和"在途请求"只有 User-Agent，分不清一条 429 是哪台机器发的。要给用量日志和在途请求各加一个 `client_ip` 字段并在管理台展示。

网关已经具备全部原材料：启动时挂了 `ConnectInfo<SocketAddr>`（`src/main.rs:655`），有从扩展取对端地址的 `request_client_addr`（`src/server/gateway.rs:2779`），有给 IP 白名单用的 `client_ip_from_headers`（`src/server/gateway.rs:10018`，按 `X-Forwarded-For` / `X-Real-IP`）。方案是：一个中间件把对端地址盖章进内部头 `x-c2r-peer-addr`（先删客户端带来的同名头再写），一个 `resolve_client_ip(&headers)` 按"代理头优先、对端地址兜底"解析，然后 `client_ip` 沿 `user_agent` 现有的每一条传递路径并排流到两个用量日志构造点和在途登记点。

## 工作规则

- **强制 TDD**：每个任务先写/改测试，运行并亲眼看到失败，再写最小实现，再跑绿，再提交。计划里每个 Task 的 Step 已经按 RED → GREEN → COMMIT 排好，逐步打勾执行。Rust 里"结构体没这个字段 / 函数没这个参数"的编译失败就是合格的 RED，但要确认报错正是缺这个字段/参数。
- 所有命令加 `rtk` 前缀：`rtk cargo test ...`、`rtk cargo clippy --all-targets -- -D warnings`、`rtk git add/commit`。
- 4 个任务按顺序做，每个任务单独 commit，commit message 已写在计划里。
- 计划中的"Global Constraints"一节是硬约束，特别是：**不改** `client_ip_from_headers` 和 IP 白名单语义；内部头 `x-c2r-peer-addr` 只能由中间件写、每次先删再写；Postgres 新列追加在列表**末尾**、索引 `25`、`COLUMNS_PER_ROW` 改 `26`，不重排既有索引；`EnrichedUsageLog` 不加字段；本期不做按 IP 过滤。
- 计划里引用的行号来自当前 `main`（`c464c441`），动手前先 `rtk grep` 核对位置。Task 2 的传递链尤其要用 `rtk grep -n user_agent src/server/gateway.rs src/server/gateway/upstream.rs src/server/gateway/stream.rs` 重新对一遍，规则是"每一处为了写日志而携带 `user_agent` 的签名、字段、字面量旁边并排加 `client_ip`"，让编译器把漏掉的地方全部报出来。
- 遇到与计划不一致的代码现状（比如函数签名不同、测试辅助函数名不同、`Request::builder()` 位置不同），先读代码确认，再按计划意图调整；不要猜着改。同一方向失败两次就停下来汇报，不要继续小修小补。
- `tests/postgres_roundtrip.rs` 在没有 `PG_TEST_DATABASE_URL` 时会自动 skip；即使本地没有 Postgres，Task 1 里 Postgres 的建表列、ALTER、INSERT、三个 SELECT、`usage_log_from_row` 索引 25、`COLUMNS_PER_ROW = 26` 也必须全部改到位，并在汇报里注明没有在真实库上跑过。
- 完成后回填计划文档末尾"完成状态"表（commit 号 + ✅），并逐项确认"验收清单"。

## 交付物

1. 4 个 commit（数据模型与持久化 / 采集与落库 / 管理台展示 / 部署文档）。
2. `rtk cargo clippy --all-targets -- -D warnings` 与 `rtk cargo test` 全绿；前端 `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run` 全绿。
3. 计划文档"完成状态"表已回填。
4. 最后给我一段简短汇报：每个任务改了哪些文件、新增/改名的测试名、Postgres 相关改动是否在真实库上验证过、以及是否有偏离计划的地方及原因。
