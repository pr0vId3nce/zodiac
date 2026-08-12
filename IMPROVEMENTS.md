# Zodiac polish log

Running log of small improvements and bug fixes, one commit each. Started
2026-08-11 under a "find little improvements for N turns" goal. Newest first.

Each entry: what changed, why, and the commit. Unresolved items / follow-ups
are collected at the bottom.

---

## Changes

<!-- new entries go here, newest first -->

### 6. Fix markdown table egui id collision
`md_table` used the header text as its egui id, so two tables with the same
header (common when an agent emits several similar tables) collided and the
second broke. Give each table a per-render unique id via a `TABLE_SEQ`
thread-local counter reset at the top of `transcript_view`. `ui.rs`.

### 5. Cap the composer height
The multiline composer (from the earlier overflow fix) could grow unbounded
with a long/pasted message and cover the transcript. Wrap it in a vertical
scroll capped at ~7 lines (132px); longer input scrolls inside. `ui.rs`.

### 4. Render `####`/`#####`/`######` headings
`md_block` only handled h1–h3, so `#### text` showed literally. Now h1–h6 are
handled (longest marker checked first so `####` isn't captured by `###`).
`ui.rs`.

### 3. Bare `www.` links in the transcript
`www.example.com` (no scheme) is now detected and rendered as a clickable
link with an `https://` href, alongside the existing bare-http detection.
Requires a dot after `www.` so `www.` alone isn't linked. `ui.rs`
(`parse_www_url`, shared `consume_url_token`).

### 2. Nested list indentation in the transcript
Sub-lists were flattened because `md_block` trims leading whitespace. Now the
leading indent (2 spaces / 1 tab per level, capped at 6) is measured and
applied as a pixel inset to bullet, task, and ordered-list rows, so nested
lists read as nested. `ui.rs` (`list_indent_px`).

### 1. Markdown task-list checkboxes in the transcript
`- [ ] todo` / `- [x] done` now render a checkbox glyph (☐ / ☑) instead of a
plain bullet; done items are dimmed. Agents emit these constantly (plans,
progress). `md_block` detects the `[ ]`/`[x]` prefix (case-insensitive x) and
routes to a new `md_task_row`. `ui.rs`.

---

## Unresolved / follow-ups

<!-- anything found but not fixed, with enough detail to pick up later -->
- **Terminal mouse coordinate mismatch (pre-existing).** `GuiApp::grid_cell`
  (app.rs) maps `cursor_px` → (col,row) using the wgpu renderer's `r.cell` and
  `r.grid_origin()`. But the terminal is drawn by egui `terminal_view` (ui.rs)
  at `13.0 * term_scale`, inside a Frame with `inner_margin(12)` and a header
  panel above it — so the cell size and the content origin both differ from
  what `grid_cell` assumes. Result: mouse reporting to terminal apps (vim,
  htop, tmux) can land on the wrong cell; the new terminal-font scale widens
  the gap. Proper fix: record the terminal grid's real rect (origin + scaled
  cell) from `terminal_view` into `UiState` (like `term_grid` already records
  rows/cols/cw/ch) and have `grid_cell` use that, accounting for
  points↔pixels and the egui zoom. Left out here because it's a larger change
  than the surrounding polish and needs on-device verification with a
  mouse-driven TUI.
