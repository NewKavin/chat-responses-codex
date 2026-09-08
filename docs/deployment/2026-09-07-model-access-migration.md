# 升级说明：模型访问单一事实源（2026-09-07）

本次升级把密钥权限从「绑定级 model_group_id」改为
`downstream_access_policies`（策略表）+ `downstream_access_migrations`
（审计表）的单一事实源。启动时 `initialize_schema` 自动完成建表与分类，
无手工 SQL 步骤；**不删除**旧列、日志、白名单表或 legacy 登录凭据。

## 升级行为

1. **自动分类**（幂等，缺策略/缺迁移记录的行才处理）：
   - 保留现状：旧 L3 权限与旧绑定组有效交集一致（或旧 L3=all、L2 无
     premium 等），密钥与用户授权一并迁入；
   - 归属冲突/孤儿：迁移后在管理端可见、API 拒绝，需管理员处理；
   - 待确认（review_required）：现拒绝保留，管理端
     `/api/admin/portal/users/access-migration` 预览并按指纹应用
     （补齐旧绑定组授权 + 置 inherit）。
2. **新密钥默认 `inherit`**（继承所属用户全部授权分组的模型并集）；
   `deny-all`/`group`/`inherit` 为明确模式，空值不再解释为继承。
3. **权限收窄**：用户授权并集是上限，密钥 `group` 模式再收窄；
   `all` 组通配正确去重，不会跨用户串权。
4. **管理端下游表现在包含门户自建密钥**（管理员配置限额/过期/策略），
   管理端密钥管理页不再提供开关/限流 UI。
5. 门户登录 JWT 与 admin 凭据分离：门户 token 带 `scope=portal`，
   admin 鉴权拒绝；旧 token（无 scope）在升级窗口内仍可用。

## 已知边界（fail-safe 方向）

- 迁移分类不重放全局别名（如 deepseek-chat ↔ deepseek-v3）：等价拼写
  差异的密钥被归入待确认而非自动放行，管理员确认后生效；
- 组内容/授权并发变化由事务内指纹复核阻止（409），不会按过期预览放权。

## 验证

```bash
OIDC_TEST_DATABASE_URL=postgres://… rtk cargo test --test model_access_migration
rtk cargo test --workspace
cd frontend && rtk npm run test && rtk npm run type-check && rtk npm run build
```

Playwright/真实部署未在本轮验收（仓库无 Playwright 基建、未做线上部署）；
本轮验证均为隔离 DB + 单元/集成测试，未将 mock 结果写成线上已修复。
