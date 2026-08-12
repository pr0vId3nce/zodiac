# zodiac-gui handoff — the GUI burns a full CPU core

_Written 2026-08-11 from the macbook, for **NTP424** (linux box). Diagnosis only
— **no code changed**, nothing committed. `git pull` gets you nothing new; this
file is the whole delivery._

## TL;DR

`zodiac-gui` pegs a core while agents are streaming. Two independent causes,
both in **portable code** — this is **not a macOS-only problem, Linux has it
too**. Neither is in a `#[cfg(target_os = ...)]` block.

1. **Every frame re-lays-out the entire transcript from scratch.**
   `transcript_view` (`src/ui.rs:1196`) walks *every* `ChatItem` through
   `ScrollArea::show` — nothing virtualized — and `md_line` (`src/ui.rs:1430`)
   re-parses inline Markdown into fresh `String`s and a brand-new `LayoutJob`
   per line, per frame. Cost grows with transcript length. **This is the
   dominant cost — fix this one first.**
2. **Nothing caps the frame rate.** `src/app.rs:1231` maps egui's zero repaint
   delay to `Some(Instant::now())`, and `about_to_wait` (`src/app.rs:1550`)
   turns that into `ControlFlow::WaitUntil(now)` — which fires immediately.
   Any zero-delay repaint request free-runs the loop as fast as the CPU allows.

## Evidence

Live process on the mac: PID 68312, **27:56 of CPU in 53:32 wall**. First
`sample(1)` caught it at **100%** (a full core), a second a few minutes later at
**41%** — it scales with how much agent output is streaming, it is not a fixed
spin.

From two `sample` runs against the running process:

- The main thread is in the winit/CFRunLoop redraw callback in **~97% of
  samples**. It is redrawing continuously, not sleeping.
- The cost is **CPU-side, not GPU**: only ~7 of 2086 samples touch
  Metal/QuartzCore.
- **~35–40% of all samples are `nanov2_malloc` / `_nanov2_free` /
  `memmove`** — per-frame allocation churn. That is the signature of cause #1:
  Markdown re-parse + `LayoutJob` rebuild for the whole transcript, every frame.

Session context that makes it bite: `main` has **7 agent panes** with
multi-megabyte scrollbacks (`~/.local/state/zodiac/main/scrollback/0.bin` alone
is 6.5 MB).

### Confidence — read this before you trust the attribution

The **profile shape is measured and solid** (redraw-bound, CPU-bound,
allocation-dominated). The **attribution to specific functions is read from the
source, not from symbols.** `Cargo.toml:42` sets `strip = true`, so the running
binary has no symbol table. I built an unstripped copy, but its `__text` differs
by 528 bytes from the running binary and spot-checked addresses symbolicated to
nonsense, so I did not use it.

I deliberately did **not** attach a second GUI client to symbolicate live: any
client's `T_RESIZE` is applied to **all** panes (`src/server.rs:612`), which
would have reflowed seven working agents. **If you reproduce on linux, do it
against a scratch session, not `main`, for the same reason.**

Cheap on your side: build with `strip = false`, run `zodiac-gui scratch`, and
`perf top -p $(pidof zodiac-gui)`. That gives named frames in seconds and will
confirm or correct the two attributions above.

## Why vsync doesn't save you (same on both platforms)

`src/render.rs:340` sets `PresentMode::AutoVsync` → Fifo everywhere. When frames
are cheap, a blocking `get_current_texture()` gives you a de facto cap on macOS
*and* Linux. But that backpressure vanishes exactly when it's needed: once CPU
frame time exceeds the refresh interval, the GPU is always ahead, the acquire
never blocks, and the loop free-runs on CPU cost alone. That's what the profile
shows — near-zero time anywhere near the present path.

## Linux specifics

Every `#[cfg(target_os = "macos")]` in `app.rs`/`ui.rs` is cosmetic: the ⌘⇧[
tab chord, transparent titlebar, traffic-light spacing, the ⌘-vs-Ctrl label
macro, the menu bar. **None are in the frame path.** `WaitUntil(Instant::now())`
fires immediately on X11 and Wayland just as it does on the macOS runloop.

Where linux differs, none of it in your favor:

- **Software rendering** (llvmpipe, VMs, remote sessions) makes the present path
  slower too — worse, not better.
- **Occlusion**: redraws are timer-driven, not driven by Wayland frame
  callbacks, so a hidden or minimized window keeps burning CPU. A
  frame-callback-driven design would idle for free on Wayland; this one doesn't.
- The only Mac-flavored thing in the profile is per-frame
  `-[CAMetalLayer setPixelFormat:]` / `setNonDefaultColorspace:` — 7 samples,
  0.3%. Noise, ignore it.

## Suggested fixes, in payoff order

1. **Virtualize the transcript** — `ScrollArea::show_rows` / `show_viewport` in
   `transcript_view` (`src/ui.rs:1196`) so only on-screen items are laid out.
   Biggest win; decouples cost from transcript length. Watch out for
   `stick_to_bottom(true)` interaction and for variable-height items — items
   here are not uniform, so `show_viewport` with cached heights is likelier to
   fit than `show_rows`.
2. **Cache per-item layout** — completed turns are immutable. Parse Markdown and
   build the `LayoutJob` once per `ChatItem` instead of every frame
   (`src/ui.rs:1430`). Only the live tail (`think_stream`, `stream`) needs
   rebuilding. Complements #1: #1 bounds *how many* items, #2 bounds *how often*.
3. **Clamp the frame rate** — floor the zero-delay case at ~16 ms (60 fps)
   instead of `Instant::now()` (`src/app.rs:1231`). One line; caps the worst
   case even if a repaint source misbehaves. Cheapest change here, so it's a
   reasonable thing to land first even though it treats the symptom.
4. **Throttle when unfocused/occluded** — skip animation repaints when the
   window isn't visible.

Repaint sources that keep the loop alive during normal use, for reference:
transcript `stick_to_bottom(true)` while output streams; thinking spinner
(`src/ui.rs:1236`, `src/ui.rs:1653`, 80 ms); the Oracle orb (`src/ui.rs:618`,
66 ms); tab spinner (`src/render.rs:1029`, 33 ms).

## How to verify a fix

Idle-ish baseline first, then with an agent actively streaming — the bug only
shows under streaming:

- `ps -o %cpu,time -p $(pidof zodiac-gui)` before/after.
- `perf top -p $(pidof zodiac-gui)` — the malloc/free/`memmove` cluster should
  stop dominating.
- Scale check: open a pane with a **long** transcript vs a fresh one. Post-fix,
  CPU should be roughly flat between them. Pre-fix it is not — that difference
  is the whole bug.
