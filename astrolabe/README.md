# Astrolabe

**The phone half of zodiac, over Tailscale.**

Open one URL on your phone and see which agent is waiting on you — then
answer it. Each pane gets a colored terminal mirror, a slash-command
palette, a special-keys pad, and scrollback you can search. When an agent
asks a numbered question, its options become buttons. The reply box is an
ordinary text field, so your phone's own voice dictation works in it;
Astrolabe ships none of its own.

## Pieces

- **zodiac observer mode** (in the zodiac binary): a `T_WATCH` frame makes
  the server treat a connection as a read-only observer — it receives the
  session state, every pane's 512 KB replay ring, and live output, without
  ever kicking the attached desktop UI. Needs a zodiac built from this
  commit; against an older running server Astrolabe degrades to a
  plain-text 1 Hz screen mirror (status, input, prompts and keys all still
  work).
- **bridge** (`bridge/`, TypeScript, systemd --user): speaks zodiac's framed
  unix-socket protocol on one side, HTTP + WebSocket on the other. Runs on
  plain `node` (≥ 23 — types are stripped natively; bun runs it unchanged).
  Binds to this machine's **tailscale IPv4** by default, so the UI is
  tailnet-only by construction.
- **web** (`web/`): Vite + React + Tailwind SPA with an xterm.js mirror.
  **Browser/PWA access is paused** — development is native-first, and a
  plain browser gets a "use the iOS app" notice. The bundle's supported
  home is the iOS app's WKWebView; `?web=1` (sticky) re-enables browser
  access for development.
- **ios** (not in this repo — it ships through the App Store separately):
  the product, and fully native as of Aug 2026 — the agents screen, pane
  view, terminal (SwiftTerm core + a native renderer), composer, question
  buttons, keys pad and slash suggestions are all SwiftUI, speaking the
  bridge's WebSocket protocol directly. No WKWebView remains. Called
  **zodiac** on the phone, since that's the thing it shows you. Pair by scanning
  zodiac's QR (Alt+P), tap one to open a WKWebView on that computer's web
  UI, unchanged. It adds APNs push ("agent needs you", with lock-screen
  inline reply and numbered-answer buttons; badge = panes waiting across
  every paired computer), per-computer nicknames, three themes it hands
  through to the web UI, and a title bar showing the selected machine's
  uptime, battery, CPU and memory from `/api/host`. Everything it talks to
  is in this directory; push stays off until `ASTROLABE_APNS_*` creds exist
  in `~/.config/astrolabe/env`.

## Install

```
./install.sh          # npm install, vite build, install the background service
```

That's a `systemd --user` unit on Linux and a launchd LaunchAgent
(`dev.d3s.astrolabe`, logging to `/tmp/astrolabe.log`) on macOS.

Then open `http://<this-machine's-tailscale-ip>:7979` on your phone and add
it to the home screen.

Env knobs (set in `~/.config/systemd/user/astrolabe.service`):

| var | default | what |
| --- | --- | --- |
| `ASTROLABE_PORT` | `7979` | listen port |
| `ASTROLABE_SESSION` | `main` | zodiac session to mirror |
| `ASTROLABE_HOST` | tailscale IPv4 | bind address override — the default is read off the interface carrying a 100.64.0.0/10 address, so it works under launchd/systemd where the `tailscale` CLI isn't on PATH |
| `ASTROLABE_APNS_KEY` | — | path to the `.p8` APNs auth key (enables push) |
| `ASTROLABE_APNS_KEY_ID` / `ASTROLABE_APNS_TEAM_ID` | — | from the developer portal |
| `ASTROLABE_APNS_TOPIC` | — | app bundle id (`dev.d3s.Astrolabe`) |
| `ASTROLABE_APNS_ENV` | `sandbox` | `production` for TestFlight builds |
| `ASTROLABE_TOKEN` | — | optional *static* shared secret, on top of the default pairing token — see **Security** below |
| `ASTROLABE_PUSH_REDACT` | — | set to hide pane content in push bodies (generic text instead) |

The `ASTROLABE_APNS_*`/`ASTROLABE_TOKEN` values belong in
`~/.config/astrolabe/env` (the unit loads it via `EnvironmentFile=`), not in
the unit file itself.

## 🔒 Security

Auth is on by default, zero-config, and pairing-QR-driven: `zodiac` itself
mints a random **pairing token** every time its server process launches
(fresh on `zodiac`, unchanged across detach/reattach, gone when the server
exits) and reports it to the bridge over their local socket. Press **Alt+P**
on zodiac's home page to reveal a QR encoding that token plus this
machine's reachable URL — both the WebSocket and every `/api/*` route
reject requests that don't carry a token matching it.

