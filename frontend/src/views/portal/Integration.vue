<template>
  <div class="integration-container">
    <el-card>
      <template #header>
        <h2>集成示例</h2>
      </template>

      <el-tabs v-model="activeTab">
        <!-- Codex Tab -->
        <el-tab-pane label="Codex" name="codex">
          <div class="integration-section">
            <h3>Codex 配置指南</h3>
            
            <div class="steps">
              <h4>步骤 1: 复制配置文件到本地</h4>
              <pre class="code-block">mkdir -p ~/.codex
cp templates/codex/config.toml.example ~/.codex/config.toml
cp templates/codex/model-catalog.json ~/.codex/model-catalog.json</pre>
              <el-button @click="copyCode(codexCopyCmd)" class="copy-btn" size="small">复制</el-button>
            </div>

            <div class="steps">
              <h4>步骤 2: 修改 ~/.codex/config.toml</h4>
              <p>将以下配置复制到你的 Codex 配置文件中，替换 YOUR_API_KEY 为你的下游密钥：</p>
              <pre class="code-block">{{ codexConfig }}</pre>
              <el-button @click="copyCode(codexConfig)" class="copy-btn" size="small">复制</el-button>
            </div>

            <div class="steps">
              <h4>步骤 3: 在网关配置上游和下游</h4>
              <p>打开网关管理页面配置上游模型和下游密钥：</p>
              <ul>
                <li>访问 <code>http://网关地址:3001/admin</code></li>
                <li>在 Upstreams 中添加上游模型配置</li>
                <li>在 Downstreams 中创建下游密钥（Codex 使用此密钥）</li>
              </ul>
            </div>

            <div class="steps">
              <h4>步骤 4: 启动 Codex 并选择模型</h4>
              <p>配置完成后，启动 Codex 并选择你在 model-catalog.json 中定义的模型（如 ZhipuAI/GLM-5, MiniMax/MiniMax-M2.7, deepseek-ai/DeepSeek-R1-0528）。</p>
            </div>
          </div>
        </el-tab-pane>

        <!-- OpenCode Tab -->
        <el-tab-pane label="OpenCode" name="opencode">
          <div class="integration-section">
            <h3>OpenCode 配置指南</h3>
            
            <div class="steps">
              <h4>步骤 1: 创建配置文件</h4>
              <p>OpenCode 配置文件通常位于项目根目录或 ~/.opencode/opencode.json：</p>
              <pre class="code-block">{{ opencodeConfig }}</pre>
              <el-button @click="copyCode(opencodeConfig)" class="copy-btn" size="small">复制</el-button>
            </div>

            <div class="steps">
              <h4>步骤 2: 替换 YOUR_API_KEY</h4>
              <p>将 YOUR_API_KEY 替换为你在网关管理页面创建的下游密钥。</p>
            </div>

            <div class="steps">
              <h4>步骤 3: 确保模型名称一致</h4>
              <p>OpenCode 使用的模型名称必须与网关配置的 supported_models 中的模型名称一致。</p>
            </div>

            <div class="steps">
              <h4>步骤 4: 启动 OpenCode</h4>
              <p>配置完成后，启动 OpenCode 即可使用网关提供的模型服务。</p>
            </div>
          </div>
        </el-tab-pane>

        <!-- Claude Code Tab -->
        <el-tab-pane label="Claude Code" name="claude">
          <div class="integration-section">
            <h3>Claude Code 配置指南</h3>
            
            <div class="steps">
              <h4>步骤 1: 找到配置文件位置</h4>
              <p>Claude Desktop 配置文件位置：</p>
              <ul>
                <li>macOS: <code>~/Library/Application Support/Claude/claude_desktop_config.json</code></li>
                <li>Windows: <code>%APPDATA%\Claude\claude_desktop_config.json</code></li>
                <li>Linux: <code>~/.config/Claude/claude_desktop_config.json</code></li>
              </ul>
            </div>

            <div class="steps">
              <h4>步骤 2: 编辑配置文件</h4>
              <p>将以下配置添加到 claude_desktop_config.json：</p>
              <pre class="code-block">{{ claudeConfig }}</pre>
              <el-button @click="copyCode(claudeConfig)" class="copy-btn" size="small">复制</el-button>
            </div>

            <div class="steps">
              <h4>步骤 3: 替换 YOUR_API_KEY</h4>
              <p>将 YOUR_API_KEY 替换为你在网关创建的下游密钥。</p>
            </div>

            <div class="steps">
              <h4>步骤 4: 重启 Claude Desktop</h4>
              <p>保存配置后重启 Claude Desktop 应用，即可使用网关提供的模型。</p>
            </div>

            </div>
        </el-tab-pane>

        <!-- Python SDK Tab -->
        <el-tab-pane label="Python SDK" name="python">
          <div class="integration-section">
            <h3>Python SDK 使用示例</h3>
            
            <div class="steps">
              <h4>步骤 1: 安装 OpenAI Python SDK</h4>
              <pre class="code-block">pip install openai</pre>
              <el-button @click="copyCode('pip install openai')" class="copy-btn" size="small">复制</el-button>
            </div>

            <div class="steps">
              <h4>步骤 2: 创建 Python 脚本</h4>
              <p>将以下代码复制到你的 Python 脚本中，替换 YOUR_API_KEY 为你的下游密钥：</p>
              <pre class="code-block">{{ pythonSdkExample }}</pre>
              <el-button @click="copyCode(pythonSdkExample)" class="copy-btn" size="small">复制</el-button>
            </div>

            <div class="steps">
              <h4>步骤 3: 运行脚本</h4>
              <p>确保网关服务正在运行，然后执行你的 Python 脚本即可。</p>
            </div>
          </div>
        </el-tab-pane>
      </el-tabs>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue'
