# 模型映射管理 UI - 部署完成报告

## 📦 部署信息

- **部署时间**: 2026-08-13 22:20:45
- **Commit**: `7917bf0`
- **分支**: `main`
- **状态**: ✅ 部署成功

---

## 🎯 功能概述

### 核心功能
全局模型别名映射管理，通过可视化界面统一下游看到的模型名称。

### 设计方案
**方案 A - 全局映射 + 上游浏览器辅助**
- 映射规则对所有上游全局生效
- 上游浏览器帮助查看各上游支持的模型
- 大小写不敏感匹配
- 网关智能路由到支持该模型的上游

---

## 🖥️ 用户界面

### 页面布局（两栏）

#### 左侧：上游模型浏览器
- 上游账号选择器（下拉框）
- 该上游支持的模型列表
- 每个模型有「快速添加」按钮

#### 右侧：全局映射规则
- 规则列表表格
  - 规范名称（canonical）
  - 别名列表（aliases）
  - 操作按钮（编辑/删除）
- 批量保存功能

---

## 📍 访问路径

### 前端页面
```
http://localhost:3000/#/admin/model-aliases
```

### 导航位置
```
管理后台 → 资源管理 → 模型映射
```

### 图标
⇄ (ArrowRightLeft)

---

## 🔌 API 端点

### GET /api/admin/model-aliases
获取当前配置的映射规则

**响应示例**：
```json
{
  "model_aliases": [
    {
      "canonical": "deepseek-v3",
      "aliases": ["deepseek-chat", "DeepSeek-Chat"]
    }
  ]
}
```

### PUT /api/admin/model-aliases
更新映射规则（全量替换）

**请求示例**：
```json
{
  "model_aliases": [
    {
      "canonical": "deepseek-v3",
      "aliases": ["deepseek-chat", "DeepSeek-Chat"]
    },
    {
      "canonical": "gpt-4",
      "aliases": ["GPT-4", "gpt-4-turbo"]
    }
  ]
}
```

---

## 🚀 使用流程

### 场景：统一多个上游的 DeepSeek 模型名称

#### 背景
- 上游 A 支持：`deepseek-chat`
- 上游 B 支持：`DeepSeek-Chat`（大小写不同）
- 目标：下游只看到统一的 `deepseek-v3`

#### 操作步骤

1. **选择上游账号 A**
   - 在左侧下拉框选择"上游 A"
   - 查看其支持的模型列表

2. **快速添加映射**
   - 点击 `deepseek-chat` 旁边的「快速添加」
   - 弹出对话框，自动填充规范名称

3. **编辑规则**
   - 修改规范名称为 `deepseek-v3`
   - 添加别名 `DeepSeek-Chat`（涵盖上游 B 的拼写）
   - 点击「确定」

4. **保存配置**
   - 点击右侧的「保存全部」按钮
   - 等待服务器确认

#### 结果
- ✅ 下游用户只看到 `deepseek-v3`
- ✅ 请求 `deepseek-v3` 会路由到上游 A 或 B（自动选择）
- ✅ 上游 A 收到请求时，模型名为 `deepseek-chat`
- ✅ 上游 B 收到请求时，模型名为 `DeepSeek-Chat`

---

## 🧪 测试

### 自动化测试
```bash
# 设置管理员 token
export ADMIN_TOKEN='your-admin-token-here'

# 运行测试脚本
./scripts/test-model-aliases.sh
```

### 手动测试清单
详见 `frontend/tests/model-aliases.spec.md`

主要测试点：
- ✅ TC-1: 访问页面
- ✅ TC-2: 选择上游查看模型
- ✅ TC-3: 快速添加映射规则
- ✅ TC-4: 手动添加规则
- ✅ TC-5: 编辑现有规则
- ✅ TC-6: 删除规则
- ✅ TC-7: 保存全部更改
- ✅ TC-8: 冲突检测
- ✅ TC-9: 切换上游账号
- ✅ TC-10: 空模型列表处理

