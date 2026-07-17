# Capability Profiles Pagination Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paginate the Capability strategy Profiles table locally with a default of 10 rows and selectable 10/20/50 page sizes.

**Architecture:** Keep the existing API and stable profile order unchanged. `TroubleshootingCenter.vue` owns page state, slices the complete `dialectProfiles` ref through a computed value, and normalizes the page after refresh; the two selected-profile detail tables remain unpaginated.

**Tech Stack:** Vue 3 Composition API, Element Plus pagination, scoped CSS, Vitest structural contracts

---

### Task 1: Paginate the main Profiles table

**Files:**
- Modify: `frontend/tests/views/admin-ui.spec.ts`
- Modify: `frontend/src/components/TroubleshootingCenter.vue`

- [ ] **Step 1: Write the failing pagination contract**

Extend the troubleshooting test in `frontend/tests/views/admin-ui.spec.ts` with:

```ts
    expect(center).toContain(':data="paginatedDialectProfiles"')
    expect(center).toContain('v-model:current-page="profilePage"')
    expect(center).toContain('v-model:page-size="profilePageSize"')
    expect(center).toContain(':page-sizes="profilePageSizes"')
    expect(center).toContain(':total="dialectProfiles.length"')
    expect(center).toContain('const profilePage = ref(1)')
    expect(center).toContain('const profilePageSize = ref(10)')
    expect(center).toContain('const profilePageSizes = [10, 20, 50]')
    expect(center).toContain('const paginatedDialectProfiles = computed')
    expect(center).toContain('normalizeProfilePage()')
    expect(center).toContain('class="capability-pagination"')
```

Add these assertions so the detail tables remain unpaginated:

```ts
    expect(center).toContain(':data="selectedResolvedCapabilityRows"')
    expect(center).toContain(':data="selectedResolved.conflicts"')
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
```

Expected: FAIL because the Profiles table still binds `dialectProfiles` directly and no page state or pagination control exists.

- [ ] **Step 3: Add the pagination control below the main Profiles table**

Change the main table binding in `frontend/src/components/TroubleshootingCenter.vue` to:

```vue
<el-table :data="paginatedDialectProfiles" size="small" empty-text="暂无 profile">
```

Immediately after the main `crc-table-shell` closing tag and before `resolvedError`, add:

```vue
<div class="capability-pagination">
  <el-pagination
    v-model:current-page="profilePage"
    v-model:page-size="profilePageSize"
    :page-sizes="profilePageSizes"
    :total="dialectProfiles.length"
    layout="total, sizes, prev, pager, next"
    @size-change="handleProfilePageSizeChange"
  />
</div>
```

- [ ] **Step 4: Add local page state, slicing, and normalization**

After the existing `dialectProfiles` ref, add:

```ts
const profilePage = ref(1)
const profilePageSize = ref(10)
const profilePageSizes = [10, 20, 50]
```

After the existing `currentProfile` computed, add:

```ts
const paginatedDialectProfiles = computed(() => {
  const start = (profilePage.value - 1) * profilePageSize.value
  return dialectProfiles.value.slice(start, start + profilePageSize.value)
})
```

Before `refreshDialectProfiles`, add:

```ts
const normalizeProfilePage = () => {
  const maxPage = Math.max(1, Math.ceil(dialectProfiles.value.length / profilePageSize.value))
  profilePage.value = Math.min(profilePage.value, maxPage)
}

const handleProfilePageSizeChange = () => {
  profilePage.value = 1
}
```

Update `refreshDialectProfiles` so the successful assignment is followed by normalization:

```ts
dialectProfiles.value = await props.loadDialectProfiles()
normalizeProfilePage()
```

- [ ] **Step 5: Add responsive pagination styles**

Add before the existing container query:

```css
.capability-pagination {
  display: flex;
  max-width: 100%;
  overflow-x: auto;
  justify-content: flex-end;
  margin-top: 12px;
  padding-bottom: 4px;
}
```

Inside the existing `@media (max-width: 767px)` block, add:

```css
.capability-pagination {
  justify-content: flex-start;
}
```

- [ ] **Step 6: Run the focused test and verify GREEN**

Run:

```bash
rtk npm test -- tests/views/admin-ui.spec.ts
```

Expected: the file passes with no failures.

- [ ] **Step 7: Run frontend regression tests and build**

Run:

```bash
rtk npm test
rtk npm run build
rtk git diff --check
```

Expected: all Vitest files pass, `vue-tsc` and Vite build succeed, and the diff has no whitespace errors.

- [ ] **Step 8: Verify pagination in Chrome**

Use the local worktree Vite server and an authenticated admin session. On `/admin/troubleshooting`, verify:

- Profiles total exceeds 10 in the current data set and the table initially renders exactly 10 rows;
- moving to page 2 changes the rendered Profiles rows;
- choosing 20 renders at most 20 rows and returns to page 1;
- Capability detail tables remain complete after selecting a profile;
- at 390x844 the pagination control scrolls inside itself and `document.documentElement.scrollWidth <= document.documentElement.clientWidth + 2`.

- [ ] **Step 9: Commit the pagination change**

```bash
rtk git add frontend/tests/views/admin-ui.spec.ts frontend/src/components/TroubleshootingCenter.vue
rtk git commit -m "feat(ui): paginate capability profiles"
```
