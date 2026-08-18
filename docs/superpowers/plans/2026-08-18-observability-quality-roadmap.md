# Observability And Quality Roadmap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the highest-value operational and quality gaps in the approved order: bounded logs, credential-failure visibility, CI, deployment verification, and better list-column UX.

**Architecture:** Keep each phase independently shippable and independently tested. Add pure helper functions and dependency-injected script seams so Rust/frontend tests can verify behavior without waiting on wall-clock rotation or real Docker deployments. Do not reorganize the large gateway/state modules in this roadmap.

**Tech Stack:** Rust 2021, `tracing-appender`, Axum integration tests, Bash deploy script with fake Docker fixtures, GitHub Actions, Vue 3, Vitest, `@vue/test-utils` + `happy-dom`.

---

## Phase 1: Bounded Runtime Logs

### Task 1: Application Log Rotation

**Files:**
- Modify: `src/main.rs:661-743`
- Test: `tests/log_rotation.rs`

- [ ] **Step 1: Write failing tests for rotation configuration and retention**

Create `tests/log_rotation.rs`:

```rust
use std::fs;
use std::io::Write;

#[test]
fn log_rotation_policy_defaults_to_daily_and_honors_configuration() {
    assert_eq!(
        chat_responses_codex::log_rotation_cadence_from_env(|| None),
        chat_responses_codex::LogRotationCadence::Daily
    );
    assert_eq!(
        chat_responses_codex::log_rotation_cadence_from_env(|| Some("never".into())),
        chat_responses_codex::LogRotationCadence::Never
    );
    assert_eq!(
        chat_responses_codex::log_rotation_cadence_from_env(|| Some("hourly".into())),
        chat_responses_codex::LogRotationCadence::Hourly
    );
    assert_eq!(
        chat_responses_codex::log_rotation_cadence_from_env(|| Some("bogus".into())),
        chat_responses_codex::LogRotationCadence::Daily
    );
}

#[test]
fn rolling_log_files_rotate_by_cadence_and_remove_expired_files() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("nested").join("logs");
    let prefix = "gateway";

    let first = chat_responses_codex::prepare_rolling_log_appender(
        &dir,
        prefix,
        chat_responses_codex::LogRotationCadence::Hourly,
        Some(2),
    );
    let first_path = first.path().to_path_buf();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expected_stamp = (now / 3600) * 3600;
    assert!(first_path
        .to_string_lossy()
        .contains(&format!("{prefix}.{expected_stamp}")));

    let mut first_file = fs::File::options().append(true).open(&first_path).unwrap();
    writeln!(first_file, "current").unwrap();

    let stale = dir.join(format!("{prefix}.2000010100"));
    let expired = dir.join(format!("{prefix}.1999010100"));
    fs::write(&stale, "stale").unwrap();
    fs::write(&expired, "expired").unwrap();

    chat_responses_codex::prepare_rolling_log_appender(
        &dir,
        prefix,
        chat_responses_codex::LogRotationCadence::Hourly,
        Some(2),
    );
    assert!(first_path.exists());
    assert!(stale.exists());
    assert!(!expired.exists());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test log_rotation`
Expected: FAIL because the new public API is undefined.

- [ ] **Step 3: Implement minimal rotation API and wire tracing**

Export the API from `src/lib.rs`. In `src/main.rs`:

