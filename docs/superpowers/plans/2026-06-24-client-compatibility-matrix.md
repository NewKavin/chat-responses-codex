# Client Compatibility Matrix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Cline as a first-class OpenAI-compatible preset, build a compatibility matrix UI in the portal Integration page, update documentation to map clients to protocol families, and lock everything with tests.

**Architecture:** Extend the existing `integration.ts` utility module with a `buildOpenAiCompatibleConfig` function for Cline/generic OpenAI-compatible clients. Add a new tab in the Integration.vue view for Cline. Add a compatibility matrix summary above the tabs. Update `integration.spec.ts` for the new generator. Update README and the integration guide with a client-to-protocol mapping section. Add backend protocol contract tests in Rust.

**Tech Stack:** Vue 3 + Element Plus frontend, Vitest for frontend tests, Rust backend tests, TypeScript utility module.

---

### Task 1: Add OpenAI-Compatible Preset Generator (Cline)

**Files:**
- Modify: `frontend/src/utils/integration.ts` (add `buildOpenAiCompatibleConfig`)
- Modify: `frontend/src/utils/integration.spec.ts` (add tests for new generator)

- [ ] **Step 1: Write the failing test for `buildOpenAiCompatibleConfig`**

Add this test in `frontend/src/utils/integration.spec.ts`, inside the existing `describe('integration config generators', ...)` block, after the Claude Code test:

```typescript
it('builds an openai-compatible config for Cline and other generic clients', () => {
  const config = JSON.parse(
    buildOpenAiCompatibleConfig({
      gatewayBaseUrl: 'https://portal.example.com',
      portalKey: 'sk-downstream-123',
      modelSlugs: ['MiniMax/MiniMax-M2.7', 'DeepSeek/DeepSeek-V3'],
      selectedModelSlug: 'MiniMax/MiniMax-M2.7'
    })
  )

  expect(config.baseURL).toBe('https://portal.example.com/v1')
  expect(config.apiKey).toBe('sk-downstream-123')
  expect(config.model).toBe('MiniMax/MiniMax-M2.7')
})
```

Also add the import for `buildOpenAiCompatibleConfig` at the top of the spec file, in the existing import block:

```typescript
import {
  buildClaudeCodeSettingsJson,
  buildCodexAuthLoginCommand,
  buildCodexConfigToml,
  buildCodexModelCatalogJson,
  buildGatewayBaseUrl,
  buildGatewayModelsEndpoint,
  buildModelUsageStats,
  buildOpenAiCompatibleConfig,
  buildOpenCodeConfig,
  extractGatewayModelSlugs,
  rankModelSlugsByUsage,
  sortPortalModelStats
} from './integration'
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd frontend && npx vitest run src/utils/integration.spec.ts`
Expected: FAIL — `buildOpenAiCompatibleConfig` is not exported from `integration.ts`

- [ ] **Step 3: Write minimal implementation of `buildOpenAiCompatibleConfig`**

Add this function at the end of `frontend/src/utils/integration.ts`, before the closing (there is no explicit closing — just append after `buildClaudeCodeSettingsJson`):

```typescript
export const buildOpenAiCompatibleConfig = (input: IntegrationConfigInput) => {
  const primaryModelSlug = choosePrimaryModelSlug(input.modelSlugs, input.selectedModelSlug)

  return `${jsonStringify({
    baseURL: `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1`,
    apiKey: input.portalKey,
    model: primaryModelSlug,
    modelsEndpoint: `${buildGatewayBaseUrl(input.gatewayBaseUrl)}/v1/models`
  })}\n`
}
```

This function uses the existing `IntegrationConfigInput` type (already defined in `integration.ts`), `choosePrimaryModelSlug`, `buildGatewayBaseUrl`, and `jsonStringify`. It produces a deterministic JSON object that any OpenAI-compatible client (Cline, generic tools) can consume: `baseURL`, `apiKey`, `model`, and `modelsEndpoint`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd frontend && npx vitest run src/utils/integration.spec.ts`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add frontend/src/utils/integration.ts frontend/src/utils/integration.spec.ts
git commit -m "feat: add OpenAI-compatible preset generator for Cline"
```

---

### Task 2: Add Cline Tab to Integration Page

**Files:**
- Modify: `frontend/src/views/portal/Integration.vue`

- [ ] **Step 1: Add the Cline computed property**

In `<script setup lang="ts">`, add the import for `buildOpenAiCompatibleConfig` in the existing import block from `@/utils/integration`:

