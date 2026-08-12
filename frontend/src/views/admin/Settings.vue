<template>
  <div class="crc-page settings-page">
    <header class="crc-page-header settings-header">
      <div>
        <p class="crc-eyebrow">GATEWAY // RUNTIME</p>
        <h1 class="crc-page-title">网关设置</h1>
      </div>
      <div class="settings-actions">
        <el-tooltip content="重新加载设置" placement="bottom">
          <el-button
            :icon="RefreshCw"
            :loading="loading"
            circle
            aria-label="重新加载设置"
            @click="loadSettings"
          />
        </el-tooltip>
        <el-button
          :icon="RotateCcw"
          :disabled="!loadedSettings || saving || !dirty"
          @click="resetToLoaded"
        >
          重置
        </el-button>
        <el-button
          type="primary"
          :icon="Save"
          :loading="saving"
          :disabled="!canSave"
          @click="saveSettings"
        >
          保存
        </el-button>
      </div>
    </header>

    <section v-if="loadedSettings" class="settings-status-band" aria-label="设置状态">
      <div class="settings-status-item">
        <span>来源</span>
        <strong>{{ sourceLabel }}</strong>
      </div>
      <div class="settings-status-item">
        <span>修订</span>
        <strong class="crc-mono">{{ revision }}</strong>
      </div>
      <div class="settings-status-item">
        <span>本次状态</span>
        <strong :class="{ 'is-warning': dirty }">{{ immediateStatusLabel }}</strong>
      </div>
      <div class="settings-status-item">
        <span>重启状态</span>
        <strong :class="{ 'is-warning': serverRestartRequired || unsavedRestartFields.length > 0 }">
          {{ restartStatusLabel }}
        </strong>
      </div>
    </section>

    <div v-if="conflictRevision !== null" class="settings-notice settings-notice--warning">
      <span>服务器设置已更新，当前修订为 {{ conflictRevision }}。</span>
      <el-button size="small" :icon="RefreshCw" @click="loadSettings">重新加载</el-button>
    </div>

    <div v-if="loadFailed" class="settings-notice settings-notice--danger">
      <span>设置加载失败。</span>
      <el-button size="small" :icon="RefreshCw" @click="loadSettings">重试</el-button>
    </div>

    <section v-if="!editableSettings" v-loading="loading" class="settings-loading" />

    <el-tabs v-else v-model="activeGroup" class="settings-tabs">
      <el-tab-pane
        v-for="group in runtimeSettingGroups"
        :key="group.id"
        :name="group.id"
      >
        <template #label>
          <span class="settings-tab-label">
            {{ group.label }}
            <span v-if="restartCountForGroup(group.id)" class="settings-tab-count">
              {{ restartCountForGroup(group.id) }}
            </span>
          </span>
        </template>

        <section class="settings-section" :aria-label="group.label">
          <div
            v-for="field in fieldsForGroup(group.id)"
            :key="field.key"
            class="settings-row"
          >
            <div class="settings-field-label">
              <div class="settings-field-title">
                <strong>{{ field.label }}</strong>
                <el-tag
                  v-if="field.apply === 'restart'"
                  size="small"
                  type="warning"
                  effect="plain"
                >
                  重启后生效
                </el-tag>
              </div>
              <code>{{ field.key }}</code>
              <p v-if="field.description" class="settings-field-description">
                {{ field.description }}
              </p>
            </div>

            <div class="settings-control-column">
              <el-switch
                v-if="field.control === 'switch'"
                :model-value="booleanValue(field.key)"
                active-text="开启"
                inactive-text="关闭"
                @update:model-value="updateBoolean(field.key, $event)"
              />

              <el-input
                v-else-if="field.control === 'text'"
                :model-value="textValue(field.key)"
                :maxlength="field.maxLength"
                @update:model-value="updateText(field.key, $event)"
              />

              <el-input
                v-else-if="field.control === 'number-list'"
                :model-value="probeDelayInput"
                placeholder="100, 500, 1000"
                class="crc-mono"
                @update:model-value="updateProbeDelayInput"
              />

              <div v-else class="settings-number-control">
                <el-input-number
                  :model-value="numberValue(field.key)"
                  :min="field.min"
                  :max="field.max"
                  :step="field.step || 1"
                  :precision="field.integer === false ? 2 : 0"
                  controls-position="right"
                  @update:model-value="updateNumber(field.key, $event)"
                />
                <span v-if="field.unit" class="settings-unit">{{ field.unit }}</span>
              </div>

              <span v-if="fieldError(field.key)" class="settings-field-error">
                {{ fieldError(field.key) }}
              </span>
            </div>
          </div>
        </section>
      </el-tab-pane>
    </el-tabs>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { RefreshCw, RotateCcw, Save } from '@lucide/vue'
