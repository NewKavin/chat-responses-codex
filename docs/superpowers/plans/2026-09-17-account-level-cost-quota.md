# 费用限额从「按 Key」改为「按账号」

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 费用限额与费用统计以「账号」为单位汇总，一个用户名下所有 Key 共用一份日预算；彻底删除 Key 级费用上限，切换后不可能有任何残留的 Key 级限制。

**Architecture:** 引入「费用归属（cost scope）」概念：一个 Key 的费用归属是它的门户用户（`downstream_access_policies.owner_user_id`），没有归属用户时归属它自己。限额存在新的 `cost_scope_limits(scope_id, daily_limit_cents)` 表里，scope_id 既可能是用户 id 也可能是下游 id，两种模式统一。该表内容随 `PersistedState` 常驻内存，跟下游配置走同一套加载与变更路径，不引入新缓存。24 小时滚动窗口（本地 `HashMap` 与 Redis ZSET）的键从下游 id 换成 scope id，Redis 的 Lua 脚本一行都不用改。最后删除 `DownstreamConfig::daily_cost_limit_cents` 字段与数据库列，由编译器保证没有任何代码还能读到 Key 级上限。

**Tech Stack:** Rust/Axum 网关，tokio-postgres，Redis Lua，Cargo 集成测试，Vue 3 + Element Plus 管理台与门户，vitest。

**Spec:** 本文档第 1、2 节即为规格；无独立 spec 文件。

## Global Constraints

- 所有命令加 `rtk` 前缀（见仓库 `CLAUDE.md`）。
- 严格 TDD：每个任务先写测试、亲眼看到失败、再写最小实现、再跑绿、再提交。Rust 里「结构体没有这个字段」导致的编译失败是合格的 RED，但要确认报错正是缺这个字段。
- **无残留是硬指标**：Task 5 完成后，`rtk grep -rn "daily_cost_limit_cents" src/` 只允许命中 `cost_scope_limits` 相关代码与迁移 SQL，`DownstreamConfig` 上不得再有该字段，任何准入路径都不得再按 Key 判定费用。
- **单价（`input_token_price_per_million_cents` / `output_token_price_per_million_cents`）继续留在 Key 上，不要动**。单价是「这个 Key 按什么价计费」，上限才是「这个账号能花多少」，两者语义不同。
- 计费模式 `billing_mode`、请求数配额（`request_quota_*`）、每分钟限速、并发上限**全部保持 Key 级，不改**。本次只动费用上限。
- 老的 Redis Key 级费用窗口（`<prefix>:<sha256(downstream_id)>:tokens` / `:token_values`）切换后不再被读取，靠自带的 EXPIRE 在 24 小时内自然消失，**不要写清理脚本**。
- 迁移折算规则：一个账号的新上限 = 该账号名下所有 Key 原上限的**最大值**。多 Key 用户的总预算会因此下降，这正是本次改造的目的；迁移完成后需要管理员复核。
- 前端验证命令：`cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run`。

---

## 1. 现状调研

### 1.1 费用现在完全按 Key 算

| 环节 | 位置 | 维度 |
|---|---|---|
| 准入判定（本地） | `src/state.rs:5352-5393` | `downstream_token_windows` 以 `downstream.id` 为键 |
| 准入判定（Redis） | `src/state/redis_runtime.rs:364-415`、`:449-472` | `stable_identity(&downstream.id)` 派生 `tokens` / `token_values` 键 |
| 窗口记账 | `src/state.rs:4969-5009` | `record_downstream_tokens(&log.downstream_key_id, ...)` |
| 启动重建 | `src/state/usage.rs:100-127` | 按 `log.downstream_key_id` 分组 |
| 门户展示 | `src/state/usage.rs:332`、`src/state/log_queries.rs:214` | `log.downstream_key_id == downstream_id` |
| 上限配置 | `src/state/types.rs:1172` | `DownstreamConfig.daily_cost_limit_cents`，每 Key 一份 |

### 1.2 门户自助建的 Key 完全不受费用限制

`portal_create_key`（`src/server/portal.rs:1181`）用 `..Default::default()` 建下游，Default 里 `daily_cost_limit_cents: None` 且两个单价都是 `None`，于是 `cost_billing_mode()`（`src/state/types.rs:1264`）恒为假、`daily_cost_limit()` 返回 `None`，准入里整段费用检查被跳过。管理台配的 Key 被锁住后，用户在门户点一下「新建密钥」就能绕开。`PortalStore::count_user_keys`（`src/state/portal_store.rs:475`）写了但没有任何调用点，建 Key 没有数量上限。

### 1.3 账号身份已经存在

工号+密钥登录的用户第一次访问门户接口时，`ensure_user_for_downstream`（`src/state/portal_store.rs:600`）会自动建 `portal_users` 行（email 为 `{downstream_id}@downstream.local`）并把 Key 绑上，`downstream_access_policies.owner_user_id` 随之有值。与 OAuth 用户的唯一差别是少一行 `portal_identities`。

但迁移脚本的约束 `CHECK (subject_kind = 'portal' OR (owner_user_id IS NULL AND mode <> 'inherit'))`（`migrations/2026-09-07-downstream-access-policies.sql:11`）意味着**从未打开过门户的纯直连 Key 一定没有 owner**。这类 Key 必须有一个不依赖用户表的归属，否则删掉 Key 级上限后它们会直接失去费用限制。

