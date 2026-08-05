#!/usr/bin/env bash
# Rebuild a zodiac session from the snapshot the server writes once a minute
# (<state dir>/snapshot.json, rotated to snapshot.prev.json at startup).
#
# zodiac already restores panes, their directories and their scrollback on
# its own. What it cannot restore is the agent that was running inside each
# pane — this script does that: it reopens claude on the same conversation
# (`claude --resume <chat id>`) and relaunches other agents in place.
#
# usage: zodiac-restore.sh [-s session] [--from file] [--dry-run]
#
# Start zodiac first (the server must be running), then run this from any
# other terminal — or let it run for you at login, after `zodiac` is up.
set -euo pipefail

session=main
from=
dry=0
while [ $# -gt 0 ]; do
  case "$1" in
    -s|--session) session="${2:?-s needs a session name}"; shift 2 ;;
    --from) from="${2:?--from needs a file}"; shift 2 ;;
    -n|--dry-run) dry=1; shift ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/zodiac/$session"
if [ -z "$from" ]; then
  # snapshot.prev.json is the session as of the last shutdown; snapshot.json
  # is the live one (only useful if the server is still the same process).
  for f in "$state_dir/snapshot.prev.json" "$state_dir/snapshot.json"; do
    if [ -f "$f" ]; then from="$f"; break; fi
  done
fi
[ -n "$from" ] && [ -f "$from" ] || {
  echo "no snapshot found (looked in $state_dir)" >&2; exit 1
}

# Fields are separated by US (0x1f), not tab: bash's `read` treats tab as
# whitespace and would collapse the empty fields (a pane with no agent) into
# one, shifting every field after it.
SEP=$'\037'

# Flatten a zodiac JSON document (a snapshot, or `zodiac ls --json` — same
# pane shape) to: index, cwd, agent, chat_id, name, renamed.
panes_fields() {
  if command -v jq >/dev/null 2>&1; then
    jq -r '.panes[] | [(.index|tostring), (.cwd // ""), (.agent // ""),
                       (.chat_id // ""), (.name // ""),
                       (if .renamed then "1" else "0" end)] | join("\u001f")'
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c '
import json, sys
for p in json.load(sys.stdin).get("panes", []):
    print("\x1f".join([str(p.get("index", "")), p.get("cwd") or "", p.get("agent") or "",
                       p.get("chat_id") or "", p.get("name") or "",
                       "1" if p.get("renamed") else "0"]))'
  else
    echo "need jq or python3 to read the snapshot" >&2; exit 1
  fi
}

zodiac ls --json -s "$session" >/dev/null 2>&1 || {
  echo "no zodiac server for session '$session' — start it with \`zodiac $session\` first" >&2
  exit 1
}

run() {
  if [ "$dry" = 1 ]; then printf '  would run: zodiac'; printf ' %q' "$@"; echo
  else zodiac -s "$session" "$@"; fi
}

live=$(zodiac ls --json -s "$session" | panes_fields)
live_count=$(printf '%s\n' "$live" | grep -c . || true)

while IFS="$SEP" read -r index cwd agent chat name renamed; do
  [ -n "$index" ] || continue
  live_line=$(printf '%s\n' "$live" | awk -F"$SEP" -v i="$index" '$1 == i')
  live_cwd=$(printf '%s' "$live_line" | cut -d"$SEP" -f2)
  live_agent=$(printf '%s' "$live_line" | cut -d"$SEP" -f3)

  if [ "$index" -gt "${live_count:-0}" ]; then
    echo "pane $index: opening"
    run new
    sleep 0.4
    live_cwd=
  fi

  # Already running the right agent (re-run of this script, or the pane
  # survived) — leave it alone rather than stacking a second agent on it.
  if [ -n "$agent" ] && [ "$agent" = "$live_agent" ]; then
    echo "pane $index: $agent already running, skipping"
    continue
  fi

  cmd=
  if [ -n "$cwd" ] && [ "$cwd" != "$live_cwd" ]; then
    cmd="cd $(printf '%q' "$cwd")"
  fi
  if [ -n "$agent" ]; then
    case "$agent" in
      claude) if [ -n "$chat" ]; then a="claude --resume $chat"; else a="claude"; fi ;;
      *)      a="$agent" ;;
    esac
    if [ -n "$cmd" ]; then cmd="$cmd && $a"; else cmd="$a"; fi
  fi

  if [ -n "$cmd" ]; then
    echo "pane $index: $cmd"
    run send "$index" "$cmd" --enter
    sleep 0.5
  fi

  # Names zodiac auto-derives come back on their own; only pinned ones
  # (Alt+R) need putting back.
  if [ "$renamed" = 1 ] && [ -n "$name" ]; then
    run rename "$index" "$name"
  fi
done <<< "$(panes_fields < "$from")"

echo "restored from $from"
