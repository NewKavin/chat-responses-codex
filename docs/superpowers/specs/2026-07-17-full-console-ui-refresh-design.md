# Full Console UI Refresh Design

Date: 2026-07-17
Status: Approved
Branch: `ui`

## Context

The Vue 3 frontend is functionally mature but does not yet present one coherent
product. Admin and portal shells duplicate navigation and layout styles, the two
login screens use an outdated purple gradient, most colors are hard-coded in
scoped styles, and responsive behavior usually stops at making a fixed sidebar
narrower. Dense operational pages also mix nested cards, inline filters, wide
tables, charts, drawers, and dialogs without shared visual rules.

This refresh applies to every existing frontend page and shared user-facing
component. It takes the current New API console as its primary reference and
the established relay-console direction in this repository as a constraint:
compact navigation, neutral surfaces, dense but readable operational content,
one restrained teal accent, clear status colors, and responsive layouts.

The 2026-07-16 live-model UI direction remains authoritative where it already
defines the shell and visual tokens. This design expands that direction to all
pages, adds theme support and interaction details, and does not alter the live
model, compatibility, troubleshooting, or gateway behavior specified elsewhere.

## Goals

- Give the admin console and employee portal one recognizable visual system.
- Refresh both login screens, both application shells, every routed view, and
  all shared operational components.
- Support light, dark, and system theme modes with persisted selection and no
  theme flash during initial render.
- Add a collapsible desktop sidebar, mobile navigation drawer, compact top bar,
  theme control, and account actions without changing route or auth semantics.
- Preserve the information density needed for gateway administration,
  observability, model qualification, compatibility checks, and troubleshooting.
- Make every page usable at desktop and mobile widths without incoherent
  overlap, clipped controls, or navigation consuming most of the viewport.
- Keep the implementation inside Vue, Element Plus, and existing local
  dependencies.

## Non-Goals

- No backend endpoint, payload, persistence, authentication, authorization, or
  gateway behavior changes.
- No new marketing or landing page.
- No route additions for currently unregistered views.
- No redesign of model qualification, compatibility, logging, quota, or
  playground business rules.
- No migration from Element Plus and no port of New API's React, Semi UI, or
  Tailwind implementation.
- No external fonts, icon services, scripts, or CDNs.
- No gradients, glow effects, glass effects, decorative blobs, oversized hero
  headings, or purple/blue AI-style decoration.

## Product Decisions

### Visual Character

The interface is quiet, precise, and work-focused. Canvas and surfaces use
neutral gray values; teal is reserved for primary actions, selection, focus,
and positive brand recognition. Red, amber, green, and blue retain conventional
semantic roles. Ordinary page sections remain unframed, while cards are used
only for individual repeated items, bounded tools, dialogs, and data that needs
a clear visual boundary.

The baseline light tokens are:

- canvas: `#f6f7f8`
- surface: `#ffffff`
- elevated surface: `#ffffff`
- strong text: `#17201d`
- regular text: `#34413d`
- muted text: `#66716d`
- border: `#dfe5e2`
- accent: `#0f8f76`
- accent hover: `#0b7662`
- accent soft: `#eaf6f2`

Dark mode uses neutral charcoal surfaces rather than a blue-tinted palette.
Exact dark values may be tuned during visual verification, but must retain at
least WCAG AA text contrast and visually distinct canvas, surface, border, and
elevated layers.

Card radius is 6-8 pixels. Shadows are limited to drawers, dialogs, dropdowns,
and other actually elevated UI. Text sizing is stable and does not scale with
viewport width. Controls use predictable heights so loading, icons, labels,
and state changes do not shift layout.

### Reference Use

Borrow from New API:

- compact grouped navigation and quiet active states
- a restrained, centered authentication surface
- clear light/dark/system theme behavior
- fixed desktop chrome and a mobile navigation drawer
- neutral backgrounds, small radii, fine borders, and concise labels

Do not copy:

- React/Semi UI/Tailwind implementation details
- pill styling on every control
- module groupings that do not match this product
- decorative or marketing-oriented authentication content
- lower information density that would slow down operational workflows

