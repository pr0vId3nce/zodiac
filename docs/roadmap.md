# Roadmap: TUI Multiplexer → GUI-Capable Agent Harness

Working agreement for the multi-phase evolution of zodiac. Rules of the road are in
[Guardrails](#guardrails). New ideas go to [icebox.md](icebox.md), not into the current
phase. Spike outcomes go to [decisions/](decisions/) as ADRs.

**The four phases, in order:**

1. **VT engine hardening** — the stability fix. Swap or extend the vendored vt100.
2. **Structured agent panes** — `claude` stream-json replaces screen-scraping heuristics.
3. **GUI client v1** — a third client on the existing unix-socket protocol. TUI stays.
4. **Full kitty enablement** — animation, Unicode placeholders, kitty keyboard,
   proportional transcripts, drag-and-drop.

Phase 0 (below) builds the regression harness that gates all of it.

**Critical path:** harness → engine decision + parity → `client_core` extraction →
GUI grid + gfx blit → placeholders/animation/keyboard. Phase 2 is off the critical
path — it may run in a parallel worktree once the Phase 1 engine trait lands with
goldens green (the one exception to the one-phase rule).

---

## Phase 0 — Baseline + Regression Harness

**Goal:** every later swap (engine, protocol, client) is gated by deterministic tests
over real recordings.

### Tasks

- [x] **0.1 Repo hygiene.** Triage the dirty tree into logical commits (include
      `flake.lock`). Worktree-per-session rule takes effect after this.
- [x] **0.2 PTY recorder.** New `src/bin/ptyrec.rs` on the existing `portable-pty 0.8`
      dep. Length-prefixed chunks `{millis u32, kind u8 (bytes|resize), payload}` —
      exact bytes, NOT asciicast (which lossily re-encodes; `GfxSplitter` needs the
      real stream). Default 50×120.
- [x] **0.3 Corpus capture** into `tests/corpus/` + `MANIFEST` (pinned app versions):
      `vim` (alt screen + scroll regions), `less` (quit/restore), `htop`, `fzf`, one
      kitty-graphics stream (`kitten icat`), then `claude` (incl. a permission prompt
      and spinner phase) and `pi`. Cap ~5 MiB/file. The claude/pi captures may need a
      human at the keyboard — record the scriptable ones first, flag the rest.
- [x] **0.4 Golden-screen runner** `tests/golden.rs`: replay each corpus file through
      the parser at recorded size; dump screen contents + per-cell attrs at every
      resize marker and EOF into `tests/golden/`. Byte-deterministic across runs.
- [x] **0.5 TermEvent goldens.** Same runner records `drain_events()` per chunk.
      This is the contract `GfxEngine::apply_event` depends on — any Phase 1 engine
      must reproduce it (or re-baseline with a written note).
- [x] **0.6 Gfx goldens.** First extract the duplicated pipeline: `PaneSim`
      (`gfx.rs` test module) must delegate to a shared `pub(crate)`
      splitter→parser→engine pipeline used by the real `SrvPane::process_output`
      (`pane.rs`), so tests exercise the real path. Then snapshot `GfxSnapshot` JSON
      at checkpoints over the graphics recording.
- [x] **0.7 QueryScanner table tests** (`query.rs`): DA1/DA2/DSR-5/6, DECRQM
      2004/2026/1049/1000-range, XTVERSION, split-across-chunk cases.
- [x] **0.8 Heuristic characterization tests.** Snapshot `status()` / `thinking()` /
      `stall_match` verdicts over the claude/pi recordings at checkpoints. Purpose:
      Phase 1 can't silently break pty-pane detection; Phase 2's "retire heuristics"
      is measured, not vibes.
- [x] **0.9 Merge gate.** `scripts/check.sh` = `cargo fmt --check && cargo clippy
      -- -D warnings && cargo test`, wired into the `flake.nix` devshell.
- [x] **0.10 DECSET 2026 to host.** Wrap the client draw tick + the four overlay
      passes in `CSI ?2026h/l`. Host-side only; removes flicker noise so later visual
      regressions are attributable. (Child-side 2026 stays in Phase 1 — it touches
      the engine/flush path.)
- [x] **0.11 Docs scaffold.** `docs/decisions/` ADR template, `docs/icebox.md`,
      this file.

### Exit criteria

- [x] ≥7 corpus recordings checked in with manifest. *(7 of 7 — claude/pi captured via a scripted PTY driver over real sessions; see MANIFEST.)*
- [x] `scripts/check.sh` green and deterministic 3 runs in a row.
- [x] Screen, TermEvent, Gfx, QueryScanner goldens exist and pass; heuristic goldens populated over the claude/pi recordings.
- [x] Working tree clean; 2026 host emission shipped in the TUI client.

### Non-goals

No fixing of vt100 gaps discovered while recording (icebox them). No engine changes.
No new protocol frames. No CI service — the local script is the gate.

---

## Phase 1 — VT Engine Hardening

### Spike S1 — engine choice (timebox: 3 days, hard stop)

**Question:** is porting zodiac's instrumentation (TermEvent stream, query replies,
mode introspection) onto `alacritty_terminal` or `wezterm-term` cheaper over 12
months than closing vt100's gaps (all DCS, OSC 8/52/10/11, DECSET/SGR coverage)
ourselves?

