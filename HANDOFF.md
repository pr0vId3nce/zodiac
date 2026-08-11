# Zodiac dev handoff — continue on the macbook (Xcode / iOS app)

_Written 2026-08-11 from the linux box (p14s / “NTP424”). Everything below is
already on `origin/main` — `git pull` on the macbook to get it._

## TL;DR

Two things were in flight. One is **fixed** (terminal crash), one needs the
**macbook + Xcode** (the native iOS app won’t finish pairing). The astrolabe
**bridge/server side is proven-working** — the iOS “connecting” bug is in the
**iOS app code**, which is not in this repo, so it can only be fixed on the mac.

- **Phone client = the native iOS app**, NOT the astrolabe PWA. Ignore the PWA
  path when chasing phone issues.

## What shipped this session (all on `origin/main`)

- `f615e34` astrolabe PWA auto-update (not relevant to the iOS app; harmless).
- `420c5e3` **terminal crash fix** + regression test (validated in the real GUI).
- `1938f8f` sturdier web reconnect (PWA-only; harmless to iOS).
- `69e4f0c` **security: fail-closed bridge auth + socket lockdown** (see below).
- `132bd8f` show the running model for pi panes (via `PaneState.model`).
- `ac04cc6` Alt+Z → observatory; drop sidebar buttons; model in the pane header.
- `8d027d7` new-agent **picker** (choose harness + model on Alt+N / “new agent”).
- `a620368` `/` only focuses the composer (no stray slash).

Deploy on the mac side isn’t needed for the iOS work, but on any linux host the
flow is `./update.sh --rebuild` (rebuilds binaries, redeploys the astrolabe web
bundle, restarts the bridge service).

## Open issue #1 — terminal autocomplete crash (FIXED, needs a relaunch)

Root cause: scrolling a terminal pane up past one screenful underflowed
`vendor/vt100/src/grid.rs` `visible_rows()` (`rows_len - scrollback_offset`).
A big tab-completion fills the scrollback; wheeling up to read it triggers the
panic (`attempt to subtract with overflow`, process exits 101). It’s client-only
(the server never scrolls its parser), which is why replaying completion output
never reproduced it — the trigger is the **scroll**.

- Fixed with a saturating subtraction; regression test in
  `src/client_core.rs::scrollback_tests`. **Validated in the real GUI**: the
  unpatched binary exits 101 on scroll; the patched one is clean.
- **Why it may still crash for the user:** the fix is in the **`zodiac-gui`
  binary**, and a rebuild does not replace an already-running process. After
  `./update.sh --rebuild` you must **quit and relaunch the GUI**.
- If it still crashes after a relaunch of the freshly-built binary, it’s a
  second bug my repro didn’t hit — capture it by running **`zodiac-gui main`
  from a terminal** and pasting the `panicked at …` line.

## Open issue #2 — iOS app stuck on “connecting” when pairing (THE macbook task)

### Established facts (bridge/server side is 100% healthy)

Verified from the linux box against the live bridge (tailnet `100.118.22.13:7979`):

- **Token is consistent** end to end: the zodiac server’s current
  `pairing_token`, the bridge’s persisted `~/.local/state/astrolabe/session-token-main`,
  and what the pairing QR encodes are all the **same** value (`2f0d6651…`, 32
  chars). No mismatch.
- **`/api` works with the token** (the native app’s path):
  `GET /api/host` with `Authorization: Bearer <token>` → **HTTP 200**; without a
  token → **401** (correct, by design after the fail-closed change).
- **`/ws` works with the token**: opens, and the first message is `hello` with
  full state (2 panes, `link:true`).
- The security fail-closed change **does not affect this user**: the bridge has
  a token, so `tokenOk()` behavior is byte-identical before/after (the
  `candidates.length === 0` branch is never taken). Confirmed.

Conclusion: **anything that sends the correct token to `/api` or `/ws` connects.**
So the iOS app is either not sending the token, sending it wrong, hitting the
wrong URL, or its pairing state machine is stuck. That code lives only in the
Xcode project.