## Architecture

### Global Styles

Add a small global style layer imported by `main.ts`:

- `styles/tokens.css` owns semantic light and dark CSS variables plus Element
  Plus variable mappings.
- `styles/base.css` owns the reset, body typography, focus treatment, scrollbars,
  reduced-motion behavior, and common page/content utility classes.

Views keep business-specific layout rules but consume semantic variables. New
hard-coded surface, text, border, and accent colors are not allowed in page
styles. Existing hard-coded colors in files touched by this refresh are migrated
to tokens.

Element Plus dark variables are imported from its packaged dark theme. The
design layer overrides only the variables needed to make native components fit
the product tokens; it does not duplicate component internals page by page.

### Theme State

A focused theme store or composable owns:

- user mode: `light`, `dark`, or `auto`
- resolved mode: `light` or `dark`
- safe local-storage persistence
- `prefers-color-scheme` observation while mode is `auto`
- the `dark` class and a theme data attribute on the document root

Theme state is initialized before Vue mounts so reloads do not flash the wrong
theme. The theme selector appears in authenticated top bars and both login
screens. It is a compact menu or segmented control appropriate to the available
space, not three large text buttons.

### Shared Application Shell

Admin and portal use one presentation-focused shell component. It accepts
navigation entries, active route, current title, account context, and actions;
it does not own authentication, portal announcements, token provisioning, or
route-specific data.

The shell provides:

- a 216-pixel expanded desktop sidebar
- a 64-pixel collapsed desktop sidebar with icon tooltips
- a 56-pixel top bar
- a mobile drawer opened by a menu icon
- grouped icon-and-label navigation using existing Element Plus icons
- a compact brand mark that remains legible in collapsed mode
- current page title, theme selector, and account menu
- stable main-content scrolling and responsive gutters

Admin composition remains in `App.vue`. Portal-specific employee state,
announcement behavior, logout behavior, and `portalToken` injection remain in
`Portal.vue`, which supplies them to the shared shell.

Desktop collapse preference is stored locally. Mobile drawer state is
transient, closes after navigation, and never changes the desktop preference.

### Authentication Surface

Both login views use the same authentication layout and visual primitives while
retaining separate forms and API calls. The surface contains a compact brand
mark, product name, account-context label, form, primary submit action, theme
selector, and context-appropriate support text.

Admin continues to accept username and password. Portal continues to accept
employee ID and downstream key. Validation, loading, Enter submission, success
navigation, and existing server error semantics remain unchanged. The layout
uses a quiet neutral canvas with a single bordered panel and no split hero,
gradient, illustration, or explanatory marketing copy.

## Page Design

### Admin Dashboard

Retain the existing data, model-health link, KPIs, and charts. Replace the large
hero treatment with a compact page header and status strip. Present KPIs as a
stable responsive grid of individual metric items. Chart sections use a single
boundary, consistent headers, and theme-aware axes, legends, tooltips, and
empty states. Explanatory prose remains secondary to operational signals.

### Admin Upstreams And Downstreams

Use a consistent management-workbench layout:

- compact page header with the primary create action
- collapsible filter or summary toolbar
- dense table with stable row actions
- controlled horizontal scrolling at narrow widths
- right drawer with clear sections and a persistent action footer
- consistent confirmations for rotate, delete, and other destructive actions

Nested cost/context/model tables remain functional. They use section headings
and separators rather than cards nested inside a drawer card. Secret masking,
copying, expiry, limits, and model behavior remain unchanged.

### Admin Logs

Keep the full operational field set and existing error-category semantics. The
filter region becomes a compact, wrappable toolbar with a mobile disclosure.
Active filters are obvious, clearing filters is direct, and table height does
not jump during loading. Long values use truncation plus existing details or
tooltips. Empty and failed states explain whether no rows matched or loading
failed without replacing the rest of the work surface.

### Admin Model Probe, Troubleshooting, And Compatibility

