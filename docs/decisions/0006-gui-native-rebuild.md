# 0006: Native GUI rebuild on egui, over the existing wgpu surface

- **Status:** accepted
- **Date:** 2026-08-09
- **Spike/timebox:** dependency-resolution spike (crates.io resolution against the
  frozen ADR-0004 pins) — 30 min, held.

## Context

The `zodiac-gui` client shipped as a faithful *terminal-grid viewer* (ADR 0004:
winit 0.30.13 + wgpu 30.0.0 + glyphon 0.12.0): it draws the active pane's vt100
grid, a tab strip, a status bar, and composites the kitty-graphics mirror. The
"Zodiac TUI → GUI Overhaul" design handoff instead specifies a **full native
desktop app** — seven screens (Observatory, Focused pane with sidebar + activity
rail, Oracle, Command palette, grouped Settings, Raise-session, Pair-phone) with
proportional typography, rounded cards, gradients, sliders/toggles, custom window
chrome, and a native agent transcript, with the raw terminal grid demoted to a
per-pane "terminal mode" toggle. The owner chose the full rebuild. That needs a
widget/layout toolkit the grid renderer does not have; this ADR picks it.

The binding constraint is the ADR-0004 pin set: `wgpu =30.0.0`, `glyphon =0.12.0`,
`winit =0.30.13`. wgpu 30 is very new, and the kitty-graphics renderer is written
against it. A UI toolkit that brings its own, different wgpu cannot share our
surface — we'd run two wgpu versions or downgrade and lose the graphics path.

## Options considered

- **egui 0.36 over our surface** — resolved a scratch crate with our exact pins
  plus `egui = "0.36"`, `egui-wgpu = "0.36"`, `egui-winit = "0.36"`. Result:
  `egui/egui-winit/egui-wgpu` all `0.36.1`, pulling `wgpu 30.0.0`, `winit
  0.30.13`, `glyphon 0.12.0`, `cosmic-text 0.19.0` — a **single** egui in the
  tree, no duplicate wgpu. `egui-wgpu 0.36.1` depends on `wgpu 30.0.0` exactly.
  So egui paints into our existing device/queue/surface; the grid + kitty
  pipelines stay verbatim for terminal mode; one process, pure Rust, still
  sharing `zodiac::client_core`. Immediate-mode: pixel-exact custom layout is
  hand-rolled but total-control (fits the high-fidelity spec); gradients (the
  amber mark, the Oracle orb) need custom painting/mesh.
- **Iced 0.14** — `iced_wgpu 0.14.0` resolves `wgpu 27.0.1`, a hard conflict
  with our `=30.0.0` pin. Adopting it means downgrading wgpu (breaking glyphon
  0.12 and the kitty renderer) or a second wgpu context. Rejected on the pin.
- **Tauri / Electron + web** — no wgpu conflict (OS webview), and the handoff's
  CSS maps 1:1 to the design. But the terminal grid + kitty compositing would
  have to be reimplemented in the webview (xterm.js + bespoke kitty handling) or
  live in a second native window; two languages + IPC; largest departure from
  the current single-process Rust client. Rejected: throws away the graphics
  path and `client_core` reuse.

## Decision

Rebuild `zodiac-gui` as a native app with **egui 0.36** (`egui`, `egui-winit`,
`egui-wgpu` all `0.36`) rendered **into the existing wgpu 30 surface**, keeping
`winit =0.30.13` / `wgpu =30.0.0` / `glyphon =0.12.0`. egui owns the seven
chrome/widget screens and the native agent transcript; the current instanced-rect
+ textured-quad + glyphon grid renderer is retained as the per-pane **terminal
mode**, drawn in the same frame (egui via `egui_wgpu::Renderer`, our passes around
it). Status semantics, `client_core`, the protocol, and config keys are unchanged;
the five status colors come verbatim from `src/theme.rs`. Fonts: Instrument Sans
(UI) + the system monospace (grid/code), loaded into egui's font set.

## Revisit when

egui drops a release that no longer tracks our wgpu pin (forcing a wgpu bump that
the kitty renderer can't follow), or the immediate-mode model proves too costly
for the transcript/observatory at scale (measure against the S3 pathological
load) — at which point Iced-with-a-matched-wgpu or a bespoke retained layer is
back on the table.
