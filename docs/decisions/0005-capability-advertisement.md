# 0005: Child capability advertisement — server is the capability authority

- **Status:** accepted
- **Date:** 2026-08-09
- **Spike/timebox:** written on paper during Spike S3 per roadmap task 4.1
  ("it constrains the GUI blit path, so decide on paper now"); no code spike.

## Context

Phase 4 enables kitty graphics/animation/placeholders/keyboard for child apps.
Children probe capabilities (kitty APC `a=q`, terminfo via `TERM`, CSI u
queries), but zodiac sits between the child and a client that may be the TUI
(host terminal capabilities vary), the GUI (full capabilities), or nobody
(detached pane). Something must decide what children are told, and it must
keep working when panes outlive clients or clients of different kinds
re-attach. Roadmap task 4.1.

## Options considered

**Per-child TERM / terminfo switching.** Spawn panes with `TERM=xterm-kitty`
when a capable client is attached. TERM is fixed at spawn while attachment
changes at runtime; long-lived panes (the zodiac norm — agents run for days)
would be stamped with whatever client happened to be attached at spawn;
re-attach from GUI to TUI turns advertised capabilities into lies. Rejected
without a spike — the failure mode is structural, not quantitative.

**Client-proxied probing.** Forward `a=q` probes to the attached client and
relay its answer. Fails detached panes (no one to answer — probe timeouts
hang child startup), multi-client attach, and probe/attach races. Also puts a
client round-trip inside a child's synchronous startup path.

**Server as capability authority (chosen).** Children keep
`TERM=xterm-256color`. The server answers capability probes itself, always
and identically, gated by a per-session **capability floor**; clients degrade
at render time. `GfxEngine.active` already gates exactly the kitty `a=q`
reply path today, so this extends an existing mechanism rather than adding
one.

## Decision

The server is the single authority for what children believe about their
terminal. Concretely:

- Children keep `TERM=xterm-256color`. No per-pane TERM switching, ever.
- The server answers kitty APC probes (`a=q`) positively for every capability
  at or below the session's **capability floor** — a settings value listing
  advertised features (v1 floor: kitty graphics static images; Phase 4 adds
  animation `a=f`/`a=a`, unicode placeholders `U=1`, kitty keyboard flags).
- **Degradation happens at client render time, not spawn time.** The GUI
  renders natively. The TUI re-emits to capable hosts and falls back per the
  documented downgrade table (frame 0 for animations, fallback cells for
  placeholders, legacy encoding for keyboard) otherwise. A pane with no
  client attached still answers probes — advertisement never depends on
  attachment.
- Floor changes apply to newly probed apps only (children cache probe
  results; documented limitation, same as terminal emulators changing config
  live).
- Constraint on the GUI blit path (why this is decided now): the floor is a
  promise the GUI must keep. The GUI client must render every capability the
  server may advertise at the configured floor, so the floor's maximum value
  is defined by the GUI feature set of the same release — the server never
  advertises ahead of what the shipped GUI renders. The TUI is allowed to
  degrade (with the table); the GUI is not.
- The server stores graphics data and answers queries; clients own timing
  (e.g. animation playback) — the server never schedules frames.

## Revisit when

- A child app misbehaves on positive probe + degraded render badly enough to
  need per-pane opt-out beyond the Phase 4 kill switch.
- Mixed concurrent attach (TUI + GUI on one session) becomes common and
  per-client advertisement pressure appears — that would force re-opening
  client-proxied probing with probe-answer versioning.
- A capability arrives that cannot degrade at render time (would require
  refusing to advertise below floor, i.e. a per-capability hard gate).
