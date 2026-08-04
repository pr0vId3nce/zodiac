// One auto-reconnecting WebSocket to the bridge, exposed as a tiny store.
// Terminal byte streams bypass React state (see Pane.tsx) — only herd-level
// data lives here.

import { useEffect, useSyncExternalStore } from "react";
import type { ServerMsg, SessionState, SlashCommand } from "./types";

export interface AstrolabeState {
  connected: boolean; // ws to bridge
  link: boolean; // bridge to zodiac server
  watch: boolean | null; // live mirror vs poll fallback
  session: string;
  state: SessionState | null;
  commands: SlashCommand[];
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
    commands: [],
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
    const ws = new WebSocket(`${proto}://${location.host}/ws`);
    this.ws = ws;
    ws.onopen = () => this.set({ connected: true });
    ws.onclose = () => {
      this.ws = null;
      this.set({ connected: false });
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
        case "hello":
          this.set({
            session: msg.session,
            state: msg.state ?? this.snapshot.state,
            link: msg.link,
            watch: msg.watch,
            commands: msg.commands,
          });
          break;
        case "state":
          this.set({ state: msg.state });
          break;
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
}

export const client = new AstrolabeClient();

export function useAstrolabe(): AstrolabeState {
  useEffect(() => client.start(), []);
  return useSyncExternalStore(client.subscribe, () => client.snapshot);
}
