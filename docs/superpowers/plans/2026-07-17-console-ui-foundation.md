# Console UI Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the shared theme, responsive application shell, and authentication surfaces used by every admin and portal page.

**Architecture:** A dependency-free theme composable owns `light`, `dark`, and `auto` state and applies resolved state before Vue mounts. One presentation-only `AppShell` renders desktop navigation, the mobile drawer, top-bar actions, and account context while `App.vue` and `Portal.vue` retain their current business responsibilities. Global CSS tokens and two small shared controls replace duplicated shell and login styling.

**Tech Stack:** Vue 3 Composition API, TypeScript, Pinia (existing auth only), Vue Router, Element Plus, CSS custom properties, Vitest source and utility tests.

---

## File Map

**Create:**

- `frontend/src/composables/useTheme.ts` - theme mode, system preference, persistence, and document synchronization.
- `frontend/src/styles/tokens.css` - semantic light/dark variables and Element Plus mappings.
- `frontend/src/styles/base.css` - reset, typography, focus, page utilities, and shared component refinements.
- `frontend/src/components/ThemeSwitcher.vue` - compact theme mode menu.
- `frontend/src/components/AppShell.vue` - shared responsive admin/portal chrome.
- `frontend/src/components/AuthShell.vue` - shared neutral authentication panel.
- `frontend/tests/utils/theme.spec.ts` - pure theme behavior tests.
- `frontend/tests/views/ui-foundation.spec.ts` - source contracts for shell, theme entry, and auth composition.

**Modify:**

- `frontend/src/main.ts` - apply theme before mount and import global styles.
- `frontend/src/router/index.ts` - add route titles and document-title synchronization.
- `frontend/src/App.vue` - compose admin navigation through `AppShell`.
- `frontend/src/views/portal/Portal.vue` - compose portal navigation through `AppShell` without moving portal logic.
- `frontend/src/views/admin/Login.vue` - use `AuthShell` while retaining admin login behavior.
- `frontend/src/views/portal/PortalLogin.vue` - use `AuthShell` while retaining portal login behavior.
- `frontend/package.json` - expose the existing Vitest command as `npm test`.
- `frontend/tests/router/index.spec.ts` - assert route titles and existing route boundaries.

### Task 1: Theme Resolution And Persistence

**Files:**

- Create: `frontend/tests/utils/theme.spec.ts`
- Create: `frontend/src/composables/useTheme.ts`

- [ ] **Step 1: Write the failing pure behavior tests**

```ts
import { describe, expect, it } from 'vitest'
import {
  applyResolvedTheme,
  normalizeThemeMode,
  readStoredThemeMode,
  resolveThemeMode
} from '../../src/composables/useTheme'

describe('theme state', () => {
  it('accepts supported persisted modes and rejects other values', () => {
    expect(normalizeThemeMode('light')).toBe('light')
    expect(normalizeThemeMode('dark')).toBe('dark')
    expect(normalizeThemeMode('auto')).toBe('auto')
    expect(normalizeThemeMode('midnight')).toBe('auto')
    expect(readStoredThemeMode({ getItem: () => 'dark' })).toBe('dark')
    expect(readStoredThemeMode({ getItem: () => { throw new Error('blocked') } })).toBe('auto')
  })

  it('resolves auto from the current system preference', () => {
    expect(resolveThemeMode('light', true)).toBe('light')
    expect(resolveThemeMode('dark', false)).toBe('dark')
    expect(resolveThemeMode('auto', true)).toBe('dark')
    expect(resolveThemeMode('auto', false)).toBe('light')
  })

  it('updates root class, data attribute, and color scheme together', () => {
    const classes = new Set<string>()
    const root = {
      classList: { toggle: (name: string, enabled: boolean) => enabled ? classes.add(name) : classes.delete(name) },
      dataset: {} as Record<string, string>,
      style: {} as Record<string, string>
    }

    applyResolvedTheme(root, 'dark')
    expect(classes.has('dark')).toBe(true)
    expect(root.dataset.theme).toBe('dark')
    expect(root.style.colorScheme).toBe('dark')
  })
})
```

