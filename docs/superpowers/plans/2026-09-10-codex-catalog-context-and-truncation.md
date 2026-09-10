# Codex 目录：上下文窗口单一来源 + 截断策略改 tokens

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让门户生成的 `model-catalog.json` 里的 `context_window` 与管理台配置（含全局上下文配置）一致，并把 `truncation_policy.mode` 从 `bytes` 改为 `tokens`。

**Architecture:** 在 `ModelCatalog` 上新增一个"对外模型有效上下文"解析函数，遍历该模型全部活跃路由，按与请求路径相同的四级顺序（上游逐模型 → 全局 profile 逐模型 → 上游默认 → 全局 profile 默认）解析每条路由的上下文，取最小值。门户配额页（`/api/portal/quota`）和 Codex 目录（`/v1/models?format=codex`）都改为调用这一个函数，删除 Codex 目录里"只看见证上游、且不读全局 profile"的旧路径。

**Tech Stack:** Rust/Axum 网关，Cargo 集成测试（`tests/gateway/capability_routing.rs`、`tests/gateway/compatibility.rs`、`tests/portal_api.rs`），Vue 门户文案，中文文档。

**Spec:** 本文档第 1、2 节即为规格；无独立 spec 文件。

## Global Constraints

- 所有命令加 `rtk` 前缀（见仓库 `CLAUDE.md`）。
- 严格 TDD：每个任务先写测试、亲眼看到失败、再写最小实现、再跑绿、再提交。
- `effective_context_window_percent` 保持 `80`，**不要改**。
- `max_context_window` 继续等于 `context_window`（下游客户端只能调低不能调高，网关本身会裁剪）。
- 不改管理台默认值 `default_model_context.context_limit = 200_000`（`src/server/admin.rs:1655`、`frontend/src/views/admin/Upstreams.vue:759`）。
- 不改 `src/server/gateway/capability_routing.rs:745-765` 的能力解析覆盖逻辑（管理台能力诊断仍显示它）。

---

## 1. 现状与根因

### 1.1 `truncation_policy`

`src/server/gateway.rs:3088-3091` 写死 `{"mode": "bytes", "limit": 10_000}`。Codex 源码（`codex-rs/protocol/src/openai_models.rs:360`，`TruncationPolicyConfig { mode: Bytes | Tokens, limit: i64 }`）两种模式都合法，但含义不同：这是**工具输出截断**上限，`bytes` 模式按 4 字节 ≈ 1 token 换算，10000 字节只有约 2500 token；`tokens` 模式 10000 就是 10000 token。用户决定：改为 `tokens`，limit 保持 `10_000`。

### 1.2 `context_window` 为什么"配置不生效"

`src/server/gateway.rs:3042-3055` 的取值顺序：

1. 优先：见证路由（`select_catalog_witness_entry`，`src/server/gateway/capability_routing.rs:867`）的 `capabilities.context_window`。该值在 `capability_routing.rs:745` 由 `upstream.context_config_for_model(exposed_model_slug)` 组成——这个调用**不传全局 profile**，所以按 base_url 配置的全局上下文被忽略，落到该上游的 `default_model_context`（管理台默认 200000）。
2. 兜底：仅当没有任何见证路由时才走 `codex_catalog_context_window`（`gateway.rs:2971`），这条才读全局 profile，且取的是所有路由的**最大值**。

两个后果：

- 全局上下文配置（内网部署里所有上游共用同一 base_url，正是最自然的配置位置）在 Codex 目录中完全不生效。
- 同一模型多上游时，只有被选中的见证上游的配置有效，配在别的上游上不显示。测试 `codex_catalog_context_limits_come_only_from_the_selected_witness`（`tests/gateway/capability_routing.rs:1357`）锁定了这个行为。

而门户配额页 `compute_portal_model_context_limits`（`src/state/usage.rs:582`）是另一套实现：读全局 profile，取所有活跃路由的**最小值**。两页显示不一致。

## 2. 设计决策

