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
//   ASTROLABE_TOKEN    shared secret required on /ws and /api/* once set —
//                      unset means NO auth beyond the tailnet IP bind
//                      itself; a startup warning fires either way so this
//                      isn't a silent gap.
//   ASTROLABE_PUSH_REDACT  when set, push notification bodies become a
//                      generic string instead of the pane's actual
//                      subtitle/recap/title text.

import * as crypto from "node:crypto";
import * as http from "node:http";
import * as fs from "node:fs";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import { WebSocketServer, WebSocket } from "ws";
import { ZodiacLink, type SessionState } from "./zodiac.ts";
import { parseQuestion, type PaneQuestion } from "./question.ts";
import { scanCommands } from "./commands.ts";
import { Apns } from "./apns.ts";
import { publishEndpoint } from "./identity.ts";
import { hostStats } from "./host.ts";

const PORT = Number(process.env.ASTROLABE_PORT || 7979);
const SESSION = process.env.ASTROLABE_SESSION || "main";
const DIST = path.join(import.meta.dirname, "..", "web", "dist");
const TOKEN = process.env.ASTROLABE_TOKEN || null;
const PUSH_REDACT = !!process.env.ASTROLABE_PUSH_REDACT;
const MAX_CLIENTS = 20;

/** The connected zodiac server's current per-launch pairing token (see
    src/server.rs's gen_pairing_token / Alt+P in the TUI). Rotates on a
    fresh zodiac launch, stable across detach/reattach, `null` before the
    first state arrives or against an old server that predates this field —
    updated by the `link.on("state", ...)` handler below. */
let sessionToken: string | null = null;

if (!TOKEN) {
  console.error(
    "astrolabe: ASTROLABE_TOKEN is not set — until a phone scans a pairing QR " +
      "(zodiac's Alt+P), or you set one yourself, the bridge is running with NO " +
      "authentication. Anyone who can reach this tailnet IP can read and control " +
      "every pane."
  );
}

function constantTimeEq(a: string, b: string): boolean {
  const ab = Buffer.from(a);
  const bb = Buffer.from(b);
  return ab.length === bb.length && crypto.timingSafeEqual(ab, bb);
}

/** Accepts either the static ASTROLABE_TOKEN (if configured) or the live
    per-launch `sessionToken` the connected zodiac server reports — whoever
    scanned the current QR has this one, without any admin having typed a
    secret anywhere. If neither is ever set (old zodiac server, or no
    bridge/zodiac link at all yet), stays unauthenticated for backward
    compat — see the startup warning above, which fires either way. */
function tokenOk(supplied: string | null): boolean {
  const candidates = [TOKEN, sessionToken].filter((t): t is string => !!t);
  if (candidates.length === 0) return true;
  return !!supplied && candidates.some((c) => constantTimeEq(supplied, c));
}

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

// scanCommands() is a synchronous readdirSync + per-file readFileSync —
// fine once, not on every WS connection (a rapid-reconnect client turns it
// into repeated blocking disk I/O on the event loop). Cache it and refresh
// on a timer instead.
const COMMANDS_REFRESH_MS = 30_000;
let commandsCache = scanCommands();
setInterval(() => (commandsCache = scanCommands()), COMMANDS_REFRESH_MS);

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
let lastPushTest = 0;

function maybePush(
  kind: string,
  pane: { id: number; name: string },
  alert: { title: string; body?: string },
  badge: number,
  question?: PaneQuestion,
) {
  const key = `${pane.id}:${kind}`;
  const now = Date.now();
  if (now - (lastPush.get(key) ?? 0) < PUSH_COOLDOWN_MS) return;
  lastPush.set(key, now);
  // Per-option-count categories are pre-registered by the iOS shell
  // (AGENT_PROMPT_2..5 with numbered answer actions); anything else falls
  // back to the plain reply-only category.
  const optCount = question?.options.length ?? 0;
  const category =
    optCount >= 2 && optCount <= 5 ? `AGENT_PROMPT_${optCount}` : "AGENT_PROMPT";
  apns
    .push(alert, {
      badge,
      category,
      threadId: `pane-${pane.id}`,
      timeSensitive: kind === "needs_input",
      // cid: which paired computer this is, for the iOS shell's
      // multi-computer routing (which Computer to reply/open against).
      extra: {
        pane: pane.id,
        session: SESSION,
        cid: identity.cid,
        ...(question ? { question: question.question, options: question.options } : {}),
      },
    })
    .catch(() => {});
}

