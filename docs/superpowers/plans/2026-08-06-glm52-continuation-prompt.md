# GLM-5.2 Continuation Prompt

你将接手开发项目 `chat2Responses`，请持续自主完成开发、测试、审查、构建、部署和推送，不要只做分析，也不要反复询问是否继续。

工作目录：

```text
/home/kavin/projects/chat2Responses
```

分支：

```text
main
```

开始前必须完整阅读：

```text
/home/kavin/projects/chat2Responses/AGENTS.md
/home/kavin/projects/chat2Responses/RTK.md
/home/kavin/projects/chat2Responses/docs/superpowers/plans/2026-08-01-account-concurrency-recovery-and-runtime-visibility.md
/home/kavin/projects/chat2Responses/docs/superpowers/plans/2026-08-06-glm52-continuation-handoff.md
```

当前 HEAD：

```text
1716e90 feat(capabilities): verify domestic reasoning and v1 delegation
```

当前工作区不是干净的，禁止丢弃：

```text
M scripts/installed_client_smoke.sh
M tests/scripts.rs
?? docs/superpowers/plans/2026-08-06-glm52-continuation-handoff.md
?? docs/superpowers/plans/2026-08-06-glm52-continuation-prompt.md
```

前两份文件是 Codex delegation smoke 的在途 TDD 改动，后两份是本次交接文档。当前离线脚本测试 39 个全部通过，真实部署的 Codex 0.146.0 + `glm-5.2` delegation 已串行实测三次通过。你必须在此基础上继续，不能 reset、checkout 或重写已有工作。

强制约束：

- 所有 shell 命令必须以 `rtk ` 开头。
- 手工编辑只能使用 `apply_patch`。
- 按 TDD 执行：先写并确认红测，再写最小实现，再跑绿测。
- 主代理负责所有代码修改、方案取舍和最终验证。
- 子代理只做探索和独立审查；必须 `agent_type="default"`、`fork_turns="none"`，每个代理只使用一轮。
- 每个任务提交前必须先规格审查，再代码质量审查。
- 不得输出或记录 API Key、Authorization、上游响应正文、计费字段、提示词、工具参数、密文或原始客户端事件正文。
- 不得自动重试 delegation smoke，避免重复请求影响判断。
- 不得把上游并发限制写死为 4。
- 私有接口 `/dashboard/api/user/request-status` 默认关闭。
- `/v1/messages` 是下游 Claude 兼容面，不是原生上游协议，不要扩展成不存在的 Messages 上游。
- Chat-only 国模使用 Codex V1 collaboration transport；所有生成配置保持 `multi_agent_v2 = false`。
- 不得把 unreadable encrypted agent payload 降级成伪造文本；Chat-only 转换必须安全拒绝，原生 Responses 能保留时才保留。
- 未完成仓库全量门禁和镜像构建前，不得修改 `/home/kavin/docker/chat-responses-codex`。
- 部署时保留现有密码、Key、卷、Redis prefix 和 `3000:3001` 端口，只重建 gateway。

执行顺序严格按交接文档：

1. 保护当前两份未提交改动并核对 diff。
2. 为未知 legacy `collab_tool_call` 增加红测，删除真实 smoke 的自动 legacy 接受分支。
3. 为跨 turn 拼接增加红测，把 `turn.started -> spawn_agent -> wait -> final agent_message -> turn.completed` 绑定到一次命令的唯一 turn。
4. 跑完整 `tests/scripts.rs`，然后真实串行复测 `glm-5.2` 三次和 `glm-5.1` 一次 delegation。
5. 核验 Responses `reasoning.effort` 现有红测、fast-preview 到 base model 的路由测试，以及所有 Codex 配置中的 `multi_agent_v2 = false`。
6. 使用临时 npm prefix 安装精确包 `@cline/cli@0.0.13` 和 `@kilocode/cli@7.4.20`，不要全局安装，也不要安装同名错误包。
7. 使用部署库中的 `test` 下游账号安全实测 Codex、Claude Code 2.1.221、Cline 0.0.13、Kilo 7.4.20。
8. 串行实测 `glm-5.2`、`glm-5.1`、DeepSeek、Kimi、Qwen、MiniMax 的实际可路由 slug；429/5xx 要先区分容量/路由健康与协议缺陷。
9. 完成规格审查、质量审查、全量 Rust/Redis/前端/Clippy/构建/Compose 门禁。
10. 使用项目脚本 `scripts/build-package-image.sh` 构建镜像，不能改用临时自创流程。
11. 只重建部署目录的 gateway，检查容器健康和 `/healthz`，重新跑关键真实 smoke。
12. 提交仓库改动并推送 `main` 到 `origin`。

最终门禁至少包含：

```bash
rtk cargo fmt --check
rtk cargo test --all-targets
rtk env TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test redis_runtime -- --test-threads=1
rtk cargo test --test gateway slow_stream
rtk cargo test --test gateway stream_client_cancelled
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk npm --prefix frontend test
rtk npm --prefix frontend exec vue-tsc -- --noEmit
rtk npm --prefix frontend run build
rtk docker compose config
rtk git diff --check
```

镜像构建必须使用：

```bash
rtk scripts/build-package-image.sh --image chat-responses-codex --tag latest --output chat-responses-codex-latest.tar
```

请从 `rtk git status --short --branch`、`rtk git diff -- scripts/installed_client_smoke.sh tests/scripts.rs` 开始，然后严格按交接文档逐项推进。遇到测试失败先做根因分析和最小红测，不要猜测性修改。除非发现会导致数据损坏、需要额外授权或现有用户改动无法安全合并的阻塞，否则持续执行直到完成。
