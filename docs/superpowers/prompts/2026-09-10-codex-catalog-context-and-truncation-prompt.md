# 交接提示词：Codex 目录上下文窗口单一来源 + 截断策略改 tokens

请在仓库 `/home/kavin/projects/chat2Responses`（Rust/Axum 网关，分支 `main`）中，严格按照计划文档执行开发：

`docs/superpowers/plans/2026-09-10-codex-catalog-context-and-truncation.md`

## 背景（详见计划文档第 1、2 节）

门户给 Codex 生成的 `model-catalog.json` 里 `context_window` 取值有两个 bug：
1. 只看"见证上游"的能力解析结果（`src/server/gateway/capability_routing.rs:745` 用 `context_config_for_model` 不带全局 profile），导致管理台"全局上下文配置"完全不生效，落到上游默认 200000。
2. 同一模型多上游时只看被选中那一个上游的配置。

同时 `truncation_policy.mode` 要从 `bytes` 改为 `tokens`（limit 保持 10000）。`effective_context_window_percent` 保持 80 不动。

## 工作规则

- **强制 TDD**：每个任务先写/改测试，运行并亲眼看到失败，且失败原因是"行为不存在/不对"而非编译错误；再写最小实现；再跑绿；再提交。计划里每个 Task 的 Step 已经按 RED → GREEN → COMMIT 排好，逐步打勾执行。
- 所有命令加 `rtk` 前缀：`rtk cargo test ...`、`rtk cargo clippy --all-targets -- -D warnings`、`rtk git add/commit`。
- 4 个任务按顺序做，每个任务单独 commit，commit message 已写在计划里。
- 计划中的"Global Constraints"一节是硬约束：不改 `effective_context_window_percent`、不改 `capability_routing.rs:745-765` 的覆盖逻辑、不改管理台 200000 默认值、`max_context_window` 继续等于 `context_window`。
- 计划里引用的行号来自当前 `main`（`ebf7e8cd` 之后），动手前先 `rtk grep` 核对一下位置。
- 遇到与计划不一致的代码现状（比如函数签名不同、测试辅助函数名不同），先读代码确认，再按计划意图调整；不要猜着改。同一方向失败两次就停下来汇报，不要继续小修小补。
- 完成后回填计划文档末尾"完成状态"表（commit 号 + ✅），并确认"验收清单"每一项。

## 交付物

1. 4 个 commit（truncation tokens / 共享上下文解析 / Codex 目录改源 / 文档文案）。
2. `rtk cargo clippy --all-targets -- -D warnings` 与 `rtk cargo test` 全绿；前端 `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run` 全绿。
3. 计划文档"完成状态"表已回填。
4. 最后给我一段简短汇报：每个任务改了哪些文件、新增/改名的测试名、以及是否有偏离计划的地方及原因。