/** The parsed question dialog per needs_input pane, merged into the state
    web clients see (publicState) and into push payloads. Entries live from
    the needs_input transition until the pane leaves that status, is
    answered through /api/answer or the WS `answer` action, or closes. */
const questions = new Map<number, PaneQuestion>();

/** needs_input just fired: grab the pane's rendered screen, parse the
    dialog out of it, then push with the real question + options and let
    web clients re-render with answer buttons. Falls back to the old
    subtitle/recap body when there's nothing parseable on screen. */
async function captureQuestion(p: { id: number; name: string; subtitle?: string; recap?: string; title?: string }, badge: number) {
  let q: PaneQuestion | null = null;
  if (link.up) {
    const screen = await link.readScreenOnce(p.id);
    q = screen ? parseQuestion(screen) : null;
  }
  if (q) questions.set(p.id, q);
  else questions.delete(p.id);
  broadcast({ t: "state", state: publicState(link.state) });
  const body = PUSH_REDACT
    ? "needs your attention"
    : q
      ? [q.question, ...q.options.map((o, i) => `${i + 1}. ${o}`)].join("\n")
      : p.subtitle || p.recap || p.title || undefined;
  maybePush(
    "needs_input", p, { title: `${p.name} needs you`, body }, badge,
    PUSH_REDACT ? undefined : q ?? undefined,
  );
}

/** Answer a question dialog: the digit picks the option in Claude Code's
    pickers (the trailing Enter commits it where the digit only moved the
    selection, and lands on an empty input box — a no-op — where it
    didn't). A note, when given, follows as a normal prompt once the
    dialog has had time to close. */
function answerPane(paneId: number, option: number, note: string) {
  link.input(paneId, Buffer.from(String(option), "utf8"));
  setTimeout(() => link.input(paneId, Buffer.from("\r")), 250);
  if (note) {
    setTimeout(() => link.prompt(paneId, note.replaceAll("\n", "\\\r")), 1000);
  }
  questions.delete(paneId);
  broadcast({ t: "state", state: publicState(link.state) });
}

link.on("state", (state: SessionState) => {
  sessionToken = state.pairing_token || null;
  const badge = state.panes.filter((p) => p.status === "needs_input").length;
  for (const p of state.panes) {
    const prev = prevStatus.get(p.id);
    if (prev === "needs_input" && p.status !== "needs_input") questions.delete(p.id);
    if (!prev || prev === p.status) continue;
    if (p.status === "needs_input") {
      captureQuestion(p, badge).catch(() => {});
    } else if (p.status === "done" && prev === "working") {
      maybePush("done", p, {
        title: `${p.name} finished`,
        body: PUSH_REDACT ? "finished" : p.recap || p.subtitle || undefined,
      }, badge);
    }
  }
  prevStatus = new Map(state.panes.map((p) => [p.id, p.status]));
});

link.on("pane_closed", (pane: number) => questions.delete(pane));

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

/** Strips `pairing_token` before anything touches a web client — the token
    only ever needs to travel zodiac→bridge (to populate `sessionToken`
    above) and bridge→phone-camera (the QR image zodiac itself draws).
    Every WS client here already passed tokenOk() to get this far, but a
    live secret sitting in client-visible JSON is worth avoiding regardless
    — devtools, extensions, client-side logging, a future endpoint that
    echoes state back. */
function publicState(state: SessionState | null): SessionState | null {
  if (!state) return state;
  const { pairing_token, ...rest } = state;
  return {
    ...rest,
    // Merge in any parsed question dialog so the web UI can render real
    // answer buttons instead of a text composer the dialog would ignore.
    panes: rest.panes.map((p) => {
      const q = questions.get(p.id);
      return q ? { ...p, question: q.question, options: q.options } : p;
    }),
  };
}

link.on("link", (up: boolean) => broadcast({ t: "link", up }));
link.on("state", (state) => broadcast({ t: "state", state: publicState(state) }));
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

/** A pane id that's actually an integer and actually exists right now —
    the same existence check `/api/prompt` already does, applied here too
    so a WS client can't drive `link.input`/`link.prompt`/`readScreen`
    with a made-up id. */
function paneExists(id: unknown): id is number {
  return typeof id === "number" && Number.isInteger(id) && !!link.state?.panes.some((p) => p.id === id);
}

