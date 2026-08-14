<template>
  <div class="crc-page model-aliases-page">
    <header class="crc-page-header">
      <div>
        <p class="crc-eyebrow">RESOURCES // MODEL MAPPING</p>
        <h1 class="crc-page-title">模型映射</h1>
        <p class="crc-page-description">
          按上游账号把模型改名后暴露给下游；跨账号的拼写归并与显示控制请用「全局规则」tab。
        </p>
      </div>
      <div class="header-actions">
        <el-button :icon="RefreshCw" :loading="loadingAll" @click="reloadAll">重新加载</el-button>
      </div>
    </header>

    <el-tabs v-model="activeTab" class="model-mappings-tabs">
      <!-- ============ Tab 1: 按上游模型映射（默认） ============ -->
      <el-tab-pane label="模型映射" name="mappings">
        <el-alert
          title="为单个上游账号改名：例如上游 A 的 gpt-4 映射为 gpt-4-premium，上游 B 的同名 gpt-4 映射为 gpt-4-standard，互不影响。映射后下游只能看到新名称，原拼写被遮蔽。"
          type="info"
          :closable="false"
          show-icon
          style="margin-bottom: 16px;"
        />

        <div v-loading="loadingUpstreams" class="mappings-panel">
          <div class="mappings-toolbar">
            <el-select
              v-model="filterUpstreamId"
              placeholder="全部上游账号"
              clearable
              filterable
              style="width: 260px"
            >
              <el-option
                v-for="upstream in upstreams"
                :key="upstream.id"
                :label="upstream.name"
                :value="upstream.id"
              />
            </el-select>
            <el-input
              v-model="mappingSearch"
              placeholder="搜索上游账号 / 上游模型 / 下游模型"
              clearable
              style="width: 320px"
            >
              <template #prefix>
                <Search :size="14" />
              </template>
            </el-input>
            <el-button type="primary" :icon="Plus" @click="openAddMapping">添加映射</el-button>
          </div>

          <el-table :data="pagedMappingRows" stripe border>
            <el-table-column label="上游账号" min-width="180">
              <template #default="{ row }">
                <span class="upstream-name">{{ row.upstreamName }}</span>
              </template>
            </el-table-column>
            <el-table-column label="上游模型名称" min-width="200">
              <template #default="{ row }">
                <span class="mono-cell">{{ row.upstreamModel }}</span>
              </template>
            </el-table-column>
            <el-table-column label="下游模型名称" min-width="200">
              <template #default="{ row }">
                <strong class="downstream-cell">{{ row.downstreamModel }}</strong>
              </template>
            </el-table-column>
            <el-table-column label="状态" width="130" align="center">
              <template #default="{ row }">
                <el-tooltip
                  v-if="row.stale"
                  content="上游模型已不在该账号的模型清单中，路由会跳过此映射；恢复清单后自动生效"
                  placement="top"
                >
                  <el-tag type="danger" size="small">失效</el-tag>
                </el-tooltip>
                <el-tag v-else type="success" size="small">生效</el-tag>
              </template>
            </el-table-column>
            <el-table-column label="操作" width="200" align="center">
              <template #default="{ row }">
                <div class="mapping-row-actions">
                  <el-button :icon="Edit" size="small" @click="openEditMapping(row)">编辑</el-button>
                  <el-button :icon="Trash2" size="small" type="danger" @click="handleDeleteMapping(row)">删除</el-button>
                </div>
              </template>
            </el-table-column>
          </el-table>

          <div v-if="filteredMappingRows.length > 0" class="mapping-table-pagination">
            <el-pagination
              v-model:current-page="mappingPage"
              v-model:page-size="mappingPageSize"
              :total="filteredMappingRows.length"
              :page-sizes="[10, 20, 50, 100]"
              layout="total, sizes, prev, pager, next"
              background
            />
          </div>

          <el-empty
            v-if="mappingRows.length === 0 && !loadingUpstreams"
            description="暂无模型映射；点击「添加映射」为上游账号改名"
          />
          <el-empty
            v-else-if="mappingRows.length > 0 && filteredMappingRows.length === 0"
            description="没有匹配当前筛选条件的映射"
          />
        </div>

        <!-- 添加/编辑映射对话框（三步：选上游 → 选模型 → 输入下游名称） -->
        <el-dialog
          v-model="mappingDialogVisible"
          :title="mappingDialogMode === 'edit' ? '编辑模型映射' : '添加模型映射'"
          width="560px"
          :close-on-click-modal="false"
          @close="handleMappingDialogClose"
        >
          <el-steps :active="mappingDialogStep - 1" simple style="margin-bottom: 20px">
            <el-step title="选择上游账号" />
            <el-step title="选择上游模型" />
            <el-step title="输入下游名称" />
          </el-steps>

          <el-form label-position="top">
            <el-form-item v-if="mappingDialogStep >= 1" label="上游账号">
              <el-select
                v-model="mappingUpstreamId"
                placeholder="选择上游账号"
                filterable
                style="width: 100%"
                :disabled="mappingDialogMode === 'edit' || savingMapping"
                @change="handleMappingUpstreamChange"
              >
                <el-option
                  v-for="upstream in upstreams"
                  :key="upstream.id"
                  :label="`${upstream.name}（${upstream.supported_models.length} 个模型）`"
                  :value="upstream.id"
                />
              </el-select>
            </el-form-item>

            <el-form-item v-if="mappingDialogStep >= 2" label="上游模型">
              <el-select
                v-model="mappingUpstreamModel"
                placeholder="选择要改名的上游模型"
                filterable
                style="width: 100%"
                :disabled="mappingDialogMode === 'edit' || savingMapping"
                @change="handleMappingModelChange"
              >
                <el-option
                  v-for="model in mappingModelOptions"
                  :key="model"
                  :label="model"
                  :value="model"
                  :disabled="mappingModelIsMapped(model)"
                />
              </el-select>
              <div class="form-help-text">
                数据源为该上游已配置的 supported_models（含各 key 的模型清单），已映射的条目不可重复选择；不会调用上游 /v1/models。
              </div>
            </el-form-item>

            <el-form-item v-if="mappingDialogStep >= 3" label="下游模型名称">
              <el-input
                v-model="mappingDownstream"
                placeholder="例如: gpt-4-premium"
                maxlength="100"
                :disabled="savingMapping"
                @keyup.enter="handleMappingDialogConfirm"
              />
              <div class="form-help-text">
                下游请求使用此名称，用量/配额也按下游名称记录；发往上游时仍使用上游原拼写。
              </div>
            </el-form-item>
          </el-form>

          <template #footer>
            <el-button v-if="mappingDialogStep > 1 && mappingDialogMode === 'add'" :disabled="savingMapping" @click="mappingDialogStep--">
              上一步
            </el-button>
            <el-button
              v-if="mappingDialogStep < 3 && mappingDialogMode === 'add'"
              type="primary"
              :disabled="!mappingStepReady"
              @click="mappingDialogStep++"
            >
              下一步
            </el-button>
            <el-button v-if="mappingDialogStep === 3 || mappingDialogMode === 'edit'" :disabled="savingMapping" @click="mappingDialogVisible = false">
              取消
            </el-button>
            <el-button
              v-if="mappingDialogStep === 3 || mappingDialogMode === 'edit'"
              type="primary"
              :loading="savingMapping"
              :disabled="!mappingDownstream.trim()"
              @click="handleMappingDialogConfirm"
            >
              确定
            </el-button>
          </template>
        </el-dialog>
      </el-tab-pane>

      <!-- ============ Tab 2: 全局规则（原内容保留） ============ -->
      <el-tab-pane label="全局规则" name="aliases">
        <el-alert
          title="跨上游的拼写归并与显示控制；按上游改名请用「模型映射」tab。例如：将 'deepseek-chat' 和 'DeepSeek-Chat' 统一映射到 'deepseek-v3'。"
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

            <div v-if="selectedUpstreamId" class="models-list">
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

        <!-- 全局规则编辑对话框 -->
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
      </el-tab-pane>
    </el-tabs>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, reactive, nextTick, computed, watch } from 'vue'
