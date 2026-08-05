// One zodiac pane, two ways to look at it:
//
//  - "mirror": read-only xterm.js at the exact server-side grid, so TUI
//    chrome renders faithfully (horizontal scroll when it outgrows the
//    phone).
//  - "read": the same emulator's buffer re-rendered as ordinary DOM text —
//    colored runs, soft-wrapped at screen width, selectable — which is what
//    you actually want to *read* on a phone. Logical lines are rebuilt by
//    joining xterm's wrapped rows.
//
// The xterm instance always runs (it is the parser); read mode just paints
// an overlay from its buffer, debounced while output streams.

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { Terminal } from "@xterm/xterm";
import type { IBufferCell } from "@xterm/xterm";
import { SearchAddon } from "@xterm/addon-search";
import { client } from "./ws";
import { b64ToBytes } from "./types";

export type ViewMode = "read" | "mirror";

export interface TermHandle {
  findNext: (q: string) => void;
  findPrevious: (q: string) => void;
  clearSearch: () => void;
  scrollToBottom: () => void;
  /** Transient visual scale during a pinch gesture — cheap CSS transform,
      not a real re-font (xterm.js re-fonting on every touchmove would
      jank). `null` clears it once the gesture ends and a real font size
      commits. */
  previewScale: (factor: number | null) => void;
}

const MONO = "SFMono-Regular, ui-monospace, Menlo, Consolas, monospace";

// ------------------------------------------------------------ color mapping

// classic xterm 16-color palette; 0 lightened so "black" text survives the
// night-sky background
const PALETTE16 = [
  "#5a6478", "#e05561", "#5fd7a7", "#e5cf5e", "#61afef", "#c678dd",
  "#56b6c2", "#e6e8f0", "#7f8ba3", "#f14c4c", "#23d18b", "#f5f543",
  "#3b8eea", "#d670d6", "#29b8db", "#ffffff",
];

function palette(n: number): string {
  if (n < 16) return PALETTE16[n];
  if (n < 232) {
    const steps = [0, 95, 135, 175, 215, 255];
    const i = n - 16;
    const r = steps[Math.floor(i / 36)];
    const g = steps[Math.floor(i / 6) % 6];
    const b = steps[i % 6];
    return `rgb(${r},${g},${b})`;
  }
  const v = 8 + 10 * (n - 232);
  return `rgb(${v},${v},${v})`;
}

function rgbCss(n: number): string {
  return `#${(n & 0xffffff).toString(16).padStart(6, "0")}`;
}

interface Run {
  text: string;
  style?: React.CSSProperties;
}

function styleOf(cell: IBufferCell): [string, React.CSSProperties | undefined] {
  let fg: string | undefined;
  let bg: string | undefined;
  if (cell.isFgRGB()) fg = rgbCss(cell.getFgColor());
  else if (cell.isFgPalette()) {
    let n = cell.getFgColor();
    if (cell.isBold() && n < 8) n += 8; // bright-for-bold, like the mirror
    fg = palette(n);
  }
  if (cell.isBgRGB()) bg = rgbCss(cell.getBgColor());
  else if (cell.isBgPalette()) bg = palette(cell.getBgColor());
  if (cell.isInverse()) {
    [fg, bg] = [bg ?? "#0b1020", fg ?? "#e6e8f0"];
  }
  const style: React.CSSProperties = {};
  if (fg) style.color = fg;
  if (bg) style.backgroundColor = bg;
  if (cell.isBold()) style.fontWeight = 600;
  if (cell.isDim()) style.opacity = 0.55;
  if (cell.isItalic()) style.fontStyle = "italic";
  if (cell.isUnderline()) style.textDecoration = "underline";
  if (cell.isStrikethrough()) style.textDecoration = "line-through";
  const any = Object.keys(style).length > 0;
  if (!any) return ["", undefined];
  return [JSON.stringify(style), style];
}

