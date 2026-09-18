# 交接提示词：费用限额从「按 Key」改为「按账号」

请在仓库 `/home/kavin/projects/chat2Responses`（Rust/Axum 网关，分支 `main`）中，严格按照计划文档执行开发：

`docs/superpowers/plans/2026-09-17-account-level-cost-quota.md`

## 背景（详见计划文档第 1、2 节）

现在费用限额完全按 Key 算：上限是 `DownstreamConfig.daily_cost_limit_cents`，24 小时滚动窗口（本地 HashMap 与 Redis ZSET）都以下游 id 为键。一个用户建 N 个 Key 就得到 N 份预算。更糟的是门户自助建的 Key 用 `..Default::default()`，没有上限也没有单价，完全不受费用限制。

改造目标：费用以「账号」为单位汇总。引入「费用归属（cost scope）」——有门户归属用户的 Key 记在用户账本上，没有归属的直连 Key 记在自己账本上。上限存进新表 `cost_scope_limits(scope_id, daily_limit_cents)`，scope_id 既可能是用户 id 也可能是下游 id。窗口键从下游 id 换成 scope id，Redis 的 Lua 脚本一行都不用改。

**用户明确要求：Key 级上限不是「忽略」而是「删掉」，切换后不允许有任何残留的 Key 级限制影响账号总限额。** 所以 Task 5 要删掉 `DownstreamConfig` 的字段和数据库列，由编译器保证没有任何读取点。

## 工作规则

- **强制 TDD**：每个任务先写/改测试，运行并亲眼看到失败，再写最小实现，再跑绿，再提交。计划里每个 Task 的 Step 已经按 RED → GREEN → COMMIT 排好，逐步打勾执行。
- 所有命令加 `rtk` 前缀：`rtk cargo test ...`、`rtk cargo clippy --all-targets -- -D warnings`、`rtk git add/commit`。
- 6 个任务**必须按顺序做**，每个任务单独 commit，commit message 已写在计划里。顺序不能改：Task 5 的删字段必须在 Task 3、4 两条准入路径都切换完之后，否则会切出一个既不按 Key 也不按账号的中间态。
- 计划中的「Global Constraints」是硬约束，特别是：
  - 单价留在 Key 上，不要动；`billing_mode`、请求数配额、每分钟限速、并发上限全部保持 Key 级。
  - 迁移折算取**最大值**，不是求和。
  - Redis 的三个 Lua 脚本一行都不要改，只在 Rust 侧用不同 identity 算键。
  - 老的 Redis Key 级窗口不要写清理脚本，靠 EXPIRE 自然过期。
- Task 5 改测试时，17 处赋了非 `None` 值的 `daily_cost_limit_cents` 要改成往 `PersistedState.cost_scope_limits` 写等值上限，**不要简单删掉断言**，否则会丢失费用限额的测试覆盖。赋 `None` 的地方直接删行。
- 计划里引用的行号来自当前 `main`（`3fa2cca8`），动手前先 `rtk grep` 核对位置。
- 遇到与计划不一致的代码现状（比如状态写回路径的函数名不同、`mutate_config` 的实际签名不同），先读代码确认，再按计划意图调整；不要猜着改。同一方向失败两次就停下来汇报，不要继续小修小补。
- `tests/postgres_roundtrip.rs` 在没有 `PG_TEST_DATABASE_URL` 时会自动 skip；即使本地没有 Postgres，建表 SQL、迁移 SQL、SELECT/INSERT 列表也必须全部改到位，并在汇报里注明没有在真实库上跑过。
- 完成后回填计划文档末尾「完成状态」表（commit 号 + ✅），并逐项确认「验收清单」。

## 特别注意：无残留自检

Task 5 完成后必须跑这两条并把结果贴进汇报：

```bash
rtk grep -rn "daily_cost_limit_cents" src/
rtk grep -rn "cost_billing_mode\|daily_cost_limit()" src/
```

第一条只允许命中 `cost_scope_limits` 相关代码；`src/state/types.rs` 里不得有任何命中。第二条应当为空（两个函数已被 `has_cost_pricing` 与 `CostScope` 取代）。

## 交付物

1. 6 个 commit（存储 / 归属解析 / 本地准入 / Redis / 删 Key 级上限 / 管理台与门户展示）。
2. `rtk cargo clippy --all-targets -- -D warnings` 与 `rtk cargo test` 全绿；前端 `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run` 全绿。
3. 计划文档「完成状态」表已回填。
4. 最后给我一段简短汇报：每个任务改了哪些文件、新增/改名的测试名、上面两条无残留自检的输出、Postgres 相关改动是否在真实库上验证过、以及是否有偏离计划的地方及原因。
