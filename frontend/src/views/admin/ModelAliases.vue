<template>
  <div class="crc-page model-aliases-page">
    <header class="crc-page-header">
      <div>
        <p class="crc-eyebrow">RESOURCES // MODEL MAPPING</p>
        <h1 class="crc-page-title">模型映射</h1>
        <p class="crc-page-description">
          统一管理模型别名，将不同拼写的模型名映射到规范名称。配置对所有上游全局生效。
        </p>
      </div>
      <div class="header-actions">
        <el-button :icon="RefreshCw" :loading="loading" @click="loadAliases">重新加载</el-button>
        <el-button type="primary" :icon="Plus" @click="handleManualAdd">手动添加</el-button>
      </div>
    </header>

    <el-alert
      title="模型别名规则对所有上游生效，支持大小写不敏感匹配。例如：将 'deepseek-chat' 和 'DeepSeek-Chat' 统一映射到 'deepseek-v3'。"
      type="info"
      :closable="false"
      show-icon
      style="margin-bottom: 20px;"
    />

    <div class="content-layout">
      <!-- 左侧：上游选择器和模型列表 -->
      <section class="upstream-panel">
        <div class="panel-header">
          <h3>上游模型浏览器</h3>
          <p class="help-text">选择上游账号查看其支持的模型，快速添加映射规则</p>
        </div>

        <el-form label-position="top">
          <el-form-item label="选择上游账号">
            <el-select
              v-model="selectedUpstreamId"
              placeholder="请选择上游账号"
              filterable
              style="width: 100%"
              @change="handleUpstreamChange"
            >
              <el-option
                v-for="upstream in upstreams"
                :key="upstream.id"
                :label="`${upstream.name} (${upstream.supported_models.length} 个模型)`"
                :value="upstream.id"
              />
            </el-select>
          </el-form-item>
        </el-form>

        <div v-if="selectedUpstreamId" v-loading="loadingModels" class="models-list">
          <div v-if="currentUpstreamModels.length === 0" class="empty-state">
            <el-empty description="该上游暂无模型" />
          </div>
          <div v-else>
            <div class="models-list-header">
              <span>模型列表 ({{ currentUpstreamModels.length }})</span>
            </div>
            <div
              v-for="model in currentUpstreamModels"
              :key="model"
              class="model-item"
            >
              <span class="model-name">{{ model }}</span>
              <el-button
                size="small"
                :icon="Plus"
                @click="handleQuickAdd(model)"
              >
                快速添加
              </el-button>
            </div>
          </div>
        </div>
      </section>

      <!-- 右侧：映射规则表格 -->
      <section class="rules-panel">
        <div class="panel-header">
          <h3>全局映射规则</h3>
          <p class="help-text">这些规则对所有上游账号生效</p>
        </div>

        <div v-loading="loading" class="rules-table">
          <el-table :data="rules" stripe border>
            <el-table-column label="规范名称" prop="canonical" min-width="180">
              <template #default="{ row }">
                <strong class="canonical-name">{{ row.canonical }}</strong>
              </template>
            </el-table-column>
            <el-table-column label="别名列表" min-width="300">
              <template #default="{ row }">
                <div class="aliases-list">
                  <el-tag
                    v-for="(alias, idx) in row.aliases"
                    :key="idx"
                    size="small"
                    class="alias-tag"
                  >
                    {{ alias }}
                  </el-tag>
                  <span v-if="row.aliases.length === 0" class="text-muted">无别名</span>
                </div>
              </template>
            </el-table-column>
            <el-table-column label="操作" width="180" align="center">
              <template #default="{ $index }">
                <el-button
                  :icon="Edit"
                  size="small"
                  @click="handleEdit($index)"
                >
                  编辑
                </el-button>
                <el-button
                  :icon="Trash2"
                  size="small"
                  type="danger"
                  @click="handleDelete($index)"
                >
                  删除
                </el-button>
              </template>
            </el-table-column>
          </el-table>

          <el-empty
            v-if="rules.length === 0 && !loading"
            description="暂无模型映射规则"
            style="margin-top: 40px;"
          />
        </div>

        <div v-if="rules.length > 0" class="actions-footer">
          <el-button type="primary" :loading="saving" @click="handleSave">保存全部</el-button>
          <el-button :disabled="saving" @click="loadAliases">重置</el-button>
        </div>
      </section>
    </div>

    <!-- Edit Dialog -->
    <el-dialog
      v-model="dialogVisible"
      :title="dialogTitle"
      width="600px"
      @close="handleDialogClose"
    >
      <el-form :model="editForm" label-position="top" :rules="formRules" ref="formRef">
        <el-form-item label="规范名称 (Canonical)" prop="canonical">
          <el-input
            v-model="editForm.canonical"
            placeholder="例如: deepseek-v3"
            maxlength="100"
          />
          <div class="form-help-text">
            下游用户看到的统一名称，用于所有内部标识（使用统计、配额等）
          </div>
        </el-form-item>

        <el-form-item label="别名列表 (Aliases)" prop="aliases">
          <div class="aliases-editor">
            <el-tag
              v-for="(alias, index) in editForm.aliases"
              :key="index"
              closable
              @close="removeAlias(index)"
              class="alias-tag-editable"
            >
              {{ alias }}
            </el-tag>
            <el-input
              v-if="inputVisible"
              ref="inputRef"
              v-model="aliasInput"
              class="alias-input"
              size="small"
              @keyup.enter="handleAliasInputConfirm"
              @blur="handleAliasInputConfirm"
            />
            <el-button
              v-else
              size="small"
              @click="showAliasInput"
            >
              + 添加别名
            </el-button>
          </div>
          <div class="form-help-text">
            其它会映射到规范名称的拼写，支持大小写不敏感匹配。留空表示该模型没有别名。
          </div>
        </el-form-item>
      </el-form>

      <template #footer>
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="primary" @click="handleDialogConfirm">确定</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, reactive, nextTick, computed } from 'vue'
import { ElMessage, ElMessageBox, type FormInstance, type FormRules } from 'element-plus'
import { RefreshCw, Plus, Edit, Trash2 } from '@lucide/vue'
import { adminApi } from '@/api/admin'
import type { ModelAliasRule, UpstreamConfig } from '@/types'