- [ ] **Step 2: Run the test and verify the missing module failure**

Run from `frontend/`: `rtk npx vitest run tests/utils/theme.spec.ts`

Expected: FAIL because `src/composables/useTheme.ts` does not exist.

- [ ] **Step 3: Implement the theme composable and its pure helpers**

Use these public types and functions exactly so later tasks consume one stable interface:

```ts
import { readonly, ref } from 'vue'

export type ThemeMode = 'light' | 'dark' | 'auto'
export type ResolvedTheme = Exclude<ThemeMode, 'auto'>

export const THEME_STORAGE_KEY = 'crc-theme-mode'

type ThemeRoot = {
  classList: { toggle: (name: string, enabled: boolean) => unknown }
  dataset: { theme?: string }
  style: { colorScheme?: string }
}

const mode = ref<ThemeMode>('auto')
const resolvedTheme = ref<ResolvedTheme>('light')
let mediaQuery: MediaQueryList | null = null
let initialized = false

export const normalizeThemeMode = (value: unknown): ThemeMode =>
  value === 'light' || value === 'dark' || value === 'auto' ? value : 'auto'

export const readStoredThemeMode = (storage: Pick<Storage, 'getItem'>): ThemeMode => {
  try { return normalizeThemeMode(storage.getItem(THEME_STORAGE_KEY)) } catch { return 'auto' }
}

export const resolveThemeMode = (value: ThemeMode, prefersDark: boolean): ResolvedTheme =>
  value === 'auto' ? (prefersDark ? 'dark' : 'light') : value

export const applyResolvedTheme = (root: ThemeRoot, value: ResolvedTheme) => {
  root.classList.toggle('dark', value === 'dark')
  root.dataset.theme = value
  root.style.colorScheme = value
}

const syncTheme = () => {
  const resolved = resolveThemeMode(mode.value, mediaQuery?.matches ?? false)
  resolvedTheme.value = resolved
  if (typeof document !== 'undefined') applyResolvedTheme(document.documentElement, resolved)
}

const handleSystemThemeChange = () => {
  if (mode.value === 'auto') syncTheme()
}

export const initializeTheme = () => {
  if (typeof window === 'undefined' || initialized) return
  initialized = true
  mode.value = readStoredThemeMode(window.localStorage)
  mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
  mediaQuery.addEventListener('change', handleSystemThemeChange)
  syncTheme()
}

export const setThemeMode = (value: ThemeMode) => {
  mode.value = value
  try { window.localStorage.setItem(THEME_STORAGE_KEY, value) } catch { /* unavailable storage */ }
  syncTheme()
}

export const useTheme = () => ({
  mode: readonly(mode),
  resolvedTheme: readonly(resolvedTheme),
  setThemeMode
})
```

- [ ] **Step 4: Run the focused test**

Run from `frontend/`: `rtk npx vitest run tests/utils/theme.spec.ts`

Expected: PASS, 3 tests.

- [ ] **Step 5: Commit the theme behavior**

```bash
rtk git add frontend/src/composables/useTheme.ts frontend/tests/utils/theme.spec.ts
rtk git commit -m "feat(ui): add persistent theme state" -m "Confidence: high
Scope-risk: narrow"
```

### Task 2: Semantic Tokens And Pre-Mount Theme Initialization

**Files:**

- Create: `frontend/src/styles/tokens.css`
- Create: `frontend/src/styles/base.css`
- Modify: `frontend/src/main.ts`
- Modify: `frontend/package.json`
- Modify: `frontend/tests/views/ui-foundation.spec.ts`

- [ ] **Step 1: Add the initial source contract test**

