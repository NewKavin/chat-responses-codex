# 集成示例与模型操练场最终整理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 整理门户集成示例的信息层级、提高操练场“自动”提示的浅色主题对比度，并构建部署最终镜像后完成八类模型的 Codex/OpenCode 实测。

**Architecture:** 保留现有门户 API 和配置生成 computed，只调整 `Integration.vue` 的模板顺序与局部样式，并在 `PlaygroundSettings.vue` 内局部覆盖 Element Plus 未选中 inline prompt 的颜色。前端产物由 Vite 构建到 `frontend/dist`，Rust release 二进制通过 `rust-embed` 嵌入后注入现有 runtime 镜像，仅重建 Compose 的 gateway 服务。

**Tech Stack:** Vue 3、TypeScript、Element Plus、Vitest、Vite、Rust/Axum、Docker Compose、Codex CLI 0.144.4、OpenCode 1.17.18。

---

### Task 1: 整理集成示例信息层级

**Files:**
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Modify: `frontend/src/views/portal/Integration.vue:15-152`
- Modify: `frontend/src/views/portal/Integration.vue:240-390`
- Modify: `frontend/src/views/portal/Integration.vue:753-993`

- [ ] **Step 1: 写客户端顺序和紧凑模型排名的失败测试**

在 `portal-ui.spec.ts` 的 integration 用例中增加：

```ts
expect(page).toContain('class="model-ranking"')
expect(page).toContain('class="model-ranking__item"')
expect(page).toContain('class="model-ranking__position"')
expect(page).toContain('v-if="stat.model === primaryModelSlug"')
expect(page).toContain('class="config-section-head"')

const tabNames = [
  'name="codex"',
  'name="opencode"',
  'name="claude"',
  'name="cline"',
  'name="anthropic"',
  'name="hermes"'
]
for (let index = 1; index < tabNames.length; index += 1) {
  expect(page.indexOf(tabNames[index])).toBeGreaterThan(page.indexOf(tabNames[index - 1]))
}
```

- [ ] **Step 2: 运行定向测试并确认 RED**

Run:

```bash
cd frontend
rtk npm test -- tests/views/portal-ui.spec.ts
```

Expected: FAIL，缺少 `model-ranking` / `config-section-head`，且 `claude` 当前位于 `anthropic` 之后。

- [ ] **Step 3: 收紧连接摘要并实现紧凑模型排名**

给 `el-descriptions` 增加 `size="small"`。将现有 `model-grid` 块替换为：

```vue
<div class="model-ranking">
  <div
    v-for="(stat, index) in sortedModelStats"
    :key="stat.model"
    class="model-ranking__item"
  >
    <span class="model-ranking__position">{{ index + 1 }}</span>
    <div class="model-ranking__identity">
      <strong>{{ stat.model }}</strong>
      <el-tag
        v-if="stat.model === primaryModelSlug"
        size="small"
        type="success"
        effect="plain"
      >
        默认
      </el-tag>
    </div>
    <span class="model-ranking__metrics">
      月 {{ stat.month_count }} · 今 {{ stat.today_count }} · 成功率
      {{ Math.round(stat.success_rate * 100) }}%
    </span>
  </div>
</div>
```

在配置 `section` 内、`el-tabs` 前增加：

```vue
<div class="section-head config-section-head">
  <div>
    <h2>客户端配置</h2>
    <p>优先提供 Codex 与 OpenCode，其他客户端按协议兼容方式配置。</p>
  </div>
  <el-tag effect="plain">实时目录已同步</el-tag>
</div>
```

将现有完整 `Claude Code` `el-tab-pane`（从 `label="Claude Code" name="claude"` 到其匹配的 `</el-tab-pane>`）原样移动到 OpenCode pane 的匹配结束标签之后；其余 pane 内容不变。最终 DOM 顺序必须是 Codex、OpenCode、Claude Code、Cline / OpenAI、Anthropic、Hermes Agent。

