# 快速原子配置变更实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让上游和下游保存、更新、删除在 PostgreSQL 大量 usage log 时仍快速完成，并保证数据库、内存路由和关联配置一致。

**Architecture:** PostgreSQL 配置事务只同步配置表，不重放 usage log；usage log 继续由独立批量追加路径写入。状态变更持有内存锁直到持久化提交后立即切换，能力探测只做非阻塞入队并由现有 reconcile 补偿。

**Tech Stack:** Rust、Axum、Tokio、PostgreSQL、`tokio-postgres`、现有 capability probe queue、Cargo integration tests、Docker Compose。

---

### Task 1: 建立 PostgreSQL 与探测队列回归测试

**Files:**
- Modify: `tests/postgres_roundtrip.rs`
- Modify: `tests/capability_state.rs`

- [ ] **Step 1: 写配置事务不插入 usage log 的失败测试**

在 `tests/postgres_roundtrip.rs` 增加 PostgreSQL 测试 `postgres_config_mutation_does_not_insert_usage_logs`：

1. 使用现有 `PG_TEST_DATABASE_URL`、`reset_test_database` 和 `env_lock`。
2. 创建一个 upstream 和一条已通过 `append_usage_log`/`flush_usage_logs_for_test` 持久化的日志。
3. 创建临时触发器，任何 `usage_logs` INSERT 都抛出异常：

```sql
CREATE FUNCTION reject_usage_log_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'config mutation must not insert usage logs';
END;
$$;
CREATE TRIGGER reject_usage_log_insert_trigger
BEFORE INSERT ON usage_logs
FOR EACH ROW EXECUTE FUNCTION reject_usage_log_insert();
```

4. 调用 `state.set_upstream_active(&upstream_id, false).await`，预期当前实现失败，修复后成功。
5. 用 `query_usage_log_ctid`、`query_usage_logs_page` 断言日志仍存在且内容不变。
6. 在测试末尾 `DROP TRIGGER ...; DROP FUNCTION ...;`，即使断言失败也通过现有测试清理路径删除。

- [ ] **Step 2: 写删除级联和审计保留的失败测试**

在同一测试文件增加 `postgres_delete_config_cascades_without_deleting_usage_logs`：

```rust
state.remove_downstream(&downstream_id).await.unwrap();
state.remove_upstream(&upstream_id).await.unwrap();
assert_eq!(scalar_count("SELECT COUNT(*) FROM downstreams WHERE id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM downstream_model_allowlist WHERE downstream_id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM downstream_ip_allowlist WHERE downstream_id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM upstreams WHERE id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM upstream_supported_models WHERE upstream_id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM upstream_premium_models WHERE upstream_id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM upstream_model_request_costs WHERE upstream_id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM dialect_profiles WHERE upstream_id = $1"), 0);
assert_eq!(scalar_count("SELECT COUNT(*) FROM usage_logs WHERE id = $1"), 1);
```

使用测试现有的 `psql` helper，把 `$1` 替换为安全转义后的测试 UUID；测试数据只使用 fixture key，不读取生产数据库。

- [ ] **Step 3: 更新探测队列回归测试契约**

在 `tests/capability_state.rs` 中修改以下现有测试的预期：

- `inserting_upstream_waits_for_every_probe_job_when_queue_is_full` 改名为 `inserting_upstream_does_not_wait_for_full_probe_queue`，队列预置 blocker 后，`insert_upstream` 在 250ms 内返回成功，且 upstream 已存在。
- `updating_upstream_waits_for_every_probe_job_when_queue_is_full` 改名为 `updating_upstream_does_not_wait_for_full_probe_queue`，更新在 250ms 内返回成功，队列中的 blocker 保持不变。
- `inserting_upstream_reports_missing_probe_worker_after_persisting` 改为断言无 worker 时插入成功且状态已持久化。
- `updating_upstream_reports_closed_probe_queue_after_persisting` 改为断言队列关闭时更新成功且状态已持久化。
- `freekey_sync_reports_missing_probe_worker` 与 `freekey_sync_reports_closed_probe_queue` 改为断言配置同步成功；具体 route probe 由 reconcile 补偿。

运行：

```bash
rtk cargo test --locked --offline postgres_
rtk cargo test --locked --offline capability_state
```

预期新增 PostgreSQL 测试在未设置 `PG_TEST_DATABASE_URL` 时按现有约定 skip；探测队列测试在修改前至少有一个明确失败。

- [ ] **Step 4: 提交回归测试**

