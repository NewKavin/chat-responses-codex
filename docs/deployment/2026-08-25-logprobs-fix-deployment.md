# 部署说明：修复 GLM/Deepseek 400 错误和 upstream_routes_exhausted 问题

**提交：** `f7dd7ad6` - fix(gateway): add logprobs/top_logprobs to dialect retry whitelist  
**日期：** 2026-08-25  
**优先级：** 🔴 高（解决内网生产问题）

---

## 快速概览

### 问题
内网部署使用 GLM5.1 和 Deepseek 模型时频繁出现：
- ❌ `upstream_routes_exhausted` - 所有上游路由不可用
- ❌ HTTP 400 `invalid_parameter_error` - 上游拒绝请求

### 根本原因
国内模型不支持 `logprobs` / `top_logprobs` 字段，但网关的方言重试机制没有覆盖这些字段。

### 解决方案
- ✅ 添加到自动重试白名单（上游拒绝时自动移除并重试）
- ✅ Responses → Chat 转换时主动移除（避免往返延迟）
- ✅ 正确的字段匹配顺序（避免 top_logprobs 误匹配为 logprobs）

---

## 部署步骤

### 1. 编译新版本

```bash
cd /home/kavin/projects/chat2Responses
git pull origin main
cargo build --release
```

### 2. 备份当前版本

```bash
cd ~/docker/chat-responses-codex
cp chat-responses-codex chat-responses-codex.backup-$(date +%Y%m%d)
```

### 3. 部署新二进制文件

```bash
cp /home/kavin/projects/chat2Responses/target/release/chat-responses-codex \
   ~/docker/chat-responses-codex/chat-responses-codex
```

### 4. 重启服务

```bash
cd ~/docker/chat-responses-codex
docker-compose restart  # 或使用你的服务管理方式
# 或者如果是直接运行：
# systemctl restart chat-responses-codex
```

### 5. 验证服务启动

```bash
# 检查日志，确认服务正常启动
tail -f ~/docker/chat-responses-codex/logs/chat-responses-codex.log

# 应该看到类似的日志：
# INFO chat_responses: starting gateway server
# INFO chat_responses: listening on 0.0.0.0:8080
```

---

## 监控清单

### 部署后 1 小时 - 立即验证

```bash
cd ~/docker/chat-responses-codex/logs

# 1. 检查是否有新的 400 invalid_parameter 错误
tail -1000 chat-responses-codex.log | grep -i "invalid_parameter" | grep "logprobs"
# 预期：应该看不到（或很少）

# 2. 检查方言重试是否生效
tail -1000 chat-responses-codex.log | grep -E "dialect.*retry|strip.*logprobs"
# 预期：可能看到自动剥离 logprobs 的日志

# 3. 检查是否还有 routes exhausted 错误
tail -1000 chat-responses-codex.log | grep "upstream_routes_exhausted"
# 预期：应该显著减少或消失
```

### 部署后 24 小时 - 趋势分析

```bash
cd ~/docker/chat-responses-codex/logs

# 统计今天的 400 错误（按 param 分类）
grep -a "status 400" chat-responses-codex.log | \
  grep -oP 'param=[^,}]+' | sort | uniq -c | sort -rn

# 统计 routes exhausted 次数（对比前一天）
echo "今天："
grep -a "upstream_routes_exhausted" chat-responses-codex.$(date +%Y-%m-%d) | wc -l
echo "昨天："
grep -a "upstream_routes_exhausted" chat-responses-codex.$(date -d yesterday +%Y-%m-%d) | wc -l

# 检查 GLM 和 Deepseek 的成功率
grep -a "model.*glm\|model.*deepseek" chat-responses-codex.log | \
  grep -c "upstream request completed"
```

### 关键指标

| 指标 | 修复前（预期） | 修复后（目标） |
|------|--------------|--------------|
| 包含 logprobs 的 400 错误 | 频繁 | 几乎为零 |
| upstream_routes_exhausted | 每小时数次 | 罕见/消失 |
| GLM/Deepseek 请求成功率 | <90% | >98% |
| 方言重试次数 | 0 | 适度（说明机制生效） |

