<template>
  <div class="console-shell" :class="{ 'console-shell--collapsed': collapsed }">
    <aside class="console-shell__sidebar" :aria-label="`${contextLabel}导航`">
      <div class="console-shell__brand">
        <BrandLogo :size="34" class="console-shell__brand-mark" />
        <div v-show="!collapsed" class="console-shell__brand-copy">
          <strong>Chat Responses</strong>
          <span>{{ contextLabel }}</span>
        </div>
      </div>

      <el-menu
        class="console-shell__menu"
        :default-active="activePath"
        :collapse="collapsed"
        :collapse-transition="false"
        @select="navigate"
      >
        <template v-for="group in navigationGroups" :key="group.label">
          <div v-if="group.label && !collapsed" class="console-shell__group-label">
            {{ group.label }}
          </div>
          <el-menu-item v-for="item in group.items" :key="item.path" :index="item.path">
            <el-icon><component :is="item.icon" /></el-icon>
            <template #title>{{ item.label }}</template>
          </el-menu-item>
        </template>
      </el-menu>

      <button
        type="button"
        class="console-shell__collapse"
        :aria-label="collapsed ? '展开侧边栏' : '收起侧边栏'"
        :title="collapsed ? '展开侧边栏' : '收起侧边栏'"
        @click="emit('toggle-collapse')"
      >
        <el-icon><Expand v-if="collapsed" /><Fold v-else /></el-icon>
        <span v-if="!collapsed">收起侧边栏</span>
      </button>
    </aside>

    <section class="console-shell__workspace">
      <header class="console-shell__topbar">
        <div class="console-shell__heading">
          <button
            type="button"
            class="console-shell__icon-button console-shell__mobile-menu"
            aria-label="打开导航"
            title="打开导航"
            @click="emit('update:mobileOpen', true)"
          >
            <el-icon><Menu /></el-icon>
          </button>
          <div>
            <p class="console-shell__context">{{ contextLabel }}</p>
            <h1>{{ pageTitle }}</h1>
          </div>
        </div>

        <div class="console-shell__actions">
          <ThemeSwitcher />
          <el-dropdown placement="bottom-end" trigger="click" @command="handleAccountCommand">
            <button type="button" class="console-shell__account" aria-label="打开账户菜单">
              <span class="console-shell__account-avatar" aria-hidden="true">
                <el-icon><User /></el-icon>
              </span>
              <span class="console-shell__account-label">{{ accountLabel }}</span>
              <el-icon class="console-shell__account-arrow"><ArrowDown /></el-icon>
            </button>
            <template #dropdown>
              <el-dropdown-menu>
                <el-dropdown-item command="logout">
                  <el-icon><SwitchButton /></el-icon>
                  退出登录
                </el-dropdown-item>
              </el-dropdown-menu>
            </template>
          </el-dropdown>
        </div>
      </header>

      <main class="console-shell__content">
        <slot />
      </main>
    </section>

    <el-drawer
      v-model="mobileDrawerOpen"
      class="console-shell__drawer"
      direction="ltr"
      size="min(86vw, 320px)"
      :show-close="false"
      :with-header="false"
    >
      <div class="console-shell__mobile-brand">
        <BrandLogo :size="34" class="console-shell__brand-mark" />
        <div class="console-shell__brand-copy">
          <strong>Chat Responses</strong>
          <span>{{ contextLabel }}</span>
        </div>
        <button
          type="button"
          class="console-shell__icon-button"
          aria-label="关闭导航"
          title="关闭导航"
          @click="emit('update:mobileOpen', false)"
        >
          <el-icon><Close /></el-icon>
        </button>
      </div>

      <el-menu class="console-shell__mobile-nav" :default-active="activePath" @select="navigate">
        <template v-for="group in navigationGroups" :key="group.label">
          <div v-if="group.label" class="console-shell__group-label">{{ group.label }}</div>
          <el-menu-item v-for="item in group.items" :key="item.path" :index="item.path">
            <el-icon><component :is="item.icon" /></el-icon>
            <template #title>{{ item.label }}</template>
          </el-menu-item>
        </template>
      </el-menu>
    </el-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import {
  ArrowDown,
  Close,
  Expand,
  Fold,
  Menu,
  SwitchButton,
  User
} from '@element-plus/icons-vue'
import ThemeSwitcher from '@/components/ThemeSwitcher.vue'
import BrandLogo from '@/components/BrandLogo.vue'
import type { AppNavItem } from '@/types/navigation'

