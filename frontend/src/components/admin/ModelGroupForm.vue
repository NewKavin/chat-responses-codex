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
        placeholder="搜索选择或输入模型名后回车添加；输入 * 表示全部"
        style="width: 100%"
        data-testid="group-models-select"
      >
        <el-option
          v-for="model in availableModelOptions"
          :key="model"
          :label="model"
          :value="model"
        />
      </el-select>
      <div class="form-help-text">
        可从网关已知模型候选中选择，也可手动输入（回车添加）；空列表不允许。`*` 代表允许所有模型（all 分组）。
      </div>
    </el-form-item>
  </el-form>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { adminApi } from '@/api/admin'
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

/** 网关已知模型候选（活跃上游模型 ∪ 各上游 supported_models） */
const modelCandidates = ref<string[]>([])

/** 候选列表 ∪ 当前已选，确保已选但不在候选中的模型也能显示 */
const availableModelOptions = computed(() => {
  const selected = form.allowed_models ?? []
  return Array.from(new Set([...modelCandidates.value, ...selected])).sort()
})

const loadModelCandidates = async () => {
  try {
    const [modelsResp, upstreamsResp, aliasesResp] = await Promise.all([
      adminApi.getModels(),
      adminApi.getUpstreams(),
      adminApi.getModelAliases()
    ])

    // 模型映射目标优先：全局别名的 aliases（映射目标） + 上游
    // model_mappings 的 downstream_model（映射目标）
    const mappedTargets = new Set<string>()
    // 已被映射的原始模型名：全局别名 canonical + 上游 mapping 的 upstream_model
    const mappedOriginals = new Set<string>()
    for (const rule of aliasesResp.data.model_aliases ?? []) {
      for (const alias of rule.aliases ?? []) {
        if (alias && alias.trim()) mappedTargets.add(alias.trim())
      }
      if (rule.canonical && rule.canonical.trim()) {
        mappedOriginals.add(rule.canonical.trim())
      }
    }
    for (const upstream of upstreamsResp.data ?? []) {
      for (const mapping of upstream.model_mappings ?? []) {
        if (mapping?.downstream_model && mapping.downstream_model.trim()) {
          mappedTargets.add(mapping.downstream_model.trim())
        }
        if (mapping?.upstream_model && mapping.upstream_model.trim()) {
          mappedOriginals.add(mapping.upstream_model.trim())
        }
      }
    }

    // 候选 = 映射目标 ∪ 未配置任何映射的原始模型名
    const set = new Set<string>(mappedTargets)
    const rawModels: string[] = [
      ...(modelsResp.data.models ?? []),
      ...(upstreamsResp.data ?? []).flatMap(u => u.supported_models ?? [])
    ]
    for (const rawModel of rawModels) {
      const raw = typeof rawModel === 'string' ? rawModel.trim() : ''
      if (!raw) continue
      // 已有映射的原始模型不再直接列出（用映射目标代替）
      if (mappedOriginals.has(raw)) continue
      // 已是映射目标的名称也跳过（避免与映射目标重复）
      if (mappedTargets.has(raw)) continue
      set.add(raw)
    }
    modelCandidates.value = Array.from(set).sort()
  } catch {
    // 候选加载失败不阻塞手动输入
    modelCandidates.value = []
  }
}

onMounted(loadModelCandidates)

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
