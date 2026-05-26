<template>
  <div class="key-management-container">
    <el-card>
      <template #header>
        <h2>秘钥管理</h2>
      </template>

      <div v-loading="loading" class="key-section">
        <el-alert
          type="info"
          :closable="false"
          class="helper-text"
        >
          这里显示的是您的完整秘钥，可用于配置客户端。如需更换秘钥，请点击"轮换秘钥"。
        </el-alert>

        <div class="key-display">
          <el-descriptions :column="1" border>
            <el-descriptions-item label="秘钥">
              <div class="key-cell">
                <code v-if="keyPrefix">{{ keyPrefix }}</code>
                <span v-else class="no-key">未设置秘钥</span>
                <el-button-group>
                  <el-button size="small" @click="copyKey(keyPrefix)" :disabled="!keyPrefix">
                    复制秘钥
                  </el-button>
                  <el-button size="small" type="warning" @click="handleRotate" :disabled="!keyPrefix">
                    轮换秘钥
                  </el-button>
                </el-button-group>
              </div>
            </el-descriptions-item>
          </el-descriptions>
        </div>

        <el-alert
          type="warning"
          :closable="false"
          class="helper-text"
        >
          轮换秘钥后，旧秘钥将立即失效。新秘钥只会显示一次，请务必妥善保存。
        </el-alert>
      </div>
    </el-card>

    <el-dialog v-model="rotateDialogVisible" title="秘钥轮换成功" width="500px">
      <el-alert type="success" :closable="false" show-icon>
        <template #title>
          新秘钥已生成，请立即保存！此秘钥只显示一次。
        </template>
      </el-alert>
      <div class="new-key-container">
        <el-input v-model="newKey" readonly>
          <template #append>
            <el-button type="primary" @click="copyFullKey(newKey)">复制秘钥</el-button>
          </template>
        </el-input>
      </div>
      <el-alert
        type="warning"
        :closable="false"
        class="helper-text"
      >
        这是完整的秘钥，可用于门户登录。请立即复制并妥善保存，关闭后无法再次查看。
      </el-alert>
      <template #footer>
        <el-button type="primary" @click="closeRotateDialog">我已保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { portalApi } from '@/api/portal'

const loading = ref(false)
const keyPrefix = ref<string | null>(null)
const rotateDialogVisible = ref(false)
const newKey = ref('')

const loadData = async () => {
  try {
    loading.value = true
    const { data } = await portalApi.getKey()
    keyPrefix.value = data.plaintext_key
  } catch (error) {
    ElMessage.error('加载秘钥信息失败')
  } finally {
    loading.value = false
  }
}

const copyKey = async (key: string | null) => {
  if (!key) return
  try {
    await navigator.clipboard.writeText(key)
    ElMessage.success('已复制到剪贴板')
  } catch {
    const textArea = document.createElement('textarea')
    textArea.value = key
    textArea.style.position = 'fixed'
    textArea.style.left = '-9999px'
    document.body.appendChild(textArea)
    textArea.focus()
    textArea.select()
    try {
      document.execCommand('copy')
      ElMessage.success('已复制到剪贴板')
    } catch {
      ElMessage.error('复制失败，请手动复制')
    }
    document.body.removeChild(textArea)
  }
}

const copyFullKey = async (key: string) => {
  await copyKey(key)
}

const handleRotate = async () => {
  try {
    await ElMessageBox.confirm(
      '确定要轮换秘钥吗？轮换后旧秘钥将立即失效，请确保您已不再使用旧秘钥。',
      '确认轮换',
      {
        type: 'warning',
        confirmButtonText: '确定轮换',
        cancelButtonText: '取消'
      }
    )

    const { data } = await portalApi.rotateKey()
    newKey.value = data.plaintext_key
    rotateDialogVisible.value = true
    ElMessage.warning('请立即保存新秘钥，此秘钥只显示一次！')

    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('轮换秘钥失败')
    }
  }
}

const closeRotateDialog = () => {
  rotateDialogVisible.value = false
  newKey.value = ''
}

onMounted(() => {
  loadData()
})
</script>

<style scoped>
.key-management-container {
  padding: 20px;
}

.key-section {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.key-display {
  margin-top: 10px;
}

.key-cell {
  display: flex;
  align-items: center;
  gap: 12px;
}

.key-cell code {
  font-family: 'Courier New', monospace;
  background: #f5f5f5;
  padding: 4px 8px;
  border-radius: 4px;
  font-size: 14px;
}

.no-key {
  color: #909399;
  font-style: italic;
}

.new-key-container {
  margin: 20px 0;
}

.helper-text {
  margin-top: 10px;
}
</style>