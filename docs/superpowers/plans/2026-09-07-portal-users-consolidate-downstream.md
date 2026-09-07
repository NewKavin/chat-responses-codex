# 门户用户管理合并下游管理（方案 + 实施记录）

- 日期：2026-09-07
- 状态：已实施并部署

## 目标

下游管理页签取消后，其全部配置能力合并进「门户用户管理」，仍保留批量编辑；管理端不再
新建/轮换密钥（用户自己在门户创建）；legacy key-xxx 登录密钥**保留**，可继续绑定与配置。

## 已拍板决策（勿再改）

1. 管理端**不新建、不轮换**密钥：用户登录门户自建密钥（sk- 前缀，服务端生成），创建即自动
   绑定到用户。管理端只负责「绑定存量密钥 + 配置 + 批量编辑」。
2. **legacy key-xxx / portal- 密钥保留**：仍用于登录。管理员可通过「添加」把存量密钥绑到用户；
   列表中带「旧版」标记。
3. 批量编辑能力保留：批量改分组 / 批量启用禁用 / 批量改限额。
4. 各配置字段语义与后端 `BATCH_UPDATE_DOWNSTREAM_ALLOWED_FIELDS` / 单条 PUT 完全一致。

## 实施记录

| 项 | Commit | 状态 |
|---|---|---|
| 门户用户页承载密钥账户管理：批量编辑保留、绑定分组可保存（内联下拉 + PUT） | 11c49192, 6b3ec9d | ✅ |
| 合并自下游管理的完整配置：限速/配额窗口/并发/每日每月Token/计费(req/token+成本字段)/IP白名单/过期/模型并发组 | 72953dd | ✅ |
| 批量改限额弹窗同宽度（配额窗口、每月Token、成本字段） | 72953dd | ✅ |
| 保留 legacy key-xxx：恢复「添加」绑定入口 + 「旧版」标记 + 名称/内部id双行展示 | 72953dd | ✅ |
| 管理端 rotateDownstream / createDownstream 从门户用户页移除（前端不再引用） | 72953dd | ✅ |
| 前端测试：完整配置字段断言 + addBinding/legacy 保留 + rotate/create 不存在 | 72953dd | ✅ |
| 门户自建密钥（sk-）管理端可配置：后端放开列表/PUT/toggle/批量 的 is_portal_key 守卫，前端编辑按钮放开 | 待提交 | ✅ |
| legacy 标记改为按 is_portal_key 判定（门户密钥 id 也是 key- 前缀，不能按前缀标旧版） | 待提交 | ✅ |
| 后端 create/rotate 端点本轮保留（API 兼容、回滚安全），仅 UI 下线入口 | — | 暂缓（下版本再评估删除） |

## 编辑账户配置弹窗字段（门户用户 → 绑定 → 编辑）

- 账户名称、启用开关
- 限速与配额：启用限速开关、每分钟请求数、配额窗口小时(1-168)、窗口请求次数、最大并发
- Token 限额：每日、每月
- 计费：按请求 / 按金额（日限额）；按金额时填 输入单价、输出单价、每日金额上限（元 → 分 换算）
- 访问控制：IP 白名单（每行一条 IP/CIDR）、过期时间
- 模型并发组：name/match（逗号/换行分隔，支持 *）/max_concurrency，同名与空匹配校验

## 批量改限额弹窗字段

每分钟请求数、配额窗口小时、窗口请求次数、最大并发、每日/每月 Token、计费模式 + 成本三字段。
写入走 `POST /admin/downstreams/batch-update`（后端已允许上述全部字段）。

## 说明

- 门户自建密钥（is_portal_key）同样**可编辑配置**：后端 `admin_list_downstreams` /
  `admin_update_downstream` / `admin_batch_update_downstreams` / `admin_toggle_downstream`
  已放开 is_portal_key 守卫；其生命周期（创建/轮换/删除）仍由门户侧持有，管理端
  `admin_delete_downstream` 保持 404 守卫（删除走 unbind / 门户侧）。
- legacy 标记按 `is_portal_key` 判定，而非 id 前缀（门户密钥 id 也是 `key-` 前缀）。
- 后端 admin.rs 的 create/rotate 端点保留未动，供 API 兼容与回滚；如需彻底下线另行评估。
