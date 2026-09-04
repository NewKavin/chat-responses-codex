<template>
  <el-form :model="form" label-width="120px" class="model-group-form">
    <el-form-item label="分组 ID" required>
      <el-input
        v-model="form.id"
        placeholder="小写字母、数字、连字符，例如 premium-tier"
        :disabled="mode === 'edit'"
        data-testid="group-id-input"
      />
      <div class="form-help-text">
        {{ mode === 'edit' ? 'ID 创建后不可修改' : '仅允许小写字母、数字和连字符' }}
      </div>
    </el-form-item>

    <el-form-item label="名称" required>
      <el-input v-model="form.name" placeholder="例如：Premium 模型" data-testid="group-name-input" />
    </el-form-item>

    <el-form-item label="描述">
      <el-input
        v-model="form.description"
        type="textarea"
        :rows="2"
        placeholder="可选：这个分组的用途说明"
      />
    </el-form-item>

    <el-form-item label="允许的模型" required>
      <el-select
        v-model="form.allowed_models"
        multiple
        filterable
        allow-create
        default-first-option
        placeholder="输入模型名后回车添加；输入 * 表示全部"
        style="width: 100%"
        data-testid="group-models-select"
      >
        <el-option
          v-for="model in form.allowed_models"
          :key="model"
          :label="model"
          :value="model"
        />
      </el-select>
      <div class="form-help-text">
        逗号分隔也可；空列表不允许。`*` 代表允许所有模型（all 分组）。
      </div>
    </el-form-item>
  </el-form>
</template>

<script setup lang="ts">
import { reactive, watch } from 'vue'
import type { ModelGroup } from '@/api/portal'

const props = defineProps<{
  mode: 'create' | 'edit'
  group?: ModelGroup | null
}>()

const form = reactive({
  id: '',
  name: '',
  description: '',
  allowed_models: [] as string[]
})

watch(
  () => props.group,
  group => {
    if (group && props.mode === 'edit') {
      form.id = group.id
      form.name = group.name
      form.description = group.description ?? ''
      form.allowed_models = [...group.allowed_models]
    } else {
      form.id = ''
      form.name = ''
      form.description = ''
      form.allowed_models = []
    }
  },
  { immediate: true }
)

function getPayload(): {
  id: string
  name: string
  description: string | null
  allowed_models: string[]
} | null {
  const id = form.id.trim()
  const name = form.name.trim()
  if (!id || !name) return null
  if (form.allowed_models.length === 0) return null
  return {
    id,
    name,
    description: form.description.trim() ? form.description.trim() : null,
    allowed_models: [...form.allowed_models]
  }
}

defineExpose({ getPayload })
</script>

<style scoped>
.model-group-form {
  padding-top: 8px;
}
.form-help-text {
  font-size: 12px;
  line-height: 1.5;
  color: var(--crc-text-muted, #909399);
  margin-top: 4px;
}
</style>
