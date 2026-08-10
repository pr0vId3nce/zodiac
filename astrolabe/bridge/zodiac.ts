// Framed unix-socket client for the zodiac server — the same 13-byte
// header protocol the Rust client speaks: [typ u8][pane id u64 LE][len u32 LE].
//
// On connect it sends T_WATCH (observer hello: T_STATE + per-pane T_REPLAY,
// then live T_OUTPUT). An old server silently ignores T_WATCH — no replay
// ever arrives — so the link degrades to "poll mode": status via T_QUERY
// still works, and screens are mirrored with 1 Hz T_READ_SCREEN polls.

import * as net from "node:net";
import { EventEmitter } from "node:events";

// client -> server
export const T_INPUT = 1;
export const T_RENAME = 5;
export const T_QUERY = 11;
export const T_READ_SCREEN = 12;
export const T_WATCH = 14;
export const T_SEEN = 17;
export const T_TRANSCRIPT_REQ = 18;
export const T_AGENT_INPUT = 19;
export const T_PERM_RESP = 34;
// server -> client
export const T_REPLAY = 21;
export const T_OUTPUT = 22;
export const T_PANE_OPENED = 23;
export const T_PANE_CLOSED = 24;
export const T_PANE_RENAMED = 30; // payload: utf8 name (server auto-naming)
export const T_SERVER_EXIT = 25;
export const T_STATE = 26;
export const T_SCREEN = 27;
export const T_TRANSCRIPT = 31;
export const T_AGENT_EVENT = 32;
export const T_PERM_REQ = 33;

export interface PaneState {
  index: number;
  id: number;
  name: string;
  /** "pty" (a shell/TUI under the VT engine) or "agent" (structured NDJSON
      pane, ADR 0002). Missing from an old server's state = "pty". */
  kind?: string;
  title: string;
  status: string; // working | idle | done | needs_input
  agent: string | null;
  cwd: string | null;
  focused: boolean;
  auto_resume: boolean;
  uptime_ms: number;
  version: string | null;
  thinking: boolean;
  recap: string | null;
  subtitle: string | null;
  /** The host this pane's shell is ssh'd into, if any. */
  ssh?: string | null;
  /** The last few non-blank rendered screen lines (empty for shells). */
  tail?: string[];
}

export interface SessionState {
  session: string;
  attached: boolean;
  rows?: number;
  cols?: number;
  panes: PaneState[];
  /** Random secret minted once per zodiac server process — rotates on a
      fresh launch, stable across detach/reattach. Empty string against an
      old server that predates this field (falls back to ASTROLABE_TOKEN-only
      or unauthenticated, same as before this existed). */
  pairing_token?: string;
}

/** T_PERM_REQ payload: one pending tool-permission request from an agent
    pane (claude's can_use_tool, ADR 0002). The server-side inbox is
    authoritative — requests persist until answered or expired (10 min =
    deny) and are re-sent on watch/attach. */
export interface PermRequest {
  request_id: string;
  tool_name: string;
  display_name?: string | null;
  /** The tool input as the agent sent it (raw JSON value). */
  input: unknown;
  /** Milliseconds this request has been pending (for UI countdowns, and
      for telling a live request from an attach-replay re-send). */
  age_ms: number;
}

/** T_PERM_RESP payload. */
export interface PermResponse {
  request_id: string;
  behavior: "allow" | "deny";
  /** Optional message shown to the agent on deny. */
  message?: string;
}

// Must match (or exceed) the zodiac server's RING_CAP (src/pane.rs) — a
// smaller cap here silently truncates the scrollback every client sees.
const RING_CAP = 6 * 1024 * 1024;
const RECONNECT_MS = 2000;
const POLL_MS = 1000;
/// Generous: the server's event loop can stall a couple of seconds (a
/// blocking monitor probe, a busy pane), and a probe that expires during
/// such a stall used to lock the bridge into poll mode — phones then only
/// ever saw the current screen. A late replay also upgrades us back to
/// watch mode (see the T_REPLAY case), so this window is belt, not roof.
const WATCH_PROBE_MS = 6000;

