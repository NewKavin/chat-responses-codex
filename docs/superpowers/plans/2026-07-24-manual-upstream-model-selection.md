# Manual Upstream Model Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make explicit administrator selection the default authority for upstream models while retaining opt-in automatic discovery and the existing exact-route retry/cooldown behavior.

**Architecture:** Add a default-off `AppConfig` gate around automatic batch discovery and the background model-key sync service. Keep the explicit discovery endpoint available, store its results as transient frontend candidates, and derive authoritative per-Key mappings from the administrator's selected models immediately before create/update submission.

**Tech Stack:** Rust 2021, Axum, Tokio, Serde, Vue 3, TypeScript, Element Plus, Vitest, Cargo integration tests, Docker Compose.

---

## File Map

- `src/state/types.rs`: owns the new application configuration field and default.
- `src/main.rs`: parses and logs `UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED`.
- `src/state/model_key_sync.rs`: prevents periodic and targeted automatic discovery from starting when the gate is off.
- `src/server/admin.rs`: accepts explicit batch model mappings and skips upstream discovery when automatic discovery is off.
- `frontend/src/api/admin.ts`: owns pure candidate/mapping reconciliation helpers and the expanded batch payload type.
- `frontend/src/views/admin/Upstreams.vue`: keeps discovery candidates separate from selected models and submits only selected mappings.
- `tests/model_key_sync.rs`, `tests/admin_upstreams.rs`, `tests/templates.rs`, `tests/docker.rs`: Rust regression coverage.
- `frontend/tests/api/admin.spec.ts`, `frontend/tests/views/admin-ui.spec.ts`: frontend behavior and page-integration coverage.
- `.env.example`, `docker-compose.yml`, `README.md`, `DEPLOYMENT.md`, `docs/codex-integration-guide.md`: deployment and operator contract.

### Task 1: Add the Default-Off Automatic Discovery Gate

**Files:**
- Modify: `tests/templates.rs:80-90`
- Modify: `tests/model_key_sync.rs:84-122`
- Modify: `tests/model_key_sync.rs:411-429`
- Modify: `src/state/types.rs:75-148`
- Modify: `src/main.rs:90-105`
- Modify: `src/main.rs:153-164`
- Modify: `src/state/model_key_sync.rs:634-654`

- [ ] **Step 1: Write failing configuration and service-gate tests**

Add the default assertion to `tests/templates.rs`:

```rust
#[test]
fn app_config_defaults_stream_watchdog_settings() {
    let config = AppConfig::default();

    assert_eq!(config.upstream_stream_keepalive_interval_seconds, 3);
    assert_eq!(config.upstream_stream_idle_timeout_seconds, 1_800);
    assert_eq!(config.upstream_stream_max_duration_seconds, 86_400);
    assert_eq!(config.model_probe_refresh_interval_seconds, 15);
    assert_eq!(config.upstream_model_key_sync_interval_seconds, 0);
    assert!(!config.upstream_model_auto_discovery_enabled);
    assert!(!config.automatic_capability_probes_enabled);
}
```

Refactor the test helper in `tests/model_key_sync.rs` so service tests can independently set interval and gate:

```rust
fn sync_state_with_auto_discovery(
    base_url: String,
    api_key_models: Vec<ApiKeyModelConfig>,
    interval_seconds: u64,
    enabled: bool,
) -> (tempfile::TempDir, AppState) {
    let tempdir = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "sync-upstream".into(),
                name: "sync upstream".into(),
                base_url,
                api_key: "key-a".into(),
                api_keys: vec!["key-b".into()],
                api_key_models,
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["old-a".into(), "old-b".into()],
                active: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        tempdir.path().join("state.json"),
        AppConfig {
            admin_upstream_timeout_seconds: 1,
            upstream_model_key_sync_interval_seconds: interval_seconds,
            upstream_model_auto_discovery_enabled: enabled,
            ..AppConfig::default()
        },
    );
    (tempdir, state)
}

fn sync_state_with_interval(
    base_url: String,
    api_key_models: Vec<ApiKeyModelConfig>,
    interval_seconds: u64,
) -> (tempfile::TempDir, AppState) {
    sync_state_with_auto_discovery(base_url, api_key_models, interval_seconds, true)
}
```

