// shadcn-style primitives, hand-rolled (shadcn is copy-in by design; these
// are the few pieces Astrolabe needs, kept offline-friendly — no registry fetch).
import { forwardRef, type ReactNode, useEffect, useRef, useState } from "react";

export function cn(...parts: Array<string | false | null | undefined>) {
  return parts.filter(Boolean).join(" ");
}

/** Keeps a closing element mounted long enough to animate out, instead of
    the usual `if (!open) return null` snap. Paints the closed position
    first (`mounted` true, `visible` still false) so opening also animates
    in rather than popping straight to its final state. Shared by the
    bottom sheet here, the screen-transition fallback, and swipe-back's
    "keep the previous screen mounted during the drag" case — same
    delayed-unmount shape in all three. */
export function useDelayedUnmount(open: boolean, durationMs: number) {
  const [mounted, setMounted] = useState(open);
  const [visible, setVisible] = useState(open);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (timer.current) clearTimeout(timer.current);
    if (open) {
      setMounted(true);
      const raf = requestAnimationFrame(() => setVisible(true));
      return () => cancelAnimationFrame(raf);
    }
    setVisible(false);
    timer.current = setTimeout(() => setMounted(false), durationMs);
    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
  }, [open, durationMs]);

  return { mounted, visible };
}

export const Button = forwardRef<
  HTMLButtonElement,
  React.ButtonHTMLAttributes<HTMLButtonElement> & {
    variant?: "default" | "ghost" | "gold";
    /** "icon" pins a 44×44pt (HIG minimum) square for icon-only buttons —
        Astrolabe's toolbar/keypad buttons all use this rather than letting
        padding-derived sizing land under the touch-target floor. */
    size?: "md" | "icon";
  }
>(function Button({ className, variant = "default", size = "md", ...props }, ref) {
  return (
    <button
      ref={ref}
      className={cn(
        "inline-flex items-center justify-center gap-1.5 rounded-lg text-sm font-medium",
        "transition-colors active:scale-[0.97] disabled:opacity-40 select-none",
        size === "icon" ? "h-11 w-11 shrink-0 p-0" : "min-h-11",
        variant === "default" &&
          cn("bg-card border border-card-edge text-zinc-200 hover:bg-sky-mid", size === "md" && "px-3 py-2"),
        variant === "ghost" && cn("text-zinc-300 hover:bg-white/5", size === "md" && "px-2 py-2"),
        variant === "gold" &&
          cn("bg-gold/15 border border-gold/40 text-gold-soft hover:bg-gold/25", size === "md" && "px-3 py-2"),
        className
      )}
      {...props}
    />
  );
});

const STATUS_STYLE: Record<string, string> = {
  working: "bg-orange-500/15 text-orange-300 border-orange-500/40",
  needs_input: "bg-red-500/15 text-red-300 border-red-500/50",
  done: "bg-emerald-500/15 text-emerald-300 border-emerald-500/40",
  idle: "bg-zinc-500/15 text-zinc-400 border-zinc-500/30",
};

export const STATUS_LABEL: Record<string, string> = {
  working: "working",
  needs_input: "needs you",
  done: "finished",
  idle: "idle",
};

export function StatusChip({ status, thinking }: { status: string; thinking?: boolean }) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-caption font-semibold",
        STATUS_STYLE[status] ?? STATUS_STYLE.idle,
        status === "needs_input" && "pulse-red"
      )}
    >
      {status === "working" && (
        <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-orange-400" />
      )}
      {STATUS_LABEL[status] ?? status}
      {thinking ? " · thinking" : ""}
    </span>
  );
}

const SHEET_DURATION_MS = 250;

/** Bottom sheet (mobile-first stand-in for shadcn's Sheet). Slides up on
    open, slides down and fades its backdrop on close, using iOS's own
    sheet-presentation curve rather than a generic ease. */
export function Sheet({
  open,
  onClose,
  title,
  children,
}: {
  open: boolean;
  onClose: () => void;
  title?: string;
  children: ReactNode;
}) {
  const { mounted, visible } = useDelayedUnmount(open, SHEET_DURATION_MS);
  if (!mounted) return null;
  return (
    <div className="fixed inset-0 z-50">
      <div
        className={cn(
          "absolute inset-0 bg-black/60 transition-opacity duration-200",
          visible ? "opacity-100" : "opacity-0"
        )}
        onClick={onClose}
      />
      <div
        className={cn(
          "absolute inset-x-0 bottom-0 max-h-[75%] overflow-hidden rounded-t-2xl border-t border-card-edge bg-sky-mid shadow-2xl",
          "transition-transform duration-[250ms] ease-[var(--ease-ios-sheet)]",
          visible ? "translate-y-0" : "translate-y-full"
        )}
      >
        <div className="mx-auto mt-2 h-1 w-10 rounded-full bg-zinc-600" />
        {title && (
          <div className="px-4 pt-3 pb-1 text-subhead font-semibold text-zinc-300">{title}</div>
        )}
        <div className="overflow-y-auto px-2 pb-4 safe-bottom" style={{ maxHeight: "62vh" }}>
          {children}
        </div>
      </div>
    </div>
  );
}
