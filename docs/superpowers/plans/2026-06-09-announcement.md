# 公告功能 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给管理员提供一个全局公告发布入口，并让门户用户登录后按浏览器和员工工号维度弹出公告，管理员每次发布新版本后旧确认状态自动失效。

**Architecture:** 后端把公告作为 `PersistedState` 的一部分，文件模式直接写回 JSON，PostgreSQL 模式通过单行 `app_announcements` 表持久化。HTTP 层提供管理员读写接口和门户只读接口，前端把公告编辑、公告弹窗和本地已读标记拆开，分别通过 API 层和纯函数 helper 做最小耦合。

**Tech Stack:** Rust, Axum, Tokio, PostgreSQL, Vue 3, Vue Router, Element Plus, Vitest, existing integration tests.

---

## File Structure

- Modify: `src/state.rs`
  Add `AnnouncementConfig` and `AnnouncementLevel`, extend `PersistedState`, and add state mutation/access helpers for reading and saving公告。
- Modify: `src/state/file_store.rs`
  Persist `PersistedState.announcement` into the file-backed JSON config.
- Modify: `src/state/postgres.rs`
  Add schema, load, upsert, and delete logic for the single-row `app_announcements` table.
- Modify: `src/server.rs`
  Register `/api/admin/announcement` and `/api/portal/announcement`, plus the request/response types and validation flow.
- Create: `tests/announcement_api.rs`
  Cover admin GET/PUT, validation errors, announcement version changes, portal visibility, and inactive announcement hiding.
- Modify: `tests/state_store.rs`
  Verify old JSON files without `announcement` still deserialize and file persistence keeps the announcement payload.
- Modify: `tests/postgres_roundtrip.rs`
  Verify PostgreSQL roundtrip preserves announcement state and updates the stored version id.
- Modify: `frontend/src/types/index.ts`
  Add announcement types shared by admin and portal code.
- Modify: `frontend/src/api/admin.ts`
  Add admin announcement API methods and export the underlying axios client for endpoint assertions.
- Modify: `frontend/src/api/portal.ts`
  Add portal announcement API method and export the underlying axios client for endpoint assertions.
- Modify: `frontend/src/api/admin.spec.ts`
  Add endpoint assertions for `getAnnouncement()` and `updateAnnouncement()`.
- Create: `frontend/src/api/portal.spec.ts`
  Assert `portalApi.getAnnouncement()` hits the correct endpoint.
- Create: `frontend/src/utils/announcement.ts`
  Factor localStorage key generation and display decision helpers out of the portal page.
- Create: `frontend/src/utils/announcement.spec.ts`
  Cover key generation and seen/unseen decision logic.
- Create: `frontend/src/views/admin/Announcement.vue`
  Add the Element Plus management page for editing and publishing公告.
- Modify: `frontend/src/router/index.ts`
  Register `/admin/announcement` behind admin auth.
- Modify: `frontend/src/App.vue`
  Add the sidebar entry for公告管理.
- Modify: `frontend/src/views/portal/Portal.vue`
  Fetch公告 after portal login, show the modal, and persist the seen announcement id in localStorage.

### Task 1: Add announcement data model and persistence

**Files:**
- Modify: `src/state.rs`
- Modify: `src/state/file_store.rs`
- Modify: `src/state/postgres.rs`
- Modify: `tests/state_store.rs`
- Modify: `tests/postgres_roundtrip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn persisted_state_without_announcement_still_deserializes() {
    let raw = serde_json::json!({
        "upstreams": [],
        "downstreams": [],
        "usage_logs": []
    });

    let state: PersistedState = serde_json::from_value(raw).unwrap();
    assert!(state.announcement.is_none());
}

#[tokio::test]
async fn file_store_persists_announcement_payload() {
    let tempdir = tempfile::tempdir().unwrap();
    let state_path = tempdir.path().join("state.json");
    let announcement = AnnouncementConfig {
        id: "ann-1".into(),
        title: "系统公告".into(),
        content: "请今天完成发布检查".into(),
        level: AnnouncementLevel::Warning,
        active: true,
        updated_at: 1_710_000_000,
    };
    let state = AppState::new(
        PersistedState {
            upstreams: vec![],
            downstreams: vec![],
            usage_logs: vec![],
            announcement: Some(announcement.clone()),
        },
        state_path.clone(),
        AppConfig::default(),
    );

    state.persist().await.unwrap();

    let persisted: PersistedState = serde_json::from_slice(&tokio::fs::read(state_path).await.unwrap()).unwrap();
    assert_eq!(persisted.announcement, Some(announcement));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test --test state_store persisted_state_without_announcement_still_deserializes -- --exact`

