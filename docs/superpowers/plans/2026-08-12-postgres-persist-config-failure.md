# 方案：Postgres 后端配置保存全部失败（failed to save runtime settings）

日期：2026-08-12
状态：✅ 已完成（2026-08-12）
- 任务 1（Hotfix 28 列对齐 + 三态布尔参数）：`05c33a1`
- 任务 2（insert_statement 辅助函数 + 防回归单测）：`98d3c79`
- 任务 3（persist 失败打印完整错误链 + details.backend）：`2c9a836`
- 任务 4（Postgres round-trip 集成测试，TEST_DATABASE_URL 未设置时跳过）：`98d3c79` 同批测试文件
严重级别：**高——Postgres 后端下所有配置写入均失败**（运行时设置、上游、下游、公告的保存共用同一条持久化路径）。

---

## 一、现象

管理后台修改网关配置（运行时设置）后点击保存，报错 `failed to save runtime settings`（错误码 `runtime_settings_persist_failed`，`src/server/admin.rs:913`）。实际上在 Postgres 后端下，上游/下游配置保存同样会失败，只是用户先撞到了这一处。

## 二、根因（已核实，可静态验证）

保存链路：`update_runtime_settings`（`src/state.rs:2737`）→ `config_store.persist_config` → Postgres 后端 `replace_state`（`src/state/postgres.rs:313`）→ `sync_config_tables` → **`sync_upstreams`**。

`sync_upstreams` 的 INSERT 语句（`src/state/postgres.rs:1156-1199`）三方数量不一致：

- 列清单 **28 列**（id … dialect_preset）；
- VALUES 占位符 **29 个**（`$1..$29`，postgres.rs:1164-1168）；
- params 数组 **28 个**（postgres.rs:1126-1155）。

tokio-postgres 执行时校验参数个数与预编译语句不符，直接报错（"wrong number of parameters: expected 29, got 28"；即使数量对上，Postgres 也会拒绝列数≠表达式数的 INSERT）。错误经 `io_other` 包装冒泡，事务回滚，前端收到 `Failed to save runtime settings`。

**只要 state 中存在 ≥1 个上游，Postgres 后端的每次 persist_config 必失败**——必现。

引入提交：`f44dfc7`（static dialect presets，加 `dialect_preset` 列时未重新对齐三方数量）。

### 同一语句中的第二处潜伏 bug（修完数量后会立刻暴露）

第 24 列 `strip_nonstandard_chat_fields` 在 schema 中是 **BOOLEAN**（postgres.rs:1769），但对应参数传的是 `as_db_str()` 返回的 **TEXT**（"auto"/"always_strip"/"forward"，postgres.rs:1150）。类型不匹配同样会报错。插入值应为布尔，语义与 ON CONFLICT 分支保持一致（`EXCLUDED.nonstandard_field_policy <> 'forward'`，即 policy ≠ Forward → true）。

### 为什么本地/CI 没发现

文件后端（`file_store.rs:175`）不走这条 SQL，本地默认文件后端一切正常；`tests/state_store.rs` 只覆盖文件后端。内网 compose 设置了 `DATABASE_URL`（Postgres 后端）→ 必现。**没有任何针对 Postgres 写路径的测试。**

### 伴随的可观测性缺陷

`admin.rs:908` 的错误日志只打 `kind = ?error.kind()`——对 `io_other` 包装的错误恒为 `Other`，真实 DB 错误文本被完全丢弃，内网日志无法定位。这也是这个必现 bug 一直没被从日志里看出来的原因。

## 三、开发任务

### 任务 1：修复 `sync_upstreams` INSERT（核心，小改动）

1. 对齐列/占位符/参数三方为 28：删除多余的一个占位符（VALUES 行按列清单分组重排，保持可读）。
2. `strip_nonstandard_chat_fields` 列（BOOLEAN）参数改传布尔：`&(upstream.strip_nonstandard_chat_fields != NonstandardFieldPolicy::Forward)`（与 ON CONFLICT 表达式语义一致：auto/always_strip → true）；`nonstandard_field_policy` 列（TEXT）保留 `as_db_str()`。
3. 逐列核对参数与列名的语义对应（当前 params[7] 对 request_quota_5h 等 legacy 双写保持不变）。

### 任务 2：从构造上消灭这一类 bug

手写 `VALUES ($1,...,$n)` 与参数数组的手工对齐已经错过一次，必须结构性防回归：

1. 新增辅助函数（postgres.rs 内部即可）：
   ```rust
   fn insert_statement(table: &str, columns: &[&str], conflict_clause: &str) -> String
   ```
   由列清单自动生成 `$1..$n` 占位符；调用处 `debug_assert_eq!(columns.len(), params.len())`。
2. 将 `sync_upstreams`、`sync_downstreams` 两条大 INSERT 迁移到该辅助函数（其余单表小语句可不动）。
3. 纯字符串单元测试（无需真实 DB）：对生成后的语句断言"列数 == 最大 `$n` == params 长度"，并对 postgres.rs 中所有含 `VALUES` 的手写语句做一次静态扫描断言（简单正则计数即可），防止未来再手写失衡语句。

### 任务 3：错误可观测性

1. `admin.rs:907-919`：日志改为输出完整错误链（`error = %error` + `source`），保留现有对外响应文案（不向下游泄漏 SQL 细节），response details 增加 `backend: "postgres" | "file"`。
2. `update_runtime_settings` 的 persist 失败路径同样 `tracing::error!` 带完整错误（`state.rs:2772-2775` 附近已有映射，只需确保错误文本不丢）。

### 任务 4：Postgres 写路径回归测试

1. 新增可选集成测试 `tests/postgres_store.rs`：读环境变量 `TEST_DATABASE_URL`，未设置时 skip（`#[ignore]` 或运行时早退）。用真实 Postgres 做 round-trip：`persist_config`（含 ≥1 upstream、≥1 downstream、runtime_settings 文档）→ `load` → 断言等值。CI 若有 postgres service 则启用。
2. 该测试必须覆盖 `strip_nonstandard_chat_fields` 三种取值的 round-trip（auto/always_strip/forward → 布尔列 + 文本列的双写与回读优先级，参考 `decode_nonstandard_field_policy`，postgres.rs:1967）。

## 四、验证与部署

1. 本地：`TEST_DATABASE_URL` 指向临时 Postgres 跑新集成测试；`rtk cargo test` 全绿。
2. 内网：部署修复版后依次验证——保存运行时设置、编辑保存一个上游、编辑保存一个下游、重启容器后配置仍在（确认真正写进了 DB 而非仅内存）。
3. 注意：该 bug 期间内网所做的所有配置修改**只存在于内存**，容器重启即丢失。部署修复版前提醒用户记录近期改动，部署后重新保存一遍。

## 五、临时缓解

无好的免代码缓解：Postgres 写路径整体损坏。不建议切文件后端（会绕开 DB 中既有配置）。此修复应最高优先级发布；改动本身很小（一条 SQL + 参数），可以先出 hotfix 再做任务 2/4 的防回归。
