# Astrolabe iOS shell

A native wrapper around the Astrolabe web UI, and the one thing genuinely
multi-computer: a list of every zodiac + bridge pair you've paired with,
each opening a full-bleed `WKWebView` on that computer's Herd/Pane UI. Plus
the things a PWA can't do well on iOS —

- **APNs push** when an agent needs you or finishes (time-sensitive, badge =
  number of panes waiting across every paired computer), and
- **inline reply from the lock screen**: long-press the notification, dictate
  or type, Send — it POSTs straight to that computer's bridge `/api/prompt`,
  no app launch needed.

Tapping a notification opens the app on that pane, on the right computer.

**Pairing**: on the computer list's "+" menu, **Scan QR** and point the
camera at zodiac's own pairing overlay (Alt+P on its home page — see the
main README) — that's the whole flow, no typing required. **Enter
Manually** is a fallback for when the camera can't see the screen (bad
lighting, the simulator, a remote/screenshared session).

Everything here is text; the Xcode project and the icon are generated. You
need a Mac with Xcode 15+ to build (nothing on this laptop can).

**First time building this on a Mac after a while?** See
[`HANDOFF.md`](./HANDOFF.md) — it has the current unverified-work status and
a real-device verification checklist to run through before trusting any of
it.

## One-time: Apple developer setup

1. **APNs auth key** — developer.apple.com → Certificates, Identifiers &
   Profiles → **Keys** → **+** → check *Apple Push Notifications service* →
   download `AuthKey_XXXXXXXXXX.p8` (downloadable exactly once) and note the
   **Key ID**. Your **Team ID** is on the Membership page.
2. Copy the key to the bridge host:
   ```
   scp AuthKey_XXXXXXXXXX.p8 ntp424:~/.config/astrolabe/AuthKey.p8
   ```

## Bridge side (this machine)

Create `~/.config/astrolabe/env` (the systemd unit reads it):

```
ASTROLABE_APNS_KEY=%h/.config/astrolabe/AuthKey.p8   # use the absolute path, %h shown for clarity
ASTROLABE_APNS_KEY_ID=XXXXXXXXXX
ASTROLABE_APNS_TEAM_ID=YYYYYYYYYY
ASTROLABE_APNS_TOPIC=dev.d3s.Astrolabe
ASTROLABE_APNS_ENV=sandbox
```

`ASTROLABE_APNS_ENV=sandbox` matches Xcode-installed (development-signed)
builds; switch to `production` if you ever distribute via TestFlight.
`chmod 600` the key and the env file, then:

```
systemctl --user daemon-reload && systemctl --user restart astrolabe
curl -s http://<bridge-tailscale-ip>:7979/healthz   # expect "push": true
```

## Mac side

```
brew install xcodegen
cd astrolabe/ios
node scripts/gen-icon.mjs      # writes the 1024px icon into the asset catalog
xcodegen generate
open Astrolabe.xcodeproj
```

In Xcode: Signing & Capabilities → pick your team (automatic signing registers
the `dev.d3s.Astrolabe` app id and its push capability for you; change
`bundleIdPrefix` in `project.yml` if you want a different id — keep
`ASTROLABE_APNS_TOPIC` in sync). Plug in the phone, Run.

First launch asks for notification permission; on grant, the app registers
its device token with every already-paired computer's bridge (each shows
`"devices": 1` on `/healthz`) — and with any computer paired *after* that,
immediately on scan/manual-add, not just on the next cold launch.

## Test

```
curl -s -X POST http://<bridge-tailscale-ip>:7979/api/push-test \
  -H "Authorization: Bearer $ASTROLABE_TOKEN" -d '{}'
```

(Omit the `-H` if you haven't set `ASTROLABE_TOKEN` on the bridge — see the
main README's security section.)

Phone should buzz within a second or two. Then ask an agent something and
lock the phone — when it flips to *needs you*, long-press the notification
and answer from the lock screen.

## Notes

- Development-signed builds run for 1 year on a paid account before needing a
  re-install from Xcode.
- Notification pushes only fire on status *transitions* observed by the
  bridge, with a 30 s per-pane cooldown; restarting the bridge never
  re-notifies existing states.
- The app's own state is just the paired-computer list (name, URL, token,
  cid) — the web UI stays the single source of truth for everything past
  that: which panes exist, their status, transcripts, all of it.
- Removing a computer from the list best-effort unregisters this device's
  push token from that computer's bridge too, so it stops trying to notify
  you about a computer you've dropped.
