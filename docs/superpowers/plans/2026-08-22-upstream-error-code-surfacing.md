# 上游错误码透传与错误可诊断性 实施计划（待执行）

**日期：** 2026-08-22
**关联：** `2026-08-20-upstream-retry-after-cap.md`、`2026-08-21-continuation-pin-escape.md`

**Goal:** 让用户在 codex/CLI 里看到的报错足以定位问题——带上**上游原始错误码**、
**上游标识**、**网关 request_id**，而不是只有一句泛化的 "upstream server error (status 502)"。

**Architecture:** 保持既有隐私边界（**永不把上游响应正文回显给客户端**），
但把「码类」信息（错误码 token、HTTP 状态、上游名、request_id）沿分类链路一路带到
客户端消息与错误体；正文只在服务端日志里、且仅在显式开关下才允许有界摘录。

**Tech Stack:** Rust / axum 0.8 / Vue3 + TypeScript。

---

## 1. 问题

用户报障时看到的是：

```
stream disconnected before completion: [upstream_routes_exhausted] all eligible
upstream routes are temporarily unavailable: transient upstream server errors
(3 routes, upstream HTTP 502); please try again in 14s
```

或者单路由失败时：

```
[upstream_temporary_unavailable] upstream server error (status 502)
```

**看不出上游到底说了什么。** new-api / one-api / sub2api 这类中转网关返回的错误体里通常带着
非常明确的码（例如配额不足、渠道不可用、模型未找到、令牌额度预扣失败之类的 token），
这些码是定位问题的关键，但**一个都没有到达客户端**。

---

## 2. 现状（已核对代码）

| 环节 | 代码位置 | 现状 |
|------|----------|------|
| 上游错误体解析 | `src/upstream_feedback.rs:35` `StructuredError` | 解析出 `codes` / `messages` / `scopes` / `statuses`，**仅用于分类，随后全部丢弃** |
| 分类结果 | `ClassifiedUpstreamFailure` | 只保留 `class` / `semantic` / `upstream_status` / `retry_after`——**没有错误码字段** |
| 数字码提取 | `src/server/gateway/upstream.rs:131` `extract_upstream_error_code` | **只认 `u16` 数字码**；`insufficient_user_quota` 这类**字符串码直接返回 None** |
| 日志用摘要 | `errors.rs:1039` `safe_upstream_error_summary` | 有 status + classification + 数字码，**只进 `tracing`，不进客户端** |
| 客户端消息 | `errors.rs:1021` `upstream_client_message(status)` | **只按 HTTP 状态给一句泛化提示**，不含任何上游信息 |
| 客户端错误体 | `errors.rs:917` `into_response` | `{message, type, param, code, details, category}`；`code` 是**网关自己的**码；`details` 里有 `upstream_status` |
| request_id | —— | **客户端错误体和响应头里都没有**，用户无法把现场和服务端日志对上 |

**结论**：诊断信息在网关内部是齐全的，只是**全部停在日志边界上没有过河**。

现状的隐私理由是成立的（`errors.rs:1035-1038` 写明：provider bodies 可能回显 prompt、
工具参数、凭证），所以**方案不是「把正文透传出去」，而是「把码类信息透传出去」**。

---

## 3. 成熟项目怎么做（对齐参考）

| 项目 | 客户端可见的诊断信息 |
|------|----------------------|
| OpenAI API | 错误体 `{message, type, param, code}` + `x-request-id` 响应头；官方 SDK 直接打印 `Error code: 429 - {...}`，`code` 是**稳定的机器可读 token** |
| Anthropic API | `{"type":"error","error":{"type":"rate_limit_error","message":...}}` + `request-id` 响应头 |
| Stripe | `code` / `decline_code`（细分原因）+ `doc_url`（直达文档）+ request id，报错自带「下一步怎么办」 |
| new-api / one-api 一族 | OpenAI 兼容错误体，`type` 标明是中转层错误，`code` 常是**字符串 token**（配额/渠道/模型类），少数是数字 |