import { ElMessage, ElMessageBox, type FormInstance, type FormRules } from 'element-plus'
import { RefreshCw, Plus, Edit, Trash2, Search } from '@lucide/vue'
import { adminApi } from '@/api/admin'
import type { ModelAliasRule, UpstreamConfig, UpstreamModelMapping } from '@/types'

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

interface MappingRow {
  upstreamId: string
  upstreamName: string
  upstreamModel: string
  downstreamModel: string
  stale: boolean
}

const activeTab = ref<'mappings' | 'aliases'>('mappings')
const loadingAll = ref(false)

// ---------------------------------------------------------------- Tab 1
const loadingUpstreams = ref(false)
const upstreams = ref<UpstreamConfig[]>([])
const filterUpstreamId = ref('')
const mappingSearch = ref('')
const mappingPage = ref(1)
const mappingPageSize = ref(10)

const mappingDialogVisible = ref(false)
const mappingDialogMode = ref<'add' | 'edit'>('add')
const mappingDialogStep = ref(1)
const mappingUpstreamId = ref('')
const mappingUpstreamModel = ref('')
const mappingDownstream = ref('')
const savingMapping = ref(false)

const canonical = (model: string) => model.trim().toLowerCase()

/** 该上游对外可见的模型清单：supported_models ∪ api_key_models[].supported_models，按 canonical 去重（首选原拼写）。 */
function upstreamModelList(upstream: UpstreamConfig): string[] {
  const raw: string[] = [...(upstream.supported_models || [])]
  for (const keyModel of upstream.api_key_models || []) {
    raw.push(...(keyModel.supported_models || []))
  }
  const seen = new Set<string>()
  const models: string[] = []
  for (const model of raw) {
    const key = canonical(model)
    if (!model.trim() || seen.has(key)) continue
    seen.add(key)
    models.push(model.trim())
  }
  return models
}