import { adminApi } from '@/api/admin'
import type {
  RuntimeSettingKey,
  RuntimeSettings,
  RuntimeSettingsResponse,
  RuntimeSettingsSource,
  RuntimeSettingsUpdateResponse
} from '@/types'
import {
  changedRestartFields,
  cloneRuntimeSettings,
  formatProbeDelays,
  isRuntimeSettingsDirty,
  parseProbeDelays,
  runtimeSettingFields,
  runtimeSettingGroups,
  validateRuntimeSettings,
  type RuntimeSettingGroupId
} from '@/utils/runtimeSettings'

type RuntimeSettingsApiError = {
  response?: {
    status?: number
    data?: {
      error?: {
        message?: string
        field?: RuntimeSettingKey
        current_revision?: number
      }
    }
  }
}

const activeGroup = ref<RuntimeSettingGroupId>('general')
const loading = ref(false)
const saving = ref(false)
const loadFailed = ref(false)
const revision = ref(0)
const source = ref<RuntimeSettingsSource>('startup')
const serverRestartRequired = ref(false)
const serverRestartFields = ref<RuntimeSettingKey[]>([])
const appliedImmediately = ref<RuntimeSettingKey[]>([])
const conflictRevision = ref<number | null>(null)
const backendFieldError = ref<{ field: RuntimeSettingKey; message: string } | null>(null)
const loadedSettings = ref<RuntimeSettings | null>(null)
const editableSettings = ref<RuntimeSettings | null>(null)
const probeDelayInput = ref('')

const localErrors = computed(() => {
  if (!editableSettings.value) return {}
  const errors = validateRuntimeSettings(editableSettings.value)
  try {
    parseProbeDelays(probeDelayInput.value)
  } catch (error) {
    errors.upstream_concurrency_probe_delays_ms =
      error instanceof Error ? error.message : '探测延迟无效'
  }
  return errors
})

const dirty = computed(() => {
  if (!loadedSettings.value || !editableSettings.value) return false
  if (isRuntimeSettingsDirty(loadedSettings.value, editableSettings.value)) return true
  try {
    const parsed = parseProbeDelays(probeDelayInput.value)
    return parsed.some(
      (value, index) =>
        value !== loadedSettings.value?.upstream_concurrency_probe_delays_ms[index]
    ) || parsed.length !== loadedSettings.value.upstream_concurrency_probe_delays_ms.length
  } catch {
    return probeDelayInput.value !== formatProbeDelays(
      loadedSettings.value.upstream_concurrency_probe_delays_ms
    )
  }
})

const unsavedRestartFields = computed(() => {
  if (!loadedSettings.value || !editableSettings.value) return []
  return changedRestartFields(loadedSettings.value, editableSettings.value)
})

const canSave = computed(
  () =>
    !loading.value &&
    !saving.value &&
    dirty.value &&
    Object.keys(localErrors.value).length === 0
)

const sourceLabel = computed(() => source.value === 'persisted' ? '已保存设置' : '启动配置')

const immediateStatusLabel = computed(() => {
  if (dirty.value) return '有未保存更改'
  return appliedImmediately.value.length > 0
    ? `${appliedImmediately.value.length} 项即时应用`
    : '已同步'
})

const restartStatusLabel = computed(() => {
  const count = new Set([
    ...serverRestartFields.value,
    ...unsavedRestartFields.value
  ]).size
  return count > 0 ? `${count} 项待重启` : '无需重启'
})

const fieldsForGroup = (group: RuntimeSettingGroupId) =>
  runtimeSettingFields.filter(field => field.group === group)

const restartCountForGroup = (group: RuntimeSettingGroupId) =>
  runtimeSettingFields.filter(field => field.group === group && field.apply === 'restart').length

const settingRecord = () =>
  editableSettings.value as unknown as Record<RuntimeSettingKey, unknown>

const booleanValue = (key: RuntimeSettingKey) => Boolean(editableSettings.value?.[key])
const numberValue = (key: RuntimeSettingKey) => {
  const value = editableSettings.value?.[key]
  return typeof value === 'number' ? value : 0
}
const textValue = (key: RuntimeSettingKey) => {
  const value = editableSettings.value?.[key]
  return typeof value === 'string' ? value : ''
}

const updateBoolean = (key: RuntimeSettingKey, value: boolean) => {
  if (!editableSettings.value) return
  settingRecord()[key] = value
  backendFieldError.value = null
}

