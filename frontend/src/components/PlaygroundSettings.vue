<template>
  <div class="playground-settings">
    <div>
      <p class="crc-eyebrow">PLAYGROUND // TUNING</p>
      <h2 class="playground-settings__title">模型设置</h2>
    </div>

    <el-alert
      v-if="statusMessage"
      :type="statusType"
      :closable="false"
      show-icon
      class="playground-settings__status"
    >
      {{ statusMessage }}
    </el-alert>

    <div class="playground-settings__section">
      <label class="playground-settings__label">模型</label>
      <el-select
        :model-value="selectedModel"
        placeholder="选择模型"
        filterable
        clearable
        :disabled="busy || !modelOptions.length"
        @update:model-value="emit('update:selectedModel', $event)"
      >
        <el-option
          v-for="model in modelOptions"
          :key="model"
          :label="model"
          :value="model"
        />
      </el-select>
    </div>

    <div class="playground-settings__section">
      <div class="playground-settings__label-row">
        <label class="playground-settings__label">温度 {{ temperature.toFixed(1) }}</label>
        <el-switch
          :model-value="temperatureEnabled"
          inline-prompt
          active-text="自定义"
          inactive-text="自动"
          :disabled="busy"
          @update:model-value="emit('update:temperatureEnabled', $event)"
        />
      </div>
      <el-slider
        :model-value="temperature"
        :min="0"
        :max="2"
        :step="0.1"
        :disabled="busy || !temperatureEnabled"
        :show-tooltip="false"
        @update:model-value="emit('update:temperature', Number($event))"
      />
    </div>

    <div class="playground-settings__section">
      <div class="playground-settings__label-row">
        <label class="playground-settings__label">max_tokens</label>
        <el-switch
          :model-value="maxTokensEnabled"
          inline-prompt
          active-text="自定义"
          inactive-text="自动"
          :disabled="busy"
          @update:model-value="emit('update:maxTokensEnabled', $event)"
        />
      </div>
      <el-input-number
        :model-value="maxTokens"
        :min="1"
        :max="999999"
        :step="1024"
        :disabled="busy || !maxTokensEnabled"
        controls-position="right"
        @update:model-value="emit('update:maxTokens', Number($event))"
      />
    </div>

    <div class="playground-settings__section">
      <div class="playground-settings__label-row">
        <label class="playground-settings__label">推理强度</label>
        <el-switch
          :model-value="inferenceStrengthEnabled"
          inline-prompt
          active-text="自定义"
          inactive-text="自动"
          :disabled="busy"
          @update:model-value="emit('update:inferenceStrengthEnabled', $event)"
        />
      </div>
      <el-select
        :model-value="inferenceStrength"
        :disabled="busy || !inferenceStrengthEnabled"
        @update:model-value="emit('update:inferenceStrength', $event)"
      >
        <el-option
          v-for="level in inferenceStrengthOptions"
          :key="level"
          :label="level"
          :value="level"
        />
      </el-select>
    </div>

    <div class="playground-settings__actions">
      <el-button :disabled="busy" @click="emit('clear')">
        <Trash2 :size="14" :stroke-width="1.8" />
        <span>清空对话</span>
      </el-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { Trash2 } from '@lucide/vue'

defineProps<{
  modelOptions: string[]
  selectedModel: string
  busy: boolean
  statusMessage: string
  statusType: 'success' | 'info' | 'warning' | 'error'
  temperature: number
  temperatureEnabled: boolean
  maxTokens: number
  maxTokensEnabled: boolean
  inferenceStrength: string
  inferenceStrengthOptions: readonly string[]
  inferenceStrengthEnabled: boolean
}>()

const emit = defineEmits<{
  clear: []
  'update:selectedModel': [value: string]
  'update:temperature': [value: number]
  'update:temperatureEnabled': [value: boolean]
  'update:maxTokens': [value: number]
  'update:maxTokensEnabled': [value: boolean]
  'update:inferenceStrength': [value: string]
  'update:inferenceStrengthEnabled': [value: boolean]
}>()
</script>

<style scoped>
.playground-settings {
  display: flex;
  flex-direction: column;
  gap: 18px;
  width: 100%;
  min-height: 100%;
}

.playground-settings__title {
  margin: 6px 0 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 16px;
  font-weight: 600;
  letter-spacing: -0.01em;
  line-height: 1.4;
}

.playground-settings__status {
  margin: 0;
}

.playground-settings__section {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.playground-settings__label-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.playground-settings__label {
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.06em;
}

.playground-settings__section :deep(.el-select),
.playground-settings__section :deep(.el-input-number) {
  width: 100%;
}

.playground-settings :deep(.el-switch:not(.is-checked) .el-switch__inner-wrapper) {
  color: var(--crc-text-strong);
}

.playground-settings__actions {
  margin-top: auto;
  padding-top: 16px;
  border-top: 1px solid var(--crc-border);
}
</style>
