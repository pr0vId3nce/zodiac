# Icebox

Ideas deliberately deferred. One line each. Promoting an item means giving it a
roadmap task in a future phase — never slipping it into the current one.

- Sixel and iTerm2 inline-image protocols.
- TUI-client animation passthrough to kitty hosts (GUI plays animations; TUI shows frame 0).
- Zero-downtime server restarts (per-pane PTY-holder shim processes, socket-passed fds).
- Transcript search across agent panes.
- Drag-and-drop content attachment (v1 sends paths only).
- MCP management UI.
- Kitty file-transfer and desktop-notification protocol extensions.
- ACP (Agent Client Protocol) support as an alternative structured-agent transport.
- Resize reflow in the VT engine (kept vt100, which has none; alacritty_terminal
  has it always-on — relevant if the escape hatch is ever taken).
- OSC 8 hyperlink storage per cell (currently parse+drop; GUI hover/click era).
- Undercurl / underline-style / underline-color (58/59) rendering — parsed and
  consumed since Phase 1; storage + rendering is GUI-era work.
- IRM insert mode (CSI 4 h) — reset is consumed, set still logs as unhandled.
- Focus reporting (DECSET 1004) passthrough to clients.
- DECALN (ESC # 8) screen alignment test.