- [ ] **Step 4: 用局部样式完成紧凑布局**

删除 `.model-grid` 和 `.model-chip` 规则，加入：

```css
.summary-grid :deep(.el-descriptions__cell) {
  padding: 10px 12px;
}

.model-ranking {
  margin-top: 12px;
  overflow: hidden;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
}

.model-ranking__item {
  display: grid;
  grid-template-columns: 28px minmax(0, 1fr) auto;
  align-items: center;
  gap: 12px;
  min-height: 44px;
  padding: 9px 12px;
  border-bottom: 1px solid var(--crc-border);
}

.model-ranking__item:last-child {
  border-bottom: 0;
}

.model-ranking__position,
.model-ranking__metrics {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.model-ranking__identity {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 8px;
}

.model-ranking__identity strong {
  min-width: 0;
  color: var(--crc-text-strong);
  font-size: 13px;
  overflow-wrap: anywhere;
}

.model-ranking__metrics {
  white-space: nowrap;
}

.config-section-head {
  margin-bottom: 12px;
}
```

在 `@media (max-width: 767px)` 中加入：

```css
.model-ranking__item {
  grid-template-columns: 24px minmax(0, 1fr);
}

.model-ranking__metrics {
  grid-column: 2;
  white-space: normal;
}
```

并从该 media query 的单列选择器中移除 `.model-grid`。

- [ ] **Step 5: 运行定向测试并确认 GREEN**

Run:

```bash
cd frontend
rtk npm test -- tests/views/portal-ui.spec.ts tests/views/portal-integration.spec.ts
```

Expected: 两个测试文件全部 PASS。

- [ ] **Step 6: 提交集成页整理**

```bash
rtk git add frontend/src/views/portal/Integration.vue frontend/tests/views/portal-ui.spec.ts
rtk git diff --cached --check
rtk git commit -m "feat(ui): organize portal integration examples" \
  -m "Constraint: Preserve live catalog and generated client configuration behavior" \
  -m "Confidence: high" \
  -m "Scope-risk: narrow"
```

### Task 2: 修复操练场“自动”提示对比度

**Files:**
- Modify: `frontend/tests/views/portal-ui.spec.ts`
- Modify: `frontend/src/components/PlaygroundSettings.vue:168-194`

- [ ] **Step 1: 写局部颜色覆盖的失败测试**

在测试文件顶部加入组件读取函数：

```ts
const componentSource = (name: string) => readFileSync(
  new URL(`../../src/components/${name}.vue`, import.meta.url),
  'utf8'
)
```

增加独立用例：

```ts
it('keeps automatic playground settings legible in the light theme', () => {
  const settings = componentSource('PlaygroundSettings')

  expect(settings.match(/inactive-text="自动"/g)).toHaveLength(3)
  expect(settings).toContain(
    '.playground-settings :deep(.el-switch:not(.is-checked) .el-switch__inner-wrapper)'
  )
  expect(settings).toContain('color: var(--crc-text-strong)')
})
```

- [ ] **Step 2: 运行测试并确认 RED**

```bash
cd frontend
rtk npm test -- tests/views/portal-ui.spec.ts
```

Expected: FAIL，组件尚无未选中 switch 的局部 selector。

- [ ] **Step 3: 实现最小局部修复**

在 `PlaygroundSettings.vue` scoped style 中加入：

```css
.playground-settings :deep(.el-switch:not(.is-checked) .el-switch__inner-wrapper) {
  color: var(--crc-text-strong);
}
```

- [ ] **Step 4: 运行测试并确认 GREEN**

```bash
cd frontend
rtk npm test -- tests/views/portal-ui.spec.ts
```

Expected: PASS。

- [ ] **Step 5: 提交对比度修复**

```bash
rtk git add frontend/src/components/PlaygroundSettings.vue frontend/tests/views/portal-ui.spec.ts
rtk git diff --cached --check
rtk git commit -m "fix(ui): improve playground automatic label contrast" \
  -m "Constraint: Limit the override to unchecked playground inline switches" \
  -m "Confidence: high" \
  -m "Scope-risk: narrow"
```

