import { useEffect, useMemo, useRef, useState } from "react";
import {
  AArrowDown,
  AArrowUp,
  ArrowDownToLine,
  ArrowLeft,
  ChevronDown,
  ChevronUp,
  Keyboard,
  MoreHorizontal,
  Search,
  SendHorizonal,
  SlashSquare,
  SquareTerminal,
  WrapText,
  X,
} from "lucide-react";
import type { PaneState, SessionState, SlashCommand } from "./types";
import { client } from "./ws";
import { Term, type TermHandle, type ViewMode } from "./Term";
import { KeyPad } from "./KeyPad";
import { SlashPalette } from "./SlashPalette";
import { Button, Sheet, StatusChip } from "./ui";
import { useSwipeBack } from "./useSwipeBack";
import { usePinchZoom } from "./usePinchZoom";
import { hapticTap } from "./native";

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

export function Pane({
  pane,
  state,
  watch,
  commands,
  onBack,
}: {
  pane: PaneState;
  state: SessionState;
  watch: boolean | null;
  commands: SlashCommand[];
  onBack: () => void;
}) {
  const rows = state.rows || 45;
  const cols = state.cols || 160;
  const [mode, setMode] = useState<ViewMode>(initialMode);
  const [mirrorFont, setMirrorFont] = useState(() => fitFont(cols));
  const [readFont, setReadFont] = useState(13);
  const [text, setText] = useState("");
  const [pad, setPad] = useState(false);
  const [palette, setPalette] = useState(false);
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [moreOpen, setMoreOpen] = useState(false);
  const term = useRef<TermHandle>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  const sendBtn = useRef<HTMLButtonElement>(null);
  const header = useRef<HTMLElement>(null);
  const prevStatus = useRef(pane.status);

  useEffect(() => setMirrorFont(fitFont(cols)), [cols]);

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

  // Claude Code inserts a newline on `\` + Enter — keep multi-line dictation
  // intact for agent panes instead of submitting at the first newline.
  const sendText = (submit: boolean) => {
    const t = text.replace(/\s+$/, "");
    if (!t) return;
    const wired = pane.agent ? t.replaceAll("\n", "\\\r") : t;
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

  const status = useMemo(
    () => <StatusChip status={pane.status} thinking={pane.thinking} />,
    [pane.status, pane.thinking]
  );

  const swipe = useSwipeBack(onBack);

  return (
    <div
      ref={swipe.ref}
      className="flex h-full flex-col"
      style={{ touchAction: "pan-y" }}
      {...swipe.handlers}
    >
      {/* header — back, name, status, search, and one overflow icon for
          everything else (mode/font are "set once" prefs, search is used
          mid-read often enough to deserve staying one tap away) */}
      <header
        ref={header}
        className="safe-top flex items-center gap-2 border-b border-card-edge bg-sky-mid/70 px-2 py-2 backdrop-blur"
      >
        <Button size="icon" variant="ghost" onClick={onBack} aria-label="back">
          <ArrowLeft className="h-5 w-5" />
        </Button>
        <div className="min-w-0 flex-1">
          <div className="truncate text-body font-semibold">{pane.name}</div>
        </div>
        {status}
        <Button size="icon" variant="ghost" onClick={() => setSearching((s) => !s)} aria-label="search">
          <Search className="h-4 w-4" />
        </Button>
        <Button size="icon" variant="ghost" onClick={() => setMoreOpen(true)} aria-label="more">
          <MoreHorizontal className="h-5 w-5" />
        </Button>
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

      {/* terminal mirror */}
      <div className="relative min-h-0 flex-1" {...pinch}>
        <Term
          ref={term}
          pane={pane.id}
          rows={rows}
          cols={cols}
          mode={mode}
          mirrorFont={mirrorFont}
          readFont={readFont}
        />
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
        <div className="flex items-end gap-2 px-2 pt-2 pb-1.5">
          <Button
            size="icon"
            variant="ghost"
            onClick={() => setPalette(true)}
            aria-label="slash commands"
          >
            <SlashSquare className="h-5 w-5" />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            onClick={() => setPad((p) => !p)}
            aria-label="special keys"
            className={pad ? "text-gold" : ""}
          >
            <Keyboard className="h-5 w-5" />
          </Button>
          <textarea
            ref={box}
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={Math.min(5, Math.max(1, text.split("\n").length))}
            placeholder={`reply to ${pane.name}…`}
            className="min-w-0 flex-1 resize-none rounded-xl border border-card-edge bg-sky-deep px-3 py-2 text-[16px] outline-none placeholder:text-zinc-600 focus:border-gold/50"
          />
          <Button ref={sendBtn} size="icon" variant="gold" onClick={() => sendText(true)} aria-label="send">
            <SendHorizonal className="h-4 w-4" />
          </Button>
        </div>
        {pad && <KeyPad pane={pane.id} />}
      </div>

      <SlashPalette
        open={palette}
        onClose={() => setPalette(false)}
        commands={commands}
        onPick={insertCommand}
      />

      <Sheet open={moreOpen} onClose={() => setMoreOpen(false)} title="view options">
        <div className="space-y-4 px-2 pb-2">
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
  );
}
