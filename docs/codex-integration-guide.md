# Codex 集成使用指南

这份指南说明如何把 Codex 接到本项目的网关上，并让 GLM、MiniMax、DeepSeek 这类上游模型通过网关正常工作。

核心思路只有一句话：

- Codex 只连网关。
- 网关再连各家上游模型。
- 模型名必须在 Codex、网关、上游三处保持完全一致。

## 客户端兼容矩阵

网关同时暴露 `/v1/chat/completions`、`/v1/responses`、`/v1/models` 和 `/v1/messages`。不同客户端根据自己支持的协议族选对应的端点：

| 客户端 | 协议族 | 端点 | 配置方式 |
|--------|--------|------|----------|
| Codex | Responses | `/v1/responses` | `config.toml` + `model-catalog.json` + `codex login --with-api-key` |
| Cline | Chat Completions | `/v1/chat/completions` | 门户 Cline preset（`baseURL` + `apiKey` + `model`） |
| OpenCode | Chat Completions | `/v1/chat/completions` | `opencode.json` |
| Claude Code | Messages | `/v1/messages` | `settings.json`（含 `ANTHROPIC_BASE_URL` 等环境变量） |
| Hermes | Chat Completions | `/v1/chat/completions` | `config.yaml` |
| 其他 Anthropic 兼容客户端 | Messages | `/v1/messages` | 门户 Anthropic preset（`baseURL` + `apiKey` + `model`） |

如果你已经登录了门户，优先打开 `<gateway_origin>/portal/integration`。页面会自动读取当前下游 key、当前网关 URL 和 live `/v1/models`，直接生成 Codex / OpenCode / Claude Code / Cline / Anthropic 兼容 的可复制配置。下面这些手工步骤保留着，方便你离线配置或做模板化部署。

## 先看整体结构

推荐的链路是：

`Codex -> chat-responses-codex -> 上游模型`

你不要把 Codex 直接指向上游厂商地址。这样做会绕过网关的协议转换、模型路由、Key 管理和限流。

## 你需要改哪些地方

一共有三类配置，再加一个推荐入口，分别在不同地方改：

1. Codex 本地配置：`~/.codex/config.toml`
2. Codex 模型目录：`~/.codex/model-catalog.json`
3. 网关状态：`STATE_PATH` 指向的 JSON 文件，通常通过网关管理页维护
4. 门户集成页：`<gateway_origin>/portal/integration`

项目里已经准备了客户端配置模板：

- [codex-config.toml.example](../templates/codex/config.toml.example)
- [gateway-state.example.json](../templates/state/gateway-state.example.json)
- [opencode.json](../templates/opencode/opencode.json)
- [claude-code-settings.json](../templates/claude-code/settings.json)
- [hermes-config.yaml](../templates/hermes/config.yaml)

Codex catalog 必须从已配置的网关读取。下面的流程不会把下游 key 写进 shell 历史，并且只会在 `.models` 是非空数组时更新本地目录：

```bash
(
  set -euo pipefail
  mkdir -p ~/.codex
  catalog_tmp="$(mktemp)"
  trap 'rm -f "$catalog_tmp"; unset CHAT2RESPONSES_DOWNSTREAM_KEY' EXIT
  read -rsp 'Gateway downstream key: ' CHAT2RESPONSES_DOWNSTREAM_KEY
  printf '\n'
  curl -fsS '<gateway_origin>/v1/models?client_version=0.144.4' \
    -H "Authorization: Bearer ${CHAT2RESPONSES_DOWNSTREAM_KEY}" \
    > "$catalog_tmp"
  jq -e '(.models | type == "array") and (.models | length > 0)' \
    "$catalog_tmp" >/dev/null
  install -m 600 "$catalog_tmp" ~/.codex/model-catalog.json
)
```

这个响应中的能力字段来自网关选择的 live catalog witness。不要按模型名手写或补全工具、图像、推理等级等能力。