/**
 * Events:
 *   link(up: boolean)          socket to the zodiac server came up / went down
 *   state(state)               fresh SessionState (only when changed)
 *   replay(pane, data)         full ring for a pane (watch hello / reconnect)
 *   output(pane, data)         live pty bytes (graphics already stripped)
 *   screen(pane, text)         poll-mode plain-text screen (only when changed)
 *   pane_closed(pane)
 *   watch(supported: boolean)  resolved once per connection
 *   agent_event(pane, lines)   NDJSON transcript lines for an agent pane
 *                              (watch replay = the whole ring at once)
 *   perm_req(pane, req)        pending tool-permission request (agent pane)
 */
export class ZodiacLink extends EventEmitter {
  socketPath: string;
  up = false;
  watchSupported: boolean | null = null; // null = still probing
  state: SessionState | null = null;
  /** Per-pane scrollback as a chunk list: appending an output frame is a
      push, not a full-ring Buffer.concat (which cost a memcpy of up to
      2 MiB per pty frame on busy panes). Compacted lazily in ringOf(). */
  private rings = new Map<number, { chunks: Buffer[]; len: number }>();

  /** The pane's ring as one buffer — for the replay a `view` sends. */
  ringOf(pane: number): Buffer {
    const r = this.rings.get(pane);
    if (!r || r.chunks.length === 0) return Buffer.alloc(0);
    if (r.chunks.length > 1) {
      r.chunks = [Buffer.concat(r.chunks)];
    }
    return r.chunks[0];
  }

  private sock: net.Socket | null = null;
  private buf: Buffer = Buffer.alloc(0);
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private probeTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private lastStateJson = "";
  private lastScreens = new Map<number, string>();
  /** Pane ids at least one web client is currently viewing (poll mode). */
  private viewed = new Set<number>();
  private stopped = false;

  constructor(socketPath: string) {
    super();
    this.socketPath = socketPath;
  }

  start() {
    this.stopped = false;
    this.connect();
  }

  stop() {
    this.stopped = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.teardown();
  }

  /** Web clients tell us which panes they watch, for poll-mode mirroring. */
  setViewed(panes: Iterable<number>) {
    this.viewed = new Set(panes);
  }

  input(pane: number, data: Buffer) {
    this.send(T_INPUT, pane, data);
  }

  /** Rename a pane; an empty name clears the pin back to auto-naming. */
  rename(pane: number, name: string) {
    this.send(T_RENAME, pane, Buffer.from(name, "utf8"));
  }

  /** Type text, pause, then Enter — same pacing as `zodiac prompt`. */
  prompt(pane: number, text: string) {
    this.send(T_INPUT, pane, Buffer.from(text, "utf8"));
    setTimeout(() => this.send(T_INPUT, pane, Buffer.from("\r")), 200);
  }

  /** One user prompt for an agent pane (`kind == "agent"`): text goes out
      verbatim as T_AGENT_INPUT — the server wraps it in the agent's native
      input envelope, so no keystroke pacing and no newline wiring. An old
      server silently ignores the frame. */
  agentInput(pane: number, text: string) {
    this.send(T_AGENT_INPUT, pane, Buffer.from(text, "utf8"));
  }

  /** Answer a pending T_PERM_REQ. Old servers silently ignore this. */
  permResponse(pane: number, resp: PermResponse) {
    this.send(T_PERM_RESP, pane, Buffer.from(JSON.stringify(resp), "utf8"));
  }

  readScreen(pane: number) {
    this.send(T_READ_SCREEN, pane, Buffer.alloc(0));
  }

  /** A phone opened this pane — acknowledge its sticky "finished" flag,
      the way focusing it in the TUI would. Old servers ignore the frame. */
  seen(pane: number) {
    this.send(T_SEEN, pane, Buffer.alloc(0));
  }

  private pendingReads = new Map<number, ((text: string | null) => void)[]>();

  /** One-shot rendered-screen fetch. Unlike the `screen` event (deduped
      against lastScreens for poll-mode mirroring), this always resolves
      with the current screen — or null after `timeoutMs` if the link is
      down or the server never answers. */
  readScreenOnce(pane: number, timeoutMs = 2000): Promise<string | null> {
    return new Promise((resolve) => {
      const list = this.pendingReads.get(pane) ?? [];
      list.push(resolve);
      this.pendingReads.set(pane, list);
      this.readScreen(pane);
      setTimeout(() => {
        const cur = this.pendingReads.get(pane) ?? [];
        if (cur.includes(resolve)) {
          this.pendingReads.set(pane, cur.filter((f) => f !== resolve));
          resolve(null);
        }
      }, timeoutMs);
    });
  }

  private pendingTranscripts = new Map<number, ((json: string | null) => void)[]>();

