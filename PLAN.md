# Zodiac — plan status

_Updated 2026-08-12. Every item below is now either fixed and covered by the
end-to-end harness, or has an explicit reason it isn't._

## Running the e2e

```sh
nix develop --command bash -c \
  'ZODIAC_GUI_E2E=1 ZODIAC_GUI_EXIT_AFTER_MS=140000 ./target/debug/zodiac-gui e2etest'
```

It attaches to a scratch session, opens a shell pane and a real claude agent
pane (never prompted, so it spends no tokens), drives the **real handlers** and
asserts on what actually got laid out. It prints PASS/FAIL per check and exits
nonzero on any failure. **16/16 passing.**

It deliberately does *not* synthesise OS input: during this work a missed
compositor focus sent test keystrokes into an unrelated window, and a
screenshot-driven check "passed" while the thing under test was broken. The
harness drives `on_key_logical`, injects egui events, and reads small probes
recorded during the frame.

Two of the checks were rewritten after they passed *vacuously* — a streaming
check that fed one token per two seconds, and an emoji check that reported
DejaVu Sans as an emoji font because `fc-match` always substitutes something.
A check that cannot fail is worse than no check.

---

## Status

| # | Item | Status | Covered by |
|---|------|--------|-----------|
| 1 | Wrapping user message escapes the transcript | **fixed** | `long user message wraps inside the transcript` |
| 2 | Alt+1–9 / Enter open a pane | **fixed** | `alt+1…`, `alt+2…`, `Enter on the Observatory…` |
| 3 | Terminal mouse mapping | **fixed + verified** | `terminal mouse maps to the drawn cell` |
| 4 | Streaming-pane CPU | **fixed** | `a large streamed transcript does not pin the repaint clock` |
| 5 | Idle transcript burning ~8–10% of a core | **root-caused + fixed** | `idle pane does not repaint continuously` |
| 6 | `/resume` in place | **implemented** | `/resume resumes in place (no extra pane)` |
| 7 | Slash commands for pi panes | **mechanism done; pi not enumerable** | see below |
| 8 | Missing emoji glyphs | **client side done; needs a system font** | `emoji fallback is a face egui can rasterize` |
| 9 | Finish sound on completion | **implemented** | `finish sound fires once on working->done` |
| 10 | Kitty graphics in terminal mode | **already implemented** | see below |
| 11 | Shift+Tab permission modes | **implemented** | `shift+tab cycles…`, `the harness confirms…` |

### 5 — the idle CPU burn, finally explained

It was a feedback loop in the event plumbing, not the animation timer.
`egui_winit` answers `WindowEvent::RedrawRequested` with `repaint: true`; we
honoured that by requesting another redraw — asking for the next frame from
inside the frame being drawn. The app then rendered at vsync forever with no
animation and no input. The 16 ms clamp bounded the loop but could not stop it,
which is exactly why the cost looked like a fixed floor rather than a runaway.

Found by measuring: a `#[track_caller]` tally on `request_redraw` plus a
window-event histogram, both behind `ZODIAC_GUI_DEBUG_REPAINT=1` and kept for
next time. **Idle: 147 frames / 2 s before, 2 after.**

### 7 — pi slash commands

The picker is now harness-aware (`slash::commands_for`) instead of hardcoding
claude, so another harness lights up the moment its commands are discoverable.
Claude's set is enumerable — built-ins, `~/.claude/commands`, project commands,
skills. **Pi's built-ins are compiled into its binary with nothing on disk to
read**, so pi panes get no picker rather than a guessed list that might not
work. Wiring pi up needs a way to ask pi for its commands.

### 8 — emoji glyphs

egui rasterizes outlines only, so `NotoColorEmoji` (CBDT bitmaps) is unusable
to it, and it is the **only** emoji font installed on this machine. That is why
`🦀` draws as `□` while `📦` (in egui's built-in subset) is fine. The client
side is fixed — the fallback search tries several monochrome families and
verifies real coverage of U+1F980 via `fc-list :charset=1f980` rather than
trusting `fc-match`, which substitutes silently.

**This one needs a system change, not a code change:** install a monochrome
emoji font (e.g. `noto-fonts-monochrome-emoji`) via home-manager. The e2e check
reports which case the machine is in.

### 11 — Shift+Tab permission modes

Shift+Tab cycles **manual → auto → plan → bypass** in a structured claude pane,
with the mode shown as a chip in the header next to the model.

Claude Code accepts `set_permission_mode` at runtime over its control protocol
(verified against the CLI before building on it: the request returns success
and the session then reports `permissionMode` on a `system` status event), so
switching keeps the conversation instead of restarting the harness. The chip
renders what the *harness* reports, not what zodiac asked for, so a rejected
switch cannot leave it lying. Shift+Tab is consumed in the egui frame because
egui treats it as reverse focus traversal and the composer usually holds focus.

Pi has no equivalent control request, so the chip and the shortcut are claude
-only rather than silently doing nothing.

### 10 — kitty graphics

Already implemented: real placements are decoded to egui textures and blitted
over the grid in `terminal_view`. The README's "not drawn inside terminal mode"
was stale, like the pty-resize gap removed earlier. Outstanding is only the
Unicode-placeholder (`virt`) tiling path and z-ordering under text.

---

## Not planned

**Rewriting the terminal layer.** Every symptom that made panes feel non-native
was an ordinary bug — a wrong grid size, egui swallowing keys after Tab, mouse
coordinates mapped through stale geometry, and the redraw loop above. All are
fixed and covered by the harness.