import { ElMessage } from 'element-plus'

const activeTab = ref('codex')
const gatewayUrl = 'http://localhost:3001/v1'

const codexCopyCmd = `mkdir -p ~/.codex
cp templates/codex/config.toml.example ~/.codex/config.toml
cp templates/codex/model-catalog.json ~/.codex/model-catalog.json`

const codexConfig = computed(() => `model_provider = "gateway"
model = "ZhipuAI/GLM-5"
review_model = "ZhipuAI/GLM-5"
model_reasoning_effort = "high"
disable_response_storage = true
model_catalog_json = "/home/YOUR_USERNAME/.codex/model-catalog.json"

[features]
skill_mcp_dependency_install = true
tool_suggest = true

[model_providers.gateway]
name = "chat-responses-codex"
base_url = "${gatewayUrl}"
wire_api = "responses"
requires_openai_auth = true

# 在网关管理页面创建下游密钥后，
# Codex 请求时会自动使用 Bearer YOUR_API_KEY 鉴权`)

const opencodeConfig = computed(() => `{
  "model": "ZhipuAI/GLM-5",
  "base_url": "${gatewayUrl}",
  "api_key": "YOUR_API_KEY",
  "provider": "openai-compatible",
  "features": {
    "streaming": true,
    "tools": true
  }
}`)

const claudeConfig = computed(() => `{
  "mcpServers": {
    "chat-gateway": {
      "command": "node",
      "args": ["mcp-client.js"],
      "env": {
        "OPENAI_API_KEY": "YOUR_API_KEY",
        "OPENAI_BASE_URL": "${gatewayUrl.replace('/v1', '')}"
      }
    }
  }
}

# 或直接使用 API 方式 (如果 Claude Code 支持 OpenAI compatible):
# {
#   "apiConfiguration": {
#     "baseURL": "${gatewayUrl}",
#     "apiKey": "YOUR_API_KEY",
#     "provider": "openai-compatible"
#   }
# }`)

const pythonSdkExample = computed(() => `from openai import OpenAI

# 初始化客户端，指向网关地址
client = OpenAI(
    api_key="YOUR_API_KEY",  # 替换为你的下游密钥
    base_url="${gatewayUrl}"  # 网关地址
)

# 发送请求
response = client.chat.completions.create(
    model="ZhipuAI/GLM-5",  # 使用网关支持的模型
    messages=[
        {"role": "user", "content": "Hello, how are you?"}
    ],
    stream=True  # 支持流式输出
)

# 处理响应
for chunk in response:
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="")`)

const copyCode = (text: string) => {
  navigator.clipboard.writeText(text)
  ElMessage.success('已复制到剪贴板')
}
</script>

<style scoped>
.integration-container {
  padding: 20px;
}

h2 {
  margin: 0;
}

.integration-section {
  padding: 20px 0;
}

.integration-section h3 {
  margin: 0 0 20px 0;
  color: #303133;
}

.steps {
  margin-bottom: 24px;
  padding: 16px;
  background: #f5f7fa;
  border-radius: 4px;
}

.steps h4 {
  margin: 0 0 12px 0;
  color: #409eff;
  font-size: 15px;
}

.steps p {
  margin: 0 0 8px 0;
  color: #606266;
  line-height: 1.6;
}

.steps ul {
  margin: 0;
  padding-left: 20px;
  color: #606266;
}

.steps li {
  margin-bottom: 6px;
}

.steps code {
  background: #e4e7ed;
  padding: 2px 6px;
  border-radius: 3px;
  color: #303133;
}

.code-block {
  background: #2d2d2d;
  color: #ccc;
  padding: 16px;
  border-radius: 4px;
  overflow-x: auto;
  font-family: 'Consolas', 'Monaco', 'Courier New', monospace;
  font-size: 14px;
  line-height: 1.5;
  margin: 12px 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.copy-btn {
  margin-top: 8px;
}

.python-sdk-section {
  padding: 20px 0;
}

.python-sdk-section h3 {
  margin: 0 0 16px 0;
  color: #303133;
}

.python-sdk-section p {
  margin: 0 0 12px 0;
  color: #606266;
}

.el-divider {
  margin: 24px 0;
}
</style>