  /** One-shot transcript fetch: the pane's conversation as rendered
      entries, parsed server-side from the agent's session transcript (see
      the Rust side's `SrvPane::transcript_json`). Resolves with the raw
      JSON array string ("" when the pane has no transcript), or null after
      `timeoutMs` when the link is down or the server predates the frame. */
  transcriptOnce(pane: number, timeoutMs = 3000): Promise<string | null> {
    return new Promise((resolve) => {
      const list = this.pendingTranscripts.get(pane) ?? [];
      list.push(resolve);
      this.pendingTranscripts.set(pane, list);
      this.send(T_TRANSCRIPT_REQ, pane, Buffer.alloc(0));
      setTimeout(() => {
        const cur = this.pendingTranscripts.get(pane) ?? [];
        if (cur.includes(resolve)) {
          this.pendingTranscripts.set(pane, cur.filter((f) => f !== resolve));
          resolve(null);
        }
      }, timeoutMs);
    });
  }

  private connect() {
    if (this.stopped) return;
    const sock = net.connect(this.socketPath);
    this.sock = sock;
    sock.on("connect", () => {
      this.up = true;
      this.buf = Buffer.alloc(0);
      this.watchSupported = null;
      this.emit("link", true);
      this.send(T_WATCH, 0, Buffer.alloc(0));
      this.send(T_QUERY, 0, Buffer.alloc(0));
      // No replay within the probe window ⇒ old server, fall back to polling.
      this.probeTimer = setTimeout(() => {
        if (this.watchSupported === null) {
          this.watchSupported = false;
          this.emit("watch", false);
        }
      }, WATCH_PROBE_MS);
      this.pollTimer = setInterval(() => this.poll(), POLL_MS);
    });
    sock.on("data", (chunk) => this.onData(chunk));
    sock.on("error", () => {});
    sock.on("close", () => {
      const wasUp = this.up;
      this.teardown();
      if (wasUp) this.emit("link", false);
      if (!this.stopped) {
        this.reconnectTimer = setTimeout(() => this.connect(), RECONNECT_MS);
      }
    });
  }

  private teardown() {
    this.up = false;
    if (this.pollTimer) clearInterval(this.pollTimer);
    if (this.probeTimer) clearTimeout(this.probeTimer);
    this.pollTimer = null;
    this.probeTimer = null;
    this.lastStateJson = "";
    this.lastScreens.clear();
    // The next connection may be a different zodiac process whose pane ids
    // collide with these; watch-mode replays repopulate on reconnect, and
    // rings for panes that died while the link was down would otherwise
    // leak up to 2 MiB each forever.
    this.rings.clear();
    if (this.sock) {
      this.sock.removeAllListeners("close");
      this.sock.destroy();
      this.sock = null;
    }
  }

  private poll() {
    this.send(T_QUERY, 0, Buffer.alloc(0));
    if (this.watchSupported === false) {
      for (const pane of this.viewed) this.readScreen(pane);
    }
  }

  private send(typ: number, pane: number, data: Buffer) {
    if (!this.sock || this.sock.destroyed) return;
    // Defense in depth: callers should already validate `pane`, but
    // BigInt(pane)/writeBigUInt64LE throw a synchronous RangeError on a
    // non-integer or negative value (confirmed: this used to be reachable
    // straight from an untrusted WS message with no try/catch anywhere
    // above it — a one-frame remote crash of the whole bridge process).
    // Silently drop rather than let that propagate.
    if (!Number.isInteger(pane) || pane < 0 || pane > Number.MAX_SAFE_INTEGER) return;
    const hdr = Buffer.alloc(13);
    hdr.writeUInt8(typ, 0);
    hdr.writeBigUInt64LE(BigInt(pane), 1);
    hdr.writeUInt32LE(data.length, 9);
    this.sock.write(Buffer.concat([hdr, data]));
  }

  /** Mirrors the Rust side's MAX_FRAME: a corrupt/desynced length would
      otherwise buffer forever waiting for a frame that never completes,
      with the link still looking "up". */
  private static readonly MAX_FRAME = 8 * 1024 * 1024;

