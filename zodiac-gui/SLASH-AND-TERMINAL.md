# Slash commands + terminal fidelity — status report

_2026-08-12, NTP424. Covers the four-part goal: Claude Code features in
structured panes, terminal passthrough, the claude-in-a-terminal graphics
glitch, and the tab-completion freeze. **All four now have fixes landed and
verified**, except the passthrough item, which is narrower than it looked —
see §4._

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
  your installed skills with origin tags, filters as you type, and completes.
- **Commands actually execute**, not just list: running `/cost` in a structured
  pane returned the real usage report into the transcript (screenshotted).
  A cosmetic follow-on from that test is also fixed — CLI-answered commands
  report a placeholder model (`<synthetic>`), which was being shown as the
  pane's running model.

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

**Verified** with a clean before/after of the same screen. Before: the welcome
box had no right border (text ran off the edge) and the bottom status line was
sliced in half. After: the box's right border is drawn and text truncates with
`…` *inside* it, and `⏸ manual mode on · ? for shortcuts · ← for agents` is
fully visible.

Separately, a genuine (smaller) glyph gap: `🦀` renders as `□` in terminal panes
while `📦` renders fine, so the fallback chain is missing some color-emoji
coverage. Not addressed; noted for later in `font.rs`/`theme.rs`.

## 3. Tab-completion freeze — **fixed** (it was never zsh)

**It was egui stealing the keyboard.** With a pane "frozen", input sent over the
CLI (`zodiac send`) still reached the shell and the shell still responded — but
the *server's own* screen stayed stale, which means the GUI had stopped
delivering keystrokes entirely. Tab is focus traversal to egui: one Tab moved
keyboard focus onto a widget, egui then reported every later key event as
`consumed`, and `on_key` (gated on `!consumed`) stopped forwarding to the pty.
Switching panes reset focus — exactly the workaround you found.

Fix: while a terminal is on screen the pty owns the keyboard, so egui's
`consumed` verdict is ignored for key events and any focus egui is holding is
dropped. Overlays and the agent composer are excluded, so they keep their keys.
Verified: after Tab the completion menu opens (`0/20`) and typing still reaches
the shell.

### What I had ruled out first (kept for the record)

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

(My first hypothesis — that it was the same grid-size bug as §2 — was wrong.
The decisive test was comparing the *server's* rendered screen against the
GUI's: both were stale, which ruled out a client-render bug, and CLI input
still working ruled out the pty.)

## 4. Near-native terminal passthrough — **three concrete defects fixed; no rewrite**

I did not restart the terminal layer, and I'd argue against it: every symptom
that made panes feel non-native turned out to be an ordinary bug, not a limit
of the design. Three are now fixed:

1. **Wrong grid size** (§2) — the child was told it had rows and columns that
   were never painted.
2. **Keys stopping after Tab** (§3) — egui was swallowing terminal input.
3. **Mouse clicks landing on the wrong cell** — `grid_cell()` mapped the
   pointer with the legacy wgpu grid's cell size and origin, but the terminal
   is drawn by egui inside a margined frame below a header, so mouse-driven
   TUIs (vim, htop, tmux) received the wrong coordinates; the per-view font
   scale widened the error. `terminal_view` now publishes the geometry it
   actually draws with and `grid_cell` maps through that. **Not verified
   on-device** — I have no way to synthesise mouse input here; worth a click
   test in vim.

Known remaining gaps, none architectural: some color-emoji glyphs are missing
from the fallback chain (`🦀` draws as `□` while `📦` is fine), and kitty
graphics inside terminal mode are still a follow-on. My recommendation is to
re-judge how far from "native" it feels now, and only then consider a rewrite.
