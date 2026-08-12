# Slash commands + terminal fidelity — status report

_2026-08-12, NTP424. Covers the four-part goal: Claude Code features in
structured panes, terminal passthrough, the claude-in-a-terminal graphics
glitch, and the tab-completion freeze. Two of the four are done, one is
root-caused and fixed pending your confirmation, one is not started._

## 1. Slash commands in structured agent panes — **done**

**The finding that unblocked this:** Claude Code *does* resolve a leading
`/name` in a user message under `--input-format stream-json`. I verified it
against the CLI — `/cost` sent as a user message comes back with real usage
output. So structured panes could always *run* slash commands; they just had no
way to discover them.

- **Discovery** (`slash.rs`): built-ins, `~/.claude/commands/**/*.md` and the
  project's `.claude/commands` (nested dirs namespaced `dir:name`), and skills
  (`SKILL.md`), with descriptions parsed from YAML front matter.
- **Picker**: typing `/` opens a menu **above** the composer. Filters as you
  type, `↑↓` selects, `Enter`/`Tab` completes, click picks. Keys are consumed
  before the text field sees them, so Enter completes rather than sends. Only
  shown for panes running the claude harness.
- Verified on a real claude pane (screenshotted): the menu lists built-ins and
  your installed skills with origin tags.

### `/resume` — **done, implemented zodiac-side**

`/resume` is the one command that genuinely cannot work over a pipe; the CLI
answers `"/resume isn't available in this environment."` So zodiac answers it
itself: submitting `/resume` opens a session picker listing past sessions for
that pane's directory (from `~/.claude/projects/<cwd-slug>/*.jsonl`, labelled
with the first user prompt and an age), and choosing one spawns a pane with
`--resume <id>`. `T_NEW_PANE` gained a `session` field — `new_agent_pane`
already accepted one, nothing ever passed it. Verified end to end.

**Caveat worth knowing:** resume opens a **new pane** rather than replacing the
current one. Replacing in place needs a server-side "respawn this pane's agent"
path; opening a pane was the safe version and is clearly labelled in the dialog.

## 2. Claude Code graphics look wrong in a terminal pane — **root-caused and fixed**

**Root cause: zodiac was lying to the pty about its size.** The grid was
measured in `focused()` (`ui.rs`) *before* `terminal_view` applied its 12px
frame margins, so the child was told it had ~2 more columns and ~1–2 more rows
than were ever painted. Claude Code laid its UI out against the larger grid and
the surplus fell outside the drawn area — which is exactly the reported symptom:
the prompt box loses its right border and its bottom status line is clipped in
half. I reproduced it by running `claude` inside a real terminal pane and
screenshotting (before/after images were the evidence).

Fixed by measuring inside the frame, against the real content rect.

**Please confirm this one on your machine** — I verified the reproduction and
the reasoning, but my post-fix screenshot attempt kept landing on the
Observatory instead of the focused pane, so I did not get a clean side-by-side
of the same screen after the change.

Separately, a genuine (smaller) glyph gap: `🦀` renders as `□` in terminal panes
while `📦` renders fine, so the fallback chain is missing some color-emoji
coverage. Not addressed; noted for later in `font.rs`/`theme.rs`.

## 3. Tab-completion freeze — **not fixed; here is what I ruled out**

The symptom (freezes until you switch panes and back) points at output that is
produced but never displayed. I ruled out the two most likely mechanisms:

- **Not the DECSET 2026 synchronized-update path.** `process_output`
  (`pane.rs`) does withhold output while a child holds `?2026h` open, which
  would look exactly like this — but the deadline valve is real and wired: the
  event loop shortens its tick to 50ms while any pane is holding
  (`server.rs:306`) and calls `sync_flush_tick` every iteration, flushing after
  `SYNC_DEADLINE` (150ms). It self-heals in ~200ms.
- **Not exotic escape sequences.** I captured what your zsh actually emits on
  Tab in a real pty: no `?2026`, no alt-screen, no bracketed-paste toggling —
  just `\r`, cursor-up, `ED`, and a prompt redraw with a grey autosuggestion.
- **Not the old scrollback panic.** `visible_rows` is correct now, and
  `set_scrollback` clamps its offset.

**Leading hypothesis, untested:** it is the *same* bug as #2. If the pty
believes it has more rows than are painted, zsh's completion redraw (which
moves the cursor up and repaints) can land in the region that is never drawn —
the pane looks frozen, and switching away and back forces a resize/redraw that
re-syncs it. If that is right, the grid fix above may have already fixed the
freeze. **Worth retesting first**, before anyone digs further.

## 4. Near-native terminal passthrough — **not started**

Untouched. The current design parses output into a vt100 model server-side and
repaints cells in egui, so fidelity is bounded by what that model covers. The
grid-size fix above removes the largest source of visible wrongness, and it is
worth re-evaluating how far off "native" actually feels once that is in, before
committing to an architectural change — a rewrite is a large, risky project and
the evidence so far points at ordinary bugs rather than a doomed design.
