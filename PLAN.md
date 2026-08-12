# Zodiac — plan for the remaining work

_Written 2026-08-12. One list, ordered so that the things most likely to bite a
user come first and the speculative work comes last. Every item says how it
will be **verified**, because several past "fixes" here were wrong until they
were measured._

Ground rules carried over from the work so far:

- Reproduce before fixing. Three of the last four bugs had a plausible wrong
  explanation that a five-minute experiment killed.
- Every change stays gate-green (`nix develop --command ./scripts/check.sh`).
- Prefer a measurement over an argument. `ZODIAC_GUI_PERF_OFF`,
  `ZODIAC_GUI_SEED_ITEMS`, `--profile profiling`, and the headless selftest
  exist for exactly this.

---

## P0 — user-visible breakage

### 1. Wrapping user message escapes the transcript ✅ fixed, verifying

**Symptom (reported):** typing a message long enough to need a second line
sends it off the left edge, and the chat window slides under the pane sidebar.

**Cause:** inside `ScrollArea::show_viewport` with `auto_shrink([false,false])`
the content ui is effectively unbounded horizontally. `turn_user` sized its
bubble from `available_width()`, so the cap was huge, the text never wrapped,
and because the bubble lays out right-to-left the overflow ran off the *left*
edge and under the sidebar.

**Fix:** pin the scroll content to `viewport.width()`, and clamp the bubble to
the container. **Verify:** send a two-line message and confirm it wraps inside
the bubble with the transcript's left edge intact.

### 2. Confirm Alt+1–9 / Enter reliably opens a pane

Scripted testing repeatedly failed to open a pane, but the cause turned out to
be window focus landing elsewhere, not zodiac. Alt+1–9 now also switches to the
focused view, which is the right behaviour regardless.

**Verify:** by hand — Alt+2 from the Observatory, and Enter on a highlighted
card. If Enter is genuinely dead, the suspect is `observatory_nav`'s
`memory().focused().is_none()` guard.

---

## P1 — verification debt (fixes that landed but were never confirmed)

### 3. Terminal mouse mapping

`grid_cell` now maps through the geometry `terminal_view` actually draws with,
instead of the legacy wgpu grid's origin/cell. I could not synthesise mouse
input here.

**Verify:** open `vim` in a terminal pane, click a word mid-screen, confirm the
cursor lands there; repeat at a non-100% Terminal font size, which is where the
old mapping drifted worst.

### 4. Streaming-pane CPU

The perf work was measured against an *idle* seeded transcript. Streaming is
what pinned the repaint clock in the original profile, so the headline number
is an analogue, not the real case.

**Verify:** with a real agent streaming a long answer, sample
`ps -o %cpu,time -p $(pidof zodiac-gui)` before/after, and A/B against
`ZODIAC_GUI_PERF_OFF=1` on the same binary.

---

## P2 — the one open perf unknown

### 5. Idle transcript still costs ~8–10% of a core

Cost is now flat in transcript length (1600 vs 4800 items), but it does not
fall to idle, and sampling at t=25s rules out startup.

**Hypothesis (unproven):** a repaint feedback loop — measured item heights
change content height, which nudges the `stick_to_bottom` scroll area, which
requests another repaint; the 16ms clamp bounds it, so it reads as a fixed cost
rather than a runaway one.

**Plan:**
1. Instrument: count frames per second in `redraw()` behind an env flag. If it
   is pinned near 60fps while idle, the loop is real.
2. If real, break it: only write back a measured height when it differs by more
   than a pixel, and skip the scroll nudge when nothing changed.
3. If it is *not* a repaint loop, profile properly —
   `cargo build --profile profiling -p zodiac-gui` then
   `perf top -p $(pidof zodiac-gui)` — and follow the symbols.

**Verify:** idle CPU for a 1600-item pane drops toward the 0.2% the small pane
shows.

---

## P3 — features and polish, roughly by value

### 6. `/resume` in place, rather than opening a new pane

Today it spawns a pane with `--resume <id>`. Resuming *in* the pane needs a
server-side "respawn this pane's agent with a session id" path — the pieces
exist (`AgentRuntime.session_id`, the autoresume respawn), they just are not
reachable from a client frame.

**Plan:** add a `T_AGENT_RESPAWN`-style frame (pane id + session id), reuse the
autoresume respawn path, and switch the picker to it. **Verify:** resume in a
pane and confirm the pane keeps its id, name, and position.

### 7. Slash commands for pi panes

The picker is gated to the claude harness. Pi has its own command set.

**Plan:** teach `slash.rs` a per-harness source (pi's commands come from its own
config/extensions), and key the picker on the pane's harness rather than a
hardcoded `"claude"`. **Verify:** open the picker in a pi pane and run one.

### 8. Missing color-emoji glyphs in terminal panes

`🦀` draws as `□` while `📦` is fine, so the fallback chain has gaps.

**Plan:** find which face supplies the working emoji and why the other misses —
`font.rs::egui_fallback_fonts` deliberately skips color-emoji faces, so the
likely fix is adding a monochrome emoji face that covers the missing ranges, or
relaxing that skip for the terminal grid. **Verify:** a prompt containing both
glyphs renders both.

### 9. Finish sound on agent completion

`finish_sound` and `protocol::play_sound` exist; the only call site is a preview
when the setting changes. Nothing plays when an agent finishes.

**Plan:** track each pane's previous status in `GuiApp`; on a working→done edge
for a pane that is *not* focused, play the sound; debounce so a flapping status
cannot machine-gun it. **Verify:** on-device with audio; confirm silence when
the pane is already focused and when the setting is "off".

### 10. Kitty graphics inside terminal mode

Long-standing: the grid draws text and colors but not images. This is the one
genuinely architectural item — it needs the wgpu image pipeline composited into
the egui terminal. Worth scoping only after everything above is done.

---

## Explicitly not planned

**Rewriting the terminal layer.** Every symptom that made panes feel non-native
turned out to be an ordinary bug — a wrong grid size, egui swallowing keys after
Tab, mouse coordinates mapped through stale geometry. All three are fixed. The
recommendation is to judge how native it feels now and only revisit this if
something concrete still misbehaves.
