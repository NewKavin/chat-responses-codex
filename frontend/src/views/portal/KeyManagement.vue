<template>
  <div class="crc-page key-management-page">
    <header class="crc-page-header">
      <div>
        <h1 class="crc-page-title">密钥管理</h1>
        <p class="crc-page-description">管理您的下游访问密钥，支持多密钥配置。</p>
      </div>
      <el-button type="primary" @click="showAddDialog = true">
        <Plus :size="16" :stroke-width="2" style="margin-right: 6px" />添加密钥
      </el-button>
    </header>

    <!-- Loading State -->
    <section v-if="loading" v-loading="true" class="key-list-surface crc-surface" style="min-height: 300px">
      <p style="text-align: center; color: var(--crc-text-muted); padding: 60px 0">加载中...</p>
    </section>

    <!-- Error State -->
    <section v-else-if="error" class="key-list-surface crc-surface">
      <el-alert type="error" :closable="false" show-icon>
        <template #title>加载失败</template>
        {{ error }}
      </el-alert>
      <el-button type="primary" @click="loadKeys" style="margin-top: 16px">
        <RotateCw :size="16" :stroke-width="2" style="margin-right: 6px" />重试
      </el-button>
    </section>

    <!-- Empty State -->
    <section v-else-if="sortedKeys.length === 0" class="key-list-surface crc-surface empty-state">
      <div class="empty-icon">
        <KeyRound :size="48" :stroke-width="1.5" />
      </div>
      <h3>暂无密钥</h3>
      <p>点击"添加密钥"按钮创建您的第一个访问密钥</p>
      <el-button type="primary" @click="showAddDialog = true">
        <Plus :size="16" :stroke-width="2" style="margin-right: 6px" />添加密钥
      </el-button>
    </section>

    <!-- Keys Grid -->
    <section v-else class="key-list-surface crc-surface">
      <div class="key-grid">
        <KeyCard
          v-for="key in sortedKeys"
          :key="key.downstream_id"
          :key-data="key"
          :model-groups="modelGroups"
          @edit="handleEdit"
          @rotate="handleRotate"
          @delete="handleDelete"
          @set-default="handleSetDefault"
          @change-model-group="handleChangeModelGroup"
        />
      </div>
      <footer class="key-count">
        显示 {{ sortedKeys.length }} 个密钥
      </footer>
    </section>

    <!-- Add Key Dialog -->
    <el-dialog
      v-model="showAddDialog"
      title="添加新密钥"
      width="min(500px, calc(100vw - 32px))"
    >
      <el-form :model="newKeyForm" label-width="100px">
        <el-form-item label="密钥 ID" required>
          <el-input
            v-model="newKeyForm.downstream_id"
            placeholder="sk-..."
          />
        </el-form-item>
        <el-form-item label="标签">
          <el-input
            v-model="newKeyForm.label"
            placeholder="为密钥添加备注标签"
          />
        </el-form-item>
        <el-form-item label="模型分组">
          <el-select
            v-model="newKeyForm.model_group_id"
            placeholder="选择模型分组（默认 basic）"
            clearable
            style="width: 100%"
          >
            <el-option
              v-for="group in modelGroups"
              :key="group.id"
              :label="group.name"
              :value="group.id"
            />
          </el-select>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="showAddDialog = false">取消</el-button>
        <el-button
          type="primary"
          :disabled="!newKeyForm.downstream_id"
          @click="handleCreate"
        >
          添加密钥
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { ElMessage } from 'element-plus'
import { Plus, RotateCw, KeyRound } from '@lucide/vue'
import { portalApi, type ModelGroup, type PortalKey } from '@/api/portal'
import KeyCard from '@/components/KeyCard.vue'

const loading = ref(true)
const error = ref<string | null>(null)
const keys = ref<PortalKey[]>([])
const showAddDialog = ref(false)
const modelGroups = ref<ModelGroup[]>([])
const newKeyForm = ref({
  downstream_id: '',
  label: '',
  model_group_id: ''
})