/** Rebuild logical lines (wrapped rows joined) from the emulator's buffer. */
function extractLines(term: Terminal, maxRows: number): Run[][] {
  const buf = term.buffer.active;
  const end = buf.length;
  let start = Math.max(0, end - maxRows);
  // don't start mid-wrap
  while (start > 0 && start < end && buf.getLine(start)?.isWrapped) start--;
  const cell = buf.getNullCell();
  const lines: Run[][] = [];
  let cur: Run[] | null = null;
  for (let y = start; y < end; y++) {
    const line = buf.getLine(y);
    if (!line) continue;
    if (!line.isWrapped || cur === null) {
      if (cur) lines.push(cur);
      cur = [];
    }
    let run = "";
    let runKey = "";
    let runStyle: React.CSSProperties | undefined;
    const flush = () => {
      if (run) cur!.push({ text: run, style: runStyle });
      run = "";
    };
    for (let x = 0; x < line.length; x++) {
      line.getCell(x, cell);
      if (cell.getWidth() === 0) continue; // tail of a wide glyph
      const [key, style] = styleOf(cell);
      if (key !== runKey) {
        flush();
        runKey = key;
        runStyle = style;
      }
      run += cell.getChars() || " ";
    }
    flush();
  }
  if (cur) lines.push(cur);
  // right-trim unstyled whitespace (keep bg-painted spaces — highlight bars)
  for (const l of lines) {
    while (l.length) {
      const last = l[l.length - 1];
      if (last.style?.backgroundColor) break;
      const t = last.text.replace(/\s+$/, "");
      if (t) {
        last.text = t;
        break;
      }
      l.pop();
    }
  }
  while (lines.length && lines[lines.length - 1].length === 0) lines.pop();
  // drop input-box chrome that only makes sense at terminal width: full-width
  // ─── separator rules — bare, or carrying an embedded title the way some
  // TUIs draw them (`── ✳ session ──`, `╭─ title ───╮`) — and a bare ❯
  // prompt line (read mode only — the mirror keeps them)
  return lines.filter((l) => {
    const text = l.map((r) => r.text).join("").trim();
    if (text === "❯") return false;
    // A rule: opens and closes with dashes (optionally cornered), and is
    // *mostly* dashes — the majority test keeps ordinary prose that merely
    // starts and ends with a dash, while a full-width rule with a short
    // title in the middle is overwhelmingly dash characters.
    if (/^[╭╰┌└]?[─━═╌╍]/.test(text) && /[─━═╌╍][╮╯┐┘]?$/.test(text)) {
      const dashes = (text.match(/[─━═╌╍]/g) ?? []).length;
      if (dashes >= text.length / 2 && dashes >= 3) return false;
    }
    return true;
  });
}

// ----------------------------------------------------------------- component

export const Term = forwardRef<
  TermHandle,
  {
    pane: number;
    rows: number;
    cols: number;
    mode: ViewMode;
    mirrorFont: number;
    readFont: number;
  }