```rust
use tracing_appender::rolling::{RollingFileAppender, Rotation};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogRotationCadence {
    Never,
    Hourly,
    Daily,
}

pub fn log_rotation_cadence_from_env(
    read: impl FnOnce() -> Option<String>,
) -> LogRotationCadence {
    match read()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("never" | "off" | "false") => LogRotationCadence::Never,
        Some("hourly") => LogRotationCadence::Hourly,
        _ => LogRotationCadence::Daily,
    }
}

fn rotation_from_cadence(cadence: LogRotationCadence) -> Rotation {
    match cadence {
        LogRotationCadence::Never => Rotation::NEVER,
        LogRotationCadence::Hourly => Rotation::HOURLY,
        LogRotationCadence::Daily => Rotation::DAILY,
    }
}

pub fn prepare_rolling_log_appender(
    directory: &std::path::Path,
    file_prefix: &str,
    cadence: LogRotationCadence,
    max_files: Option<usize>,
) -> RollingFileAppender {
    std::fs::create_dir_all(directory).expect("create log rotation directory");
    if let Some(max_files) = max_files {
        let mut names: Vec<std::path::PathBuf> = std::fs::read_dir(directory)
            .expect("read log rotation directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&format!("{file_prefix}.")))
            })
            .collect();
        names.sort();
        let excess = names.len().saturating_sub(max_files.saturating_sub(1));
        for path in names.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }

    RollingFileAppender::builder()
        .rotation(rotation_from_cadence(cadence))
        .filename_prefix(file_prefix)
        .max_log_files(max_files)
        .build(directory)
        .expect("build rolling log appender")
}
```

Change `init_tracing(log_path)` to:
1. Read `LOG_ROTATION` through `log_rotation_cadence_from_env`.
2. Read `LOG_ROTATION_MAX_FILES`; normalize `0` to `None`.
3. Use the log parent directory and original file stem as the rolling prefix.
4. Keep stdout + file tee behavior. Change `TeeWriter` to own a boxed `Write + Send` so it can wrap either current file appender or rolling appender.
5. Preserve the existing non-blocking `WorkerGuard`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test log_rotation`
Expected: PASS.

- [ ] **Step 5: Document configuration**

Append to `.env.example`:

```bash
# never | hourly | daily
LOG_ROTATION=daily
LOG_ROTATION_MAX_FILES=14
```

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/lib.rs tests/log_rotation.rs .env.example
git commit -m "feat: rotate gateway runtime logs"
```

### Task 2: Bound Docker Runtime Logs

**Files:**
- Modify: `docker-compose.yml:1-75`
- Test: `tests/scripts.rs:410-554`

- [ ] **Step 1: Extend the deploy fixture with a failing assertion**

Add after `fs::copy("docker-compose.yml", ...)`:

```rust
let compose_source = fs::read_to_string(repo_root.join("docker-compose.yml")).unwrap();
assert!(compose_source.contains("max-size: \"50m\""));
assert!(compose_source.contains("max-file: \"5\""));
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test deploy_builds_local_artifacts_before_packaging_runtime_image`
Expected: FAIL on missing `max-size`.

- [ ] **Step 3: Add logging bounds to every service**

Add to `postgres`, `redis`, and `gateway`:

```yaml
    logging:
      driver: json-file
      options:
        max-size: "50m"
        max-file: "5"
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test deploy_builds_local_artifacts_before_packaging_runtime_image`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docker-compose.yml tests/scripts.rs
git commit -m "chore(deploy): bound docker runtime logs"
```

---

## Phase 2: Credential-Failure Visibility

### Task 3: Show Route Health In Upstream List

**Files:**
- Modify: `frontend/src/types/index.ts:88-129`
- Modify: `frontend/src/views/admin/Upstreams.vue:138-203,426-471`
- Test: `frontend/tests/views/admin-ui.spec.ts:175-206`

- [ ] **Step 1: Write failing regression assertions**

Add to the existing visible-column test:

```typescript
expect(upstream).toContain("key: 'route_health', label: '路由健康'")
expect(upstream).toContain('formatRouteFailureClasses(row.route_health)')
expect(upstream).toContain('formatRouteCooldown(row.route_health)')
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm --prefix frontend test -- tests/views/admin-ui.spec.ts`
Expected: FAIL because column and helpers are absent.

- [ ] **Step 3: Extend frontend types**

Add:

```typescript
export interface RouteHealthSnapshot {
  healthy_routes: number
  cooldown_routes: number
  half_open_routes: number
  legacy_local_admission_poisoned_routes: number
  earliest_retry_after_seconds?: number | null
  failure_classes?: Record<string, number>
}
```

Add to `UpstreamConfig`:

```typescript
  route_health?: RouteHealthSnapshot
