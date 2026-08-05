# Mac/Xcode handoff

This work was written entirely on a Linux box with no Swift toolchain — the
Swift in `Sources/` and `Resources/Settings.bundle/Root.plist` has never been
compiled or run. Brace/paren balance was checked by hand; nothing more. Full
build-and-feel verification is the point of this handoff.

## Get the code

```
git clone https://github.com/pr0vId3nce/zodiac.git
cd zodiac/astrolabe/ios
```

(If you already have a clone: `git pull`.) The repo is public, so no auth is
needed to clone.

## What's new and unverified since the last time this ran on a Mac

Everything under **Phase D** of the working plan, all written but never
built:

- **Token auth wiring** — `Bridge.swift` adds a `token` accessor reading the
  new `bridge_token` Settings.bundle field, sends it as
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
- **Genericized defaults** — `Bridge.swift`'s `defaultURL` and the
  Settings.bundle `bridge_url` default are now `http://127.0.0.1:7979`
  (deliberately unreachable loopback, not the real tailscale IP that used to
  be hardcoded here — see the comment in `Bridge.swift` for why it's a valid
  URL string rather than empty).

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

**Token round-trip** — set `ASTROLABE_TOKEN` on the bridge first
(`astrolabe/README.md`'s Security section), then:
- [ ] iOS Settings.app → Astrolabe → paste the token into the new Token
      field → app loads the pane list (not a 401/blank screen).
- [ ] Web: open `http://<bridge-ip>:7979/#/?t=<token>` in mobile Safari,
      confirm it connects, confirm the URL bar cleans the `?t=` back off.
- [ ] Both wrong-token and no-token cases show the "unauthorized" banner
      instead of silently hanging.

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
first build. Check `Sources/WebView.swift`'s new `Coordinator` methods first
(the `WKScriptMessageHandler` conformance and the `owner` back-reference are
the newest, least-battle-tested code in this pass) before assuming the bug
is somewhere older.