```bash
rtk git add tests/postgres_roundtrip.rs tests/capability_state.rs
rtk git diff --cached --check
rtk git commit -m "test(state): cover atomic config persistence and probe backpressure" \
  -m "Constraint: Preserve usage logs while requiring deleted config relations to disappear" \
  -m "Confidence: high" \
  -m "Scope-risk: moderate"
```

### Task 2: 去除配置事务中的 usage log 重放并关闭取消窗口

**Files:**
- Modify: `src/state/postgres.rs:302-309`
- Modify: `src/state.rs:3060-3120`

- [ ] **Step 1: 删除 PostgreSQL 配置事务中的日志插入**

将 `PostgresStateStore::replace_state` 从：

```rust
sync_config_tables(&tx, state).await?;
insert_usage_logs(&tx, &state.usage_logs).await?;
tx.commit().await.map_err(io_other)
```

改为：

```rust
sync_config_tables(&tx, state).await?;
tx.commit().await.map_err(io_other)
```

保留 `append_usage_logs` 和 `insert_usage_logs`，它们是唯一的 usage log 批量写入路径。

- [ ] **Step 2: 让内存状态切换紧跟数据库提交**

将 `mutate_persisted_state` 改成持有 `inner` guard 直到持久化完成，核心结构如下：

```rust
let _persist_guard = self.config_persist_lock.lock().await;
let mut state = self.inner.lock().await;
let mut candidate_state = state.clone();
let result = mutator(&mut candidate_state)?;

if !downstream_plaintext_pairs_unchanged(
    &state.downstreams,
    &candidate_state.downstreams,
) {
    validate_downstream_plaintext_pairs(&mut candidate_state);
}

self.config_store
    .persist_config(&candidate_state)
    .await
    .map_err(map_io)?;

state.upstreams = candidate_state.upstreams;
state.downstreams = candidate_state.downstreams;
state.announcement = candidate_state.announcement;
state.global_context_profiles = candidate_state.global_context_profiles;
Ok(result)
```

这样数据库提交失败时不会替换内存，提交成功后没有新的 `.await` 取消点，客户端断开不会留下 DB 已变更但内存仍旧的窗口。保持 `config_persist_lock -> inner` 的锁顺序，与 usage log flush 路径一致。

- [ ] **Step 3: 运行 PostgreSQL 回归测试确认 GREEN**

```bash
rtk cargo test --locked --offline postgres_
```

预期触发器测试通过、usage log 行仍保留、删除后的配置关联行均为零。

- [ ] **Step 4: 提交持久化修复**

```bash
rtk git add src/state.rs src/state/postgres.rs
rtk git diff --cached --check
rtk git commit -m "fix(state): make config mutations fast and atomic" \
  -m "Avoid replaying historical usage logs during config writes and keep the in-memory swap adjacent to the durable commit." \
  -m "Constraint: Preserve audit rows and synchronous database consistency" \
  -m "Confidence: high" \
  -m "Scope-risk: moderate"
```

### Task 3: 让能力探测入队不阻塞配置接口

**Files:**
- Modify: `src/state.rs:108,2492-2558`
- Modify: `tests/capability_state.rs`

- [ ] **Step 1: 实现非阻塞 route batch 入队**

删除仅用于配置变更入队的 `CAPABILITY_PROBE_ENQUEUE_TIMEOUT`，将 `submit_capability_probe_jobs` 的发送段改为：

```rust
match sender.try_send(batch) {
    Ok(()) => {}
    Err(TrySendError::Full(batch)) => {
        tracing::warn!(jobs = batch.jobs().len(), "capability probe queue is full; reconcile will retry");
    }
    Err(TrySendError::Closed(batch)) => {
        tracing::warn!(jobs = batch.jobs().len(), "capability probe queue is closed; reconcile will retry");
    }
}
Ok(())
```

引入 `tokio::sync::mpsc::error::TrySendError`，不要记录 API key。保留完整 batch 和 route 去重逻辑；每秒 reconcile 负责补偿 Full/Closed 情况。

- [ ] **Step 2: 运行探测队列 RED/GREEN 测试**

```bash
rtk cargo test --locked --offline capability_state
```

预期所有“队列满/worker 缺失仍成功”的新契约通过，并且现有“完整 route batch 只提交一次”断言继续通过。

- [ ] **Step 3: 验证手动探测接口语义未改变**

运行 capability admin 相关测试：

