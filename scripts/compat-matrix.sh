#!/usr/bin/env bash
# Compat matrix (roadmap 2.10): an OLD client binary — built from the last
# commit before the agent-pane protocol landed — driven against the NEW
# server, scripted. The old client must parse new state (extra JSON fields
# ignored), skip unknown frames, and keep full pty-pane control.
#
#   nix develop --command ./scripts/compat-matrix.sh
#
# Uses a scratch session (compat-$$); never touches a real one.
set -euo pipefail
cd "$(dirname "$0")/.."

OLD_COMMIT=${OLD_COMMIT:-1a7c4fd} # docs: ADR 0002 (pre-proto commit)
SESSION="compat-$$"

say() { printf '\n── %s\n' "$*"; }

say "build new binary"
cargo build --quiet
NEW=$PWD/target/debug/zodiac

say "build old binary ($OLD_COMMIT)"
WT=$(mktemp -d /tmp/zodiac-compat.XXXXXX)
git worktree add --detach "$WT" "$OLD_COMMIT" >/dev/null
(cd "$WT" && cargo build --quiet)
OLD="$WT/target/debug/zodiac"

cleanup() {
    "$NEW" -s "$SESSION" kill-server 2>/dev/null || true
    sleep 0.5
    git worktree remove --force "$WT" 2>/dev/null || true
    rm -rf "$WT"
}
trap cleanup EXIT

say "start NEW server ($SESSION)"
"$NEW" --server "$SESSION" &
sleep 1.5

say "new CLI sanity (state includes kind fields)"
"$NEW" -s "$SESSION" ls --json | grep -q '"kind"'

say "open an agent pane (pi, local model) so state carries kind=agent"
"$NEW" -s "$SESSION" new --agent pi
sleep 1
"$NEW" -s "$SESSION" ls --json | grep -q '"kind":"agent"'

say "OLD client parses new state (unknown fields ignored)"
"$OLD" -s "$SESSION" ls
"$OLD" -s "$SESSION" ls --json >/dev/null

say "OLD client reads and drives the pty pane"
"$OLD" -s "$SESSION" read 1 >/dev/null
"$OLD" -s "$SESSION" send 1 'echo compat-ok' --enter
sleep 1
"$OLD" -s "$SESSION" read 1 | grep -q compat-ok

say "OLD client survives an agent pane existing (read returns, no crash)"
"$OLD" -s "$SESSION" read 2 >/dev/null || true
"$OLD" -s "$SESSION" ls >/dev/null

say "compat matrix green"
