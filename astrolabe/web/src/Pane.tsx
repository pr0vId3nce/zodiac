import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  AArrowDown,
  AArrowUp,
  ArrowDownToLine,
  ChevronDown,
  ChevronUp,
  Keyboard,
  MoreHorizontal,
  Search,
  SendHorizonal,
  SquareTerminal,
  WrapText,
  X,
} from "lucide-react";
import type { CommandSets, PaneState, SessionState } from "./types";
import { client } from "./ws";
import { Term, type TermHandle, type ViewMode } from "./Term";
import { KeyPad } from "./KeyPad";
import { SlashPalette } from "./SlashPalette";
import { Button, ROMAN, Sheet, StatusPill, cn, roman, sigil } from "./ui";
import { observeStatus } from "./statusClock";
import { useSwipeBack } from "./useSwipeBack";
import { usePinchZoom } from "./usePinchZoom";
import { hapticTap } from "./native";

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

/** One brass corner of the scan-reticle frame around the terminal — the
    pairing motif promoted to the app's signature. */
function Bracket({ pos }: { pos: "tl" | "tr" | "bl" | "br" }) {
  const cls = {
    tl: "left-0 top-0 rounded-tl-md border-l-2 border-t-2",
    tr: "right-0 top-0 rounded-tr-md border-r-2 border-t-2",
    bl: "bottom-0 left-0 rounded-bl-md border-b-2 border-l-2",
    br: "bottom-0 right-0 rounded-br-md border-b-2 border-r-2",
  }[pos];
  return (
    <span
      aria-hidden
      className={cn("pointer-events-none absolute z-10 h-[18px] w-[18px] border-gold/55", cls)}
    />
  );
}

// Mirror font: fit ~110 cols (where agent content lives) instead of the full
// desktop grid, and never drop below readable — the rest scrolls sideways.
function fitFont(cols: number) {
  const px = (window.innerWidth - 10) / (Math.min(cols, 110) * 0.62);
  return Math.max(9, Math.min(15, Math.floor(px)));
}

function initialMode(): ViewMode {
  const saved = localStorage.getItem("astrolabe-view-mode");
  if (saved === "read" || saved === "mirror") return saved;
  return window.innerWidth < 700 ? "read" : "mirror";
}

const MIRROR_FONT_KEY = "astrolabe-mirror-font";
const READ_FONT_KEY = "astrolabe-read-font";

function initialMirrorFont(cols: number): number {
  const saved = Number(localStorage.getItem(MIRROR_FONT_KEY));
  return saved > 0 ? saved : fitFont(cols);
}

function initialReadFont(): number {
  const saved = Number(localStorage.getItem(READ_FONT_KEY));
  return saved > 0 ? saved : 13;
}