Expected: FAIL because `PersistedState` does not yet expose `announcement`.

Run: `rtk cargo test --test state_store file_store_persists_announcement_payload -- --exact`

Expected: FAIL because file persistence still drops `announcement`.

- [ ] **Step 3: Write the minimal implementation**

```rust
// src/state.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnouncementLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncementConfig {
    pub id: String,
    pub title: String,
    pub content: String,
    pub level: AnnouncementLevel,
    pub active: bool,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    pub upstreams: Vec<UpstreamConfig>,
    pub downstreams: Vec<DownstreamConfig>,
    pub usage_logs: Vec<UsageLog>,
    #[serde(default)]
    pub announcement: Option<AnnouncementConfig>,
}
```

```rust
// src/state/file_store.rs
let bytes = serde_json::to_vec_pretty(&PersistedState {
    upstreams: state.upstreams.clone(),
    downstreams: state.downstreams.clone(),
    usage_logs: Vec::new(),
    announcement: state.announcement.clone(),
})
```

```rust
// src/state/postgres.rs
async fn sync_announcements(tx: &Transaction<'_>, announcement: &Option<AnnouncementConfig>) -> io::Result<()> {
    match announcement {
        Some(announcement) => {
            let level = match announcement.level {
                AnnouncementLevel::Info => "info",
                AnnouncementLevel::Success => "success",
                AnnouncementLevel::Warning => "warning",
                AnnouncementLevel::Error => "error",
            };
            tx.execute(
                "INSERT INTO app_announcements (
                    singleton_id, announcement_id, title, content, level, active, updated_at
                ) VALUES ('global', $1, $2, $3, $4, $5, $6)
                ON CONFLICT (singleton_id) DO UPDATE SET
                    announcement_id = EXCLUDED.announcement_id,
                    title = EXCLUDED.title,
                    content = EXCLUDED.content,
                    level = EXCLUDED.level,
                    active = EXCLUDED.active,
                    updated_at = EXCLUDED.updated_at",
                &[&announcement.id, &announcement.title, &announcement.content, &level, &announcement.active, &(announcement.updated_at as i64)],
            )
            .await
            .map_err(io_other)?;
        }
        None => {
            tx.execute("DELETE FROM app_announcements WHERE singleton_id = 'global'", &[])
                .await
                .map_err(io_other)?;
        }
    }
    Ok(())
}

async fn load_announcement(conn: &tokio_postgres::Client) -> io::Result<Option<AnnouncementConfig>> {
    let row = conn
        .query_opt(
            "SELECT announcement_id, title, content, level, active, updated_at
             FROM app_announcements
             WHERE singleton_id = 'global'",
            &[],
        )
        .await
        .map_err(io_other)?;

    let Some(row) = row else {
        return Ok(None);
    };

    let level_text: String = row.get(3);
    let level = match level_text.as_str() {
        "info" => AnnouncementLevel::Info,
        "success" => AnnouncementLevel::Success,
        "warning" => AnnouncementLevel::Warning,
        "error" => AnnouncementLevel::Error,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid announcement level: {other}"),
            ))
        }
    };

    Ok(Some(AnnouncementConfig {
        id: row.get(0),
        title: row.get(1),
        content: row.get(2),
        level,
        active: row.get(4),
        updated_at: row.get::<_, i64>(5).max(0) as u64,
    }))
}
```

