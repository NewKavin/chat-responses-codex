<template>
  <div class="key-card">
    <div class="key-card-header">
      <div class="key-card-info">
        <div class="key-id-row">
          <code class="key-id">{{ maskedKeyId }}</code>
          <el-tooltip content="复制密钥 ID" placement="top">
            <el-button
              aria-label="Copy key ID"
              circle
              size="small"
              @click="handleCopy"
            >
              <Copy :size="14" :stroke-width="1.8" />
            </el-button>
          </el-tooltip>
          <span v-if="keyData.is_default" class="default-badge">
            <Star :size="11" :stroke-width="2" />DEFAULT
          </span>
        </div>

        <div class="key-label-row">
          <template v-if="!editing">
            <span class="key-label">{{ keyData.label }}</span>
            <el-button
              aria-label="Edit label"
              text
              size="small"
              @click="startEditing"
            >
              <Pencil :size="13" :stroke-width="1.8" />
            </el-button>
          </template>
          <template v-else>
            <el-input
              v-model="editingLabel"
              type="text"
              size="small"
              style="max-width: 200px"
            />
            <el-button
              aria-label="Save label"
              size="small"
              type="primary"
              :disabled="loading"
              @click="handleEditSave"
            >
              <Check :size="13" :stroke-width="2" />
            </el-button>
            <el-button
              aria-label="Cancel edit"
              size="small"
              @click="cancelEditing"
            >
              <X :size="13" :stroke-width="2" />
            </el-button>
          </template>
        </div>

        <div class="key-meta">
          <span class="meta-item">
            <Layers :size="12" :stroke-width="1.8" />
            {{ keyData.model_group_name || keyData.model_group_id }}
            <el-button
              aria-label="Change model group"
              text
              size="small"
              class="group-change-btn"
              :disabled="loading"
              @click="showGroupDialog = true"
            >
              <Pencil :size="11" :stroke-width="1.8" />
            </el-button>
          </span>
          <span class="meta-item">
            <Clock :size="12" :stroke-width="1.8" />
            {{ formattedTime }}
          </span>
          <span class="meta-item">
            <Activity :size="12" :stroke-width="1.8" />
            {{ formattedUsage }} 次调用
          </span>
        </div>
      </div>

      <div class="key-card-actions">
        <el-button
          v-if="!keyData.is_default"
          aria-label="Set as default"
          size="small"
          :disabled="loading"
          @click="handleSetDefault"
        >
          <StarOff :size="13" :stroke-width="1.8" />
          设为默认
        </el-button>
        <el-button
          aria-label="Rotate key"
          size="small"
          type="warning"
          :disabled="loading"
          @click="showRotateDialog = true"
        >
          <RotateCcw :size="13" :stroke-width="1.8" />
          轮换
        </el-button>
        <el-button
          aria-label="Delete key"
          size="small"
          type="danger"
          :disabled="loading"
          @click="showDeleteDialog = true"
        >
          <Trash2 :size="13" :stroke-width="1.8" />
          删除
        </el-button>
      </div>
    </div>

    <div v-if="error" class="key-card-error">
      {{ error }}
    </div>

    <!-- Rotate Dialog -->
    <el-dialog
      v-model="showRotateDialog"
      title="轮换密钥"
      width="min(500px, calc(100vw - 32px))"
    >
      <p>请输入新的密钥 ID 来替换当前密钥。旧密钥将立即失效。</p>
      <el-input
        v-model="newKeyId"
        placeholder="输入新密钥 ID (例如: sk-xxxxx)"
        style="margin-top: 16px"
      />
      <template #footer>
        <el-button @click="showRotateDialog = false">取消</el-button>
        <el-button
          type="primary"
          :disabled="!newKeyId || loading"
          @click="handleRotate"
        >
          确认轮换
        </el-button>
      </template>
    </el-dialog>

    <!-- Change Model Group Dialog -->
    <el-dialog
      v-model="showGroupDialog"
      title="更改模型分组"
      width="min(440px, calc(100vw - 32px))"
    >
      <p class="group-dialog-hint">
        当前分组：{{ keyData.model_group_name || keyData.model_group_id }}。切换后该密钥只能请求新分组允许的模型。
      </p>
      <el-select
        v-model="selectedGroupId"
        placeholder="选择模型分组"
        style="width: 100%; margin-top: 12px"
        data-testid="group-select"
      >
        <el-option
          v-for="group in modelGroups"
          :key="group.id"
          :label="group.name"
          :value="group.id"
        />
      </el-select>
      <template #footer>
        <el-button @click="showGroupDialog = false">取消</el-button>
        <el-button
          type="primary"
          :disabled="!selectedGroupId || loading"
          @click="handleGroupChange"
        >
          确认更改
        </el-button>
      </template>
    </el-dialog>

    <!-- Delete Dialog -->
    <el-dialog
      v-model="showDeleteDialog"
      title="确认删除"
      width="min(400px, calc(100vw - 32px))"
    >
      <p>确定要删除此密钥吗？此操作无法撤销。</p>
      <template #footer>
        <el-button @click="showDeleteDialog = false">取消</el-button>
        <el-button
          type="danger"
          :disabled="loading"
          @click="handleDelete"
        >
          确认删除
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue'
import {
  Copy,
  Pencil,
  Check,
  X,
  Star,
  StarOff,
  RotateCcw,
  Trash2,
  Clock,
  Activity,
  Layers
} from '@lucide/vue'
import type { ModelGroup, PortalKey } from '@/api/portal'