const props = defineProps<{
  items: AppNavItem[]
  activePath: string
  pageTitle: string
  contextLabel: string
  accountLabel: string
  collapsed: boolean
  mobileOpen: boolean
}>()

const emit = defineEmits<{
  navigate: [path: string]
  logout: []
  'toggle-collapse': []
  'update:mobileOpen': [value: boolean]
}>()

const navigationGroups = computed(() => {
  const groups = new Map<string, AppNavItem[]>()
  for (const item of props.items) {
    const label = item.group ?? ''
    const items = groups.get(label) ?? []
    items.push(item)
    groups.set(label, items)
  }
  return Array.from(groups, ([label, items]) => ({ label, items }))
})

const mobileDrawerOpen = computed({
  get: () => props.mobileOpen,
  set: (value: boolean) => emit('update:mobileOpen', value)
})

const navigate = (path: string) => {
  emit('navigate', path)
  emit('update:mobileOpen', false)
}

const handleAccountCommand = (command: string) => {
  if (command === 'logout') emit('logout')
}
</script>

<style scoped>
.console-shell {
  display: flex;
  width: 100%;
  height: 100vh;
  min-height: 0;
  color: var(--crc-text);
  background: var(--crc-canvas);
}

.console-shell__sidebar {
  position: relative;
  z-index: 10;
  display: flex;
  width: var(--crc-sidebar-expanded);
  min-width: var(--crc-sidebar-expanded);
  height: 100%;
  flex-direction: column;
  border-right: 1px solid var(--crc-border);
  background: var(--crc-surface);
  transition: width var(--crc-duration) var(--crc-ease),
    min-width var(--crc-duration) var(--crc-ease),
    background-color var(--crc-duration) var(--crc-ease),
    border-color var(--crc-duration) var(--crc-ease);
}

.console-shell--collapsed .console-shell__sidebar {
  width: var(--crc-sidebar-collapsed);
  min-width: var(--crc-sidebar-collapsed);
}

.console-shell__brand,
.console-shell__mobile-brand {
  display: flex;
  height: var(--crc-topbar-height);
  min-height: var(--crc-topbar-height);
  padding: 0 14px;
  align-items: center;
  gap: 10px;
  border-bottom: 1px solid var(--crc-border);
}

.console-shell__brand-mark {
  width: 34px;
  height: 34px;
  flex: 0 0 34px;
  border-radius: var(--crc-radius);
  transition: transform var(--crc-duration) var(--crc-ease-out),
    box-shadow var(--crc-duration) var(--crc-ease-out);
}

.console-shell__brand:hover .console-shell__brand-mark,
.console-shell__mobile-brand:hover .console-shell__brand-mark {
  transform: translateY(-1px);
}

.console-shell__brand-copy {
  display: flex;
  min-width: 0;
  flex: 1;
  flex-direction: column;
  line-height: 1.2;
}

.console-shell__brand-copy strong {
  overflow: hidden;
  color: var(--crc-text-strong);
  font-size: 13px;
  font-weight: 650;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.console-shell__brand-copy span {
  margin-top: 3px;
  color: var(--crc-text-muted);
  font-size: 11px;
}

.console-shell__menu,
.console-shell__mobile-nav {
  flex: 1;
  overflow-x: hidden;
  overflow-y: auto;
  border-right: 0;
  background: transparent;
}

.console-shell__menu {
  padding: 10px 8px;
}

.console-shell__menu:not(.el-menu--collapse) {
  width: 100%;
}

.console-shell__menu.el-menu--collapse {
  width: 100%;
}

.console-shell__menu :deep(.el-menu-item),
.console-shell__mobile-nav :deep(.el-menu-item) {
  position: relative;
  height: 40px;
  margin: 3px 0;
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  line-height: 40px;
  transition: color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease);
}

.console-shell__menu :deep(.el-menu-item:hover),
.console-shell__mobile-nav :deep(.el-menu-item:hover) {
  color: var(--crc-text-strong);
  background: var(--crc-surface-hover);
}

.console-shell__menu :deep(.el-menu-item.is-active),
.console-shell__mobile-nav :deep(.el-menu-item.is-active) {
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
  font-weight: 600;
}

.console-shell__menu:not(.el-menu--collapse)
  :deep(.el-menu-item.is-active)::before,
