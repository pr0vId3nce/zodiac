# Zodiac polish log

Running log of small improvements and bug fixes, one commit each. Started
2026-08-11 under a "find little improvements for N turns" goal. Newest first.

Each entry: what changed, why, and the commit. Unresolved items / follow-ups
are collected at the bottom.

---

## Changes

<!-- new entries go here, newest first -->

### 24. Copy button on tool result boxes
Extended the hover "copy" affordance to tool **result** boxes (copies the full,
unclipped output — errors and command output are worth grabbing). Factored the
button into a shared `copy_overlay` helper used by both code and result boxes.
`ui.rs`.

### 23. Prune composer draft on pane close
Follow-up to #21: when `T_PANE_CLOSED` removes a pane, drop its entry from the
per-pane `composers` map so drafts for closed panes don't linger. `app.rs`.

### 22. Esc blurs the composer (then returns to Observatory)
The Esc→Observatory handler is gated on nothing being focused, so Esc did
nothing while the composer held focus. Now a first Esc surrenders the
composer's focus and a second Esc returns to the Observatory — the standard
two-step. `ui.rs` (`composer_bar`).

### 21. Per-pane composer drafts
The composer was one shared buffer that got cleared on every pane switch, so a
draft typed in one agent pane was lost when you switched away. Replaced
`UiState.composer` with a per-pane `composers: HashMap<u64, String>`: each
agent keeps its own in-progress message, drafts survive switching, and Send
clears only that pane's draft. `ui.rs` + `app.rs`. (Resolves the follow-up
noted below.)

### 20. Fix drag-and-drop of files onto an agent pane
`on_drop` appended the dropped path to `p.agent.input` (the TUI-side editor),
but the GUI composer is `ui_state.composer` and never reads `agent.input` — so
files dropped on a structured agent pane silently vanished. Now the path is
appended to the GUI composer where the user can review and send it. `app.rs`.

### 19. Skip empty code boxes
An empty fenced block (```` ``` ```` with nothing inside) rendered an empty
bordered box (now also with a copy button). `render_body` skips a code box
whose content is blank. `ui.rs`.

### 18. Show the running model on Observatory cards
Agent cards now show the running model (e.g. `opus`, `haiku 4.5`) next to the
agent chip — from `p.agent.model` (claude, via the stream) or `PaneState.model`
(pi) → `short_model`. Matches the focused-header treatment. `ui.rs`.

### 17. Copy button on code boxes
Hovering a fenced code box now shows a small "copy" button in the top-right
that copies the raw code to the system clipboard (via the arboard-backed
`copy_text` path). `ui.rs` (`code_box`).

### 16. Ordered lists with `)` delimiter
`md_split_ordered` only accepted `1. `; GFM also allows `1) `. Now both
render as ordered-list items. `ui.rs`.

### 15. Scroll the settings dialog on short windows
The settings dialog grew (Theme, numerals, view, motion, the 3 font-size rows,
behavior toggles) and had no scroll, so on a short window or at a large GUI
font zoom it could run off-screen with the Done button unreachable. The body
groups are now in a height-capped vertical scroll; the title and Done footer
stay pinned. `ui.rs`.

### 14. Fix paste into the composer / egui text fields
`egui-winit` is built without its `clipboard` feature (copy is served from our
arboard handle), so egui never converted Ctrl/⌘+V into a Paste event — pasting
into the composer, palette, or settings fields silently did nothing. Now the
redraw path detects the paste chord in the egui event stream and injects the
system clipboard as an `egui::Event::Paste`. Harmless in terminal mode (no
egui widget focused; the pty paste path still runs). `app.rs`.

### 13. Markdown links with parens in the URL
`parse_link` stopped at the first `)`, truncating links like
`.../wiki/Foo_(bar)`. Scan for the matching close paren with depth tracking
so balanced parens inside the URL are kept. `ui.rs` + test.

### 12. README: drop stale "pty not resized" gap
The known-gaps list claimed terminal-mode pty resize wasn't implemented, but
the focused terminal's measured grid (`st.term_grid`) is sent via
`apply_grid` → `T_RESIZE` every frame, so panes are resized to the widget.
Removed the outdated bullet. `zodiac-gui/README.md`.

### 11. README: document the new features
Updated `zodiac-gui/README.md` to reflect what shipped: Observatory
arrow/Enter nav + vertical cards, transcript markdown (h1–h6, nested lists,
task checkboxes, tables, strikethrough, browser-opening links),
PageUp/PageDown scroll, the wrapping composer, per-view font sizes, and Alt+Z.

### 10. Auto-expand failed tool boxes
Tool call boxes default to collapsed; error results are important, so a tool
box whose result `is_error` now starts expanded (successful ones stay
collapsed). `ui.rs` (`turn_tool_box`).

### 9. Clearer Observatory empty state
The Observatory showed "connecting…" whenever there were no panes — including
when fully connected with everything closed. Now, once state has arrived
(`d.state.is_some()`), it shows "No panes yet" plus the Alt+N / Alt+Shift+N /
Alt+Z hints; "connecting…" only shows pre-connection. `ui.rs`.

### 8. Non-blocking pi model loading (fix UI freeze)
The earlier `pi --list-models` change waited on the CLI with an 8s
`recv_timeout` on the UI thread — the first new-agent-picker open could freeze
the GUI for up to 8s while pi (Node) started. Now the fetch runs as a
background prewarm kicked off at startup (`prewarm_pi_models`), cached in a
`Mutex`; the picker reads the cache if ready else falls back to `models.json`
without blocking. `agents.rs` + `app.rs`.

### 7. Markdown strikethrough (`~~text~~`)
`parse_inline` now toggles a strike run on `~~` (like `**` for bold) and
`span_format` draws the strikethrough. Threaded a `strike` flag through the
`MdSpan` constructions. `ui.rs`.

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
- ~~Composer draft is shared, not per-pane.~~ **Done in #21** — per-pane
  `composers: HashMap<u64,String>`; closed panes' drafts are pruned on
  `T_PANE_CLOSED` (#23).
- **Finish sound never plays on agent completion (GUI, and seemingly TUI).**
  The `finish_sound`/ringtone setting exists and `protocol::play_sound` works,
  but the only call site is `client.rs::cycle_finish_sound` — a *preview* when
  you change the setting. I couldn't find a working→done transition that
  actually plays it in the TUI, and the GUI has no audio wiring at all. To
  finish it: track each pane's previous status in `GuiApp`, and on a
  working→done edge call `zodiac::protocol::play_sound(settings.finish_sound_path())`.
  Left out because the intended trigger/debounce is unclear (a mis-timed sound
  on every state flip would be worse than silence) and it wants on-device
  audio testing.
- **Transcript re-parses everything every frame.** `transcript_view` renders
  all `items` (and re-parses the streaming tail's markdown) each frame with no
  virtualization; a very long transcript or a very long single streaming
  message is O(n)/frame. Fine today; if it ever drags, cache parsed turns or
  cull with `ScrollArea::show_viewport`.
- **Couldn't find `zodiac-gui/handoff.md`.** The user referenced findings from
  another agent at `zodiac-gui/handoff.md`, but as of these commits it isn't in
  the working tree, on `origin/main`, in any branch/PR ref, or anywhere on disk
  under `/home/d3s` (searched case-insensitively — only the unrelated
  `HANDOFF.md` iOS handoff exists). Likely committed to a different clone/remote
  that hasn't reached `pr0vId3nce/zodiac`. Needs the correct path/repo to fold
  its findings in.
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
