# zodiac-gui

The GUI client for zodiac (roadmap Phase 3, tasks 3.3–3.6): a third client
on the existing session socket, sharing `zodiac::client_core` with the TUI.
Rendering stack per `../docs/decisions/0004-gui-stack.md`, pinned exactly:
winit 0.30.13 + wgpu 30.0.0 + glyphon 0.12.0 (cosmic-text 0.19.0).

```sh
nix develop --command cargo run --release -p zodiac-gui [session]   # default: main
```

## What v1 does

- **Attach**: `connect_or_spawn(session)`, then the gfx-capable `T_ATTACH`
  payload (`[1, cell_w, cell_h]` in px from the measured font metrics), so
  panes' PTYs report pixel dimensions and the server mirrors kitty graphics.
  Frames are read on a socket thread and injected into the winit loop;
  per-pane state is the same `CPane` the TUI uses.
- **Grid renderer (3.3)**: the active pane's vt100 screen, full-window minus
  a 1-line tab bar and 1-line status bar. Per-cell bg quads (instanced rect
  pipeline), glyphon text with the xterm-256 palette and bold / italic /
  dim / underline / inverse mapping, block cursor. Damage-driven: text is
  re-shaped only when the cell signature changes; redraws happen on server
  frames / input / resize, never on a timer; the atlas is never trimmed per
  frame (S3 findings).
- **Input (3.4)**: winit keys → crossterm `KeyEvent` → `encode_key`
  (honoring the pane's application-cursor mode) → `T_INPUT`. IME commits
  are sent as utf8. Wheel/click go through `encode_mouse` honoring the
  pane's mouse protocol mode/encoding (→ `T_MOUSE`, or `T_INPUT` for old
  servers), with the alternate-scroll arrow fallback and local scrollback
  otherwise. **Ctrl+PageUp / Ctrl+PageDown switch panes** (same spirit as
  the TUI's Alt bindings, but chosen to never collide with pty input);
  clicking a tab focuses it. Everything else goes to the pane.
- **Graphics blit (3.5)**: the first actual pixel decode in zodiac —
  `T_GFX_IMG` payloads (format 100 = PNG via the `png` crate, 24/32 raw,
  zlib honored via `flate2`) become RGBA8 textures cached by
  (pane, img, ver), drawn per `VisPlacement` with source-rect crops,
  z < 0 under the text pass, z ≥ 0 over it, clipped to the grid.
- **Agent panes**: transcript rendered as text (❯ user, ✦ assistant,
  ⏺ tool, ✗ error) with the streaming tail, a local one-line prompt
  (Enter → `T_AGENT_INPUT`), and a centered permission modal answered with
  y/n (→ `T_PERM_RESP`).
- **Chrome (3.6)**: tab bar with status dots (working / needs_input / done /
  idle) and active highlight; status line with session, pane title, grid.

## Keys / settings

- **Ctrl+PageUp / Ctrl+PageDown** — previous / next pane.
- **Alt+1 … Alt+9** — jump straight to a pane by number.
- **Ctrl+S** — open the fullscreen settings page (Esc closes). ↑/↓ select a
  row, ←/→ or Enter cycle its value; changes persist to
  `~/.config/zodiac/config.json` and apply live.
- Current settings: **Pane tabs** — `top` (a bar across the top) or `side`
  (a left column, like the TUI sidebar). A placeholder page; more settings
  land here over time. The `gui_tabs` key is GUI-only (the TUI/server
  ignore it).

## Fonts

The system font database is loaded via fontconfig (cosmic-text's default).
The monospace family is chosen by, in order: `ZODIAC_GUI_FONT=<family>`,
`fc-match monospace`, the first monospaced face known to fontdb. Cell width
is the *measured* advance of that face at 15 px (× scale factor) — the same
number cosmic-text lays rows out with, so bg quads and glyphs stay aligned
across wide rows.

## NixOS / running

The devshell (`flake.nix`) carries `vulkan-loader`, `wayland`, and
`libxkbcommon` and exports an `LD_LIBRARY_PATH` including
`/run/opengl-driver/lib` (the Vulkan ICDs), so `cargo run -p zodiac-gui`
works from `nix develop` with no extra setup. Outside the devshell you need
those libraries on `LD_LIBRARY_PATH` yourself.

Testing hook: `ZODIAC_GUI_EXIT_AFTER_MS=<ms>` detaches and exits after the
delay (used by unattended smoke tests).

## Not in v1 (known gaps)

- `cpu-render` softbuffer fallback (ADR 0004 stretch goal) — GPU path only.
- Selection/clipboard, pane create/close/rename/move from the GUI, the home
  page, zoom, settings, chat panel — the TUI remains the full-featured
  client; the GUI is a viewer/driver of existing panes.
- Shaped-line (per-BufferLine) caching: damage granularity is per-screen,
  which already holds 60 fps at the S3 pathological load.
- Proportional fonts for agent transcripts (Phase 4).

## Fonts

- `ZODIAC_GUI_FONT` — monospace family for grid panes (else `fc-match monospace`).
- `ZODIAC_GUI_UI_FONT` — proportional family for agent transcripts (roadmap 4.5;
  else `fc-match sans-serif`, else the monospace family). Grid panes are always
  monospace.