interface Props {
  keyData: PortalKey
  modelGroups: ModelGroup[]
  onEdit: (downstreamId: string, newLabel: string) => Promise<void>
  onRotate: (downstreamId: string, newId: string) => Promise<void>
  onDelete: (downstreamId: string) => Promise<void>
  onSetDefault: (downstreamId: string) => Promise<void>
  onChangeModelGroup: (downstreamId: string, modelGroupId: string) => Promise<void>
}

const props = defineProps<Props>()

const editing = ref(false)
const editingLabel = ref(props.keyData.label)
const loading = ref(false)
const error = ref<string | null>(null)
const showRotateDialog = ref(false)
const showDeleteDialog = ref(false)
const showGroupDialog = ref(false)
const selectedGroupId = ref('')
const newKeyId = ref('')

const maskedKeyId = computed(() => {
  const id = props.keyData.downstream_id
  if (id.length <= 10) return id
  return `${id.slice(0, 3)}***${id.slice(-5)}`
})

const formattedTime = computed(() => {
  const seconds = Date.now() / 1000 - props.keyData.created_at
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(seconds / 3600)
  const days = Math.floor(seconds / 86400)

  if (seconds < 60) return 'just now'
  if (minutes < 60) return `${minutes} minute${minutes > 1 ? 's' : ''} ago`
  if (hours < 24) return `${hours} hour${hours > 1 ? 's' : ''} ago`
  return `${days} day${days > 1 ? 's' : ''} ago`
})

const formattedUsage = computed(() => {
  return props.keyData.usage_count.toLocaleString()
})

const startEditing = () => {
  editing.value = true
  editingLabel.value = props.keyData.label
}

const cancelEditing = () => {
  editing.value = false
  editingLabel.value = props.keyData.label
}

const handleEditSave = async () => {
  loading.value = true
  error.value = null
  try {
    await props.onEdit(props.keyData.downstream_id, editingLabel.value)
    editing.value = false
  } catch (err: any) {
    error.value = err.message || '操作失败'
  } finally {
    loading.value = false
  }
}

const handleCopy = async () => {
  try {
    await navigator.clipboard.writeText(props.keyData.downstream_id)
  } catch (err) {
    error.value = '复制失败'
  }
}

const handleSetDefault = async () => {
  loading.value = true
  error.value = null
  try {
    await props.onSetDefault(props.keyData.downstream_id)
  } catch (err: any) {
    error.value = err.message || '操作失败'
  } finally {
    loading.value = false
  }
}

const handleGroupChange = async () => {
  if (!selectedGroupId.value) return
  loading.value = true
  error.value = null
  try {
    await props.onChangeModelGroup(props.keyData.downstream_id, selectedGroupId.value)
    showGroupDialog.value = false
    selectedGroupId.value = ''
  } catch (err: any) {
    error.value = err.message || '操作失败'
  } finally {
    loading.value = false
  }
}

const handleRotate = async () => {
  loading.value = true
  error.value = null
  try {
    await props.onRotate(props.keyData.downstream_id, newKeyId.value)
    showRotateDialog.value = false
    newKeyId.value = ''
  } catch (err: any) {
    error.value = err.message || '操作失败'
  } finally {
    loading.value = false
  }
}

const handleDelete = async () => {
  loading.value = true
  error.value = null
  try {
    await props.onDelete(props.keyData.downstream_id)
    showDeleteDialog.value = false
  } catch (err: any) {
    error.value = err.message || '操作失败'
  } finally {
    loading.value = false
  }
}
</script>

<style scoped>
.group-change-btn {
  margin-left: 4px;
  vertical-align: middle;
}
.group-dialog-hint {
  font-size: 13px;
  color: var(--crc-text-muted, #909399);
  line-height: 1.6;
}
.key-card {
  padding: 20px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface);
}

.key-card-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.key-card-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.key-id-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.key-id {
  padding: 4px 10px;
  border: 1px dashed var(--crc-border-strong);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-strong);
  background: var(--crc-canvas);
  font-family: var(--crc-font-mono);
  font-size: 13px;
  letter-spacing: 0.02em;
}

.default-badge {
  display: inline-flex;
  padding: 3px 8px;
  align-items: center;
  gap: 4px;
  border: 1px solid var(--crc-border);
  border-radius: 999px;
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
  font-family: var(--crc-font-mono);
  font-size: 9px;
  font-weight: 600;
  letter-spacing: 0.1em;
}

.key-label-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.key-label {
  color: var(--crc-text-strong);
  font-size: 16px;
  font-weight: 600;
}

.key-meta {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.meta-item {
  display: flex;
  align-items: center;
  gap: 4px;
  color: var(--crc-text-muted);
  font-size: 12px;
}

.key-card-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.key-card-error {
  margin-top: 12px;
  padding: 8px 12px;
  border-radius: var(--crc-radius-sm);
  color: var(--el-color-danger);
  background: var(--el-color-danger-light-9);
  font-size: 13px;
}

@media (max-width: 767px) {
  .key-card-header {
    flex-direction: column;
  }

  .key-card-actions {
    width: 100%;
    justify-content: flex-start;
  }
}
</style>
