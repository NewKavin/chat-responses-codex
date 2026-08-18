<template>
  <el-popover
    placement="bottom-end"
    :width="320"
    trigger="click"
    :teleported="true"
  >
    <template #reference>
      <el-button class="table-column-settings" aria-label="设置列表展示列">
        <Columns3 :size="15" :stroke-width="2" style="margin-right: 5px" />
        展示列
      </el-button>
    </template>

    <div class="table-column-panel" role="group" aria-label="列表展示列设置">
      <el-select
        :model-value="modelValue"
        multiple
        filterable
        collapse-tags
        collapse-tags-tooltip
        placeholder="选择要展示的字段"
        class="table-column-select"
        @update:model-value="handleSelectionChange"
      >
        <el-option
          v-for="column in columns"
          :key="column.key"
          :label="column.label"
          :value="column.key"
        />
      </el-select>

      <div class="table-column-actions">
        <el-checkbox
          :model-value="allSelected"
          :indeterminate="someSelected && !allSelected"
          @change="toggleAll"
        >
          全选
        </el-checkbox>
        <el-button link type="primary" @click="resetDefaults">恢复默认</el-button>
      </div>
    </div>
  </el-popover>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { Columns3 } from '@lucide/vue'
import type { TableColumnDefinition } from '@/composables/useTableColumns'

const props = withDefaults(defineProps<{
  columns: TableColumnDefinition[]
  modelValue: string[]
  defaultKeys?: string[]
}>(), {
  defaultKeys: () => []
})

const emit = defineEmits<{
  (event: 'update:modelValue', keys: string[]): void
}>()

const allSelected = computed(() => props.modelValue.length === props.columns.length)
const someSelected = computed(() => props.modelValue.length > 0)

const orderedKeys = (keys: string[]) => props.columns
  .map(column => column.key)
  .filter(key => keys.includes(key))

const handleSelectionChange = (keys: string[]) => {
  if (keys.length > 0) emit('update:modelValue', orderedKeys(keys))
}

const toggleAll = (checked: boolean | string | number) => {
  if (checked) {
    emit('update:modelValue', props.columns.map(column => column.key))
    return
  }
  // Element Plus emits false after the last selected item is unchecked.
  emit('update:modelValue', [props.columns[props.columns.length - 1]?.key].filter(Boolean))
}

const resetDefaults = () => {
  const keys = props.defaultKeys.length > 0
    ? props.defaultKeys
    : props.columns.map(column => column.key)
  emit('update:modelValue', orderedKeys(keys))
}
</script>

<style scoped>
.table-column-panel {
  display: grid;
  gap: 10px;
}

.table-column-select {
  width: 100%;
}

.table-column-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
</style>
