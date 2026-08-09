// One auto-reconnecting WebSocket to the bridge, exposed as a tiny store.
// Terminal byte streams bypass React state (see Pane.tsx) — only herd-level
// data lives here.

import { useEffect, useSyncExternalStore } from "react";
import type { CommandSets, PermRequest, ServerMsg, SessionState } from "./types";
import { getToken } from "./auth";

export interface AstrolabeState {
  connected: boolean; // ws to bridge
  link: boolean; // bridge to zodiac server
  watch: boolean | null; // live mirror vs poll fallback
  session: string;
  state: SessionState | null;
  commands: CommandSets;
  /** Pending tool-permission requests per agent pane id, oldest first —
      seeded by hello, kept live by perm_req/perm_resolved. */
  perms: Record<number, PermRequest[]>;
  /** The bridge closed the connection specifically for a missing/wrong
      token (close code 4001) — worth telling apart from "still trying to
      reconnect," since retrying with the same bad token forever looks
      identical to a network problem otherwise. */
  unauthorized: boolean;
}

type StreamListener = (msg: ServerMsg) => void;

class AstrolabeClient {
  ws: WebSocket | null = null;
  snapshot: AstrolabeState = {
    connected: false,
    link: false,
    watch: null,
    session: "main",
    state: null,
    commands: {},
    perms: {},
    unauthorized: false,
  };
  private listeners = new Set<() => void>();
  private streams = new Set<StreamListener>();
  private retry: ReturnType<typeof setTimeout> | null = null;

  start() {
    if (this.ws) return;
    this.connect();
  }

  private connect() {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const token = getToken();
    const qs = token ? `?t=${encodeURIComponent(token)}` : "";
    const ws = new WebSocket(`${proto}://${location.host}/ws${qs}`);
    this.ws = ws;
    ws.onopen = () => this.set({ connected: true, unauthorized: false });
    ws.onclose = (ev) => {
      this.ws = null;
      this.set({ connected: false, unauthorized: ev.code === 4001 });
      this.retry = setTimeout(() => this.connect(), 1500);
    };
    ws.onerror = () => ws.close();
    ws.onmessage = (ev) => {
      let msg: ServerMsg;
      try {
        msg = JSON.parse(ev.data);
      } catch {
        return;
      }
      switch (msg.t) {
        case "hello": {
          // The hello's perm list is authoritative — it replaces anything
          // held across a reconnect (requests settled while we were away).
          const perms: Record<number, PermRequest[]> = {};
          for (const { pane, req } of msg.perms ?? []) {
            (perms[pane] ??= []).push(req);
          }
          this.set({
            session: msg.session,
            state: msg.state ?? this.snapshot.state,
            link: msg.link,
            watch: msg.watch,
            commands: msg.commands,
            perms,
          });
          break;
        }
        case "state":
          this.set({ state: msg.state });
          break;
        case "perm_req": {
          const cur = this.snapshot.perms[msg.pane] ?? [];
          // attach replays re-send pending requests — dedup on request_id
          const perms = {
            ...this.snapshot.perms,
            [msg.pane]: [
              ...cur.filter((r) => r.request_id !== msg.req.request_id),
              msg.req,
            ],
          };
          this.set({ perms });
          break;
        }
        case "perm_resolved": {
          const cur = this.snapshot.perms[msg.pane];
          if (!cur) break;
          const left = cur.filter((r) => r.request_id !== msg.request_id);
          const perms = { ...this.snapshot.perms };
          if (left.length) perms[msg.pane] = left;
          else delete perms[msg.pane];
          this.set({ perms });
          break;
        }
        case "pane_closed": {
          if (this.snapshot.perms[msg.pane]) {
            const perms = { ...this.snapshot.perms };
            delete perms[msg.pane];
            this.set({ perms });
          }
          break;
        }
        case "link":
          this.set({ link: msg.up });
          break;
        case "watch":
          this.set({ watch: msg.supported });
          break;
        default:
          break;
      }
      for (const fn of this.streams) fn(msg);
    };
  }

  private set(patch: Partial<AstrolabeState>) {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const fn of this.listeners) fn();
  }

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  onStream(fn: StreamListener) {
    this.streams.add(fn);
    return () => this.streams.delete(fn);
  }

  send(msg: unknown) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  view(pane: number | null) {
    this.send({ t: "view", pane });
  }

  input(pane: number, data: string) {
    this.send({ t: "input", pane, data });
  }

  promptPane(pane: number, text: string) {
    this.send({ t: "prompt", pane, text });
  }

  /** Answer a question dialog by option number, optionally following up
      with a note once the dialog closes (wired server-side). */
  answer(pane: number, option: number, note?: string) {
    this.send({ t: "answer", pane, option, note });
  }

  /** Allow/Deny an agent pane's pending tool-permission request. The note
      becomes the deny message, or a follow-up prompt on allow. */
  perm(pane: number, requestId: string, behavior: "allow" | "deny", note?: string) {
    this.send({ t: "perm", pane, request_id: requestId, behavior, note });
  }
}

export const client = new AstrolabeClient();

export function useAstrolabe(): AstrolabeState {
  useEffect(() => client.start(), []);
  return useSyncExternalStore(client.subscribe, () => client.snapshot);
}