Wire `sync_announcements(tx, &state.announcement).await?;` into `sync_config_tables`, and in `load_state` read `let announcement = load_announcement(&conn).await?;` before constructing `PersistedState`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --test state_store -- --nocapture`

Expected: both state-store tests pass.

Run: `rtk cargo test --test postgres_roundtrip -- --nocapture`

Expected: the announcement roundtrip assertion passes against `PG_TEST_DATABASE_URL` when it is set, and the test prints a skip message instead of failing when it is not.

- [ ] **Step 5: Commit**

```bash
rtk git add src/state.rs src/state/file_store.rs src/state/postgres.rs tests/state_store.rs tests/postgres_roundtrip.rs
rtk git commit -m "feat: persist global announcements"
```

### Task 2: Add admin and portal announcement endpoints

**Files:**
- Modify: `src/server.rs`
- Create: `tests/announcement_api.rs`

- [ ] **Step 1: Write the failing tests**

Add three local helpers at the top of `tests/announcement_api.rs`: `create_test_state_without_announcement`, `create_test_state_with_draft_announcement`, and `put_announcement`. Copy the existing admin-login token helper pattern from `tests/admin_dashboard.rs` so the new file can log in without extra setup.

```rust
#[tokio::test]
async fn admin_announcement_get_returns_null_when_missing() {
    let (state, _) = create_test_state_without_announcement();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["announcement"].is_null());
}

#[tokio::test]
async fn admin_announcement_put_generates_new_version_id() {
    let (state, _) = create_test_state_without_announcement();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let first = put_announcement(&app, &token, "系统公告", "第一版", "info", true).await;
    let second = put_announcement(&app, &token, "系统公告", "第二版", "info", true).await;

    assert_ne!(first["id"], second["id"]);
    assert_eq!(second["active"], true);
}

#[tokio::test]
async fn portal_announcement_hides_inactive_drafts() {
    let (state, portal_key) = create_test_state_with_draft_announcement();
    let app = chat_responses_codex::server::build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/portal/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", portal_key))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["announcement"].is_null());
}

#[tokio::test]
async fn admin_announcement_rejects_blank_title_when_active() {
    let (state, _) = create_test_state_without_announcement();
    let app = chat_responses_codex::server::build_router(state);
    let token = get_admin_token(&app, "admin", "admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/announcement")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "title": "   ",
                        "content": "正文",
                        "level": "info",
                        "active": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test --test announcement_api admin_announcement_get_returns_null_when_missing -- --exact`

Expected: FAIL because the route does not exist yet.

Run: `rtk cargo test --test announcement_api admin_announcement_put_generates_new_version_id -- --exact`

Expected: FAIL because the PUT handler does not exist yet.

Run: `rtk cargo test --test announcement_api portal_announcement_hides_inactive_drafts -- --exact`

Expected: FAIL because the portal announcement route does not exist yet.

- [ ] **Step 3: Write the minimal implementation**

```rust
// src/server.rs
.route(
    "/api/admin/announcement",
    get(admin_get_announcement)
        .put(admin_update_announcement)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        )),
)
.route("/api/portal/announcement", get(portal_announcement))
```

```rust
#[derive(Debug, Deserialize)]
struct AnnouncementUpdateRequest {
    title: String,
    content: String,
    level: AnnouncementLevel,
    active: bool,
}

async fn admin_get_announcement(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({ "announcement": state.snapshot().await.announcement })).into_response()
}

async fn admin_update_announcement(
    State(state): State<AppState>,
    Json(body): Json<AnnouncementUpdateRequest>,
) -> impl IntoResponse {
    let title = body.title.trim().to_string();
    let content = body.content.trim().to_string();
    if body.active && (title.is_empty() || content.is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "Active announcement requires non-empty title and content" } })),
        )
            .into_response();
    }

    let announcement = AnnouncementConfig {
        id: Uuid::new_v4().to_string(),
        title,
        content,
        level: body.level,
        active: body.active,
        updated_at: unix_seconds(),
    };

    if let Err(error) = state.update_announcement(Some(announcement.clone())).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": { "message": format!("Failed to save announcement: {error}") } })),
        )
            .into_response();
    }

    Json(json!({ "announcement": announcement })).into_response()
}