**Method:** scratch bins (outside the repo) feeding the Phase 0 corpus through each
candidate. Evaluate concretely:

1. **Scroll semantics** — can we observe `ScrollUp{top,bottom,n}`-equivalent?
   `alacritty_terminal` damage is line/cell-based with no semantic scroll events
   (expect a fork or wrapper hook). `wezterm-term` exposes *stable line indices* —
   evaluate anchoring `Placement`s to stable rows as a **replacement** for TermEvent
   scroll-replay, not just a port (possibly a net simplification of `gfx.rs`).
2. **Query replies** — both engines answer DA1/DSR/DECRQM themselves; measure whether
   `QueryScanner` shrinks to XTVERSION-only.
3. **Kitty APC conflict** — `wezterm-term` parses kitty graphics itself; zodiac needs
   verbatim relay through `GfxSplitter` *before* the engine. If APC can't be
   intercepted or disabled, that is a **disqualifier**.
4. Dep count, build time, publication cadence (wezterm-term publishes from the
   monorepo sporadically — check 12-month history), doc quality.
5. Cell-attr fidelity vs the Phase 0 goldens (run the golden runner against each).

**Decision note `decisions/0001-vt-engine.md` must contain:** capability matrix;
measured golden diffs per engine; estimated adapter+fork LOC; pinned versions;
dep-tree/build-time numbers; the APC interception answer; recommendation + explicit
fallback ("extend vt100 remains viable because X"). Inconclusive after 3 days →
default = extend vt100.

### Tasks

- [ ] **1.1 `TermEngine` trait**, extracted from actual call sites (`pane.rs` screen
      reads/title/modes/screen_hash, `gfx.rs` drain_events, `query.rs`, `server.rs`/
      `snapshot.rs` reads). Surface: `process(&[u8]) -> Responses`, cell/row iteration
      with attrs, cursor, modes, title, `drain_events()`, `resize`.
- [ ] **1.2 Implement trait for vendored vt100**; port `pane.rs`/`gfx.rs`/`query.rs`
      to the trait. Mechanical, move-only. Gate: all goldens byte-identical.
- [ ] **1.3 Run Spike S1 → ADR 0001.**
- [ ] **1.4A (switch)** Adapter for chosen engine behind a cargo feature; dual-run
      differential mode diffing full screens per chunk between engines. Zero cell
      diffs on corpus, or each diff class documented as benign in the ADR.
      **— or 1.4B (extend)** in `vendor/vt100/`: DCS tolerance (consume cleanly),
      OSC 8 (parse+store or parse+drop, decided in ADR), OSC 52 (surface as a new
      `TermEvent::Clipboard` for the server to gate), OSC 10/11 replies, DECSET audit
      driven by the corpus debug-log offender list, SGR gaps (4:x underline styles,
      58/59 underline colors).
- [ ] **1.5 TermEvent parity** — golden event logs match; re-baselines carry a
      `REBASELINE:` commit note.
- [ ] **1.6 Child-side 2026** — honor a child's `?2026h/l` by coalescing that pane's
      output flush; report `?2026;1$y` once honored.
- [ ] **1.7 Shrink `QueryScanner`** to whatever the engine doesn't answer; update its
      table tests.

### Exit criteria

- [ ] All Phase 0 goldens green under the final engine; dual-run diff clean (or
      ADR-documented) on the full corpus.
- [ ] One week of daily driving: vim/htop/fzf/less/claude/pi eyeball-identical.
- [ ] OSC 8/52 covered by tests; corpus runs produce zero "unhandled" debug lines.
- [ ] Child 2026 honored; ADR 0001 merged.

### Non-goals