const updateNumber = (key: RuntimeSettingKey, value: number | undefined) => {
  if (!editableSettings.value) return
  settingRecord()[key] = value ?? Number.NaN
  backendFieldError.value = null
}

const updateText = (key: RuntimeSettingKey, value: string) => {
  if (!editableSettings.value) return
  settingRecord()[key] = value
  backendFieldError.value = null
}

const updateProbeDelayInput = (value: string) => {
  probeDelayInput.value = value
  backendFieldError.value = null
  if (!editableSettings.value) return
  try {
    editableSettings.value.upstream_concurrency_probe_delays_ms = parseProbeDelays(value)
  } catch {
    // Preserve the last valid array while the editor reports the raw input error.
  }
}

const fieldError = (key: RuntimeSettingKey) =>
  backendFieldError.value?.field === key
    ? backendFieldError.value.message
    : localErrors.value[key]

const applyResponse = (response: RuntimeSettingsResponse | RuntimeSettingsUpdateResponse) => {
  revision.value = response.revision
  source.value = response.source
  serverRestartRequired.value = response.restart_required
  serverRestartFields.value = [...response.restart_required_fields]
  appliedImmediately.value = 'applied_immediately' in response
    ? [...response.applied_immediately]
    : []
  loadedSettings.value = cloneRuntimeSettings(response.settings)
  editableSettings.value = cloneRuntimeSettings(response.settings)
  probeDelayInput.value = formatProbeDelays(response.settings.upstream_concurrency_probe_delays_ms)
  conflictRevision.value = null
  backendFieldError.value = null
  loadFailed.value = false
}

const apiErrorMessage = (error: unknown, fallback: string) => {
  const apiError = error as RuntimeSettingsApiError
  return apiError.response?.data?.error?.message ||
    (error instanceof Error ? error.message : fallback)
}

const showSettingsMessage = (
  type: 'success' | 'warning' | 'error',
  message: string
) => {
  ElMessage.closeAll()
  ElMessage({
    type,
    message,
    customClass: 'settings-feedback-message'
  })
}

const loadSettings = async () => {
  try {
    loading.value = true
    const { data } = await adminApi.getRuntimeSettings()
    applyResponse(data)
  } catch (error) {
    loadFailed.value = true
    showSettingsMessage('error', apiErrorMessage(error, '加载设置失败'))
  } finally {
    loading.value = false
  }
}

const resetToLoaded = () => {
  if (!loadedSettings.value) return
  editableSettings.value = cloneRuntimeSettings(loadedSettings.value)
  probeDelayInput.value = formatProbeDelays(
    loadedSettings.value.upstream_concurrency_probe_delays_ms
  )
  conflictRevision.value = null
  backendFieldError.value = null
}

const saveSettings = async () => {
  if (!editableSettings.value || !canSave.value) return
  const settings = cloneRuntimeSettings(editableSettings.value)
  settings.app_name = settings.app_name.trim()
  settings.upstream_user_agent = settings.upstream_user_agent.trim()
  settings.upstream_concurrency_probe_delays_ms = parseProbeDelays(probeDelayInput.value)

  try {
    saving.value = true
    const { data } = await adminApi.updateRuntimeSettings({
      expected_revision: revision.value,
      settings
    })
    applyResponse(data)
    const message = data.restart_required
      ? `设置已保存，${data.restart_required_fields.length} 项重启后生效`
      : `设置已保存，${data.applied_immediately.length} 项已应用`
    showSettingsMessage('success', message)
  } catch (error) {
    const apiError = error as RuntimeSettingsApiError
    const responseError = apiError.response?.data?.error
    if (apiError.response?.status === 409) {
      conflictRevision.value = responseError?.current_revision ?? revision.value
      showSettingsMessage('warning', '设置已被其他管理员更新')
    } else {
      if (responseError?.field && responseError.message) {
        backendFieldError.value = {
          field: responseError.field,
          message: responseError.message
        }
      }
      showSettingsMessage('error', apiErrorMessage(error, '保存设置失败'))
    }
  } finally {
    saving.value = false
  }
}

onMounted(loadSettings)
</script>

<style scoped>
.settings-page {
  min-height: 100%;
}

.settings-header {
  align-items: center;
}

.settings-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.settings-status-band {
  display: grid;
  grid-template-columns: repeat(4, minmax(120px, 1fr));
  margin-bottom: 18px;
  border-top: 1px solid var(--crc-border);
  border-bottom: 1px solid var(--crc-border);
  background: var(--crc-surface-muted);
}

.settings-status-item {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  min-width: 0;
  gap: 12px;
  padding: 11px 14px;
  border-right: 1px solid var(--crc-border);
}