Add a test proving the Boolean gate wins over a positive interval:

```rust
#[test]
fn disabled_auto_discovery_blocks_periodic_and_targeted_sync_with_positive_interval() {
    let (_tempdir, state) = sync_state_with_auto_discovery(
        "http://127.0.0.1:1".into(),
        Vec::new(),
        900,
        false,
    );

    assert!(ModelKeySyncService::spawn(state.clone()).is_none());
    assert!(!state.submit_targeted_model_discovery("up-1", "fingerprint", "model"));
    assert_eq!(state.targeted_model_discovery_pending_count(), 0);
}
```

Update `zero_interval_disables_periodic_and_targeted_model_sync` to set
`upstream_model_auto_discovery_enabled: true`, proving that interval `0` remains an independent kill switch.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
rtk cargo test --test templates app_config_defaults_stream_watchdog_settings -- --exact
rtk cargo test --test model_key_sync disabled_auto_discovery_blocks_periodic_and_targeted_sync_with_positive_interval -- --exact
```

Expected: compilation fails because `AppConfig` does not yet contain `upstream_model_auto_discovery_enabled`.

- [ ] **Step 3: Implement the configuration field, environment parsing, logging, and service gate**

Insert the field immediately before `upstream_model_key_sync_interval_seconds` in
`src/state/types.rs`:

```rust
pub upstream_model_auto_discovery_enabled: bool,
```

Insert the matching default immediately before
`upstream_model_key_sync_interval_seconds: 0`:

```rust
upstream_model_auto_discovery_enabled: false,
```

Parse and log the setting in `src/main.rs`:

```rust
upstream_model_auto_discovery_enabled: env_bool(
    "UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED",
    false,
),
upstream_model_key_sync_interval_seconds: env_u64(
    "UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS",
    0,
),
```

```rust
upstream_model_auto_discovery_enabled = config.upstream_model_auto_discovery_enabled,
```

Gate `ModelKeySyncService::spawn` before creating the targeted-discovery channel:

```rust
if !state.config.upstream_model_auto_discovery_enabled {
    return None;
}
```

The existing interval check and channel/task setup remain after this new early return.

- [ ] **Step 4: Run targeted tests and verify GREEN**

Run:

```bash
rtk cargo test --test templates app_config_defaults_stream_watchdog_settings -- --exact
rtk cargo test --test model_key_sync
```

Expected: both commands exit `0`; all model-key sync tests pass, including the disabled-gate and enabled-positive-interval cases.

- [ ] **Step 5: Commit the configuration gate**

```bash
rtk git add src/state/types.rs src/main.rs src/state/model_key_sync.rs tests/templates.rs tests/model_key_sync.rs
rtk git commit -m "feat(config): gate automatic upstream model discovery"
```

### Task 2: Make Batch Creation Persist Explicit Models by Default

**Files:**
- Modify: `tests/admin_upstreams.rs:93-110`
- Modify: `tests/admin_upstreams.rs:2416-2575`
- Modify: `src/server/admin.rs:842-883`
- Modify: `src/server/admin.rs:1080-1271`

- [ ] **Step 1: Add a configurable test-state helper**

Replace the single helper body in `tests/admin_upstreams.rs` with a configurable form while retaining the existing wrapper:

```rust
fn create_test_state_with_upstreams_and_config(
    upstreams: Vec<UpstreamConfig>,
    config: AppConfig,
) -> AppState {
    let state = PersistedState {
        upstreams,
        downstreams: vec![],
        usage_logs: vec![],
        announcement: None,
        global_context_profiles: std::collections::HashMap::new(),
    };
    attach_capability_probe_sink(AppState::new(state, unique_state_path(), config))
}