const mappingRows = computed<MappingRow[]>(() => {
  const rows: MappingRow[] = []
  for (const upstream of upstreams.value) {
    for (const mapping of upstream.model_mappings || []) {
      const models = upstreamModelList(upstream)
      rows.push({
        upstreamId: upstream.id,
        upstreamName: upstream.name || upstream.id,
        upstreamModel: mapping.upstream_model,
        downstreamModel: mapping.downstream_model,
        stale: !models.some(model => canonical(model) === canonical(mapping.upstream_model))
      })
    }
  }
  return rows
})

const filteredMappingRows = computed<MappingRow[]>(() => {
  const keyword = mappingSearch.value.trim().toLowerCase()
  return mappingRows.value.filter(row => {
    if (filterUpstreamId.value && row.upstreamId !== filterUpstreamId.value) return false
    if (!keyword) return true
    return (
      row.upstreamName.toLowerCase().includes(keyword) ||
      row.upstreamModel.toLowerCase().includes(keyword) ||
      row.downstreamModel.toLowerCase().includes(keyword)
    )
  })
})

/** 客户端切片分页：映射数据一次性全量拉取，表格只渲染当前页 */
const pagedMappingRows = computed<MappingRow[]>(() => {
  const start = (mappingPage.value - 1) * mappingPageSize.value
  return filteredMappingRows.value.slice(start, start + mappingPageSize.value)
})

// 筛选条件变化时回到第一页
watch([filterUpstreamId, mappingSearch], () => {
  mappingPage.value = 1
})

// 删除映射导致行数减少时，页码自动回落到有效范围
watch(
  () => filteredMappingRows.value.length,
  () => {
    const maxPage = Math.max(1, Math.ceil(filteredMappingRows.value.length / mappingPageSize.value))
    if (mappingPage.value > maxPage) mappingPage.value = maxPage
  },
)

const mappingModelOptions = computed<string[]>(() => {
  const upstream = upstreams.value.find(u => u.id === mappingUpstreamId.value)
  return upstream ? upstreamModelList(upstream) : []
})

const mappingModelIsMapped = (model: string) => {
  const upstream = upstreams.value.find(u => u.id === mappingUpstreamId.value)
  if (!upstream) return false
  return (upstream.model_mappings || []).some(
    mapping => canonical(mapping.upstream_model) === canonical(model)
  )
}

const mappingStepReady = computed(() => {
  if (mappingDialogStep.value === 1) return !!mappingUpstreamId.value
  if (mappingDialogStep.value === 2) {
    return !!mappingUpstreamModel.value && !mappingModelIsMapped(mappingUpstreamModel.value)
  }
  return !!mappingDownstream.value.trim()
})

const handleMappingUpstreamChange = () => {
  mappingUpstreamModel.value = ''
  mappingDownstream.value = ''
  mappingDialogStep.value = 2
}

const handleMappingModelChange = () => {
  mappingDownstream.value = ''
  mappingDialogStep.value = 3
}

const openAddMapping = () => {
  mappingDialogMode.value = 'add'
  mappingDialogStep.value = 1
  mappingUpstreamId.value = ''
  mappingUpstreamModel.value = ''
  mappingDownstream.value = ''
  mappingDialogVisible.value = true
}

const openEditMapping = (row: MappingRow) => {
  mappingDialogMode.value = 'edit'
  mappingDialogStep.value = 3
  mappingUpstreamId.value = row.upstreamId
  mappingUpstreamModel.value = row.upstreamModel
  mappingDownstream.value = row.downstreamModel
  mappingDialogVisible.value = true
}

const handleMappingDialogClose = () => {
  mappingUpstreamId.value = ''
  mappingUpstreamModel.value = ''
  mappingDownstream.value = ''
  mappingDialogStep.value = 1
}