```

- [ ] **Step 4: Add visible column and formatting helpers**

Add to `tableColumns` after `status`:

```typescript
  { key: 'route_health', label: '路由健康' },
```

Exclude it from defaults by extending the existing filter:

```typescript
.filter(key =>
  key !== 'base_url' &&
  key !== 'supported_models' &&
  key !== 'key_concurrency' &&
  key !== 'route_health'
)
```

Insert a column between `status` and `priority`:

```vue
<el-table-column v-if="isColumnVisible('route_health')" label="路由健康" min-width="180">
  <template #default="{ row }">
    <el-tooltip
      :content="formatRouteFailureClasses(row.route_health)"
      :disabled="!formatRouteFailureClasses(row.route_health)"
      placement="top"
    >
      <span>
        <el-tag type="warning" size="small">
          冷却 {{ row.route_health?.cooldown_routes ?? 0 }}
        </el-tag>
        <span v-if="formatRouteCooldown(row.route_health)">
          {{ formatRouteCooldown(row.route_health) }}
        </span>
      </span>
    </el-tooltip>
  </template>
</el-table-column>
```

Add helpers near `formatModelList`:

```typescript
const failureClassLabels: Record<string, string> = {
  credentials: '凭证失败',
  rate_limited: '限流',
  key_quota: 'Key 配额',
  capacity_unavailable: '容量不足',
  transient_server: '临时故障',
  transport: '网络失败',
  concurrency_saturated: '并发饱和'
}

const formatRouteFailureClasses = (health?: UpstreamConfig['route_health']) => {
  const entries = Object.entries(health?.failure_classes ?? {})
    .filter(([key]) => key in failureClassLabels)
  if (entries.length === 0) return ''
  return entries.map(([key, count]) => `${failureClassLabels[key]} ${count}`).join('，')
}

const formatRouteCooldown = (health?: UpstreamConfig['route_health']) => {
  const seconds = health?.earliest_retry_after_seconds
  if (!seconds || seconds <= 0) return ''
  if (seconds < 60) return `${seconds} 秒后恢复`
  if (seconds < 3600) return `${Math.ceil(seconds / 60)} 分钟后恢复`
  return `${(seconds / 3600).toFixed(1)} 小时后恢复`
}
```

- [ ] **Step 5: Run tests and type check**

Run:
```bash
npm --prefix frontend test -- tests/views/admin-ui.spec.ts
npm --prefix frontend run type-check
```
Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/types/index.ts frontend/src/views/admin/Upstreams.vue frontend/tests/views/admin-ui.spec.ts
git commit -m "feat(frontend): surface upstream route health"
```

### Task 4: Add Failing-Credentials Filter

**Files:**
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Test: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Write failing test**

```typescript
it('filters upstreams with credential failures', () => {
  const page = source('views/admin/Upstreams.vue')

  expect(page).toContain("credentials: 'failing'")
  expect(page).toContain('hasCredentialFailure')
  expect(page).toContain('凭证失败')
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm --prefix frontend test -- tests/views/admin-ui.spec.ts`
Expected: FAIL.

- [ ] **Step 3: Implement the filter**

Extend `filters`:

```typescript
const filters = ref({
  status: 'all',
  protocol: 'all',
  credentials: 'all',
  search: ''
})
```

Add a selector near the existing filters:

```vue
<el-select v-model="filters.credentials" clearable placeholder="凭证状态">
  <el-option label="凭证失败" value="failing" />
</el-select>
```

Add:

```typescript
const hasCredentialFailure = (row: UpstreamConfig) =>
  (row.route_health?.failure_classes?.credentials ?? 0) > 0
```

Modify the existing `filteredUpstreams` computed at `frontend/src/views/admin/Upstreams.vue:647` by adding this condition before the search predicate:

```typescript
    if (filters.value.credentials === 'failing' && !hasCredentialFailure(item)) {
      return false
    }
```

Do not create a second computed list.

- [ ] **Step 4: Run tests and type check**