## 一把点亮版

如果你已经有网关地址、上游 key 和管理员账号，最快的做法是按这个顺序来。

### 1. 先把 Codex 模板放到本机

```bash
mkdir -p ~/.codex
cp templates/codex/config.toml.example ~/.codex/config.toml
```

然后执行上一节的 live catalog 获取和非空校验流程。

### 2. 把 `~/.codex/config.toml` 改成这样

```toml
model_provider = "gateway"
model = "<model_slug>"
review_model = "<model_slug>"
model_reasoning_effort = "none"
disable_response_storage = true
model_catalog_json = "model-catalog.json"
cli_auth_credentials_store = "file"

[features]
skill_mcp_dependency_install = true
tool_suggest = true
multi_agent = true

[agents]
max_threads = 8
max_depth = 3
# max_threads controls concurrent agent threads; max_depth controls nested delegation depth.
# These local limits do not override gateway quota.

[model_providers.gateway]
name = "Chat Responses Gateway"
base_url = "<gateway_origin>/v1"
wire_api = "responses"
requires_openai_auth = true
web_search = "disabled"
```

完成配置后可运行 `codex --strict-config doctor --summary` 检查配置是否符合当前 Codex 版本。

把 `<gateway_origin>` 换成你的网关根地址，本机就填你本机监听的网关地址，远程就填你反向代理或公网域名对应的根地址。

### 3. 网关上配置上游

在网关管理页打开：

- `<gateway_origin>/admin`

进入 `Upstreams`，给每个上游填：

- `base_url`
- `api_key`
- `protocol`
- `supported_models`

下面这三个模型名只是示例，按你的实际上游模型替换也可以；这里只是演示如何把模型名写进 `supported_models`。

- `ZhipuAI/GLM-5`
- `MiniMax/MiniMax-M2.7`
- `deepseek-ai/DeepSeek-R1-0528`

### 4. 网关上配置下游

在同一个管理页里进入 `Downstreams`，新建一个下游 key。

这个下游 key 是 Codex 实际要用的访问凭证。

### 5. 再启动 Codex

Codex 里选模型时，直接选你在目录里写的 slug，例如：

- `ZhipuAI/GLM-5`
- `MiniMax/MiniMax-M2.7`
- `deepseek-ai/DeepSeek-R1-0528`

## 这三个地方分别在哪改

1. Codex 本地配置：`~/.codex/config.toml`
2. Codex 模型目录：`~/.codex/model-catalog.json`
3. 网关状态：`STATE_PATH` 对应的 JSON，或者直接通过管理页改

## 第一步: 启动网关

网关是实际接收 Codex 请求的服务。

### 1.1 本地启动

```bash
cargo run
```

默认会使用这些环境变量：

- `BIND_ADDR=0.0.0.0:3001`
- `STATE_PATH=data/state.json`
- `LOG_PATH=logs/chat-responses-codex.log`
- `ADMIN_USERNAME=admin`
- `ADMIN_PASSWORD=admin`
- `MODEL_PROBE_REFRESH_INTERVAL_SECONDS=15`

### 1.2 Docker 启动

如果你用 Docker，建议直接看 [DEPLOYMENT.md](../DEPLOYMENT.md)。

通常需要：

- 把 `STATE_PATH` 挂载到持久化目录
- 把 `LOG_PATH` 挂载到日志目录
- 设置强一点的 `ADMIN_PASSWORD`

### 1.3 网关地址怎么填

如果网关在本机：

- `<gateway_origin>/v1`

如果网关在其他机器：

- `<gateway_origin>/v1`
- 如果你走了反向代理，也把代理后的根地址放进 `<gateway_origin>`

Codex 里填的是网关地址，不是上游厂商地址。

## Capability 与降级规则