```ts
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const readSource = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8')

describe('ui foundation composition', () => {
  it('initializes theme before mounting and imports global token layers', () => {
    const main = readSource('../../src/main.ts')
    expect(main).toContain("element-plus/theme-chalk/dark/css-vars.css")
    expect(main).toContain("./styles/tokens.css")
    expect(main).toContain("./styles/base.css")
    expect(main.indexOf('initializeTheme()')).toBeLessThan(main.indexOf("createApp(App)"))
  })
})
```

- [ ] **Step 2: Run the test and verify it fails on missing imports**

Run from `frontend/`: `rtk npx vitest run tests/views/ui-foundation.spec.ts`

Expected: FAIL because `main.ts` does not initialize or import the design system.

- [ ] **Step 3: Add the semantic token layer**

`tokens.css` must define the approved tokens and map Element Plus variables:

```css
:root {
  --crc-canvas: #f6f7f8;
  --crc-surface: #ffffff;
  --crc-surface-elevated: #ffffff;
  --crc-surface-muted: #f0f3f2;
  --crc-text-strong: #17201d;
  --crc-text: #34413d;
  --crc-text-muted: #66716d;
  --crc-border: #dfe5e2;
  --crc-accent: #0f8f76;
  --crc-accent-hover: #0b7662;
  --crc-accent-soft: #eaf6f2;
  --crc-success: #15803d;
  --crc-warning: #b45309;
  --crc-danger: #c2413b;
  --crc-info: #2563a6;
  --crc-radius-sm: 6px;
  --crc-radius: 8px;
  --crc-shadow-elevated: 0 14px 36px rgb(23 32 29 / 14%);
  --el-color-primary: var(--crc-accent);
  --el-color-primary-light-9: var(--crc-accent-soft);
  --el-bg-color: var(--crc-surface);
  --el-bg-color-page: var(--crc-canvas);
  --el-text-color-primary: var(--crc-text-strong);
  --el-text-color-regular: var(--crc-text);
  --el-text-color-secondary: var(--crc-text-muted);
  --el-border-color: var(--crc-border);
  --el-border-radius-base: var(--crc-radius-sm);
}

html.dark {
  --crc-canvas: #111514;
  --crc-surface: #181d1b;
  --crc-surface-elevated: #202624;
  --crc-surface-muted: #252c29;
  --crc-text-strong: #f1f5f3;
  --crc-text: #d2dad6;
  --crc-text-muted: #98a59f;
  --crc-border: #343d39;
  --crc-accent: #39b99c;
  --crc-accent-hover: #53c9ae;
  --crc-accent-soft: #173d34;
  --crc-success: #4ade80;
  --crc-warning: #f6ad55;
  --crc-danger: #fb7185;
  --crc-info: #60a5d8;
  --crc-shadow-elevated: 0 16px 40px rgb(0 0 0 / 36%);
}
```

`base.css` must include the reset and stable shared page primitives:

```css
* { box-sizing: border-box; }
html, body, #app { width: 100%; min-width: 320px; min-height: 100%; margin: 0; }
body {
  color: var(--crc-text);
  background: var(--crc-canvas);
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont,
    "Segoe UI", "Microsoft YaHei", sans-serif;
  letter-spacing: 0;
  text-rendering: optimizeLegibility;
}
button, input, textarea, select { font: inherit; }
:focus-visible { outline: 2px solid var(--crc-accent); outline-offset: 2px; }
.crc-page { width: 100%; max-width: 1600px; margin: 0 auto; }
.crc-page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 20px; }
.crc-page-title { margin: 0; color: var(--crc-text-strong); font-size: 22px; line-height: 1.3; }
.crc-page-description { margin: 6px 0 0; color: var(--crc-text-muted); font-size: 13px; line-height: 1.6; }
.crc-toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-bottom: 16px; }
.crc-surface { border: 1px solid var(--crc-border); border-radius: var(--crc-radius); background: var(--crc-surface); }
.crc-table-shell { width: 100%; overflow-x: auto; border: 1px solid var(--crc-border); border-radius: var(--crc-radius); background: var(--crc-surface); }
@media (max-width: 767px) {
  .crc-page-header { flex-direction: column; }
  .crc-toolbar { align-items: stretch; }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { scroll-behavior: auto !important; transition-duration: 0.01ms !important; animation-duration: 0.01ms !important; }
}
```