Run:
```bash
npm --prefix frontend test -- tests/views/admin-ui.spec.ts
npm --prefix frontend run type-check
```
Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/views/admin/Upstreams.vue frontend/tests/views/admin-ui.spec.ts
git commit -m "feat(frontend): filter credentials-failing upstreams"
```

### Task 5: Add CI Quality Gates

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `Cargo.toml`
- Test: `frontend/tests/ci-workflow.spec.ts`

- [ ] **Step 1: Write failing workflow test**

Create:

```typescript
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

describe('CI workflow', () => {
  it('runs frontend and backend quality gates', () => {
    const workflow = readFileSync(
      new URL('../../../.github/workflows/ci.yml', import.meta.url),
      'utf8'
    )

    expect(workflow).toContain('npm --prefix frontend ci')
    expect(workflow).toContain('npm --prefix frontend test')
    expect(workflow).toContain('npm --prefix frontend run type-check')
    expect(workflow).toContain('cargo fmt --all --check')
    expect(workflow).toContain('cargo clippy --all-targets -- -D warnings')
    expect(workflow).toContain('cargo test --all')
  })
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm --prefix frontend test -- tests/ci-workflow.spec.ts`
Expected: FAIL because workflow is absent.

- [ ] **Step 3: Format current Rust code**

Run:
```bash
cargo fmt --all
cargo fmt --all --check
```
Expected: formatting check PASS.

- [ ] **Step 4: Add clippy gate and workflow**

Add to root `Cargo.toml`:

```toml
[lints.rust]
unsafe_code = "forbid"
```

Create `.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  frontend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm
          cache-dependency-path: frontend/package-lock.json
      - run: npm --prefix frontend ci
      - run: npm --prefix frontend test
      - run: npm --prefix frontend run type-check

  backend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test --all
```

- [ ] **Step 5: Run local equivalents**

Run:
```bash
npm --prefix frontend test
npm --prefix frontend run type-check
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```
Expected: all PASS. Fix concrete findings only; do not add broad `#[allow]`.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml Cargo.toml Cargo.lock frontend/tests/ci-workflow.spec.ts
git add $(git diff --name-only -- '*.rs')
git commit -m "chore(ci): add frontend and backend quality gates"
```

---

## Phase 3: Deployment Verification

### Task 6: Wait For Health And Verify Deployed Assets

**Files:**
- Modify: `scripts/deploy.sh:141-159`
- Test: `tests/scripts.rs`

- [ ] **Step 1: Write failing deploy-script test**

Add a test modeled on `deploy_builds_local_artifacts_before_packaging_runtime_image`, with:
- copied deploy scripts, compose, env example, and frontend package lock;
- fake `npm` that creates `frontend/dist/assets/Upstreams-test.js`;
- fake `cargo` that creates the release binary;
- fake `docker` that records compose calls to `$DEPLOY_TRACE`, returns healthy from `inspect`, and supports build;
- fake `curl` that returns `index assets/Upstreams-test.js` for `/`, `ok` for `/healthz`, and `key_concurrency` for the Upstreams chunk.

Invoke:

```rust
Command::new("bash")
    .arg("scripts/deploy.sh")
    .arg("--deploy-dir")
    .arg(&deploy_dir)
    .arg("--skip-build")
    .current_dir(&repo_root)
    .env("PATH", format!("{}:{inherited_path}", fake_bin.display()))
    .env("DEPLOY_TRACE", &trace)
    .output()
```

Assert success and assert trace order:

```rust
let trace_text = fs::read_to_string(&trace).unwrap();
let compose_up = trace_text.find("up\t-d\t--remove-orphans").unwrap();
let compose_ps = trace_text.find("ps").unwrap();
assert!(compose_up < compose_ps);
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test deploy_waits_for_container_health_before_reporting_success`
Expected: FAIL because current deploy does no health or asset verification.

- [ ] **Step 3: Implement verification**

After `compose up -d`, add:

```bash
wait_for_gateway_health() {
  local container="${GATEWAY_CONTAINER_NAME:-chat-responses-codex}"
  local port="${GATEWAY_HEALTH_PORT:-3000}"
  local health attempt

  for attempt in $(seq 1 30); do
    health=$(docker inspect --format '{{.State.Health.Status}}' "$container" 2>/dev/null || true)
    printf '[%s] Gateway health attempt %s/30: %s\n' \
      "$(date '+%Y-%m-%d %H:%M:%S')" "$attempt" "${health:-unknown}"
    if [[ "$health" == healthy ]] &&
       [[ "$(curl --noproxy '*' -fsS "http://127.0.0.1:${port}/healthz" 2>/dev/null || true)" == ok ]]; then
      return 0
    fi
    sleep 2
  done

  echo "Error: gateway failed health verification" >&2
  return 1
}

verify_deployed_frontend() {
  local port="${GATEWAY_HEALTH_PORT:-3000}"
  local index asset main upstream_asset

  index=$(curl --noproxy '*' -fsS "http://127.0.0.1:${port}/") || return 1
  asset=$(printf '%s' "$index" | grep -o 'assets/index-[^" ]*\.js' | head -n1)
  [[ -n "$asset" ]] || { echo "Error: deployed index has no main asset" >&2; return 1; }
  main=$(curl --noproxy '*' -fsS "http://127.0.0.1:${port}/${asset}") || return 1
  upstream_asset=$(printf '%s' "$main" | grep -o 'assets/Upstreams-[^" ]*\.js' | head -n1)
  [[ -n "$upstream_asset" ]] || { echo "Error: deployed main asset has no Upstreams chunk" >&2; return 1; }
  curl --noproxy '*' -fsS "http://127.0.0.1:${port}/${upstream_asset}" |
    grep -F -q 'key_concurrency' || {
      echo "Error: deployed Upstreams chunk is stale" >&2
      return 1
    }
}

wait_for_gateway_health
verify_deployed_frontend
```

Keep `compose ps` after both checks.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test deploy_waits_for_container_health_before_reporting_success`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add scripts/deploy.sh tests/scripts.rs
git commit -m "chore(deploy): verify gateway health and deployed frontend"
```

---

## Phase 4: List-Column UX

### Task 7: Checkbox Column Settings With Component Tests

**Files:**
- Modify: `frontend/package.json`
- Modify: `frontend/vite.config.ts`
- Modify: `frontend/src/components/TableColumnSettings.vue`
- Test: `frontend/src/components/TableColumnSettings.spec.ts`

- [ ] **Step 1: Add dependencies**

Run:
```bash
npm --prefix frontend install -D @vue/test-utils@2 happy-dom
```

- [ ] **Step 2: Write failing component test**

Create `frontend/src/components/TableColumnSettings.spec.ts`:

```typescript
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import TableColumnSettings from './TableColumnSettings.vue'

const columns = [
  { key: 'name', label: '名称' },
  { key: 'base_url', label: 'Base URL' },
  { key: 'remark', label: '备注' }
]

const render = () => mount(TableColumnSettings, {
  props: {
    columns,
    modelValue: ['name'],
    defaultKeys: ['name', 'remark']
  },
  global: {
    stubs: {
      ElPopover: { template: '<div><slot /><slot name="reference" /></div>' }
    }
  }
})

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn()
  }))
})