| 项 | 决定 | 理由 |
|---|---|---|
| `truncation_policy.mode` | `tokens`，limit `10_000` | 用户指定 |
| `effective_context_window_percent` | 保持 80 | 用户指定 |
| `context_window` 来源 | 所有活跃路由的上下文配置取**最小值**，解析顺序与请求路径一致，读全局 profile | 与配额页一致；请求可能落到任一路由，最小窗口最安全，网关裁剪逻辑也按这个值工作 |
| 见证上游的 `capabilities.context_window` | Codex 目录**不再使用** | 它不读全局 profile，无法修复；能力策略里的 `semantic.context_window` 目前除目录外没有其他运行时消费者，仅剩管理台能力诊断展示（保留） |
| 共享实现 | `ModelCatalog::effective_context_for_model` | 配额页与 Codex 目录单一来源 |

> ⚠️ 行为变更提醒：能力策略文档（如 `templates/capabilities/current-deployment.example.json` 里的 `semantic.context_window`）以后不再对 Codex 目录的 `context_window` 起封顶作用。上下文上限只认管理台"模型上下文"与"全局上下文配置"。

---

### Task 1: `truncation_policy` 改为 tokens

**Files:**
- Modify: `src/server/gateway.rs:3088-3091`
- Test: `tests/gateway/compatibility.rs:419`

**Interfaces:**
- Consumes: 无
- Produces: Codex 目录 JSON 中 `truncation_policy == {"mode": "tokens", "limit": 10000}`

- [ ] **Step 1: 改测试断言**

在 `tests/gateway/compatibility.rs` 测试 `v1_models_endpoint_returns_codex_model_catalog_for_client_version` 中，把

```rust
    assert_eq!(model["truncation_policy"]["mode"], "bytes");
```

改为

```rust
    assert_eq!(model["truncation_policy"]["mode"], "tokens");
```

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test gateway v1_models_endpoint_returns_codex_model_catalog_for_client_version`
Expected: FAIL，断言 `left: "bytes"`, `right: "tokens"`。

- [ ] **Step 3: 最小实现**

`src/server/gateway.rs` 中把

```rust
                "truncation_policy": {
                    "mode": "bytes",
                    "limit": 10_000
                },
```

改为

```rust
                "truncation_policy": {
                    "mode": "tokens",
                    "limit": 10_000
                },
```

- [ ] **Step 4: 跑测试确认通过**

Run: `rtk cargo test --test gateway v1_models_endpoint_returns_codex_model_catalog_for_client_version`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
rtk git add src/server/gateway.rs tests/gateway/compatibility.rs
rtk git commit -m "fix(codex-catalog): advertise tool-output truncation in tokens instead of bytes"
```

---

### Task 2: `ModelCatalog::effective_context_for_model`，配额页改用它

**Files:**
- Modify: `src/state/model_catalog.rs`（在 `route_context` 之后新增方法）
- Modify: `src/state/usage.rs:602-647`（`compute_portal_model_context_limits` 内层循环）
- Test: `tests/model_access_policy.rs`（新增测试，沿用该文件已有的 `set_contexts` 风格）

**Interfaces:**
- Consumes: `ModelCatalog::find(&self, name) -> Option<&PublishedModel>`、`ModelCatalog::route_context(&self, snapshot: &PersistedState, route: &PublishedRoute) -> Option<ModelContextConfig>`（均已存在于 `src/state/model_catalog.rs`）
- Produces:

```rust
impl ModelCatalog {
    pub fn effective_context_for_model(
        &self,
        snapshot: &super::PersistedState,
        name: &str,
    ) -> Option<super::ModelContextConfig>;
}
```

  返回该对外模型所有活跃路由中 `context_limit` 最小的那条配置（`slug` 改写为对外名），全部路由无配置或全为 0 时返回 `None`。

- [ ] **Step 1: 写失败测试**