type ApiError = {
  response?: {
    data?: {
      error?: {
        message?: string
      }
      message?: string
    }
  }
  message?: string
}

const loading = ref(false)
const saving = ref(false)
const loadingUpstreams = ref(false)
const loadingModels = ref(false)
const rules = ref<ModelAliasRule[]>([])
const upstreams = ref<UpstreamConfig[]>([])
const selectedUpstreamId = ref<string>('')
const dialogVisible = ref(false)
const dialogMode = ref<'add' | 'edit' | 'quick'>('add')
const editIndex = ref(-1)
const formRef = ref<FormInstance>()
const inputRef = ref()
const inputVisible = ref(false)
const aliasInput = ref('')

const editForm = reactive<ModelAliasRule>({
  canonical: '',
  aliases: []
})

const formRules: FormRules = {
  canonical: [
    { required: true, message: '请输入规范名称', trigger: 'blur' },
    { min: 1, max: 100, message: '长度在 1 到 100 个字符', trigger: 'blur' }
  ]
}

const dialogTitle = computed(() => {
  if (dialogMode.value === 'quick') return '快速添加映射规则'
  if (dialogMode.value === 'edit') return '编辑映射规则'
  return '手动添加映射规则'
})

const currentUpstreamModels = computed(() => {
  const upstream = upstreams.value.find(u => u.id === selectedUpstreamId.value)
  return upstream?.supported_models || []
})

const loadUpstreams = async () => {
  loadingUpstreams.value = true
  try {
    const res = await adminApi.getUpstreams()
    upstreams.value = res.data
  } catch (err) {
    const error = err as ApiError
    const message = error.response?.data?.error?.message || error.message || '加载失败'
    ElMessage.error('加载上游列表失败: ' + message)
  } finally {
    loadingUpstreams.value = false
  }
}

const loadAliases = async () => {
  loading.value = true
  try {
    const res = await adminApi.getModelAliases()
    rules.value = res.data.model_aliases || []
  } catch (err) {
    const error = err as ApiError
    const message = error.response?.data?.error?.message || error.message || '加载失败'
    ElMessage.error('加载模型映射失败: ' + message)
  } finally {
    loading.value = false
  }
}

const handleSave = async () => {
  saving.value = true
  try {
    await adminApi.updateModelAliases({ model_aliases: rules.value })
    ElMessage.success('保存成功')
    await loadAliases()
  } catch (err) {
    const error = err as ApiError
    const message = error.response?.data?.error?.message || error.message || '保存失败'
    ElMessage.error('保存失败: ' + message)
  } finally {
    saving.value = false
  }
}

const handleUpstreamChange = () => {
  // 切换上游账号，模型列表会自动更新（通过 computed）
}

const handleQuickAdd = (modelName: string) => {
  dialogMode.value = 'quick'
  editIndex.value = -1
  editForm.canonical = modelName
  editForm.aliases = []
  dialogVisible.value = true
}

const handleManualAdd = () => {
  dialogMode.value = 'add'
  editIndex.value = -1
  editForm.canonical = ''
  editForm.aliases = []
  dialogVisible.value = true
}

