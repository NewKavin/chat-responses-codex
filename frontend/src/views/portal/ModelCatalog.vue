<template>
  <div class="model-catalog-container">
    <el-card>
      <template #header>
        <div class="header">
          <h2>模型目录</h2>
          <el-button @click="loadData" :loading="loading" circle>
            <el-icon><Refresh /></el-icon>
          </el-button>
        </div>
      </template>

      <div v-loading="loading">
        <el-table :data="models" stripe>
          <el-table-column prop="model" label="模型名称" width="200" />
          <el-table-column label="今日使用" width="120">
            <template #default="{ row }">
              {{ row.today_count }} 次
            </template>
          </el-table-column>
          <el-table-column label="今日 Token" width="150">
            <template #default="{ row }">
              {{ row.today_tokens.toLocaleString() }}
            </template>
          </el-table-column>
          <el-table-column label="本月使用" width="120">
            <template #default="{ row }">
              {{ row.month_count }} 次
            </template>
          </el-table-column>
          <el-table-column label="本月 Token" width="150">
            <template #default="{ row }">
              {{ row.month_tokens.toLocaleString() }}
            </template>
          </el-table-column>
          <el-table-column label="平均耗时" width="120">
            <template #default="{ row }">
              {{ row.avg_latency_ms }}ms
            </template>
          </el-table-column>
          <el-table-column label="成功率" width="100">
            <template #default="{ row }">
              <el-tag :type="getSuccessRateType(row.success_rate)">
                {{ (row.success_rate * 100).toFixed(1) }}%
              </el-tag>
            </template>
          </el-table-column>
          <el-table-column label="操作" width="120">
            <template #default="{ row }">
              <el-button size="small" @click="showExample(row.model)">
                接入示例
              </el-button>
            </template>
          </el-table-column>
        </el-table>
      </div>
    </el-card>

    <!-- 接入示例对话框 -->
    <el-dialog v-model="exampleDialogVisible" :title="`${selectedModel} 接入示例`" width="700px">
      <el-tabs v-model="exampleTab">
        <el-tab-pane label="cURL" name="curl">
          <pre class="code-block">{{ curlExample }}</pre>
          <el-button @click="copyExample(curlExample)" class="copy-btn">复制</el-button>
        </el-tab-pane>
        <el-tab-pane label="Python" name="python">
          <pre class="code-block">{{ pythonExample }}</pre>
          <el-button @click="copyExample(pythonExample)" class="copy-btn">复制</el-button>
        </el-tab-pane>
        <el-tab-pane label="JavaScript" name="javascript">
          <pre class="code-block">{{ javascriptExample }}</pre>
          <el-button @click="copyExample(javascriptExample)" class="copy-btn">复制</el-button>
        </el-tab-pane>
      </el-tabs>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { ElMessage } from 'element-plus'
import { Refresh } from '@element-plus/icons-vue'
import { portalApi } from '@/api/portal'
import type { ModelStats } from '@/types'

const loading = ref(false)
const models = ref<ModelStats[]>([])
const exampleDialogVisible = ref(false)
const exampleTab = ref('curl')
const selectedModel = ref('')

const getSuccessRateType = (rate: number) => {
  if (rate >= 0.95) return 'success'
  if (rate >= 0.8) return 'warning'
  return 'danger'
}

const apiEndpoint = computed(() => {
  return window.location.origin + '/v1/chat/completions'
})

const apiKey = computed(() => {
  return 'sk-your-api-key'
})

const curlExample = computed(() => {
  return `curl ${apiEndpoint.value} \\
  -H "Content-Type: application/json" \\
  -H "Authorization: Bearer ${apiKey.value}" \\
  -d '{
    "model": "${selectedModel.value}",
    "messages": [
      {
        "role": "user",
        "content": "Hello!"
      }
    ]
  }'`
})

const pythonExample = computed(() => {
  return `import openai

client = openai.OpenAI(
    api_key="${apiKey.value}",
    base_url="${apiEndpoint.value.replace('/v1/chat/completions', '/v1')}"
)

response = client.chat.completions.create(
    model="${selectedModel.value}",
    messages=[
        {"role": "user", "content": "Hello!"}
    ]
)

print(response.choices[0].message.content)`
})

const javascriptExample = computed(() => {
  return `import OpenAI from 'openai';

const client = new OpenAI({
  apiKey: '${apiKey.value}',
  baseURL: '${apiEndpoint.value.replace('/v1/chat/completions', '/v1')}'
});

const response = await client.chat.completions.create({
  model: '${selectedModel.value}',
  messages: [
    { role: 'user', content: 'Hello!' }
  ]
});

console.log(response.choices[0].message.content);`
})

const loadData = async () => {
  try {
    loading.value = true
    const { data } = await portalApi.getModels()
    models.value = data
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const showExample = (model: string) => {
  selectedModel.value = model
  exampleDialogVisible.value = true
}

const copyExample = (text: string) => {
  navigator.clipboard.writeText(text)
  ElMessage.success('已复制到剪贴板')
}

onMounted(() => {
  loadData()
})
</script>

<style scoped>
.model-catalog-container {
  padding: 20px;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.header h2 {
  margin: 0;
}

.code-block {
  background: #f5f5f5;
  padding: 15px;
  border-radius: 4px;
  overflow-x: auto;
  font-family: 'Courier New', monospace;
  font-size: 13px;
  line-height: 1.5;
  margin: 0;
}

.copy-btn {
  margin-top: 10px;
}
</style>
