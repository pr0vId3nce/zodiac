# 0004: GUI stack — winit + wgpu + glyphon, softbuffer CPU fallback

- **Status:** accepted
- **Date:** 2026-08-09
- **Spike/timebox:** Spike S3 (roadmap Phase 3, timebox 5 days; measurement
  portion done in ~1 day, offscreen — scratch prototype kept as reference)

## Context

Phase 3 needs a GUI client that renders a 120x50 SGR-attributed grid plus kitty
image blits at 60 fps on Linux at one-person maintenance cost (roadmap Spike
S3). The candidate stack was winit + wgpu + cosmic-text via glyphon, with
softbuffer as the no-GPU fallback. Measured on the primary dev machine
(ThinkPad P14s Gen1, Renoir iGPU, RADV Mesa 26.1.5, Wayland).

## Options considered

**winit + wgpu + glyphon (chosen).** Prototype: offscreen render-to-texture
1152x1000, 600 frames, pathological load (all 6000 cells re-shaped and redrawn
every frame; 256-color + RGB fg/bg; bold/italic mix; per-cell bg rects;
one 512x512 RGBA alpha-blended quad). Results:

| scenario                        | mean    | p50   | p95    |
|---------------------------------|---------|-------|--------|
| text-only grid                  | 13.3 ms | 12.8  | 14.2   |
| grid + 512x512 image quad       | 12.8 ms | 12.7  | 13.9   |
| atlas churn (~5.8k glyphs/frame)| 117.6ms | 111.8 | 146.7  |

Breakdown (text): cosmic-text shaping 10.7 ms, glyphon prepare 1.0 ms, GPU
submit+wait 1.6 ms. `Shaping::Basic` drops the frame to 9.0 ms. Peak RSS
74 MiB. First frame ~110 ms (atlas warmup). Real window: Wayland handle,
Vulkan/RADV, presented OK. Cold build 2m24s, 391 cargo-tree lines / 295
packages. Softbuffer CPU path on the same load: 14.8 ms text / 14.9 ms with
image / 19.9 ms churn, 44 MiB RSS — viable fallback.

Findings that shape the design:
- The GPU is never the bottleneck (1.6 ms for 6000 rects + 6000 glyphs +
  blit). Shaping per-cell spans is 80% of the frame; a shaped-line cache
  (mutate only damaged BufferLines) is the damage strategy that matters.
  Pixel-damage / partial present is optional: full redraw already makes
  60 fps.
- `atlas.trim()` per frame forces full re-rasterization (44.5 ms/frame — 3x).
  Trim only on font change / pane close / idle.
- Atlas churn worst case is capacity (AtlasFull → trim+retry), not speed; it
  needs ~5.8k unique glyphs *per frame* to trigger — unreachable in real use.
- Row `\n` must be covered by a text span or the whole grid shapes as one
  line (silently renders row 0 only) — baked into the prototype.

**softbuffer + cosmic-text (CPU) — kept as fallback, not primary.** Ships as
a feature-gated second render path for no-GPU environments.

**wgpu without glyphon (hand-rolled atlas).** Not prototyped. glyphon 0.12 is
small and its measured overhead above raw wgpu is ~1 ms/frame. Copy
Rio/sugarloaf and Zed atlas *patterns* only if glyphon becomes a limit.

## Decision

zodiac-gui v1 renders with **winit 0.30.13 + wgpu 30.0.0 + glyphon 0.12.0
(cosmic-text 0.19.0)**, pinned exactly; **softbuffer 0.4.8** + cosmic-text as
a feature-gated CPU fallback. One Device/Queue/Surface; all panes in one
render pass (bg-rect instances → glyphon text → image quads → cursor overlay,
per-pane scissor). Damage = shaped-line cache: re-shape only damaged
BufferLines; present-on-demand on server frames (AutoVsync). `T_GFX_IMG`
chunks decode (png crate for f=100, raw for f=24/32) into one RGBA8 texture
per image id, LRU under the existing 64 MiB/pane quota; `VisPlacement` crops
map to quad UVs. HiDPI: shape at device pixels using the winit scale factor.
Wayland is the verified primary path; X11 rides the same winit code. Never
trim the atlas per frame. Linux only for v1. NixOS: add `vulkan-loader` to
the devshell/runtime closure (the spike needed an explicit LD_LIBRARY_PATH
for libvulkan.so.1).

## Revisit when

- Real workloads (Phase 3.7 daily driving) show shaped-line caching missing
  60 fps on panes > 200x60, or AtlasFull occurs outside synthetic tests.
- wgpu major-version churn costs more than a day per upgrade twice in a row
  (then consider vello/sugarloaf or freezing on an LTS-ish wgpu).
- macOS support becomes a goal (winit/wgpu keep it open; softbuffer too).