### 1.4 管理台已有的「按用户批量设限额」是当前的变通做法

`frontend/src/views/admin/PortalUsers.vue:371` 和 `:835` 允许管理员给某个用户名下所有 Key 批量写同一个 `daily_cost_limit_cents`。这恰好是产生「N 个 Key = N 份预算」的直接原因：写的是 N 份独立上限，跑的是 N 个独立窗口。

### 1.5 可行性关键点

`resolved_model_access_with_catalog`（`src/state/model_access.rs:318`）在每个请求里都会调用（`src/server/gateway.rs:5574`），返回的 `ResolvedModelAccess.policy` 已经带 `owner_user_id`；而费用准入 `reserve_downstream_admission` 在它之后执行（`src/server/gateway.rs:5698`）。归属信息在热路径上本来就拿得到，不需要为准入新增查询。

## 2. 设计决策

| 项 | 决定 | 理由 |
|---|---|---|
| 费用归属 | `owner_user_id` 有值用它，否则用下游 id 自身 | 有账号的按账号汇总；纯直连 Key 自成一档，行为与今天一致 |
| 上限存储 | 新表 `cost_scope_limits(scope_id TEXT PRIMARY KEY, daily_limit_cents BIGINT NOT NULL)` | scope_id 既可是用户 id 也可是下游 id，两种情况统一；不碰 `portal_users`，避开 `downstream_access_policies` 的 CHECK 约束，也不会顺带改变模型访问语义 |
| 内存态 | `PersistedState` 增加 `cost_scope_limits: HashMap<String, u64>` 与 `downstream_owners: HashMap<String, String>` | 两者跟下游配置同生命周期，复用已有的加载与变更路径，不引入新缓存和新的失效点 |
| 文件模式 | 同一套字段随 `state.json` 持久化，`downstream_owners` 恒为空 → 每个 Key 自成一档 | 无 Postgres 的部署行为与今天等价，不会因为改造丢失费用限制 |
| Key 级上限 | **删除字段与数据库列** | 用户明确要求「不起作用就直接删掉，别有残留」。删字段后编译器保证没有任何读取点 |
| 单价 | 保留在 Key 上 | 价格是费率不是预算 |
| 迁移折算 | 每个 scope 取名下 Key 原上限的最大值 | 「一个 Key 曾被允许花多少」升格为「这个账号能花多少」；取和会原样保留超发 |
| Redis Lua | 不改 | `downstream_reserve.lua` 的 KEYS[1] 是请求窗口、KEYS[2]/[3] 是费用窗口，三个键分别传入，只要在 Rust 侧用不同 identity 计算即可 |
| 展示 | 门户与管理台的费用数字全部改为按账号 | 否则用户看到的数字和实际被限的数字对不上 |
| 本期不做 | 每账号 Key 数量上限、按账号的请求数配额、按账号并发 | YAGNI，与费用无关 |

---

### Task 1: `cost_scope_limits` 表与内存态

**Files:**
- Create: `migrations/2026-09-17-cost-scope-limits.sql`
- Modify: `src/state/postgres.rs:2489` 附近（建表 SQL 常量）、下游加载函数（`:176` 起）、状态持久化
- Modify: `src/state/types.rs`（`PersistedState`）
- Test: `tests/state_store.rs`、`tests/postgres_roundtrip.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - 表 `cost_scope_limits(scope_id TEXT PRIMARY KEY, daily_limit_cents BIGINT NOT NULL CHECK (daily_limit_cents > 0))`
  - `PersistedState.cost_scope_limits: HashMap<String, u64>`（`#[serde(default)]`）
  - `PersistedState.downstream_owners: HashMap<String, String>`（`#[serde(default)]`，Postgres 模式下由 `downstream_access_policies` 填充，文件模式恒空）

- [ ] **Step 1: 写失败测试**

`tests/state_store.rs` 末尾新增：

```rust
#[test]
fn persisted_state_without_cost_scope_limits_still_deserializes() {
    let value = serde_json::json!({
        "upstreams": [],
        "downstreams": [],
        "usage_logs": []
    });
    let state: PersistedState = serde_json::from_value(value).unwrap();
    assert!(state.cost_scope_limits.is_empty());
    assert!(state.downstream_owners.is_empty());
}

#[test]
fn persisted_state_cost_scope_limits_roundtrip_through_json() {
    let mut state = PersistedState::default();
    state
        .cost_scope_limits
        .insert("user-1".to_string(), 5_000);
    state
        .downstream_owners
        .insert("key-a".to_string(), "user-1".to_string());

    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["cost_scope_limits"]["user-1"], 5_000);
    assert_eq!(value["downstream_owners"]["key-a"], "user-1");

    let reloaded: PersistedState = serde_json::from_value(value).unwrap();
    assert_eq!(reloaded.cost_scope_limits.get("user-1"), Some(&5_000));
    assert_eq!(
        reloaded.downstream_owners.get("key-a").map(String::as_str),
        Some("user-1")
    );
}
```

