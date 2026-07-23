<template>
  <div class="console-shell" :class="{ 'console-shell--collapsed': collapsed }">
    <aside class="console-shell__sidebar" :aria-label="`${contextLabel}导航`">
      <div class="console-shell__brand">
        <BrandLogo :size="36" class="console-shell__brand-mark" />
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
        <template v-for="(group, groupIndex) in navigationGroups" :key="group.label">
          <div v-if="group.label && !collapsed" class="console-shell__group-label">
            <em>{{ String(groupIndex + 1).padStart(2, '0') }}</em>
            {{ group.label }}
          </div>
          <el-menu-item v-for="item in group.items" :key="item.path" :index="item.path">
            <span class="console-shell__nav-icon">
              <component :is="item.icon" :size="16" :stroke-width="1.8" />
            </span>
            <template #title>{{ item.label }}</template>
          </el-menu-item>
        </template>
      </el-menu>

      <div class="console-shell__sidebar-foot">
        <div v-if="!collapsed" class="console-shell__status">
          <span class="crc-pulse-dot" aria-hidden="true"></span>
          <span class="console-shell__status-text">
            <em>GATEWAY</em>
            <strong>ONLINE</strong>
          </span>
        </div>
        <button
          type="button"
          class="console-shell__collapse"
          :aria-label="collapsed ? '展开侧边栏' : '收起侧边栏'"
          :title="collapsed ? '展开侧边栏' : '收起侧边栏'"
          @click="emit('toggle-collapse')"
        >
          <PanelLeftOpen v-if="collapsed" :size="16" :stroke-width="1.8" />
          <PanelLeftClose v-else :size="16" :stroke-width="1.8" />
          <span v-if="!collapsed">收起侧边栏</span>
        </button>
      </div>
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
            <Menu :size="16" :stroke-width="1.8" />
          </button>
          <div class="console-shell__title-stack">
            <p class="console-shell__context">
              <span>CRC</span>
              <ChevronRight :size="10" :stroke-width="2" aria-hidden="true" />
              <span>{{ contextLabel }}</span>
              <ChevronRight :size="10" :stroke-width="2" aria-hidden="true" />
              <b>{{ pageTitle }}</b>
            </p>
            <h1>{{ pageTitle }}</h1>
          </div>
        </div>

        <div class="console-shell__actions">
          <div class="console-shell__clock" aria-hidden="true">
            <span class="console-shell__clock-time">{{ clockTime }}</span>
            <span class="console-shell__clock-zone">{{ clockZone }}</span>
          </div>
          <ThemeSwitcher />
          <el-dropdown placement="bottom-end" trigger="click" @command="handleAccountCommand">
            <button type="button" class="console-shell__account" aria-label="打开账户菜单">
              <span class="console-shell__account-avatar" aria-hidden="true">
                <UserRound :size="14" :stroke-width="1.8" />
              </span>
              <span class="console-shell__account-label">{{ accountLabel }}</span>
              <ChevronDown :size="12" :stroke-width="2" class="console-shell__account-arrow" />
            </button>
            <template #dropdown>
              <el-dropdown-menu>
                <el-dropdown-item command="logout">
                  <LogOut :size="14" :stroke-width="1.8" />
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
          <X :size="16" :stroke-width="1.8" />
        </button>
      </div>

      <el-menu class="console-shell__mobile-nav" :default-active="activePath" @select="navigate">
        <template v-for="(group, groupIndex) in navigationGroups" :key="group.label">
          <div v-if="group.label" class="console-shell__group-label">
            <em>{{ String(groupIndex + 1).padStart(2, '0') }}</em>
            {{ group.label }}
          </div>
          <el-menu-item v-for="item in group.items" :key="item.path" :index="item.path">
            <span class="console-shell__nav-icon">
              <component :is="item.icon" :size="16" :stroke-width="1.8" />
            </span>
            <template #title>{{ item.label }}</template>
          </el-menu-item>
        </template>
      </el-menu>
    </el-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import {
  ChevronDown,
  ChevronRight,
  LogOut,
  Menu,
  PanelLeftClose,
  PanelLeftOpen,
  UserRound,
  X
} from '@lucide/vue'
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
  (event: 'navigate', path: string): void
  (event: 'logout'): void
  (event: 'toggle-collapse'): void
  (event: 'update:mobileOpen', value: boolean): void
}>()

const navigationGroups = computed(() => {
  const groups: Array<{ label: string; items: AppNavItem[] }> = []
  for (const item of props.items) {
    const label = item.group ?? ''
    const last = groups[groups.length - 1]
    if (last && last.label === label) {
      last.items.push(item)
    } else {
      groups.push({ label, items: [item] })
    }
  }
  return groups
})