- [ ] **Step 4: Initialize theme and styles before app creation**

Add the dark CSS variables, token layers, and `initializeTheme()` above
`const app = createApp(App)`. Add `"test": "vitest run"` to `package.json`
without changing dependency versions.

- [ ] **Step 5: Run focused tests and production build**

Run from `frontend/`: `rtk npm test -- tests/utils/theme.spec.ts tests/views/ui-foundation.spec.ts && rtk npm run build`

Expected: both test files pass and Vite completes without type errors.

- [ ] **Step 6: Commit global theme entry**

```bash
rtk git add frontend/src/main.ts frontend/src/styles frontend/package.json frontend/tests/views/ui-foundation.spec.ts
rtk git commit -m "feat(ui): establish console design tokens" -m "Constraint: Keep Element Plus and existing dependencies
Confidence: high
Scope-risk: moderate"
```

### Task 3: Route Titles

**Files:**

- Modify: `frontend/src/router/index.ts`
- Modify: `frontend/tests/router/index.spec.ts`

- [ ] **Step 1: Add failing title assertions for every route group**

```ts
it('provides page titles for shell and document context', () => {
  const titles = new Map(router.getRoutes().map(route => [route.name, route.meta.title]))
  expect(titles.get('PortalLogin')).toBe('门户登录')
  expect(titles.get('PortalOverview')).toBe('概览')
  expect(titles.get('PortalPlayground')).toBe('模型操练场')
  expect(titles.get('AdminLogin')).toBe('管理员登录')
  expect(titles.get('AdminDashboard')).toBe('控制台总览')
  expect(titles.get('AdminTroubleshooting')).toBe('排障中心')
})
```

- [ ] **Step 2: Run the test and verify metadata is undefined**

Run from `frontend/`: `rtk npx vitest run tests/router/index.spec.ts`

Expected: FAIL on the first title assertion.

- [ ] **Step 3: Add `meta.title` without changing paths, names, redirects, or guards**

Add the approved Chinese title to every login and content route. Add:

```ts
router.afterEach(to => {
  if (typeof document === 'undefined') return
  const title = typeof to.meta.title === 'string' ? to.meta.title : ''
  document.title = title ? `${title} - CRC Console` : 'CRC Console'
})
```

- [ ] **Step 4: Run router tests**

Run from `frontend/`: `rtk npx vitest run tests/router/index.spec.ts`

Expected: all router tests pass, including the portal troubleshooting absence.

- [ ] **Step 5: Commit route context**

```bash
rtk git add frontend/src/router/index.ts frontend/tests/router/index.spec.ts
rtk git commit -m "feat(ui): add console page titles" -m "Constraint: Preserve route and guard behavior
Confidence: high
Scope-risk: narrow"
```

### Task 4: Theme Switcher And Shared Shell

**Files:**

- Create: `frontend/src/components/ThemeSwitcher.vue`
- Create: `frontend/src/components/AppShell.vue`
- Modify: `frontend/tests/views/ui-foundation.spec.ts`

- [ ] **Step 1: Add failing shell source contracts**

```ts
it('provides responsive shared shell and compact theme control', () => {
  const shell = readSource('../../src/components/AppShell.vue')
  const switcher = readSource('../../src/components/ThemeSwitcher.vue')
  expect(shell).toContain('<el-drawer')
  expect(shell).toContain(':collapse="collapsed"')
  expect(shell).toContain('width="216px"')
  expect(shell).toContain("emit('navigate', path)")
  expect(shell).toContain("emit('update:mobileOpen', false)")
  expect(switcher).toContain("setThemeMode('auto')")
  expect(switcher).toContain('跟随系统')
})
```

- [ ] **Step 2: Run the source test and verify missing components**

Run from `frontend/`: `rtk npx vitest run tests/views/ui-foundation.spec.ts`