- **iOS app**: computer list → **+** → **Scan QR**, point the camera at the
  Alt+P overlay. Adds this machine to your list (or, if you've paired it
  before, refreshes its token in place — matched by a stable id the bridge
  persists, not by the token itself, which is exactly what's rotating).
  **Enter Manually** is a fallback for when the camera can't see the
  screen.
- **Web (PWA/browser)**: same QR encodes a magic link
  (`http://<bridge-ip>:7979/?t=<token>`) — scanning it with your phone's
  regular camera app opens Safari straight to an authenticated session,
  which saves the token to `localStorage` and cleans the URL up. Re-add to
  your home screen from that tab if you'd already installed it.

Rescan whenever the *zodiac server itself* restarted (not just detached) —
the old token stops working the moment a fresh one exists.

**`ASTROLABE_TOKEN`** is an optional second, *static* secret layered on top
— useful if you want a credential that doesn't rotate (e.g. for a script
hitting `/api/*` directly), or as the fallback when a client's on an old
zodiac build that predates pairing tokens (degrades to poll mode, per the
observer-mode note above). Either token, the static one or the current live
one, is accepted:

```
echo "ASTROLABE_TOKEN=$(openssl rand -hex 16)" >> ~/.config/astrolabe/env
chmod 600 ~/.config/astrolabe/env
systemctl --user restart astrolabe
```

With *neither* a live zodiac link nor `ASTROLABE_TOKEN` set (e.g. zodiac
isn't running yet), the bridge falls back to tailnet-IP-only access control
and logs a loud startup warning for as long as that's true.

Static assets (the app shell itself) stay unauthenticated on purpose — you
need somewhere to load the page from before you have anywhere to read a
token out of a URL.

HTTP API (used by the iOS shell; the PWA uses the WebSocket):

- `POST /api/apns/register` / `unregister` `{token}` — device enrollment
  (stored in `~/.local/state/astrolabe/devices.json`, pruned when Apple
  reports a token dead)
- `POST /api/prompt` `{pane, text}` — send a reply (newline wiring for agent
  panes handled server-side); this is what lock-screen inline reply hits
- `POST /api/answer` `{pane, option, note?}` — answer a numbered question by
  option number; a note follows the digit into the pane as a normal prompt
- `GET /api/panes` — pane summary (index, name, status, agent, question)
  for the home-screen widget: a snapshot poll, no WebSocket
- `GET /api/host` — the bridge machine's uptime, CPU, memory and battery,
  for the phone's title bar. Every field but uptime is nullable (desktops
  have no battery). Memory comes from `vm_stat` on macOS and `MemAvailable`
  on Linux, not `os.freemem()`, which counts cache as used and reads as 95%
  on a healthy machine; CPU is sampled on a timer, since one `os.cpus()`
  reading is only an average since boot. Cached for 3 s.
- `POST /api/push-test` — push "the bridge can reach you" to every device

Pushes fire on status transitions only (→ `needs_input`, and `working` →
`done`), 30 s per-pane cooldown, and never on the first state after a bridge
restart.

## The UI

- **Herd view**: one card per pane — name, live status (`needs you` panes
  pulse red and float to the top), agent + version, the one-line summary and
  latest ⏺ transcript bullet, cwd, uptime. Tap a card to enter the pane.
- **Pane view**: two ways to look at a pane. **Read** re-renders the
  emulator's buffer as ordinary wrapped DOM text — what you actually want on
  a phone — and hides full-width separator rules and bare prompt rows.
  **Mirror** is the exact server-side grid in xterm.js, pinch- or
  button-zoomable, and clipped to the columns that actually carry content so
  a 120-column session doesn't scroll off into blank space. Both have 10k
  lines of scrollback and search with prev/next. The composer's send button
  submits text + Enter (`zodiac prompt` pacing); newlines in a multi-line
  dictation are sent as `\`+Enter so Claude Code keeps them as newlines.
- **Questions**: when a pane goes to `needs_input` the bridge parses the
  numbered options off its screen and shows them as buttons above the
  composer (and in the push notification). Tapping one sends that digit;
  anything typed in the composer rides along as a note.
- **Slash palette**: Claude Code built-ins plus everything in
  `~/.claude/commands/*.md`; tapping inserts the command into the reply box.
- **Keys pad**: Esc, Tab, ⇧Tab, arrows, ⏎, ^C ^U ^R ^O, PgUp/PgDn — raw
  escape sequences straight into the pane's PTY.

## Notes

- **HTTPS / PWA install**: over plain `http://` the app works fully in the
  browser (add-to-home-screen still gives an icon), but service-worker
  caching and a real install prompt need a secure context. If you want
  that, front it with `tailscale serve 7979` and open the `https://…ts.net`
  URL instead. Voice dictation works either way.
- **Old server, new binary**: the running zodiac server only grows the
  observer mode after a restart (`Alt+Shift+Q`, then `zodiac` — scrollback
  and pane layout are restored; `Alt+Shift+R` brings the agents back).
  Until then the pane mirror shows the amber "plain 1 Hz mirror" banner.
- Multiple phones/tabs can watch at once; replay rings are only sent for
  the pane you're actually viewing, so herd view costs almost nothing.
- **Renamed from scry**: this was built and briefly run as "scry" — if you
  have an old `~/.config/scry` or `~/.local/state/scry` lying around,
  `install.sh` migrates it to the `astrolabe` paths above automatically on
  first run, and retires the old `scry.service` unit.
