#!/usr/bin/env bash
# Keep a headless zodiac session alive on a Mac you control from elsewhere.
#
# The astrolabe bridge (dev.d3s.astrolabe) is only half the story: it serves
# the phone, but it has nothing to serve until a zodiac *server* owns the
# session socket. Without this agent a rebooted Mac comes back with a
# healthy bridge reporting `"link":false, "panes":0` — the machine looks
# online and is completely uncontrollable.
#
# Usage: scripts/install-macos-agent.sh [session]   (default: main)
#        scripts/install-macos-agent.sh --uninstall [session]
set -euo pipefail

if [ "$(uname)" != "Darwin" ]; then
  echo "this installer is macOS-only; on Linux use systemd --user" >&2
  exit 1
fi

UNINSTALL=0
if [ "${1:-}" = "--uninstall" ]; then
  UNINSTALL=1
  shift
fi
SESSION="${1:-main}"
LABEL="dev.d3s.zodiac.${SESSION}"
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"

if [ "$UNINSTALL" = "1" ]; then
  launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
  rm -f "$PLIST"
  echo "removed ${LABEL}"
  exit 0
fi

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/target/release/zodiac"
if [ ! -x "$BIN" ]; then
  echo "no release binary at $BIN — run: cargo build --release --workspace" >&2
  exit 1
fi

LOGDIR="$HOME/Library/Logs"
mkdir -p "$HOME/Library/LaunchAgents" "$LOGDIR"

cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${BIN}</string>
    <string>--server</string>
    <string>${SESSION}</string>
  </array>
  <key>WorkingDirectory</key><string>${HOME}</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <!-- Panes spawn agents that must not be throttled while the Mac idles;
       this is the whole point of a machine you drive from a phone. -->
  <key>ProcessType</key><string>Interactive</string>
  <key>ThrottleInterval</key><integer>10</integer>
  <!-- launchd hands a bare PATH to its jobs, so an agent harness installed
       in /usr/local/bin or a Homebrew prefix would be invisible to every
       pane. Keep this in sync with where claude/pi actually live. -->
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>${HOME}/.local/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>StandardOutPath</key><string>${LOGDIR}/zodiac-${SESSION}.log</string>
  <key>StandardErrorPath</key><string>${LOGDIR}/zodiac-${SESSION}.log</string>
</dict>
</plist>
EOF

launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
sleep 2

echo "zodiac session '${SESSION}' is now a LaunchAgent (${LABEL})"
echo "logs: ${LOGDIR}/zodiac-${SESSION}.log"
echo
echo "note: LaunchAgents only run once someone is logged in. For a Mac you"
echo "      reboot remotely, turn on automatic login — otherwise the session"
echo "      (and the phone's view of it) waits at the login window."