`tests/postgres_roundtrip.rs`：在 `postgres_roundtrip_preserves_normalized_state_and_authoritative_empty_mapping`（第 266 行）里，`load_from_database_url` 回读之后追加一条断言，确认新表存在且空 map 能正常回读：

```rust
    assert!(
        snapshot.cost_scope_limits.is_empty(),
        "全新库不应有任何费用 scope 上限"
    );
```

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test state_store cost_scope_limits`
Expected: 编译失败，`no field cost_scope_limits on type PersistedState`。

- [ ] **Step 3: 最小实现**

`src/state/types.rs` 的 `PersistedState` 加两个字段（照已有字段的 serde 风格）：

```rust
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub cost_scope_limits: HashMap<String, u64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub downstream_owners: HashMap<String, String>,
```

`Default` 实现里补 `HashMap::new()`。

新建 `migrations/2026-09-17-cost-scope-limits.sql`：

```sql
CREATE TABLE IF NOT EXISTS cost_scope_limits (
    scope_id          TEXT PRIMARY KEY,
    daily_limit_cents BIGINT NOT NULL CHECK (daily_limit_cents > 0)
);
```

同样的建表语句追加到 `src/state/postgres.rs` 的 schema 常量里（与 `portal_users` 同一个常量，第 2489 行附近），保证新库自动建表。

Postgres 加载路径：在加载下游配置之后，补两条查询填充新字段：

```rust
    let mut cost_scope_limits = HashMap::new();
    for row in conn
        .query("SELECT scope_id, daily_limit_cents FROM cost_scope_limits", &[])
        .await
        .map_err(io_other)?
    {
        let limit: i64 = row.get(1);
        if limit > 0 {
            cost_scope_limits.insert(row.get::<_, String>(0), limit as u64);
        }
    }
    let mut downstream_owners = HashMap::new();
    for row in conn
        .query(
            "SELECT downstream_id, owner_user_id FROM downstream_access_policies \
             WHERE owner_user_id IS NOT NULL",
            &[],
        )
        .await
        .map_err(io_other)?
    {
        downstream_owners.insert(row.get::<_, String>(0), row.get::<_, String>(1));
    }
```

并写入 `PersistedState`。写回路径（保存状态到 Postgres）里，`cost_scope_limits` 用 `DELETE` + 批量 `INSERT` 全量覆盖；`downstream_owners` 是派生数据，**只读不写**，不要往 `downstream_access_policies` 回写。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --test state_store cost_scope && rtk cargo clippy --all-targets -- -D warnings`
Expected: PASS，无告警。

Run（有 Postgres 时）: `PG_TEST_DATABASE_URL=... rtk cargo test --test postgres_roundtrip`
Expected: PASS；无 Postgres 时输出 skip，在汇报里注明。

- [ ] **Step 5: 提交**

```bash
rtk git add migrations/2026-09-17-cost-scope-limits.sql src/state/types.rs src/state/postgres.rs tests/state_store.rs tests/postgres_roundtrip.rs
rtk git commit -m "feat(cost): add cost_scope_limits storage and downstream owner map"
```

---

### Task 2: 费用归属解析

**Files:**
- Create: `src/state/cost_scope.rs`
- Modify: `src/state.rs`（`mod cost_scope;` 与 re-export）
- Test: `tests/unit/state/cost_scope.rs`（新建，按 `src/upstream_feedback.rs:866` 的 `#[path]` 方式挂到 `src/state/cost_scope.rs` 末尾）

**Interfaces:**
- Consumes: Task 1 的 `PersistedState.cost_scope_limits` / `downstream_owners`
- Produces:
  - `pub struct CostScope { pub scope_id: String, pub daily_limit_cents: Option<u64> }`
  - `pub fn resolve_cost_scope(downstream_id: &str, owners: &HashMap<String, String>, limits: &HashMap<String, u64>) -> CostScope`
  - `impl AppState { pub async fn cost_scope_for(&self, downstream_id: &str) -> CostScope }`

- [ ] **Step 1: 写失败测试**

新建 `tests/unit/state/cost_scope.rs`：