const handleMappingDialogConfirm = async () => {
  const upstreamId = mappingUpstreamId.value
  const upstreamModel = mappingUpstreamModel.value.trim()
  const downstreamModel = mappingDownstream.value.trim()
  if (!upstreamId || !upstreamModel || !downstreamModel) return

  const upstream = upstreams.value.find(u => u.id === upstreamId)
  if (!upstream) return

  savingMapping.value = true
  try {
    const nextMappings: UpstreamModelMapping[] = [...(upstream.model_mappings || [])]
    const matched = nextMappings.findIndex(
      m => canonical(m.upstream_model) === canonical(upstreamModel)
    )
    if (mappingDialogMode.value === 'edit' && matched >= 0) {
      nextMappings[matched] = { upstream_model: upstreamModel, downstream_model: downstreamModel }
    } else if (mappingDialogMode.value === 'add' && matched < 0) {
      nextMappings.push({ upstream_model: upstreamModel, downstream_model: downstreamModel })
    } else {
      ElMessage.error('该上游模型已存在映射，请勿重复添加')
      return
    }
    await adminApi.updateUpstream(upstreamId, { model_mappings: nextMappings })
    ElMessage.success(mappingDialogMode.value === 'edit' ? '映射已更新' : '映射已添加')
    mappingDialogVisible.value = false
    await loadUpstreams()
  } catch (err) {
    const error = err as ApiError
    const message =
      error.response?.data?.error?.message || error.response?.data?.message || error.message || '保存失败'
    ElMessage.error('保存失败: ' + message)
  } finally {
    savingMapping.value = false
  }
}

const handleDeleteMapping = async (row: MappingRow) => {
  try {
    await ElMessageBox.confirm(
      `确定要删除映射 "${row.upstreamModel} → ${row.downstreamModel}"（上游：${row.upstreamName}）吗？`,
      '确认删除',
      {
        confirmButtonText: '删除',
        cancelButtonText: '取消',
        type: 'warning'
      }
    )
  } catch {
    return
  }
  const upstream = upstreams.value.find(u => u.id === row.upstreamId)
  if (!upstream) return
  savingMapping.value = true
  try {
    const nextMappings = (upstream.model_mappings || []).filter(
      m => canonical(m.upstream_model) !== canonical(row.upstreamModel)
    )
    await adminApi.updateUpstream(row.upstreamId, { model_mappings: nextMappings })
    ElMessage.success('映射已删除')
    await loadUpstreams()
  } catch (err) {
    const error = err as ApiError
    const message =
      error.response?.data?.error?.message || error.response?.data?.message || error.message || '删除失败'
    ElMessage.error('删除失败: ' + message)
  } finally {
    savingMapping.value = false
  }
}

// ---------------------------------------------------------------- Tab 2 (原逻辑保留)
const loading = ref(false)
const saving = ref(false)
const rules = ref<ModelAliasRule[]>([])
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
  return upstream ? upstreamModelList(upstream) : []
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

const reloadAll = async () => {
  loadingAll.value = true
  try {
    await Promise.all([loadUpstreams(), loadAliases()])
  } finally {
    loadingAll.value = false
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

const handleQuickAdd = (modelName: string) => {
  dialogMode.value = 'quick'
  editIndex.value = -1
  editForm.canonical = modelName
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
  width: 100%;
  max-width: none;
}

.header-actions {
  display: flex;
  gap: 12px;
}

.model-mappings-tabs {
  margin-top: 8px;
}

.mapping-table-pagination {
  display: flex;
  justify-content: flex-end;
  margin-top: 14px;
}

.mapping-row-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  white-space: nowrap;
}

/* 映射表格（Tab 1） */
.mappings-panel {
  background: var(--el-bg-color);
  border-radius: 8px;
  padding: 20px;
  border: 1px solid var(--el-border-color-light);
  min-height: 300px;
}

.mappings-toolbar {
  display: flex;
  gap: 12px;
  align-items: center;
  margin-bottom: 16px;
  flex-wrap: wrap;
}

.upstream-name {
  font-weight: 600;
}

.mono-cell {
  font-family: var(--el-font-family-mono, monospace);
  font-size: 13px;
  color: var(--el-text-color-primary);
}

.downstream-cell {
  font-family: var(--el-font-family-mono, monospace);
  font-size: 13px;
  color: var(--el-color-primary);
}

.form-help-text {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-top: 4px;
  line-height: 1.5;
}

/* 全局规则（Tab 2，原样式） */
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
</style>