### Task 3: 完整验证前端与 release 二进制

**Files:**
- Verify only: `frontend/**`
- Verify only: Rust workspace

- [ ] **Step 1: 运行完整前端测试**

```bash
cd frontend
rtk npm test
```

Expected: 所有 Vitest 文件和用例 PASS，0 failures。

- [ ] **Step 2: 构建生产前端**

```bash
cd frontend
rtk npm run build
```

Expected: `vue-tsc` 与 Vite 均退出 0，`frontend/dist` 生成成功。

- [ ] **Step 3: 运行完整 Rust 测试**

```bash
rtk cargo test --locked --offline
```

Expected: 所有 Rust test suites PASS，0 failures。

- [ ] **Step 4: 构建宿主机 release 二进制**

```bash
rtk cargo build --release --locked --offline
rtk sha256sum target/release/chat-responses-codex
```

Expected: build 退出 0，并记录宿主机二进制 SHA-256。

### Task 4: 注入最终镜像并仅替换 gateway

**Files:**
- Runtime config: `/home/kavin/docker/chat-responses-codex/docker-compose.yml`
- Artifact: `target/release/chat-responses-codex`

- [ ] **Step 1: 记录当前状态并创建不可变回滚标签**

```bash
rtk docker inspect --format '{{.Image}} {{.RestartCount}} {{.State.Health.Status}}' chat-responses-codex
rtk docker tag chat-responses-codex:latest chat-responses-codex:rollback-before-final-ui
```

Expected: 当前容器 healthy；回滚标签创建成功。

- [ ] **Step 2: 通过临时容器注入宿主机二进制**

```bash
FINAL_TAG="chat-responses-codex:final-$(rtk git rev-parse --short HEAD)"
TEMP_CONTAINER="chat-responses-final-image-$(rtk git rev-parse --short HEAD)"
rtk docker create --name "$TEMP_CONTAINER" chat-responses-codex:latest
rtk docker cp target/release/chat-responses-codex "$TEMP_CONTAINER:/usr/local/bin/chat-responses-codex"
rtk docker commit "$TEMP_CONTAINER" "$FINAL_TAG"
rtk docker rm "$TEMP_CONTAINER"
rtk docker tag "$FINAL_TAG" chat-responses-codex:latest
rtk docker run --rm --entrypoint sha256sum "$FINAL_TAG" /usr/local/bin/chat-responses-codex
```

Expected: 镜像内二进制 SHA-256 与 Task 3 宿主机 SHA 完全一致。

- [ ] **Step 3: 仅重建 gateway**

```bash
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml up -d --no-deps gateway
```

Expected: postgres 和 redis 不重建，gateway 使用 `chat-responses-codex:latest` 创建新容器。

- [ ] **Step 4: 等待并核验健康状态**

```bash
rtk docker compose -f /home/kavin/docker/chat-responses-codex/docker-compose.yml ps
rtk docker inspect --format '{{.State.Status}} {{.State.Health.Status}} {{.RestartCount}}' chat-responses-codex
rtk curl -fsS http://127.0.0.1:3000/healthz
```

Expected: gateway `running healthy 0`，`/healthz` 返回 `ok`，数据库与 Redis 保持原运行时长。

### Task 5: 并发实测八类模型并审计运行时

**Files:**
- Execute: `scripts/installed_client_smoke.sh`
- Runtime logs: Docker gateway logs and `/logs/chat-responses-codex.log`

- [ ] **Step 1: 从活动的无限制 downstream 安全读取测试 key 并验证模型目录**

在同一个 `set +x` shell 中读取 key，禁止打印变量：