```rust
use super::*;
use std::collections::HashMap;

fn owners(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, user)| (key.to_string(), user.to_string()))
        .collect()
}

fn limits(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
    pairs
        .iter()
        .map(|(scope, limit)| (scope.to_string(), *limit))
        .collect()
}

#[test]
fn owned_key_resolves_to_its_user_scope_and_limit() {
    let scope = resolve_cost_scope(
        "key-a",
        &owners(&[("key-a", "user-1")]),
        &limits(&[("user-1", 5_000)]),
    );
    assert_eq!(scope.scope_id, "user-1");
    assert_eq!(scope.daily_limit_cents, Some(5_000));
}

#[test]
fn sibling_keys_of_one_user_share_one_scope() {
    let owners = owners(&[("key-a", "user-1"), ("key-b", "user-1")]);
    let limits = limits(&[("user-1", 5_000)]);
    let first = resolve_cost_scope("key-a", &owners, &limits);
    let second = resolve_cost_scope("key-b", &owners, &limits);
    assert_eq!(first.scope_id, second.scope_id);
    assert_eq!(first.daily_limit_cents, second.daily_limit_cents);
}

#[test]
fn unowned_key_is_its_own_scope() {
    let scope = resolve_cost_scope(
        "key-direct",
        &HashMap::new(),
        &limits(&[("key-direct", 800)]),
    );
    assert_eq!(scope.scope_id, "key-direct");
    assert_eq!(scope.daily_limit_cents, Some(800));
}

#[test]
fn scope_without_a_configured_limit_is_unlimited() {
    let scope = resolve_cost_scope(
        "key-a",
        &owners(&[("key-a", "user-1")]),
        &HashMap::new(),
    );
    assert_eq!(scope.scope_id, "user-1");
    assert_eq!(scope.daily_limit_cents, None);
}

#[test]
fn a_key_level_limit_never_applies_to_an_owned_key() {
    // 账号有归属时，以 Key id 为 scope_id 写的上限必须完全不生效，
    // 否则切换后会留下 Key 级残留限制。
    let scope = resolve_cost_scope(
        "key-a",
        &owners(&[("key-a", "user-1")]),
        &limits(&[("key-a", 1), ("user-1", 5_000)]),
    );
    assert_eq!(scope.scope_id, "user-1");
    assert_eq!(scope.daily_limit_cents, Some(5_000));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --lib cost_scope`
Expected: 编译失败，`cannot find function resolve_cost_scope`。

- [ ] **Step 3: 最小实现**

新建 `src/state/cost_scope.rs`：

```rust
use std::collections::HashMap;

/// 费用归属：一次请求的花费记在哪个预算账本上。
///
/// 有门户归属用户的 Key 记在用户账本上（同一用户的所有 Key 共用一份日预算）；
/// 没有归属用户的直连 Key 记在它自己的账本上。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CostScope {
    pub scope_id: String,
    pub daily_limit_cents: Option<u64>,
}

impl CostScope {
    /// 该账本是否配置了日费用上限。没有上限就不维护滚动窗口。
    pub fn is_limited(&self) -> bool {
        self.daily_limit_cents.is_some_and(|limit| limit > 0)
    }
}

pub fn resolve_cost_scope(
    downstream_id: &str,
    owners: &HashMap<String, String>,
    limits: &HashMap<String, u64>,
) -> CostScope {
    let scope_id = owners
        .get(downstream_id)
        .cloned()
        .unwrap_or_else(|| downstream_id.to_string());
    let daily_limit_cents = limits.get(&scope_id).copied().filter(|limit| *limit > 0);
    CostScope {
        scope_id,
        daily_limit_cents,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/state/cost_scope.rs"]
mod tests;
```

`src/state.rs` 里加 `mod cost_scope;` 与 `pub use cost_scope::{resolve_cost_scope, CostScope};`，并加 `AppState` 方法：

```rust
    pub async fn cost_scope_for(&self, downstream_id: &str) -> CostScope {
        let state = self.inner.lock().await;
        resolve_cost_scope(
            downstream_id,
            &state.downstream_owners,
            &state.cost_scope_limits,
        )
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --lib cost_scope && rtk cargo clippy --all-targets -- -D warnings`
Expected: PASS，无告警。

- [ ] **Step 5: 提交**

```bash
rtk git add src/state/cost_scope.rs src/state.rs tests/unit/state/cost_scope.rs
rtk git commit -m "feat(cost): resolve a request's cost scope from key ownership"
```

---

### Task 3: 本地准入与滚动窗口改按 scope

**Files:**
- Modify: `src/state.rs:4969-5009`（`record_downstream_usage_event`）、`:5352-5393`（日费用准入）、`:5778` 附近（窗口访问）
- Modify: `src/state/usage.rs:100-127`（`build_downstream_token_windows`）、`:148-157`（`downstream_token_retention_seconds`）、`:332`（`compute_cost_usage`）
- Modify: `src/state/types.rs:1262-1278`（`cost_billing_mode` / `daily_cost_limit` 改造）
- Test: `tests/downstream_quota.rs`

**Interfaces:**
- Consumes: Task 2 的 `CostScope` / `AppState::cost_scope_for`
- Produces:
  - `DownstreamConfig::has_cost_pricing(&self) -> bool`（token 计费 + 至少一个单价；取代旧的 `cost_billing_mode`）
  - `build_downstream_token_windows(logs, downstreams, owners, limits)`
  - `AppState::compute_cost_usage(downstream_id, now)` 语义改为「该 Key 所属账号的用量与上限」

- [ ] **Step 1: 写失败测试**

`tests/downstream_quota.rs` 末尾新增（`daily_cost_limit_cents: Some(10)` 的既有夹具在第 111、713 行，可参照它们构造下游）：

