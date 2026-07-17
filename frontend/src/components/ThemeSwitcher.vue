<template>
  <el-dropdown trigger="click" placement="bottom-end" @command="handleCommand">
    <button
      type="button"
      class="theme-switcher"
      aria-label="切换主题"
      :title="currentLabel"
    >
      <el-icon :size="17">
        <component :is="currentIcon" />
      </el-icon>
    </button>

    <template #dropdown>
      <el-dropdown-menu class="theme-switcher__menu">
        <el-dropdown-item command="light" :class="{ 'is-active': mode === 'light' }">
          <el-icon><Sunny /></el-icon>
          <span>浅色</span>
          <el-icon v-if="mode === 'light'" class="theme-switcher__check"><Check /></el-icon>
        </el-dropdown-item>
        <el-dropdown-item command="dark" :class="{ 'is-active': mode === 'dark' }">
          <el-icon><Moon /></el-icon>
          <span>深色</span>
          <el-icon v-if="mode === 'dark'" class="theme-switcher__check"><Check /></el-icon>
        </el-dropdown-item>
        <el-dropdown-item command="auto" :class="{ 'is-active': mode === 'auto' }">
          <el-icon><Monitor /></el-icon>
          <span>跟随系统</span>
          <el-icon v-if="mode === 'auto'" class="theme-switcher__check"><Check /></el-icon>
        </el-dropdown-item>
      </el-dropdown-menu>
    </template>
  </el-dropdown>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { Check, Monitor, Moon, Sunny } from '@element-plus/icons-vue'
import { useTheme, type ThemeMode } from '@/composables/useTheme'

const { mode, resolvedTheme, setThemeMode } = useTheme()

const currentIcon = computed(() => {
  if (mode.value === 'auto') return Monitor
  return resolvedTheme.value === 'dark' ? Moon : Sunny
})

const currentLabel = computed(() => {
  if (mode.value === 'auto') return '主题：跟随系统'
  return mode.value === 'dark' ? '主题：深色' : '主题：浅色'
})

const selectLight = () => setThemeMode('light')
const selectDark = () => setThemeMode('dark')
const selectAuto = () => setThemeMode('auto')

const handleCommand = (command: ThemeMode) => {
  if (command === 'light') selectLight()
  else if (command === 'dark') selectDark()
  else selectAuto()
}
</script>

<style scoped>
.theme-switcher {
  display: inline-flex;
  width: 36px;
  height: 36px;
  padding: 0;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  background: var(--crc-surface);
  cursor: pointer;
  transition: color 160ms ease, border-color 160ms ease, background-color 160ms ease;
}

.theme-switcher:hover {
  color: var(--crc-text-strong);
  border-color: var(--crc-border-strong);
  background: var(--crc-surface-hover);
}

.theme-switcher__menu :deep(.el-dropdown-menu__item) {
  min-width: 148px;
  gap: 8px;
}

.theme-switcher__menu :deep(.el-dropdown-menu__item.is-active) {
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

.theme-switcher__check {
  margin-left: auto;
}
</style>