fn create_test_state_with_upstreams(upstreams: Vec<UpstreamConfig>) -> AppState {
    create_test_state_with_upstreams_and_config(
        upstreams,
        AppConfig {
            admin_username: "admin".to_string(),
            admin_password: "admin".to_string(),
            jwt_secret: "test_secret".to_string(),
            ..Default::default()
        },
    )
}
```

Change the two existing automatic batch-discovery tests to call the configurable helper with:

```rust
AppConfig {
    admin_username: "admin".into(),
    admin_password: "admin".into(),
    jwt_secret: "test_secret".into(),
    upstream_model_auto_discovery_enabled: true,
    ..AppConfig::default()
}
```

This preserves their existing opt-in automatic-discovery assertions.

- [ ] **Step 2: Write the failing manual batch test**

Add a mock `/v1/models` endpoint with an `AtomicUsize` hit counter, create a state with default config, and submit explicit mappings:

```rust
#[tokio::test]
async fn batch_create_uses_explicit_models_without_automatic_discovery_by_default() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_hits = hits.clone();
    let upstream_app = Router::new().route(
        "/v1/models",
        get(move || {
            let hits = upstream_hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Json(json!({"data": [{"id": "unselected-model"}]}))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let state = create_test_state_with_upstreams(vec![]);
    let app = build_router(state.clone());
    let token = get_admin_token(&app, "admin", "admin").await;
    let payload = json!({
        "name": "Manual Batch",
        "base_url": format!("http://{address}"),
        "keys": ["key-a", "key-b"],
        "supported_models": ["selected-a", "manual-shared"],
        "api_key_models": [
            {"api_key": "key-a", "supported_models": ["selected-a", "manual-shared"]},
            {"api_key": "key-b", "supported_models": ["manual-shared"]}
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/upstreams/batch")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let snapshot = state.snapshot().await;
    let upstream = snapshot
        .upstreams
        .iter()
        .find(|upstream| upstream.name == "Manual Batch")
        .unwrap();
    assert_eq!(upstream.supported_models, vec!["selected-a", "manual-shared"]);
    assert_eq!(
        upstream.api_key_models,
        vec![
            ApiKeyModelConfig {
                api_key: "key-a".into(),
                supported_models: vec!["selected-a".into(), "manual-shared".into()],
            },
            ApiKeyModelConfig {
                api_key: "key-b".into(),
                supported_models: vec!["manual-shared".into()],
            },
        ]
    );
    assert!(!upstream.auto_managed);
    assert_eq!(upstream.last_synced_at, 0);
    assert!(upstream.keys_for_model("unselected-model").is_empty());
}
```

- [ ] **Step 3: Run the new test and verify RED**

Run:

```bash
rtk cargo test --test admin_upstreams batch_create_uses_explicit_models_without_automatic_discovery_by_default -- --exact
```

Expected: FAIL because the payload ignores explicit mappings and the endpoint calls `/v1/models`.

- [ ] **Step 4: Extend the payload and branch model acquisition on the config gate**

Insert explicit fields immediately after `keys` in `BatchCreateUpstreamPayload`:

```rust
#[serde(default)]
supported_models: Vec<String>,
#[serde(default)]
api_key_models: Vec<ApiKeyModelConfig>,
```

Add a private result type and two acquisition helpers next to the payload defaults:

```rust
struct BatchModelConfiguration {
    api_key_models: Vec<ApiKeyModelConfig>,
    supported_models: Vec<String>,
    results: Vec<Value>,
    failed: usize,
}

fn explicit_batch_model_configuration(
    current_keys: &[String],
    supported_models: Vec<String>,
    api_key_models: Vec<ApiKeyModelConfig>,
) -> BatchModelConfiguration {
    let mut upstream = UpstreamConfig {
        api_key: current_keys.first().cloned().unwrap_or_default(),
        api_keys: current_keys.to_vec(),
        api_key_models,
        supported_models,
        ..Default::default()
    };
    upstream.normalize_for_storage();
    let results = current_keys
        .iter()
        .enumerate()
        .map(|(key_index, api_key)| {
            let model_list = upstream
                .api_key_models
                .iter()
                .find(|mapping| mapping.api_key == *api_key)
                .map(|mapping| mapping.supported_models.clone())
                .unwrap_or_default();
            json!({
                "key_index": key_index,
                "models": model_list.len(),
                "model_list": model_list,
            })
        })
        .collect();
    BatchModelConfiguration {
        api_key_models: upstream.api_key_models,
        supported_models: upstream.supported_models,
        results,
        failed: 0,
    }
}

async fn discover_batch_model_configuration(
    payload: &BatchCreateUpstreamPayload,
    current_keys: &[String],
    timeout_seconds: u64,
) -> BatchModelConfiguration {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .unwrap_or_default();
    let discovery_results = fetch_models_from_upstream_keys_concurrently(
        &client,
        &payload.base_url,
        &payload.keys,
        timeout_seconds,
    )
    .await;
    let mut models_by_key: HashMap<String, Vec<String>> = HashMap::new();
    let mut results = Vec::new();
    let mut failed = 0usize;
    for result in discovery_results {
        let api_key = payload
            .keys
            .get(result.key_index)
            .map(|key| key.trim().to_string())
            .unwrap_or_default();
        if let Some(error) = result.error {
            failed = failed.saturating_add(1);
            results.push(json!({"key_index": result.key_index, "error": error}));
            continue;
        }
        models_by_key
            .entry(api_key)
            .or_default()
            .extend(result.models.iter().cloned());
        results.push(json!({
            "key_index": result.key_index,
            "models": result.models.len(),
            "model_list": result.models,
        }));
    }
    for models in models_by_key.values_mut() {
        models.sort();
        models.dedup();
    }
    let api_key_models = current_keys
        .iter()
        .map(|api_key| ApiKeyModelConfig {
            api_key: api_key.clone(),
            supported_models: models_by_key.get(api_key).cloned().unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let mut supported_models = api_key_models
        .iter()
        .flat_map(|mapping| mapping.supported_models.iter().cloned())
        .collect::<Vec<_>>();
    supported_models.sort();
    supported_models.dedup();
    BatchModelConfiguration {
        api_key_models,
        supported_models,
        results,
        failed,
    }
}
```

In `admin_create_upstreams_batch`, replace the inline discovery block with:

```rust
let automatic_discovery = state.config.upstream_model_auto_discovery_enabled;
let model_configuration = if automatic_discovery {
    discover_batch_model_configuration(&payload, &current_keys, admin_timeout).await
} else {
    explicit_batch_model_configuration(
        &current_keys,
        payload.supported_models,
        payload.api_key_models,
    )
};
let BatchModelConfiguration {
    api_key_models,
    supported_models: all_models,
    results: key_results,
    failed,
} = model_configuration;
```

Set management metadata according to the branch:

```rust
auto_managed: automatic_discovery,
managed_source: automatic_discovery.then(|| "batch".to_string()),
last_synced_at: if automatic_discovery { now } else { 0 },
```

The only HTTP client construction now lives in `discover_batch_model_configuration`, so the disabled path has no accidental network work.

- [ ] **Step 5: Run the complete admin upstream test target and verify GREEN**

Run:

```bash
rtk cargo test --test admin_upstreams
```

Expected: exit `0`; the new manual-default test and the two explicitly enabled legacy automatic-discovery tests pass.

- [ ] **Step 6: Commit the backend behavior**

```bash
rtk git add src/server/admin.rs tests/admin_upstreams.rs
rtk git commit -m "feat(admin): persist explicitly selected upstream models"
```

### Task 3: Add Pure Frontend Candidate and Selection Reconciliation

**Files:**
- Modify: `frontend/tests/api/admin.spec.ts:1-150`
- Modify: `frontend/src/api/admin.ts:28-106`

- [ ] **Step 1: Write failing tests for candidate merging and selected mappings**

Import `buildSelectedKeyModelMappings` and `mergeDiscoveredModelCandidates` in
`frontend/tests/api/admin.spec.ts`, then add:

```typescript
it('keeps discovered models as candidates without changing the selected set', () => {
  const selected = ['existing-model']

  expect(mergeDiscoveredModelCandidates(
    selected,
    ['older-candidate'],
    [
      { key_index: 0, models: 2, model_list: ['glm-5.2', 'unwanted-model'] },
      { key_index: 1, error: 'upstream returned 503' }
    ]
  )).toEqual(['existing-model', 'glm-5.2', 'older-candidate', 'unwanted-model'])
  expect(selected).toEqual(['existing-model'])
})

it('builds authoritative key mappings from selected models only', () => {
  expect(buildSelectedKeyModelMappings(
    ['key-a', 'key-b'],
    ['glm-5.2', 'old-b', 'manual-only'],
    [
      { api_key: 'key-a', supported_models: ['old-a'] },
      { api_key: 'key-b', supported_models: ['old-b'] }
    ],
    [
      { key_index: 0, models: 2, model_list: ['glm-5.2', 'unwanted-model'] },
      { key_index: 1, error: 'upstream returned 503' }
    ]
  )).toEqual([
    { api_key: 'key-a', supported_models: ['glm-5.2', 'manual-only'] },
    { api_key: 'key-b', supported_models: ['old-b', 'manual-only'] }
  ])
})
```

The second assertion proves that `unwanted-model` and deselected `old-a` are removed, a failed Key retains selected prior evidence, and a selected model with no evidence is treated as an explicit administrator assertion on all current Keys.

- [ ] **Step 2: Run the frontend API test and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- tests/api/admin.spec.ts
```

Expected: FAIL because the two helper exports do not exist.

- [ ] **Step 3: Implement the pure helpers and expand the batch payload type**

Add required explicit fields to `BatchCreateUpstreamPayload`:

```typescript
export interface BatchCreateUpstreamPayload {
  name: string
  base_url: string
  keys: string[]
  supported_models: string[]
  api_key_models: ApiKeyModelConfig[]
  protocol?: string
  protocols?: string[]
  active?: boolean
  strip_nonstandard_chat_fields?: boolean
}
```

Add a shared normalizer plus the two pure helpers after `reconcileKeyModelMappings`:

```typescript
const uniqueModels = (models: string[]): string[] => {
  const seen = new Set<string>()
  const normalized: string[] = []
  for (const rawModel of models) {
    const model = String(rawModel || '').trim()
    if (model && !seen.has(model)) {
      seen.add(model)
      normalized.push(model)
    }
  }
  return normalized
}

export function mergeDiscoveredModelCandidates(
  selected: string[],
  previousCandidates: string[],
  results: KeyModelDiscoveryResult[]
): string[] {
  return uniqueModels([
    ...selected,
    ...previousCandidates,
    ...results.flatMap(result => result.error ? [] : (result.model_list || []))
  ]).sort()
}

export function buildSelectedKeyModelMappings(
  keys: string[],
  selectedModels: string[],
  previous: ApiKeyModelConfig[] = [],
  results: KeyModelDiscoveryResult[] = []
): ApiKeyModelConfig[] {
  const selected = uniqueModels(selectedModels)
  const selectedSet = new Set(selected)
  const mappings = reconcileKeyModelMappings(keys, previous, results).map(mapping => ({
    api_key: mapping.api_key,
    supported_models: mapping.supported_models.filter(model => selectedSet.has(model))
  }))
  const assigned = new Set(mappings.flatMap(mapping => mapping.supported_models))
  const assertedModels = selected.filter(model => !assigned.has(model))
  for (const mapping of mappings) {
    mapping.supported_models = uniqueModels([
      ...mapping.supported_models,
      ...assertedModels
    ])
  }
  return mappings
}
```

- [ ] **Step 4: Run the frontend API test and verify GREEN**

Run:

```bash
rtk npm --prefix frontend test -- tests/api/admin.spec.ts
```

Expected: exit `0`; existing indexed-discovery reconciliation and both new selection tests pass.

- [ ] **Step 5: Commit the frontend model-selection domain logic**

```bash
rtk git add frontend/src/api/admin.ts frontend/tests/api/admin.spec.ts
rtk git commit -m "feat(frontend): derive selected per-key model mappings"
```

### Task 4: Change the Upstream Editor to Candidate-Only Discovery

**Files:**
- Modify: `frontend/tests/views/admin-ui.spec.ts:50-77`
- Modify: `frontend/src/views/admin/Upstreams.vue:122-137`
- Modify: `frontend/src/views/admin/Upstreams.vue:285-335`
- Modify: `frontend/src/views/admin/Upstreams.vue:437-533`
- Modify: `frontend/src/views/admin/Upstreams.vue:535-656`
- Modify: `frontend/src/views/admin/Upstreams.vue:698-765`

- [ ] **Step 1: Write failing page-integration assertions**

Extend the upstream workbench test in `frontend/tests/views/admin-ui.spec.ts`:

```typescript
it('keeps discovered upstream models as explicit selection candidates', () => {
  const page = source('views/admin/Upstreams.vue')

  expect(page).toContain('discoveredModelCandidates')
  expect(page).toContain('latestDiscoveryResults')
  expect(page).toContain('mergeDiscoveredModelCandidates')
  expect(page).toContain('buildSelectedKeyModelMappings')
  expect(page).toContain('v-for="model in selectableModelOptions"')
  expect(page).not.toContain('form.value.supported_models = mappedModels')
})
```

- [ ] **Step 2: Run the page test and verify RED**

Run:

```bash
rtk npm --prefix frontend test -- tests/views/admin-ui.spec.ts
```

Expected: FAIL because the page still overwrites `form.supported_models` and has no transient candidate state.

- [ ] **Step 3: Add candidate state and render explicit options**

Import the two helpers and `KeyModelDiscoveryResult`:

```typescript
import {
  adminApi,
  buildSelectedKeyModelMappings,
  mergeDiscoveredModelCandidates,
  type BatchCreateUpstreamPayload
} from '@/api/admin'
import type {
  ApiKeyModelConfig,
  KeyModelDiscoveryResult,
  UpstreamConfig
} from '@/types'
```

Add transient state and a stable option list:

```typescript
const discoveredModelCandidates = ref<string[]>([])
const latestDiscoveryResults = ref<KeyModelDiscoveryResult[]>([])

const selectableModelOptions = computed(() => Array.from(new Set([
  ...(form.value.supported_models || []),
  ...discoveredModelCandidates.value
])).sort())

const resetDiscoveryCandidates = () => {
  discoveredModelCandidates.value = []
  latestDiscoveryResults.value = []
}
```

Call `resetDiscoveryCandidates()` in `handleCreate`, `handleCopy`, and `handleEdit`. Render candidates without selecting them:

```vue
<el-select
  v-model="form.supported_models"
  multiple
  filterable
  allow-create
  placeholder="手动输入或点击获取模型"
>
  <el-option
    v-for="model in selectableModelOptions"
    :key="model"
    :label="model"
    :value="model"
  />
</el-select>
```

- [ ] **Step 4: Make fetch update candidates only**

Replace the current mapping and selection assignment in `fetchModels` with:

```typescript
const result = response.data
latestDiscoveryResults.value = result.results || []
discoveredModelCandidates.value = mergeDiscoveredModelCandidates(
  form.value.supported_models || [],
  discoveredModelCandidates.value,
  latestDiscoveryResults.value
)
```

Do not assign `form.value.api_key_models` or `form.value.supported_models` inside `fetchModels`. Keep the current success and indexed failure messages.

- [ ] **Step 5: Build authoritative mappings immediately before submit**

After protocol normalization in `handleSubmit`, normalize current Keys once and derive the submitted mappings:

```typescript
const submittedKeys = (form.value.api_key || '')
  .split('\n')
  .map(key => key.trim())
  .filter((key, index, keys) => key.length > 0 && keys.indexOf(key) === index)

if (submittedKeys.length === 0) {
  ElMessage.error('请输入至少一个 API Key')
  submitting.value = false
  return
}

const selectedModels = Array.from(new Set(
  (form.value.supported_models || [])
    .map(model => String(model || '').trim())
    .filter(Boolean)
))
submitData.api_key = submittedKeys[0]
submitData.api_keys = submittedKeys.slice(1)
submitData.supported_models = selectedModels
submitData.api_key_models = buildSelectedKeyModelMappings(
  submittedKeys,
  selectedModels,
  form.value.api_key_models || [],
  latestDiscoveryResults.value
)
```

Use `submittedKeys` in both create and edit branches. For multi-Key batch creation, pass the explicit authority:

```typescript
const batchPayload: BatchCreateUpstreamPayload = {
  name: form.value.name!,
  base_url: form.value.base_url!,
  keys: submittedKeys,
  supported_models: submitData.supported_models || [],
  api_key_models: submitData.api_key_models || [],
  protocol: protocols[0] ? String(protocols[0]) : 'ChatCompletions',
  protocols: protocols.map(protocol => String(protocol)),
  active: submitData.active,
  strip_nonstandard_chat_fields: Boolean(submitData.strip_nonstandard_chat_fields)
}
```

For edit, retain `_replace_api_keys = true`; for single-Key create, submit the same explicit `api_key_models` through `createUpstream`.

- [ ] **Step 6: Run frontend tests and build**

Run:

```bash
rtk npm --prefix frontend test -- tests/api/admin.spec.ts tests/views/admin-ui.spec.ts
rtk npm --prefix frontend run build
```

Expected: both commands exit `0`; Vue TypeScript accepts the expanded batch payload, and the production bundle builds.

- [ ] **Step 7: Commit the upstream editor behavior**

```bash
rtk git add frontend/src/views/admin/Upstreams.vue frontend/tests/views/admin-ui.spec.ts
rtk git commit -m "feat(frontend): make upstream discovery candidate-only"
```

### Task 5: Publish the Default-Off Deployment Contract

**Files:**
- Modify: `tests/docker.rs:428-462`
- Modify: `tests/docker.rs:464-493`
- Modify: `.env.example:34-44`
- Modify: `docker-compose.yml:63-72`
- Modify: `README.md:88-100`
- Modify: `README.md:168-183`
- Modify: `README.md:438-450`
- Modify: `DEPLOYMENT.md:24-35`
- Modify: `DEPLOYMENT.md:84-100`
- Modify: `docs/codex-integration-guide.md:170-183`

- [ ] **Step 1: Write failing deployment-surface assertions**

Add this exact Compose interpolation to the expected list in `tests/docker.rs`:

```rust
"UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED: ${UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED:-false}",
```

Extend `deployment_surfaces_document_model_key_sync_and_process_local_health`:

```rust
assert!(dotenv.contains("UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED=false"));
assert!(compose.contains(
    "UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED: ${UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED:-false}"
));

for marker in [
    "Automatic upstream model discovery is disabled by default.",
    "Manual model discovery remains available when automatic discovery is disabled.",
    "Set to 0 to disable background model-key synchronization.",
    "UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS is parsed for backward compatibility only.",
    "UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS is deprecated for real upstream 429 responses.",
    "UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS is deprecated for route-health Retry-After.",
    "UPSTREAM_RATE_LIMIT_FORCE_RETRY_ENABLED does not force in-request waiting.",
    "Exact route health is process-local; run one active gateway instance per database.",
] {
    for (name, surface) in [
        (".env.example", dotenv.as_str()),
        ("docker-compose.yml", compose.as_str()),
        ("DEPLOYMENT.md", deployment.as_str()),
    ] {
        assert!(surface.contains(marker), "{name} should state `{marker}`");
    }
}
```

- [ ] **Step 2: Run deployment tests and verify RED**

Run:

```bash
rtk cargo test --test docker docker_compose_references_the_same_runtime_defaults_as_the_env_template -- --exact
rtk cargo test --test docker deployment_surfaces_document_model_key_sync_and_process_local_health -- --exact
```

Expected: FAIL because the new environment variable and documentation markers are absent.

- [ ] **Step 3: Add the environment variable to deployment templates**

Add to `.env.example`:

```dotenv
# Automatic discovery can add/remove persisted upstream model mappings.
# Leave false to make explicit administrator selection authoritative.
UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED=false
UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS=0
# Automatic upstream model discovery is disabled by default.
# Manual model discovery remains available when automatic discovery is disabled.
```

Add to the gateway environment in `docker-compose.yml`:

```yaml
UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED: ${UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED:-false}
UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS: ${UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS:-0}
# Automatic upstream model discovery is disabled by default.
# Manual model discovery remains available when automatic discovery is disabled.
```

- [ ] **Step 4: Update operator and Codex documentation**

Document these exact semantics in the Chinese and English README sections, `DEPLOYMENT.md`, and `docs/codex-integration-guide.md`:

```text
UPSTREAM_MODEL_AUTO_DISCOVERY_ENABLED defaults to false. When false, batch creation,
periodic synchronization, and targeted discovery cannot add or remove persisted model
mappings. The administrator's “获取模型” action remains available and only loads
candidates; selected models are persisted when the upstream is saved.

UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS controls the periodic cadence only after
automatic discovery is enabled. A value of 0 independently disables periodic and
targeted synchronization.

Automatic upstream model discovery is disabled by default.
Manual model discovery remains available when automatic discovery is disabled.
```

Keep the existing explanation that runtime cooldown does not alter `/v1/models` and that real upstream 429 responses fall through to other routes without waiting in-request.

- [ ] **Step 5: Run deployment tests and verify GREEN**

Run:

```bash
rtk cargo test --test templates
rtk cargo test --test docker docker_compose_references_the_same_runtime_defaults_as_the_env_template -- --exact
rtk cargo test --test docker deployment_surfaces_document_model_key_sync_and_process_local_health -- --exact
rtk docker compose config --quiet
```

Expected: every command exits `0`; Compose resolves with the new variable defaulting to `false`.

- [ ] **Step 6: Commit deployment documentation**

```bash
rtk git add .env.example docker-compose.yml README.md DEPLOYMENT.md docs/codex-integration-guide.md tests/docker.rs
rtk git commit -m "docs: default upstream model discovery to manual"
```

### Task 6: Run Full Regression and Acceptance Verification

**Files:**
- Verify: `src/server/gateway.rs`
- Verify: `src/state/route_health.rs`
- Verify: `tests/gateway/chat/routing.rs`
- Verify: `tests/multi_key_mapping.rs`
- Verify: all files modified in Tasks 1-5

- [ ] **Step 1: Format and check the complete diff**

Run:

```bash
rtk cargo fmt --all
rtk git diff --check
rtk git status --short
```

Expected: formatting completes, `git diff --check` exits `0`, and status contains only the intended files.

- [ ] **Step 2: Run focused Rust regressions**

Run:

```bash
rtk cargo test --test model_key_sync
rtk cargo test --test admin_upstreams
rtk cargo test --test multi_key_mapping
rtk cargo test --test gateway rate_limit_retry_after_cools_the_route_without_waiting_in_request
rtk cargo test --test gateway generic_500_retries_the_same_key_route_once_before_fallback
```

Expected: all commands exit `0`. The 429 test proves immediate fallback/cooldown behavior is unchanged; the 500 test proves the one-time same-route retry is unchanged.

- [ ] **Step 3: Run the full Rust workspace**

Run:

```bash
rtk cargo test --workspace
```

Expected: exit `0` with no failing unit, integration, persistence, routing, or template tests.

- [ ] **Step 4: Run the full frontend verification**

Run:

```bash
rtk npm --prefix frontend test
rtk npm --prefix frontend run build
```

Expected: all Vitest files pass, `vue-tsc` passes, and Vite emits a production bundle.

- [ ] **Step 5: Verify acceptance behavior against the requirements**

Use the admin UI with a mock or disposable upstream and confirm:

```text
1. Open an existing upstream and record its selected models.
2. Click “获取模型”.
3. Confirm newly discovered models appear as dropdown candidates but are not selected.
4. Select one discovered model and leave another unselected.
5. Save and reload the upstream.
6. Confirm only the selected model was persisted and advertised.
7. Confirm a selected model with two configured upstreams still falls through after one route returns 429.
```

Do not use a production Key for this acceptance check. The backend integration tests provide the authoritative no-network and exact-mapping evidence.

- [ ] **Step 6: Record the final verification state**

If formatting changed files after their task commits, create one formatting-only commit:

```bash
rtk git add src tests frontend .env.example docker-compose.yml README.md DEPLOYMENT.md docs/codex-integration-guide.md
rtk git commit -m "style: normalize manual model selection changes"
```

If formatting produced no diff, do not create an empty commit. Report exact test counts, build results, and any skipped live acceptance step in the completion handoff.
