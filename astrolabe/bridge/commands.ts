// Slash-command palette entries: a curated set of Claude Code built-ins,
// plus every custom command found in ~/.claude/commands/*.md (name from the
// filename, description from frontmatter `description:` or the first line).

import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

export interface SlashCommand {
  name: string; // includes the leading slash
  desc: string;
  custom?: boolean;
}

const BUILTINS: SlashCommand[] = [
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

export function scanCommands(): SlashCommand[] {
  const out = [...BUILTINS];
  const dir = path.join(os.homedir(), ".claude", "commands");
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
