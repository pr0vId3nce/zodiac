# Kitty graphics protocol in panes — design

Panes become graphics terminals: apps inside a pane can transmit images and
placements via the kitty graphics protocol (`APC _G … ST`), and zodiac
renders them through the outer terminal's own kitty protocol, faithfully —
images scroll with text, live in scrollback, and survive detach/reattach.

## Architecture

Graphics state is **server-authoritative**; the client is a dumb compositor.

```
inner app ──APC _G──▶ pane splitter ──┬─ text ──▶ vendored vt100 ──▶ ring / T_OUTPUT
                                      └─ gfx cmds ──▶ GfxEngine (server)
                                                        │  replies (OK/err) → pane PTY
                                                        ├─ T_GFX_IMG   (pixel data, lazy, chunked)
                                                        └─ T_GFX_STATE (placement snapshot, JSON)
client: applies snapshots ──▶ compositor: after each ratatui draw, diffs the
focused pane's visible placements against what's on the outer terminal and
emits kitty escapes (place / crop / delete), pixels forwarded verbatim.
```

Why server-side: `t=t`/`t=s` media must be read (and deleted) exactly once;
query replies must hit the pane PTY immediately; and state must survive
detach. The ring/backlog carries **stripped** text only (plus synthesized
cursor advances), so trimming can never corrupt graphics state and replay
stays lean.

## Vendored vt100 (vendor/vt100)

vte 0.11 consumes APC strings silently, so the splitter removes `_G`
sequences before the parser ever sees them. The vendored crate additionally
records an ordered **event log** on `Screen` — the signals the placement
engine needs and infers nothing from:

- `ScrollUp { top, bottom, n, to_scrollback }` / `ScrollDown { top, bottom, n }`
  (linefeed-at-bottom, wrap, CSI S/T, IL/DL, RI)
- `EraseScreen` (ED 2/3), `AltEnter` / `AltExit`, `Resize`, `Reset` (RIS)

Grid methods return scroll amounts; `Screen` pushes events, preserving exact
order. `Parser::drain_events()` hands them to the engine after each text
segment.

## Lifecycle semantics (kitty-observed; the spec leaves these implicit)

- Placements anchor to the cursor cell at placement time, tracked as an
  **absolute line index** (`total_scrolled + screen_row`).
- Full-screen scroll: anchors keep their absolute line — images move with
  text, into scrollback, and are dropped past scrollback retention
  (CLIENT_SCROLLBACK lines).
- Scroll-region scroll (also IL/DL): placements anchored inside the region
  move with it and are **deleted** when the anchor leaves the region;
  placements outside don't move. Region scrolls never enter scrollback.
- ED 2 / RIS: delete on-screen placements (RIS also frees all image data).
  ED 0/1 and EL never touch images (cell-independent, as in kitty).
- Alt screen: its own placement layer; entering hides main placements,
  leaving deletes the alt layer and restores main.
- Resize: anchors keep their cells; no reflow; clipping happens at render.
- Cursor advance after a placement (unless `C=1`) is synthesized as CUD/CUF
  into the text stream, so both vt100 instances agree.

## Protocol coverage

Full: `a=t/T/p/q/d`, chunked `m=1`, formats 24/32/100 (PNG header parsed for
dimensions only — pixels are relayed verbatim, never decoded), `o=z` passed
through, media `t=d/f/t/s` (temp-file deletion honors the spec's
`tty-graphics-protocol` + tmpdir rule; shm read+unlinked), the whole `d=`
delete matrix (lowercase = placements, uppercase = + image data), `i`/`I`
numbering with id allocation, `q` quiet levels, geometry `x,y,w,h,X,Y,c,r,z,C`,
responses echoing `i`/`I`/`p`.

Declined (error reply, documented): animation (`a=f/a`) and Unicode
placeholders (`U=1`). Queries are answered only while the last-attached
client reported outer-terminal graphics support, so apps degrade cleanly in
plain terminals.

Quotas: 64 MiB stored image data per pane, 32 MiB per image; LRU eviction of
unplaced images; `ENOSPC` on breach.

## Client compositor

- Snapshot rows arrive as `screen_row: i32` (negative = that many lines into
  scrollback); viewport row = `screen_row + scroll_offset`.
- Partial visibility → proportional source-rect crop in image pixels; also
  clamped to the pane rect (sidebar never overlapped).
- Outer ids: per-(pane, inner id) remap from a reserved range — inner apps
  can't collide with each other or with the home-page card art; anonymous
  placements get fresh keys, identified ones replace on (i,p).
- Pixels transmitted to the outer terminal once per (pane, image, version),
  then placements move via cheap re-place commands — no retransmits, and
  place-then-delete batching avoids flicker. Cursor is saved/restored around
  compositor writes.
- On exit/detach the client deletes every outer image it created.

## PTY pixel size

The client reports its cell size (px) in T_ATTACH/T_RESIZE; panes' PTYs get
`pixel_width/height = cols·cw / rows·ch` and the query scanner answers
CSI 14t/16t — inner apps compute correct image geometry, and inner cell size
always equals outer cell size, so placements map 1:1.
