<template>
  <div class="playground-settings">
    <div class="playground-settings__head">
      <p class="crc-eyebrow">PLAYGROUND // TUNING</p>
      <h2 class="playground-settings__title">参数设置</h2>
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

    <div class="playground-settings__group">
      <h3 class="playground-settings__group-title">生成参数</h3>

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
    </div>

    <div class="playground-settings__group">
      <h3 class="playground-settings__group-title">会话</h3>
      <div class="playground-settings__section">
        <el-button class="playground-settings__clear" :disabled="busy" @click="emit('clear')">
          <Trash2 :size="14" :stroke-width="1.8" />
          <span>清空对话</span>
        </el-button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { Trash2 } from '@lucide/vue'

defineProps<{
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

.playground-settings__head {
  padding-bottom: 2px;
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

.playground-settings__group {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding-top: 2px;
}

.playground-settings__group-title {
  margin: 0;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--crc-border);
  color: var(--crc-text-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.06em;
  text-transform: uppercase;
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

.playground-settings__clear {
  width: 100%;
}
</style>