### endpoint the app should be using

`~/.local/state/astrolabe/endpoint.json`:
```json
{"url":"http://100.118.22.13:7979","cid":"7d8fc7b2-f6a4-4c77-a3de-366fa10beea8","name":"NTP424"}
```
The pairing QR (zodiac GUI → “pair phone” / Alt+P) encodes `{url}/?t=<token>&cid=…&name=…`.

### How the native app talks to the bridge

Per `astrolabe/bridge/main.ts:733` — “**The native iOS shell talks to these
[`/api/*`]; the PWA keeps using the WebSocket.**” The app is Bearer-token +
`/api` for reads (`/api/host`, `/api/panes`, `/api/transcript`), `/api/apns/register`
for push, `/api/prompt` + `/api/answer` for actions, and APNs for live pushes.
(The `/ws` WebSocket is the PWA/widget path.)

### Debug plan on the macbook

1. Run the iOS app in Xcode against the live bridge; watch its **network logs**.
2. Confirm it resolves the endpoint to `http://100.118.22.13:7979` and hits
   `/api/host` (or whatever its first pairing call is) with
   `Authorization: Bearer <token>`.
3. Confirm the **token it extracted from the QR** equals `2f0d6651…`
   (print/log it). A truncated/URL-decoded/whitespaced token is the usual
   culprit.
4. Check the **pairing state machine**: what response does it wait for to leave
   “connecting”? Does it require a field that changed? (My `/healthz` trim now
   hides `session`/`panes`/`devices` unless a token is supplied via `?t=` — **if
   the app polls `/healthz` unauthenticated and parses those fields, that would
   strand it; revert that trim in `main.ts` `/healthz` handler if so.** It’s the
   one plausible server-side regression, though `/healthz` shouldn’t gate
   “connecting”.)
5. Verify it isn’t trying `https`/`wss` against an `http` endpoint, or a stale
   cached endpoint/token from a previous pairing (clear app state / re-scan).

### Re-verify the bridge from anywhere (paste-ready)

```bash
# token the bridge accepts (== server pairing_token == QR token)
TOK=$(cat ~/.local/state/astrolabe/session-token-main)
curl -s -o /dev/null -w "api w/token:  %{http_code}\n" -H "Authorization: Bearer $TOK" http://100.118.22.13:7979/api/host   # expect 200
curl -s -o /dev/null -w "api no token: %{http_code}\n" http://100.118.22.13:7979/api/host                                   # expect 401
curl -s http://100.118.22.13:7979/healthz   # {"ok":true,"link":true,...}
# full WS check (needs the `ws` npm dep, e.g. from astrolabe/bridge):
#   node -e 'const W=require("ws");const t=require("fs").readFileSync(process.env.HOME+"/.local/state/astrolabe/session-token-main","utf8").trim();const s=new W("ws://100.118.22.13:7979/ws?t="+t);s.on("message",d=>{console.log(JSON.parse(d).t);process.exit(0)})'
```

## Bridge / security reference (line refs in `astrolabe/bridge/main.ts`)

- Auth: `tokenOk()` @116 (now **fail-closed**), `haveToken()` @125, static/`/api`
  gate reads Bearer @~730, `/ws` gate reads `?t=` @~974. Static PWA assets are
  served unauthenticated on purpose.
- Runs as `node main.ts` (Node 24 type-stripping, no build) via
  `astrolabe.service` (systemd --user), `WorkingDirectory=…/astrolabe/bridge`,
  serves `../web/dist` off disk. Token file mode 0600; socket now 0600 + dir
  0700 + `SO_PEERCRED` owner check (`src/server.rs`).

## First moves on the macbook

1. `git pull` this repo; read this file.
2. Open the iOS Xcode project; reproduce pairing against the live bridge.
3. Log the token + URL the app actually uses; compare to the facts above.
4. The bug is almost certainly the app’s token handling or pairing state
   machine — the bridge answers correctly to a properly-tokened request.