在 `tests/model_access_policy.rs` 末尾追加。该文件已有 `catalog_state(&[allowlist]) -> (TempDir, AppState, key)` 构造器（上游 id `provider`，base_url `http://127.0.0.1:9`，`deepseek-chat` 通过别名发布为 `deepseek-v3`）以及 `set_contexts` / `compute_portal_model_context_limits` 的调用示例（`tests/model_access_policy.rs:82-101`）。在文件顶部 `use chat_responses_codex::state::{...}` 里补上 `DefaultModelContextConfig, GlobalContextProfile, ModelContextConfig`。

```rust
#[tokio::test]
async fn portal_context_limits_prefer_global_profile_over_upstream_default() {
    let (_dir, state, key) = catalog_state(&["deepseek-v3"]);
    let mut upstream = state.upstreams().await.remove(0);
    upstream.model_contexts = vec![];
    upstream.default_model_context = Some(DefaultModelContextConfig {
        context_limit: 200_000,
        output_reserve: 4_096,
        max_output_tokens: 0,
        context_group: String::new(),
    });
    state.update_upstream("provider", upstream).await.unwrap();

    let mut profiles = std::collections::HashMap::new();
    profiles.insert(
        "http://127.0.0.1:9".to_string(),
        GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: "deepseek-chat".into(),
                context_limit: 1_000_000,
                output_reserve: 8_192,
                max_output_tokens: 0,
                context_group: String::new(),
            }],
            default_model_context: None,
        },
    );
    state.set_global_context_profiles(profiles).await.unwrap();

    let downstream = state.downstream_for_secret(&key).await.unwrap();
    let contexts = state.compute_portal_model_context_limits(&downstream).await;
    assert_eq!(
        contexts.get("deepseek-v3").map(|c| c.context_limit),
        Some(1_000_000),
        "global profile per-model entry must beat upstream default_model_context"
    );
}
```

  说明：`set_global_context_profiles` 是管理台 PUT `/api/admin/global-context-profiles` 背后的 `AppState` 方法（`src/server/admin.rs:915` 处调用），`downstream_for_secret` 在 `src/server/gateway.rs:2992` 处有调用示例。若其返回类型与上面写法不符（例如不是 `Result`），按编译器提示调整 `.unwrap()`。

- [ ] **Step 2: 跑测试确认失败**

Run: `rtk cargo test --test model_access_policy portal_context_limits_prefer_global_profile_over_upstream_default`
Expected: FAIL。若失败原因是编译错误（方法名不存在），先按上一步说明修正测试脚手架，直到失败原因是断言 `Some(200000) != Some(1000000)`……注意：现有 `compute_portal_model_context_limits` 已经读 profile，所以这个测试**可能直接通过**。若直接通过，说明配额页路径本身没问题，本任务只做重构：保留该测试作为回归保护，继续 Step 3。

- [ ] **Step 3: 新增共享方法**

`src/state/model_catalog.rs`，紧跟 `route_context` 方法之后加入：

```rust
    /// 对外模型的有效上下文配置：遍历全部活跃路由，取 `context_limit` 最小者
    /// （与门户配额页语义一致——请求可能落到任一路由，最小窗口最安全）。
    /// 每条路由按请求路径同样的顺序解析：上游 model_contexts → 全局 profile
    /// model_contexts → 上游 default_model_context → 全局 profile default。
    /// 跳过 `context_limit == 0` 的路由；全部无配置时返回 `None`。
    pub fn effective_context_for_model(
        &self,
        snapshot: &super::PersistedState,
        name: &str,
    ) -> Option<super::ModelContextConfig> {
        let published = self.find(name)?;
        published
            .routes
            .iter()
            .filter_map(|route| self.route_context(snapshot, route))
            .filter(|config| config.context_limit > 0)
            .min_by_key(|config| config.context_limit)
            .map(|config| super::ModelContextConfig {
                slug: published.name.clone(),
                ..config
            })
    }
```

- [ ] **Step 4: 配额页改用共享方法**

`src/state/usage.rs` 中 `compute_portal_model_context_limits` 从 `let mut result: HashMap<String, ModelContextConfig> = HashMap::new();` 到函数末尾 `result` 之前的整个 `for published in catalog.models()` 循环替换为：

