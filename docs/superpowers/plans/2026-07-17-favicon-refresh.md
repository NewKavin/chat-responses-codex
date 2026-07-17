# Fresh Favicon Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the outdated gradient `CRC` favicon with a crisp local teal `C` mark that matches the current console brand.

**Architecture:** Keep the existing `/favicon.svg` entry contract and replace only the SVG geometry. Add a source contract in the existing UI foundation suite so the icon remains local, font-free, gradient-free, and aligned with the console accent.

**Tech Stack:** SVG, HTML, Vitest, Vite

---

### Task 1: Local geometric `C` favicon

**Files:**
- Modify: `frontend/tests/views/ui-foundation.spec.ts`
- Modify: `frontend/public/favicon.svg`

- [ ] **Step 1: Write the failing favicon contract**

Add this test to `frontend/tests/views/ui-foundation.spec.ts`:

```ts
  it('uses a crisp local accent favicon without font or network dependencies', () => {
    const index = readSource('../../index.html')
    const favicon = readSource('../../public/favicon.svg')

    expect(index).toContain('<link rel="icon" type="image/svg+xml" href="/favicon.svg" />')
    expect(favicon).toContain('viewBox="0 0 64 64"')
    expect(favicon).toContain('fill="#0f8f76"')
    expect(favicon).toContain('<path')
    expect(favicon).not.toMatch(/linearGradient|<text|font-family|<script|(?:href|src)="https?:/)
  })
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
rtk npm test -- tests/views/ui-foundation.spec.ts
```

Expected: FAIL because the current favicon contains a `linearGradient`, `<text>`, and font dependency instead of a geometric path.

- [ ] **Step 3: Replace the SVG with the approved local mark**

Replace `frontend/public/favicon.svg` with:

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" role="img" aria-label="Chat Responses">
  <rect width="64" height="64" rx="12" fill="#0f8f76" />
  <path
    fill="#ffffff"
    d="M47 22.5C43.4 17.3 38.2 14 31.5 14C21.8 14 14 21.8 14 32s7.8 18 17.5 18c6.7 0 11.9-3.3 15.5-8.5l-6.7-4.2c-1.9 2.8-4.8 4.7-8.8 4.7-5.5 0-9.5-4.2-9.5-10s4-10 9.5-10c4 0 6.9 1.9 8.8 4.7z"
  />
</svg>
```

- [ ] **Step 4: Run the focused test and verify GREEN**

Run:

```bash
rtk npm test -- tests/views/ui-foundation.spec.ts
```

Expected: the file passes with no failures.

- [ ] **Step 5: Verify SVG delivery and visual decoding**

Run the development server from `frontend/`, open `/favicon.svg`, and verify the browser reports an SVG document with a teal rounded square and a centered white `C`. Also render it inside a 16x16 `<img>` through Chrome DevTools Protocol and inspect the screenshot for recognizable open-right geometry.

- [ ] **Step 6: Commit the favicon change**

```bash
rtk git add frontend/tests/views/ui-foundation.spec.ts frontend/public/favicon.svg
rtk git commit -m "feat(ui): refresh the browser tab icon"
```
