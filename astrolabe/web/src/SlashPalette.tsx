import { useState } from "react";
import type { SlashCommand } from "./types";
import { Sheet } from "./ui";
import { hapticTap } from "./native";

export function SlashPalette({
  open,
  onClose,
  commands,
  onPick,
}: {
  open: boolean;
  onClose: () => void;
  commands: SlashCommand[];
  onPick: (name: string) => void;
}) {
  const [filter, setFilter] = useState("");
  const q = filter.trim().toLowerCase().replace(/^\//, "");
  const list = commands.filter(
    (c) => !q || c.name.toLowerCase().includes(q) || c.desc.toLowerCase().includes(q)
  );
  return (
    <Sheet open={open} onClose={onClose} title="slash commands">
      <div className="px-2 pb-2">
        <input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="filter…"
          className="w-full rounded-lg border border-card-edge bg-sky-deep px-3 py-2 text-[16px] outline-none placeholder:text-zinc-600 focus:border-gold/50"
        />
      </div>
      <ul>
        {list.map((c) => (
          <li key={c.name}>
            <button
              onClick={(e) => {
                onPick(c.name);
                onClose();
                setFilter("");
                hapticTap("selection", e.currentTarget);
              }}
              className="flex min-h-11 w-full items-baseline gap-2 rounded-lg px-3 py-2.5 text-left active:bg-white/5"
            >
              <span className="shrink-0 font-mono text-sm text-gold-soft">{c.name}</span>
              {c.custom && (
                <span className="shrink-0 rounded bg-gold/15 px-1 text-[10px] text-gold">
                  custom
                </span>
              )}
              <span className="truncate text-xs text-zinc-500">{c.desc}</span>
            </button>
          </li>
        ))}
        {list.length === 0 && (
          <li className="px-3 py-4 text-center text-sm text-zinc-600">no matches</li>
        )}
      </ul>
    </Sheet>
  );
}