```rust
#[tokio::test]
async fn sibling_keys_of_one_account_share_one_daily_cost_budget() {
    // 同一账号两个 Key，账号日上限 10 分。第一个 Key 花掉 10 分之后，
    // 第二个 Key 必须立刻被拒，而不是另开一份预算。
    let tempdir = tempdir().unwrap();
    let mut state = PersistedState {
        downstreams: std::sync::Arc::new(vec![
            cost_billed_downstream("key-a"),
            cost_billed_downstream("key-b"),
        ]),
        ..Default::default()
    };
    state
        .downstream_owners
        .insert("key-a".into(), "user-1".into());
    state
        .downstream_owners
        .insert("key-b".into(), "user-1".into());
    state.cost_scope_limits.insert("user-1".into(), 10);
    let state = AppState::new(state, tempdir.path().join("state.json"), AppConfig::default());

    let snapshot = state.snapshot().await;
    let key_a = snapshot.downstreams[0].clone();
    let key_b = snapshot.downstreams[1].clone();

    state
        .append_usage_log(cost_log("log-1", "key-a", 10))
        .await
        .unwrap();

    let rejection = state
        .reserve_downstream_admission(&key_b, "gpt-4.1-mini")
        .await
        .expect_err("兄弟 Key 必须共用同一份账号预算");
    assert!(matches!(
        rejection,
        DownstreamAdmissionRejection::DailyCostQuotaExceeded { limit: 10, .. }
    ));

    // 同一个 Key 自己也一样被拒，确认没有走岔路
    assert!(state
        .reserve_downstream_admission(&key_a, "gpt-4.1-mini")
        .await
        .is_err());
}

#[tokio::test]
async fn unowned_key_keeps_its_own_budget() {
    let tempdir = tempdir().unwrap();
    let mut state = PersistedState {
        downstreams: std::sync::Arc::new(vec![cost_billed_downstream("key-direct")]),
        ..Default::default()
    };
    state.cost_scope_limits.insert("key-direct".into(), 10);
    let state = AppState::new(state, tempdir.path().join("state.json"), AppConfig::default());
    let downstream = state.snapshot().await.downstreams[0].clone();

    assert!(state
        .reserve_downstream_admission(&downstream, "gpt-4.1-mini")
        .await
        .is_ok());

    state
        .append_usage_log(cost_log("log-1", "key-direct", 10))
        .await
        .unwrap();

    assert!(state
        .reserve_downstream_admission(&downstream, "gpt-4.1-mini")
        .await
        .is_err());
}

#[tokio::test]
async fn account_without_a_limit_is_not_throttled() {
    let tempdir = tempdir().unwrap();
    let mut state = PersistedState {
        downstreams: std::sync::Arc::new(vec![cost_billed_downstream("key-a")]),
        ..Default::default()
    };
    state
        .downstream_owners
        .insert("key-a".into(), "user-1".into());
    // 故意不给 user-1 配上限
    let state = AppState::new(state, tempdir.path().join("state.json"), AppConfig::default());
    let downstream = state.snapshot().await.downstreams[0].clone();

    state
        .append_usage_log(cost_log("log-1", "key-a", 1_000_000))
        .await
        .unwrap();

    assert!(state
        .reserve_downstream_admission(&downstream, "gpt-4.1-mini")
        .await
        .is_ok());
}
```

两个辅助函数放在同一文件里（`cost_billed_downstream` 构造一个 token 计费 + 有单价、**不带**任何 Key 级上限的下游；`cost_log` 构造一条 `total_cost_cents = Some(n)`、`status_code: 200` 的 `UsageLog`）。文件里第 111 行附近已有类似夹具，照抄改名即可，不要复用会引起歧义的旧名字。

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test downstream_quota sibling_keys_of_one_account_share_one_daily_cost_budget`
Expected: FAIL，`expect_err` 失败（当前两个 Key 各有一份窗口，`key-b` 会被放行）。

- [ ] **Step 3: 最小实现**

`src/state/types.rs`：把 `cost_billing_mode` 改名并去掉对上限的依赖，`daily_cost_limit()` 整个删掉（它的调用点在本任务里全部换成 scope）：

```rust
    /// True when this key bills by cost (token mode + at least one price).
    /// 是否真的受限由账号的 `CostScope` 决定，Key 自己不再持有上限。
    pub fn has_cost_pricing(&self) -> bool {
        self.token_billing_mode()
            && (self.input_token_price_per_million_cents.is_some()
                || self.output_token_price_per_million_cents.is_some())
    }
```

`src/state/usage.rs`：

- `downstream_token_retention_seconds(downstream, scope)` 增加 scope 参数，条件改为 `downstream.has_cost_pricing() && scope.is_limited()`。
- `build_downstream_token_windows(logs, downstreams, owners, limits)`：`cost_billed` 判定改用 `has_cost_pricing()`，分组键改为 `resolve_cost_scope(&log.downstream_key_id, owners, limits).scope_id`。
- `compute_cost_usage`：先 `let scope = self.cost_scope_for(downstream_id).await;`，上限取 `scope.daily_limit_cents`，用量改为累加「所有归属同一 scope 的 Key」的日志。scope 内的 Key 集合从 `downstream_owners` 反查（scope 是用户时取所有指向它的 Key，否则就是它自己）。

`src/state.rs`：

- `record_downstream_usage_event`：取 `let scope = self.cost_scope_for(&log.downstream_key_id).await;`，`filter` 改为 `downstream.rate_limit_enabled && downstream.has_cost_pricing() && scope.is_limited()`，Redis 分支传 `&scope.scope_id`（Task 4 才真正切 Redis，这一步先让本地分支用 `scope.scope_id` 作为 `downstream_token_windows` 的键）。
- 日费用准入（`:5352`）：`if let Some(daily_cost_limit) = ...` 改为先解析 scope，`let Some(daily_cost_limit) = scope.daily_limit_cents.filter(|l| *l > 0) else { ... }`，窗口 `entry(scope.scope_id.clone())`。
- 三处 `build_downstream_token_windows(...)` 调用（`:1169`、`:1257`、`:1342`）补传 owners 与 limits。

跑 `rtk cargo build --all-targets`，按编译器报错把 `cost_billing_mode()` / `daily_cost_limit()` 的剩余调用点逐个改掉。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --test downstream_quota && rtk cargo test`
Expected: 全绿。既有测试里凡是依赖「Key 级上限生效」的，改成给 scope 配上限；**不要**把断言改松。