```typescript
import {
  buildClaudeCodeSettingsJson,
  buildCodexAuthLoginCommand,
  buildCodexConfigToml,
  buildCodexModelCatalogJson,
  buildModelUsageStats,
  buildGatewayBaseUrl,
  buildGatewayModelsEndpoint,
  buildOpenAiCompatibleConfig,
  buildOpenCodeConfig,
  extractGatewayModelSlugs,
  rankModelSlugsByUsage,
  sortPortalModelStats
} from '@/utils/integration'
```

Then add a computed property alongside the existing `claudeCodeSettingsJson` computed:

```typescript
const openAiCompatibleConfig = computed(() =>
  buildOpenAiCompatibleConfig({
    gatewayBaseUrl: gatewayBaseUrl.value,
    portalKey: portalKey.value,
    modelSlugs: allModelSlugs.value,
    selectedModelSlug: primaryModelSlug.value
  })
)
```

- [ ] **Step 2: Add the Cline tab pane in the template**

Inside `<el-tabs v-model="activeTab">`, add a new tab pane after the OpenCode pane and before the Claude Code pane. Insert between the closing `</el-tab-pane>` of OpenCode and the opening `<el-tab-pane label="Claude Code" name="claude">`:

```html
        <el-tab-pane label="Cline / OpenAI 兼容" name="cline">
          <div class="tab-body">
            <el-alert
              class="section-alert"
              type="info"
              :closable="false"
              show-icon
              title="Cline 和其他 OpenAI 兼容客户端共用同一个配置格式：只需要 `baseURL`、`apiKey` 和默认模型。模型列表来自网关 `/v1/models`，不需要手工维护。"
            />

            <div class="step-card">
              <div class="step-head">
                <div>
                  <h4>步骤 1: 配置 Cline 或其他 OpenAI 兼容客户端</h4>
                  <p>
                    复制下面的 JSON，在客户端里填入 <code>Base URL</code>、<code>API Key</code>
                    和 <code>Model</code> 即可。模型列表可以从网关的
                    <code>/v1/models</code> 实时获取。
                  </p>
                </div>
                <el-button size="small" @click="copyCode(openAiCompatibleConfig)">复制</el-button>
              </div>
              <pre class="code-block">{{ openAiCompatibleConfig }}</pre>
            </div>

            <el-alert
              class="section-alert"
              type="success"
              :closable="false"
              show-icon
              title="保存后重新打开客户端即可。模型名、网关 URL 和 key 都已经按当前门户的最新值写好了。"
            />
          </div>
        </el-tab-pane>
```

- [ ] **Step 3: Run frontend type check and tests**

Run: `cd frontend && npx vue-tsc --noEmit && npx vitest run`
Expected: Both pass with 0 errors.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/views/portal/Integration.vue
git commit -m "feat: add Cline / OpenAI-compatible tab to Integration page"
```

---

### Task 3: Add Compatibility Matrix Summary to Integration Page

**Files:**
- Modify: `frontend/src/views/portal/Integration.vue`

- [ ] **Step 1: Add a compatibility matrix section in the template**

Insert a new `<el-card>` block after the `<el-card class="integration-hero">` closing tag and before the `<el-card v-if="sortedModelStats.length" class="model-card">` opening tag:

```html
    <el-card class="compat-matrix-card">
      <template #header>
        <div class="section-head">
          <div>
            <h3>客户端兼容矩阵</h3>
            <p>按协议族分组，每个客户端只需要一个配置即可连接网关。</p>
          </div>
        </div>
      </template>

      <div class="compat-grid">
        <div class="compat-family">
          <h4>Codex</h4>
          <span class="compat-protocol">Responses 协议</span>
          <code>{{ gatewayApiBaseUrl }}/responses</code>
          <span class="compat-auth">codex login --with-api-key</span>
          <span class="compat-models">model-catalog.json + /v1/models</span>
        </div>

        <div class="compat-family">
          <h4>OpenAI 兼容客户端</h4>
          <span class="compat-protocol">Chat Completions 协议</span>
          <code>{{ gatewayApiBaseUrl }}/chat/completions</code>
          <span class="compat-auth">Bearer 下游 Key</span>
          <span class="compat-models">/v1/models</span>
          <div class="compat-clients">
            <el-tag size="small" effect="plain">Cline</el-tag>
            <el-tag size="small" effect="plain">OpenCode</el-tag>
            <el-tag size="small" effect="plain">其他兼容工具</el-tag>
          </div>
        </div>

        <div class="compat-family">
          <h4>Anthropic 兼容客户端</h4>
          <span class="compat-protocol">Messages 协议</span>
          <code>{{ gatewayApiBaseUrl }}/messages</code>
          <span class="compat-auth">Bearer 下游 Key</span>
          <span class="compat-models">/v1/models + custom model option</span>
          <div class="compat-clients">
            <el-tag size="small" effect="plain">Claude Code</el-tag>
          </div>
        </div>
      </div>

      <el-alert
        class="status-alert"
        type="info"
        :closable="false"
        show-icon
        title="网关同时暴露 `/v1/chat/completions`、`/v1/responses`、`/v1/models` 和 `/v1/messages`。客户端只需要根据自己支持的协议族选对应的 endpoint 和 preset。"
      />
    </el-card>