生产路由不会检查 GLM、DeepSeek、MiniMax、Kimi 或 Qwen 的名字。模型语义由外部 policy 提供，实际 wire 支持由精确 upstream/runtime slug/protocol probe profile 提供。文档能力不能替代 probe 证据。

- 能原样表达时 preserve。
- 能无损映射时 adapt，例如 Responses namespace/custom tool 到 Chat function tool，并在返回时恢复 identity。
- 只有明确允许的可选项才 downgrade，并通过响应头和 usage metadata 报告。
- 显式选择的 hosted tool、最后一个必需工具或其他无法保留的必需能力在调度前 reject。

部署模板、Qwen VLM 渲染和四客户端矩阵见 [PROTOCOL_COMPATIBILITY.md](PROTOCOL_COMPATIBILITY.md) 与 [DEPLOYMENT.md](../DEPLOYMENT.md)。

## 第二步: 配置网关里的上游模型

这一步是在网关里做，不是在 Codex 里做。

### 2.1 在哪配

有两种方式：

1. 网关管理页
2. 直接编辑 `STATE_PATH` 对应的 JSON 文件

推荐先用管理页，改完再导出或备份 JSON。

### 2.2 管理页入口

打开：

- `<gateway_origin>/admin`

登录后去：

- `Upstreams`
- `Downstreams`

### 2.3 上游怎么填

每个上游需要填这些字段：

- `name`：显示名
- `base_url`：上游 OpenAI 风格 API 地址
- `api_key`：上游密钥
- `protocol`：`ChatCompletions` 或 `Responses`
- `supported_models`：这个上游对外暴露给 Codex 的模型 slug 列表
例子：

```json
{
  "id": "<upstream_id>",
  "name": "<upstream_name>",
  "base_url": "<upstream_base_url>",
  "api_key": "<upstream_api_key>",
  "protocol": "ChatCompletions",
  "supported_models": ["<model_slug>"],
  "active": true,
  "failure_count": 0
}
```

### 2.4 这里最重要的点

`supported_models` 里请直接写上游返回的原始模型 ID，哪怕带 vendor 前缀或大小写混合。

例如：

- Codex 目录、网关 `supported_models`、上游 `/v1/models` 返回的 `id` 必须完全一致
- 不再使用 `model_aliases` 做大小写或别名映射
- `supported_models` 请直接填写上游真实返回的模型名

### 2.5 什么时候用 `ChatCompletions`

如果上游只支持传统 Chat Completions，就把 `protocol` 设成 `ChatCompletions`。

如果上游本身支持 Responses 且你要走 Responses 协议，就设成 `Responses`。

你之前碰到的：

- `streaming requests require an upstream that supports the requested protocol`

通常就是网关路由到的上游协议类型不对，或者模型没有在任何活跃上游的 `supported_models` 里出现。

补充一点：如果上游只支持 `ChatCompletions`，网关会尽量把标准 `function` 工具转成 Chat 兼容格式，并按阶段降级 Responses 扩展语义；`web_search`、`file_search`、`computer_use` 这类 Responses 内置工具在 chat-only 路径下会做 best-effort 降级，但不保证保留原始 Responses 语义。

## 第三步: 配置网关里的下游 key

Codex 不应该直接用上游厂商 key。它应该用网关发的下游 key。

### 3.1 在哪配

同样是在网关管理页：

- `<gateway_origin>/admin`
- 进入 `Downstreams`

### 3.2 下游 key 是什么

下游 key 是网关发给 Codex 的访问凭证。

Codex 请求网关时，实际发送的是：

- `Authorization: Bearer <downstream_key>`

### 3.3 下游允许哪些模型

在下游里填 `model_allowlist`。

如果你想让某个下游只能看到三个模型，就写：

- `ZhipuAI/GLM-5`
- `MiniMax/MiniMax-M2.7`
- `deepseek-ai/DeepSeek-R1-0528`

如果留空，一般表示不过滤模型，只要网关里可用就给。

## 第四步: 配置 Codex

