# zodiac-gui

The GUI client for zodiac: a third client on the session socket, sharing
`zodiac::client_core` with the TUI.

**Native rebuild (ADR 0006).** Following the "TUI → GUI Overhaul" design
handoff, the client is a native **egui 0.36** app painted into the existing
wgpu-30 surface (egui tracks our exact `wgpu =30.0.0` pin, so the kitty grid
renderer stays intact). egui owns the screens — Observatory, focused pane
(sidebar + native transcript + activity rail), command palette, settings,
pair-phone, oracle — and the original wgpu grid renderer (ADR 0004: winit
0.30.13 + wgpu 30.0.0 + glyphon 0.12.0) is reused as the per-pane **terminal
mode**.

```sh
nix develop --command cargo run --release -p zodiac-gui [session]   # default: main
```

## Screens & keys

- **Observatory** (home) — a responsive card grid of the session's panes from
  live `T_STATE`: sigil tile, agent+version chip, cwd, one-line subtitle,
  transcript-tail well, status pill, left status rail. Click a card to open it.
- **Focused pane** — sidebar (pane list, click to switch) · main · activity
  rail. The view follows the pane **kind** (shown as a chip in the header):
  agent panes are headless structured NDJSON — no pty — so they render the
  **transcript**; pty panes (shells, or a TUI you launched) render the live
  **terminal** grid. There is no toggle: each kind has exactly one real view,
  so neither can land on an empty black screen. The transcript renders agent
  turns with the Claude-Code feel: user bubbles, `⏺` assistant recaps with
  **Markdown** (headings, lists, quotes, **bold**/*italic*/`code`, links) and
  fenced **code in its own boxes**, **expandable tool boxes** (full command +
  collapsed output, red on error), **collapsible thinking** panels plus the live
  orange "Cogitating…" sayings and spinner, and the streaming tail. The composer
  sends to the agent (Enter / **Send** → `T_AGENT_INPUT`). A pending permission
  raises a **modal question popup** navigable by mouse, number keys, ↑/↓ (or
  j/k), and Enter (Esc denies) → `T_PERM_RESP`. When the agent is running a plan
  (`TodoWrite`), the activity rail shows a **PLAN** panel with a progress bar and
  the checklist (done struck-through, the in-progress step highlighted). **Esc**
  returns.
- **⌘K / Ctrl+K** — command palette (fuzzy pane jump; ↑/↓, Enter, Esc).
- **⌘, / Ctrl+,** — settings (grouped; edits real `config.json` keys, persists).
- **Alt+O** — the Oracle panel (gradient orb; presentational for now).
- **Alt+N** — new agent (structured) pane; **Alt+Shift+N** — new shell pane.
  **Alt+←/→** (or **Alt+↑/↓** with side tabs) and **Alt+1–9** switch panes;
  **Alt+W** closes the active pane.
- **Copy / paste** — in the **transcript**, drag to select across turns and
  **Ctrl/⌘+C** copies; paste into the composer with **Ctrl/⌘+V**. In the
  **terminal**, drag to select (hold **Shift** to select even when a TUI is
  grabbing the mouse) — release copies, or **Ctrl/⌘+Shift+C**; paste with
  **Ctrl/⌘+Shift+V**, **⌘+V**, or **Shift+Insert** (bracketed when the app
  asked for it). `Ctrl+C`/`Ctrl+V` still reach the shell as SIGINT / literal.
- Title-bar buttons: **⌘K find pane**, **oracle**, **pair phone**, **settings**.
  Pair-phone renders the astrolabe pairing QR from the endpoint + token.

## How it renders

- **Attach**: `connect_or_spawn(session)`, then the gfx-capable `T_ATTACH`
  payload (`[1, cell_w, cell_h]` in px), so panes' PTYs report pixel
  dimensions and the server mirrors kitty graphics. Frames are read on a
  socket thread and injected into the winit loop; per-pane state is the same
  `CPane` the TUI uses.
- **egui layer**: each redraw runs `egui::Context::run_ui` and hands the
  tessellated jobs to `Renderer::paint_egui`, which uploads texture deltas,
  builds egui's buffers, clears to the themed backdrop, and renders into the
  same wgpu surface. Window events feed egui first (`egui_winit`), so a
  focused widget's keys don't leak to a pane's pty. The `WaitUntil` timer is
  driven by egui's `repaint_delay`.
- **Terminal mode** (the legacy ADR-0004 path): the pane's vt100 screen —
  per-cell bg quads, glyphs, underline, block cursor — reusing
  `palette::cell_colors` for the xterm-256 + SGR fold. The instanced-rect +
  textured-quad + glyphon machinery in `render.rs` is retained for this and
  for kitty-graphics compositing (a follow-on: kitty images aren't yet drawn
  inside terminal mode).
- **Design tokens** (`theme.rs`): the handoff's ground/chrome/text/accent
  palette folded into egui `Visuals`, with the five status colors carried
  verbatim from `src/theme.rs` (thinking → violet, idle text override).

## Fonts

**egui UI/screens**: rendered in **JetBrains Mono Nerd Font** — resolved via
`fc-match` and loaded into egui as both the proportional and monospace family
(so the Nerd glyphs cover the UI's symbols). `ZODIAC_GUI_UI_FONT=<family>`
overrides it; if neither resolves, egui keeps its built-in font.

**Terminal mode** (the wgpu grid): the system font database is loaded via
fontconfig (cosmic-text's default). The monospace family is chosen by, in
order: `ZODIAC_GUI_FONT=<family>`, `fc-match monospace`, the first monospaced
face known to fontdb. Cell width is the *measured* advance of that face at
15 px (× scale factor) — the same number cosmic-text lays rows out with, so
bg quads and glyphs stay aligned across wide rows.

## NixOS / running

The devshell (`flake.nix`) carries `vulkan-loader`, `wayland`, and
`libxkbcommon` and exports an `LD_LIBRARY_PATH` including
`/run/opengl-driver/lib` (the Vulkan ICDs), so `cargo run -p zodiac-gui`
works from `nix develop` with no extra setup. Outside the devshell you need
those libraries on `LD_LIBRARY_PATH` yourself.

Testing hook: `ZODIAC_GUI_EXIT_AFTER_MS=<ms>` detaches and exits after the
delay (used by unattended smoke tests).

## Known gaps / deferred (native rebuild)

- **Kitty graphics inside terminal mode** — the grid shows text + colors but
  not images; compositing the wgpu image pipeline into egui is a follow-on.
- **Terminal-mode pty resize** — renders the pane's existing screen size; the
  pty isn't yet resized to the terminal widget.
- **Output-rate sparklines / activity histogram** — blocked: `PaneState`
  carries no rate buckets (needs a server/protocol addition).
- **Instrument Sans** — the UI uses egui's default proportional font; the OFL
  TTF isn't vendored yet (not installed system-wide).
- **Raise-the-last-session** dialog — the GUI doesn't receive the restore
  snapshot, so it's not built.
- **Oracle chat** — the panel is presentational; send/receive wiring is a
  follow-on.
- **Custom window chrome** — the app draws its own title bar but keeps the OS
  titlebar (removing it would strand min/max/close on non-tiling WMs until
  custom controls land).
- **Settings**: full 33-row parity and the Motion slider (no config key) are
  pending; a curated functional subset persists today.

## Fonts

- `ZODIAC_GUI_FONT` — monospace family for grid panes (else `fc-match monospace`).
- `ZODIAC_GUI_UI_FONT` — proportional family for agent transcripts (roadmap 4.5;
  else `fc-match sans-serif`, else the monospace family). Grid panes are always
  monospace.