**共同点，也是本方案要抄的三条：**
1. **码是稳定 token，和人读的 message 分离**——机器可读、可搜索、可写进 FAQ；
2. **每个错误都带 request id**——用户复制一行就能让运维在日志里精确定位；
3. **错误体分层**：`type`（大类）/ `code`（具体原因）/ `message`（人读）/ `details`（结构化补充）。

---

## 4. 设计原则（不变量）

- **码可以透传，正文不可以。** 上游错误码 token 走白名单净化后可进客户端；
  上游 message 正文默认**永不**进客户端（它可能回显 prompt / 工具参数 / 凭证）。
- **token 净化规则**（必须实现，否则等于开了正文透传的后门）：
  - 转小写、trim；
  - 字符集白名单 `[a-z0-9_.:-]`，出现空格或其它字符 → **整体丢弃**；
  - 长度上限 64 字符，超出 → 丢弃；
  - 纯数字码同时保留为 `upstream_error_status`。
- 不改变任何错误**分类**语义（`FailureClass` 映射、HTTP 状态、重试语义全部不动）；
  本方案只增加**描述性**信息。
- SSE 流式路径上 message 是唯一载体——诊断信息**必须进 message 字符串**，
  只放进 `details` 对 codex 无效。

---

## 5. 任务清单

### E1 让错误码 token 活着走完分类链路

- [x] RED：`tests/upstream_feedback.rs`（或既有分类用例文件）
  - 上游返回字符串码的错误体 → `ClassifiedUpstreamFailure.upstream_error_code`
    == 净化后的 token；
  - 上游返回数字码 → token 为 None，`upstream_error_status` 有值；
  - **净化用例**：码里带空格 / 超长 / 含奇怪字符 → 全部被丢弃（断言 None）；
  - 分类结果（`class` / `semantic` / HTTP 状态）**逐条不变**（回归）。
- [x] GREEN：
  - `src/upstream_feedback.rs`：`ClassifiedUpstreamFailure` 增
    `upstream_error_code: Option<String>`（净化后的 token）；
    `classify_upstream_response` 从既有 `StructuredError.codes` 里取**第一个**通过净化的码
    （codes 已经按 `code` / `error_code` / `type` 收集，不需要新解析逻辑）；
  - 新增 `fn sanitize_upstream_error_token(raw: &str) -> Option<String>` 并加单元测试；
  - `src/server/gateway/upstream.rs:131` `extract_upstream_error_code` 保持数字语义不动，
    另加一个返回 token 的姊妹函数（或直接用分类结果里的字段，避免两套解析）。
- [x] 验证：`rtk cargo test --test upstream_feedback --test gateway`。
  - commit `faaac3f`（E1）：`ClassifiedUpstreamFailure.upstream_error_code: Option<String>`
    + `sanitize_upstream_error_token`；44 个沉睡分类测试随 lib `#[cfg(test)]` 模块一并唤醒。
  - 行号快照已漂移：`extract_upstream_error_code` 现于 `src/server/gateway/upstream.rs:131`，
    分类入口 `classify_upstream_response` 于 `src/upstream_feedback.rs`；净化白名单
    `[a-z0-9_.:-]`、长度 <=64、不合规整体丢弃（见 `sanitize_upstream_error_token` 单测）。

### E2（核心）把诊断信息拼进客户端消息

- [x] RED：`tests/gateway/chat/feedback.rs` / `tests/gateway/responses/upstream_feedback.rs`
  - 上游 502 + 字符串码 → 客户端 message **包含**该 token、上游名、status；
  - 上游 502 + 无码 → 消息里只有上游名与 status，**不得**出现 `code=`（不许打印空值）；
  - **正文不泄漏**：上游 message 正文里放一段特征串 → 断言客户端 message
    **不包含**它（这条是隐私红线，必须有）；
  - SSE 路径同样断言（message 是唯一载体）。
  - 注：3 个端到端断言随 E3（`cb53364`）一并落地——单路由终态走
    `terminal_route_failure_error` 摘要渲染，E2 的 message 级断言依赖 E3 的摘要扩展。