这是你本机上的配置，位置是：

- `~/.codex/config.toml`

### 4.1 直接复制模板

你可以先把项目里的模板复制过去，再改值：

- [codex-config.toml.example](../templates/codex/config.toml.example)

### 4.2 关键字段

最关键的是这些：

```toml
model_provider = "gateway"
model = "<model_slug>"
review_model = "<model_slug>"
model_reasoning_effort = "none"
disable_response_storage = true
model_catalog_json = "model-catalog.json"
cli_auth_credentials_store = "file"

[features]
multi_agent = true

[agents]
max_threads = 8
max_depth = 3

[model_providers.gateway]
name = "Chat Responses Gateway"
base_url = "<gateway_origin>/v1"
wire_api = "responses"
requires_openai_auth = true
web_search = "disabled"
```

### 4.3 每个字段是什么意思

- `model_provider`：使用哪个 provider
- `model`：日常对话主模型
- `review_model`：审查/评审模型
- `model_reasoning_effort`：推理强度。门户会使用所选模型目录项的 `default_reasoning_level`；没有验证到可配置推理控制时使用 `none`
- `disable_response_storage`：关闭响应存储
- `model_catalog_json`：Codex 模型目录文件路径，按相对路径解析
- `base_url`：网关根地址加 `/v1`
- `wire_api = "responses"`：让 Codex 按 Responses 协议跟网关通信
- `requires_openai_auth = true`：使用 OpenAI 风格的 Bearer 鉴权头

### 4.4 你现在最容易配错的地方

1. `model_catalog_json` 路径写错
2. `base_url` 写成了上游厂商地址，而不是网关根地址
3. `model_catalog_json` 不在 `~/.codex/config.toml` 同目录
4. `model` 和 `model_slug` 不一致
5. 模型名被手动转成小写，或者改成了别名

## 第五步: 配置 Codex 模型目录

这个文件也在你本机上，位置由 `~/.codex/config.toml` 里的 `model_catalog_json` 指定。

### 5.1 这个文件是干什么的

它告诉 Codex：

- 有哪些模型可选
- 模型怎么显示
- 默认推理等级是什么
- 是否支持工具调用、搜索、流式等

这些字段必须来自 live catalog witness。不要手动声明搜索、工具、图像或推理能力；手写的乐观能力可能让 Codex 发出当前路由无法执行的请求。

### 5.2 为什么必须和网关一致

Codex 会根据这个目录决定模型是否存在。

如果目录、网关 `supported_models` 和上游 `/v1/models` 返回的 `id` 不完全一致，就会出问题。

### 5.3 你要改什么

重新执行前面的 live catalog 获取流程，然后从已验证的目录中选择模型：

```bash
jq -r '.models[].slug' ~/.codex/model-catalog.json
```

把其中一个原始 slug 写入 `~/.codex/config.toml` 的 `model` 和 `review_model`。`model_catalog_json` 保持为 `model-catalog.json`；不要把 slug 转成小写、改成别名或手动编辑目录里的能力字段。

## 第六步: 如果你想直接用模板

建议按这个顺序做：

1. 在网关上启动服务
2. 打开 `<gateway_origin>/admin`
3. 配好上游模型
4. 配好下游 key
5. 把 `templates/codex/config.toml.example` 复制到 `~/.codex/config.toml`
6. 使用下游 key 从 live `/v1/models?client_version=0.144.4` 获取目录，验证 `.models` 非空后写入 `~/.codex/model-catalog.json`
7. 确认 `base_url` 是网关地址
8. 确认 `model` 和 `review_model` 都是目录里真实存在的 slug

## 第七步: 怎么验证配置是否成功

### 7.1 先测网关健康

```bash
curl -i <gateway_origin>/healthz
```

### 7.2 再测管理页

```bash
curl -u admin:<admin_password> <gateway_origin>/admin
```

### 7.3 再测下游模型列表