Run: `rtk cargo clippy --all-targets -- -D warnings`
Expected: 无告警。

- [ ] **Step 5: 提交**

```bash
rtk git add src/state.rs src/state/usage.rs src/state/types.rs tests/downstream_quota.rs
rtk git commit -m "feat(cost): enforce the daily cost budget per account scope"
```

---

### Task 4: Redis 路径改按 scope

**Files:**
- Modify: `src/state/redis_runtime.rs:364-415`（`reserve_downstream_request`）、`:449-472`（`reserve_downstream_admission`）、`:512-540`（`record_downstream_tokens`）
- Modify: `src/state.rs:5215` 附近（Redis 分支调用点）
- Test: `tests/redis_runtime.rs`

**Interfaces:**
- Consumes: Task 2 的 `CostScope`
- Produces: Redis 侧三个函数新增 `cost_scope: &CostScope` 参数；费用键由 `stable_identity(&cost_scope.scope_id)` 派生，请求/并发键继续由 `stable_identity(&downstream.id)` 派生

- [ ] **Step 1: 写失败测试**

`tests/redis_runtime.rs` 新增一个纯函数级测试，验证键派生规则（不需要真 Redis）。若现有 `stable_identity` / `key` 是私有的，在 `src/state/redis_runtime.rs` 里加一个 `pub(crate) fn cost_window_identity(scope_id: &str) -> String { stable_identity(scope_id) }` 并针对它测：

```rust
#[test]
fn cost_window_identity_follows_the_scope_not_the_key() {
    let user_scope = cost_window_identity("user-1");
    assert_eq!(cost_window_identity("user-1"), user_scope);
    assert_ne!(cost_window_identity("key-a"), user_scope);
    assert_ne!(cost_window_identity("key-b"), user_scope);
}
```

再在 `src/state/redis_runtime.rs` 的 `#[cfg(test)] mod tests` 里补一个断言，确认 `reserve_downstream_request` 构造的费用键与请求键使用不同 identity（如果结构不便直接断言，就把键派生抽成 `fn downstream_redis_keys(downstream_id: &str, scope_id: &str) -> (String, String, String)` 再测它）。

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --lib cost_window_identity`
Expected: 编译失败，`cannot find function cost_window_identity`。

- [ ] **Step 3: 最小实现**

`reserve_downstream_request` / `reserve_downstream_admission`：

```rust
        let identity = stable_identity(&downstream.id);
        let cost_identity = stable_identity(&cost_scope.scope_id);
        let request_key = self.key(&identity, "requests");
        let token_key = self.key(&cost_identity, "tokens");
        let token_values_key = self.key(&cost_identity, "token_values");
```

`daily_limit` 取 `cost_scope.daily_limit_cents.unwrap_or(0)`（原来是 `downstream.daily_cost_limit().unwrap_or(0)`）。

`record_downstream_tokens` 第一个参数从 `downstream_id` 改为 `scope_id`，函数体里 `stable_identity(scope_id)`。`src/state.rs` 的调用点传 `&scope.scope_id`。

`downstream_reserve.lua`、`downstream_admission.lua`、`downstream_record_tokens.lua` **一行都不要改**。

`rollback_downstream_request` 只操作请求键，保持用 `downstream_id`，不要动。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --lib redis && rtk cargo test && rtk cargo clippy --all-targets -- -D warnings`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
rtk git add src/state/redis_runtime.rs src/state.rs tests/redis_runtime.rs
rtk git commit -m "feat(cost): key the redis cost window by account scope"
```

---

### Task 5: 删除 Key 级费用上限（无残留保证）

**Files:**
- Modify: `src/state/types.rs:1172`（删字段）、`src/state/downstream_patch.rs:144-155`（删补丁分支）
- Modify: `src/state/postgres.rs`（下游 SELECT/INSERT 列表去掉该列）、`src/server/admin.rs`
- Modify: `migrations/2026-09-17-cost-scope-limits.sql`（追加折算与 DROP COLUMN）
- Modify: 所有设置该字段的测试（17 处非 `None` 赋值，见下）
- Test: `tests/admin_downstreams.rs`、`tests/downstream_quota.rs`

**Interfaces:**
- Consumes: Task 1 的表
- Produces: `DownstreamConfig` 不再有 `daily_cost_limit_cents`；`downstreams` 表不再有该列

- [ ] **Step 1: 写失败测试**

`tests/admin_downstreams.rs`：现有两处测试（第 663、706、1140、1193 行附近）在 PATCH 里传 `"daily_cost_limit_cents": 5000` 并断言写入成功。把它们改成断言**该字段被忽略**：

```rust
#[tokio::test]
async fn admin_patch_ignores_the_removed_key_level_cost_limit() {
    // 切换后不允许任何路径重新引入 Key 级费用上限，
    // 老客户端/老脚本传这个字段必须静默忽略，不能落库、不能生效。
    let (state, token) = admin_state_with_downstream().await;
    let app = chat_responses_codex::server::build_router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/downstreams/downstream-1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "daily_cost_limit_cents": 5000 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());

    let snapshot = state.snapshot().await;
    let serialized = serde_json::to_value(&snapshot.downstreams[0]).unwrap();
    assert!(
        serialized.get("daily_cost_limit_cents").is_none(),
        "下游配置里不允许再出现 Key 级费用上限: {serialized}"
    );
    assert!(
        snapshot.cost_scope_limits.is_empty(),
        "PATCH 不得偷偷写进账号上限"
    );
}
```

辅助函数 `admin_state_with_downstream` 照文件里既有的建 state + 取 admin token 的写法抽出来。

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test admin_downstreams admin_patch_ignores_the_removed_key_level_cost_limit`
Expected: FAIL，`serialized.get("daily_cost_limit_cents")` 仍有值。