- [x] GREEN（`src/server/gateway/errors.rs`）：
  - `upstream_client_message(status)` 扩成
    `upstream_client_message(status, upstream_name, upstream_error_code)`，
    输出形如：
    ```
    upstream server error (status 502, upstream=k-api, code=channel_not_found)
    ```
    括号内为**固定顺序的 kv**，缺项直接省略该 kv，不留空值；
  - 调用点 `src/server/gateway/upstream.rs:2135` 一带传入新参数；
  - `from_classified_upstream_failure` 把 `upstream_error_code` 一并塞进 `details`
    （`details.upstream_error_code`），供程序化消费。
  - commit `69cf84d`（E2）：`upstream_client_message(status, upstream_name, upstream_error_code)`
    + 3 个单测（固定 kv 序 / 缺项省略不打印空 `code=` / 空白 name 剥离）。
  - 行号漂移：`upstream_client_message` 现于 `errors.rs:1121`，`from_classified_upstream_failure`
    于 `errors.rs:465`。

### E3 聚合终态错误也带上上游码

- [x] RED：多路由全失败 → 终态 message 的 class 摘要里，除既有
      `(3 routes, upstream HTTP 502)` 外，出现出现次数最多的上游码；
      `details.class_counts` 旁新增 `upstream_error_codes`（token → 次数）。
      - 落地见下；单元测试 `tests/unit/server/gateway.rs`（`e3_class_summary_picks_most_common_code_and_name`、
        `e3_terminal_error_message_and_details_carry_code_and_name`）+ 网关层 3 个用例。
- [x] GREEN：
  - `AttemptFailure` 增 `upstream_error_code: Option<String>`（记账点与 `upstream_status` 同处）；
  - `FailureClassSummary` 增同名字段，`ledger_failure_summary`（`errors.rs:40`）拼进摘要；
  - `terminal_route_failure_error` 的 `details` 增 `upstream_error_codes`。
- [x] 注意：`class_summaries` 已有「取出现次数最多的 status」的逻辑，码沿用同一套取法。
  - commit `cb53364`（E3）：`AttemptFailure`/`FailureClassSummary` 增
    `upstream_error_code`，`terminal_route_failure_error` details 增 `upstream_error_codes` map。
  - **偏离方案**：`FailureClassSummary` 同时增 `upstream_name`（同取法取最常见上游名）。
    方案 E2 的 RED 要求终态 message 含上游名，而单路由/SSE 终态唯一通道就是该摘要，
    不加名字没有别的消息通道可满足该断言；实现与码完全同构，成本为零。
  - 行号漂移：`ledger_failure_summary` 现于 `errors.rs:41`，`class_summaries` 于
    `route_attempts.rs:657`，`terminal_route_failure_error` 于 `errors.rs:125`。

### E4 request_id 进客户端（对齐 OpenAI/Anthropic）

> 这条性价比最高：即使正文永远不透传，用户复制一行 request_id，运维就能在日志里精确定位。

- [x] RED：任意错误响应
  - 响应头含 `x-gateway-request-id`；
  - JSON 错误体 `details.request_id` 有值，且与日志里的 `request_id` 一致；
  - message 尾部含 `request_id=<rid>`（SSE 路径唯一可见处）；
  - **成功响应**也应带该响应头（便于事后追溯），但不改 body。
  - 已落地：`tests/gateway/chat/request_id_surfacing.rs`（JSON 非流式 503 + SSE 流式 503 两用例）。
