# 🔭 Astrolabe

> *An astrolabe for your zodiac herd — read the sky, answer what needs you, over Tailscale.*

Open one URL on your phone, see which familiar is waiting on you, and answer
it with your phone's keyboard. Each pane gets a colored terminal mirror, a
slash-command palette, a special-keys pad, and scrollback you can search.
The reply box is an ordinary text field, so your phone's own voice dictation
works in it — Astrolabe ships none of its own.

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
- **web** (`web/`): Vite + React + Tailwind PWA with an xterm.js mirror.
- **ios** (`ios/`): thin native shell — a WKWebView around the same web UI
  plus APNs push ("agent needs you" with lock-screen **inline reply**, badge =
  panes waiting, tap opens the pane). Built with XcodeGen on a Mac; see
  `ios/README.md`. Push stays off until `ASTROLABE_APNS_*` creds exist in
  `~/.config/astrolabe/env`.

## Install

```
./install.sh          # npm install, vite build, enable systemd --user unit
```

Then open `http://<this-machine's-tailscale-ip>:7979` on your phone and add
it to the home screen.

Env knobs (set in `~/.config/systemd/user/astrolabe.service`):

| var | default | what |
| --- | --- | --- |
| `ASTROLABE_PORT` | `7979` | listen port |
| `ASTROLABE_SESSION` | `main` | zodiac session to mirror |
| `ASTROLABE_HOST` | tailscale IPv4 | bind address override |
| `ASTROLABE_APNS_KEY` | — | path to the `.p8` APNs auth key (enables push) |
| `ASTROLABE_APNS_KEY_ID` / `ASTROLABE_APNS_TEAM_ID` | — | from the developer portal |
| `ASTROLABE_APNS_TOPIC` | — | app bundle id (`dev.d3s.Astrolabe`) |
| `ASTROLABE_APNS_ENV` | `sandbox` | `production` for TestFlight builds |

The `ASTROLABE_APNS_*` values belong in `~/.config/astrolabe/env` (the unit
loads it via `EnvironmentFile=`), not in the unit file itself.

HTTP API (used by the iOS shell; the PWA uses the WebSocket):

- `POST /api/apns/register` / `unregister` `{token}` — device enrollment
  (stored in `~/.local/state/astrolabe/devices.json`, pruned when Apple
  reports a token dead)
- `POST /api/prompt` `{pane, text}` — send a reply (newline wiring for agent
  panes handled server-side); this is what lock-screen inline reply hits
- `POST /api/push-test` — push "the bridge can reach you" to every device

Pushes fire on status transitions only (→ `needs_input`, and `working` →
`done`), 30 s per-pane cooldown, and never on the first state after a bridge
restart.

## The UI

- **Herd view**: one card per pane — name, live status (`needs you` panes
  pulse red and float to the top), agent + version, the ✶ LLM subtitle and
  latest ⏺ transcript bullet, cwd, uptime. Tap a card to enter the pane.
- **Pane view**: read-only colored mirror of the real terminal (exact
  server-side grid, horizontal scroll + A−/A+ font buttons), 10k lines of
  scrollback, search with prev/next. The composer's send button submits
  text + Enter (`zodiac prompt` pacing); newlines in a multi-line dictation
  are sent as `\`+Enter so Claude Code keeps them as newlines.
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
  and pane layout are restored, agents revive with `claude --resume`).
  Until then the pane mirror shows the amber "plain 1 Hz mirror" banner.
- Multiple phones/tabs can watch at once; replay rings are only sent for
  the pane you're actually viewing, so herd view costs almost nothing.
- **Renamed from scry**: this was built and briefly run as "scry" — if you
  have an old `~/.config/scry` or `~/.local/state/scry` lying around,
  `install.sh` migrates it to the `astrolabe` paths above automatically on
  first run, and retires the old `scry.service` unit.
