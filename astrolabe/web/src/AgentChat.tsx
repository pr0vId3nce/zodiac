// An agent pane's conversation as a chat feed — the structured stand-in for
// the terminal mirror (agent panes have no pty; there is nothing to mirror).
//
// History comes from the bridge's GET /api/transcript (the zodiac server
// renders its event ring into role+text entries — one parser, every
// client). Live T_AGENT_EVENT lines arriving over the WebSocket keep it
// fresh the simple way: a completed entry (user prompt, assistant message,
// tool call, turn result) schedules a debounced re-fetch of those same
// server-rendered entries, and streaming deltas just light a "working…"
// row — no client-side NDJSON reassembly, partials never rendered halfway.
// Unknown event types are ignored (forward compatibility).

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { client } from "./ws";
import { getToken } from "./auth";
import { cn } from "./ui";

export interface AgentChatHandle {
  scrollToBottom: () => void;
}

interface ChatEntry {
  role: "user" | "assistant" | "tool";
  text: string;
}

const MONO = "SFMono-Regular, ui-monospace, Menlo, Consolas, monospace";

// Streaming partials (claude stream_event deltas, pi message_update/start) —
// they only drive the working indicator, completed entries carry content.
const PARTIAL_TYPES = new Set(["stream_event", "message_update", "message_start"]);
// A turn settled — stop the indicator (the refetch repaints the content).
const SETTLED_TYPES = new Set(["result", "agent_end", "turn_end"]);
// Bookkeeping lines that never change the transcript entries.
const NO_REFETCH_TYPES = new Set(["zodiac_perm_resolved", ...PARTIAL_TYPES]);

const REFETCH_DEBOUNCE_MS = 300;

export const AgentChat = forwardRef<
  AgentChatHandle,
  { pane: number; font: number; status: string }
>(
  /** `status` is the pane's server-tracked status — it both backs the
      indicator up (working before any delta arrives) and unsticks it (a
      missed settle line clears within a status poll). */
  function AgentChat({ pane, font, status }, ref) {
    const [entries, setEntries] = useState<ChatEntry[] | null>(null);
    const [working, setWorking] = useState(false);
    const [failed, setFailed] = useState(false);
    const scroller = useRef<HTMLDivElement>(null);
    const pinned = useRef(true);
    const refetch = useRef<ReturnType<typeof setTimeout> | null>(null);

    const load = useCallback(async () => {
      try {
        const token = getToken();
        const res = await fetch(`/api/transcript?pane=${pane}`, {
          headers: token ? { Authorization: `Bearer ${token}` } : {},
        });
        if (!res.ok) {
          setFailed(true);
          return;
        }
        const body = (await res.json()) as unknown;
        if (Array.isArray(body)) {
          setEntries(
            body.filter(
              (e): e is ChatEntry =>
                !!e && typeof e.text === "string" &&
                (e.role === "user" || e.role === "assistant" || e.role === "tool"),
            ),
          );
          setFailed(false);
        }
      } catch {
        /* keep whatever is on screen through a blip */
      }
    }, [pane]);

    useEffect(() => {
      // Subscribe before the first load so nothing can land between them —
      // any line that races the fetch just re-schedules it.
      const off = client.onStream((msg) => {
        if (msg.t !== "agent_event" || msg.pane !== pane) return;
        let dirty = false;
        for (const line of msg.lines) {
          let typ: unknown;
          try {
            typ = (JSON.parse(line) as { type?: unknown })?.type;
          } catch {
            continue;
          }
          if (typeof typ !== "string") continue;
          if (PARTIAL_TYPES.has(typ)) setWorking(true);
          if (SETTLED_TYPES.has(typ)) setWorking(false);
          // Everything except partials/bookkeeping might have produced a
          // transcript entry — including types this build doesn't know.
          // An extra debounced refresh is harmless; silence is not.
          if (!NO_REFETCH_TYPES.has(typ)) dirty = true;
        }
        if (dirty && !refetch.current) {
          refetch.current = setTimeout(() => {
            refetch.current = null;
            load();
          }, REFETCH_DEBOUNCE_MS);
        }
      });
      client.view(pane); // the bridge only forwards agent_event to viewers
      load();
      return () => {
        off();
        client.view(null);
        if (refetch.current) clearTimeout(refetch.current);
        refetch.current = null;
      };
    }, [pane, load]);

    // the server declaring the turn over beats a lingering local flag
    useEffect(() => {
      if (status !== "working") setWorking(false);
    }, [status]);
    const busy = working || status === "working";

    // stay pinned to the live end while the conversation grows (chat-style)
    useEffect(() => {
      const el = scroller.current;
      if (el && pinned.current) el.scrollTop = el.scrollHeight;
    }, [entries, busy]);

    useImperativeHandle(ref, () => ({
      scrollToBottom: () => {
        pinned.current = true;
        const el = scroller.current;
        if (el) el.scrollTop = el.scrollHeight;
      },
    }));

    return (
      <div
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 100;
        }}
        className="h-full select-text overflow-y-auto overscroll-contain bg-sky-deep px-2.5 py-2"
        style={{ fontFamily: MONO, fontSize: font, lineHeight: 1.5, WebkitTouchCallout: "default" }}
      >
        {entries === null ? (
          <div className="py-3 text-dim">{failed ? "no transcript from the bridge" : "loading transcript…"}</div>
        ) : entries.length === 0 && !busy ? (
          <div className="py-3 text-dim">nothing said yet — send the first prompt below</div>
        ) : (
          entries.map((e, i) => (
            <div
              key={i}
              className={cn(
                "whitespace-pre-wrap break-words",
                e.role === "user" &&
                  "mb-2 mt-3 border-l-2 border-gold/60 pl-2.5 text-gold-soft first:mt-0",
                e.role === "assistant" && "mb-2 text-fg",
                e.role === "tool" && "mb-1 text-dim",
              )}
            >
              {e.role === "user" && <span className="mr-1.5 font-bold text-gold">❯</span>}
              {e.role === "tool" && <span className="mr-1.5">⚙</span>}
              {e.text}
            </div>
          ))
        )}
        {busy && (
          <div className="cursor-blink py-1 text-orange-300">working…</div>
        )}
      </div>
    );
  },
);