describe('TableColumnSettings', () => {
  it('renders one searchable checkbox row per column', async () => {
    const wrapper = render()
    const labels = () => wrapper.findAll('.table-column-option').map(row => row.text())

    expect(labels()).toEqual(['名称', 'Base URL', '备注'])

    await wrapper.find('input.table-column-search__input').setValue('base')
    expect(labels()).toEqual(['Base URL'])
  })

  it('keeps at least one column selected', async () => {
    const wrapper = render()
    const checkboxes = wrapper.findAll('input[type="checkbox"]')
    await checkboxes[0].setValue(false)

    expect(wrapper.emitted('update:modelValue')).toBeUndefined()
  })
})
```

- [ ] **Step 3: Run test to verify it fails**

Run: `npm --prefix frontend test -- src/components/TableColumnSettings.spec.ts`
Expected: FAIL because current UI is a multi-select.

- [ ] **Step 4: Configure DOM test environment**

Add to `frontend/vite.config.ts`:

```typescript
/// <reference types="vitest" />
test: {
  environment: 'happy-dom'
}
```

- [ ] **Step 5: Replace multi-select with searchable checkbox rows**

Template:

```vue
<el-input
  v-model="search"
  size="small"
  clearable
  placeholder="搜索字段"
  class="table-column-search"
