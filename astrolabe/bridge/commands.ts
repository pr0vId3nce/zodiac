// Slash-command palette entries, per agent: a curated set of that agent's
// built-ins plus every user-defined command found on disk (name from the
// filename, description from frontmatter `description:` or the first line).
// Claude Code keeps those in ~/.claude/commands/*.md and pi its prompt
// templates in ~/.pi/agent/prompts/*.md — same shape, different folder.

import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

export interface SlashCommand {
  name: string; // includes the leading slash
  desc: string;
  custom?: boolean;
}

/** Palettes keyed by the agent name zodiac reports for a pane. */
export type CommandSets = Record<string, SlashCommand[]>;

const CLAUDE_BUILTINS: SlashCommand[] = [
  { name: "/compact", desc: "Compact the conversation, keep a summary" },
  { name: "/clear", desc: "Clear conversation history" },
  { name: "/resume", desc: "Resume a previous session" },
  { name: "/rewind", desc: "Rewind conversation / code changes" },
  { name: "/model", desc: "Switch model" },
  { name: "/fast", desc: "Toggle fast mode" },
  { name: "/plan", desc: "Enter plan mode" },
  { name: "/review", desc: "Review a pull request" },
  { name: "/code-review", desc: "Review the current branch" },
  { name: "/context", desc: "Show context usage" },
  { name: "/cost", desc: "Show token usage and cost" },
  { name: "/usage", desc: "Show plan usage limits" },
  { name: "/todos", desc: "List current todos" },
  { name: "/memory", desc: "Edit memory files" },
  { name: "/init", desc: "Create CLAUDE.md for this repo" },
  { name: "/permissions", desc: "View or update permissions" },
  { name: "/agents", desc: "Manage subagents" },
  { name: "/mcp", desc: "Manage MCP servers" },
  { name: "/hooks", desc: "Manage hooks" },
  { name: "/config", desc: "Open config panel" },
  { name: "/status", desc: "Show session status" },
  { name: "/doctor", desc: "Check Claude Code installation" },
  { name: "/export", desc: "Export the conversation" },
  { name: "/help", desc: "Show help" },
];

// pi's BUILTIN_SLASH_COMMANDS, in its own order. Several of these open a
// picker rather than acting outright (/model, /resume, /tree) — the phone
// can drive those with the arrow keypad once they're on screen.
const PI_BUILTINS: SlashCommand[] = [
  { name: "/new", desc: "Start a new session" },
  { name: "/resume", desc: "Resume a different session" },
  { name: "/session", desc: "Show session info and stats" },
  { name: "/name", desc: "Set session display name" },
  { name: "/tree", desc: "Navigate session tree (switch branches)" },
  { name: "/fork", desc: "Create a new fork from a previous user message" },
  { name: "/clone", desc: "Duplicate the current session at the current position" },
  { name: "/compact", desc: "Manually compact the session context" },
  { name: "/model", desc: "Select model (opens selector UI)" },
  { name: "/scoped-models", desc: "Enable/disable models for Ctrl+P cycling" },
  { name: "/copy", desc: "Copy last agent message to clipboard" },
  { name: "/export", desc: "Export session (HTML default, or a .html/.jsonl path)" },
  { name: "/import", desc: "Import and resume a session from a JSONL file" },
  { name: "/share", desc: "Share session as a secret GitHub gist" },
  { name: "/trust", desc: "Save project trust decision for future sessions" },
  { name: "/login", desc: "Configure provider authentication" },
  { name: "/logout", desc: "Remove provider authentication" },
  { name: "/settings", desc: "Open settings menu" },
  { name: "/reload", desc: "Reload keybindings, extensions, skills, prompts, themes" },
  { name: "/hotkeys", desc: "Show all keyboard shortcuts" },
  { name: "/changelog", desc: "Show changelog entries" },
  { name: "/quit", desc: "Quit pi" },
];

/** Every agent's palette, built-ins plus whatever is on disk. */
export function scanCommands(): CommandSets {
  return {
    claude: [...CLAUDE_BUILTINS, ...scanDir(path.join(os.homedir(), ".claude", "commands"))],
    pi: [...PI_BUILTINS, ...scanDir(path.join(os.homedir(), ".pi", "agent", "prompts"))],
  };
}

/** The `*.md` command/template files in one directory, alphabetically. */
function scanDir(dir: string): SlashCommand[] {
  const out: SlashCommand[] = [];
  let entries: string[] = [];
  try {
    entries = fs.readdirSync(dir);
  } catch {
    return out;
  }
  for (const f of entries.sort()) {
    if (!f.endsWith(".md")) continue;
    const name = "/" + f.slice(0, -3);
    let desc = "custom command";
    try {
      const text = fs.readFileSync(path.join(dir, f), "utf8");
      const m = text.match(/^description:\s*(.+)$/m);
      if (m) {
        desc = m[1].trim().replace(/^["']|["']$/g, "");
      } else {
        const line = text
          .replace(/^---[\s\S]*?---\s*/m, "")
          .split("\n")
          .map((l) => l.trim())
          .find((l) => l.length > 0);
        if (line) desc = line.replace(/^#+\s*/, "").slice(0, 80);
      }
    } catch {
      /* unreadable file — keep placeholder desc */
    }
    out.push({ name, desc, custom: true });
  }
  return out;
}