Expected: FAIL because both components are absent.

- [ ] **Step 3: Implement `ThemeSwitcher.vue`**

Use `el-dropdown` with `Sunny`, `Moon`, and `Monitor` icons. The trigger is one
36x36 icon button with `aria-label="切换主题"`; the menu contains `浅色`,
`深色`, and `跟随系统`, shows the active mode, and calls the stable theme API.
Do not add a text pill to the top bar.

- [ ] **Step 4: Implement the presentation-only `AppShell.vue` interface**

Use this interface exactly:

```ts
import type { Component } from 'vue'

export interface AppNavItem {
  path: string
  label: string
  icon: Component
  group?: string
}

const props = defineProps<{
  items: AppNavItem[]
  activePath: string
  pageTitle: string
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

const navigate = (path: string) => {
  emit('navigate', path)
  emit('update:mobileOpen', false)
}
```

The template renders the same navigation list in a desktop aside and Element
Plus drawer. Use `el-menu :collapse="collapsed"`, icon tooltips in collapsed
mode, a bottom collapse icon button, a mobile menu icon, page title,
`ThemeSwitcher`, and an account dropdown with a logout command. Slots are not
used for auth or route logic.

- [ ] **Step 5: Add token-based shell styles**

Use fixed CSS custom properties `--crc-sidebar-expanded: 216px`,
`--crc-sidebar-collapsed: 64px`, and `--crc-topbar-height: 56px`. Hide the
desktop aside below 768px, show the mobile menu button there, constrain drawer
width with `min(86vw, 320px)`, and keep main scroll on `.app-shell__content`.
Use no gradients or hard-coded page colors.

- [ ] **Step 6: Run source tests and build**

Run from `frontend/`: `rtk npm test -- tests/views/ui-foundation.spec.ts && rtk npm run build`

Expected: source contracts pass and Vue type checking accepts component props.

- [ ] **Step 7: Commit shared chrome**

```bash
rtk git add frontend/src/components/AppShell.vue frontend/src/components/ThemeSwitcher.vue frontend/tests/views/ui-foundation.spec.ts
rtk git commit -m "feat(ui): add responsive shared console shell" -m "Constraint: Keep shell presentation-only
Confidence: high
Scope-risk: moderate"
```

### Task 5: Migrate The Admin Shell

**Files:**

- Modify: `frontend/src/App.vue`
- Modify: `frontend/tests/views/ui-foundation.spec.ts`

- [ ] **Step 1: Add a failing admin composition contract**

```ts
it('composes admin navigation through the shared shell', () => {
  const app = readSource('../../src/App.vue')
  expect(app).toContain('<AppShell')
  expect(app).toContain('adminNavItems')
  expect(app).toContain('admin-sidebar-collapsed')
  expect(app).toContain('authStore.clearToken()')
  expect(app).toContain("router.push('/admin/login')")
  expect(app).not.toContain('linear-gradient')
})
```

- [ ] **Step 2: Run the source test and verify the old shell fails it**

Run from `frontend/`: `rtk npx vitest run tests/views/ui-foundation.spec.ts`

Expected: FAIL because `App.vue` contains its own fixed aside.

- [ ] **Step 3: Compose the admin shell without changing route selection**

Replace the admin `el-container` template with `AppShell`. Define
`adminNavItems` for dashboard, model probe, upstreams, downstreams, logs,
troubleshooting, and announcement using existing Element Plus icons. Derive
the title from `route.meta.title`, retain `isAdminShell`, navigate only when the
path changes, and implement:

```ts
const collapsed = ref(safeReadBoolean('admin-sidebar-collapsed'))
const mobileOpen = ref(false)

const toggleCollapsed = () => {
  collapsed.value = !collapsed.value
  safeWriteBoolean('admin-sidebar-collapsed', collapsed.value)
}

const handleLogout = () => {
  authStore.clearToken()
  router.push('/admin/login')
}
```