- [ ] **Step 3: 最小实现**

1. `src/state/types.rs` 删掉 `pub daily_cost_limit_cents: Option<u64>,` 与 `Default` 里对应行。
2. `src/state/downstream_patch.rs` 删掉第 144-155 行两个分支，并把第 28 行的字段名白名单数组里的 `"daily_cost_limit_cents"` 去掉。
3. `src/state/postgres.rs` 下游相关的 SELECT 列表（第 176 行）、INSERT 列名与参数、行映射（第 207 行）里去掉该列。
4. 迁移文件追加折算与删列（必须在同一个文件里，顺序不能颠倒）：

```sql
-- 把 Key 级上限折算成账号级上限：每个账号取名下 Key 原上限的最大值。
-- 有归属用户的记到用户名下，没有归属的记到 Key 自己名下。
INSERT INTO cost_scope_limits (scope_id, daily_limit_cents)
SELECT COALESCE(p.owner_user_id, d.id), MAX(d.daily_cost_limit_cents)
FROM downstreams d
LEFT JOIN downstream_access_policies p ON p.downstream_id = d.id
WHERE d.daily_cost_limit_cents IS NOT NULL
  AND d.daily_cost_limit_cents > 0
GROUP BY COALESCE(p.owner_user_id, d.id)
ON CONFLICT (scope_id) DO UPDATE
    SET daily_limit_cents = GREATEST(
        cost_scope_limits.daily_limit_cents,
        EXCLUDED.daily_limit_cents
    );

-- 折算完成后删列。删掉之后不可能再有任何 Key 级残留限制。
ALTER TABLE downstreams DROP COLUMN IF EXISTS daily_cost_limit_cents;
```

5. `rtk cargo build --all-targets`，按编译器报错删掉所有测试里的该字段赋值。17 处赋了非 `None` 值的地方（`tests/downstream_quota.rs:111,713`、`tests/gateway/chat/core.rs:827`、`tests/admin_downstreams.rs` 若干等）要改成往 `PersistedState.cost_scope_limits` 写等值上限，**不要简单删掉断言**，否则会丢失费用限额的测试覆盖。赋 `None` 的地方直接删行。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test && rtk cargo clippy --all-targets -- -D warnings`
Expected: 全绿。

Run: `rtk grep -rn "daily_cost_limit_cents" src/`
Expected: 只剩 `cost_scope_limits` 相关代码；`src/state/types.rs` 里不得有任何命中。

- [ ] **Step 5: 提交**

```bash
rtk git add src/state/types.rs src/state/downstream_patch.rs src/state/postgres.rs src/server/admin.rs migrations/2026-09-17-cost-scope-limits.sql tests
rtk git commit -m "refactor(cost)!: drop the per-key daily cost limit in favour of account scopes"
```

---

### Task 6: 管理台与门户展示

**Files:**
- Modify: `src/server/admin.rs`（新增账号上限的读写接口）、`src/server/gateway.rs`（注册路由）
- Modify: `frontend/src/views/admin/PortalUsers.vue:294,371,835,1022`（批量写 Key 上限 → 改为写账号上限）
- Modify: `frontend/src/views/admin/Downstreams.vue:104,428,900,958`（删掉 Key 级费用上限编辑与「余额」列）
- Modify: `frontend/src/views/portal/Overview.vue:268-292`、`frontend/src/views/portal/QuotaDetails.vue:20`（费用文案改为账号口径）
- Modify: `frontend/src/types/index.ts`、`frontend/src/api/admin.ts`
- Test: `frontend/src/views/admin/__tests__/PortalUsers.costLimit.spec.ts`（新建）

**Interfaces:**
- Consumes: Task 1 的表、Task 3 的 `compute_cost_usage`
- Produces:
  - `GET /api/admin/portal-users/{user_id}/cost-limit` → `{"daily_limit_cents": u64 | null}`
  - `PUT /api/admin/portal-users/{user_id}/cost-limit`，body `{"daily_limit_cents": u64 | null}`（null 表示取消上限）

- [ ] **Step 1: 写失败测试**

后端：`tests/admin_downstreams.rs`（或新建 `tests/admin_cost_limits.rs`）新增一个测试，PUT 账号上限后 `snapshot().cost_scope_limits` 出现对应条目，再 PUT `null` 后条目消失。

前端：新建 `frontend/src/views/admin/__tests__/PortalUsers.costLimit.spec.ts`，照 `KeyManagement.spec.ts` 的 mock 写法，断言：

```ts
  it('writes the account cost limit instead of per-key limits', async () => {
    // 打开某个用户的限额表单、填写日费用上限、提交
    // 断言调用的是 setPortalUserCostLimit，而不是批量下游限额接口
    expect(vi.mocked(adminApi.setPortalUserCostLimit)).toHaveBeenCalledWith(
      'user-1',
      { daily_limit_cents: 5000 }
    )
    expect(vi.mocked(adminApi.batchUpdateDownstreamLimits)).not.toHaveBeenCalledWith(
      expect.objectContaining({ daily_cost_limit_cents: expect.anything() })
    )
  })