```

- [ ] **Step 2: Add CSS for the compatibility matrix**

Append these styles inside the existing `<style scoped>` block in `Integration.vue`:

```css
.compat-matrix-card {
  border-radius: 16px;
  box-shadow: 0 12px 32px rgba(15, 23, 42, 0.06);
}

.compat-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 16px;
}

.compat-family {
  padding: 16px;
  border-radius: 12px;
  background: linear-gradient(180deg, #ffffff 0%, #f8fbff 100%);
  border: 1px solid #e6eef9;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.compat-family h4 {
  font-size: 15px;
  color: #1f2d3d;
  margin: 0;
}

.compat-protocol {
  font-size: 12px;
  color: #409eff;
  font-weight: 600;
}

.compat-family code {
  background: #f3f6fa;
  border: 1px solid #e3eaf3;
  padding: 2px 6px;
  border-radius: 6px;
  color: #1f2d3d;
  font-size: 12px;
}

.compat-auth,
.compat-models {
  font-size: 12px;
  color: #606266;
}

.compat-clients {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  margin-top: 4px;
}
```

- [ ] **Step 3: Run frontend type check and tests**

Run: `cd frontend && npx vue-tsc --noEmit && npx vitest run`
Expected: Both pass.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/views/portal/Integration.vue
git commit -m "feat: add client compatibility matrix to Integration page"
```

---

### Task 4: Update README with Client-to-Protocol Mapping

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a compatibility section in the Chinese section**

Find the line `### Codex 集成` in the Chinese section of README.md. Insert a new section before it:

```markdown
### 客户端兼容矩阵

网关同时暴露以下协议端点：

| 协议族 | 端点 | 典型客户端 |
|--------|------|------------|
| Responses | `/v1/responses` | Codex |
| Chat Completions | `/v1/chat/completions` | Cline, OpenCode, 其他 OpenAI 兼容工具 |
| Messages | `/v1/messages` | Claude Code |

每个客户端只需要一个 `base_url` 和一个下游 Bearer Key：

- Codex → 门户集成页的 **Codex** preset（`config.toml` + `model-catalog.json` + `codex login`）
- Cline → 门户集成页的 **Cline / OpenAI 兼容** preset（`baseURL` + `apiKey` + `model`）
- OpenCode → 门户集成页的 **OpenCode** preset（`opencode.json`）
- Claude Code → 门户集成页的 **Claude Code** preset（`settings.json`）
```

- [ ] **Step 2: Add a compatibility section in the English section**

Find the line `### Codex Integration` in the English section of README.md. Insert a new section before it:

```markdown
### Client Compatibility Matrix

The gateway exposes these protocol endpoints simultaneously:

| Protocol family | Endpoint | Typical clients |
|-----------------|----------|-----------------|
| Responses | `/v1/responses` | Codex |
| Chat Completions | `/v1/chat/completions` | Cline, OpenCode, other OpenAI-compatible tools |
| Messages | `/v1/messages` | Claude Code |

Each client only needs a `base_url` and a downstream Bearer key:

- Codex → portal integration page **Codex** preset (`config.toml` + `model-catalog.json` + `codex login`)
- Cline → portal integration page **Cline / OpenAI-compatible** preset (`baseURL` + `apiKey` + `model`)
- OpenCode → portal integration page **OpenCode** preset (`opencode.json`)
- Claude Code → portal integration page **Claude Code** preset (`settings.json`)
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add client compatibility matrix section to README"
```

---

### Task 5: Update Integration Guide with Client-to-Protocol Mapping

**Files:**
- Modify: `docs/codex-integration-guide.md`

- [ ] **Step 1: Add a compatibility section at the top of the guide**

Find the line `如果你已经登录了门户` in the guide. Before it, insert:

```markdown
## 客户端兼容矩阵

网关同时暴露 `/v1/chat/completions`、`/v1/responses`、`/v1/models` 和 `/v1/messages`。不同客户端根据自己支持的协议族选对应的端点：

| 客户端 | 协议族 | 端点 | 配置方式 |
|--------|--------|------|----------|
| Codex | Responses | `/v1/responses` | `config.toml` + `model-catalog.json` + `codex login --with-api-key` |
| Cline | Chat Completions | `/v1/chat/completions` | 门户 Cline preset（`baseURL` + `apiKey` + `model`） |
| OpenCode | Chat Completions | `/v1/chat/completions` | `opencode.json` |
| Claude Code | Messages | `/v1/messages` | `settings.json`（含 `ANTHROPIC_BASE_URL` 等环境变量） |

如果你已经登录了门户，优先打开 `<gateway_origin>/portal/integration`。页面会自动读取当前下游 key、当前网关 URL 和 live `/v1/models`，直接生成 Codex / OpenCode / Claude Code / Cline 的可复制配置。下面这些手工步骤保留着，方便你离线配置或做模板化部署。
```

Also update the original sentence to add Cline:

Change `直接生成 Codex / OpenCode / Claude Code 的可复制配置` to `直接生成 Codex / OpenCode / Claude Code / Cline 的可复制配置`.

- [ ] **Step 2: Commit**

```bash
git add docs/codex-integration-guide.md
git commit -m "docs: add client compatibility matrix to integration guide"
```

---

### Task 6: Add Protocol Route Contract Tests

**Files:**
- Create: `tests/gateway/compatibility.rs`
- Modify: `tests/gateway.rs`

- [ ] **Step 1: Create the compatibility test file**

Create `tests/gateway/compatibility.rs` with the following content:

```rust
use super::*;

#[tokio::test]
async fn v1_models_route_is_available() {
    let state = common::with_proxy_env_cleared(|| async {
        let dir = tempdir().unwrap();
        let downstream_key = generate_downstream_key();
        let state = AppState::new_with_file_backend(
            AppConfig::default(),
            dir.path().join("state.json"),
        )
        .await
        .unwrap();

        state
            .add_downstream(DownstreamConfig {
                id: "ds-1".to_string(),
                name: "test-downstream".to_string(),
                hash: downstream_key.clone(),
                plaintext_key: Some(downstream_key.clone()),
                model_allowlist: vec![],
                rate_limit_enabled: false,
                per_minute_limit: 0,
                max_concurrency: 0,
                ip_allowlist: vec![],
                active: true,
            })
            .await
            .unwrap();

        state
    })
    .await;

    let app = build_router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, format!("Bearer {}", state.downstreams().await.first().unwrap().plaintext_key.unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 2: Add the compatibility module reference**

In `tests/gateway.rs`, add a new module declaration:

```rust
#[path = "gateway/compatibility.rs"]
mod compatibility;
```

Insert this after the existing `mod responses;` line.

- [ ] **Step 3: Note about backend compilation**

The Rust backend tests require the frontend to be built first (embedded assets). In the worktree, the backend may not compile until `frontend/dist/` is populated. The frontend build (`cd frontend && npm run build`) will generate the dist directory. If backend compilation is not feasible in this worktree, these tests should be verified in the main checkout after merging.

- [ ] **Step 4: Commit**

```bash
git add tests/gateway/compatibility.rs tests/gateway.rs
git commit -m "test: add gateway protocol route contract test for /v1/models"
```

---

### Task 7: Verify All Frontend Tests Still Pass

**Files:**
- None (verification only)

- [ ] **Step 1: Run the full frontend test suite**

Run: `cd frontend && npx vitest run`
Expected: All 61+ tests pass (original 61 plus new `buildOpenAiCompatibleConfig` test).

- [ ] **Step 2: Run TypeScript type check**

Run: `cd frontend && npx vue-tsc --noEmit`
Expected: 0 errors.

- [ ] **Step 3: Verify no regressions in existing integration tests**

Run: `cd frontend && npx vitest run src/utils/integration.spec.ts`
Expected: All tests pass, including new Cline preset test.

- [ ] **Step 4: Final commit (if any fixes needed)**

If any fixes were needed, commit them. If everything passes cleanly, no commit needed.
