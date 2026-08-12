#!/usr/bin/env bash
# Package zodiac-gui as a real macOS .app.
#
# A bare Mach-O run from a shell is a second-class citizen on macOS: it gets
# a generic Dock tile, no icon, no bundle identity, and the window server
# treats activation and focus differently. A bundle is most of what makes
# the GUI read as a Mac app rather than a ported binary.
#
# The `zodiac` server binary is copied in beside the GUI on purpose:
# `server_binary()` in zodiac-gui/src/main.rs looks for a sibling of the
# running executable first, so a self-contained bundle starts its own
# session without needing anything on PATH.
#
# Usage: scripts/bundle-macos-app.sh [output-dir]   (default: target/release)
set -euo pipefail

if [ "$(uname)" != "Darwin" ]; then
  echo "macOS only" >&2
  exit 1
fi

REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$REPO/target/release}"
APP="$OUT/zodiac.app"
GUI="$REPO/target/release/zodiac-gui"
SRV="$REPO/target/release/zodiac"

for bin in "$GUI" "$SRV"; do
  [ -x "$bin" ] || { echo "missing $bin — run: cargo build --release --workspace" >&2; exit 1; }
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$GUI" "$APP/Contents/MacOS/zodiac-gui"
cp "$SRV" "$APP/Contents/MacOS/zodiac"

# ---- icon: reuse the shipped zodiac mark -------------------------------
ICON_SRC="$REPO/astrolabe/web/public/icon-512.png"
if [ -f "$ICON_SRC" ] && command -v iconutil >/dev/null 2>&1; then
  SET="$(mktemp -d)/zodiac.iconset"
  mkdir -p "$SET"
  for sz in 16 32 64 128 256 512; do
    sips -z $sz $sz "$ICON_SRC" --out "$SET/icon_${sz}x${sz}.png" >/dev/null 2>&1
    d=$((sz * 2))
    sips -z $d $d "$ICON_SRC" --out "$SET/icon_${sz}x${sz}@2x.png" >/dev/null 2>&1
  done
  iconutil -c icns "$SET" -o "$APP/Contents/Resources/zodiac.icns" 2>/dev/null || true
  rm -rf "$(dirname "$SET")"
fi

# ---- Info.plist --------------------------------------------------------
# LSEnvironment is a hint, not a guarantee: Launch Services caches it and
# ignores it often enough that it cannot be relied on — measured here, an
# app opened from Finder still came up with a bare
# /usr/bin:/bin:/usr/sbin:/sbin. Harness detection therefore resolves its
# own search paths in agents.rs rather than trusting this. Kept because it
# costs nothing and helps when it is honoured.
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>zodiac</string>
  <key>CFBundleDisplayName</key><string>zodiac</string>
  <key>CFBundleIdentifier</key><string>dev.d3s.zodiac.gui</string>
  <key>CFBundleExecutable</key><string>zodiac-gui</string>
  <key>CFBundleIconFile</key><string>zodiac</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSEnvironment</key>
  <dict>
    <key>PATH</key><string>${HOME}/.local/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
</dict>
</plist>
EOF

# An ad-hoc signature is enough for local use and keeps Gatekeeper from
# re-verifying the whole bundle on every launch (which is what makes a
# freshly built binary's first run stall).
codesign --force --deep --sign - "$APP" 2>/dev/null || \
  echo "note: ad-hoc codesign failed; the app still runs" >&2

echo "built $APP"
echo "run:  open $APP           (or: $APP/Contents/MacOS/zodiac-gui [session])"
echo "note: drag it to /Applications if you want it in Spotlight."
