# Astrolabe iOS shell

A thin native wrapper around the Astrolabe web UI: a full-bleed `WKWebView`
pointed at the bridge, plus the two things a PWA can't do well on iOS —

- **APNs push** when an agent needs you or finishes (time-sensitive, badge =
  number of panes waiting), and
- **inline reply from the lock screen**: long-press the notification, dictate
  or type, Send — it POSTs straight to the bridge's `/api/prompt`, no app
  launch needed.

Tapping a notification opens the app on that pane. The bridge URL is editable
in Settings.app → Astrolabe (default `http://100.118.22.13:7979`).

Everything here is text; the Xcode project and the icon are generated. You
need a Mac with Xcode 15+ to build (nothing on this laptop can).

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
curl -s http://100.118.22.13:7979/healthz   # expect "push": true
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

First launch asks for notification permission; on grant the app registers its
device token with the bridge (`/healthz` shows `"devices": 1`).

## Test

```
curl -s -X POST http://100.118.22.13:7979/api/push-test -d '{}'
```

Phone should buzz within a second or two. Then ask an agent something and
lock the phone — when it flips to *needs you*, long-press the notification
and answer from the lock screen.

## Notes

- Development-signed builds run for 1 year on a paid account before needing a
  re-install from Xcode.
- Notification pushes only fire on status *transitions* observed by the
  bridge, with a 30 s per-pane cooldown; restarting the bridge never
  re-notifies existing states.
- The app deliberately holds no state beyond the bridge URL — the web UI
  stays the single source of truth for features.
