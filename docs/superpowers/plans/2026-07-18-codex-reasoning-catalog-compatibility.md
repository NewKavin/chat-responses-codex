# Codex Reasoning Catalog Compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Codex 0.144.5 model selection close correctly and expose only reasoning efforts verified for the selected gateway route, while keeping portal-generated TOML aligned with the live catalog.

**Architecture:** The backend derives Codex reasoning metadata from the existing `CatalogWitnessEntry.capabilities`; an explicit single `none` level represents unknown controls, while a non-empty resolved effort map becomes the selectable level list. The portal consumes the selected catalog model's default and passes it into the TOML generator instead of embedding `high`.

**Tech Stack:** Rust 2024, Axum, serde/serde_json, Tokio integration tests, Vue 3, TypeScript, Vitest, Codex CLI 0.144.5, Docker.

---

## File Map

- Modify `src/server/gateway.rs`: derive and serialize Codex reasoning metadata from the selected route witness.
- Modify `tests/gateway/capability_routing.rs`: cover explicit `none`, verified mapped levels, deterministic order, and defaults through the real HTTP catalog route.
- Modify `frontend/src/utils/integration.ts`: carry the selected catalog default through the integration view state and TOML builder.
- Modify `frontend/src/views/portal/Integration.vue`: pass the catalog-derived effort into `buildCodexConfigToml`.
- Modify `frontend/tests/utils/integration.spec.ts`: cover catalog-default propagation and `none` fallback.
- Modify `templates/codex/config.toml.example`: use the conservative static `none` default.
- Modify `docs/codex-integration-guide.md`: explain that the portal derives the default from the downloaded catalog.

### Task 1: Lock Backend Catalog Behavior With Failing HTTP Tests

**Files:**
- Test: `tests/gateway/capability_routing.rs:559`

- [ ] **Step 1: Add the unknown-control regression test**

Add this test after the existing image catalog test. It exercises the same `/v1/models?client_version=...` route used by Codex rather than testing a private helper:

```rust
#[tokio::test]
async fn codex_catalog_uses_explicit_none_when_reasoning_control_is_unverified() {
    let model = "arbitrary/reasoning-unknown";
    let upstream = catalog_upstream("reasoning-unknown-route", &[model]);
    let (_tempdir, state, secret) = catalog_state(vec![upstream.clone()], vec![model.into()]);
    put_catalog_profile(
        &state,
        &upstream,
        model,
        DialectProfileState::Verified,
        &[
            (Capability::FunctionTools, EvidenceState::Supported),
            (Capability::ToolContinuation, EvidenceState::Supported),
            (Capability::ReasoningOutput, EvidenceState::Supported),
        ],
    )
    .await;

    let catalog = get_models(state, &secret, true).await;
    let model = &catalog["models"][0];
    assert_eq!(
        model["supported_reasoning_levels"],
        json!([{
            "effort": "none",
            "description": "Do not request a configurable reasoning effort"
        }])
    );
    assert_eq!(model["default_reasoning_level"], "none");
    assert_eq!(model["supports_reasoning_summaries"], false);
}
```

- [ ] **Step 2: Convert the existing verified-control test to the desired behavior**

Rename `codex_catalog_suppresses_reasoning_levels_when_summaries_are_not_supported` to `codex_catalog_advertises_only_verified_reasoning_levels`. Give its semantic policy three canonical mappings and its profile the three accepted upstream values:

```rust
semantic: SemanticPolicy {
    effort_map: std::collections::BTreeMap::from([
        ("high".into(), "upstream-high".into()),
        ("low".into(), "upstream-low".into()),
        ("medium".into(), "upstream-medium".into()),
    ]),
    ..Default::default()
},
```

```rust
profile.reasoning_controls.insert(
    "reasoning_effort".into(),
    vec![
        "upstream-high".into(),
        "upstream-low".into(),
        "upstream-medium".into(),
    ],
);
```

Replace the old empty assertions with:

```rust
assert_eq!(model["supports_reasoning_summaries"], true);
assert_eq!(
    model["supported_reasoning_levels"],
    json!([
        {"effort": "low", "description": "Use low reasoning effort"},
        {"effort": "medium", "description": "Use medium reasoning effort"},
        {"effort": "high", "description": "Use high reasoning effort"}
    ])
);
assert_eq!(model["default_reasoning_level"], "medium");
```

