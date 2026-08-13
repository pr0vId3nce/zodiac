# Zodiac — plan status

_Updated 2026-08-13._ The request in this file's previous revision:

> When not on observatory page, please hide the topbar (containing the uptime,
> cpu, mem, pair phone, etc). Make the left sidebar hideable with CTRL+left
> arrow and the activity sidebar hideable with CTRL+right arrow.

All three are implemented and covered by the end-to-end harness. **26/26.**

## Running the e2e

```sh
nix develop --command bash -c \
  'ZODIAC_GUI_E2E=1 ZODIAC_GUI_EXIT_AFTER_MS=220000 ./target/debug/zodiac-gui e2etest'
```

It attaches to a scratch session, opens a shell pane and a real claude agent
pane (never prompted, so it spends no tokens), drives the **real handlers** and
asserts on what actually got laid out. PASS/FAIL per check; nonzero exit on any
failure.

It deliberately does *not* synthesise OS input: during this work a missed
compositor focus sent test keystrokes into an unrelated window, and a
screenshot-driven check "passed" while the thing under test was broken. The
harness drives `on_key_logical`, injects egui events, and reads small probes
recorded during the frame.

Checks are written so they can fail. Two were rewritten after passing
*vacuously* (a streaming check that fed one token per two seconds; an emoji
check that trusted `fc-match`, which always substitutes something). The newer
ones follow the same rule: the interrupt check is paired with an assertion that
an *idle* pane is **not** interrupted, and the panel checks assert the pty's
measured column count actually grew, not merely that a panel stopped drawing.

---

## This round

| # | Item | Status | Covered by |
|---|------|--------|-----------|
| 1 | Top bar hidden off the Observatory | **done** | `the top bar is drawn on the Observatory`, `…is hidden in the focused view` |
| 2 | Sidebar collapses on Ctrl+← | **done** | `ctrl+left collapses the sidebar and the pty reclaims it` |
| 3 | Activity rail collapses on Ctrl+→ | **done** | `ctrl+right collapses the activity rail too` |
| 4 | Both toggles restore | **done** | `the toggles restore both panels` |

**The top bar** is Observatory chrome — it belongs to the overview, not to a
pane you're working in, where it only costs 52px. The panels are *not drawn*
rather than drawn at zero width, which is what lets the central panel (and with
it the pty's measured grid) reclaim the space: in the harness a terminal pane
went 97 → 124 → 154 columns as the two panels closed.

Both toggles persist to `config.json`, so a panel you closed stays closed next
launch. Ctrl+←/→ are handled as **window-level chords** (`is_global_chord`) for
two reasons: egui reports every key as consumed while a text field holds focus,
and a terminal pane would otherwise *also* receive the keystroke.

Two consequences worth knowing:

- **Ctrl+←/→ no longer reach the shell inside a terminal pane**, so zsh's
  word-jump is shadowed there. That is the cost of the binding as specified;
  say the word and it can move to Ctrl+Shift+←/→.
- With the bar hidden, the window's own move/minimise/close controls are gone
  in the focused view — the WM handles it, and Alt+Z brings the bar back with
  the Observatory. Every *button* the bar carried has a key: ⌘K palette,
  ⌘, settings, Alt+O oracle, and **Alt+P pair-phone, which is new** — that one
  had no shortcut at all and would otherwise have become unreachable.

## Follow-on (same round)

| # | Item | Status | Covered by |
|---|------|--------|-----------|
| 5 | Alt+R renames a pane (TUI parity) | **done** | `alt+r opens the rename dialog on the current name`, `the rename reaches the server` |
| 6 | Output-rate chart removed from the rail | **done** | — |

The rename mirrors the TUI exactly, including **empty name = un-pin** (the
server resumes auto-naming). Alt+Shift+R (raise) moved into the same handler,
since one key can't be claimed in two places once Alt+R is a global chord.

The rail's output-rate histogram is gone. An agent idles for a long stretch and
then works for one, so the chart only ever showed a flat line or a spike —
motion where information was supposed to be. The small card sparkline on the
Observatory stays; at that size it reads as "this one has been busy", which is
true. **What replaces it in the rail is an open question** — see the
conversation; the rail currently carries the session facts and the PLAN panel.

## Standing items (unchanged)

- **pi slash commands** — the picker is harness-aware (`slash::commands_for`),
  but pi's built-ins are compiled into its binary with nothing on disk to read,
  so pi panes get no picker rather than a guessed list.
- **Emoji glyphs** — needs a *system* change, not a code change: install a
  monochrome emoji font (e.g. `noto-fonts-monochrome-emoji`). egui rasterizes
  outlines only, so `NotoColorEmoji` (CBDT bitmaps) is unusable to it. The
  client side verifies real coverage via `fc-list :charset=1f980`; the e2e
  check reports which case the machine is in.
- **Kitty graphics** — real placements are decoded and blitted over the grid;
  only the Unicode-placeholder (`virt`) tiling path and z-ordering under text
  are outstanding.

## Not planned

**Rewriting the terminal layer.** Every symptom that made panes feel non-native
was an ordinary bug — a wrong grid size, egui swallowing keys after Tab, mouse
coordinates mapped through stale geometry, a self-sustaining redraw loop, and
egui consuming chords while a text field had focus. All are fixed and covered.
