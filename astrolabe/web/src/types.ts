export interface PaneState {
  index: number;
  id: number;
  name: string;
  /** "pty" (a shell/TUI mirrored through the VT engine) or "agent" (a
      structured NDJSON pane — chat view, no terminal). Missing on an old
      server = "pty". */
  kind?: "pty" | "agent";
  title: string;
  status: "working" | "idle" | "done" | "needs_input";
  agent: string | null;
  cwd: string | null;
  focused: boolean;
  auto_resume: boolean;
  uptime_ms: number;
  version: string | null;
  thinking: boolean;
  recap: string | null;
  subtitle: string | null;
  /** Parsed question dialog, merged in by the bridge while the pane is
      needs_input with a numbered picker on screen (bridge/question.ts). */
  question?: string;
  options?: string[];
}

export interface SessionState {
  session: string;
  attached: boolean;
  rows?: number;
  cols?: number;
  panes: PaneState[];
}

export interface SlashCommand {
  name: string;
  desc: string;
  custom?: boolean;
}

/** Slash palettes keyed by agent name — each agent has its own commands. */
export type CommandSets = Record<string, SlashCommand[]>;

/** One pending tool-permission request from an agent pane (the server-side
    inbox is authoritative; requests expire there after 10 min as a deny). */
export interface PermRequest {
  request_id: string;
  tool_name: string;
  display_name?: string | null;
  /** The tool input as the agent sent it (raw JSON value). */
  input: unknown;
  /** Milliseconds pending when the bridge learned of it. */
  age_ms: number;
}

export type ServerMsg =
  | { t: "hello"; session: string; state: SessionState | null; link: boolean; watch: boolean | null; commands: CommandSets; perms?: { pane: number; req: PermRequest }[] }
  | { t: "state"; state: SessionState }
  | { t: "link"; up: boolean }
  | { t: "watch"; supported: boolean }
  | { t: "replay"; pane: number; data: string }
  | { t: "output"; pane: number; data: string }
  | { t: "screen"; pane: number; text: string }
  | { t: "pane_closed"; pane: number }
  /** NDJSON transcript lines for an agent pane (only sent while viewed). */
  | { t: "agent_event"; pane: number; lines: string[] }
  | { t: "perm_req"; pane: number; req: PermRequest }
  /** A permission was settled — here, elsewhere, or by timeout. */
  | { t: "perm_resolved"; pane: number; request_id: string; how: string };

export function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
