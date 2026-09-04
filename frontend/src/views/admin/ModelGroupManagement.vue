<template>
  <div class="crc-page model-groups-page">
    <header class="crc-page-header">
      <div>
        <p class="crc-eyebrow">RESOURCES // MODEL GROUPS</p>
        <h1 class="crc-page-title">模型分组</h1>
        <p class="crc-page-description">
          定义模型分组并把下游密钥绑定到分组：网关按分组白名单校验每次请求的 model 参数。
        </p>
      </div>
      <div class="header-actions">
        <el-button :icon="RefreshCw" :loading="loading" @click="loadGroups">重新加载</el-button>
        <el-button type="primary" :icon="Plus" @click="openCreate">新建分组</el-button>
      </div>
    </header>

    <div v-loading="loading" class="groups-surface crc-surface">
      <el-table :data="groups" stripe empty-text="暂无模型分组">
        <el-table-column prop="id" label="ID" width="150">
          <template #default="{ row }">
            <code class="group-id">{{ row.id }}</code>
          </template>
        </el-table-column>
        <el-table-column prop="name" label="名称" min-width="160" />
        <el-table-column prop="description" label="描述" min-width="200">
          <template #default="{ row }">
            <span class="group-desc">{{ row.description || '—' }}</span>
          </template>
        </el-table-column>
        <el-table-column label="允许的模型" min-width="300">
          <template #default="{ row }">
            <div class="model-tags">
              <span class="all-models" v-if="row.allowed_models.includes('*')">*（全部）</span>
              <el-tag
                v-for="m in row.allowed_models.filter((x: string) => x !== '*')"
                :key="m"
                size="small"
                type="info"
                class="model-tag"
              >
                {{ m }}
              </el-tag>
              <span v-if="row.allowed_models.length === 0" class="model-tags-empty">（空）</span>
            </div>
          </template>
        </el-table-column>
        <el-table-column label="操作" width="170" align="center">
          <template #default="{ row }">
            <el-button :icon="Pencil" size="small" @click="openEdit(row)">编辑</el-button>
            <el-button
              :icon="Trash2"
              size="small"
              type="danger"
              :disabled="row.id === 'basic'"
              @click="handleDelete(row)"
            >
              删除
            </el-button>
          </template>
        </el-table-column>
      </el-table>
    </div>

    <!-- Create/Edit dialog -->
    <el-dialog
      v-model="dialogVisible"
      :title="dialogMode === 'create' ? '新建模型分组' : '编辑模型分组'"
      width="min(640px, calc(100vw - 32px))"
      :close-on-click-modal="false"
      @closed="closeDialog"
    >
      <ModelGroupForm ref="formRef" :mode="dialogMode" :group="editingGroup" />
      <template #footer>
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="saving" @click="handleSubmit">保存</el-button>
      </template>
    </el-dialog>

    <!-- Delete confirm -->
    <el-dialog v-model="deleteDialogVisible" title="确认删除分组" width="min(480px, calc(100vw - 32px))">
      <p>
        确定删除分组 <strong>{{ deletingGroup?.name }}</strong>（<code>{{ deletingGroup?.id }}</code>）吗？
        使用该分组的密钥将回退到 <code>basic</code> 分组。
      </p>
      <template #footer>
        <el-button @click="deleteDialogVisible = false">取消</el-button>
        <el-button type="danger" :loading="saving" @click="confirmDelete">确认删除</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { ElMessage } from 'element-plus'
import { Plus, RefreshCw, Pencil, Trash2 } from '@lucide/vue'
import { adminApi } from '@/api/admin'
import type { ModelGroup } from '@/api/portal'
import ModelGroupForm from '@/components/admin/ModelGroupForm.vue'

const groups = ref<ModelGroup[]>([])
const loading = ref(false)
const saving = ref(false)

const dialogVisible = ref(false)
const dialogMode = ref<'create' | 'edit'>('create')
const editingGroup = ref<ModelGroup | null>(null)
const formRef = ref<InstanceType<typeof ModelGroupForm> | null>(null)

const deleteDialogVisible = ref(false)
const deletingGroup = ref<ModelGroup | null>(null)

const loadGroups = async () => {
  loading.value = true
  try {
    const { data } = await adminApi.listModelGroups()
    groups.value = data.groups ?? []
  } catch (err: any) {
    ElMessage.error(err?.message || '加载模型分组失败')
  } finally {
    loading.value = false
  }
}

const openCreate = () => {
  dialogMode.value = 'create'
  editingGroup.value = null
  dialogVisible.value = true
}

const openEdit = (group: ModelGroup) => {
  dialogMode.value = 'edit'
  editingGroup.value = group
  dialogVisible.value = true
}

const closeDialog = () => {
  editingGroup.value = null
}

const handleSubmit = async () => {
  const formEl = formRef.value
  if (!formEl) return
  const payload = formEl.getPayload()
  if (!payload) {
    ElMessage.warning('请填写分组 ID、名称和至少一个允许的模型')
    return
  }

  saving.value = true
  try {
    if (dialogMode.value === 'create') {
      await adminApi.createModelGroup(payload)
      ElMessage.success('模型分组已创建')
    } else {
      await adminApi.updateModelGroup(payload.id, {
        name: payload.name,
        description: payload.description,
        allowed_models: payload.allowed_models
      })
      ElMessage.success('模型分组已更新')
    }
    dialogVisible.value = false
    await loadGroups()
  } catch (err: any) {
    ElMessage.error(err?.message || '保存失败')
  } finally {
    saving.value = false
  }
}

const handleDelete = (group: ModelGroup) => {
  deletingGroup.value = group
  deleteDialogVisible.value = true
}

const confirmDelete = async () => {
  if (!deletingGroup.value) return
  saving.value = true
  try {
    await adminApi.deleteModelGroup(deletingGroup.value.id)
    ElMessage.success('模型分组已删除')
    deleteDialogVisible.value = false
    await loadGroups()
  } catch (err: any) {
    ElMessage.error(err?.message || '删除失败')
  } finally {
    saving.value = false
  }
}

onMounted(() => {
  loadGroups()
})
</script>

<style scoped>
.model-groups-page {
  min-height: 100%;
}
.crc-page-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
}
.header-actions {
  display: flex;
  gap: 8px;
}
.groups-surface {
  max-width: 1200px;
  padding: 24px;
}
.group-id {
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--crc-canvas, #f5f7fa);
  font-family: var(--crc-font-mono, monospace);
  font-size: 13px;
}
.group-desc {
  color: var(--crc-text-muted, #606266);
  font-size: 13px;
}
.model-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: center;
}
.all-models {
  font-weight: 600;
  color: var(--crc-accent, #409eff);
}
.model-tags-empty {
  color: var(--crc-text-subtle, #c0c4cc);
}
</style>