Keep non-admin and `/admin/login` rendering as a direct `router-view`.

- [ ] **Step 4: Delete the duplicated hard-coded sidebar/topbar CSS**

`App.vue` may retain only root sizing needed for router composition. All chrome
styles come from `AppShell` and tokens.

- [ ] **Step 5: Run foundation and router tests, then build**

Run from `frontend/`: `rtk npm test -- tests/views/ui-foundation.spec.ts tests/router/index.spec.ts && rtk npm run build`

Expected: PASS with the same admin route set.

- [ ] **Step 6: Commit admin composition**

```bash
rtk git add frontend/src/App.vue frontend/tests/views/ui-foundation.spec.ts
rtk git commit -m "feat(ui): migrate admin to shared shell" -m "Constraint: Preserve admin auth and navigation semantics
Confidence: high
Scope-risk: moderate"
```

### Task 6: Migrate The Portal Shell

**Files:**

- Modify: `frontend/src/views/portal/Portal.vue`
- Modify: `frontend/tests/views/ui-foundation.spec.ts`
- Test: `frontend/tests/utils/announcement.spec.ts`

- [ ] **Step 1: Add a failing portal responsibility contract**

```ts
it('uses shared chrome while retaining portal-owned behavior', () => {
  const portal = readSource('../../src/views/portal/Portal.vue')
  expect(portal).toContain('<AppShell')
  expect(portal).toContain('portalNavItems')
  expect(portal).toContain('portal-sidebar-collapsed')
  expect(portal).toContain('loadAnnouncement')
  expect(portal).toContain("provide('portalToken'")
  expect(portal).toContain("localStorage.removeItem('portal_token')")
  expect(portal).not.toContain('linear-gradient')
})
```

- [ ] **Step 2: Run the source and announcement tests**

Run from `frontend/`: `rtk npx vitest run tests/views/ui-foundation.spec.ts tests/utils/announcement.spec.ts`

Expected: the new source contract fails while announcement tests remain green.

- [ ] **Step 3: Compose portal navigation through `AppShell`**

Define portal nav items for overview, model probe, usage history, integration,
playground, and key management. Preserve the current `titleMap`/active-path
behavior or replace it only with equivalent route metadata. Keep employee ID,
announcement loading/acknowledgment, token injection, and logout logic inside
`Portal.vue`. Add portal-specific persisted collapse state and transient mobile
drawer state using the same safe storage pattern as admin.

- [ ] **Step 4: Keep the announcement dialog outside the shell content**

Retain its title, non-dismissible behavior, semantic tag, content, and
acknowledge action. Convert its colors to semantic variables and constrain width
with `width="min(560px, calc(100vw - 32px))"` or an equivalent Element Plus
width binding that type-checks.

- [ ] **Step 5: Run focused tests and build**

Run from `frontend/`: `rtk npm test -- tests/views/ui-foundation.spec.ts tests/utils/announcement.spec.ts tests/router/index.spec.ts && rtk npm run build`

Expected: all pass; portal troubleshooting remains absent.

- [ ] **Step 6: Commit portal composition**

```bash
rtk git add frontend/src/views/portal/Portal.vue frontend/tests/views/ui-foundation.spec.ts
rtk git commit -m "feat(ui): migrate portal to shared shell" -m "Constraint: Keep announcements and token injection portal-owned
Confidence: high
Scope-risk: moderate"
```

### Task 7: Shared Authentication Surface

**Files:**

- Create: `frontend/src/components/AuthShell.vue`
- Modify: `frontend/src/views/admin/Login.vue`
- Modify: `frontend/src/views/portal/PortalLogin.vue`
- Modify: `frontend/tests/views/ui-foundation.spec.ts`
- Test: `frontend/tests/api/admin.spec.ts`
- Test: `frontend/tests/api/portal.spec.ts`

- [ ] **Step 1: Add failing auth composition contracts**

