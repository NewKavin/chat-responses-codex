# 开发提示词：门户用户配置合并、模型权限统一与总览局部刷新

请完整阅读 [设计与实施方案](../plans/2026-09-07-model-group-single-source.md)，再按 A-G 顺序开发。该文档是事实源；本提示词仅列执行约束，不另建权限或迁移规则。

当前只有设计修订，没有功能实现。不要沿用旧文档“下游配置已经完整迁移”的结论替代行为验收。

## 目标

门户用户管理完整承接下游配置与跨用户批量操作。新门户密钥默认继承所属用户全部授权分组的模型并集；候选、门户、客户端目录和请求鉴权共用对外模型身份；总览局部刷新保留布局和操作状态。

## 必须遵循的决策

1. deny-all 始终拒绝。inherit/group/deny 为明确模式，不能将 deny-all、null 或空字符串重新解释为继承。
2. 数据库模式采用 downstream_access_policies 保存密钥归属和策略，用户授权保留在 portal_user_model_groups；旧 L2/L3 只作迁移输入及升级前留档。
3. 用户授权并集只属于该用户。新门户密钥唯一 owner；旧多用户绑定标记冲突并拒绝 API，不取跨用户并集。
4. portal 主体持久化，解绑/用户删除/禁用/空 owner 不退回 direct。删除/轮换真正撤销旧 secret，轮换保留全部额度与访问配置。
5. 权限核心使用 All/None/Models。DB 错误、坏 JSON、缺策略不回退旧白名单。文件模式保留 legacy allowlist 适配，不假称能查询数据库分组。
6. 必须有启动事务迁移、审计分类、幂等及备份恢复验证。旧生效 L2 权限不能直接丢掉；旧 deny-all 的意图不靠猜测，提供门户用户页的明确批量修复。
7. 允许并要求设计所需 DDL/迁移，接入 initialize_schema，不仅把文件放进 migrations/；不删除旧列、日志、白名单表或 legacy 登录凭据。
8. 目录和请求共用 ModelId 解析。canonical 是对外名，aliases 是归一拼写；每个上游分别映射。禁止仅在列表使用 display/raw 的 OR 放宽判断。
9. 普通/Codex 目录、三协议、count_tokens、门户探测、上下文和统计全部接入；前端不再用 quota 白名单二次过滤 /v1/models，不能将 * 当模型。
10. 概览显示用户模型范围，调用使用所选密钥权限。新增 /api/portal/model-access，不改变原 /api/portal/models 统计结构。
11. 限流、配额、计费继续按密钥生效。管理员 UI 无创建/复制 secret/轮换入口；用户在门户自建。
12. 单条与批量覆盖设计第 2.2 节全部配置。默认保持不变、null 只明确清空；部分失败按实际结果显示并回读配置。
13. 静默轮询有独立 inFlight 和请求版本，保留数据、空态、高度、图表实例、滚动及焦点，隐藏页面暂停。
14. 首次登录也要接入：legacy ensure_user_for_downstream、OIDC bind intent 都走唯一 owner；合法无密钥 OIDC 用户可进入门户自行创建，服务端不自动发 key。
15. session 统一 cookie/legacy principal，显式 downstream_id 不能被 cookie 默认 key 覆盖；共享 stores/portal.ts 维护选择。Integration 的 key/目录/上下文一致，Overview 用户模型数不误用单 key 统计；原数组响应不能为增加 scope 改结构。
16. 所有 insert/import/sync 创建路径初始化新策略；资格审定改读新策略，检查策略引用及用户授权引用，不能修改共享上限。标签编辑等旁路不再更新旧绑定分组。

## 工作规则

- 所有 shell 命令以 rtk 开头，在正确目录分别执行，不串接。手工修改使用 apply_patch。
- 尊重工作区已有修改，不因测试顺序而删除他人代码；先理解并补齐有意义的失败用例。
- TDD 验证行为，先复现再实现再回归；不要只断言源码出现某函数或字段。
- 子代理仅探索、定位、核验，使用仓库要求的 default 角色与空历史；代码修改、设计取舍、最终验证由主代理负责。
- DB 测试只用隔离库并遵守测试锁，不重置生产库、不输出凭据、不把跳过项计为通过。
- 配置和策略逐密钥事务化，授权替换逐用户事务化；不能拆到 PortalStore/PgStore 两条连接独立提交。
- 迁移与新读路径同版本发布，禁止新旧权限语义网关混跑，不中途部署开发提交。
- 若当前授权仅为修改设计，止于文档；实施指令后才开发业务代码。设计完成不等于获准部署、推送或改生产数据。

## 实施顺序

1. A：ModelId、PublishedModel、All/None/Models、模式组合，验证真改名与跨上游映射。
2. B：策略/审计表、同事务持久化、旧库分类；先测 all/premium、deny-all、孤立/共享绑定再切读取。
3. C：权限入口与错误处理、全部目录/请求/元数据路径，新增 exposed 和 portal/model-access。
4. D：完整配置 PATCH、兼容翻译、归属/生命周期、迁移报告和选定修复。
5. E：门户用户配置、跨用户批量、新模型契约、回读与部分失败，新密钥默认 inherit。
6. F：独立轮询、稳定布局、取消/旧响应保护；此任务可独立验证。
7. G：设计第 9 节验收，更新 API/部署文档和实际实施记录。

## 最低验证

- 权限：用户多组并集、显式限定、deny、删组、撤权、禁用、解绑、空 owner、共享冲突、DB 失败。
- 升级：旧库分类、旧权限保留、待确认状态、重启不覆盖新配置、事务回滚、备份恢复；迁移修复预览失效必须 409。
- 模型：deepseek-chat -> deepseek-v3 真改名，两端分别单独授权；上游映射与另一个未映射同名；普通/Codex 目录与请求一致；all 组在操练场可用。
- 配置：只改并发不改计费/Token/IP/过期等其它字段；过期秒/毫秒往返；批量部分失败与回读；原配置全部可用；资格审定不影响共享授权。
- 身份：无 key OIDC 首次登录可自建，legacy 首次/重复登录归属一致，cookie/JWT 冲突统一 principal，明确 key 不被默认覆盖，越权 key 直接拒绝。
- 刷新：Vue 挂载配合假计时器/慢响应/乱序响应；Playwright 桌面/移动截图检查空态高度、滚动、焦点与图表。

新增测试实现后，在仓库根目录分别运行：

```bash
rtk cargo test --test model_access_policy
rtk cargo test --test model_access_migration
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets
```

在 frontend/ 目录分别运行：

```bash
rtk npm run test
rtk npm run type-check
rtk npm run build
```

Clippy 不新增警告，既有警告记录基线。Playwright 和模拟调用使用隔离服务；未做真实部署验收时明确说明，不把 mock 或静态测试写成线上已修复。

## 交付

在设计第 10 节填写实际完成状态、验证证据及相关提交（确有提交时）。最终说明已完成行为、验证范围、迁移待处理记录和未完成项；构建通过或 UI 出现控件都不足以单独证明任务完成。
