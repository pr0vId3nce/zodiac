// The orrery: the whole herd as status-colored stars on a brass arc,
// readable from across the room, above a flat instrument ledger — one row
// per pane with a colored status rail, sigil + numeral column, and the
// pane's recap (or its open question). Filter chips triage a grown herd.
import { useEffect, useState } from "react";
import { Moon } from "lucide-react";
import type { PaneState, SessionState } from "./types";
import { STATUS_DOT, STATUS_TEXT, StatusGlyph, cn, roman, sigil } from "./ui";
import { observeStatus } from "./statusClock";

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

// ------------------------------------------------------------- orrery arc

const ARC_R = 150; // outer arc radius (px) — 300×150 half circle
const INNER_R = 110; // dashed inner ring

/** Stars sit evenly along the arc from ~160° to ~20° — the mockup insets
    the endpoints so no star sits exactly on the horizon. */
function starAngle(i: number, n: number) {
  const from = Math.PI * (160 / 180);
  const to = Math.PI * (20 / 180);
  if (n <= 1) return Math.PI / 2;
  return from + ((to - from) * i) / (n - 1);
}

function Orrery({
  panes,
  onOpen,
}: {
  panes: PaneState[];
  onOpen: (id: number) => void;
}) {
  const counts = {
    working: panes.filter((p) => p.status === "working").length,
    needs_input: panes.filter((p) => p.status === "needs_input").length,
    done: panes.filter((p) => p.status === "done").length,
    idle: panes.filter((p) => p.status === "idle").length,
  };
  return (
    <div className="relative mx-auto mt-1.5 h-[158px] w-[300px] max-w-full">
      {/* the dial */}
      <div
        className="absolute bottom-0 left-1/2 h-[150px] w-[300px] -translate-x-1/2 rounded-t-[300px] border border-b-0 border-gold/28"
        aria-hidden
      />
      <div
        className="absolute bottom-0 left-1/2 h-[110px] w-[220px] -translate-x-1/2 rounded-t-[300px] border border-b-0 border-dashed border-gold/12"
        aria-hidden
      />
      {/* one star per pane, tap to jump */}
      {panes.map((p, i) => {
        const a = starAngle(i, panes.length);
        const cx = 150 + ARC_R * Math.cos(Math.PI - a);
        const cy = ARC_R * Math.sin(a); // height above the horizon line
        const needs = p.status === "needs_input";
        const size = needs ? 13 : p.status === "idle" ? 9 : 11;
        // numeral label pushed radially outward from the star
        const lx = 150 + (ARC_R + 16) * Math.cos(Math.PI - a);
        const ly = (ARC_R + 16) * Math.sin(a);
        return (
          <button
            key={p.id}
            onClick={() => onOpen(p.id)}
            aria-label={`open ${p.name}`}
            className="absolute z-10 flex h-11 w-11 items-center justify-center"
            style={{ left: cx - 22, bottom: cy - 22 }}
          >
            <span
              className={cn("rounded-full", STATUS_DOT[p.status], needs && "glow-red")}
              style={{ width: size, height: size }}
            />
            <span
              className={cn(
                "pointer-events-none absolute font-mono text-[9px]",
                needs ? "font-bold text-red-300" : "text-dim"
              )}
              style={{ left: lx - cx + 22 - 4, bottom: ly - cy + 22 - 6 }}
            >
              {roman(p.index)}
            </span>
          </button>
        );
      })}
      {/* the count, dead center on the horizon */}
      <div className="pointer-events-none absolute inset-x-0 bottom-2 text-center font-mono">
        <div className="text-[24px] font-semibold text-white">
          {roman(panes.length)}
          <span className="text-[12px] font-normal text-dim"> panes</span>
        </div>
        <div className="mt-0.5 text-[10px] text-dim">
          {(
            [
              ["working", counts.working, "working"],
              ["needs_input", counts.needs_input, "needs you"],
              ["done", counts.done, "done"],
              ["idle", counts.idle, "idle"],
            ] as const
          )
            .filter(([, n]) => n > 0)
            .map(([key, n, label], i, arr) => (
              <span key={key}>
                <span className={key === "idle" ? undefined : STATUS_TEXT[key]}>
                  {n} {label}
                </span>
                {i < arr.length - 1 && " · "}
              </span>
            ))}
        </div>
      </div>
    </div>
  );
}

// ------------------------------------------------------------ filter chips

type Filter = "all" | "working" | "needs_input" | "done";
const FILTER_KEY = "astrolabe-herd-filter";

