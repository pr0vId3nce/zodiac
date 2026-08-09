# ADR 0002 — Structured agent surfaces: claude stream-json + pi rpc (Spike S2)

Status: accepted · 2026-08-09 · roadmap Phase 2, spike S2

## Context

Phase 2 replaces screen-scraping heuristics with real agent events. The spike had
to verify, against live binaries on this machine, what structured surface each
agent offers and whether permission prompts can be intercepted.

## claude sub-spike (pinned: claude 2.1.226)

All verified live, headless:

- **Spawn**: `claude -p --input-format stream-json --output-format stream-json
  --verbose --include-partial-messages --permission-prompt-tool stdio`
  (`--verbose` is mandatory with `-p` + stream-json output; `--permission-mode`
  optional on top, `manual` forces prompting).
- **Output events** (NDJSON, one object/line): `system/init` (session_id, model,
  tools, permissionMode, slash_commands…), `stream_event` (raw API deltas:
  message_start, content_block_delta text/thinking, message_stop),
  `assistant` / `user` message envelopes (complete blocks incl. tool_use +
  tool_result), `rate_limit_event`, terminal `result` (subtype success/error,
  terminal_reason, session_id, total_cost_usd, usage, permission_denials).
- **Input**: `{"type":"user","message":{"role":"user","content":[…]}}` lines.
- **Permission interception** (the 2.5 mechanism): with `--permission-prompt-tool
  stdio`, a gated tool emits
  `{"type":"control_request","request_id":…,"request":{"subtype":"can_use_tool",
  "tool_name":"Write","display_name":…,"input":{…},"permission_suggestions":[…]}}`;
  the embedder answers
  `{"type":"control_response","response":{"request_id":…,"subtype":"success",
  "response":{"behavior":"allow"|"deny","updatedInput":…}}}`. Verified: Write
  paused until the allow arrived, then executed. **Allowlisted tools (user
  settings) bypass the prompt tool entirely** — the user's existing allow rules
  keep working; zodiac only sees genuinely undecided calls.
- **Resume**: `claude -p --resume <session-id>` continues the same session_id
  with full context (verified). This replaces keystroke-injection autoresume for
  agent panes (task 2.7).

## pi (pinned: pi 0.84.1)

**pi does not stay a heuristic pty pane — it has a structured surface.**

- `--mode json -p "…"`: NDJSON out: `session` (version 3, id, cwd),
  `agent_start`, `turn_start`, `message_start/update/end` with
  `assistantMessageEvent` deltas (`thinking_start/delta`, `text_start/delta`…),
  provider/model/usage on assistant message_start.
- `--mode rpc`: bidirectional — NDJSON commands on stdin
  (`{"type":"prompt","message":…}` → ack `{"type":"response","command":"prompt",
  "success":true}`), same event stream out. Session resume: `--session <id>` /
  `--session-id <id>`.
- **Permissions**: pi executes its tools without permission gates by design
  (same as its own TUI), so pi agent panes get transcript/status/resume but no
  permission inbox in v1. Revisit trigger: a zodiac-shipped pi *extension* could
  intercept tool calls and forward them into `T_PERM_REQ` — icebox until the
  claude path has proven the inbox UX.

## Event mapping (zodiac `T_AGENT_EVENT` payload = agent-native NDJSON line,
tagged with the agent kind; the server does not re-encode)

| zodiac concern | claude | pi |
|---|---|---|
| session id for resume | `system/init.session_id`, `result.session_id` | `session.id` |
| working/idle status | stream_event message_start…message_stop | `agent_start`/`turn_start`…`turn_end`/`agent_end` |
| streamed text | `content_block_delta.text_delta` | `message_update.text_delta` |
| tool calls | `assistant` blocks `tool_use` / `user` blocks `tool_result` | `toolcall_*` events |
| permission | `control_request` can_use_tool | none (v1) |
| API failure | `result` subtype error / api_error_status | provider error events |

## Decision

Agent panes spawn over **pipes, no PTY, no VT engine**, speak the agent's native
NDJSON both ways, and the server relays raw lines to clients (`T_AGENT_EVENT`)
while parsing just enough for: session capture, status, permission routing, and
retry. Both claude and pi get `kind=agent` panes in Phase 2 (task 2.9 = pi wiring
over the same runtime, using `--mode rpc`).