```rust
        let mut result: HashMap<String, ModelContextConfig> = HashMap::new();

        for published in catalog.models() {
            if !catalog.allows_legacy(&effective_allowlist, &published.name) {
                continue;
            }
            if let Some(cfg) = catalog.effective_context_for_model(&snapshot, &published.name) {
                result.insert(published.name.clone(), cfg);
            }
        }

        result
```

  删除因此不再使用的 `normalize_context_profile_base_url` 导入（若编译器报 unused）。

- [ ] **Step 5: 跑相关测试确认全绿**

Run:
```bash
rtk cargo test --test model_access_policy
rtk cargo test --test portal_api model_contexts
rtk cargo test --test portal_api portal_quota
```
Expected: 全部 PASS。特别是 `tests/portal_api.rs:1700-1728` 的断言（GLM-5 取 128000 最小值、MiniMax 回落 200000 默认值、inactive 上游忽略）必须保持通过。

- [ ] **Step 6: 提交**

```bash
rtk git add src/state/model_catalog.rs src/state/usage.rs tests/model_access_policy.rs
rtk git commit -m "refactor(context): single ModelCatalog::effective_context_for_model for portal limits"
```

---

### Task 3: Codex 目录改用共享方法，删除见证路径的上下文取值

**Files:**
- Modify: `src/server/gateway.rs:2971-2984`（`codex_catalog_context_window`）
- Modify: `src/server/gateway.rs:3042-3055`（`context_window` 取值）
- Test: `tests/gateway/capability_routing.rs:1357-1406`（改名改断言）
- Test: `tests/gateway/capability_routing.rs:140-183`（`catalog_state` 增加带 profile 的变体）
- Test: `tests/gateway/capability_routing.rs`（新增全局 profile 测试）

**Interfaces:**
- Consumes: `ModelCatalog::effective_context_for_model(&self, &PersistedState, &str) -> Option<ModelContextConfig>`（Task 2）
- Produces: Codex 目录 `context_window`/`max_context_window` == 上述函数的 `context_limit`（转 `i64`），无配置时为 `null`

- [ ] **Step 1: 改现有见证测试为"取所有路由最小值"**

`tests/gateway/capability_routing.rs` 中把测试

```rust
async fn codex_catalog_context_limits_come_only_from_the_selected_witness() {
```

改名为

```rust
async fn codex_catalog_context_window_is_min_across_all_active_routes() {
```

并把结尾两行断言改为：

```rust
    assert_eq!(model["context_window"], 111_111);
    assert_eq!(model["max_context_window"], 111_111);
```

（该测试里 `a-unrelated-context` 配了 111_111，`z-selected-context` 配了 222_222；新语义下取最小 111_111，不再看谁是见证。）

- [ ] **Step 2: 加 `catalog_state_with_profiles` 辅助函数并写全局 profile 测试**

把 `catalog_state` 改成薄封装：

```rust
fn catalog_state(
    upstreams: Vec<UpstreamConfig>,
    model_allowlist: Vec<String>,
) -> (tempfile::TempDir, AppState, String) {
    catalog_state_with_profiles(
        upstreams,
        model_allowlist,
        std::collections::HashMap::new(),
    )
}

fn catalog_state_with_profiles(
    upstreams: Vec<UpstreamConfig>,
    model_allowlist: Vec<String>,
    global_context_profiles: std::collections::HashMap<String, GlobalContextProfile>,
) -> (tempfile::TempDir, AppState, String) {
    // 原 catalog_state 的函数体原样搬进来，只把
    //   global_context_profiles: std::sync::Arc::new(std::collections::HashMap::new()),
    // 换成
    //   global_context_profiles: std::sync::Arc::new(global_context_profiles),
}
```

然后紧跟改名后的测试之后新增：

