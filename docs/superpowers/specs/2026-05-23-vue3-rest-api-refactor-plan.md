# 前后端分离重构计划：Vue 3 + Axum REST API（TDD 模式）

Date: 2026-05-23  
Status: In Progress

## 概述

将当前的 SSR 架构（Leptos SSR）完全重构为前后端分离架构。后端保持核心业务逻辑不变，只提供 REST API（Axum）。前端使用 Vue 3 + Element Plus + ECharts 构建完整的 SPA，包括管理后台和自助门户。所有前端依赖通过 npm 安装并打包到 Docker 镜像中，运行时完全离线。使用 TDD 开发模式，先写测试，再写实现。最终打包成单个 Docker 镜像。

## 关键变更

### 后端变更

**删除 SSR 相关代码（约 2000+ 行）：**

- 删除 crates/gateway-web/ 整个目录
- 删除 src/server.rs 中所有 render_* 函数（16 个函数）
- 删除 src/server.rs 中所有 Html 响应
- 删除 Cargo.toml 中的 workspace.members 中的 crates/gateway-web
- 删除 Cargo.toml 中的 workspace.default-members

**新增 REST API 端点（约 800 行）：**

管理后台 API（/api/admin/*）：

- POST /api/admin/login - 管理员登录，返回 JWT token
- POST /api/admin/logout - 管理员登出
- GET /api/admin/dashboard - 仪表盘数据（上游/下游/日志统计）
- GET /api/admin/upstreams - 上游列表
- POST /api/admin/upstreams - 创建上游
- GET /api/admin/upstreams/:id - 获取上游详情
- PUT /api/admin/upstreams/:id - 更新上游
- DELETE /api/admin/upstreams/:id - 删除上游
- POST /api/admin/upstreams/:id/toggle - 切换上游启用状态
- GET /api/admin/downstreams - 下游列表（支持筛选）
- POST /api/admin/downstreams - 创建下游
- GET /api/admin/downstreams/:id - 获取下游详情
- PUT /api/admin/downstreams/:id - 更新下游
- DELETE /api/admin/downstreams/:id - 删除下游
- POST /api/admin/downstreams/:id/toggle - 切换下游启用状态
- POST /api/admin/downstreams/:id/rotate - 轮换下游密钥
- GET /api/admin/logs - 日志列表（支持筛选和分页）

自助门户 API（/api/portal/*）：

- GET /api/portal/overview - 概览数据（配额摘要、Token 摘要、模型摘要）
- GET /api/portal/quota - 限额详情（每分钟限制、滑动窗口配额、Token 限额、白名单）
- GET /api/portal/usage-history?time_range=7d - 使用历史（每日统计、最近日志）
- GET /api/portal/models - 模型目录（每个模型的使用情况和可用性）

**新增辅助函数（在 src/state.rs 中，约 400 行）：**

- pub fn compute_per_minute_usage(&self, downstream_id: &str) -> PerMinuteUsage
- pub fn compute_request_quota_usage(&self, downstream: &DownstreamConfig) -> Option<RequestQuotaUsage>
- pub fn compute_token_usage(&self, downstream_id: &str, now: u64) -> TokenUsage
- pub fn compute_daily_stats(&self, downstream_id: &str, days: usize) -> Vec<DailyStats>
- pub fn compute_model_stats(&self, downstream: &DownstreamConfig) -> Vec<ModelStats>

**新增 JWT 认证（在 src/auth.rs 中，约 200 行）：**

- pub fn generate_admin_token(username: &str, secret: &str) -> String
- pub fn verify_admin_token(token: &str, secret: &str) -> Result<Claims, JwtError>
- pub async fn admin_auth_middleware(req: Request, next: Next) -> Result<Response, StatusCode>

**静态资源嵌入（在 src/server.rs 中，约 100 行）：**

- 使用 rust-embed 嵌入前端构建产物（frontend/dist/）
- 添加 serve_frontend() 函数处理静态资源和 SPA 路由
- 更新路由配置：API 路由 → 前端 SPA（fallback）

**依赖变更（在 Cargo.toml 中）：**

- 新增 rust-embed = "8.5.0"
- 新增 mime_guess = "2.0"
- 新增 jsonwebtoken = "9.3.0"
- 删除 gateway-core 依赖（合并到主项目）
- 删除 workspace 配置

**不变更的部分：**

- 保持现有的状态管理逻辑（AppState、PersistedState）
- 保持现有的配额计算逻辑（reserve_downstream_request）
- 保持现有的路由逻辑（choose_upstream、select_upstream）
- 保持现有的协议转换逻辑（chat_completions、responses）
- 保持现有的网关核心功能（/v1/models、/v1/chat/completions、/v1/responses）

### 前端变更（全新）

**项目结构：**

```
frontend/
├── src/
│   ├── main.ts
│   ├── App.vue
│   ├── router/index.ts
│   ├── views/
│   │   ├── admin/
│   │   │   ├── Login.vue
│   │   │   ├── Dashboard.vue
│   │   │   ├── Upstreams.vue
│   │   │   ├── Downstreams.vue
│   │   │   └── Logs.vue
│   │   └── portal/
│   │       ├── Portal.vue
│   │       ├── Overview.vue
│   │       ├── QuotaDetails.vue
│   │       ├── UsageHistory.vue
│   │       └── ModelCatalog.vue
│   ├── components/
│   │   ├── admin/
│   │   │   ├── UpstreamForm.vue
│   │   │   ├── DownstreamForm.vue
│   │   │   └── LogTable.vue
│   │   └── portal/
│   │       ├── StatCard.vue
│   │       ├── QuotaProgress.vue
│   │       ├── QuotaStatus.vue
│   │       └── UsageChart.vue
│   ├── api/
│   │   ├── admin.ts
│   │   └── portal.ts
│   ├── types/index.ts
│   ├── stores/auth.ts
│   └── assets/styles/main.css
├── public/
├── index.html
├── package.json
├── tsconfig.json
├── vite.config.ts
└── .gitignore
```

**技术栈：**

- Vue 3（Composition API + <script setup>）
- TypeScript
- Element Plus（企业级 UI 组件库）
- Apache ECharts（企业级图表库）
- Vue Router（Hash 模式）
- Pinia（状态管理，用于存储 JWT token）
- Axios（HTTP 客户端）
- Vite（构建工具）

**核心功能：**

管理后台（/admin/*）：

- 登录页面（JWT 认证）
- 仪表盘（上游/下游/日志统计）
- 上游管理（列表、创建、编辑、删除、切换状态）
- 下游管理（列表、创建、编辑、删除、切换状态、轮换密钥、筛选）
- 日志管理（列表、筛选、分页）

自助门户（/portal/*）：

- 概览（统计卡片、配额状态总览）
- 限额详情（每分钟限制、滑动窗口配额、Token 限额、白名单）
- 使用历史（请求趋势图、Token 使用趋势图、最近请求日志）
- 模型目录（模型列表、使用情况、接入示例）
- 每 30 秒自动刷新数据

### 构建和部署变更

**多阶段 Dockerfile（Dockerfile.multistage）：**

- 阶段 1：Node.js 构建前端（npm ci && npm run build）
- 阶段 2：Rust 构建后端（复制前端构建产物，cargo build --release）
- 阶段 3：最终运行镜像（只包含 Rust 二进制文件）

**docker-compose.yml：**

- 更新 build.dockerfile 为 Dockerfile.multistage

**.gitignore：**

- 新增 frontend/node_modules/
- 新增 frontend/dist/
- 删除 crates/gateway-web/target/

## 测试计划（TDD 模式）

### 后端测试

**测试文件 1：tests/admin_api.rs（约 600 行）**

JWT 认证测试：

- test_admin_login_returns_jwt_token - 验证正确的用户名密码返回 JWT token
- test_admin_login_rejects_invalid_credentials - 验证错误的用户名密码返回 401
- test_admin_api_requires_jwt_token - 验证没有 JWT token 时返回 401
- test_admin_api_rejects_invalid_jwt_token - 验证无效 JWT token 时返回 401

仪表盘 API 测试：

- test_admin_dashboard_returns_statistics - 验证返回正确的统计数据

上游 API 测试：

- test_admin_upstreams_list_returns_all_upstreams - 验证返回所有上游列表
- test_admin_upstreams_create_adds_new_upstream - 验证创建新上游
- test_admin_upstreams_update_modifies_existing_upstream - 验证更新上游
- test_admin_upstreams_delete_removes_upstream - 验证删除上游
- test_admin_upstreams_toggle_changes_active_status - 验证切换上游启用状态

下游 API 测试：

- test_admin_downstreams_list_returns_all_downstreams - 验证返回所有下游列表
- test_admin_downstreams_list_supports_filtering - 验证支持筛选（按状态、生命周期、搜索）
- test_admin_downstreams_create_adds_new_downstream - 验证创建新下游
- test_admin_downstreams_update_modifies_existing_downstream - 验证更新下游
- test_admin_downstreams_delete_removes_downstream - 验证删除下游
- test_admin_downstreams_toggle_changes_active_status - 验证切换下游启用状态
- test_admin_downstreams_rotate_generates_new_key - 验证轮换下游密钥

日志 API 测试：

- test_admin_logs_list_returns_recent_logs - 验证返回最近日志
- test_admin_logs_list_supports_filtering - 验证支持筛选（按状态码、模型、时间范围）
- test_admin_logs_list_supports_pagination - 验证支持分页

**测试文件 2：tests/portal_api.rs（约 400 行）**

概览 API 测试：

- test_portal_overview_returns_quota_summary - 验证返回配额摘要
- test_portal_overview_calculates_per_minute_usage - 验证计算每分钟使用量
- test_portal_overview_calculates_request_quota_usage - 验证计算滑动窗口配额使用量
- test_portal_overview_calculates_token_summary - 验证计算 Token 使用量
- test_portal_overview_requires_bearer_token - 验证需要 Bearer token

限额详情 API 测试：

- test_portal_quota_returns_detailed_quota_info - 验证返回详细限额信息
- test_portal_quota_includes_model_allowlist - 验证包含模型白名单
- test_portal_quota_includes_ip_allowlist - 验证包含 IP 白名单

使用历史 API 测试：

- test_portal_usage_history_returns_daily_stats - 验证返回每日统计
- test_portal_usage_history_returns_recent_logs - 验证返回最近日志
- test_portal_usage_history_supports_time_range - 验证支持时间范围（7d、30d）

模型目录 API 测试：

- test_portal_models_returns_model_stats - 验证返回模型统计
- test_portal_models_calculates_today_usage - 验证计算今日使用量
- test_portal_models_calculates_monthly_usage - 验证计算本月使用量
- test_portal_models_calculates_avg_latency - 验证计算平均耗时
- test_portal_models_calculates_success_rate - 验证计算成功率

**测试文件 3：tests/frontend_assets.rs（约 100 行）**

静态资源测试：

- test_serve_frontend_returns_index_html_for_root - 验证 GET / 返回 index.html
- test_serve_frontend_returns_index_html_for_admin - 验证 GET /admin 返回 index.html
- test_serve_frontend_returns_index_html_for_portal - 验证 GET /portal 返回 index.html
- test_serve_frontend_returns_js_bundle - 验证 GET /assets/index-*.js 返回 JavaScript 文件
- test_serve_frontend_returns_css_bundle - 验证 GET /assets/index-*.css 返回 CSS 文件
- test_serve_frontend_spa_fallback - 验证 SPA 路由 fallback 到 index.html

### 前端测试（可选）

- 组件渲染测试（Vue Test Utils）
- API 调用测试（Mock axios）
- 路由测试（Vue Router）

### 集成测试

- 测试完整的管理员流程：登录 → 查看仪表盘 → 管理上游/下游 → 查看日志
- 测试完整的租户流程：访问门户 → 查看概览 → 切换标签页 → 查看图表
- 测试自动刷新（等待 30 秒，验证数据更新）
- 测试错误处理（无效 token、API 失败）

### 构建测试

- 测试多阶段 Dockerfile 构建成功
- 验证镜像大小合理（< 150MB）
- 验证所有前端资源正确嵌入（无 404 错误）
- 验证内网环境运行（断网测试）

## 实现顺序（TDD 模式）

### 阶段 1：清理 SSR 代码

1. 删除 crates/gateway-web/ 目录
2. 更新 Cargo.toml，删除 workspace 配置
3. 删除 src/server.rs 中所有 render_* 函数
4. 删除 src/server.rs 中所有 Html 响应
5. 运行 cargo build，验证编译通过

### 阶段 2：后端 JWT 认证测试和实现

1. 创建 tests/admin_api.rs，编写 JWT 认证测试（测试用例 1-4）
2. 运行 cargo test admin_api::test_admin_login，验证测试失败（红灯）
3. 在 Cargo.toml 中添加 jsonwebtoken 依赖
4. 创建 src/auth.rs，实现 JWT 生成和验证函数
5. 在 src/server.rs 中实现 POST /api/admin/login 端点
6. 在 src/server.rs 中实现 admin_auth_middleware 中间件
7. 运行 cargo test admin_api::test_admin_login，验证测试通过（绿灯）

### 阶段 3：后端管理 API 测试和实现

1. 在 tests/admin_api.rs 中编写管理 API 测试（测试用例 5-20）
2. 运行 cargo test admin_api，验证测试失败（红灯）
3. 在 src/server.rs 中实现管理 API 端点（/api/admin/*）
4. 运行 cargo test admin_api，验证测试通过（绿灯）

### 阶段 4：后端门户 API 测试和实现

1. 创建 tests/portal_api.rs，编写门户 API 测试（测试用例 1-15）
2. 运行 cargo test portal_api，验证测试失败（红灯）
3. 在 src/state.rs 中实现辅助函数（compute_*）
4. 在 src/server.rs 中实现门户 API 端点（/api/portal/*）
5. 运行 cargo test portal_api，验证测试通过（绿灯）

### 阶段 5：后端静态资源嵌入测试和实现

1. 创建 tests/frontend_assets.rs，编写静态资源测试（测试用例 1-6）
2. 运行 cargo test frontend_assets，验证测试失败（红灯）
3. 在 Cargo.toml 中添加 rust-embed 和 mime_guess 依赖
4. 在 src/server.rs 中实现 serve_frontend() 函数
5. 更新路由配置，添加 SPA fallback
6. 运行 cargo test frontend_assets，验证测试通过（绿灯）

### 阶段 6：前端项目初始化

1. 创建 frontend/ 目录
2. 初始化 package.json，安装依赖（Vue 3、Element Plus、ECharts、Vite）
3. 运行 cd frontend && npm install
4. 创建 src/main.ts、src/App.vue、src/router/index.ts
5. 创建 src/types/index.ts、src/api/admin.ts、src/api/portal.ts
6. 创建 src/stores/auth.ts（Pinia store，存储 JWT token）

### 阶段 7：前端管理后台实现

1. 实现 Login.vue（登录表单，JWT 认证）
2. 实现 Dashboard.vue（仪表盘，统计卡片）
3. 实现 Upstreams.vue（上游列表，CRUD 操作）
4. 实现 Downstreams.vue（下游列表，CRUD 操作，筛选）
5. 实现 Logs.vue（日志列表，筛选，分页）

### 阶段 8：前端自助门户实现

1. 实现 Portal.vue（主页面，标签页容器，自动刷新逻辑）
2. 实现 Overview.vue（概览，统计卡片，配额状态）
3. 实现 QuotaDetails.vue（限额详情，进度条，白名单）
4. 实现 UsageHistory.vue（使用历史，ECharts 图表，日志表格）
5. 实现 ModelCatalog.vue（模型目录，模型卡片，使用情况）

### 阶段 9：前端构建和集成

1. 配置 vite.config.ts（构建输出到 dist/）
2. 运行 npm run build，验证构建成功
3. 验证 dist/ 目录包含 index.html、assets/index-*.js、assets/index-*.css
4. 测试后端嵌入前端资源（cargo run，访问 http://localhost:3000）

### 阶段 10：多阶段 Dockerfile 和部署

1. 创建 Dockerfile.multistage（三阶段构建）
2. 更新 docker-compose.yml（使用 Dockerfile.multistage）
3. 运行 docker-compose build，验证构建成功
4. 运行 docker-compose up，验证服务启动
5. 测试完整流程（管理后台 + 自助门户）

## 技术决策

1. UI 库：Element Plus，因为它是 Vue 3 生态中最成熟的企业级 UI 库
2. 图表库：Apache ECharts，因为它是企业级标准，功能强大且文档完善
3. 构建工具：Vite，因为它是 Vue 3 官方推荐的构建工具，速度快
4. TypeScript：启用，提供类型安全和更好的开发体验
5. 样式方案：Element Plus 默认主题 + 自定义 CSS 变量
6. 路由模式：Hash 模式，避免后端路由冲突
7. 状态管理：Pinia，Vue 3 官方推荐的状态管理库
8. JWT 认证：使用 jsonwebtoken crate，token 有效期 12 小时
9. JWT 密钥：从环境变量 JWT_SECRET 读取，默认值为 admin_password
10. API 认证：管理后台使用 JWT token（存储在 localStorage），自助门户使用 Bearer token（存储在 URL 参数）
11. 静态资源嵌入：使用 rust-embed，编译时嵌入前端构建产物
12. SPA 路由：所有非 API 路由 fallback 到 index.html
13. 错误处理：API 失败时显示 Element Plus 的 ElMessage 提示，不中断页面
14. 加载状态：使用 Element Plus 的 v-loading 指令显示加载动画
15. 响应式设计：支持桌面端（1280px+），暂不支持移动端
16. 国际化：暂不支持，所有文本使用中文
17. 主题切换：暂不支持，使用浅色主题
18. 数据缓存：不缓存 API 响应，每次刷新都重新请求
19. 并发优化：后端 API 使用 async/await，支持高并发场景；前端使用 Promise.all() 并行请求多个 API
20. 分页：日志列表支持分页，默认每页 50 条
21. 筛选：下游列表和日志列表支持筛选，使用 URL 查询参数传递筛选条件
22. 表单验证：使用 Element Plus 的表单验证功能
23. 密钥轮换：下游密钥轮换后，返回新的明文密钥（只显示一次）
24. 密钥预览：下游列表中只显示密钥的前 4 位和后 4 位（如 sk-te...demo）
25. 配额颜色编码：使用率 ≤ 70% 绿色，70-90% 黄色，> 90% 红色

## 进度跟踪

- [x] 阶段 1.1：删除 crates/gateway-web/ 目录
- [x] 阶段 1.2：更新 Cargo.toml，删除 workspace 配置
- [ ] 阶段 1.3：删除 src/server.rs 中所有 render_* 函数
- [ ] 阶段 1.4：删除 src/server.rs 中所有 Html 响应
- [ ] 阶段 1.5：运行 cargo build，验证编译通过
- [ ] 阶段 2：后端 JWT 认证测试和实现
- [ ] 阶段 3：后端管理 API 测试和实现
- [ ] 阶段 4：后端门户 API 测试和实现
- [ ] 阶段 5：后端静态资源嵌入测试和实现
- [ ] 阶段 6：前端项目初始化
- [ ] 阶段 7：前端管理后台实现
- [ ] 阶段 8：前端自助门户实现
- [ ] 阶段 9：前端构建和集成
- [ ] 阶段 10：多阶段 Dockerfile 和部署