- [x] GREEN（commit `e343529`）：
  - `GatewayError` 增 `request_id: Option<String>`（在 `GatewayErrorMeta` 上，`with_request_id`
    在**响应边界**统一注入；注入点覆盖 JSON / Anthropic / SSE 三条出口）；
  - `into_json_response`（`errors.rs:1056`）统一加 `x-gateway-request-id` 响应头 +
    `details.request_id`；
  - SSE 路径在 `response.failed` / `error` 帧的 message 尾部 + `details.request_id` 追加；
    接线点：两个 dispatch 边界包装 + `claude_messages`（rid 在边界 mint，`map_err` 挂到错误上）、
    `dispatch_claude_success` 的 9 处 Anthropic 出口、`early_keepalive_stream`（rid 随状态流转，
    覆盖 channel-closed / synthesize 失败）、两个 `finish_with_gateway_error`（流中途错误帧）。
  - 顺带：`GatewayErrorMeta` 装箱为 `Box<GatewayErrorMeta>`（新增 `Option<String>` 把
    `Classified` 变体顶破 clippy `result_large_err` 128B 阈值）。
- [x] 注意：`request_id` 必须是网关自己生成的 id：在 `dispatch_streaming_request` /
      `process_gateway_request_with_runtime_settings` / `claude_messages` 里 mint，
      **不**回显上游 request id。
- [x] 受影响既有断言（仅更新，不改语义）：`chat/core.rs:853` 配额错误 message 改为
  prefix + `request_id=` 尾部断言；`stream_lifecycle.rs:661` 响应失败帧 message 断言
  追加 `; request_id=` 前缀匹配。

### E5 可选的正文摘录开关（默认关）

- [x] 新增运行时设置 `upstream_error_body_excerpt_enabled`（默认 **false**）+
      `upstream_error_body_excerpt_max_chars`（默认 200，范围 50..=2000）。
      commit: `caed69c`；实现：`src/state/types.rs`（两个 default fn）、
      `src/state/runtime_settings.rs`（IMMEDIATE 数组 + from/apply + validate 范围）、
      `src/main.rs`（env 读取 + clamp）、`state.rs` re-export 补漏。
- [x] 打开时：客户端消息尾部追加**有界**的上游正文摘录，且必须先过
      既有脱敏（复用 `safe_upstream_*` 一族的思路：剥 key/token 形状的串）。
- [x] 关闭时：行为与 E2 完全一致（单测 `e5_no_excerpt_keeps_e2_message_shape_exactly` +
      集成 `upstream_error_body_excerpt_off_keeps_body_red_line`）。
- [x] 文档必须写明：**内网自有上游才建议打开**；公网/多租户场景保持关闭。
- [x] 理由：内网部署里运维同时拥有上下游，正文就是最快的线索；但这必须是显式选择，不能是默认。

实现说明（与快照的出入）：

- 摘录取的是**整个脱敏后的正文**（`upstream.rs:2062` 处 `error_text` 全量过
  `sanitize_upstream_body_excerpt`），不是仅 message 字段——方案未指定粒度，
  全量正文对运维更有用，且同样过同一脱敏器。正文经过：脱敏（剥 `sk-`、
  `Bearer`、JSON secret 对，`[redacted]` 标记）→ 空白折叠 → 按
  `max_chars` 截断加 `…`。
- 通道共三处：① per-attempt message 尾部 `; upstream_body="..."`（
  `from_classified_upstream_failure`，转义 `\` 与 `"`）；② 终态摘要
  `body="..."`（`ledger_failure_summary`，同样转义）；③ 终态 details
  `upstream_error_body_excerpt`（`AttemptLedger::upstream_error_body_excerpt()`
  取最常见，开关关闭时整键缺席，不出现 `null`）。
- `AttemptFailure`/`FailureClassSummary`/`record_failure_with_status` 各加
  `upstream_error_body_excerpt` 字段（E3 同模式）；`record_cooled_route_attempt`
  恒传 `None`（存量冷却无正文）。
- 测试：`sanitize_upstream_body_excerpt` 5 单测（sk-/Bearer/JSON secret 剥除、
  截断省略号、空白折叠、空输入、plain text 直通）+ gateway 4 单测 +
  `tests/gateway/chat/feedback.rs` 2 端到端（开关开/关）。

