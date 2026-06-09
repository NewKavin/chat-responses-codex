# 公告功能设计

## 目标

新增一个全局公告功能：管理员可以发布公告，门户用户登录后看到弹窗；用户确认当前公告后，同一个用户在同一个浏览器里不再重复弹出，直到管理员发布新的公告版本。

## 范围

本功能包含：

- 管理后台的公告管理页面。
- 网关状态中的全局公告配置。
- 门户用户登录后的公告读取。
- 基于浏览器本地存储的用户已读标记。
- 文件模式和 PostgreSQL 模式持久化。
- 覆盖主要行为的后端和前端自动化测试。

本功能不包含：

- 按下游用户或团队定向发布公告。
- 必须确认后才能继续使用的强制公告。
- 富文本或 Markdown 渲染。
- 服务端按用户记录已读状态。
- 用户在线时通过 websocket 或推送即时弹出。

## 产品行为

管理员一次管理一个全局公告。公告包含标题、正文、等级、启用状态、版本 ID 和更新时间。

门户用户进入已登录门户后，如果当前存在启用公告，并且该用户尚未确认当前公告版本，则页面显示 Element Plus 弹窗。用户点击“我知道了”后，前端把当前公告 ID 写入浏览器本地存储，存储 key 按门户工号隔离。

管理员每次保存发布公告时，后端生成新的公告 ID。公告 ID 变化后，之前确认过旧公告的用户会在下一次进入或刷新门户时再次看到新公告。

如果公告未启用、标题为空或正文为空，用户端不弹窗。

## 数据模型

在 `PersistedState` 中新增可选全局公告：

```rust
pub struct AnnouncementConfig {
    pub id: String,
    pub title: String,
    pub content: String,
    pub level: AnnouncementLevel,
    pub active: bool,
    pub updated_at: u64,
}

pub enum AnnouncementLevel {
    Info,
    Success,
    Warning,
    Error,
}

pub struct PersistedState {
    pub upstreams: Vec<UpstreamConfig>,
    pub downstreams: Vec<DownstreamConfig>,
    pub usage_logs: Vec<UsageLog>,
    #[serde(default)]
    pub announcement: Option<AnnouncementConfig>,
}
```

序列化必须兼容旧状态文件。旧文件缺少 `announcement` 字段时，表示当前没有配置公告。

公告 ID 由服务端在每次管理员成功保存时生成，使用 UUID 字符串。更新时间使用现有 `unix_seconds()`。

## 持久化

文件模式把公告直接写入现有 JSON 状态文件中的 `PersistedState.announcement`。

PostgreSQL 模式新增一个单行配置表：

```sql
CREATE TABLE IF NOT EXISTS app_announcements (
    singleton_id TEXT PRIMARY KEY,
    announcement_id TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    level TEXT NOT NULL,
    active BOOLEAN NOT NULL,
    updated_at BIGINT NOT NULL
);
```

固定使用 `singleton_id = 'global'`。加载状态时把这行读入 `PersistedState.announcement`。保存配置状态时，如果公告存在则 upsert 该行；如果公告为 `None`，则删除该行。

## 后端 API

新增管理员接口，沿用现有 admin auth middleware：

- `GET /api/admin/announcement`
- `PUT /api/admin/announcement`

`GET /api/admin/announcement` 返回：

```json
{
  "announcement": {
    "id": "uuid",
    "title": "系统公告",
    "content": "公告正文",
    "level": "info",
    "active": true,
    "updated_at": 1710000000
  }
}
```

如果没有公告，返回：

```json
{
  "announcement": null
}
```

`PUT /api/admin/announcement` 接收：

```json
{
  "title": "系统公告",
  "content": "公告正文",
  "level": "info",
  "active": true
}
```

校验规则：

- `title` trim 后最多 120 个字符。
- `content` trim 后最多 5000 个字符。
- `level` 只能是 `info`、`success`、`warning`、`error`。
- `active=true` 时，标题和正文都不能为空。
- `active=false` 时，可以保留草稿内容。