- [ ] **Step 3: Run both focused tests and verify RED**

Run:

```bash
rtk cargo test --test gateway capability_routing::codex_catalog_uses_explicit_none_when_reasoning_control_is_unverified -- --exact
rtk cargo test --test gateway capability_routing::codex_catalog_advertises_only_verified_reasoning_levels -- --exact
```

Expected: both tests fail against the current hard-coded metadata. The first receives `[]`/`null`; the second receives `false`, `[]`, and `null`. A compile error or missing model is not an acceptable RED result.

### Task 2: Derive Backend Reasoning Metadata From the Selected Witness

**Files:**
- Modify: `src/server/gateway.rs:1387-1455`
- Test: `tests/gateway/capability_routing.rs`

- [ ] **Step 1: Add a focused metadata value and deterministic ordering helpers**

Place these helpers immediately before `list_models_codex_format`:

```rust
struct CodexReasoningMetadata {
    supported_levels: Vec<Value>,
    default_level: Value,
    supports_summaries: bool,
}

const CODEX_REASONING_EFFORT_ORDER: [&str; 6] =
    ["minimal", "low", "medium", "high", "xhigh", "max"];

fn codex_reasoning_effort_rank(effort: &str) -> usize {
    CODEX_REASONING_EFFORT_ORDER
        .iter()
        .position(|candidate| *candidate == effort)
        .unwrap_or(CODEX_REASONING_EFFORT_ORDER.len())
}

fn codex_reasoning_description(effort: &str) -> String {
    format!("Use {effort} reasoning effort")
}

fn codex_reasoning_metadata(resolved: &ResolvedCapabilities) -> CodexReasoningMetadata {
    let verified_control = resolved.supports(Capability::ReasoningOutput)
        && resolved.reasoning_control_field.is_some()
        && !resolved.effort_map.is_empty();

    if !verified_control {
        return CodexReasoningMetadata {
            supported_levels: vec![json!({
                "effort": "none",
                "description": "Do not request a configurable reasoning effort"
            })],
            default_level: Value::String("none".into()),
            supports_summaries: false,
        };
    }

    let mut efforts = resolved
        .effort_map
        .keys()
        .filter(|effort| !effort.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    efforts.sort_by(|left, right| {
        codex_reasoning_effort_rank(left)
            .cmp(&codex_reasoning_effort_rank(right))
            .then_with(|| left.cmp(right))
    });

    if efforts.is_empty() {
        return CodexReasoningMetadata {
            supported_levels: vec![json!({
                "effort": "none",
                "description": "Do not request a configurable reasoning effort"
            })],
            default_level: Value::String("none".into()),
            supports_summaries: false,
        };
    }

    let default_effort = efforts
        .iter()
        .find(|effort| effort.as_str() == "medium")
        .cloned()
        .unwrap_or_else(|| efforts[0].clone());
    let supported_levels = efforts
        .into_iter()
        .map(|effort| {
            json!({
                "description": codex_reasoning_description(&effort),
                "effort": effort,
            })
        })
        .collect();

    CodexReasoningMetadata {
        supported_levels,
        default_level: Value::String(default_effort),
        supports_summaries: true,
    }
}
```

- [ ] **Step 2: Replace the hard-coded catalog fields**

Immediately after selecting `witness`, derive metadata and use it in the JSON object:

```rust
let reasoning = codex_reasoning_metadata(&witness.capabilities);
```

```rust
"supported_reasoning_levels": reasoning.supported_levels,
"default_reasoning_level": reasoning.default_level,
"supports_reasoning_summaries": reasoning.supports_summaries,
```

Remove the old `let supported_reasoning_levels: Vec<Value> = Vec::new();`, the null default, and the hard-coded false summary flag. Do not change witness selection, context limits, tool capabilities, or visibility.

