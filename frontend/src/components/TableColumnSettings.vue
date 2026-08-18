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
      <el-input
        v-model="search"
        size="small"
        clearable
        placeholder="搜索字段"
        class="table-column-search"
      />

      <div class="table-column-options">
        <el-checkbox
          v-for="column in filteredColumns"
          :key="column.key"
          class="table-column-option"
          :model-value="isSelected(column.key)"
          :label="column.label"
          @change="toggleColumn(column.key, $event === true)"
        />
        <div v-if="filteredColumns.length === 0" class="table-column-empty">没有匹配字段</div>
      </div>

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
import { computed, ref } from 'vue'
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

const search = ref('')
const filteredColumns = computed(() => {
  const query = search.value.trim().toLowerCase()
  if (!query) return props.columns
  return props.columns.filter(column =>
    column.label.toLowerCase().includes(query) ||
    column.key.toLowerCase().includes(query)
  )
})
const isSelected = (key: string) => props.modelValue.includes(key)

const orderedKeys = (keys: string[]) => props.columns
  .map(column => column.key)
  .filter(key => keys.includes(key))

const toggleColumn = (key: string, checked: boolean) => {
  if (!checked && props.modelValue.length === 1) return
  const next = checked
    ? orderedKeys([...props.modelValue, key])
    : props.modelValue.filter(item => item !== key)
  emit('update:modelValue', next)
}

const availableKeys = computed(() => new Set(props.columns.map(column => column.key)))
const validSelectedCount = computed(
  () => props.modelValue.filter(key => availableKeys.value.has(key)).length
)
const allSelected = computed(() => validSelectedCount.value === props.columns.length)
const someSelected = computed(() => validSelectedCount.value > 0)

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

.table-column-options {
  display: grid;
  gap: 8px;
  max-height: 260px;
  overflow-y: auto;
}

.table-column-empty {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}

.table-column-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
</style>