```rust
#[tokio::test]
async fn codex_catalog_context_window_prefers_global_profile_over_upstream_default() {
    let model = "arbitrary/profile-context";
    let mut upstream = catalog_upstream("profile-host", &[model]);
    upstream.default_model_context = Some(DefaultModelContextConfig {
        context_limit: 200_000,
        output_reserve: 4_096,
        max_output_tokens: 0,
        context_group: String::new(),
    });
    let mut profiles = std::collections::HashMap::new();
    profiles.insert(
        "https://profile-host.invalid".to_string(),
        GlobalContextProfile {
            model_contexts: vec![ModelContextConfig {
                slug: model.into(),
                context_limit: 1_000_000,
                output_reserve: 8_192,
                max_output_tokens: 0,
                context_group: String::new(),
            }],
            default_model_context: None,
        },
    );
    let (_tempdir, state, secret) =
        catalog_state_with_profiles(vec![upstream.clone()], vec![model.into()], profiles);
    put_catalog_profile(
        &state,
        &upstream,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
        ],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let model = &catalog["models"][0];
    assert_eq!(model["context_window"], 1_000_000);
    assert_eq!(model["max_context_window"], 1_000_000);
}
```

  按需在文件顶部补 `use chat_responses_codex::state::{DefaultModelContextConfig, GlobalContextProfile, ModelContextConfig};`（`ModelContextConfig` 可能已由 `super::common::*` 导出，编译器报重复导入则去掉）。

- [ ] **Step 3: 跑两个测试确认失败**

Run: `rtk cargo test --test gateway codex_catalog_context_window`
Expected: 两个测试都 FAIL——第一个 `left: 222222, right: 111111`，第二个 `left: 200000, right: 1000000`。这两个失败恰好复现 1.2 节的两个根因。

- [ ] **Step 4: 实现**

`src/server/gateway.rs` 把 `codex_catalog_context_window` 整个函数替换为：

```rust
fn codex_catalog_context_window(
    snapshot: &crate::state::PersistedState,
    catalog: &crate::state::ModelCatalog,
    model: &str,
) -> Option<i64> {
    catalog
        .effective_context_for_model(snapshot, model)
        .map(|config| i64::from(config.context_limit))
}
```

并把目录构建里的

```rust
            let context_window = capabilities
                .and_then(|capabilities| {
                    capabilities
                        .context_window
                        .and_then(|limit| i64::try_from(limit).ok())
                })
                .or_else(|| {
                    codex_catalog_context_window(
                        &snapshot,
                        &catalog,
                        &slug,
                        case_insensitive,
                    )
                });
```

替换为

```rust
            // 上下文上限单一来源：所有活跃路由取最小值（与门户配额页一致），
            // 不再使用见证路由的 capabilities.context_window（它不读全局 profile）。
            let context_window = codex_catalog_context_window(&snapshot, &catalog, &slug);
```

同步更新函数上方的 doc 注释（`/// Build a Codex-compatible model catalog response` 一段）说明 `context_window` 来自上游/全局上下文配置的最小值。

- [ ] **Step 5: 跑测试确认通过**

Run:
```bash
rtk cargo test --test gateway codex_catalog
rtk cargo test --test gateway v1_models
rtk cargo test --test model_access_policy
```
Expected: 全部 PASS。`tests/gateway/compatibility.rs` 里 `priority-low` 配 272_000、`priority-high` 无配置无默认，最小值仍是 272_000；`tests/model_access_policy.rs` 的 `codex_alias_metadata_uses_the_underlying_route_context` 期望 64000，单路由不变。

- [ ] **Step 6: clippy 与全量回归**

Run:
```bash
rtk cargo clippy --all-targets -- -D warnings
rtk cargo test
```
Expected: 无 warning，全绿。若出现 `case_insensitive` unused 之类的告警，删掉对应无用参数/变量。

- [ ] **Step 7: 提交**

```bash
rtk git add src/server/gateway.rs tests/gateway/capability_routing.rs
rtk git commit -m "fix(codex-catalog): context_window from min across active routes incl. global profiles"
```

---

### Task 4: 文档与门户文案

**Files:**
- Modify: `docs/codex-integration-guide.md:472-475`
- Modify: `frontend/src/views/portal/Integration.vue:233-241`
- Modify: 本文档末尾"完成状态"

