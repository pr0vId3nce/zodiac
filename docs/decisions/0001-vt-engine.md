# ADR 0001 — VT engine: extend the vendored vt100 (Spike S1)

Status: accepted · 2026-08-09 · roadmap Phase 1, tasks 1.3/1.4

## Context

Phase 1 must decide whether porting zodiac's instrumentation (TermEvent stream,
query replies, mode introspection) onto `alacritty_terminal` or `wezterm-term` is
cheaper over 12 months than closing the vendored vt100's gaps. Two source-level
research passes and a measurement harness (scratch project feeding the full Phase 0
corpus through each candidate at recorded size, checkpoint schedule identical to
`tests/golden.rs`) produced the evidence below.

## Findings

**Corpus fidelity** (7 recordings, 8 checkpoints + final each, vs the vt100 goldens):

| metric | alacritty_terminal 0.26.0 | wezterm-term (git HEAD) |
|---|---|---|
| text lines differing | 0 / 2,650 (100%) | 0 / 2,650 (100%) |
| attr runs differing | blank-cell erase pen only (vim 291/300 rows, htop 46/165; all 22,081 differing cells are spaces — BCE bg-only vs vt100's full-pen stamp; invisible) | 0 — string-identical attr dumps |
| panics / hangs | none | none |

**Capability matrix** (source citations in the spike transcripts):

| question | vt100 (fork) | alacritty_terminal | wezterm-term |
|---|---|---|---|
| Semantic scroll events `ScrollUp{top,bottom,n}` | native (fork's TermEvent) | none — scroll calls `mark_fully_damaged()`; needs a vendored ~20-line patch (`scroll_up_relative`/`scroll_down_relative` + Event variant) | none — `StableRowIndex` diffing recovers full-screen scrolls but NOT DECSTBM region scrolls (`screen.rs:645-760`); needs an ~8-site sink patch |
| Query replies | none (QueryScanner owns replies) | answers DA1/DA2 (advertising *alacritty's* version)/DSR/DECRQM/kitty-kbd/CSI 18t itself via `Event::PtyWrite` — would double-reply beside QueryScanner | answers DA1/DA2/DA3/XTVERSION/DSR/DECRQM/XTGETTCAP/DECRQSS straight into the single pty writer; needs a filtering `Write` wrapper |
| Kitty APC (stripped upstream by GfxSplitter) | n/a | inert (vte discards) | dead path (`enable_kitty_graphics()` defaults false) |
| Resize | no reflow (matches goldens) | always reflows primary screen, no public switch (one-line fork) | reflow available |
| DECSET 2026 | tracked (task 1.6) | in vte Processor, needs embedder-driven timeout | untracked; DECRQM lies |
| transitive crates | 0 new | 35 | 201 (incl. `image` codecs, unconditional) |
| cold build | — | ~19.5 s | ~95 s |
| distribution | in-tree | crates.io, healthy cadence (0.26.0 2026-04) | crates.io stale (termwiz last 2025-03); practical use = git dep on monorepo or vendoring ~14 crates |

## Decision

**Extend the vendored vt100 (roadmap path 1.4B).**

1. All three futures require a maintained fork for the one non-negotiable:
   ordered semantic scroll/erase/alt events for kitty placement replay. Neither
   candidate provides them stock. Since the fork is unavoidable, the deciding
   axes become golden parity, reply ownership, and weight — all favoring vt100.
2. Golden parity: vt100 *is* the behavior the Phase 0 goldens describe.
   alacritty's forced reflow and self-answered queries (wrong DA2 identity) and
   wezterm's unconditional answerbacks each churn contracts QueryScanner owns.
3. Weight: 0 new crates vs 35 vs 201; 73-95 s cold builds for wezterm.
4. The measured gap list was closed corpus-driven in the same phase (see the
   1.4B commit): DECAWM, SGR 2/22 intensity + 4:x underline styles + 58/59
   consumption, OSC 8/133/color-query/reset consumption, OSC 52 →
   `TermEvent::Clipboard` for the server to gate, charset designators, DCS
   query silence, kitty-keyboard/window-op consumption. Corpus replays now
   produce **zero** unhandled-sequence debug lines (`tests/audit.rs`).

## Revisit when

- Phase 3/4 hit a vt100 architectural wall (resize reflow, grapheme-cluster
  width, kitty keyboard flag stack proving invasive), or
- the fork's gap-closure burden exceeds ~2 weeks/year.

Prepared escape hatch: `alacritty_terminal` + the vendored scroll-event patch —
its `EventListener` model maps 1:1 onto `TermEngine`, its build is light, its
cadence healthy. The `TermEngine` seam (`src/engine.rs`, tasks 1.1/1.2) exists so
that swap is a `type ActiveEngine` flip gated by the corpus goldens, and the S1
measurement harness (scratchpad `s1-diff`) is the dual-run differential runner.