.settings-status-item:last-child {
  border-right: 0;
}

.settings-status-item span {
  color: var(--crc-text-muted);
  font-size: 12px;
}

.settings-status-item strong {
  color: var(--crc-text-strong);
  font-size: 12px;
  font-weight: 600;
  text-align: right;
}

.settings-status-item strong.is-warning {
  color: var(--crc-warning);
}

.settings-notice {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 16px;
  padding: 10px 12px;
  border-left: 3px solid;
  font-size: 13px;
}

.settings-notice--warning {
  border-color: var(--crc-warning);
  background: var(--crc-warning-soft);
  color: var(--crc-text-strong);
}

.settings-notice--danger {
  border-color: var(--crc-danger);
  background: var(--crc-danger-soft);
  color: var(--crc-text-strong);
}

.settings-loading {
  min-height: 360px;
  border-top: 1px solid var(--crc-border);
}

.settings-tabs :deep(.el-tabs__header) {
  margin-bottom: 0;
}

.settings-tabs :deep(.el-tabs__nav-wrap::after) {
  height: 1px;
  background: var(--crc-border);
}

.settings-tabs :deep(.el-tabs__item) {
  height: 44px;
  color: var(--crc-text-muted);
  font-weight: 600;
}

.settings-tabs :deep(.el-tabs__item.is-active) {
  color: var(--crc-accent);
}

.settings-tabs :deep(.el-tabs__content) {
  overflow: visible;
}

.settings-tab-label {
  display: inline-flex;
  align-items: center;
  gap: 7px;
}

.settings-tab-count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 18px;
  height: 18px;
  padding: 0 4px;
  border: 1px solid var(--crc-border);
  border-radius: 4px;
  color: var(--crc-warning);
  background: var(--crc-warning-soft);
  font-family: var(--crc-font-mono);
  font-size: 10px;
}

.settings-section {
  width: 100%;
}

.settings-row {
  display: grid;
  grid-template-columns: minmax(240px, 1fr) minmax(300px, 430px);
  align-items: center;
  gap: 28px;
  min-height: 78px;
  padding: 14px 4px;
  border-bottom: 1px solid var(--crc-border);
}

.settings-field-label {
  min-width: 0;
}

.settings-field-title {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
  margin-bottom: 5px;
}

.settings-field-title strong {
  color: var(--crc-text-strong);
  font-size: 13px;
  font-weight: 600;
}

.settings-field-label code {
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  overflow-wrap: anywhere;
}

.settings-field-title :deep(.el-tag) {
  border-radius: 4px;
  flex-shrink: 0;
}

.settings-control-column {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  min-width: 0;
  gap: 5px;
}

.settings-control-column > :deep(.el-input),
.settings-control-column > .crc-mono {
  width: 100%;
}

.settings-number-control {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 48px;
  align-items: center;
  width: 100%;
  gap: 10px;
}

.settings-number-control :deep(.el-input-number) {
  width: 100%;
}

.settings-unit {
  color: var(--crc-text-muted);
  font-size: 11px;
  text-align: left;
  white-space: nowrap;
}

.settings-field-error {
  width: 100%;
  color: var(--crc-danger);
  font-size: 11px;
  line-height: 1.4;
  text-align: left;
}

@media (max-width: 900px) {
  .settings-status-band {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .settings-status-item:nth-child(2) {
    border-right: 0;
  }

  .settings-status-item:nth-child(-n + 2) {
    border-bottom: 1px solid var(--crc-border);
  }

  .settings-row {
    grid-template-columns: minmax(200px, 1fr) minmax(260px, 1fr);
    gap: 18px;
  }
}

@media (max-width: 767px) {
  .settings-header {
    align-items: flex-start;
  }

  .settings-actions {
    width: 100%;
  }

  .settings-actions .el-button:not(.is-circle) {
    flex: 1;
    min-width: 0;
    margin-left: 0;
  }

  .settings-status-band {
    grid-template-columns: 1fr;
  }

  .settings-status-item,
  .settings-status-item:nth-child(2) {
    border-right: 0;
    border-bottom: 1px solid var(--crc-border);
  }

  .settings-status-item:last-child {
    border-bottom: 0;
  }

  .settings-notice {
    align-items: flex-start;
    flex-direction: column;
  }

  .settings-tabs :deep(.el-tabs__nav-scroll) {
    overflow-x: auto;
  }

  .settings-row {
    grid-template-columns: 1fr;
    gap: 10px;
    padding: 16px 0;
  }

  .settings-control-column {
    align-items: stretch;
  }
}
</style>
