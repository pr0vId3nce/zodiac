# Zodiac polish log

Running log of small improvements and bug fixes, one commit each. Started
2026-08-11 under a "find little improvements for N turns" goal. Newest first.

Each entry: what changed, why, and the commit. Unresolved items / follow-ups
are collected at the bottom.

**Summary (25 changes):** markdown polish (task checkboxes, nested lists, GFM
tables, strikethrough, h4–h6, `www.`/paren-URL links, code/result copy
buttons), real bug fixes (composer paste was dead, file drag-drop hit the wrong
buffer, table egui-id collisions, a UI-freeze loading pi models, a
multibyte-slice panic, links truncated at parens), and UX (per-pane composer
drafts, wrapping+capped composer, Observatory arrow-nav empty-state, Esc blur,
auto-expand failed tools, card model chip, settings scroll). All gate-green;
the headless GUI selftest walks every screen + a seeded rich pane and exits 0.

---

## Changes

<!-- new entries go here, newest first -->

### 27. Claude Code slash commands + the terminal-clipping root cause
Four-part goal: slash commands in structured panes, terminal passthrough
fidelity, the claude-in-a-terminal graphics glitch, and the tab-completion
freeze. Status of each is in **`zodiac-gui/SLASH-AND-TERMINAL.md`** — read that
for the detail, including the two items not finished.

Landed: slash-command discovery + a floating picker above the composer
(arrows/Enter/Tab/click, filters as you type, claude panes only); `/resume`
implemented zodiac-side with a real session picker (the CLI refuses it over a
pipe); `T_NEW_PANE` gained a `session` field; **the terminal grid is now
measured inside the frame margins** — it was over-reported by ~2 columns and
~1–2 rows, which is why Claude Code's prompt box lost its right border and had
its bottom line clipped; and a composer-inflation regression from the earlier
wrapping-composer change. Commits `729cb12`, `2c39eaa`, `4860552`.

### 26. CPU-burn handoff: all four fixes (see `zodiac-gui/HANDOFF.md`)
Acted on the macbook's profiling handoff — the GUI pegged a core while agents
streamed. Landed all four suggested fixes: virtualized the transcript
(`show_viewport` + cached item heights), cached laid-out galleys so completed
turns stop re-parsing Markdown and rebuilding `LayoutJob`s every frame, floored
the zero-delay repaint at 16ms, and stopped/throttled animation when the window
is occluded/unfocused. Measured ~40x less CPU at 400 items and cost now flat
from 1600→4800 items. Added `ZODIAC_GUI_PERF_OFF` (A/B on one binary),
`ZODIAC_GUI_SEED_ITEMS` (soak fixture), and a `profiling` cargo profile
(release + symbols; `strip = true` had made `perf` useless). **Residual, still
open:** a large idle pane costs ~8-10% of a core; details and hypothesis in the
handoff's RESOLUTION section. Commits `3d07c51`, `74987aa`.

### 25. Exercise new markdown in the selftest seed
`seed_agent_pane` (the `ZODIAC_GUI_SELFTEST` rich pane) now includes a table,
nested list, task-list checkboxes, strikethrough, a link with parens in the
URL, and a bare `www.` link — so the headless selftest actually renders the
new markdown paths and would panic-fail if any regressed. Verified: the
selftest walks all screens + this pane and exits 0 (no panic) on a real
Wayland run. `app.rs`.

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

_Refreshed 2026-08-12 after the slash-command/terminal work. Resolved items are
struck through so the list stays honest about what's actually open._

### Open

- **Perf: a large idle transcript still costs ~8–10% of a core.** After
  virtualizing + caching layout, cost is flat in transcript length (1600 vs
  4800 items), but it doesn't fall to idle. Not startup — sampling at t=25s
  gives the same figure. Hypothesis: a repaint feedback loop (measured heights
  nudge content height, which nudges the stick-to-bottom ScrollArea, which
  repaints), now bounded by the 16ms clamp so it reads as a fixed cost.
  Confirm with a frame counter or `perf top` before trusting that.
  See `zodiac-gui/HANDOFF.md`.
- **Perf was never measured against a *streaming* pane.** My fixture seeds a
  transcript but nothing streams, and streaming is what pinned the repaint
  clock in the original profile. The closest analogue showed ~40x, but a real
  streaming session is the honest confirmation.
- **Terminal mouse mapping is unverified on-device.** `grid_cell` now maps
  through the geometry `terminal_view` actually draws with, but I can't
  synthesise mouse input here — needs a click test in a mouse-driven TUI (vim,
  htop).
- **Missing color-emoji glyphs in terminal panes.** `🦀` draws as `□` while
  `📦` is fine, so the fallback chain has gaps. `font.rs` / `theme.rs`.
- **Kitty graphics inside terminal mode.** Long-standing follow-on: the grid
  draws text and colors but not images.
- **`/resume` opens a new pane** rather than resuming in place. Replacing in
  place needs a server-side "respawn this pane's agent" path.
- **Slash picker is claude-only.** Pi has its own command set; nothing is
  offered for pi panes.
- **Finish sound never plays on agent completion (GUI, and seemingly TUI).**
  `finish_sound` + `protocol::play_sound` exist, but the only call site is
  `client.rs::cycle_finish_sound` — a *preview* when the setting changes. To
  finish: track each pane's previous status and call `play_sound` on a
  working→done edge. Left alone because the trigger/debounce is a judgement
  call (a mis-timed sound is worse than silence) and it wants audio testing.
- **Enter on the Observatory didn't reliably open a pane** during scripted
  testing (I worked around it with Alt+1–9, which now also switches to the
  pane). Might be my synthetic input rather than a real bug — worth a quick
  manual check that Enter opens the highlighted card.

### Resolved since this list was written

- ~~Composer draft is shared, not per-pane.~~ Done (#21/#23).
- ~~Transcript re-parses everything every frame.~~ Done (#26) — virtualized
  with `show_viewport` + cached item heights, plus a galley cache.
- ~~Terminal mouse coordinate mismatch.~~ Fixed; verification still open above.
- ~~Couldn't find `zodiac-gui/handoff.md`.~~ It landed later; acted on in full.