These surfaces prioritize evidence over decoration. Qualification commands,
status summaries, filters, matrices, conflicts, active requests, and expanded
checks retain their current behavior. Shared status tokens distinguish healthy,
degraded, offline, warning, and failed states in both themes. Tables remain
dense, while mobile layouts move toolbars into vertical groups and allow the
evidence grid to scroll without covering controls.

`ModelProbeBoard`, `TroubleshootingCenter`, and `CompatibilityMatrixPanel` use
unframed page sections or one bounded tool surface rather than stacks of nested
cards.

### Admin Announcement

Use a narrow readable form width inside an unframed page section. Group content,
severity, enabled state, and metadata with spacing and dividers. The save action
remains easy to locate and does not move when validation or loading text appears.

### Portal Overview And Quota Details

Replace the summary card containing quota cards with a page header, a quota
summary band, and a responsive metric grid. Request and token limits use clear
labels, values, reset timing, and theme-safe progress indicators. Model and usage
sections remain scannable without repeating container chrome.

`QuotaDetails.vue` receives the same visual treatment even though this design
does not add or change a route for it.

### Portal Usage History

Place range selection and refresh in a compact toolbar. Charts and history rows
share the same date, number, tooltip, empty, and loading styles as the admin
dashboard. Mobile controls wrap without resizing the chart region.

### Portal Integration

Keep the compatibility matrix, model information, configuration examples,
language tabs, and copy behavior. Remove nested decorative cards, use one code
surface per example, and use icon-based copy actions with tooltips. Code blocks
must preserve contrast, wrapping/scrolling, and copied feedback in both themes.

### Portal Playground

Preserve the existing chat workflow and API behavior. The desktop view is a
full-height work surface with a bounded configuration sidebar, message stream,
and stable composer. On mobile, model settings move into a drawer so the chat
surface owns the viewport.

Replace text triangles and ad hoc symbols with existing library icons and
tooltips. Message content, reasoning details, files, streaming status, errors,
usage, and timing remain visible. Markdown, inline code, fenced code, tables,
and long words receive explicit overflow and dark-theme rules. The composer has
stable dimensions while sending and keeps attachments and actions from
overlapping entered text.

### Portal Key Management

Use a focused security surface with the current key state, concise metadata,
and clearly separated copy/rotate actions. Rotation remains a confirmed
destructive operation. Secret values retain current masking and handling rules.

### Portal Model Probe

Use the same `ModelProbeBoard` system as admin with portal-specific content and
tone. Admin-only operational data remains hidden exactly as it is now.

## Shared Interaction States

Every refreshed page covers:

- initial loading and background refresh
- empty data and empty filtered results
- recoverable API error and validation error
- disabled and submitting actions
- hover, active, selected, and keyboard focus
- destructive confirmation and successful completion
- long text, long identifiers, and large numeric values

Element Plus message behavior remains the default transient feedback path.
Page-local errors remain visible when the user needs them to understand or
retry a failed operation. No raw secret, token, or unsanitized error content is
added to logs or decorative UI.

## Responsive Behavior

Use layout breakpoints based on content needs, with 768 pixels as the main
mobile-shell threshold unless visual verification finds a specific page needs
an earlier transition.

- Navigation becomes a drawer on mobile.
- Page gutters reduce predictably rather than disappearing.
- Header actions wrap or move into menus; text is not squeezed under icons.
- Filter forms become vertical or disclosed panels.
- Tables scroll inside stable containers and do not widen the page.
- Drawers use full or near-full viewport width on small screens.
- Dialog widths use viewport constraints.
- Metric grids reduce columns without changing card height unexpectedly.
- Charts retain explicit minimum heights and resize after container changes.
- No fixed element covers page content, messages, dialogs, or the composer.

## Charts And Theme Changes

Dashboard, model-probe, portal overview, and usage-history charts consume
semantic chart colors. When resolved theme changes, each owner updates or
recreates its ECharts instance so axes, labels, legends, tooltips, and series
colors all change together. Existing chart data utilities remain authoritative;
theme plumbing does not move business aggregation into components.

