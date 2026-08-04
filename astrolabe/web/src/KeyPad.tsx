// Special-keys pad: raw escape sequences straight into the pane's PTY.
import { client } from "./ws";
import { cn } from "./ui";
import { hapticTap } from "./native";

const KEYS: Array<{ label: string; seq: string; accent?: boolean }> = [
  { label: "Esc", seq: "\x1b", accent: true },
  { label: "⇧Tab", seq: "\x1b[Z" },
  { label: "Tab", seq: "\t" },
  { label: "↑", seq: "\x1b[A" },
  { label: "↓", seq: "\x1b[B" },
  { label: "←", seq: "\x1b[D" },
  { label: "→", seq: "\x1b[C" },
  { label: "^C", seq: "\x03", accent: true },
  { label: "^U", seq: "\x15" },
  { label: "^R", seq: "\x12" },
  { label: "^O", seq: "\x0f" },
  { label: "PgUp", seq: "\x1b[5~" },
  { label: "PgDn", seq: "\x1b[6~" },
  { label: "⏎", seq: "\r", accent: true },
];

export function KeyPad({ pane }: { pane: number }) {
  return (
    <div className="grid grid-cols-7 gap-2 px-2 pb-2">
      {KEYS.map((k) => (
        <button
          key={k.label}
          onClick={(e) => {
            client.input(pane, k.seq);
            hapticTap("selection", e.currentTarget);
          }}
          className={cn(
            // A dense 14-key grid can't hit the full 44pt HIG minimum without
            // ballooning the pad's height — grown as far as that tradeoff
            // allows (py-2 → py-2.5, wider gaps to act as tap-tolerance);
            // still worth a real on-device check, not just this math.
            "min-h-11 rounded-md border py-2.5 font-mono text-xs active:scale-95 select-none",
            k.accent
              ? "border-gold/40 bg-gold/10 text-gold-soft"
              : "border-card-edge bg-card text-zinc-300"
          )}
        >
          {k.label}
        </button>
      ))}
    </div>
  );
}
