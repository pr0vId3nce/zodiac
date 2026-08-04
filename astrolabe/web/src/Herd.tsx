import { Moon, Sparkles } from "lucide-react";
import type { PaneState, SessionState } from "./types";
import { StatusChip, cn } from "./ui";

const ROMAN = ["I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII"];

function roman(n: number) {
  return ROMAN[n - 1] ?? String(n);
}

function uptime(ms: number) {
  const m = Math.floor(ms / 60000);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

function tail(p: string | null, n = 2) {
  if (!p) return "";
  const parts = p.replace(/\/+$/, "").split("/");
  return parts.slice(-n).join("/");
}

function Card({ pane, onOpen }: { pane: PaneState; onOpen: (id: number) => void }) {
  const working = pane.status === "working";
  return (
    <button
      onClick={() => onOpen(pane.id)}
      className={cn(
        "w-full rounded-xl border bg-card p-3 text-left transition-colors",
        "active:bg-sky-mid",
        pane.status === "needs_input"
          ? "border-red-500/60"
          : pane.status === "done"
            ? "border-emerald-500/40"
            : "border-card-edge"
      )}
    >
      <div className="flex items-center gap-2">
        <span className="w-7 shrink-0 text-center text-sm text-gold">
          {roman(pane.index)}
        </span>
        <span
          className={cn("min-w-0 flex-1 truncate font-semibold", working && "shimmer")}
        >
          {pane.name}
        </span>
        <StatusChip status={pane.status} thinking={pane.thinking} />
      </div>
      {(pane.subtitle || pane.recap) && (
        <div className="mt-1.5 space-y-0.5 pl-9">
          {pane.subtitle && (
            <div className="truncate text-subhead italic text-gold-soft/90">
              ✶ {pane.subtitle}
            </div>
          )}
          {pane.recap && (
            <div className="line-clamp-2 text-caption text-zinc-400">⏺ {pane.recap}</div>
          )}
        </div>
      )}
      <div className="mt-1.5 flex items-center gap-2 pl-9 text-caption text-zinc-500">
        {pane.agent && (
          <span className="rounded bg-white/5 px-1.5 py-px text-zinc-300">
            {pane.version ?? pane.agent}
          </span>
        )}
        {pane.cwd && <span className="truncate">{tail(pane.cwd)}</span>}
        <span className="ml-auto shrink-0 tabular-nums">{uptime(pane.uptime_ms)}</span>
      </div>
    </button>
  );
}

export function Herd({
  state,
  onOpen,
}: {
  state: SessionState | null;
  onOpen: (id: number) => void;
}) {
  if (!state) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-zinc-500">
        <Moon className="mr-2 h-4 w-4" /> waiting for the herd…
      </div>
    );
  }
  const waiting = state.panes.filter((p) => p.status === "needs_input");
  const rest = state.panes.filter((p) => p.status !== "needs_input");
  return (
    <div className="space-y-4 p-3 pb-8">
      {waiting.length > 0 && (
        <section className="space-y-2">
          <h2 className="flex items-center gap-1.5 px-1 text-caption font-bold uppercase tracking-wider text-red-300">
            <Sparkles className="h-3.5 w-3.5" /> waiting on you
          </h2>
          {waiting.map((p) => (
            <Card key={p.id} pane={p} onOpen={onOpen} />
          ))}
        </section>
      )}
      <section className="space-y-2">
        {waiting.length > 0 && (
          <h2 className="px-1 text-caption font-bold uppercase tracking-wider text-zinc-500">
            the rest of the herd
          </h2>
        )}
        {rest.map((p) => (
          <Card key={p.id} pane={p} onOpen={onOpen} />
        ))}
      </section>
    </div>
  );
}
