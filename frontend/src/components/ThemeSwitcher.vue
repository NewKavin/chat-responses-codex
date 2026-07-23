<template>
  <el-dropdown
    placement="bottom-end"
    trigger="click"
    popper-class="theme-switcher__menu"
    @command="handleCommand"
  >
    <button
      type="button"
      class="theme-switcher"
      :aria-label="currentLabel"
      :title="currentLabel"
    >
      <transition name="theme-icon" mode="out-in">
        <component :is="currentIcon" :key="mode" :size="16" :stroke-width="1.8" />
      </transition>
    </button>
    <template #dropdown>
      <el-dropdown-menu>
        <el-dropdown-item command="light" :class="{ 'is-active': mode === 'light' }">
          <Sun :size="14" :stroke-width="1.8" />
          <span>浅色</span>
          <Check v-if="mode === 'light'" :size="13" class="theme-switcher__check" />
        </el-dropdown-item>
        <el-dropdown-item command="dark" :class="{ 'is-active': mode === 'dark' }">
          <Moon :size="14" :stroke-width="1.8" />
          <span>深色</span>
          <Check v-if="mode === 'dark'" :size="13" class="theme-switcher__check" />
        </el-dropdown-item>
        <el-dropdown-item command="auto" :class="{ 'is-active': mode === 'auto' }">
          <Monitor :size="14" :stroke-width="1.8" />
          <span>跟随系统</span>
          <Check v-if="mode === 'auto'" :size="13" class="theme-switcher__check" />
        </el-dropdown-item>
      </el-dropdown-menu>
    </template>
  </el-dropdown>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { Check, Monitor, Moon, Sun } from '@lucide/vue'
import { useTheme, type ThemeMode } from '@/composables/useTheme'

const { mode, resolvedTheme, setThemeMode } = useTheme()

const currentIcon = computed(() => {
  if (mode.value === 'auto') return Monitor
  return resolvedTheme.value === 'dark' ? Moon : Sun
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
  transition: color var(--crc-duration-fast) var(--crc-ease),
    border-color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease),
    box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.theme-switcher:hover {
  color: var(--crc-accent);
  border-color: var(--crc-border-strong);
  background: var(--crc-surface-hover);
  box-shadow: var(--crc-accent-glow);
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

.theme-icon-enter-active,
.theme-icon-leave-active {
  transition: opacity var(--crc-duration-fast) var(--crc-ease),
    transform var(--crc-duration-fast) var(--crc-ease);
}

.theme-icon-enter-from {
  opacity: 0;
  transform: rotate(-90deg) scale(0.6);
}

.theme-icon-leave-to {
  opacity: 0;
  transform: rotate(90deg) scale(0.6);
}
</style>
