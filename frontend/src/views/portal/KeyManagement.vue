<template>
  <div class="crc-page key-management-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">密钥管理</h1>
        <p class="crc-page-description">查看当前下游密钥，或在需要时执行安全轮换。</p>
      </div>
    </header>

    <section v-loading="loading" class="key-security-surface crc-surface">
        <div class="key-security-head">
          <div>
            <p class="crc-eyebrow">VAULT // ACCESS KEY</p>
            <h2 class="key-security-title">下游访问密钥</h2>
          </div>
          <span class="key-security-badge">
            <ShieldCheck :size="13" :stroke-width="1.8" />AES GUARDED
          </span>
        </div>

        <el-alert
          type="info"
          :closable="false"
          class="helper-text"
        >
          这里显示的是您的完整秘钥，可用于配置客户端。如需更换秘钥，请点击"轮换秘钥"。
        </el-alert>

        <div class="key-display">
          <span class="key-display__label">CURRENT KEY // 当前访问密钥</span>
          <Fingerprint class="key-display__watermark" :size="84" :stroke-width="0.8" aria-hidden="true" />
          <code v-if="keyPlaintext">{{ keyPlaintext }}</code>
          <span v-else class="no-key">未设置密钥</span>
          <div class="key-actions">
            <el-tooltip content="复制密钥" placement="top">
              <el-button
                aria-label="复制密钥"
                circle
                :disabled="!keyPlaintext"
                @click="copyKey(keyPlaintext)"
              >
                <Copy :size="15" :stroke-width="1.8" />
              </el-button>
            </el-tooltip>
            <el-button type="warning" @click="handleRotate">
              <RotateCcw :size="14" :stroke-width="1.8" style="margin-right: 6px" />轮换密钥
            </el-button>
          </div>
        </div>

        <el-alert
          type="warning"
          :closable="false"
          class="helper-text"
        >
          轮换秘钥后，旧秘钥将立即失效。新秘钥只会显示一次，请务必妥善保存。
        </el-alert>
    </section>

    <el-dialog
      v-model="rotateDialogVisible"
      class="rotate-key-dialog"
      title="密钥轮换成功"
      width="min(500px, calc(100vw - 32px))"
    >
      <el-alert type="success" :closable="false" show-icon>
        <template #title>
          新秘钥已生成，请立即保存！此秘钥只显示一次。
        </template>
      </el-alert>
      <div class="new-key-container">
        <code>{{ newKey }}</code>
        <el-tooltip content="复制密钥" placement="top">
          <el-button
            aria-label="复制密钥"
            circle
            type="primary"
            @click="copyFullKey(newKey)"
          >
            <Copy :size="15" :stroke-width="1.8" />
          </el-button>
        </el-tooltip>
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
import { Copy, Fingerprint, RotateCcw, ShieldCheck } from '@lucide/vue'
import { portalApi } from '@/api/portal'
import { getCopyableKey } from '@/utils/keyUtils'

const loading = ref(false)
const keyPlaintext = ref<string | null>(null)
const rotateDialogVisible = ref(false)
const newKey = ref('')

const loadData = async () => {
  try {
    loading.value = true
    const { data } = await portalApi.getKey()
    keyPlaintext.value = data.plaintext_key
  } catch (error) {
    ElMessage.error('加载秘钥信息失败')
  } finally {
    loading.value = false
  }
}

const copyKey = async (key: unknown) => {
  const copyableKey = getCopyableKey(key)
  if (!copyableKey) {
    ElMessage.warning('当前没有可复制的真实秘钥，请先轮换秘钥')
    return
  }

  try {
    await navigator.clipboard.writeText(copyableKey)
    ElMessage.success('已复制到剪贴板')
  } catch {
    const textArea = document.createElement('textarea')
    textArea.value = copyableKey
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
    keyPlaintext.value = data.plaintext_key
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
.key-management-page {
  min-height: 100%;
}

.key-security-surface {
  display: flex;
  flex-direction: column;
  gap: 18px;
  max-width: 880px;
  padding: 24px;
}

.key-security-head {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 16px;
}

.key-security-title {
  margin: 6px 0 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 20px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.key-security-badge {
  display: inline-flex;
  padding: 6px 10px;
  align-items: center;
  gap: 6px;
  border: 1px solid var(--crc-border);
  border-radius: 999px;
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.1em;
}

.key-display {
  position: relative;
  display: grid;
  align-items: center;
  gap: 14px;
  grid-template-columns: minmax(0, 1fr) auto;
  padding: 20px;
  overflow: hidden;
  border: 1px solid var(--crc-border-strong);
  border-radius: var(--crc-radius);
  background:
    radial-gradient(ellipse 90% 130% at 100% 0%, var(--crc-accent-soft) 0%, transparent 55%),
    var(--crc-canvas);
}

.key-display__watermark {
  position: absolute;
  right: 16px;
  top: 50%;
  color: var(--crc-accent);
  opacity: 0.1;
  pointer-events: none;
  transform: translateY(-50%);
}

.key-display__label {
  grid-column: 1 / -1;
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  font-weight: 500;
  letter-spacing: 0.12em;
}

.key-display code,
.new-key-container code {
  position: relative;
  min-width: 0;
  padding: 12px 14px;
  border: 1px dashed var(--crc-border-strong);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-strong);
  background: var(--crc-surface);
  font-family: var(--crc-font-mono);
  font-size: 15px;
  letter-spacing: 0.02em;
  overflow-wrap: anywhere;
  user-select: all;
}

.key-actions {
  position: relative;
  display: flex;
  align-items: center;
  gap: 8px;
}

.no-key {
  color: var(--crc-text-muted);
  font-style: italic;
}

.new-key-container {
  display: grid;
  align-items: center;
  gap: 10px;
  grid-template-columns: minmax(0, 1fr) auto;
  margin: 20px 0;
  padding: 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-canvas);
}

.helper-text {
  margin: 0;
}

@media (max-width: 767px) {
  .key-security-surface {
    padding: 16px;
  }

  .key-display,
  .new-key-container {
    grid-template-columns: 1fr;
  }

  .key-actions {
    justify-content: flex-start;
  }
}
</style>