.console-shell__mobile-nav :deep(.el-menu-item.is-active)::before {
  content: '';
  position: absolute;
  top: 50%;
  left: 0;
  width: 3px;
  height: 18px;
  border-radius: 999px;
  background: var(--crc-accent);
  transform: translateY(-50%);
  animation: menu-indicator-in var(--crc-duration) var(--crc-ease-out) backwards;
}

@keyframes menu-indicator-in {
  from {
    transform: translateY(-50%) scaleY(0.3);
    opacity: 0;
  }
  to {
    transform: translateY(-50%) scaleY(1);
    opacity: 1;
  }
}

.console-shell__group-label {
  padding: 14px 12px 5px;
  color: var(--crc-text-subtle);
  font-size: 10px;
  font-weight: 650;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.console-shell__collapse {
  display: flex;
  height: 48px;
  margin: 8px;
  padding: 0 12px;
  align-items: center;
  justify-content: center;
  gap: 9px;
  border: 0;
  border-top: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  background: transparent;
  cursor: pointer;
  transition: color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease);
}

.console-shell__collapse:hover {
  color: var(--crc-text-strong);
  background: var(--crc-surface-hover);
}

.console-shell__workspace {
  display: flex;
  min-width: 0;
  min-height: 0;
  flex: 1;
  flex-direction: column;
}

.console-shell__topbar {
  display: flex;
  height: var(--crc-topbar-height);
  min-height: var(--crc-topbar-height);
  padding: 0 20px;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  border-bottom: 1px solid var(--crc-border);
  background: var(--crc-topbar-bg);
  backdrop-filter: saturate(1.4) blur(10px);
  -webkit-backdrop-filter: saturate(1.4) blur(10px);
  transition: background-color var(--crc-duration) var(--crc-ease),
    border-color var(--crc-duration) var(--crc-ease);
}

.console-shell__heading,
.console-shell__actions,
.console-shell__account {
  display: flex;
  align-items: center;
}

.console-shell__heading {
  min-width: 0;
  gap: 10px;
}

.console-shell__heading h1 {
  overflow: hidden;
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 15px;
  font-weight: 650;
  line-height: 1.25;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.console-shell__context {
  display: none;
  margin: 0 0 2px;
  color: var(--crc-text-subtle);
  font-size: 10px;
}

.console-shell__actions {
  flex: 0 0 auto;
  gap: 8px;
}

.console-shell__account {
  min-width: 0;
  height: 36px;
  padding: 0 8px 0 5px;
  gap: 7px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text);
  background: var(--crc-surface);
  cursor: pointer;
  transition: border-color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease);
}

.console-shell__account:hover {
  border-color: var(--crc-border-strong);
  background: var(--crc-surface-hover);
}

.console-shell__account-avatar {
  display: grid;
  width: 26px;
  height: 26px;
  flex: 0 0 26px;
  place-items: center;
  border-radius: var(--crc-radius-sm);
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

.console-shell__account-label {
  max-width: 160px;
  overflow: hidden;
  font-size: 12px;
  font-weight: 550;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.console-shell__account-arrow {
  color: var(--crc-text-subtle);
  font-size: 12px;
}

.console-shell__content {
  min-width: 0;
  min-height: 0;
  flex: 1;
  overflow: auto;
  padding: 22px 24px 28px;
  background: var(--crc-canvas);
  transition: background-color var(--crc-duration) var(--crc-ease);
}

.console-shell__icon-button {
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
    background-color var(--crc-duration-fast) var(--crc-ease);
}

.console-shell__icon-button:hover {
  color: var(--crc-text-strong);
  background: var(--crc-surface-hover);
}

.console-shell__mobile-menu {
  display: none;
}

.console-shell__mobile-brand {
  padding: 0 12px;
}

.console-shell__mobile-nav {
  padding: 8px;
}

:global(.console-shell__drawer .el-drawer__body) {
  display: flex;
  padding: 0;
  flex-direction: column;
  background: var(--crc-surface);
}

@media (max-width: 767px) {
  .console-shell__sidebar {
    display: none;
  }

  .console-shell__topbar {
    padding: 0 12px;
  }

  .console-shell__mobile-menu {
    display: inline-flex;
  }

  .console-shell__context {
    display: block;
  }

  .console-shell__account-label,
  .console-shell__account-arrow {
    display: none;
  }

  .console-shell__account {
    width: 36px;
    padding: 4px;
  }

  .console-shell__account-avatar {
    width: 26px;
    height: 26px;
  }

  .console-shell__content {
    padding: 16px 14px 22px;
  }
}
</style>