const handleEdit = (index: number) => {
  dialogMode.value = 'edit'
  editIndex.value = index
  const rule = rules.value[index]
  editForm.canonical = rule.canonical
  editForm.aliases = [...rule.aliases]
  dialogVisible.value = true
}

const handleDelete = async (index: number) => {
  try {
    await ElMessageBox.confirm(
      `确定要删除规则 "${rules.value[index].canonical}" 吗？`,
      '确认删除',
      {
        confirmButtonText: '删除',
        cancelButtonText: '取消',
        type: 'warning'
      }
    )
    rules.value.splice(index, 1)
    ElMessage.success('已删除，请点击"保存全部"来应用更改')
  } catch {
    // User cancelled
  }
}

const handleDialogClose = () => {
  formRef.value?.resetFields()
  inputVisible.value = false
  aliasInput.value = ''
}

const handleDialogConfirm = async () => {
  if (!formRef.value) return

  await formRef.value.validate((valid) => {
    if (valid) {
      const newRule: ModelAliasRule = {
        canonical: editForm.canonical.trim(),
        aliases: editForm.aliases
      }

      if (dialogMode.value === 'edit') {
        rules.value[editIndex.value] = newRule
        ElMessage.success('已修改，请点击"保存全部"来应用更改')
      } else {
        rules.value.push(newRule)
        ElMessage.success('已添加，请点击"保存全部"来应用更改')
      }

      dialogVisible.value = false
    }
  })
}

const showAliasInput = () => {
  inputVisible.value = true
  nextTick(() => {
    inputRef.value?.focus()
  })
}

const handleAliasInputConfirm = () => {
  const value = aliasInput.value.trim()
  if (value && !editForm.aliases.includes(value)) {
    editForm.aliases.push(value)
  }
  inputVisible.value = false
  aliasInput.value = ''
}

const removeAlias = (index: number) => {
  editForm.aliases.splice(index, 1)
}

onMounted(() => {
  loadUpstreams()
  loadAliases()
})
</script>

<style scoped>
.model-aliases-page {
  max-width: 1600px;
}

.header-actions {
  display: flex;
  gap: 12px;
}

.content-layout {
  display: grid;
  grid-template-columns: 400px 1fr;
  gap: 24px;
  margin-top: 20px;
}

@media (max-width: 1200px) {
  .content-layout {
    grid-template-columns: 1fr;
  }
}

/* 上游面板 */
.upstream-panel {
  background: var(--el-bg-color);
  border-radius: 8px;
  padding: 20px;
  border: 1px solid var(--el-border-color-light);
}

.panel-header h3 {
  margin: 0 0 8px 0;
  font-size: 16px;
  font-weight: 600;
  color: var(--el-text-color-primary);
}

.help-text {
  margin: 0 0 16px 0;
  font-size: 13px;
  color: var(--el-text-color-secondary);
}

.models-list {
  margin-top: 16px;
  min-height: 200px;
}

.models-list-header {
  padding: 8px 12px;
  background: var(--el-fill-color-light);
  border-radius: 4px;
  font-size: 13px;
  font-weight: 600;
  color: var(--el-text-color-regular);
  margin-bottom: 12px;
}

.model-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  border: 1px solid var(--el-border-color-lighter);
  border-radius: 4px;
  margin-bottom: 8px;
  transition: all 0.2s;
}

.model-item:hover {
  border-color: var(--el-border-color);
  background: var(--el-fill-color-lighter);
}

.model-name {
  font-family: var(--el-font-family-mono, monospace);
  font-size: 13px;
  color: var(--el-text-color-primary);
  flex: 1;
}

.empty-state {
  padding: 40px 0;
}

/* 规则面板 */
.rules-panel {
  background: var(--el-bg-color);
  border-radius: 8px;
  padding: 20px;
  border: 1px solid var(--el-border-color-light);
}

.rules-table {
  min-height: 300px;
}

.canonical-name {
  font-family: var(--el-font-family-mono, monospace);
  color: var(--el-color-primary);
  font-weight: 600;
}

.aliases-list {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.alias-tag {
  font-family: var(--el-font-family-mono, monospace);
}

.text-muted {
  color: var(--el-text-color-secondary);
  font-size: 14px;
}

.actions-footer {
  margin-top: 24px;
  padding-top: 24px;
  border-top: 1px solid var(--el-border-color-light);
  display: flex;
  gap: 12px;
}

/* 对话框 */
.aliases-editor {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}

.alias-tag-editable {
  font-family: var(--el-font-family-mono, monospace);
}

.alias-input {
  width: 140px;
}

.form-help-text {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-top: 4px;
  line-height: 1.5;
}
</style>