---

## 📊 技术实现

### 前端
- **框架**: Vue 3 + TypeScript
- **UI 库**: Element Plus
- **路由**: Vue Router
- **文件**:
  - `src/views/admin/ModelAliases.vue` (主页面)
  - `src/api/admin.ts` (API 调用)
  - `src/types/index.ts` (类型定义)
  - `src/router/index.ts` (路由配置)
  - `src/App.vue` (导航菜单)

### 后端
- **API**: `/api/admin/model-aliases` (GET/PUT)
- **实现**: 已在 Part B-2 完成
- **数据库**: PostgreSQL 持久化

### 构建产物
- `ModelAliases-BIiRKwlt.js` (8.76 kB)
- `ModelAliases-dO-gBxjD.css` (2.41 kB)

---

## 📝 配置规则

### 有效配置
```json
{
  "canonical": "deepseek-v3",
  "aliases": ["deepseek-chat", "DeepSeek-Chat"]
}
```

### 约束条件
- ✅ `canonical` 不能为空
- ✅ `aliases` 可以为空数组（表示无别名）
- ✅ 每个 `alias` 在所有规则中必须唯一（大小写不敏感）
- ✅ `canonical` 不能同时作为其他规则的 `alias`
- ✅ 大小写不敏感匹配

### 错误示例
```json
// ❌ 错误：alias 重复
[
  {"canonical": "model-a", "aliases": ["alias-1"]},
  {"canonical": "model-b", "aliases": ["alias-1"]}  // 冲突！
]

// ❌ 错误：canonical 与 alias 冲突
[
  {"canonical": "model-a", "aliases": []},
  {"canonical": "model-b", "aliases": ["model-a"]}  // 冲突！
]
```

---

## 🎓 设计原理

### 全局映射的工作机制

1. **下游请求阶段**
   - 用户请求 `deepseek-v3`
   - 网关查询映射表：`deepseek-v3` → `[deepseek-chat, DeepSeek-Chat]`

2. **路由选择阶段**
   - 查找所有支持 `deepseek-chat` 或 `DeepSeek-Chat` 的上游
   - 根据负载、配额等策略选择一个上游

3. **请求转发阶段**
   - 使用上游的**原始模型名**发送请求
   - 上游 A 收到 `deepseek-chat`
   - 上游 B 收到 `DeepSeek-Chat`

### 优势
- ✅ 下游视图统一
- ✅ 自动聚合多个上游的相同模型
- ✅ 大小写智能匹配
- ✅ 网关透明路由

---

## 🔧 维护指南

### 添加新的映射规则
1. 登录管理后台
2. 资源管理 > 模型映射
3. 选择上游查看其模型（可选）
4. 点击「快速添加」或「手动添加」
5. 配置规范名称和别名
6. 保存

### 修改现有规则
1. 在规则列表中找到对应规则
2. 点击「编辑」按钮
3. 修改后保存

### 删除规则
1. 点击「删除」按钮
2. 确认删除
3. 保存全部

### 常见问题

**Q: 为什么修改后下游还看到旧的模型名？**
A: 需要点击「保存全部」才会应用到服务器。

**Q: 如何验证映射是否生效？**
A: 查看下游的模型列表，应该显示配置的 `canonical` 名称。

**Q: 多个上游支持同一个模型，如何选择？**
A: 网关会根据负载、配额、优先级等策略自动选择。

**Q: 上游的模型列表是实时的吗？**
A: 是的，从上游的 `supported_models` 配置读取。

---

## 📚 相关文档

- **测试用例**: `frontend/tests/model-aliases.spec.md`
- **测试脚本**: `scripts/test-model-aliases.sh`
- **计划文档**: `docs/superpowers/plans/2026-08-13-route-exhaustion-self-healing-and-model-alias-unification.md`
- **后端实现**: `src/state/model_identity.rs`

---