/>

<div class="table-column-options" role="group" aria-label="列表展示列设置">
  <el-checkbox
    v-for="column in filteredColumns"
    :key="column.key"
    class="table-column-option"
    :model-value="isSelected(column.key)"
    :label="column.label"
    @change="checked => toggleColumn(column.key, checked === true)"
  />
  <div v-if="filteredColumns.length === 0" class="table-column-empty">没有匹配字段</div>
</div>
```

Script:

```typescript
import { computed, ref } from 'vue'

const search = ref('')
const filteredColumns = computed(() => {
  const query = search.value.trim().toLowerCase()
  if (!query) return props.columns
  return props.columns.filter(column =>
    column.label.toLowerCase().includes(query) ||
    column.key.toLowerCase().includes(query)
  )
})
const isSelected = (key: string) => props.modelValue.includes(key)

const toggleColumn = (key: string, checked: boolean) => {
  if (!checked && props.modelValue.length === 1) return
  const next = checked
    ? orderedKeys([...props.modelValue, key])
    : props.modelValue.filter(item => item !== key)
  emit('update:modelValue', next)
}
```

Styles:

```css
.table-column-options {
  display: grid;
  gap: 8px;
  max-height: 260px;
  overflow-y: auto;
}

.table-column-empty {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
```

- [ ] **Step 6: Run component test, full frontend suite, and type check**

Run:
```bash
npm --prefix frontend test -- src/components/TableColumnSettings.spec.ts
npm --prefix frontend test
npm --prefix frontend run type-check
```
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add frontend/package.json frontend/package-lock.json frontend/vite.config.ts frontend/src/components/TableColumnSettings.vue frontend/src/components/TableColumnSettings.spec.ts
git commit -m "feat(frontend): improve table column settings"
```

---

## Phase 5: Smaller Quality Cleanup

### Task 8: Extract Clipboard Utility

**Files:**
- Create: `frontend/src/utils/clipboard.ts`
- Modify: `frontend/src/views/admin/Downstreams.vue:500-526`
- Modify: `frontend/src/views/portal/Integration.vue:902-927`
- Test: `frontend/tests/utils/clipboard.spec.ts`

- [ ] **Step 1: Write failing utility tests**

```typescript
import { describe, expect, it, vi } from 'vitest'
import { copyTextToClipboard } from '@/utils/clipboard'

describe('copyTextToClipboard', () => {
  it('prefers the clipboard API', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', { clipboard: { writeText } })

    await expect(copyTextToClipboard('secret')).resolves.toBe(true)
    expect(writeText).toHaveBeenCalledWith('secret')
  })

  it('uses the textarea fallback when clipboard API rejects', async () => {
    const writeText = vi.fn().mockRejectedValue(new Error('denied'))
    const textarea = {
      value: '',
      style: {},
      setAttribute: vi.fn(),
      focus: vi.fn(),
      select: vi.fn()
    }
    const execCommand = vi.fn().mockReturnValue(true)
    vi.stubGlobal('navigator', { clipboard: { writeText } })
    vi.stubGlobal('document', {
      createElement: vi.fn().mockReturnValue(textarea),
      body: { appendChild: vi.fn(), removeChild: vi.fn() },
      execCommand
    })

    await expect(copyTextToClipboard('secret')).resolves.toBe(true)
    expect(execCommand).toHaveBeenCalledWith('copy')
  })
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm --prefix frontend test -- tests/utils/clipboard.spec.ts`
Expected: FAIL because module is missing.

- [ ] **Step 3: Implement utility**

```typescript
export const copyTextToClipboard = async (content: string): Promise<boolean> => {
  try {
    await navigator.clipboard.writeText(content)
    return true
  } catch {
    const textarea = document.createElement('textarea')
    textarea.value = content
    textarea.setAttribute('readonly', 'true')
    textarea.style.position = 'fixed'
    textarea.style.opacity = '0'
    textarea.style.pointerEvents = 'none'
    document.body.appendChild(textarea)
    textarea.select()
    try {
      return document.execCommand('copy')
    } finally {
      document.body.removeChild(textarea)
    }
  }
}
```

- [ ] **Step 4: Replace both call sites**

Each caller becomes:

```typescript
if (await copyTextToClipboard(copyableKey)) {
  ElMessage.success('已复制到剪贴板')
} else {
  ElMessage.error('复制失败，请手动复制')
}
```

- [ ] **Step 5: Run tests and type check**

Run:
```bash
npm --prefix frontend test
npm --prefix frontend run type-check
```
Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/utils/clipboard.ts frontend/tests/utils/clipboard.spec.ts frontend/src/views/admin/Downstreams.vue frontend/src/views/portal/Integration.vue
git commit -m "refactor(frontend): share clipboard fallback"
```

### Task 9: Pause Downstream Runtime Polling When Hidden

**Files:**
- Modify: `frontend/src/views/admin/Downstreams.vue:812-824`
- Test: `frontend/tests/views/admin-ui.spec.ts`

- [ ] **Step 1: Write failing test**

```typescript
it('pauses downstream runtime polling while hidden', () => {
  const page = source('views/admin/Downstreams.vue')

  expect(page).toContain("document.addEventListener('visibilitychange'")
  expect(page).toContain('isDocumentVisible')
  expect(page).toContain('clearRuntimeTimer')
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm --prefix frontend test -- tests/views/admin-ui.spec.ts`
Expected: FAIL.

- [ ] **Step 3: Implement visibility-aware polling**

```typescript
const isDocumentVisible = () =>
  typeof document === 'undefined' || document.visibilityState === 'visible'

const clearRuntimeTimer = () => {
  if (runtimeTimer !== null) {
    clearInterval(runtimeTimer)
    runtimeTimer = null
  }
}

const startRuntimeTimer = () => {
  if (runtimeTimer === null && isDocumentVisible()) {
    runtimeTimer = window.setInterval(loadRuntime, 5000)
  }
}

const handleRuntimeVisibility = () => {
  if (isDocumentVisible()) {
    void loadRuntime()
    startRuntimeTimer()
  } else {
    clearRuntimeTimer()
  }
}

onMounted(() => {
  loadData()
  loadModels()
  loadRuntime()
  startRuntimeTimer()
  document.addEventListener('visibilitychange', handleRuntimeVisibility)
})

onUnmounted(() => {
  clearRuntimeTimer()
  document.removeEventListener('visibilitychange', handleRuntimeVisibility)
})
```

- [ ] **Step 4: Run tests and type check**

Run:
```bash
npm --prefix frontend test
npm --prefix frontend run type-check
```
Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/views/admin/Downstreams.vue frontend/tests/views/admin-ui.spec.ts
git commit -m "perf(frontend): pause hidden downstream polling"
```

---

## Final Verification

- [ ] Run `npm --prefix frontend test`
- [ ] Run `npm --prefix frontend run type-check`
- [ ] Run `cargo test --all`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo clippy --all-targets -- -D warnings`
- [ ] Run `scripts/deploy.sh`
- [ ] Verify `docker inspect --format '{{.State.Health.Status}}' chat-responses-codex` returns `healthy`
- [ ] Verify `curl --noproxy '*' -fsS http://127.0.0.1:3000/healthz` returns `ok`
- [ ] Verify rotated files appear under `~/docker/chat-responses-codex/logs/`

## Explicitly Deferred

- Server-side per-admin table preferences
- Drag-and-drop column ordering
- Automatic upstream disabling after credential failures
- Gateway/state module decomposition
- Token refresh/revocation redesign