保存成功后，后端生成新的公告 ID，持久化公告，并返回已保存的公告。

新增门户接口，沿用现有 portal bearer token 校验：

- `GET /api/portal/announcement`

接口先校验门户 Bearer token，再返回当前启用公告或 `null`。该接口不能暴露未启用的草稿公告。

## 管理后台 UI

新增管理后台路由和侧边栏菜单项：

- 路由：`/admin/announcement`
- 菜单文案：`公告管理`
- 组件：`frontend/src/views/admin/Announcement.vue`

页面沿用现有 Element Plus 风格：

- `el-card` 作为页面容器。
- `el-form` 编辑标题、正文、等级和启用开关。
- `el-input` 编辑标题。
- `el-input type="textarea"` 编辑正文。
- `el-select` 选择等级。
- `el-switch` 控制启用状态。
- 主按钮用于保存并发布。

页面挂载时读取当前公告。保存时调用 `PUT /api/admin/announcement`。保存成功后显示成功提示，并用返回结果更新本地表单中的公告 ID 和更新时间。

页面展示当前公告版本 ID 和更新时间，让管理员明确每次保存都会生成一个新版本。

## 门户 UI

在 `frontend/src/views/portal/Portal.vue` 中，在 `extractEmployeeId()` 成功后读取公告。

门户展示规则：

- 进入已认证门户后调用 `portalApi.getAnnouncement()`。
- 如果返回公告为 `null`，不做任何处理。
- 本地存储 key 为 `portal_announcement_seen:<employee_id>`。
- 如果本地存储值等于当前 `announcement.id`，不弹窗。
- 如果本地存储值不存在或不是当前公告 ID，显示 `el-dialog`。
- 用户点击“我知道了”后，把当前公告 ID 写入本地存储并关闭弹窗。

弹窗使用公告标题和正文作为纯文本展示，不渲染 HTML。这可以避免 XSS 风险，也保持第一版实现简单。

## 前端 API 与类型

新增前端类型：

```ts
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

新增管理员 API：

- `adminApi.getAnnouncement()`
- `adminApi.updateAnnouncement(data)`

新增门户 API：

- `portalApi.getAnnouncement()`

## 错误处理

管理员保存失败时，前端用 `ElMessage.error` 展示 API 返回的错误信息。后端校验失败返回 HTTP 400 和 JSON 错误消息。

门户公告读取失败不能阻塞门户使用。前端把错误打印到浏览器 console，并继续渲染正常门户内容。

如果浏览器本地存储不可用，门户仍然显示公告，并允许用户关闭当前弹窗；但刷新后可能再次显示。

## 测试

后端测试：

- 管理员在未配置公告时读取到 `null`。
- 管理员可以发布启用公告，并收到服务端生成的公告 ID。
- 再次发布公告会改变公告 ID。
- 管理员不能发布标题或正文为空的启用公告。
- 门户认证用户可以读取当前启用公告。
- 公告未启用时，门户接口返回 `null`。
- 不包含 `announcement` 字段的旧状态文件可以正常反序列化。
- PostgreSQL 保存和加载可以保留公告配置。

前端测试：

- 管理员 API 方法调用正确 endpoint。
- 门户公告已读 key 按工号隔离。
- 当前公告 ID 未被确认时，门户显示弹窗。
- 本地存储中的公告 ID 与当前公告一致时，门户不显示弹窗。

## 发布说明

这个功能对现有部署向后兼容。文件模式的旧状态文件不需要迁移。PostgreSQL 部署通过现有初始化路径新增一个小型配置表。

第一版使用浏览器本地已读标记。如果后续需要跨设备一致的已读状态，可以新增服务端表，按下游 ID 和公告 ID 记录确认状态，不需要改变公告读取 API 的返回结构。