const mobileDrawerOpen = computed({
  get: () => props.mobileOpen,
  set: value => emit('update:mobileOpen', value)
})

const navigate = (path: string) => {
  emit('navigate', path)
  emit('update:mobileOpen', false)
}

const handleAccountCommand = (command: string) => {
  if (command === 'logout') emit('logout')
}

/* Live instrument clock */
const clockTime = ref('')
const clockZone = ref('')
const updateClock = () => {
  const now = new Date()
  const pad = (value: number) => String(value).padStart(2, '0')
  clockTime.value = `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`
  const offset = -now.getTimezoneOffset() / 60
  clockZone.value = `UTC${offset >= 0 ? '+' : ''}${offset}`
}
updateClock()
const clockTimer = window.setInterval(updateClock, 1000)
onBeforeUnmount(() => window.clearInterval(clockTimer))
</script>

<style scoped>
.console-shell {
  display: flex;
  width: 100%;
  height: 100vh;
  min-height: 0;
  overflow: hidden;
  background: var(--crc-canvas);
}

/* -- Sidebar ---------------------------------------------------------------- */

.console-shell__sidebar {
  position: relative;
  display: flex;
  width: var(--crc-sidebar-expanded);
  min-height: 0;
  flex: 0 0 auto;
  flex-direction: column;
  border-right: 1px solid var(--crc-border);
  background: var(--crc-surface);
  transition: width var(--crc-duration) var(--crc-ease-expo),
    background-color var(--crc-duration) var(--crc-ease);
}

.console-shell--collapsed .console-shell__sidebar {
  width: var(--crc-sidebar-collapsed);
}

.console-shell__brand {
  display: flex;
  height: var(--crc-topbar-height);
  padding: 0 14px;
  align-items: center;
  gap: 11px;
  border-bottom: 1px solid var(--crc-border);
}

.console-shell__brand-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  line-height: 1.25;
  white-space: nowrap;
}

.console-shell__brand-copy strong {
  overflow: hidden;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 14px;
  font-weight: 600;
  letter-spacing: 0;
  text-overflow: ellipsis;
}

.console-shell__brand-copy span {
  margin-top: 2px;
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  font-weight: 500;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

/* -- Nav --------------------------------------------------------------------- */

.console-shell__menu {
  min-height: 0;
  flex: 1;
  padding: 10px 10px;
  overflow-x: hidden;
  overflow-y: auto;
  border-right: 0;
  background: transparent;
}

.console-shell__menu :deep(.el-menu-item) {
  position: relative;
  height: 38px;
  margin: 1px 0;
  padding: 0 10px !important;
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  font-size: 13px;
  font-weight: 500;
  letter-spacing: 0;
  transition: color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease),
    transform var(--crc-duration-fast) var(--crc-ease-out);
}

.console-shell__menu :deep(.el-menu-item:hover) {
  color: var(--crc-text-strong);
  background: var(--crc-surface-hover);
  transform: translateX(2px);
}

.console-shell__menu :deep(.el-menu-item.is-active) {
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
  font-weight: 600;
}

.console-shell__menu :deep(.el-menu-item.is-active)::before {
  content: '';
  position: absolute;
  top: 50%;
  left: 0;
  width: 3px;
  height: 18px;
  border-radius: 999px;
  background: var(--crc-accent);
  box-shadow: var(--crc-accent-glow);
  transform: translateY(-50%);
  animation: menu-indicator-in var(--crc-duration) var(--crc-ease-out) backwards;
}

@keyframes menu-indicator-in {
  from {
    opacity: 0;
    transform: translateY(-50%) scaleY(0.3);
  }
  to {
    opacity: 1;
    transform: translateY(-50%) scaleY(1);
  }
}

.console-shell__nav-icon {
  display: inline-flex;
  width: 20px;
  height: 20px;
  margin-right: 9px;
  flex: 0 0 20px;
  align-items: center;
  justify-content: center;
}

.console-shell--collapsed .console-shell__nav-icon {
  margin-right: 0;
}