---

## 回滚计划

如果部署后出现新问题：

```bash
cd ~/docker/chat-responses-codex

# 1. 停止服务
docker-compose stop

# 2. 恢复旧版本
cp chat-responses-codex.backup-YYYYMMDD chat-responses-codex

# 3. 重启服务
docker-compose start

# 4. 报告问题
# 收集日志并报告给开发团队
```

---

## 技术细节

### 修改的文件

1. **src/server/gateway/capability_probe.rs**
   - 添加 `logprobs` / `top_logprobs` 到安全字段白名单
   - 调整字段检测顺序避免误匹配

2. **src/server/gateway/responses_fallback.rs**
   - Responses → Chat 转换时主动移除这些字段

3. **tests/unit/server/gateway.rs**
   - 新增单元测试验证修复

### 为什么这个修复是安全的

- ✅ **不丢失功能**：Codex 不依赖 logprobs 数据
- ✅ **向后兼容**：支持 logprobs 的上游不受影响
- ✅ **有测试覆盖**：228 个单元测试全部通过
- ✅ **符合标准**：logprobs 是可选字段，移除是合法操作

### 自动重试机制

```
请求带 logprobs → 上游 400 → 网关检测到 param=logprobs 
→ 自动移除 logprobs → 在同一路由重试 → 成功 
→ 记录该路由不支持 logprobs → 后续请求自动过滤
```

---

## 预期效果

### 立即效果（部署后 1 小时内）
- GLM/Deepseek 的 400 错误应该几乎消失
- 不再出现由 logprobs 导致的路由冷却

### 中期效果（24 小时后）
- `upstream_routes_exhausted` 错误频率显著下降
- Codex 长会话的稳定性提升
- 用户体验更流畅，减少中断

### 长期效果
- 能力系统学习到各路由的字段支持情况
- 后续类似问题可以自动处理（白名单可扩展）

---

## 问题排查

### 如果问题依旧存在

1. **检查是否还有其他参数导致 400**
   ```bash
   grep "status 400" logs/chat-responses-codex.log | \
     grep -oP 'code=[^,}]+' | sort | uniq -c
   ```

2. **检查是否是其他类型的错误**
   ```bash
   grep "upstream_routes_exhausted" logs/chat-responses-codex.log | \
     grep -oP 'failure_class=[^,}]+' | sort | uniq -c
   ```

3. **查看完整的错误链路**
   ```bash
   # 找一个 routes exhausted 的 request_id
   grep "upstream_routes_exhausted" logs/chat-responses-codex.log | tail -1 | \
     grep -oP 'request_id=[a-f0-9-]+'
   
   # 查看该请求的完整日志
   grep "request_id=<上面的id>" logs/chat-responses-codex.log
   ```

### 需要进一步修复的信号

- 如果仍然频繁出现 400，但 `param` 是其他字段（如 `parallel_tool_calls`、`reasoning_effort`）
- 如果日志中出现大量 `channel_not_found` 或 `quota_exceeded`（配置问题，非代码问题）
- 如果仅特定模型或特定上游有问题（可能需要针对性的能力档案）

---

## 联系与支持

如果部署后遇到问题或需要帮助：

1. 收集最近 1000 行日志：`tail -1000 logs/chat-responses-codex.log > issue.log`
2. 记录具体的错误消息和 request_id
3. 联系开发团队并附上日志

---

**部署checklist：**
- [ ] 代码已拉取到最新（包含 f7dd7ad6）
- [ ] 编译成功（`cargo build --release`）
- [ ] 旧版本已备份
- [ ] 新版本已部署
- [ ] 服务已重启
- [ ] 启动日志正常
- [ ] 监控已设置（24小时）
- [ ] 回滚计划已准备

**预计部署时间：** 5-10 分钟  
**预计影响：** 服务重启期间短暂不可用（<30秒）  
**预计收益：** GLM/Deepseek 400错误消失，routes exhausted显著减少
