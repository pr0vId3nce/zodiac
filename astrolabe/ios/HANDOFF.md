# Mac/Xcode handoff

This work was written entirely on a Linux box with no Swift toolchain — the
Swift in `Sources/` has never been compiled or run. Brace/paren balance was
checked by hand; nothing more. Full build-and-feel verification is the
point of this handoff.

## Get the code

```
git clone https://github.com/pr0vId3nce/zodiac.git
cd zodiac/astrolabe/ios
```

(If you already have a clone: `git pull`.) The repo is public, so no auth is
needed to clone.

## What's new and unverified since the last time this ran on a Mac

### Multi-computer QR pairing (latest pass)

The app's whole shape changed: it used to be one WKWebView permanently
pointed at one bridge configured in Settings.app. Now the first screen is a
**list of paired computers** (`ComputerListView.swift`, `ComputerStore.swift`
— UserDefaults-backed, keyed by a stable `cid` each bridge mints once and
persists), and tapping one pushes to a `WebView` parameterized by that
`Computer` instead of a single global `Bridge.baseURL`/`Bridge.token`.

Pairing is QR-driven: zodiac itself (the Rust TUI, Alt+P on its home page)
draws a QR encoding a magic-link-shaped URL — `http://<bridge>/?t=<token>&cid=<id>&name=<host>`.
**Scan QR** (`QRScannerView.swift`, `AVCaptureMetadataOutput`) decodes it,
`Computer.parse` pulls the three params out, `ComputerStore.upsert` adds it
or — matched by `cid` — updates an already-paired entry in place (this is
how a rescanned, rotated token replaces a stale one without duplicating the
list entry). **Enter Manually** (`ComputerListView.swift`'s
`ManualAddView`) is the fallback for when the camera can't see the screen.

Every file that used to assume a single global bridge now takes an
explicit `Computer`: `Bridge.swift` (every function gains an `on computer:`
param, `token` is always sent now rather than being optional), `WebView.swift`
(per-computer `loadURL`/host-restriction), `AppDelegate.swift` (loops every
paired computer to register push, keyed by `cid` to route replies/opens to
the right one — the bridge now tags each push's payload with its own `cid`,
see `astrolabe/bridge/main.ts`'s `maybePush`), `Router.swift` (carries
`pendingCid` alongside `pendingPane`). `Resources/Settings.bundle/` is gone
entirely — there's nothing left in it to hand-edit; pairing happens by
scanning, not by typing into Settings.app.

None of this — the camera permission prompt, `AVCaptureSession` actually
decoding a QR, the `NavigationStack` push/pop between the list and a
computer's web view, or the multi-computer push routing — has run once.

### Phase D (prior pass)

Everything below, all written but never built:

- **Token auth wiring** (superseded by the multi-computer pass above —
  `Bridge.swift` no longer has a `token`/`bridge_token` accessor at all,
  every call takes an explicit `Computer` instead; kept here for history) —
  originally: `Bridge.swift` adds a `token` accessor reading the
  `bridge_token` Settings.bundle field, sends it as
  `Authorization: Bearer <token>`; `WebView.swift`'s load URL appends
  `?t=<token>` the same way the web magic-link flow does.
- **Real haptics** — `WebView.swift` adds a `WKScriptMessageHandler` named
  `"astrolabe"`. `web/src/native.ts` (already shipped, previously a no-op
  fallback everywhere since no native bridge existed) posts
  `{type:"haptic", kind}` messages to it; the handler dispatches to
  `UIImpactFeedbackGenerator`/`UINotificationFeedbackGenerator`/
  `UISelectionFeedbackGenerator` depending on `kind`. This is the first time
  any of that code path has run on real hardware.
- **Navigation policy** — `WebView.swift` adds a `decidePolicyFor` delegate
  method restricting navigation to the configured bridge host.
- **Genericized defaults** (also superseded — there's no more single
  `defaultURL`/Settings.bundle default; each `Computer` carries its own
  `url`, obtained by pairing, never hardcoded) — originally: `Bridge.swift`'s
  `defaultURL` and the Settings.bundle `bridge_url` default were changed
  from a real tailscale IP to `http://127.0.0.1:7979`, a deliberately
  unreachable loopback placeholder.

Also worth knowing: this is the **first Xcode build of everything from the
prior PWA-polish pass** too (screen transitions, swipe-back, pinch-zoom,
bottom sheet animation, status bar chrome) — none of that has been felt on a
real device either, only tested in a desktop browser's responsive mode.

## Build

```
brew install xcodegen
node scripts/gen-icon.mjs
xcodegen generate
open Astrolabe.xcodeproj
```

