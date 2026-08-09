# Reasoning Probe Results Tab Design

Date: 2026-08-09

## Status

Approved in conversation. Pending user review of this written specification
before implementation planning begins.

## Problem

The admin model-probe page currently inserts one-click reasoning-level results
above the main model status board. The model summary and exact-route table are
large enough to push the primary probe content down the page and make the page
hard to scan.

## Selected Design

Keep a single admin route and split its content into two in-page tabs:

- `模型状态` contains the existing `ModelProbeBoard` and qualification result.
- `思考档位` contains the one-click probe progress, model-level reasoning
  summary, and exact-route diagnostics.

The command bar remains above the tabs so both actions are always available.
Starting `一键探测思考档位` immediately selects the `思考档位` tab, allowing
the user to see progress without another click. Existing discovery data loaded
on page mount also appears in this tab but does not force a tab switch.

## Behavior

- The default tab is `模型状态`.
- The reasoning tab remains available before any probe has run and shows a
  compact empty state instead of disappearing.
- While a probe is running, its progress indicator stays in the reasoning tab.
- Completed model and exact-route results retain their current fields,
  statuses, tags, and retry timestamps.
- Probe errors use the existing toast behavior and preserve any previously
  loaded discovery results.
- Qualification behavior, model-probe polling, capability APIs, and backend
  behavior do not change.

## Layout

The tabs use Element Plus's existing `el-tabs` pattern and the project's
current border, surface, spacing, and typography tokens. The reasoning result
section becomes an unframed tab body rather than an additional floating panel.
Wide route data remains inside the existing responsive table shell so it can
scroll horizontally on narrow screens without expanding the viewport.

## Testing And Verification

- Add a view-level regression test proving the two tab labels and their content
  ownership.
- Verify that triggering the one-click probe selects the reasoning tab.
- Run the focused frontend tests, type check, and production frontend build.
- Start a local development server and inspect desktop and mobile layouts with
  Playwright, including horizontal overflow and element overlap checks.

## Non-Goals

- Adding a new router entry or admin navigation item.
- Changing capability discovery, polling, or result aggregation.
- Redesigning the model status board or qualification workflow.
- Changing wording or behavior outside the model-probe page.