Chart containers have stable dimensions and show an explicit loading or empty
surface rather than a blank canvas.

## Routing And Data Flow

Existing paths, names, redirects, lazy imports, and guards remain intact. Route
metadata may gain page titles used by the shared shell and document title. The
theme and shell state are local presentation state and do not enter API calls.

Admin token handling remains in the existing auth store and guard. Portal token
and employee ID semantics remain unchanged. Portal announcements still load and
acknowledge through the current APIs and storage keys.

## Accessibility

- Interactive controls have visible keyboard focus in both themes.
- Icon-only controls have accessible labels and hover tooltips.
- Color never carries status meaning alone; text or icons remain present.
- Form labels remain associated with controls.
- Reduced-motion preference disables nonessential transitions.
- Body text, muted text, controls, borders, and charts are checked for usable
  contrast in both themes.
- Drawer and dialog behavior continues to use Element Plus focus management.

## Testing Strategy

Implementation follows focused test-driven slices. Add tests for:

- theme parsing, persistence, auto resolution, system-theme changes, and root
  document state
- route page-title metadata without weakening existing auth guards
- shared shell desktop collapse, mobile drawer, navigation-close behavior, and
  account/theme actions
- both login forms retaining their current API calls and navigation behavior
- chart theme update helpers and stable empty/loading behavior
- source-level invariants where component mounting would require excessive
  mocking, while favoring behavior tests for reusable logic

Retain all existing API, router, utility, portal integration, model probe,
playground, chart, compatibility, and troubleshooting tests.

Verification includes:

- `rtk npx vitest run` from `frontend`
- `rtk npm run build` from `frontend`
- desktop and mobile browser checks for both login screens, both shells, and
  every routed view in light and dark modes
- screenshot review for overlap, clipping, blank charts, theme leaks, nested
  cards, and unintended gradients
- focused interaction smoke for sidebar collapse, mobile drawer, theme switch,
  login validation, filters, drawers, dialogs, copy actions, and playground
  composer behavior

Backend tests are not expanded unless implementation unexpectedly changes a
shared contract. Such a change is outside this design and requires separate
approval.

## Delivery Sequence

1. Add theme tests, semantic tokens, global base styles, and pre-mount theme
   initialization.
2. Build and test the shared shell, then migrate admin and portal composition.
3. Refresh both login screens using the shared authentication surface.
4. Migrate admin pages: dashboard, model probe, upstreams, downstreams, logs,
   troubleshooting/compatibility, and announcement.
5. Migrate portal pages: overview/quota details, model probe, usage history,
   integration, playground, and key management.
6. Make every ECharts owner theme-aware and verify stable chart sizing.
7. Run full automated verification and browser-based desktop/mobile visual QA;
   fix visual and behavioral regressions before completion.

## Acceptance Criteria

1. Every routed admin and portal page, both login screens, `QuotaDetails.vue`,
   and all shared user-facing components use the unified visual system.
2. Light, dark, and system themes resolve correctly, persist safely, and do not
   flash the wrong theme at startup.
3. Admin and portal share responsive chrome while retaining separate auth,
   announcement, account, and token behavior.
4. Desktop navigation collapses to an icon rail and mobile navigation uses a
   drawer that closes after selection.
5. Existing routes, APIs, filters, forms, tables, drawers, dialogs, charts,
   copy actions, qualification evidence, troubleshooting, and playground flows
   retain their behavior.
6. No page uses gradients, glow effects, glass effects, decorative blobs,
   oversized hero type, floating section cards, or cards nested inside cards.
7. Dense operational pages remain efficient to scan on desktop and usable on
   mobile without incoherent overlap or page-level horizontal overflow.
8. Charts, code blocks, markdown, tables, forms, empty states, loading states,
   and errors remain readable in both light and dark themes.
9. Frontend tests and the production build pass.
10. Desktop and mobile visual verification covers every routed page in both
    resolved themes and finds no clipped navigation, blank content, overlap,
    or unintended hard-coded theme leaks.