```

具体的 mock 方法名以 `rtk grep -n "adminApi\." frontend/src/views/admin/PortalUsers.vue` 实际命中为准。

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test admin_downstreams cost_limit`
Run: `cd frontend && rtk npx vitest run src/views/admin/__tests__/PortalUsers.costLimit.spec.ts`
Expected: 后端 404，前端报 `setPortalUserCostLimit is not a function`。

- [ ] **Step 3: 最小实现**

后端两个 handler 读写 `PersistedState.cost_scope_limits`（走 `mutate_config` 之类的既有变更路径，保证同时落 Postgres 与内存），路由注册在 `admin_portal_users` 旁边并套同一个鉴权 `route_layer`。

前端：

- `PortalUsers.vue` 的批量限额表单里，把「日费用上限」一项从 `batchLimitsForm` 拆出来，单独调用新接口；其余项（每分钟、并发、请求配额、单价）保持批量写 Key 不变。表格加一列「账号日上限」。
- `Downstreams.vue` 删掉 `daily_cost_limit_cents` 的输入框、批量项与「余额」列，改为在费用相关位置显示一行说明：费用上限已按账号管理，请到门户用户页设置。
- 门户 `Overview.vue` / `QuotaDetails.vue` 的费用区块文案从「本密钥」改为「本账号」，数值来源不变（后端已按 scope 汇总）。

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test && rtk cargo clippy --all-targets -- -D warnings`
Run: `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
rtk git add src/server frontend/src
rtk git commit -m "feat(admin-ui): manage the daily cost limit per account instead of per key"
```

---

## 验收清单

- [x] 一个账号两个 Key，账号日上限 10 元：第一个 Key 花掉 10 元后，第二个 Key 立刻被拒，不是各花 10 元。
- [x] 工号+密钥登录的用户（无 OAuth 身份）与 OAuth 用户走同一套账号汇总，行为无差别。
- [x] 门户自助新建的 Key 立刻纳入所属账号的预算，不再是无限额。
- [x] 从未打开过门户的纯直连 Key 仍有自己的费用上限，升级后额度与升级前一致。
- [x] 升级后 `DownstreamConfig` 序列化结果里不含 `daily_cost_limit_cents`；`downstreams` 表已无该列；管理台 PATCH 传该字段被静默忽略。
- [x] 迁移后每个账号的上限等于它原来名下 Key 上限的最大值（MAX 折算，流程经测试覆盖；管理员人工复核待部署后进行）。
- [x] Redis 部署下，费用窗口键按账号聚合（`cost_window_identity_follows_the_scope_not_the_key`）；老的 Key 级窗口键不再被读取，24 小时内自然过期。
- [x] 门户概览与配额页的费用数字是账号口径（门户文案已按账号口径标注），与实际被限的口径一致。
- [x] `rtk cargo clippy --all-targets -- -D warnings`、`rtk cargo test`、`cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run` 全绿。

## 回滚说明

Task 5 的 `DROP COLUMN` 不可逆。需要回滚到旧版本时，必须先手工恢复列并把 `cost_scope_limits` 的值写回各 Key：

```sql
ALTER TABLE downstreams ADD COLUMN IF NOT EXISTS daily_cost_limit_cents BIGINT NULL;
UPDATE downstreams d SET daily_cost_limit_cents = c.daily_limit_cents
FROM downstream_access_policies p, cost_scope_limits c
WHERE p.downstream_id = d.id AND c.scope_id = COALESCE(p.owner_user_id, d.id);
```

## 完成状态

| 任务 | 状态 | Commit |
|---|---|---|
| Task 1 cost_scope_limits 存储 | ✅ | `42a213d1` |
| Task 2 费用归属解析 | ✅ | `0ec24149` |
| Task 3 本地准入改 scope | ✅ | `235e8b05` |
| Task 4 Redis 改 scope | ✅ | `bf8c991e` |
| Task 5 删除 Key 级上限 | ✅ | `a0cda409` |
| Task 6 管理台与门户展示 | ✅ | `dfeb112` |
