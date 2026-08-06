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
export const T_QUERY = 11;
export const T_READ_SCREEN = 12;
export const T_WATCH = 14;
// server -> client
export const T_REPLAY = 21;
export const T_OUTPUT = 22;
export const T_PANE_OPENED = 23;
export const T_PANE_CLOSED = 24;
export const T_SERVER_EXIT = 25;
export const T_STATE = 26;
export const T_SCREEN = 27;

export interface PaneState {
  index: number;
  id: number;
  name: string;
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

// Must match (or exceed) the zodiac server's RING_CAP (src/pane.rs) — a
// smaller cap here silently truncates the scrollback every client sees.
const RING_CAP = 2 * 1024 * 1024;
const RECONNECT_MS = 2000;
const POLL_MS = 1000;
const WATCH_PROBE_MS = 1500;

/**
 * Events:
 *   link(up: boolean)          socket to the zodiac server came up / went down
 *   state(state)               fresh SessionState (only when changed)
 *   replay(pane, data)         full ring for a pane (watch hello / reconnect)
 *   output(pane, data)         live pty bytes (graphics already stripped)
 *   screen(pane, text)         poll-mode plain-text screen (only when changed)
 *   pane_closed(pane)
 *   watch(supported: boolean)  resolved once per connection
 */
export class ZodiacLink extends EventEmitter {
  socketPath: string;
  up = false;
  watchSupported: boolean | null = null; // null = still probing
  state: SessionState | null = null;
  rings = new Map<number, Buffer>();

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

  /** Type text, pause, then Enter — same pacing as `zodiac prompt`. */
  prompt(pane: number, text: string) {
    this.send(T_INPUT, pane, Buffer.from(text, "utf8"));
    setTimeout(() => this.send(T_INPUT, pane, Buffer.from("\r")), 200);
  }

  readScreen(pane: number) {
    this.send(T_READ_SCREEN, pane, Buffer.alloc(0));
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

  private onData(chunk: Buffer) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk;
    while (this.buf.length >= 13) {
      const typ = this.buf.readUInt8(0);
      const pane = Number(this.buf.readBigUInt64LE(1));
      const len = this.buf.readUInt32LE(9);
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
          const json = JSON.stringify(state);
          if (json !== this.lastStateJson) {
            this.lastStateJson = json;
            this.emit("state", state);
          }
        } catch {
          /* malformed state frame — skip */
        }
        break;
      }
      case T_REPLAY: {
        if (this.watchSupported === null) {
          this.watchSupported = true;
          if (this.probeTimer) clearTimeout(this.probeTimer);
          this.emit("watch", true);
        }
        this.rings.set(pane, data);
        this.emit("replay", pane, data);
        break;
      }
      case T_OUTPUT: {
        const ring = this.rings.get(pane);
        let next = ring ? Buffer.concat([ring, data]) : data;
        if (next.length > RING_CAP) next = next.subarray(next.length - RING_CAP);
        this.rings.set(pane, next);
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
      case T_PANE_CLOSED: {
        this.rings.delete(pane);
        this.lastScreens.delete(pane);
        this.emit("pane_closed", pane);
        break;
      }
      case T_PANE_OPENED:
        // state poll picks the new pane up within a second
        break;
      case T_SERVER_EXIT:
        this.sock?.destroy();
        break;
      default:
        break; // UI-only frames (gfx etc.) never reach watchers
    }
  }
}