- [ ] **Step 3: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --test gateway capability_routing::codex_catalog_uses_explicit_none_when_reasoning_control_is_unverified -- --exact
rtk cargo test --test gateway capability_routing::codex_catalog_advertises_only_verified_reasoning_levels -- --exact
```

Expected: both pass, with exact ordered JSON values.

- [ ] **Step 4: Run the catalog regression group**

Run:

```bash
rtk cargo test --test gateway capability_routing::codex_catalog
rtk cargo fmt --check
```

Expected: every `codex_catalog_*` test passes and formatting is clean. Update pinned catalog assertions only when they conflict with the explicit `none` contract; do not weaken unrelated capability checks.

- [ ] **Step 5: Commit the backend behavior**

```bash
rtk git add src/server/gateway.rs tests/gateway/capability_routing.rs
rtk git commit -m "fix(codex): derive catalog reasoning levels" \
  -m "Emit verified effort mappings and an explicit none fallback so Codex 0.144.5 can finish model selection without speculative upstream controls." \
  -m "Constraint: Only advertise effort values accepted by the selected route witness" \
  -m "Rejected: Publish low, medium, and high for every model | can trigger upstream parameter errors" \
  -m "Confidence: high" \
  -m "Scope-risk: moderate"
```

### Task 3: Lock Portal Default Propagation With Failing Tests

**Files:**
- Test: `frontend/tests/utils/integration.spec.ts:115-217`

- [ ] **Step 1: Add catalog-derived effort assertions**

Add this test before the TOML builder test:

```ts
it('derives the primary Codex reasoning effort from the live catalog', () => {
  const state = buildIntegrationCatalogViewState({
    catalog: {
      models: [
        {
          slug: 'verified/model',
          default_reasoning_level: 'medium'
        }
      ]
    },
    modelAllowlist: [],
    portalModelStats: []
  })

  expect(state.primaryModelSlug).toBe('verified/model')
  expect(state.primaryModelReasoningEffort).toBe('medium')
})

it('uses none when the catalog default reasoning effort is absent', () => {
  const state = buildIntegrationCatalogViewState({
    catalog: { models: [{ slug: 'unknown/model', default_reasoning_level: null }] },
    modelAllowlist: [],
    portalModelStats: []
  })

  expect(state.primaryModelReasoningEffort).toBe('none')
})
```

Update the existing empty-state expectation to include:

```ts
primaryModelReasoningEffort: 'none',
```

- [ ] **Step 2: Make the TOML test demand the caller-provided effort**

Pass `modelReasoningEffort: 'medium'` into the existing `buildCodexConfigToml` test and add:

```ts
expect(toml).toContain('model_reasoning_effort = "medium"')
expect(toml).not.toContain('model_reasoning_effort = "high"')
```

Pass `modelReasoningEffort: 'none'` to the second builder call in the hosted-search test so TypeScript remains ready for the required input field.

- [ ] **Step 3: Run the frontend test and verify RED**

Run from the repository root:

```bash
rtk npm --prefix frontend test -- tests/utils/integration.spec.ts
```

Expected: the view-state assertions fail because `primaryModelReasoningEffort` is missing, and the TOML assertion fails because the implementation still emits `high`.

### Task 4: Make Portal TOML Follow the Live Catalog

**Files:**
- Modify: `frontend/src/utils/integration.ts:11-35, 185-260`
- Modify: `frontend/src/views/portal/Integration.vue:532-585`
- Modify: `frontend/tests/utils/integration.spec.ts`
- Modify: `templates/codex/config.toml.example:8`
- Modify: `docs/codex-integration-guide.md:91,334,350-365`

- [ ] **Step 1: Add the reasoning effort to integration view state**

Extend the types and empty state:

```ts
export type IntegrationCatalogViewState = {
  allModelSlugs: string[]
  primaryModelSlug: string
  primaryModelReasoningEffort: string
  sortedModelStats: PortalModelStat[]
  canGenerateConfigurationContent: boolean
}