function onClientMessage(c: Client, raw: string) {
  let msg: any;
  try {
    msg = JSON.parse(raw);
  } catch {
    return;
  }
  // Belt-and-suspenders: `ZodiacLink.send` now guards its own BigInt
  // conversion too, but nothing here should ever let an unanticipated
  // bad message take the whole process down.
  try {
    switch (msg.t) {
      case "view": {
        c.viewing = paneExists(msg.pane) ? msg.pane : null;
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
        if (paneExists(msg.pane) && typeof msg.data === "string") {
          link.input(msg.pane, Buffer.from(msg.data, "utf8"));
        }
        break;
      }
      case "prompt": {
        if (paneExists(msg.pane) && typeof msg.text === "string") {
          link.prompt(msg.pane, msg.text);
        }
        break;
      }
      case "answer": {
        if (
          paneExists(msg.pane) &&
          Number.isInteger(msg.option) && msg.option >= 1 && msg.option <= 9
        ) {
          answerPane(msg.pane, msg.option, typeof msg.note === "string" ? msg.note.trim() : "");
        }
        break;
      }
      default:
        break;
    }
  } catch (e) {
    console.error("astrolabe: error handling a WS message, ignoring it:", e);
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
  const auth = req.headers.authorization;
  const supplied = auth?.startsWith("Bearer ") ? auth.slice(7) : null;
  if (!tokenOk(supplied)) {
    json(res, 401, { error: "unauthorized" });
    return true;
  }
  // Vitals for the phone's title bar — a read, so a plain GET.
  if (url === "/api/host" && req.method === "GET") {
    json(res, 200, await hostStats());
    return true;
  }
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
    // Answer a parsed question dialog by option number — what the iOS
    // notification's numbered actions hit. `note` (optional) follows as a
    // regular prompt after the dialog closes.
    case "/api/answer": {
      if (
        typeof body.pane !== "number" ||
        !Number.isInteger(body.option) || body.option < 1 || body.option > 9
      ) {
        json(res, 400, { error: "need { pane, option (1-9), note? }" });
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
      answerPane(pane.id, body.option, typeof body.note === "string" ? body.note.trim() : "");
      json(res, 200, { ok: true });
      return true;
    }
    case "/api/push-test": {
      // Same cooldown shape as maybePush — a valid token shouldn't still
      // be able to spam every device's lock screen or hammer Apple's push
      // service in a loop.
      const now = Date.now();
      if (now - lastPushTest < PUSH_COOLDOWN_MS) {
        json(res, 429, { error: "too soon, try again later" });
        return true;
      }
      lastPushTest = now;
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
  // A plain `startsWith(DIST)` prefix match would also let a sibling
  // directory like `DIST + "-evil"` through — require the separator (or an
  // exact match) so containment is real, not just a string prefix.
  if (file !== DIST && !file.startsWith(DIST + path.sep)) {
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

// 256 KiB — comfortably above real terminal input/prompt sizes, same order
// of magnitude as readJson's 64 KiB HTTP body cap; `ws`'s own default is an
// unbounded-in-practice 100 MiB per message.
const WS_MAX_PAYLOAD = 256 * 1024;
const wss = new WebSocketServer({ server, path: "/ws", maxPayload: WS_MAX_PAYLOAD });

wss.on("connection", (ws, req) => {
  // Browsers don't enforce CORS/SOP on WebSocket connections — any page
  // loaded by any device that can route to this IP could otherwise open
  // one regardless of which site served it. The Origin header is
  // supplementary, not the real defense (a non-browser client can send
  // any Origin it likes) — tokenOk() below is what actually gates this.
  const origin = req.headers.origin;
  if (origin && origin !== `http://${req.headers.host}`) {
    ws.close(4003, "origin not allowed");
    return;
  }
  const supplied = new URL(req.url || "", "http://x").searchParams.get("t");
  if (!tokenOk(supplied)) {
    ws.close(4001, "unauthorized");
    return;
  }
  if (clients.size >= MAX_CLIENTS) {
    ws.close(4008, "too many connections");
    return;
  }
  const c: Client = { ws, viewing: null };
  clients.add(c);
  sendJson(c, {
    t: "hello",
    session: SESSION,
    state: publicState(link.state),
    link: link.up,
    watch: link.watchSupported,
    commands: commandsCache,
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
const endpointURL = `http://${host}:${PORT}`;
const identity = publishEndpoint(endpointURL);
// endpoint.json is what zodiac's pairing QR advertises, and there is only
// one of it per machine — so a second bridge (another port, another
// session) overwrites it, and pairing then points at whichever bridge
// started last, even after that one exits. Re-claim it periodically: the
// bridge still running wins, within a minute, without any cleanup on exit
// (which a killed process can't do anyway).
setInterval(() => publishEndpoint(endpointURL), 60_000).unref();
link.start();
server.listen(PORT, host, () => {
  console.log(`astrolabe: serving http://${host}:${PORT} → zodiac session '${SESSION}'`);
});