### E6 前端 + 文档

- [x] 前端设置项（group `observability`，模式 `immediate`）：E5 的两个设置。
      commit: `c57a666`；`frontend/src/types/index.ts`、
      `frontend/src/utils/runtimeSettings.ts`（新 group + 两条目）、
      `runtimeSettings.spec.ts`（fixture +2、expectedKeys +2、51→53、immediate 38→40、
      groups 断言 + observability）。
- [x] `DEPLOYMENT.md` 排障小节：新增「怎么读一条网关错误」——
      `[网关码] 人读描述 (status=..., upstream=..., code=..., request_id=...)` 各字段含义，
      以及拿 request_id 去日志里 grep 什么字段（含 `error_excerpt` 字段说明）。
- [~] 管理页日志详情：**未做专用列**——`upstream_error_code` 目前不存独立字段，
      做成列需要 usage-log schema 变更（Postgres 列 + 本地 JSON + 查询过滤 + 前端列），
      超出 E 系列范围；且 `UsageLog.error_message` 已存同一条客户端消息（E2/E3/E5
      之后自带 `upstream=`/`code=`/`request_id=`），Admin > Logs 里可直接搜索。
      留作后续独立任务（若需要按码筛选再加专用列）。
- [x] 文档写明（DEPLOYMENT.md Intranet 小节表格 + 设置描述）：
      **内网自有上游才建议打开**；公网/多租户场景保持关闭。

---

## 6. 测试矩阵

| 场景 | 期望 |
|------|------|
| 上游返回字符串码 | 客户端 message 与 details 都带该 token（E1/E2） |
| 上游只返回数字码 | 消息里出现 status，不出现空的 `code=`（E1/E2） |
| 码含空格/超长/怪字符 | 被净化丢弃，消息里不出现（E1） |
| **上游正文含特征串** | 客户端 message **不含**该串（隐私红线，E2） |
| 多路由全失败 | 终态摘要含出现最多的上游码，details 有 `upstream_error_codes`（E3） |
| 任意错误 | 响应头 `x-gateway-request-id` + details.request_id + message 尾部（E4） |
| SSE 流式错误 | 同上，且信息在 message 字符串里（E4） |
| E5 开关关闭 | 与 E2 完全一致；打开 → 追加有界摘录 |
| 回归 | 既有错误码/状态断言全绿——**本方案不改任何分类语义** |

---

## 7. 风险与回滚

| 风险 | 缓解 |
|------|------|
| 净化不严，上游把正文塞进 `code` 字段导致泄漏 | 字符集白名单 + 长度上限 + 「不合规就整体丢弃」；隐私红线用例必须有 |
| 消息变长，客户端 UI 截断 | kv 段固定顺序且短；正文摘录默认关 |
| 既有测试大量断言 message 全等 | 预期会有一批用例要更新——**只允许更新断言，不允许改分类语义**；改动前后 `error_code`/status 必须一致 |
| 回显上游 request id 引入跨信任域标识 | E4 明确只用网关自己的 id |

- **回滚**：E5 关开关即可；E1–E4 是纯增量描述信息，revert commit 即可。
- **Constraint:** 不改错误分类语义；不把上游正文默认透传给客户端。
- **Rejected:**
  ① 直接把上游错误体原样透传（会回显 prompt/凭证，且不同上游格式不一，客户端更难读）；
  ② 只往 `details` 里加而不进 message（SSE 路径上 codex 只显示 message，等于没做）；
  ③ 回显上游的 request id（跨信任域标识）。
- **Confidence:** 现状已逐行核实（`upstream_client_message` 只用 status、
  `extract_upstream_error_code` 只认数字、客户端错误体无 request_id 均为硬事实）。
- **Scope-risk:** low-medium（触碰面广但都是描述性字段；风险集中在隐私红线与既有断言更新）。
