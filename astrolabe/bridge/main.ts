// Astrolabe bridge — serves the phone PWA and relays between web clients and the
// zodiac server socket. Run with plain `node main.ts` (Node ≥ 23 strips the
// types natively; bun runs it as-is too).
//
// Env:
//   ASTROLABE_PORT     listen port                (default 7979)
//   ASTROLABE_HOST     bind address                (default: this machine's
//                      tailscale IPv4, so the UI is tailnet-only by
//                      construction; falls back to 127.0.0.1 when
//                      tailscale isn't up)
//   ASTROLABE_SESSION  zodiac session name         (default "main")

import * as http from "node:http";
import * as fs from "node:fs";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import { WebSocketServer, WebSocket } from "ws";
import { ZodiacLink, type SessionState } from "./zodiac.ts";
import { scanCommands } from "./commands.ts";
import { Apns } from "./apns.ts";

const PORT = Number(process.env.ASTROLABE_PORT || 7979);
const SESSION = process.env.ASTROLABE_SESSION || "main";
const DIST = path.join(import.meta.dirname, "..", "web", "dist");

function tailscaleIp(): string | null {
  try {
    const out = execFileSync("tailscale", ["ip", "-4"], {
      encoding: "utf8",
      timeout: 3000,
    });
    const ip = out.split("\n")[0].trim();
    return /^\d+\.\d+\.\d+\.\d+$/.test(ip) ? ip : null;
  } catch {
    return null;
  }
}

function socketPath(session: string): string {
  const run = process.env.XDG_RUNTIME_DIR || `/tmp/zodiac-${process.getuid?.()}`;
  return path.join(run, "zodiac", `${session}.sock`);
}

// ---------------------------------------------------------------- zodiac link

const link = new ZodiacLink(socketPath(SESSION));

// ------------------------------------------------------------ notifications

const apns = new Apns();

// Push when a pane flips to needs_input, or an agent finishes. Panes with no
// previous known status (bridge just started / pane just appeared) don't
// fire — that keeps bridge restarts silent.
const PUSH_COOLDOWN_MS = 30_000;
let prevStatus = new Map<number, string>();
const lastPush = new Map<string, number>();

function maybePush(kind: string, pane: { id: number; name: string }, alert: { title: string; body?: string }, badge: number) {
  const key = `${pane.id}:${kind}`;
  const now = Date.now();
  if (now - (lastPush.get(key) ?? 0) < PUSH_COOLDOWN_MS) return;
  lastPush.set(key, now);
  apns
    .push(alert, {
      badge,
      category: "AGENT_PROMPT",
      threadId: `pane-${pane.id}`,
      timeSensitive: kind === "needs_input",
      extra: { pane: pane.id, session: SESSION },
    })
    .catch(() => {});
}

link.on("state", (state: SessionState) => {
  const badge = state.panes.filter((p) => p.status === "needs_input").length;
  for (const p of state.panes) {
    const prev = prevStatus.get(p.id);
    if (!prev || prev === p.status) continue;
    if (p.status === "needs_input") {
      maybePush("needs_input", p, {
        title: `${p.name} needs you`,
        body: p.subtitle || p.recap || p.title || undefined,
      }, badge);
    } else if (p.status === "done" && prev === "working") {
      maybePush("done", p, {
        title: `${p.name} finished`,
        body: p.recap || p.subtitle || undefined,
      }, badge);
    }
  }
  prevStatus = new Map(state.panes.map((p) => [p.id, p.status]));
});

// ------------------------------------------------------------------- web side

interface Client {
  ws: WebSocket;
  viewing: number | null; // pane id
}

const clients = new Set<Client>();

function refreshViewed() {
  const viewed = new Set<number>();
  for (const c of clients) if (c.viewing !== null) viewed.add(c.viewing);
  link.setViewed(viewed);
}

function sendJson(c: Client, msg: unknown) {
  if (c.ws.readyState === WebSocket.OPEN) c.ws.send(JSON.stringify(msg));
}

function broadcast(msg: unknown, pane?: number) {
  const json = JSON.stringify(msg);
  for (const c of clients) {
    if (pane !== undefined && c.viewing !== pane) continue;
    if (c.ws.readyState === WebSocket.OPEN) c.ws.send(json);
  }
}

link.on("link", (up: boolean) => broadcast({ t: "link", up }));
link.on("state", (state) => broadcast({ t: "state", state }));
link.on("watch", (supported: boolean) => broadcast({ t: "watch", supported }));
link.on("pane_closed", (pane: number) => broadcast({ t: "pane_closed", pane }));
link.on("output", (pane: number, data: Buffer) =>
  broadcast({ t: "output", pane, data: data.toString("base64") }, pane)
);
link.on("replay", (pane: number, data: Buffer) =>
  broadcast({ t: "replay", pane, data: data.toString("base64") }, pane)
);
link.on("screen", (pane: number, text: string) =>
  broadcast({ t: "screen", pane, text }, pane)
);

function onClientMessage(c: Client, raw: string) {
  let msg: any;
  try {
    msg = JSON.parse(raw);
  } catch {
    return;
  }
  switch (msg.t) {
    case "view": {
      c.viewing = typeof msg.pane === "number" ? msg.pane : null;
      refreshViewed();
      if (c.viewing !== null) {
        if (link.watchSupported) {
          const ring = link.rings.get(c.viewing);
          sendJson(c, {
            t: "replay",
            pane: c.viewing,
            data: (ring ?? Buffer.alloc(0)).toString("base64"),
          });
        } else if (link.up) {
          link.readScreen(c.viewing); // poll-mode: fetch a screen right away
        }
      }
      break;
    }
    case "input": {
      if (typeof msg.pane === "number" && typeof msg.data === "string") {
        link.input(msg.pane, Buffer.from(msg.data, "utf8"));
      }
      break;
    }
    case "prompt": {
      if (typeof msg.pane === "number" && typeof msg.text === "string") {
        link.prompt(msg.pane, msg.text);
      }
      break;
    }
    default:
      break;
  }
}