Signing & Capabilities → pick your team. Plug in a real device (haptics
don't work in the Simulator) → Run.

## Verification checklist

**QR pairing** (do this section first — everything else needs at least one
paired computer):
- [ ] Fresh install, empty list: `ComputerListView`'s empty state shows,
      not a crash or a blank screen.
- [ ] "+" → **Scan QR**: camera permission prompt appears (first time only)
      — confirm `NSCameraUsageDescription`'s wording actually shows up
      rather than an instant crash (a missing/malformed Info.plist key
      crashes on first camera access rather than prompting).
- [ ] On a computer's zodiac session, press **Alt+P** on the home page —
      confirm a QR actually renders (this itself is unverified from the
      Linux dev box, see the Phase 2 note in the main plan) — then scan it.
      Confirm the app adds it to the list and opens straight to its Herd
      view (not a 401/blank screen).
- [ ] Quit and relaunch the app — the paired computer is still in the list
      (UserDefaults persistence).
- [ ] Restart `zodiac` on that computer (fresh process, not just
      detach/reattach) — its Alt+P QR should now encode a different token.
      Try connecting with the *old* session (don't rescan) — confirm it
      goes unauthorized. Rescan the new QR — confirm it updates the
      *existing* list entry in place rather than adding a duplicate (same
      `cid`, matched in `ComputerStore.upsert`).
- [ ] **Enter Manually**: type a bridge URL + token by hand (e.g. copied
      from `curl .../healthz` reachability plus a token obtained some other
      way) — confirm it adds an entry and opens correctly.
- [ ] Swipe-to-delete a computer — confirm it's gone from the list, and
      (if push is set up — see below) that bridge's `/healthz` drops back
      to `"devices": 0` shortly after (best-effort unregister).

**Multi-computer**, with at least two paired:
- [ ] Both appear in the list; opening each shows *that* computer's panes,
      not a stale/mixed view from the other one.
- [ ] Trigger a push from computer A while viewing computer B (or with the
      app backgrounded) — tapping it opens computer A specifically, not
      whichever one happened to be showing (`AppDelegate`'s `cid`-based
      routing through `Router.pendingCid`/`ComputerStore.computer(cid:)`).
- [ ] Lock-screen inline reply on a computer-A notification lands on
      computer A's bridge, confirmed by checking that pane's actual
      transcript, not just "some bridge accepted the POST."
- [ ] Grant notification permission with computer A already paired, *then*
      pair computer B — confirm B also ends up registered for push without
      needing an app relaunch (`ComputerStore.upsert`'s immediate-register
      path, since `didRegisterForRemoteNotificationsWithDeviceToken` won't
      fire again this session).

**Haptics** (real device only):
- [ ] Send a message from the composer → feel a tap.
- [ ] Watch a pane flip to *needs input* while viewing it → feel the
      stronger notification buzz.
- [ ] Cross the swipe-back commit threshold → feel a light tap at the
      threshold, not just at gesture end.
- [ ] KeyPad taps and slash-command picks each give a light tap.
- [ ] None of the above throw a console error if `native.ts`'s message
      shape ever drifts from what `WebView.swift` expects — check Xcode's
      console while doing this pass.

**Navigation policy**:
- [ ] Confirm normal in-app navigation (pane switches, sheet opens) still
      works — the host-restriction delegate should be transparent to
      same-origin navigation and only block anything that tries to leave
      the bridge's host.

**Everything from the previous PWA pass, first real-device pass**:
- [ ] Herd ⇄ Pane transition feels like a real iOS push/pop, not a jump cut.
- [ ] Swipe-back: edge-only arm zone, direction lock doesn't fight vertical
      scroll or the mirror's horizontal scroll, live parallax reveal of Herd
      underneath during the drag, correct commit/snap-back threshold feel.
- [ ] Bottom sheet opens and *closes* with an animation (this was the bug
      that got fixed right before this handoff was written).
- [ ] Pinch-to-zoom on the terminal: smooth live preview, commits to a
      real discrete font size on release, doesn't re-render xterm.js every
      frame (watch for jank on a slower device).
- [ ] Status bar overlays the app background (`black-translucent`) instead
      of showing as a separate opaque bar; content doesn't draw under it.
- [ ] `prefers-reduced-motion` (iOS Settings → Accessibility → Motion →
      Reduce Motion): transitions/animations should shorten to near-instant.
- [ ] Lock-screen inline reply: long-press an "agent needs you" push
      notification, type or dictate, Send — confirm it lands in the pane
      without opening the app. (Needs `ASTROLABE_APNS_*` creds configured
      first — see `ios/README.md`'s Apple-developer-setup section; skip
      this item if push isn't set up yet.)

## If something's broken

Nothing here has ever compiled — expect at least a few Xcode diagnostics on
first build. Check the multi-computer files first, since they're the
newest, least-battle-tested code in this pass: `ComputerStore.swift`
(actor-isolation — it's `@MainActor`, called from `AppDelegate`'s
notification-delegate methods which aren't automatically MainActor-isolated,
hence the explicit `await`/`Task { @MainActor in ... }` there),
`QRScannerView.swift` (the `AVCaptureSession` setup — a bad camera
permission state or a device with no back camera are the likely first
failure modes), and `WebView.swift`'s `Coordinator` (the
`WKScriptMessageHandler` conformance and the `owner` back-reference).