async fn portal_announcement(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let _downstream_id = match extract_downstream_id_from_bearer(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let announcement = state
        .snapshot()
        .await
        .announcement
        .filter(|announcement| announcement.active && !announcement.title.trim().is_empty() && !announcement.content.trim().is_empty());

    Json(json!({ "announcement": announcement })).into_response()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --test announcement_api -- --nocapture`

Expected: all announcement API cases pass.

- [ ] **Step 5: Commit**

```bash
rtk git add src/server.rs tests/announcement_api.rs
rtk git commit -m "feat: add announcement api"
```

### Task 3: Wire admin and portal frontend APIs and pure helpers

**Files:**
- Modify: `frontend/src/types/index.ts`
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/api/portal.ts`
- Modify: `frontend/src/api/admin.spec.ts`
- Create: `frontend/src/api/portal.spec.ts`
- Create: `frontend/src/utils/announcement.ts`
- Create: `frontend/src/utils/announcement.spec.ts`

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, expect, it, vi } from 'vitest'
import { adminApi, adminHttp } from './admin'

it('calls the admin announcement endpoint', async () => {
  const spy = vi.spyOn(adminHttp, 'get').mockResolvedValue({ data: { announcement: null } } as never)
  await adminApi.getAnnouncement()
  expect(spy).toHaveBeenCalledWith('/admin/announcement')
})

it('calls the admin announcement update endpoint', async () => {
  const spy = vi.spyOn(adminHttp, 'put').mockResolvedValue({ data: { announcement: null } } as never)
  await adminApi.updateAnnouncement({ title: '系统公告', content: '正文', level: 'info', active: true })
  expect(spy).toHaveBeenCalledWith('/admin/announcement', {
    title: '系统公告',
    content: '正文',
    level: 'info',
    active: true
  })
})
```

```ts
import { describe, expect, it, vi } from 'vitest'
import { portalApi, portalHttp } from './portal'

it('calls the portal announcement endpoint', async () => {
  const spy = vi.spyOn(portalHttp, 'get').mockResolvedValue({ data: { announcement: null } } as never)
  await portalApi.getAnnouncement()
  expect(spy).toHaveBeenCalledWith('/portal/announcement')
})
```

```ts
import { describe, expect, it } from 'vitest'
import { buildAnnouncementSeenKey, shouldShowAnnouncement } from './announcement'

it('builds the announcement seen key from employee id', () => {
  expect(buildAnnouncementSeenKey('team-a')).toBe('portal_announcement_seen:team-a')
})

it('shows the announcement when the seen id differs', () => {
  expect(
    shouldShowAnnouncement(
      { id: 'ann-1', title: '系统公告', content: '正文', level: 'info', active: true, updated_at: 1 },
      'ann-0'
    )
  ).toBe(true)
})
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd frontend && rtk npx vitest run src/api/admin.spec.ts src/api/portal.spec.ts src/utils/announcement.spec.ts`

Expected: FAIL because the new methods and helpers do not exist yet.

- [ ] **Step 3: Write the minimal implementation**

```ts
// frontend/src/types/index.ts
export type AnnouncementLevel = 'info' | 'success' | 'warning' | 'error'

export interface Announcement {
  id: string
  title: string
  content: string
  level: AnnouncementLevel
  active: boolean
  updated_at: number
}
```

```ts
// frontend/src/api/admin.ts
import type { Announcement, AnnouncementLevel } from '@/types'

export const adminHttp = createAdminApiClient()
export const adminApi = {
  getAnnouncement: () => adminHttp.get<{ announcement: Announcement | null }>('/admin/announcement'),
  updateAnnouncement: (data: {
    title: string
    content: string
    level: AnnouncementLevel
    active: boolean
  }) => adminHttp.put<{ announcement: Announcement }>('/admin/announcement', data)
}
```

```ts
// frontend/src/api/portal.ts
import type { Announcement } from '@/types'

export const portalHttp = axios.create({ baseURL: '/api', timeout: 10000 })
export const portalApi = {
  getAnnouncement: () => portalHttp.get<{ announcement: Announcement | null }>('/portal/announcement')
}
```

```ts
// frontend/src/utils/announcement.ts
import type { Announcement } from '@/types'

export const buildAnnouncementSeenKey = (employeeId: string) =>
  `portal_announcement_seen:${employeeId}`

export const shouldShowAnnouncement = (
  announcement: Announcement | null,
  seenAnnouncementId: string | null
) => {
  if (!announcement || !announcement.active) return false
  return seenAnnouncementId !== announcement.id
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd frontend && rtk npx vitest run src/api/admin.spec.ts src/api/portal.spec.ts src/utils/announcement.spec.ts`

Expected: all three Vitest files pass.

- [ ] **Step 5: Commit**

```bash
rtk git add frontend/src/types/index.ts frontend/src/api/admin.ts frontend/src/api/portal.ts frontend/src/api/admin.spec.ts frontend/src/api/portal.spec.ts frontend/src/utils/announcement.ts frontend/src/utils/announcement.spec.ts
rtk git commit -m "feat: add announcement frontend api"
```

### Task 4: Build the admin announcement management page

**Files:**
- Create: `frontend/src/views/admin/Announcement.vue`
- Modify: `frontend/src/router/index.ts`
- Modify: `frontend/src/App.vue`

- [ ] **Step 1: Write the failing test**

```ts
// frontend/src/views/admin/Announcement.vue does not exist yet, so the route import should fail.
// The practical failing check is the build.
```

- [ ] **Step 2: Run the failing check**

Run: `cd frontend && rtk npm run build`

Expected: FAIL because the router points to a missing admin announcement view.

- [ ] **Step 3: Write the minimal implementation**

```vue
<template>
  <div class="announcement-container">
    <el-card>
      <template #header>
        <div class="header">
          <h2>公告管理</h2>
          <el-button type="primary" :loading="saving" @click="handleSave">保存并发布</el-button>
        </div>
      </template>

      <el-form :model="form" label-width="120px">
        <el-form-item label="版本 ID">
          <el-input :model-value="form.id || '保存后自动生成'" disabled />
        </el-form-item>
        <el-form-item label="更新时间">
          <el-input :model-value="formatUpdatedAt(form.updated_at)" disabled />
        </el-form-item>
        <el-form-item label="标题">
          <el-input v-model="form.title" maxlength="120" show-word-limit />
        </el-form-item>
        <el-form-item label="正文">
          <el-input v-model="form.content" type="textarea" :rows="8" maxlength="5000" show-word-limit />
        </el-form-item>
        <el-form-item label="等级">
          <el-select v-model="form.level">
            <el-option label="信息" value="info" />
            <el-option label="成功" value="success" />
            <el-option label="警告" value="warning" />
            <el-option label="错误" value="error" />
          </el-select>
        </el-form-item>
        <el-form-item label="启用">
          <el-switch v-model="form.active" />
        </el-form-item>
      </el-form>
    </el-card>
  </div>
</template>
```

```ts
// frontend/src/router/index.ts
{
  path: '/admin/announcement',
  name: 'AdminAnnouncement',
  component: () => import('@/views/admin/Announcement.vue'),
  meta: { requiresAuth: true }
}
```

```vue
<!-- frontend/src/App.vue -->
<el-menu-item index="/admin/announcement">公告管理</el-menu-item>
```

- [ ] **Step 4: Run the check to verify it passes**

Run: `cd frontend && rtk npm run build`

Expected: build succeeds and the new route chunk is generated.

- [ ] **Step 5: Commit**

```bash
rtk git add frontend/src/views/admin/Announcement.vue frontend/src/router/index.ts frontend/src/App.vue
rtk git commit -m "feat: add admin announcement page"
```

### Task 5: Add the portal announcement modal and localStorage behavior

**Files:**
- Modify: `frontend/src/views/portal/Portal.vue`
- Modify: `frontend/src/utils/announcement.ts`

- [ ] **Step 1: Write the failing test**

```ts
import { describe, expect, it } from 'vitest'
import { buildAnnouncementSeenKey, shouldShowAnnouncement } from './announcement'

it('keeps announcement keys isolated per employee id', () => {
  expect(buildAnnouncementSeenKey('alice')).not.toBe(buildAnnouncementSeenKey('bob'))
})

it('does not show an announcement that was already seen', () => {
  expect(
    shouldShowAnnouncement(
      { id: 'ann-1', title: '系统公告', content: '正文', level: 'info', active: true, updated_at: 1 },
      'ann-1'
    )
  ).toBe(false)
})
```

- [ ] **Step 2: Run the failing check**

Run: `cd frontend && rtk npx vitest run src/utils/announcement.spec.ts`

Expected: FAIL before the helper and portal integration exist.

- [ ] **Step 3: Write the minimal implementation**

```vue
<!-- frontend/src/views/portal/Portal.vue -->
<el-dialog
  v-model="announcementVisible"
  title="公告通知"
  width="600px"
  :show-close="false"
  :close-on-click-modal="false"
  :close-on-press-escape="false"
>
  <h3>{{ announcement?.title }}</h3>
  <div class="announcement-body">{{ announcement?.content }}</div>
  <template #footer>
    <el-button type="primary" @click="handleAcknowledgeAnnouncement">我知道了</el-button>
  </template>
</el-dialog>
```

```ts
// frontend/src/views/portal/Portal.vue
const loadAnnouncement = async () => {
  try {
    const { data } = await portalApi.getAnnouncement()
    announcement.value = data.announcement
    if (!announcement.value) return
    const seenKey = buildAnnouncementSeenKey(employeeId.value)
    const seenAnnouncementId = readLocalStorageValue(seenKey)
    announcementVisible.value = shouldShowAnnouncement(announcement.value, seenAnnouncementId)
  } catch (error) {
    console.error('failed to load announcement', error)
  }
}

const handleAcknowledgeAnnouncement = () => {
  if (!announcement.value) return
  writeLocalStorageValue(buildAnnouncementSeenKey(employeeId.value), announcement.value.id)
  announcementVisible.value = false
}
```

```ts
// frontend/src/utils/announcement.ts
export const readLocalStorageValue = (key: string): string | null => {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

export const writeLocalStorageValue = (key: string, value: string): void => {
  try {
    localStorage.setItem(key, value)
  } catch {
    return
  }
}
```

- [ ] **Step 4: Run the check to verify it passes**

Run: `cd frontend && rtk npx vitest run src/utils/announcement.spec.ts && rtk npm run build`

Expected: helper tests pass and the frontend build succeeds with the modal code included.

- [ ] **Step 5: Commit**

```bash
rtk git add frontend/src/views/portal/Portal.vue frontend/src/utils/announcement.ts
rtk git commit -m "feat: add portal announcement modal"
```

### Task 6: End-to-end deployment and verification

**Files:**
- Modify: any files touched above only if verification exposes a real bug

- [ ] **Step 1: Run the full backend test suite**

Run: `rtk cargo test --workspace`

Expected: all Rust tests pass, including the announcement API, state persistence, and PostgreSQL roundtrip checks.

- [ ] **Step 2: Run the frontend build and unit checks**

Run: `cd frontend && rtk npx vitest run`

Expected: all Vitest checks pass.

Run: `cd frontend && rtk npm run build`

Expected: production frontend build succeeds.

- [ ] **Step 3: Start the full stack locally**

Run backend: `rtk cargo run`

Expected: server listens on `127.0.0.1:3001` and serves `/healthz`.

Run frontend in another terminal: `cd frontend && rtk npm run dev -- --host 127.0.0.1`

Expected: Vite serves the UI on `http://127.0.0.1:5173`.

- [ ] **Step 4: Verify the announcement flow end to end**

1. Log in to the admin UI at `http://127.0.0.1:5173/#/admin/login`.
2. Open `公告管理`, create an active announcement, and save it.
3. Log in to the portal as a downstream user at `http://127.0.0.1:5173/#/portal/login`.
4. Confirm the announcement dialog appears once.
5. Click `我知道了`, refresh the page, and confirm the dialog stays hidden.
6. Change the announcement content in the admin page, save again, refresh the portal, and confirm the dialog appears again because the version id changed.

- [ ] **Step 5: Final commit if verification exposed any adjustments**

```bash
rtk git status --short
rtk git add <any corrected files>
rtk git commit -m "feat: finish announcement end-to-end flow"
```