>(function Term({ pane, rows, cols, mode, mirrorFont, readFont }, ref) {
  const root = useRef<HTMLDivElement>(null);
  const holder = useRef<HTMLDivElement>(null);
  const readRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const modeRef = useRef(mode);
  modeRef.current = mode;

  const [lines, setLines] = useState<Run[][]>([]);
  /// Width the mirror is allowed to scroll to, in px: the columns actually
  /// carrying content, not the full server grid. See `updateTrim`.
  const [trimPx, setTrimPx] = useState<number | null>(null);
  const [query, setQuery] = useState("");
  const [hit, setHit] = useState(0);
  const pinned = useRef(true);
  const debounce = useRef<ReturnType<typeof setTimeout> | null>(null);

  /// The mirror renders the whole server grid, and the font can only shrink
  /// to 9px, so on a phone a typical 120-column session is always wider than
  /// the screen — with the right-hand columns usually blank, which reads as
  /// scrolling into nothing. Clip the scrollable area to the widest visible
  /// row that has anything on it: wide content still pans, empty space
  /// doesn't.
  const updateTrim = useCallback(() => {
    const term = termRef.current;
    const node = holder.current;
    if (!term || !node) return;
    const buf = term.buffer.active;
    let used = 0;
    for (let y = buf.viewportY; y < buf.viewportY + term.rows; y++) {
      const len = buf.getLine(y)?.translateToString(true).length ?? 0;
      if (len > used) used = len;
    }
    const screen = node.querySelector<HTMLElement>(".xterm-screen");
    if (!screen || !term.cols) return;
    const cell = screen.clientWidth / term.cols;
    if (!cell) return;
    setTrimPx(Math.ceil(used * cell) + 8); // + the holder's px-1 either side
  }, []);

  const refresh = useCallback(() => {
    if (debounce.current) return;
    debounce.current = setTimeout(() => {
      debounce.current = null;
      if (modeRef.current === "read" && termRef.current) {
        setLines(extractLines(termRef.current, 2500));
      }
      if (modeRef.current === "mirror") updateTrim();
    }, 350);
  }, [updateTrim]);

  useImperativeHandle(ref, () => ({
    findNext: (q) => {
      if (modeRef.current === "read") {
        setQuery(q);
        setHit((h) => (q === query ? h + 1 : 0));
      } else {
        searchRef.current?.findNext(q, { incremental: false });
      }
    },
    findPrevious: (q) => {
      if (modeRef.current === "read") {
        setQuery(q);
        setHit((h) => h - 1);
      } else {
        searchRef.current?.findPrevious(q);
      }
    },
    clearSearch: () => {
      setQuery("");
      setHit(0);
      searchRef.current?.clearDecorations();
    },
    scrollToBottom: () => {
      pinned.current = true;
      termRef.current?.scrollToBottom();
      const el = readRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    },
    previewScale: (factor) => {
      const node = root.current;
      if (!node) return;
      node.style.transformOrigin = "center center";
      node.style.transform = factor && factor !== 1 ? `scale(${factor})` : "";
    },
  }));

  useEffect(() => {
    if (!holder.current) return;
    // xterm can't read CSS variables itself — resolve the active theme's
    // canvas/accent at mount, and again if the native shell flips
    // data-theme while a pane is open.
    const themeColors = () => {
      const css = getComputedStyle(document.documentElement);
      return {
        background: css.getPropertyValue("--color-sky-deep").trim() || "#0b1020",
        foreground: css.getPropertyValue("--color-fg").trim() || "#e6e8f0",
        cursor: css.getPropertyValue("--color-gold").trim() || "#d4af37",
        selectionBackground: "#3a4470",
      };
    };
    const term = new Terminal({
      rows,
      cols,
      fontSize: mirrorFont,
      fontFamily: MONO,
      lineHeight: 1.1,
      scrollback: 10000,
      disableStdin: true,
      convertEol: false,
      theme: themeColors(),
      allowProposedApi: true,
    });
    const search = new SearchAddon();
    term.loadAddon(search);
    term.open(holder.current);
    termRef.current = term;
    searchRef.current = search;

    // Scrolling through scrollback changes which rows are visible, and so
    // how wide the content is.
    const scrolled = term.onScroll(() => updateTrim());

    const off = client.onStream((msg) => {
      if (!("pane" in msg) || msg.pane !== pane) return;
      if (msg.t === "replay") {
        term.reset();
        term.write(b64ToBytes(msg.data), refresh);
        setTimeout(() => term.scrollToBottom(), 50);
      } else if (msg.t === "output") {
        term.write(b64ToBytes(msg.data), refresh);
      } else if (msg.t === "screen") {
        term.reset();
        term.write(msg.text.replaceAll("\n", "\r\n"), refresh);
      }
    });
    client.view(pane);
    const themeWatch = new MutationObserver(() => {
      term.options.theme = themeColors();
    });
    themeWatch.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => {
      off();
      scrolled.dispose();
      themeWatch.disconnect();
      client.view(null);
      term.dispose();
      termRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pane]);

  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontSize = mirrorFont;
    term.resize(cols, rows);
    // xterm re-measures its cell size asynchronously after a font change;
    // trim against the new geometry, not the old.
    const t = setTimeout(updateTrim, 50);
    return () => clearTimeout(t);
  }, [rows, cols, mirrorFont, updateTrim]);

  // entering read mode: build lines now, land at the bottom
  useEffect(() => {
    if (mode === "read" && termRef.current) {
      setLines(extractLines(termRef.current, 2500));
      pinned.current = true;
    }
    if (mode === "mirror") updateTrim();
  }, [mode, updateTrim]);

  // stay pinned to the live end while output streams (chat-style)
  useEffect(() => {
    const el = readRef.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  // scroll the active search hit into view
  useEffect(() => {
    const el = readRef.current;
    if (!el || !query) return;
    const marks = el.querySelectorAll<HTMLElement>("mark[data-astrolabe]");
    if (!marks.length) return;
    const idx = ((hit % marks.length) + marks.length) % marks.length;
    marks.forEach((m, i) => m.classList.toggle("astrolabe-mark-active", i === idx));
    pinned.current = false;
    marks[idx].scrollIntoView({ block: "center" });
  }, [query, hit, lines]);

  const renderRun = (r: Run, key: number) => {
    if (!query) {
      return r.style ? (
        <span key={key} style={r.style}>
          {r.text}
        </span>
      ) : (
        r.text
      );
    }
    const parts: React.ReactNode[] = [];
    const lower = r.text.toLowerCase();
    const q = query.toLowerCase();
    let i = 0;
    let n = 0;
    while (true) {
      const at = lower.indexOf(q, i);
      if (at < 0) {
        parts.push(r.text.slice(i));
        break;
      }
      parts.push(r.text.slice(i, at));
      parts.push(
        <mark key={n++} data-astrolabe className="astrolabe-mark">
          {r.text.slice(at, at + q.length)}
        </mark>
      );
      i = at + q.length;
    }
    return r.style ? (
      <span key={key} style={r.style}>
        {parts}
      </span>
    ) : (
      <span key={key}>{parts}</span>
    );
  };

  return (
    <div ref={root} className="relative h-full">
      {/* mirror (always mounted — it's the parser) */}
      <div
        className={
          mode === "mirror"
            ? "h-full overflow-auto overscroll-x-contain"
            : "invisible h-full overflow-hidden"
        }
      >
        {/* Clipped to the columns in use (see updateTrim); the terminal
            itself still renders the full grid inside. */}
        <div
          className="h-full min-w-full overflow-hidden"
          style={mode === "mirror" && trimPx ? { width: `${trimPx}px` } : undefined}
        >
          <div ref={holder} className="h-full w-max min-w-full px-1" />
        </div>
      </div>

      {mode === "read" && (
        <div
          ref={readRef}
          onScroll={(e) => {
            const el = e.currentTarget;
            pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 100;
          }}
          className="absolute inset-0 select-text overflow-y-auto overscroll-contain bg-sky-deep px-2 py-1"
          style={{
            fontFamily: MONO,
            fontSize: readFont,
            lineHeight: 1.5,
            WebkitTouchCallout: "default",
          }}
        >
          {lines.map((l, i) => (
            <div key={i} className="min-h-[1.2em] whitespace-pre-wrap break-words">
              {l.map(renderRun)}
            </div>
          ))}
        </div>
      )}
    </div>
  );
});