  private onData(chunk: Buffer) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk;
    while (this.buf.length >= 13) {
      const typ = this.buf.readUInt8(0);
      const pane = Number(this.buf.readBigUInt64LE(1));
      const len = this.buf.readUInt32LE(9);
      if (len > ZodiacLink.MAX_FRAME) {
        // Desynced or corrupt — drop the socket; reconnect resyncs.
        this.sock?.destroy();
        return;
      }
      if (this.buf.length < 13 + len) break;
      const data = this.buf.subarray(13, 13 + len);
      this.buf = this.buf.subarray(13 + len);
      this.onFrame(typ, pane, Buffer.from(data));
    }
  }

  private onFrame(typ: number, pane: number, data: Buffer) {
    switch (typ) {
      case T_STATE: {
        try {
          const state = JSON.parse(data.toString("utf8")) as SessionState;
          this.state = state;
          // Dedup on a normalized copy: uptime_ms ticks every second and
          // host vitals jitter, so comparing the raw JSON made "only when
          // changed" always fire — a full-state broadcast to every phone
          // at 1 Hz. Uptime still busts the dedup once a minute, keeping
          // the displayed values honest.
          const norm = JSON.stringify({
            ...state,
            host: undefined,
            panes: state.panes.map((p) => ({
              ...p,
              uptime_ms: Math.floor((p.uptime_ms ?? 0) / 60_000),
            })),
          });
          if (norm !== this.lastStateJson) {
            this.lastStateJson = norm;
            this.emit("state", state);
          }
        } catch {
          /* malformed state frame — skip */
        }
        break;
      }
      case T_REPLAY: {
        // A replay is proof of watch support, even when it lands after
        // the probe already gave up — upgrade out of poll mode instead of
        // staying degraded until the next bridge restart.
        if (this.watchSupported !== true) {
          this.watchSupported = true;
          if (this.probeTimer) clearTimeout(this.probeTimer);
          this.emit("watch", true);
        }
        this.rings.set(pane, { chunks: [data], len: data.length });
        this.emit("replay", pane, data);
        break;
      }
      case T_OUTPUT: {
        const r = this.rings.get(pane) ?? { chunks: [], len: 0 };
        r.chunks.push(data);
        r.len += data.length;
        // Trim whole head chunks once the tail alone still covers the cap
        // — O(1) amortized, and consumers only ever want "about the last
        // RING_CAP bytes".
        while (r.chunks.length > 1 && r.len - r.chunks[0].length >= RING_CAP) {
          r.len -= r.chunks[0].length;
          r.chunks.shift();
        }
        this.rings.set(pane, r);
        this.emit("output", pane, data);
        break;
      }
      case T_SCREEN: {
        const text = data.toString("utf8");
        const waiters = this.pendingReads.get(pane);
        if (waiters?.length) {
          this.pendingReads.set(pane, []);
          for (const w of waiters) w(text);
        }
        if (this.lastScreens.get(pane) !== text) {
          this.lastScreens.set(pane, text);
          this.emit("screen", pane, text);
        }
        break;
      }
      case T_TRANSCRIPT: {
        const waiters = this.pendingTranscripts.get(pane);
        if (waiters?.length) {
          this.pendingTranscripts.set(pane, []);
          const json = data.toString("utf8");
          for (const w of waiters) w(json);
        }
        break;
      }
      case T_AGENT_EVENT: {
        // One or more newline-joined NDJSON lines: live traffic is one line
        // per frame, a watch/attach replay is the pane's whole transcript
        // ring in a single frame. Consumers get lines, never raw bytes.
        const lines = data.toString("utf8").split("\n").filter((l) => l.trim());
        if (lines.length) this.emit("agent_event", pane, lines);
        break;
      }
      case T_PERM_REQ: {
        try {
          const req = JSON.parse(data.toString("utf8")) as PermRequest;
          if (typeof req.request_id === "string" && typeof req.tool_name === "string") {
            this.emit("perm_req", pane, req);
          }
        } catch {
          /* malformed permission frame — skip */
        }
        break;
      }
      case T_PANE_CLOSED: {
        this.rings.delete(pane);
        this.lastScreens.delete(pane);
        this.emit("pane_closed", pane);
        break;
      }
      case T_PANE_OPENED:
        // state poll picks the new pane up within a second
        break;
      case T_PANE_RENAMED:
        // Auto-rename: refresh state now so the phone updates the pane name
        // immediately instead of waiting for the 1 Hz poll.
        this.send(T_QUERY, 0, Buffer.alloc(0));
        break;
      case T_SERVER_EXIT:
        this.sock?.destroy();
        break;
      default:
        break; // UI-only frames (gfx etc.) never reach watchers
    }
  }
}