const FILTERS: Array<{ key: Filter; label: string }> = [
  { key: "all", label: "all" },
  { key: "working", label: "working" },
  { key: "needs_input", label: "needs you" },
  { key: "done", label: "done" },
];

function Chips({ filter, onPick }: { filter: Filter; onPick: (f: Filter) => void }) {
  return (
    <div className="flex gap-1.5 px-4 pb-2 pt-3.5 font-mono text-[10px]">
      {FILTERS.map(({ key, label }) => (
        <button
          key={key}
          onClick={() => onPick(key)}
          className={cn(
            "rounded-full border px-2.5 py-1",
            filter === key
              ? "border-gold/50 bg-gold/15 text-gold-soft"
              : cn(
                  "border-card-edge",
                  key === "needs_input" ? "text-red-300" : "text-dim"
                )
          )}
        >
          {label}
        </button>
      ))}
    </div>
  );
}

// ------------------------------------------------------------- the ledger

function Row({ pane, onOpen }: { pane: PaneState; onOpen: (id: number) => void }) {
  const working = pane.status === "working";
  const needs = pane.status === "needs_input";
  const since = observeStatus(pane.id, pane.status);
  const recap = needs && pane.question ? pane.question : pane.recap || pane.subtitle;
  return (
    <button
      onClick={() => onOpen(pane.id)}
      className={cn(
        "relative flex w-full flex-col gap-[3px] border-b border-card-edge/60 py-[11px] pl-5 pr-[18px] text-left",
        "active:bg-sky-mid/60",
        needs && "bg-red-400/5",
        pane.status === "idle" && "opacity-55"
      )}
    >
      <span
        className={cn(
          "absolute inset-y-0 left-0 w-0.5",
          STATUS_DOT[pane.status],
          needs && "glow-red"
        )}
        aria-hidden
      />
      <span className="flex items-baseline gap-2">
        <span className="w-[46px] shrink-0 whitespace-nowrap font-mono text-[12px] text-gold">
          {sigil(pane.index)} {roman(pane.index)}
        </span>
        <span
          className={cn(
            "min-w-0 flex-1 truncate font-mono text-[14px] font-semibold text-white",
            working && "shimmer"
          )}
        >
          {pane.name}
        </span>
        <StatusGlyph status={pane.status} thinking={pane.thinking} sinceMs={since} />
      </span>
      {recap && (
        <span
          className={cn(
            "truncate pl-[54px] font-mono text-caption",
            needs && pane.question ? "text-gold-soft" : "text-zinc-400"
          )}
        >
          {needs && pane.question ? recap : `⏺ ${recap}`}
        </span>
      )}
      <span className="flex gap-2 pl-[54px] font-mono text-[9.5px] text-dim">
        {pane.agent && <span>{pane.version ?? pane.agent}</span>}
        {pane.cwd && <span className="truncate">{tail(pane.cwd)}</span>}
        <span className="ml-auto shrink-0 tabular-nums">↑{uptime(pane.uptime_ms)}</span>
      </span>
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
  const [filter, setFilter] = useState<Filter>(() => {
    const saved = localStorage.getItem(FILTER_KEY);
    return saved === "working" || saved === "needs_input" || saved === "done"
      ? saved
      : "all";
  });
  // durations ("needs you · 4m") tick forward without any state change
  const [, bump] = useState(0);
  useEffect(() => {
    const t = setInterval(() => bump((n) => n + 1), 30_000);
    return () => clearInterval(t);
  }, []);

  if (!state) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-zinc-500">
        <Moon className="mr-2 h-4 w-4" /> waiting for the herd…
      </div>
    );
  }

  const pick = (f: Filter) => {
    setFilter(f);
    localStorage.setItem(FILTER_KEY, f);
  };

  // needs_input first, everything else in pane order (unchanged behavior)
  const ordered = [
    ...state.panes.filter((p) => p.status === "needs_input"),
    ...state.panes.filter((p) => p.status !== "needs_input"),
  ];
  const rows = filter === "all" ? ordered : ordered.filter((p) => p.status === filter);

  return (
    <div className="pb-8">
      <Orrery panes={state.panes} onOpen={onOpen} />
      <Chips filter={filter} onPick={pick} />
      <div className="flex flex-col border-t border-card-edge/80">
        {rows.map((p) => (
          <Row key={p.id} pane={p} onOpen={onOpen} />
        ))}
        {rows.length === 0 && (
          <div className="px-5 py-6 font-mono text-caption text-dim">
            nothing {FILTERS.find((f) => f.key === filter)?.label} right now
          </div>
        )}
      </div>
    </div>
  );
}