```bash
set +x
DOWNSTREAM_KEY="$(rtk docker exec chat-responses-codex-postgres \
  psql -U chat_responses_codex -d chat_responses_codex -Atc \
  "SELECT plaintext_key FROM downstreams d WHERE active AND plaintext_key IS NOT NULL AND NOT EXISTS (SELECT 1 FROM downstream_model_allowlist a WHERE a.downstream_id = d.id) ORDER BY id LIMIT 1")"
test -n "$DOWNSTREAM_KEY"
export DOWNSTREAM_KEY
rtk curl -fsS http://127.0.0.1:3000/v1/models \
  -H "Authorization: Bearer $DOWNSTREAM_KEY" \
  | rtk jq -e '[.data[].id] as $ids | all(["glm-5.2","deepseek-v4-flash","deepseek-v4-pro","Qwen/Qwen3.5-122B-A10B","MiniMax-M2.7","kimi-k2.5","grok-4.5","claude-opus-4-7"][]; . as $model | $ids | index($model) != null)'
```

Expected: key 非空且目录断言返回 `true`；命令输出不得包含 key。

- [ ] **Step 2: 以最多四个模型并发运行真实 Codex/OpenCode smoke**

```bash
set +x
OUTDIR="$(mktemp -d)"
STARTED_AT="$(date --iso-8601=seconds)"
models=(
  glm-5.2
  deepseek-v4-flash
  deepseek-v4-pro
  Qwen/Qwen3.5-122B-A10B
  MiniMax-M2.7
  kimi-k2.5
  grok-4.5
  claude-opus-4-7
)
running=0
for model in "${models[@]}"; do
  safe="${model//\//_}"
  (
    BASE_URL=http://127.0.0.1:3000 \
    DOWNSTREAM_KEY="$DOWNSTREAM_KEY" \
    MODEL_SLUG="$model" \
    CLIENTS_JSON='["codex","opencode"]' \
    CLIENT_TIMEOUT_SECONDS=420 \
    rtk bash scripts/installed_client_smoke.sh >"$OUTDIR/$safe.log" 2>&1
  ) &
  running=$((running + 1))
  if [[ "$running" -ge 4 ]]; then
    wait -n || true
    running=$((running - 1))
  fi
done
wait || true
rtk rg -n 'client=|status=failed|status=passed' "$OUTDIR"
```

Expected: 每个模型都有 Codex/OpenCode 的 `text_task` 和 `read_only_tool_task` 记录；任务内容来自脚本中的协议转换分析和只读 `probe.txt`，不使用探活问候。

- [ ] **Step 3: 对失败模型隔离重试一次**

从日志中只选 `status=failed` 的模型，使用相同环境单模型执行一次 `scripts/installed_client_smoke.sh`。记录原始与重试结果；不得无限重试，也不得在模型已经输出有效内容后拼接另一个上游响应。

- [ ] **Step 4: 审计 499、502、首字慢和容器稳定性**

```bash
rtk docker logs --since "$STARTED_AT" chat-responses-codex >"$OUTDIR/gateway.log" 2>&1
rtk rg -n 'status.?=.?499| 499 |upstream_stream_error_event|status.?=.?502|first_meaningful|hedge|ERROR|panic' "$OUTDIR/gateway.log"
rtk docker inspect --format '{{.State.Status}} {{.State.Health.Status}} {{.RestartCount}}' chat-responses-codex
rtk curl -fsS http://127.0.0.1:3000/healthz
```

Expected: 汇总每个异常而不是要求搜索命令必须命中；最终容器 `running healthy`、restart count 为 0、healthz 为 `ok`。若出现 499，关联下游取消；若出现 502，区分首输出前失败与有效输出后的上游晚流错误；首字慢模型记录 hedge 是否实际启动以及候选容量情况。

- [ ] **Step 5: 最终工作树与提交核验**

```bash
rtk git status --short --branch
rtk git log -6 --oneline --decorate
```

Expected: 本轮生产改动均已提交；仅保留用户原有 `frontend/tests/router/index.spec.ts` 与 `tests/troubleshooting.rs` 未提交修改。