```ts
it('uses one neutral authentication surface for both account types', () => {
  const authShell = readSource('../../src/components/AuthShell.vue')
  const adminLogin = readSource('../../src/views/admin/Login.vue')
  const portalLogin = readSource('../../src/views/portal/PortalLogin.vue')
  expect(authShell).toContain('<ThemeSwitcher')
  expect(authShell).toContain('<slot />')
  expect(adminLogin).toContain('<AuthShell')
  expect(portalLogin).toContain('<AuthShell')
  expect(adminLogin).not.toContain('linear-gradient')
  expect(portalLogin).not.toContain('linear-gradient')
})
```

- [ ] **Step 2: Run the source test and confirm missing `AuthShell`**

Run from `frontend/`: `rtk npx vitest run tests/views/ui-foundation.spec.ts`

Expected: FAIL because the shared auth surface is absent.

- [ ] **Step 3: Implement `AuthShell.vue`**

Props are `title` and `subtitle`; expose an optional `footer` slot. Render a neutral full-page
canvas, a 40-pixel CRC mark, product label `Chat Responses Codex`, title,
subtitle, default form slot, optional footer slot, and `ThemeSwitcher` in a
top-right utility position. Use one bordered panel with `max-width: 420px`,
responsive 20-pixel outer gutters, no illustration, no gradient, and no nested
card.

- [ ] **Step 4: Migrate the admin login template only**

Wrap the existing form in `AuthShell title="管理员登录" subtitle="使用管理账号进入控制台"`.
Keep the same model, rules, API response validation, token store, messages,
Enter behavior, and `/admin` navigation. Use icon-prefixed large inputs and one
stable full-width submit button.

- [ ] **Step 5: Migrate the portal login template only**

Wrap the existing form in `AuthShell title="自助门户" subtitle="使用工号和密钥登录"`.
Keep portal token/employee storage keys, API payload, messages, Enter behavior,
and `/portal` navigation. Put the existing first-use support sentence in the
footer slot without an extra card or divider.

- [ ] **Step 6: Run auth/API tests and production build**

Run from `frontend/`: `rtk npm test -- tests/views/ui-foundation.spec.ts tests/api/admin.spec.ts tests/api/portal.spec.ts && rtk npm run build`

Expected: all focused tests pass and both SFCs type-check.

- [ ] **Step 7: Commit authentication refresh**

```bash
rtk git add frontend/src/components/AuthShell.vue frontend/src/views/admin/Login.vue frontend/src/views/portal/PortalLogin.vue frontend/tests/views/ui-foundation.spec.ts
rtk git commit -m "feat(ui): unify console login surfaces" -m "Constraint: Preserve both authentication flows
Rejected: Split marketing login | inappropriate for an operational console
Confidence: high
Scope-risk: moderate"
```

### Task 8: Foundation Verification

**Files:**

- Verify only; do not add generated `frontend/dist` output to git.

- [ ] **Step 1: Run all frontend tests**

Run from `frontend/`: `rtk npm test`

Expected: all test files pass with zero failures.

- [ ] **Step 2: Run type checking and production build**

Run from `frontend/`: `rtk npm run build`

Expected: `vue-tsc` and Vite finish successfully.

- [ ] **Step 3: Audit foundation constraints**

Run:

```bash
rtk rg -n 'linear-gradient|radial-gradient|backdrop-filter|#[0-9a-fA-F]{3,8}' \
  frontend/src/App.vue \
  frontend/src/components/AppShell.vue \
  frontend/src/components/AuthShell.vue \
  frontend/src/components/ThemeSwitcher.vue \
  frontend/src/views/admin/Login.vue \
  frontend/src/views/portal/Portal.vue \
  frontend/src/views/portal/PortalLogin.vue
```

Expected: no gradients or glass effects; any remaining hex value exists only in
`styles/tokens.css`, not component styles.

- [ ] **Step 4: Confirm the worktree contains no generated or unrelated changes**

Run: `rtk git status --short && rtk git diff --check`

Expected: only intentional source/test changes from the completed tasks and no
whitespace errors.