.console-shell__group-label {
  display: flex;
  padding: 16px 10px 6px;
  align-items: baseline;
  gap: 7px;
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  font-weight: 500;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.console-shell__group-label em {
  color: var(--crc-accent);
  font-style: normal;
  font-weight: 600;
}

/* -- Sidebar foot -------------------------------------------------------------- */

.console-shell__sidebar-foot {
  border-top: 1px solid var(--crc-border);
}

.console-shell__status {
  display: flex;
  margin: 12px 14px 4px;
  padding: 9px 11px;
  align-items: center;
  gap: 10px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  background: var(--crc-canvas);
}

.console-shell__status-text {
  display: flex;
  flex-direction: column;
  font-family: var(--crc-font-mono);
  line-height: 1.35;
}

.console-shell__status-text em {
  color: var(--crc-text-subtle);
  font-size: 9px;
  font-style: normal;
  letter-spacing: 0.06em;
}

.console-shell__status-text strong {
  color: var(--crc-accent);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
}

.console-shell__collapse {
  display: flex;
  width: calc(100% - 20px);
  height: 40px;
  margin: 10px;
  padding: 0 12px;
  align-items: center;
  justify-content: center;
  gap: 9px;
  border: 1px solid transparent;
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  background: transparent;
  cursor: pointer;
  font-size: 12px;
  letter-spacing: 0;
  transition: color var(--crc-duration-fast) var(--crc-ease),
    border-color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease);
}

.console-shell__collapse:hover {
  border-color: var(--crc-border);
  color: var(--crc-text-strong);
  background: var(--crc-surface-hover);
}

/* -- Workspace ------------------------------------------------------------------ */

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
  padding: 0 22px;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  border-bottom: 1px solid var(--crc-border);
  background: var(--crc-topbar-bg);
  backdrop-filter: saturate(1.5) blur(14px);
  -webkit-backdrop-filter: saturate(1.5) blur(14px);
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
  gap: 12px;
}

.console-shell__title-stack {
  min-width: 0;
}

.console-shell__heading h1 {
  overflow: hidden;
  margin: 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 16px;
  font-weight: 600;
  letter-spacing: 0;
  line-height: 1.25;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.console-shell__context {
  display: none;
  margin: 0 0 2px;
  align-items: center;
  gap: 4px;
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.console-shell__context b {
  color: var(--crc-accent);
  font-weight: 600;
}

.console-shell__actions {
  flex: 0 0 auto;
  gap: 10px;
}

.console-shell__clock {
  display: flex;
  padding: 0 12px;
  align-items: baseline;
  gap: 8px;
  font-family: var(--crc-font-mono);
}

.console-shell__clock-time {
  color: var(--crc-text);
  font-size: 13px;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
  letter-spacing: 0;
}

.console-shell__clock-zone {
  color: var(--crc-text-subtle);
  font-size: 10px;
  letter-spacing: 0.06em;
}

.console-shell__account {
  min-width: 0;
  height: 38px;
  padding: 0 10px 0 6px;
  gap: 8px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text);
  background: var(--crc-surface);
  cursor: pointer;
  transition: border-color var(--crc-duration-fast) var(--crc-ease),
    background-color var(--crc-duration-fast) var(--crc-ease),
    box-shadow var(--crc-duration-fast) var(--crc-ease);
}

.console-shell__account:hover {
  border-color: var(--crc-border-strong);
  background: var(--crc-surface-hover);
}

.console-shell__account-avatar {
  display: grid;
  width: 27px;
  height: 27px;
  flex: 0 0 27px;
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
  letter-spacing: 0;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.console-shell__account-arrow {
  color: var(--crc-text-subtle);
}

.console-shell__content {
  min-width: 0;
  min-height: 0;
  flex: 1;
  overflow: auto;
  padding: 26px 28px 32px;
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
  display: flex;
  height: var(--crc-topbar-height);
  padding: 0 14px;
  align-items: center;
  gap: 11px;
  border-bottom: 1px solid var(--crc-border);
}

.console-shell__mobile-brand .console-shell__brand-copy {
  flex: 1;
}

.console-shell__mobile-nav {
  padding: 10px;
  border-right: 0;
}

.console-shell__mobile-nav :deep(.el-menu-item) {
  height: 40px;
  margin: 1px 0;
  border-radius: var(--crc-radius-sm);
  color: var(--crc-text-muted);
  letter-spacing: 0;
}

.console-shell__mobile-nav :deep(.el-menu-item.is-active) {
  color: var(--crc-accent);
  background: var(--crc-accent-soft);
}

:global(.console-shell__drawer .el-drawer__body) {
  display: flex;
  padding: 0;
  flex-direction: column;
  background: var(--crc-surface);
}

@media (max-width: 1023px) {
  .console-shell__clock {
    display: none;
  }
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
    display: flex;
  }

  .console-shell__account-label,
  .console-shell__account-arrow {
    display: none;
  }

  .console-shell__account {
    width: 38px;
    padding: 4px;
  }

  .console-shell__content {
    padding: 18px 14px 24px;
  }
}
</style>