**Interfaces:** 无代码接口。

- [ ] **Step 1: 更新集成指南**

`docs/codex-integration-guide.md` 中把

```
每个目录条目使用模型自己的绝对 `context_window`，并设置
`effective_context_window_percent = 80`。Codex 会在累计 token 接近该模型窗口的 80% 时压缩
历史；不要在 `config.toml` 再设置一个全局绝对压缩上限，否则切换不同窗口的模型时会失去
按模型计算的压缩点。
```

替换为

```
每个目录条目使用模型自己的绝对 `context_window`，并设置
`effective_context_window_percent = 80`。Codex 会在累计 token 接近该模型窗口的 80% 时压缩
历史；不要在 `config.toml` 再设置一个全局绝对压缩上限，否则切换不同窗口的模型时会失去
按模型计算的压缩点。

`context_window` 的来源：该模型全部活跃路由的上下文配置取**最小值**，与门户配额页显示的
数值相同。每条路由按"上游模型上下文 → 全局上下文配置（按 base_url）模型条目 →
上游默认上下文 → 全局默认上下文"的顺序解析。同一模型走多个上游时，要么每个上游都配，
要么在全局上下文配置里配一次（同一 base_url 的上游共享）。`max_context_window` 与
`context_window` 相同，因此 `config.toml` 里的 `model_context_window` 只能调低不能调高。

`truncation_policy` 固定为 `{"mode": "tokens", "limit": 10000}`，即单次工具输出最多保留约
10000 token；`config.toml` 里的 `tool_output_token_limit` 可以覆盖这个上限。
```

- [ ] **Step 2: 更新门户步骤 2 文案**

`frontend/src/views/portal/Integration.vue` 步骤 2 的 `<p>` 改为：

```html
                  <p>
                    这个文件包含当前下游白名单中的完整模型目录，route 与能力元数据由网关生成。
                    每个模型的 <code>context_window</code> 取其全部活跃上游上下文配置的最小值
                    （与配额页一致，全局上下文配置同样生效）。
                    Codex 默认会在累计 token 达到该窗口的
                    <strong>80%</strong> 时自动压缩历史，无需在 <code>config.toml</code>
                    再设全局阈值；切换模型时压缩点会跟着模型的实际窗口变。
                  </p>
```

- [ ] **Step 3: 前端检查**

Run: `cd frontend && rtk npx vue-tsc --noEmit && rtk npx vitest run`
Expected: 无类型错误，测试全绿（文案改动不应影响任何测试）。

- [ ] **Step 4: 回填本文档完成状态并提交**

在本文档末尾"完成状态"表填入各任务 commit 号，然后：

```bash
rtk git add docs/codex-integration-guide.md frontend/src/views/portal/Integration.vue docs/superpowers/plans/2026-09-10-codex-catalog-context-and-truncation.md
rtk git commit -m "docs(codex): explain context_window source and tokens truncation policy"
```

---

## 验收清单

- [x] `GET /v1/models?format=codex` 每个条目 `truncation_policy == {"mode":"tokens","limit":10000}`。
- [x] 同一模型两个活跃上游分别配 111_111 / 222_222 时，目录 `context_window == 111_111`。
- [x] 上游只有默认 200_000、全局 profile 配该模型 1_000_000 时，目录 `context_window == 1_000_000`。
- [x] `/api/portal/quota` 的 `model_contexts[*].context_window` 与目录里同模型的 `context_window` 数值相同。
- [x] `effective_context_window_percent` 仍为 80；`max_context_window == context_window`。
- [x] `rtk cargo clippy --all-targets -- -D warnings` 与 `rtk cargo test` 全绿。

## 完成状态

| 任务 | 状态 | Commit |
|---|---|---|
| Task 1 truncation tokens | ✅ | `bef5c3f4` |
| Task 2 共享上下文解析 | ✅ | `16bfbc15` |
| Task 3 Codex 目录改源 | ✅ | `5a8e0878` |
| Task 4 文档文案 | ✅ | `（本提交）` |