No sixel. No kitty keyboard (Phase 4). No resize reflow (if the engine gives it
free, note it, don't wire it). No performance work beyond parity. No protocol
changes. No ACP.

---

## Phase 2 — Structured Agent Panes

### Spike S2 — pi integration (timebox: 2 days)

**Question:** what structured interface does `pi` offer (stream-json equivalent?
RPC/serve mode? ACP?), and can permission prompts be intercepted — or does pi stay a
heuristic pty pane in v1? Decision note `decisions/0002-pi-structured.md`: exact
flags/protocol, event mapping table, permission interception answer, resume story,
verdict with revisit trigger.

**Sub-spike (half day):** verify the claude CLI structured surface: `claude -p
--input-format stream-json --output-format stream-json --include-partial-messages`,
permission routing (stream-json control-request `can_use_tool` vs
`--permission-prompt-tool`), `--resume <session-id>` semantics. Pin the tested
version.

### Tasks

- [ ] **2.1 Protocol.** `proto: u32` on `Hello` (first explicit version field,
      alongside the existing serde-default pattern). New frames: `T_AGENT_EVENT`
      (server→client NDJSON), `T_AGENT_INPUT`, `T_PERM_REQ`/`T_PERM_RESP` (frame ids
      19 and 32+ are free; all three implementations skip unknown frames). Extend
      `PaneState` with `#[serde(default)] kind: "pty"|"agent"`. Add new client frame
      types to the distinctness test in `protocol.rs` (note: it currently omits
      `T_TRANSCRIPT_REQ`).
- [ ] **2.2 Agent pane runtime.** New spawn path: pipes, no PTY, no VT engine; NDJSON
      reader thread → `SrvEvent`; capture `session_id` from the init event; stderr to
      pane log. Needs a pane-kind representation — `SrvPane` is unconditionally PTY
      today (master/writer/killer are non-optional fields).
- [ ] **2.3 Server-side transcript store.** Bounded AgentEvent ring per pane;
      replay-on-attach analogous to `T_REPLAY`; feeds the existing `T_TRANSCRIPT`
      path so phone read-mode stops scraping `~/.claude/projects` JSONL for
      structured panes (scrape stays for pty panes).
- [ ] **2.4 Ratatui transcript widget v1.** Plain text: role headers, tool-call
      header lines, streaming partials, scrollback. Reuse the pure helpers
      `parse_md`/`wrap_md_runs`/`apply_md` from `client.rs` (already unit-tested);
      do NOT entangle with `chat.rs` (that's the home-page chat panel).
- [ ] **2.5 Permission inbox.** `T_PERM_REQ` → TUI modal (allow / deny /
      always-this-tool) + astrolabe bridge → phone push. Server-side inbox is
      authoritative (requests persist, replay on attach); explicit timeout policy
      (e.g. 10 min → deny with reason). This deletes the bridge's screen-scraping
      question parser (`astrolabe/bridge/question.ts`) for structured panes.
- [ ] **2.6 Structured status.** For `kind=agent`: `status()` derives from events;
      `monitor.rs` skips LLM classification. Pty heuristics untouched (0.8
      characterization tests stay green).
- [ ] **2.7 Structured retry.** On API-error events: relaunch `--resume
      <session_id>` + re-send, with backoff, max attempts, surfaced failure state.
      Keystroke-injection `fire_autoresume` stays for pty panes only.
- [ ] **2.8 Snapshot/restore.** `SnapPane` gains `kind` + `session_id`; agent panes
      restore via structured relaunch, not a typed shell line (fixes the
      `restore_command` assumption; keep `scripts/zodiac-restore.sh` reading valid).
- [ ] **2.9 pi per S2 verdict.**
- [ ] **2.10 Compat matrix test.** Old TUI client + old bridge against new server;
      scripted, a phase-gate item.

### Exit criteria

- [ ] Full claude task end-to-end in an agent pane: prompt from TUI, tool approval
      from phone, streamed transcript in both places.
- [ ] `kill -9` the claude process mid-task → structured retry resumes the session.
- [ ] Zero heuristic/scrape code *active* for structured panes (grep-able: monitor,
      stall watchdog, JSONL scrape all branch on `kind`).
- [ ] Old clients verified against new server; `proto` landed; ADR 0002 merged.

### Non-goals

No markdown/rich rendering, no transcript images, no subagent-tree UI, no MCP
management UI, no Agent SDK dependency (raw stream-json), no ACP, no removal of pty
heuristics, no transcript search.

---

## Phase 3 — GUI Client v1

### Spike S3 — GUI stack (timebox: 5 days)

**Question:** can winit + wgpu + cosmic-text (directly or via `glyphon`) render a
120×50 SGR-attributed grid plus kitty image blits at 60 fps with input latency
comparable to the TUI, at a one-person maintenance cost? Prototype fed by a canned
`T_REPLAY`+`T_OUTPUT` capture; study Rio's `sugarloaf` and Zed's
`alacritty_terminal`+GPU pattern for atlas/damage architecture — copy patterns, not
code. Decision note `decisions/0004-gui-stack.md`: pinned crate versions,
glyph-atlas plan, damage/redraw strategy, hidpi + Wayland/X11 answer, measured frame
time + latency, `softbuffer` CPU fallback yes/no. Target = Linux only for v1.

### Tasks

- [ ] **3.1 Extract `client_core`** from `client.rs`: socket IO + frame decode,
      attach/replay, per-pane client-side engine, input encoding, gfx snapshot
      tracking. Move-only, gated by goldens + a scripted TUI smoke check.
- [ ] **3.2 Workspace split**: `zodiac` (server + TUI), `zodiac-gui`, `vendor/vt100`.
      One atomic commit, announced (concurrent sessions rebase).
- [ ] **3.3 Grid renderer**: monospace, full SGR, cursor shapes, damage-driven
      redraw, present-on-demand.
- [ ] **3.4 Input**: keyboard → existing legacy encoder from `client_core`; mouse per
      inner-app mode; basic winit IME wired.
- [ ] **3.5 Graphics blit** — first time zodiac *decodes* pixels: `T_GFX_IMG` chunks
      (`f=100` PNG via a png crate; `f=24/32` raw) → wgpu textures, placed per
      `VisPlacement` with the existing crops. The TUI's kitty re-emission path is
      untouched.
- [ ] **3.6 Chrome parity, minimal**: reuse `kitty.rs`'s RGBA card/sparkline/mascot
      renderer as textures; tabs/status as text.
- [ ] **3.7 Perf + a week of daily driving.**
- [ ] **(during S3) Write ADR 0005** — child capability advertisement (Phase 4
      design; it constrains the GUI blit path, so decide on paper now).

### Exit criteria

- [ ] Daily-driven `zodiac-gui` for shell + claude + agent panes on Linux for a week,
      against an **unchanged server**.
- [ ] TUI client still green (goldens + smoke). `kitten icat` corpus pane renders in
      GUI. ADR 0004 merged.

### Non-goals

No proportional fonts/ligatures beyond cosmic-text defaults. No macOS/Windows
promises. No config UI. No selection/clipboard polish beyond basics. No kitty
keyboard, animation, or placeholders (Phase 4). No astrolabe changes.

---

## Phase 4 — Full Kitty Enablement

### Tasks

- [ ] **4.1 Capability authority** (per ADR 0005, written in Phase 3). Recommended
      shape: **the server is the capability authority.** Children keep
      `TERM=xterm-256color`; the server answers the kitty APC probe (`a=q`)
      positively — `GfxEngine.active` already gates exactly this — and degradation
      happens at client render time, not spawn time. A per-session capability floor
      (settings) controls what's advertised; changes apply to newly probed apps only
      (documented limitation). No per-pane TERM switching; panes outliving clients
      stops mattering because advertisement never depended on the attached client.
- [ ] **4.2 Animation**: accept `a=f`/`a=a` (drop the ENOTSUP), frames under the
      existing 64 MiB/pane quota; GUI plays, TUI renders frame 0. Server stores,
      client times playback — the server never schedules frames.
- [ ] **4.3 Unicode placeholders (`U=1`)**: placeholder-cell → placement mapping in
      `GfxEngine` + `GfxSnapshot`; GUI renders; TUI on kitty hosts may re-emit
      (kitty resolves placeholders itself — verify, else fallback cells). Unblocks
      yazi/mdcat-style TUIs.
- [ ] **4.4 Kitty keyboard protocol**: per-pane CSI `>`/`<`/`=` flag stack in the
      engine; GUI synthesizes full CSI-u (Ctrl+Shift disambiguation, finally); TUI
      translates only if the host supports it (one-shot probe), else a documented
      downgrade table with a per-pane kill switch.
- [ ] **4.5 Proportional-font transcript** in the GUI (cosmic-text shaping); grid
      panes stay monospace.
- [ ] **4.6 Drag-and-drop**: winit `DroppedFile` on an agent pane → paths into
      `T_AGENT_INPUT` (content attachment → icebox).
- [ ] **4.7 OSC 52 write-through**: Phase 1's clipboard event → GUI clipboard
      (`arboard`) behind the Phase 2 permission inbox.

### Exit criteria

- [ ] yazi/mdcat image previews work in GUI; an animated gif plays in GUI.
- [ ] vim with kitty-protocol keys verified; Ctrl+Shift+P ≠ Ctrl+P in a test app.
- [ ] TUI degrades with zero artifacts on the full corpus; ADR 0005 merged.

### Non-goals

No sixel, no iTerm2 inline images, no kitty file-transfer/notification extensions,
no graphics over ssh-nested zodiac, no TUI-client animation.

---

## Risk Register

| Phase | Risk | Mitigation |
|---|---|---|
| 0 | Corpus non-determinism (clocks, spinners) | Recordings are replayed bytes — deterministic by construction. Never re-capture to fix a test; re-baseline only with a written note. |
| 0 | Dirty shared repo destroys the baseline | 0.1 first; worktree-per-session + check.sh gate. Top project-management risk of the roadmap. |
| 1 | TermEvent port onto alacritty/wezterm damage APIs | S1 evaluates wezterm stable-row anchoring as a *replacement*; trait lands before any swap so fallback is a feature-flag flip; dual-run differential is the tripwire. |
| 1 | wezterm-term API stability / APC self-parsing | S1 hard checks publication history, build, dep count, APC interception. "No" on APC interception disqualifies. Vendoring is the practiced escape hatch. |
| 1 | Engine swap silently changes pty status heuristics | 0.8 characterization goldens gate this explicitly. |
| 1 | Spike overrun | 3-day hard stop; inconclusive → extend vt100; revisit trigger in ADR. |
| 2 | claude stream-json churn across versions | Pin tested version in ADR + manifest; feature-detect at spawn; structured spawn failure falls back to pty pane with a visible badge. |
| 2 | pi has no structured surface | S2 allows "pi stays pty" without blocking the phase. |
| 2 | Phone-bridge compat | Additive-only changes (`serde(default)`, skippable frames); `Hello.proto` lands first; 2.10 compat matrix is a gate; bridge updated in-phase. |
| 2 | Permission requests lost while detached | Server-side inbox authoritative; phone push covers detached; explicit timeout. |
| 2 | Server restarts kill children | Additive-only frames within the phase; breaking changes batched at boundaries with announced restart; 2.8 session-id resume makes restarts cheap. Zero-downtime restart → icebox. |
| 3 | client.rs carve-out breaks the TUI | 3.1 move-only, one commit series, goldens + smoke after each step; TUI stays primary until phase exit. |
| 3 | wgpu/Wayland breakage on the dev box | S3 measures on the actual machine; fallback recorded in ADR before committing. |
| 3 | GUI scope creep | Non-goals enforced at gate; ideas → icebox. |
| 3 | Workspace split churns concurrent sessions | 3.2 as one atomic announced commit; everyone rebases. |
| 4 | Capability floor can't re-probe running apps | Server-as-authority (4.1); limitation documented, not fought. |
| 4 | Keyboard downgrade corrupts input on legacy hosts | Offer only when floor permits; losses enumerated + tested; per-pane kill switch. |
| 4 | Animation blows quota / server timing | Frames count against the quota; client times playback. |

---

## Guardrails

1. **One phase in flight.** Nothing from phase N+1 starts before phase N's exit
   checklist is reviewed and ticked here. Named exception: Phase 2 may run in a
   parallel worktree once Phase 1 task 1.2 lands (trait + goldens green). Merge
   order: Phase 1 branch first.
2. **Phase gates are checklists, not vibes** — ticked with evidence links (test
   names, ADR paths) in the gate-review commit.
3. **Every spike ends in an ADR** (`decisions/NNNN-*.md`, template `0000`), even if
   inconclusive. No engine/stack/protocol choice exists until its note is merged.
4. **Icebox** ([icebox.md](icebox.md)): any mid-session idea gets one line there and
   zero code in the current phase.
5. **`scripts/check.sh` before every commit.** Golden re-baselines require
   `REBASELINE: <reason>` in the commit message + a diff summary; a re-baseline
   without a reason is a revert.
6. **Shared-repo discipline**: `git worktree` per concurrent session; session-start
   ritual = clean `git status` or the first job is landing/reverting; no
   mixed-concern commits; feature branches ≤2 days; protocol-touching commits
   prefixed `proto:`.
7. **Server-restart batching**: restarts kill every pane's children and require
   explicit user OK; protocol-breaking work queues behind a `restart-pending` note
   here and lands in announced batches at phase boundaries.