```bash
rtk cargo test --locked --offline capability_probe
rtk cargo test --locked --offline admin_model_probe
```

预期手动探测仍返回 queued/202，队列确实不可用时仍返回 503；只有配置变更路径不再等待队列容量。

- [ ] **Step 4: 提交探测解耦修复**

```bash
rtk git add src/state.rs tests/capability_state.rs
rtk git diff --cached --check
rtk git commit -m "fix(state): decouple capability probes from config writes" \
  -m "Queue configuration-change probes without holding the save or delete request on worker capacity." \
  -m "Constraint: Preserve bounded deduplication and reconcile retry" \
  -m "Confidence: high" \
  -m "Scope-risk: moderate"
```

### Task 4: 全量验证、真实 CRUD、部署与回滚证据

**Files:**
- Verify: Rust workspace, `frontend/**`
- Runtime: `/home/kavin/docker/chat-responses-codex/docker-compose.yml`

- [ ] **Step 1: 运行完整测试与构建**

```bash
rtk cargo test --locked --offline
rtk cargo build --release --locked --offline
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run build
```

预期 Rust 0 failures，前端 0 failures，release 和生产前端构建均退出 0。

- [ ] **Step 2: 构建最终镜像并只替换 gateway**

```bash
rtk sha256sum target/release/chat-responses-codex
rtk docker tag chat-responses-codex:latest chat-responses-codex:rollback-before-fast-config
rtk docker create --name chat-responses-fast-config-image chat-responses-codex:latest
rtk docker cp target/release/chat-responses-codex chat-responses-fast-config-image:/usr/local/bin/chat-responses-codex
rtk docker commit chat-responses-fast-config-image chat-responses-codex:final-fast-config
rtk docker rm chat-responses-fast-config-image
rtk docker tag chat-responses-codex:final-fast-config chat-responses-codex:latest
rtk docker run --rm --entrypoint sha256sum chat-responses-codex:final-fast-config /usr/local/bin/chat-responses-codex
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml up -d --no-deps gateway
```

镜像内和宿主 SHA 必须一致；postgres/redis 不重建。

- [ ] **Step 3: 进行真实配置 CRUD 验收**

使用临时唯一 ID 通过管理 API 创建一个 upstream 和 downstream，记录每个请求的 `time_total`；然后更新、删除，并在每次 204/200 后立即执行：

```bash
rtk docker exec chat-responses-codex-postgres psql -U chat_responses_codex -d chat_responses_codex -c "SELECT id FROM upstreams WHERE id LIKE 'perf-%'; SELECT id FROM downstreams WHERE id LIKE 'perf-%'; SELECT COUNT(*) FROM upstream_supported_models WHERE upstream_id LIKE 'perf-%'; SELECT COUNT(*) FROM downstream_model_allowlist WHERE downstream_id LIKE 'perf-%'; SELECT COUNT(*) FROM downstream_ip_allowlist WHERE downstream_id LIKE 'perf-%'; SELECT COUNT(*) FROM dialect_profiles WHERE upstream_id LIKE 'perf-%';"
rtk curl -fsS http://127.0.0.1:3000/api/admin/upstreams
rtk curl -fsS http://127.0.0.1:3000/api/admin/downstreams
```

断言每个配置请求小于 1 秒；删除后主表、模型表、allowlist、IP allowlist、dialect profile 均无临时 ID，历史 usage log 数量不下降。失败时先保存 response/log 证据，不手工删除未知用户数据。

- [ ] **Step 4: 审计探测和运行时稳定性**

验证配置请求返回后 capability probe 在后台执行，队列满时请求仍结束；检查：

```bash
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml ps
rtk docker inspect --format '{{.State.Status}} {{.State.Health.Status}} {{.RestartCount}}' chat-responses-codex
rtk curl -fsS http://127.0.0.1:3000/healthz
STARTED_AT="$(date --iso-8601=seconds)"
rtk docker logs --since "$STARTED_AT" chat-responses-codex
```

统计 499、502、`upstream_stream_error_event`、panic、真实 ERROR 和重启次数；确认 usage log 总数保持不变。

- [ ] **Step 5: 删除临时数据、核对工作树并请求最终审查**

```bash
rtk git diff --check
rtk git status --short --branch
rtk git log -8 --oneline --decorate
```

只允许保留用户原有的 `frontend/tests/router/index.spec.ts` 与 `tests/troubleshooting.rs` 未提交修改；完成后对实现提交范围请求独立代码审查，再决定最终保留或回滚镜像。