// ------------------------------------------------------------------ http+ws

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".json": "application/json",
  ".webmanifest": "application/manifest+json",
  ".woff2": "font/woff2",
};

function readJson(req: http.IncomingMessage, limit = 64 * 1024): Promise<any> {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => {
      size += c.length;
      if (size > limit) {
        reject(new Error("body too large"));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
      } catch {
        reject(new Error("bad json"));
      }
    });
    req.on("error", reject);
  });
}

function json(res: http.ServerResponse, code: number, body: unknown) {
  res.writeHead(code, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

// The native iOS shell talks to these; the PWA keeps using the WebSocket.
async function handleApi(req: http.IncomingMessage, res: http.ServerResponse, url: string): Promise<boolean> {
  if (!url.startsWith("/api/")) return false;
  if (req.method !== "POST") {
    json(res, 405, { error: "POST only" });
    return true;
  }
  let body: any;
  try {
    body = await readJson(req);
  } catch (e) {
    json(res, 400, { error: String((e as Error).message) });
    return true;
  }
  switch (url) {
    case "/api/apns/register": {
      if (typeof body.token !== "string" || !apns.register(body.token)) {
        json(res, 400, { error: "bad token" });
      } else {
        json(res, 200, { ok: true, push: apns.enabled });
      }
      return true;
    }
    case "/api/apns/unregister": {
      if (typeof body.token === "string") apns.unregister(body.token);
      json(res, 200, { ok: true });
      return true;
    }
    // Inline reply from a notification. Same wiring the web client does:
    // agent panes get newlines as `\` + Enter so Claude Code keeps them.
    case "/api/prompt": {
      if (typeof body.pane !== "number" || typeof body.text !== "string" || !body.text.trim()) {
        json(res, 400, { error: "need { pane, text }" });
        return true;
      }
      if (!link.up) {
        json(res, 503, { error: "zodiac link down" });
        return true;
      }
      const pane = link.state?.panes.find((p) => p.id === body.pane);
      if (!pane) {
        json(res, 404, { error: "no such pane" });
        return true;
      }
      const text = body.text.replace(/\s+$/, "");
      link.prompt(pane.id, pane.agent ? text.replaceAll("\n", "\\\r") : text);
      json(res, 200, { ok: true });
      return true;
    }
    case "/api/push-test": {
      const r = await apns.push(
        { title: "Astrolabe", body: "push test — the bridge can reach you" },
        { extra: { pane: null } }
      );
      json(res, 200, { ok: true, enabled: apns.enabled, devices: apns.deviceCount, ...r });
      return true;
    }
    default:
      json(res, 404, { error: "unknown endpoint" });
      return true;
  }
}

const server = http.createServer(async (req, res) => {
  const url = (req.url || "/").split("?")[0];
  if (url === "/healthz") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify({
        ok: true,
        session: SESSION,
        link: link.up,
        watch: link.watchSupported,
        panes: link.state?.panes.length ?? 0,
        push: apns.enabled,
        devices: apns.deviceCount,
      })
    );
    return;
  }
  try {
    if (await handleApi(req, res, url)) return;
  } catch {
    json(res, 500, { error: "internal" });
    return;
  }
  let file = path.normalize(path.join(DIST, url === "/" ? "index.html" : url));
  if (!file.startsWith(DIST)) {
    res.writeHead(403).end();
    return;
  }
  if (!fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    file = path.join(DIST, "index.html"); // SPA fallback
  }
  try {
    const body = fs.readFileSync(file);
    res.writeHead(200, {
      "content-type": MIME[path.extname(file)] || "application/octet-stream",
      "cache-control": file.endsWith("index.html")
        ? "no-cache"
        : "public, max-age=31536000, immutable",
    });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
});

const wss = new WebSocketServer({ server, path: "/ws" });

wss.on("connection", (ws) => {
  const c: Client = { ws, viewing: null };
  clients.add(c);
  sendJson(c, {
    t: "hello",
    session: SESSION,
    state: link.state,
    link: link.up,
    watch: link.watchSupported,
    commands: scanCommands(),
  });
  ws.on("message", (raw) => onClientMessage(c, raw.toString()));
  ws.on("close", () => {
    clients.delete(c);
    refreshViewed();
  });
  ws.on("error", () => {});
});

// Keepalive: drop web clients that stopped ponging (phone locked etc.).
setInterval(() => {
  for (const c of clients) {
    if ((c as any).dead) {
      c.ws.terminate();
      clients.delete(c);
      continue;
    }
    (c as any).dead = true;
    c.ws.ping();
    c.ws.once("pong", () => ((c as any).dead = false));
  }
  refreshViewed();
}, 30000);

// Bind to the tailscale IP so the UI is tailnet-only. On boot the bridge may
// start before tailscaled has an address — retry for a while before giving
// up and binding loopback (systemd Restart= then gets another shot later).
async function resolveHost(): Promise<string> {
  if (process.env.ASTROLABE_HOST) return process.env.ASTROLABE_HOST;
  for (let i = 0; i < 15; i++) {
    const ip = tailscaleIp();
    if (ip) return ip;
    await new Promise((r) => setTimeout(r, 2000));
  }
  console.error("astrolabe: no tailscale IPv4 after 30s — binding 127.0.0.1");
  return "127.0.0.1";
}

const host = await resolveHost();
link.start();
server.listen(PORT, host, () => {
  console.log(`astrolabe: serving http://${host}:${PORT} → zodiac session '${SESSION}'`);
});