const sortedKeys = computed(() => {
  return [...keys.value].sort((a, b) => {
    if (a.is_default !== b.is_default) {
      return a.is_default ? -1 : 1
    }
    return b.created_at - a.created_at
  })
})

const loadKeys = async () => {
  try {
    loading.value = true
    error.value = null
    const { data } = await portalApi.listKeys()
    keys.value = data
  } catch (err: any) {
    error.value = err.message || '加载密钥失败'
  } finally {
    loading.value = false
  }
}

const handleCreate = async () => {
  try {
    await portalApi.createKey({
      downstream_id: newKeyForm.value.downstream_id,
      label: newKeyForm.value.label || undefined,
      model_group_id: newKeyForm.value.model_group_id || 'basic'
    })
    ElMessage.success('密钥添加成功')
    showAddDialog.value = false
    newKeyForm.value = { downstream_id: '', label: '', model_group_id: '' }
    await loadKeys()
  } catch (err: any) {
    ElMessage.error(err.message || '添加密钥失败')
  }
}

const handleEdit = async (downstreamId: string, newLabel: string) => {
  try {
    await portalApi.updateKeyLabel(downstreamId, newLabel.trim())
    ElMessage.success('标签已更新')
    await loadKeys()
  } catch (err: any) {
    ElMessage.error(err.message || '更新标签失败')
  }
}

const handleRotate = async (downstreamId: string, newId: string) => {
  try {
    await portalApi.rotateKeyById(downstreamId, newId)
    ElMessage.success('密钥轮换成功')
    await loadKeys()
  } catch (err: any) {
    ElMessage.error(err.message || '轮换密钥失败')
  }
}

const handleDelete = async (downstreamId: string) => {
  try {
    await portalApi.deleteKey(downstreamId)
    ElMessage.success('密钥已删除')
    await loadKeys()
  } catch (err: any) {
    ElMessage.error(err.message || '删除密钥失败')
  }
}

const handleSetDefault = async (downstreamId: string) => {
  try {
    await portalApi.setDefaultKey(downstreamId)
    ElMessage.success('默认密钥已设置')
    await loadKeys()
  } catch (err: any) {
    ElMessage.error(err.message || '设置默认密钥失败')
  }
}

const handleChangeModelGroup = async (downstreamId: string, modelGroupId: string) => {
  try {
    await portalApi.updateKeyModelGroup(downstreamId, modelGroupId)
    ElMessage.success('模型分组已更新')
    await loadKeys()
  } catch (err: any) {
    ElMessage.error(err.message || '更新模型分组失败')
    throw err
  }
}

const loadModelGroups = async () => {
  try {
    const { data } = await portalApi.listModelGroups()
    modelGroups.value = data.groups ?? []
  } catch (err: any) {
    console.warn('加载模型分组失败', err)
  }
}

onMounted(() => {
  loadKeys()
  loadModelGroups()
})
</script>

<style scoped>
.key-management-page {
  min-height: 100%;
}

.crc-page-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
}

.key-list-surface {
  max-width: 1200px;
  padding: 24px;
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  padding: 80px 24px;
  text-align: center;
}

.empty-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 96px;
  height: 96px;
  border: 2px solid var(--crc-border);
  border-radius: 50%;
  color: var(--crc-text-muted);
  background: var(--crc-canvas);
}

.empty-state h3 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 20px;
  font-weight: 600;
}

.empty-state p {
  margin: 0;
  color: var(--crc-text-muted);
  max-width: 400px;
}

.key-grid {
  display: grid;
  gap: 16px;
  grid-template-columns: repeat(auto-fill, minmax(min(100%, 360px), 1fr));
  margin-bottom: 24px;
}

.key-count {
  padding-top: 16px;
  border-top: 1px solid var(--crc-border);
  color: var(--crc-text-subtle);
  font-size: 14px;
  text-align: center;
}

@media (max-width: 767px) {
  .crc-page-header {
    flex-direction: column;
    align-items: stretch;
  }

  .key-list-surface {
    padding: 16px;
  }

  .key-grid {
    grid-template-columns: 1fr;
  }
}
</style>