拿到下游 key 以后：

```bash
curl -s \
  -H "Authorization: Bearer <downstream_key>" \
  <gateway_origin>/v1/models
```

### 7.4 再测 chat 请求

```bash
curl -s \
  -H "Authorization: Bearer <downstream_key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model_slug>","messages":[{"role":"user","content":"hello"}]}' \
  <gateway_origin>/v1/chat/completions
```

### 7.5 再测 Codex

Codex 启动后选你在目录里写的模型，比如：

- `ZhipuAI/GLM-5`
- `MiniMax/MiniMax-M2.7`
- `deepseek-ai/DeepSeek-R1-0528`

如果 Codex 能正常发起请求，说明配置链路通了。

## 常见报错

### 1. `streaming requests require an upstream that supports the requested protocol`

含义：

- Codex 发了流式请求
- 但网关选中的上游协议不匹配，或者上游不支持当前 wire/protocol 组合

怎么查：

- 看网关 `Upstreams` 里这个模型挂在哪个 `protocol`
- 看 Codex 是否走的是 `responses`
- 看模型是否实际被路由到了支持该协议的上游

### 2. `model metadata for xxx not found`

含义：

- `~/.codex/config.toml` 里的 `model_catalog_json` 路径不对
- 或者 `slug` 不存在

怎么查：

- 检查 `model_catalog_json` 是否指向正确文件
   - 检查 `model = "..."` 是否和 `~/.codex/model-catalog.json` 里的 `slug` 完全一致

### 3. `skill descriptions were shortened to fit the skills context budget`

这个一般不是模型接入错误。

它表示 Codex 的技能上下文太多，部分描述被压缩了。

通常可以先忽略，除非你发现技能自动加载异常。

### 4. 上游并发满导致 429

如果错误里明确是 `429`，而且上游文案提到了 `concurrency`、`busy`、`limit exceeded` 这类并发饱和信号，先调这两个参数：

- `UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS`
- `UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS`

这组参数控制网关遇到并发饱和时会不会换一个候选上游、以及换候选前等多久。它和普通限流 429 不是一回事：

- 普通限流 429 看 `UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS`
- 普通限流窗口看 `UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS`
- `Retry-After` 过大的保护上限看 `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS`

### 5. 能用 Python `requests`，但 Codex 不行

这通常说明：

- 上游本身是通的
- 但是 Codex 侧的协议、目录、模型名、鉴权或网关路由有一处不一致

优先检查：

1. `~/.codex/config.toml`
2. `~/.codex/model-catalog.json`，必要时重新执行 live catalog 获取和非空校验
3. 网关 `STATE_PATH`
4. 网关上游 `protocol`
5. 网关上游 `supported_models`

## 推荐的实际落地方式

如果你是第一次接，建议按这个组合来：

- Codex 本机：只放 `~/.codex/config.toml`
- 模型目录：放 `~/.codex/model-catalog.json`
- 网关机器：运行 `chat-responses-codex`
- 网关管理页：配置上游和下游

补充两条实战规则：

- 同一个模型挂在多个上游账号上时，网关会自动按请求压力分摊，不需要拆成不同的模型名。
- 如果上游因为并发满返回 429，可以调 `UPSTREAM_CONCURRENCY_RETRY_ATTEMPTS` 和 `UPSTREAM_CONCURRENCY_RETRY_BACKOFF_MS`；常规限流 429 则看 `UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS`、`UPSTREAM_RATE_LIMIT_RETRY_WINDOW_SECONDS` 和 `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS`。

这样职责最清楚：

- Codex 只管发请求
- 网关管协议转换和路由
- 上游只提供模型能力

## 相关文件

- [README.md](../README.md)
- [DEPLOYMENT.md](../DEPLOYMENT.md)
- [codex-config.toml.example](../templates/codex/config.toml.example)
- [gateway-state.example.json](../templates/state/gateway-state.example.json)