## ✅ 完成清单

- [x] 类型定义 (`ModelAliasRule`)
- [x] API 方法 (`getModelAliases`, `updateModelAliases`)
- [x] 主页面组件 (`ModelAliases.vue`)
- [x] 路由配置 (`/admin/model-aliases`)
- [x] 导航菜单项
- [x] TypeScript 类型检查通过
- [x] 前端构建成功
- [x] Docker 镜像构建
- [x] 容器部署成功
- [x] 测试脚本创建
- [x] 文档完善

---

## 🎊 部署状态

**状态**: ✅ 已上线
**访问**: http://localhost:3000/#/admin/model-aliases
**服务**: 运行正常

---

*生成时间: 2026-08-13 22:20*
*生成者: Claude Opus 5*

---

## 🔀 更新（Part B-3，2026-08-14）：按上游模型映射 vs 全局规则

模型映射页面（`/admin/model-aliases`）改版为**双 tab**，默认 tab 为「模型映射」，
原全局规则内容移入第二个 tab「全局规则」。

### Tab 1「模型映射」（按上游，默认）

表达「**上游账号 + 上游模型名称 → 下游模型名称**」的三元组，平铺表格展示：

上游账号 | 上游模型名称 | 下游模型名称 | 状态 | 操作（编辑/删除）

- 顶部支持按上游筛选 + 模型名搜索。
- 添加映射为三步对话框：选上游账号 → 选上游模型（**数据源 = 该上游已配置的
  `supported_models` ∪ `api_key_models[].supported_models` 去重**，已映射条目禁用，
  **绝不调用上游 `/v1/models`**）→ 输入下游名称。
- 编辑锁定「上游账号 / 上游模型」，只改下游名称；删除需确认。
- 「状态」列：`upstream_model` 已不在该上游任何模型清单 → 标记 **失效**（路由跳过，
  恢复清单后自动生效，无需改配置）。

### Tab 2「全局规则」（跨上游）

保留原 Model Aliases 内容：canonical + aliases 规则对所有上游全局生效，
做入口归一与列表显示拼写控制；页头提示改到 Tab 1 做按上游改名。

### 两者怎么选

| 场景 | 用哪个 |
|------|--------|
| 不同上游的同名模型要改成**不同的**下游名（A 的 `gpt-4` → `gpt-4-premium`，B 的 `gpt-4` → `gpt-4-standard`） | 「模型映射」tab（全局规则数学上无法表达） |
| 跨上游把多种拼写归并成一个名字（`deepseek-chat`/`DeepSeek-Chat` → `deepseek-v3`） | 「全局规则」tab |
| 只声明 `deepseek-chat` 的上游要能命中 `deepseek-v3` 请求 | 「模型映射」配一条 `deepseek-chat → deepseek-v3`（映射优先于全局规则） |

### 运行语义（管理员应了解）

- **解析顺序**：请求名先过全局 alias 归一，再到各上游的按上游映射（canonical 比较）；
  映射命中后 `runtime_model_slug`、发往上游的 payload model、premium 判定、
  `ModelContextConfig.slug` 查找全部使用**上游原拼写**。
- **原名遮蔽**：被映射占用的上游原拼写对下游不可见、不可路由（除非其它上游未映射地
  声明了它）；下游模型列表 = 各上游 { 映射下游名 } ∪ { 未映射原拼写 } 再经全局
  alias 显示 + canonical 去重。
- **键归属**：usage / 配额 / affinity / 日志 model 字段记**下游名**。
- **持久化**：`model_mappings` 挂在 `UpstreamConfig` 上（file / postgres / redis 三后端
  快照 `serde(default)`），不建数据库表、无 sqlx；删除上游即级联删除其映射。
- **保存**：前端沿用既有上游更新接口（按字段 PATCH `{ model_mappings }`），
  后端 `update_upstream_by_id` 合并并跑校验（下游名不得与全局 alias 冲突等）。
