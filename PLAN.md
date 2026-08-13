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
true.

| # | Item | Status | Covered by |
|---|------|--------|-----------|
| 7 | Rail: CONTEXT meter | **done** | `the context meter folds the harness's own usage`, `both rail panels draw what they folded` |
| 8 | Rail: FILES touched | **done** | `the files panel lists what was changed, newest first` |

**CONTEXT** answers the question nothing else in zodiac answers: how full is
the window, and therefore when should this session compact. It folds `usage`
off the message envelope — totals accumulate, but `context` is a *snapshot* of
the newest turn, since that is what occupancy means (summing it across turns
would be nonsense). The window size is read from the model id (200k, or 1M when
it carries the `[1m]` marker). Cost shows only when the harness reports a
non-zero one: subscription plans report zero, and a confident `$0.00` is worse
than no number.

**FILES** lists what the agent *changed* — Edit/MultiEdit/Write/NotebookEdit —
newest first, repeat edits counted, click to copy the path. Read is
deliberately excluded: the panel answers "what did it change", not "what did it
look at".

### Reported missing — three real bugs behind it

"Context and files seem to be missing from the right sidebar." Investigated by
dumping what the client actually folded from a *live* prompted pane
(`ZODIAC_GUI_DUMP_AGENT=1`, kept — it separates "the data isn't there" from
"the panel isn't drawing it", which the e2e can't do since it seeds its own
events). Three causes:

1. **Pi panes recorded no files at all.** Pi's tool calls take a different
   branch (`toolCall` with `arguments`, not `tool_use` with `input`) that never
   called the recorder. The matcher is now case-insensitive across a set of
   write-tool names and tries several path keys, so both harnesses land.
2. **Pi's token usage was silently read as zero.** Its bridge reports
   camelCase (`input`, `cacheRead`); only claude's snake_case was parsed.
3. **Claude's totals were double-counted, and its output undercounted.** The
   `result` event repeats the whole turn the assistant message already
   reported. Occupancy now comes from any message envelope; session totals only
   from the turn-final event (claude's `result`, pi's `message_end`) — an
   assistant message's output count is still growing when it is sent.

**And the design fault that made it unreportable:** both panels drew *nothing*
when they had no data, which is indistinguishable from a missing feature. They
now say "no turn reported yet" / "nothing changed yet". Covered by `the rail
panels announce themselves before any data`.

### Harness hygiene fixed along the way

Two of my own bugs, both found by the checks rather than by eye:

- The e2e **persisted the panel toggles to the user's real `config.json`**. A
  run that died before restoring left the user's panels collapsed *and*
  poisoned the next run, which then "restored" the wrong value. It no longer
  saves under `ZODIAC_GUI_E2E`.
- Each run **inherited every pane the previous runs had made** — 46 of them.
  Killing the server wasn't enough: the next one rebuilds its pane list from
  `state.json`. Cleanup now happens *before* connecting (the server rewrites
  that file as it shuts down, so cleaning at teardown raced the write).

Steps that meant "the shell pane" now pick it by **kind**, not by index 0:
`T_PANE_OPENED` moves `active` to each new pane as the server reports it, so
index-based picks were quietly racing and produced three different flaky
failures before the cause was clear.

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