type CodexConfigInput = {
  gatewayBaseUrl: string
  modelSlug: string
  modelReasoningEffort: string
}
```

```ts
const emptyIntegrationCatalogViewState = (): IntegrationCatalogViewState => ({
  allModelSlugs: [],
  primaryModelSlug: '',
  primaryModelReasoningEffort: 'none',
  sortedModelStats: [],
  canGenerateConfigurationContent: false
})
```

Add this helper next to `choosePrimaryModelSlug`:

```ts
const chooseCodexReasoningEffort = (
  catalog: CodexCatalogResponse,
  modelSlug: string
) => {
  const model = catalog.models.find(item => normalizeSlug(item.slug) === modelSlug)
  const effort = normalizeSlug(model?.default_reasoning_level)
  return effort || 'none'
}
```

In the non-empty return path, compute `primaryModelSlug` once and return:

```ts
const primaryModelSlug = allModelSlugs[0]
return {
  allModelSlugs,
  primaryModelSlug,
  primaryModelReasoningEffort: chooseCodexReasoningEffort(catalog, primaryModelSlug),
  sortedModelStats: buildModelUsageStats(allModelSlugs, portalModelStats),
  canGenerateConfigurationContent: true
}
```

- [ ] **Step 2: Remove the hard-coded TOML effort**

Inside `buildCodexConfigToml`, normalize the caller value and serialize it safely:

```ts
const modelReasoningEffort = normalizeSlug(input.modelReasoningEffort) || 'none'
```

Replace the literal line with:

```ts
model_reasoning_effort = ${tomlString(modelReasoningEffort)}
```

- [ ] **Step 3: Pass the selected catalog default from the portal view**

Add a computed alias after `primaryModelSlug`:

```ts
const primaryModelReasoningEffort = computed(
  () => catalogViewState.value.primaryModelReasoningEffort
)
```

Update the builder call:

```ts
buildCodexConfigToml({
  gatewayBaseUrl: gatewayBaseUrl.value,
  modelSlug: primaryModelSlug.value,
  modelReasoningEffort: primaryModelReasoningEffort.value
})
```

- [ ] **Step 4: Align static examples and guide text**

Change the static template and both literal TOML blocks in the guide from:

```toml
model_reasoning_effort = "high"
```

to:

```toml
model_reasoning_effort = "none"
```

Next to the guide's option descriptions, state that the portal-generated value comes from the selected model's `default_reasoning_level`; `none` is the conservative fallback when no verified configurable control is available. Do not tell users to force `high` for an unknown model.

- [ ] **Step 5: Run frontend tests and build**

Run:

```bash
rtk npm --prefix frontend test -- tests/utils/integration.spec.ts
rtk npm --prefix frontend run build
rtk cargo test --test templates
```

Expected: all commands exit 0; Vitest confirms derived `medium` and fallback `none`; `vue-tsc` accepts the updated call signature; template checks remain aligned.

- [ ] **Step 6: Commit portal and documentation behavior**

```bash
rtk git add frontend/src/utils/integration.ts frontend/src/views/portal/Integration.vue frontend/tests/utils/integration.spec.ts templates/codex/config.toml.example docs/codex-integration-guide.md
rtk git commit -m "fix(portal): align Codex effort with catalog" \
  -m "Generate model_reasoning_effort from the selected live catalog entry and use none for static or malformed metadata." \
  -m "Constraint: Keep config.toml and model-catalog.json on one capability snapshot" \
  -m "Confidence: high" \
  -m "Scope-risk: narrow"