export function Pane({
  pane,
  state,
  watch,
  commands,
  onBack,
  onSwipeComplete,
  backdrop,
}: {
  pane: PaneState;
  state: SessionState;
  watch: boolean | null;
  commands: CommandSets;
  onBack: () => void;
  /** Fires once a swipe-back's own release animation finishes, instead of
      `onBack` — the swipe already revealed `backdrop` live, so the caller
      should sync state without replaying a transition on top of it. */
  onSwipeComplete?: () => void;
  /** Rendered behind the pane, revealed by an edge-swipe drag. */
  backdrop?: ReactNode;
}) {
  const rows = state.rows || 45;
  const cols = state.cols || 160;
  const [mode, setMode] = useState<ViewMode>(initialMode);
  const [mirrorFont, setMirrorFont] = useState(() => initialMirrorFont(cols));
  const [readFont, setReadFont] = useState(initialReadFont);
  const [text, setText] = useState("");
  const [pad, setPad] = useState(false);
  const [palette, setPalette] = useState(false);
  // Each agent has its own slash commands; a pane running neither claude
  // nor pi gets an empty palette rather than another agent's menu.
  const paneCommands = (pane.agent && commands[pane.agent]) || [];
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [moreOpen, setMoreOpen] = useState(false);
  const term = useRef<TermHandle>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  const sendBtn = useRef<HTMLButtonElement>(null);
  const header = useRef<HTMLElement>(null);
  const prevStatus = useRef(pane.status);

  useEffect(() => {
    // Only auto-refit on a grid resize (rotation, split view) when there's
    // no saved preference yet — once pinch/the +/- buttons have set one,
    // a resize shouldn't silently discard it.
    if (!localStorage.getItem(MIRROR_FONT_KEY)) setMirrorFont(fitFont(cols));
  }, [cols]);

  useEffect(() => {
    localStorage.setItem(MIRROR_FONT_KEY, String(mirrorFont));
  }, [mirrorFont]);
  useEffect(() => {
    localStorage.setItem(READ_FONT_KEY, String(readFont));
  }, [readFont]);

  // A pane you're actively looking at flipping to needs_input is a
  // high-signal moment — worth the same haptic-feel pulse a send gets.
  useEffect(() => {
    if (pane.status === "needs_input" && prevStatus.current !== "needs_input") {
      hapticTap("warning", header.current);
    }
    prevStatus.current = pane.status;
  }, [pane.status]);

  const setViewMode = (m: ViewMode) => {
    setMode(m);
    localStorage.setItem("astrolabe-view-mode", m);
  };
  const bumpFont = (d: number) => {
    if (mode === "read") setReadFont((f) => Math.max(9, Math.min(22, f + d)));
    else setMirrorFont((f) => Math.max(4, Math.min(22, f + d)));
  };
  const pinch = usePinchZoom(
    (factor) => {
      if (mode === "read") {
        setReadFont((f) => Math.max(9, Math.min(22, Math.round(f * factor))));
      } else {
        setMirrorFont((f) => Math.max(4, Math.min(22, Math.round(f * factor))));
      }
    },
    (factor) => term.current?.previewScale(factor)
  );

  // Keep multi-line dictation intact instead of submitting at the first
  // newline: Claude Code inserts one on `\` + Enter, while pi binds Ctrl+J
  // (a plain newline byte) to it and would show claude's backslash as text.
  const sendText = (submit: boolean) => {
    const t = text.replace(/\s+$/, "");
    if (!t) return;
    const wired =
      !pane.agent || pane.agent === "pi" ? t : t.replaceAll("\n", "\\\r");
    if (submit) {
      client.promptPane(pane.id, wired);
      hapticTap("success", sendBtn.current);
    } else {
      client.input(pane.id, wired);
    }
    setText("");
  };

  const insertCommand = (name: string) => {
    setText((cur) => (cur.trim() ? cur : name + " "));
    box.current?.focus();
  };

  // durations ("needs you · 4m") tick forward without any state change
  const [, bump] = useState(0);
  useEffect(() => {
    const t = setInterval(() => bump((n) => n + 1), 30_000);
    return () => clearInterval(t);
  }, []);
  const since = observeStatus(pane.id, pane.status);
  const status = useMemo(
    () => <StatusPill status={pane.status} thinking={pane.thinking} sinceMs={since} />,
    [pane.status, pane.thinking, since]
  );

  const swipe = useSwipeBack(onSwipeComplete ?? onBack);

  return (
    <div className="relative h-full overflow-hidden">
      {/* revealed live by an edge-swipe drag (useSwipeBack drives both
          refs directly — no React state, so it can track the finger at
          60fps); sits at rest pulled back and dimmed, like the view
          underneath a native push. */}
      {backdrop && (
        <div
          ref={swipe.backdropRef}
          className="absolute inset-0"
          style={{ transform: swipe.restTransform }}
        >
          {backdrop}
          <div
            ref={swipe.scrimRef}
            className="pointer-events-none absolute inset-0 bg-black"
            style={{ opacity: swipe.restScrimOpacity }}
          />
        </div>
      )}
      <div
        ref={swipe.ref}
        className="relative z-10 flex h-full flex-col bg-sky-deep"
        style={{ touchAction: "pan-y" }}
        {...swipe.handlers}
      >
      {/* header — a two-line ledger: sigil + numeral + name + status pill,
          then agent · cwd · uptime with search/overflow folded into the
          second line's right edge */}
      <header
        ref={header}
        className="safe-top flex flex-col gap-[3px] border-b border-card-edge/80 bg-sky-mid/70 px-4 pb-2.5 pt-2 font-mono backdrop-blur"
      >
        <div className="flex items-center gap-2.5">
          <button
            onClick={onBack}
            aria-label="back"
            className="-ml-2 flex h-11 w-8 shrink-0 items-center justify-center text-[16px] text-zinc-300"
          >
            ‹
          </button>
          <span className="shrink-0 whitespace-nowrap text-[13px] text-gold">
            {sigil(pane.index)} {roman(pane.index)}
          </span>
          <span className="min-w-0 flex-1 truncate text-[16px] font-semibold text-white">
            {pane.name}
          </span>
          {status}
        </div>
        <div className="flex items-center gap-2.5 pl-[26px] text-[9.5px] text-dim">
          {pane.agent && <span>{pane.version ?? pane.agent}</span>}
          {pane.cwd && <span className="truncate">{tail(pane.cwd)}</span>}
          <span className="tabular-nums">↑{uptime(pane.uptime_ms)}</span>
          <span className="ml-auto flex shrink-0 items-center gap-1 text-zinc-300">
            <button
              onClick={() => setSearching((s) => !s)}
              aria-label="search"
              className="flex h-8 w-8 items-center justify-center"
            >
              <Search className="h-3.5 w-3.5" />
            </button>
            <button
              onClick={() => setMoreOpen(true)}
              aria-label="more"
              className="flex h-8 w-8 items-center justify-center"
            >
              <MoreHorizontal className="h-4 w-4" />
            </button>
          </span>
        </div>
      </header>

      {watch === false && (
        <div className="border-b border-amber-500/30 bg-amber-500/10 px-3 py-1 text-caption text-amber-300">
          old zodiac server — plain 1 Hz mirror. Restart the session on the new
          build for live colors.
        </div>
      )}

      {searching && (
        <div className="flex items-center gap-1.5 border-b border-card-edge bg-sky-mid px-2 py-1.5">
          <input
            autoFocus
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              if (e.target.value) term.current?.findNext(e.target.value);
            }}
            placeholder="search scrollback…"
            className="min-w-0 flex-1 rounded-md border border-card-edge bg-sky-deep px-2 py-2 text-[16px] outline-none focus:border-gold/50"
          />
          <Button size="icon" variant="ghost" onClick={() => query && term.current?.findPrevious(query)}>
            <ChevronUp className="h-4 w-4" />
          </Button>
          <Button size="icon" variant="ghost" onClick={() => query && term.current?.findNext(query)}>
            <ChevronDown className="h-4 w-4" />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            onClick={() => {
              term.current?.clearSearch();
              setSearching(false);
              setQuery("");
            }}
          >
            <X className="h-4 w-4" />
          </Button>
        </div>
      )}

      {/* terminal mirror, framed by the scan-reticle corner brackets */}
      <div className="relative mx-3 my-2.5 min-h-0 flex-1" {...pinch}>
        <Bracket pos="tl" />
        <Bracket pos="tr" />
        <Bracket pos="bl" />
        <Bracket pos="br" />
        <div className="h-full overflow-hidden rounded-md bg-[rgba(4,6,14,0.6)] p-1.5">
          <Term
            ref={term}
            pane={pane.id}
            rows={rows}
            cols={cols}
            mode={mode}
            mirrorFont={mirrorFont}
            readFont={readFont}
          />
        </div>
        <button
          onClick={() => term.current?.scrollToBottom()}
          className="absolute bottom-2 right-2 flex h-11 w-11 items-center justify-center rounded-full border border-card-edge bg-sky-mid/80 text-zinc-300 backdrop-blur active:scale-95"
          aria-label="jump to live"
        >
          <ArrowDownToLine className="h-4 w-4" />
        </button>
      </div>

      {/* composer */}
      <div className="border-t border-card-edge bg-sky-mid/70 backdrop-blur safe-bottom">
        {/* Question dialog on screen: real answer buttons. Typing into the
            composer would feed keystrokes to a picker that ignores text —
            these send the option's digit instead (plus the composer text
            as a follow-up note, if any). */}
        {pane.status === "needs_input" && pane.options?.length ? (
          <div className="px-3.5 pt-3">
            {pane.question && (
              <div className="mb-2 flex items-baseline gap-2 font-mono">
                <span className="text-caption font-bold text-red-300">?</span>
                <span className="text-subhead text-gold-soft">{pane.question}</span>
              </div>
            )}
            <div className="flex flex-col gap-[7px]">
              {pane.options.map((opt, i) => (
                <button
                  key={i}
                  onClick={() => {
                    client.answer(pane.id, i + 1, text.trim() || undefined);
                    setText("");
                    hapticTap("success", header.current);
                  }}
                  className={cn(
                    "flex items-center gap-3 rounded-[10px] border px-3 py-[11px] text-left active:scale-[0.98]",
                    i === 0
                      ? "border-gold/55 bg-gold/8 text-white"
                      : "border-card-edge/90 bg-card/60 text-zinc-300"
                  )}
                >
                  <span
                    className={cn(
                      "flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-full border font-mono text-[12px]",
                      i === 0 ? "border-gold/60 text-gold" : "border-dim/40 text-dim"
                    )}
                  >
                    {ROMAN[i] ?? i + 1}
                  </span>
                  <span className="min-w-0 flex-1 font-mono text-subhead">{opt}</span>
                </button>
              ))}
            </div>
            <div className="mt-1.5 font-mono text-[10px] text-zinc-600">
              anything typed below rides along as a note
            </div>
          </div>
        ) : null}
        <div className="flex items-center gap-2 px-3.5 pb-1.5 pt-2.5">
          <div
            className="flex min-w-0 flex-1 items-center gap-2 rounded-[10px] border border-card-edge bg-[rgba(4,6,14,0.8)] px-3 py-[7px]"
            onClick={() => box.current?.focus()}
          >
            <span className="shrink-0 font-mono font-bold text-prompt">❯</span>
            <textarea
              ref={box}
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={Math.min(5, Math.max(1, text.split("\n").length))}
              placeholder={`reply to ${pane.name}…`}
              className="min-w-0 flex-1 resize-none bg-transparent py-1 text-[16px] outline-none placeholder:text-zinc-600"
            />
            {!text && (
              <span aria-hidden className="cursor-blink shrink-0 font-mono text-gold">
                ▍
              </span>
            )}
          </div>
          {paneCommands.length > 0 && (
            <button
              onClick={() => setPalette(true)}
              aria-label="slash commands"
              className="flex h-11 w-11 shrink-0 items-center justify-center rounded-[10px] border border-card-edge font-mono text-subhead text-dim active:scale-95"
            >
              /
            </button>
          )}
          <button
            ref={sendBtn}
            onClick={() => sendText(true)}
            aria-label="send"
            className="flex h-11 w-11 shrink-0 items-center justify-center rounded-[10px] bg-gold text-sky-deep active:scale-95"
          >
            <SendHorizonal className="h-4 w-4" />
          </button>
        </div>
        {pad && <KeyPad pane={pane.id} />}
      </div>

      <SlashPalette
        open={palette}
        onClose={() => setPalette(false)}
        commands={paneCommands}
        onPick={insertCommand}
      />

      <Sheet open={moreOpen} onClose={() => setMoreOpen(false)} title="view options">
        <div className="space-y-4 px-2 pb-2">
          <div>
            <div className="mb-1.5 px-1 text-caption font-semibold uppercase tracking-wider text-zinc-500">
              input
            </div>
            <Button
              variant={pad ? "gold" : "default"}
              onClick={() => {
                setPad((p) => !p);
                setMoreOpen(false);
              }}
              className="w-full"
            >
              <Keyboard className="h-4 w-4" /> special keys {pad ? "on" : "off"}
            </Button>
          </div>
          <div>
            <div className="mb-1.5 px-1 text-caption font-semibold uppercase tracking-wider text-zinc-500">
              display
            </div>
            <div className="flex gap-2">
              <Button
                variant={mode === "mirror" ? "gold" : "default"}
                onClick={() => setViewMode("mirror")}
                className="flex-1"
              >
                <SquareTerminal className="h-4 w-4" /> mirror
              </Button>
              <Button
                variant={mode === "read" ? "gold" : "default"}
                onClick={() => setViewMode("read")}
                className="flex-1"
              >
                <WrapText className="h-4 w-4" /> read
              </Button>
            </div>
          </div>
          <div>
            <div className="mb-1.5 px-1 text-caption font-semibold uppercase tracking-wider text-zinc-500">
              font size
            </div>
            <div className="flex items-center gap-2">
              <Button size="icon" variant="default" onClick={() => bumpFont(-1)} aria-label="smaller">
                <AArrowDown className="h-4 w-4" />
              </Button>
              <div className="flex-1 text-center text-body tabular-nums text-zinc-300">
                {mode === "read" ? readFont : mirrorFont}px
              </div>
              <Button size="icon" variant="default" onClick={() => bumpFont(1)} aria-label="bigger">
                <AArrowUp className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </div>
      </Sheet>
      </div>
    </div>
  );
}
