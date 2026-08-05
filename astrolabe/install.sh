#!/usr/bin/env bash
# Build Astrolabe and install it as a background service:
# systemd --user on Linux, a launchd LaunchAgent on macOS.
set -euo pipefail
cd "$(dirname "$0")"

echo "── bridge deps"
(cd bridge && npm install --no-fund --no-audit)

echo "── web build"
(cd web && npm install --no-fund --no-audit && npm run build)

if [ "$(uname)" = "Darwin" ]; then
  # launchd starts agents with a bare PATH, so bake in the absolute node
  # path found right now (Node ≥ 23 for native type stripping).
  NODE="$(command -v node)"
  PLIST=~/Library/LaunchAgents/dev.d3s.astrolabe.plist
  mkdir -p ~/Library/LaunchAgents
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.d3s.astrolabe</string>
  <key>ProgramArguments</key>
  <array>
    <string>${NODE}</string>
    <string>$(pwd)/bridge/main.ts</string>
  </array>
  <key>WorkingDirectory</key><string>$(pwd)/bridge</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/astrolabe.log</string>
  <key>StandardErrorPath</key><string>/tmp/astrolabe.log</string>
</dict>
</plist>
EOF
  launchctl bootout "gui/$(id -u)/dev.d3s.astrolabe" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
  sleep 2
  port="${ASTROLABE_PORT:-7979}"
  ip="$(tailscale ip -4 2>/dev/null | head -1 || /Applications/Tailscale.app/Contents/MacOS/Tailscale ip -4 2>/dev/null | head -1 || true)"
  echo
  echo "astrolabe is up (launchd): http://${ip:-<tailscale-ip>}:${port}"
  echo "logs: /tmp/astrolabe.log · env overrides: edit $PLIST (EnvironmentVariables dict)"
  exit 0
fi

echo "── migrating config/state from the old scry name, if present"
if [ -d ~/.config/scry ] && [ ! -d ~/.config/astrolabe ]; then
  cp -r ~/.config/scry ~/.config/astrolabe
  echo "   copied ~/.config/scry -> ~/.config/astrolabe"
fi
if [ -d ~/.local/state/scry ] && [ ! -d ~/.local/state/astrolabe ]; then
  cp -r ~/.local/state/scry ~/.local/state/astrolabe
  echo "   copied ~/.local/state/scry -> ~/.local/state/astrolabe"
fi

echo "── retiring the old scry.service unit, if enabled"
if systemctl --user list-unit-files scry.service &>/dev/null; then
  systemctl --user disable --now scry.service 2>/dev/null || true
  rm -f ~/.config/systemd/user/scry.service
  systemctl --user daemon-reload
fi

echo "── systemd unit"
mkdir -p ~/.config/systemd/user
cp astrolabe.service ~/.config/systemd/user/astrolabe.service
systemctl --user daemon-reload
systemctl --user enable --now astrolabe.service
sleep 2
systemctl --user --no-pager status astrolabe.service | head -5 || true

port="${ASTROLABE_PORT:-7979}"
ip="$(tailscale ip -4 2>/dev/null | head -1 || true)"
echo
echo "astrolabe is up: http://${ip:-<tailscale-ip>}:${port}"
echo "open it on your phone (same tailnet), add to home screen."