```

### Task 5: Full Automated Verification

**Files:**
- Verify: Rust workspace and `frontend/**`

- [ ] **Step 1: Run complete automated verification**

```bash
rtk cargo fmt --check
rtk cargo test --locked --offline
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run build
rtk cargo build --release --locked --offline
```

Expected: zero failures, no formatting changes, and `target/release/chat-responses-codex` is rebuilt from the verified source tree.

### Task 6: Build the Final Image and Replace Only the Gateway

**Files:**
- Artifact: `target/release/chat-responses-codex`
- Runtime compose: `/home/kavin/docker/chat-responses-codex/docker-compose.yml`

- [ ] **Step 1: Record the current runtime and artifact identity**

```bash
rtk sha256sum target/release/chat-responses-codex
rtk docker inspect --format '{{.Image}} {{.RestartCount}} {{.State.Health.Status}}' chat-responses-codex
rtk docker images chat-responses-codex --format '{{.Repository}}:{{.Tag}} {{.ID}}'
```

Expected: current gateway is healthy and the only retained named project image is `chat-responses-codex:latest`.

- [ ] **Step 2: Build `latest` from the local verified binary**

```bash
rtk scripts/build-package-image.sh --skip-npm-install --skip-frontend-build --skip-backend-build --skip-export
rtk docker run --rm --entrypoint sha256sum chat-responses-codex:latest /usr/local/bin/chat-responses-codex
```

Expected: image and host binary SHA-256 values match. The script creates no extra named project tag or tar archive.

- [ ] **Step 3: Recreate only the gateway container**

```bash
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml up -d --no-deps gateway
rtk curl -fsS --retry 20 --retry-delay 1 --retry-all-errors http://127.0.0.1:3000/healthz
rtk docker inspect --format '{{.State.Status}} {{.State.Health.Status}} {{.RestartCount}}' chat-responses-codex
```

Expected: gateway reports `running healthy 0`, `/healthz` returns `ok`, and PostgreSQL/Redis containers are not recreated.

- [ ] **Step 4: Final repository and image audit**

```bash
rtk git status --short
rtk git log -4 --oneline --decorate
rtk docker images chat-responses-codex --format '{{.Repository}}:{{.Tag}} {{.ID}}'
```

Expected: only the user's pre-existing modifications remain unstaged, the implementation commits are present, and `chat-responses-codex:latest` is the only named project image.

### Task 7: Codex 0.144.5 TUI Acceptance Against the Deployed Gateway

**Files:**
- Read-only source config: `/home/kavin/.codex/config.toml`
- Read-only source auth: `/home/kavin/.codex/auth.json`
- Temporary acceptance home: `/tmp/codex-reasoning-catalog-acceptance`

- [ ] **Step 1: Record the real Codex file hashes**

```bash
rtk sha256sum /home/kavin/.codex/config.toml /home/kavin/.codex/model-catalog.json /home/kavin/.codex/auth.json
```

Expected: three hashes are recorded without printing file contents or credentials.

- [ ] **Step 2: Prepare an isolated Codex home**

Create `/tmp/codex-reasoning-catalog-acceptance` with mode `0700`, copy `auth.json` with mode `0600`, and mechanically copy `config.toml` while omitting only the `model_catalog_json` line. The resulting config keeps the gateway provider but forces Codex to fetch the deployed `/models?client_version=...` response. Do not print authentication data.

```bash
rtk install -d -m 700 /tmp/codex-reasoning-catalog-acceptance
rtk cp /home/kavin/.codex/auth.json /tmp/codex-reasoning-catalog-acceptance/auth.json
rtk chmod 600 /tmp/codex-reasoning-catalog-acceptance/auth.json
rtk awk '$1 != "model_catalog_json" { print }' /home/kavin/.codex/config.toml > /tmp/codex-reasoning-catalog-acceptance/config.toml
rtk chmod 600 /tmp/codex-reasoning-catalog-acceptance/config.toml
```

- [ ] **Step 3: Verify the live catalog contract through Codex**

```bash
rtk env CODEX_HOME=/tmp/codex-reasoning-catalog-acceptance codex debug models
```

Expected: every listed model has at least one `supported_reasoning_levels` entry; models without verified controls have exactly `none` with default `none`; no parse error occurs.

- [ ] **Step 4: Reproduce the model picker interaction without sending a prompt**

Start an interactive PTY:

```bash
rtk env CODEX_HOME=/tmp/codex-reasoning-catalog-acceptance codex --no-alt-screen
```

Enter `/model`, select `kimi-k3` once, and observe that the picker closes after one Enter with a single model-change notification. For any catalog entry with multiple verified levels, select it and confirm that `Select Reasoning Level` appears and that choosing one closes both views. Exit Codex without submitting a model prompt.

- [ ] **Step 5: Audit logs and remove only the isolated acceptance home**

```bash
rtk docker logs --since 10m chat-responses-codex
rtk rm -rf /tmp/codex-reasoning-catalog-acceptance
rtk sha256sum /home/kavin/.codex/config.toml /home/kavin/.codex/model-catalog.json /home/kavin/.codex/auth.json
```

Expected: no new 400, 499, 5xx, panic, or restart evidence; the TUI-only acceptance creates no inference request; the three real-file hashes match Step 1 exactly.
