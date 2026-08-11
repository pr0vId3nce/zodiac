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
import * as os from "node:os";
import * as http from "node:http";
import * as fs from "node:fs";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import { WebSocketServer, WebSocket } from "ws";
import { ZodiacLink, type PaneState, type PermRequest, type SessionState } from "./zodiac.ts";
import { parseQuestion, type PaneQuestion } from "./question.ts";
import { scanCommands } from "./commands.ts";
import { Apns, stateDir } from "./apns.ts";
import { publishEndpoint } from "./identity.ts";
import { hostStats } from "./host.ts";

// ~/.config/astrolabe/env holds the secrets (ASTROLABE_APNS_*,
// ASTROLABE_TOKEN). The Linux systemd unit loads it via EnvironmentFile=,
// but launchd has no equivalent — so the bridge reads it itself, before
// any config constant below is evaluated. KEY=value lines, # comments,
// optional surrounding quotes; the process environment wins on conflict
// so a unit-level override still works.
function loadEnvFile() {
  const dir =
    process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config");
  let text: string;
  try {
    text = fs.readFileSync(path.join(dir, "astrolabe", "env"), "utf8");
  } catch {
    return;
  }
  for (const line of text.split("\n")) {
    const m = line.match(/^\s*(?:export\s+)?([A-Z0-9_]+)\s*=\s*(.*?)\s*$/);
    if (!m || line.trimStart().startsWith("#")) continue;
    const [, key, raw] = m;
    const value = raw.replace(/^(["'])(.*)\1$/, "$2");
    if (!(key in process.env)) process.env[key] = value;
  }
}
loadEnvFile();

const PORT = Number(process.env.ASTROLABE_PORT || 7979);
const SESSION = process.env.ASTROLABE_SESSION || "main";
const DIST = path.join(import.meta.dirname, "..", "web", "dist");
const TOKEN = process.env.ASTROLABE_TOKEN || null;
const PUSH_REDACT = !!process.env.ASTROLABE_PUSH_REDACT;
const MAX_CLIENTS = 20;

/** The connected zodiac server's pairing token (see src/server.rs's
    load_or_mint_pairing_token / Alt+P in the TUI). Mirrored to disk so a
    bridge restart enforces the last-known token from its very first
    request — without that, the stretch between bridge start and the first
    T_STATE (indefinite if zodiac happens to be down) served the whole API
    unauthenticated on the tailnet even though phones were paired. */
const SESSION_TOKEN_FILE = path.join(stateDir(), `session-token-${SESSION}`);
let sessionToken: string | null = (() => {
  try {
    const t = fs.readFileSync(SESSION_TOKEN_FILE, "utf8").trim();
    return /^[0-9a-f]{32,}$/i.test(t) ? t : null;
  } catch {
    return null;
  }
})();

function rememberSessionToken(tok: string) {
  if (tok === sessionToken) return;
  sessionToken = tok;
  try {
    fs.mkdirSync(stateDir(), { recursive: true });
    fs.writeFileSync(SESSION_TOKEN_FILE, tok, { mode: 0o600 });
  } catch {
    /* still enforced in memory for this process's lifetime */
  }
}

if (!TOKEN && !sessionToken) {
  console.error(
    "astrolabe: ASTROLABE_TOKEN is not set and no pairing token is known yet — " +
      "control paths (/ws, /api) are DENIED until zodiac connects and reports a " +
      "pairing token (it does automatically), or you set ASTROLABE_TOKEN. The " +
      "PWA still loads and will connect once a token exists."
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
    secret anywhere. Fails CLOSED: if no token exists yet (old zodiac server
    with no pairing token, or the zodiac link isn't up), control paths are
    denied rather than served to the whole tailnet. The zodiac server reports
    a pairing token automatically, so this only gates the brief startup window
    (the PWA reconnects) or a genuinely token-less deployment. */
function tokenOk(supplied: string | null): boolean {
  const candidates = [TOKEN, sessionToken].filter((t): t is string => !!t);
  if (candidates.length === 0) return false;
  return !!supplied && candidates.some((c) => constantTimeEq(supplied, c));
}

/** True once any accepting token exists (static ASTROLABE_TOKEN or the learned
    per-launch pairing token). Lets callers tell a transient "not ready yet"
    (no token learned — retry) apart from a genuine "wrong token". */
function haveToken(): boolean {
  return !!(TOKEN || sessionToken);
}

/// This machine's tailnet IPv4, read straight off the interface list: a
/// tailscale address is always in the CGNAT block (100.64.0.0/10) on the
/// utun/tailscale device it owns. No subprocess, which matters under
/// launchd and systemd, where PATH is minimal and the `tailscale` CLI is
/// usually not on it — on macOS it isn't even installed by default, it
/// lives inside Tailscale.app.
function tailscaleIfaceIp(): string | null {
  for (const addrs of Object.values(os.networkInterfaces())) {
    for (const a of addrs ?? []) {
      if (a.family !== "IPv4" || a.internal) continue;
      const [x, y] = a.address.split(".").map(Number);
      if (x === 100 && y >= 64 && y <= 127) return a.address;
    }
  }
  return null;
}

/// Fallback for an unusual setup (a tailnet on a non-CGNAT range): ask the
/// CLI, wherever it happens to live.
function tailscaleCliIp(): string | null {
  const candidates = [
    "tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
  ];
  for (const bin of candidates) {
    try {
      const out = execFileSync(bin, ["ip", "-4"], { encoding: "utf8", timeout: 3000 });
      const ip = out.split("\n")[0].trim();
      if (/^\d+\.\d+\.\d+\.\d+$/.test(ip)) return ip;
    } catch {
      /* not here, or not up yet — try the next one */
    }
  }
  return null;
}

function tailscaleIp(): string | null {
  return tailscaleIfaceIp() ?? tailscaleCliIp();
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
      // Stale delivery guard: a status push undelivered after 15 min is
      // describing a pane that's probably moved on — drop it rather than
      // let APNs hand it over hours later. Collapse per pane so a newer
      // push ("finished") replaces an older one ("needs you").
      ttl: 15 * 60,
      collapseId: `pane-${pane.id}`,
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
    answered through /api/answer or the WS `answer` action, or closes.
    PTY panes only — agent panes never go through the screen scraper (their
    permission asks arrive structured, see `perms` below). */
const questions = new Map<number, PaneQuestion>();

// ------------------------------------------------------- agent permissions

/** Pending tool-permission requests per agent pane (T_PERM_REQ, ADR 0002),
    keyed pane id → request_id, insertion-ordered (oldest first). The zodiac
    server's inbox is authoritative — this cache only exists so a phone that
    connects mid-request still gets the card (hello carries it) and so the
    push fires once per request. Entries clear on zodiac_perm_resolved (the
    event stream carries resolution, wherever it was answered), pane close,
    or link drop — the server re-sends still-pending ones on reconnect. */
const perms = new Map<number, Map<string, PermRequest>>();

function pendingPerms(): { pane: number; req: PermRequest }[] {
  const out: { pane: number; req: PermRequest }[] = [];
  for (const [pane, m] of perms) for (const req of m.values()) out.push({ pane, req });
  return out;
}

function dropPerm(pane: number, requestId: string): boolean {
  const m = perms.get(pane);
  if (!m?.delete(requestId)) return false;
  if (m.size === 0) perms.delete(pane);
  return true;
}

function paneOf(id: number): PaneState | undefined {
  return link.state?.panes.find((p) => p.id === id);
}

/** One line of a tool input for a push body: `{"file_path":"/x","content":…}`
    squeezed to something a lock screen can show. */
function compactInput(input: unknown): string | undefined {
  try {
    const s = JSON.stringify(input);
    if (!s || s === "{}" || s === "null") return undefined;
    return s.length > 200 ? `${s.slice(0, 199)}…` : s;
  } catch {
    return undefined;
  }
}

/** Answer a pending permission request. The zodiac server settles it and
    broadcasts a zodiac_perm_resolved transcript line (which clears every
    client's card, including other bridges'); dropping the cache entry and
    broadcasting here too just makes the answering phone snappy while that
    echo is in flight. An allow with a note sends the note on as a normal
    agent prompt; on deny it rides inside the response as the message the
    agent sees. */
function resolvePerm(paneId: number, requestId: string, behavior: "allow" | "deny", note: string) {
  link.permResponse(paneId, {
    request_id: requestId,
    behavior,
    ...(behavior === "deny" && note ? { message: note } : {}),
  });
  dropPerm(paneId, requestId);
  broadcast({ t: "perm_resolved", pane: paneId, request_id: requestId, how: behavior });
  if (behavior === "allow" && note) link.agentInput(paneId, note);
}

link.on("perm_req", (pane: number, req: PermRequest) => {
  const m = perms.get(pane) ?? new Map<string, PermRequest>();
  const fresh = !m.has(req.request_id);
  m.set(req.request_id, req);
  perms.set(pane, m);
  broadcast({ t: "perm_req", pane, req });
  // Push once per request: an attach/reconnect replay re-sends still-pending
  // requests with their real age, so anything older than the cooldown either
  // already fired or predates this bridge process — don't re-alert.
  if (fresh && req.age_ms < PUSH_COOLDOWN_MS) {
    const p = paneOf(pane);
    const name = p?.name ?? `pane ${pane}`;
    const label = req.display_name || req.tool_name;
    const badge = link.state?.panes.filter((x) => x.status === "needs_input").length ?? 0;
    maybePush(
      "perm",
      { id: pane, name },
      {
        title: `${name} asks: ${label}`,
        body: PUSH_REDACT ? "wants to use a tool" : compactInput(req.input),
      },
      badge,
      // Rides as question/options so the iOS shell's numbered notification
      // actions (AGENT_PROMPT_2) work — they land on /api/answer, which
      // maps 1/2 to allow/deny for agent panes below.
      PUSH_REDACT ? undefined : { question: `allow ${label}?`, options: ["Allow", "Deny"] },
    );
  }
});

link.on("agent_event", (pane: number, lines: string[]) => {
  // Permission lifecycle rides the event stream as synthetic
  // zodiac_perm_resolved lines — track those even when nobody is viewing,
  // so the inbox (and every phone's cards) stays honest no matter where a
  // request was answered (desktop TUI, another phone, server timeout).
  for (const line of lines) {
    if (!line.includes('"zodiac_perm_resolved"')) continue;
    try {
      const ev = JSON.parse(line);
      if (ev?.type === "zodiac_perm_resolved" && typeof ev.request_id === "string") {
        dropPerm(pane, ev.request_id);
        broadcast({
          t: "perm_resolved",
          pane,
          request_id: ev.request_id,
          how: typeof ev.how === "string" ? ev.how : "",
        });
      }
    } catch {
      /* a transcript line that merely mentions the marker — not ours */
    }
  }
  if (paneViewed(pane)) broadcast({ t: "agent_event", pane, lines }, pane);
});

/** needs_input just fired: grab the pane's rendered screen, parse the
    dialog out of it, then push with the real question + options and let
    web clients re-render with answer buttons. Falls back to the old
    subtitle/recap body when there's nothing parseable on screen. */
async function captureQuestion(p: { id: number; name: string; subtitle?: string | null; recap?: string | null; title?: string | null }, badge: number) {
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

/** Multi-line text as a pane's editor wants it. Claude Code reads `\` +
    Enter as "insert a newline", so a bare newline there would submit at the
    first line break. pi binds Ctrl+J — a plain \n byte — to the same job
    and would render claude's backslash literally. A pane with no agent is
    a shell, which wants exactly the bytes it was given. */
function wireNewlines(agent: string | null | undefined, text: string): string {
  if (!agent || agent === "pi") return text;
  return text.replaceAll("\n", "\\\r");
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

/** The agent-pane flavor of answerPane, for the same numbered entry points
    (iOS notification actions, /api/answer, the WS `answer` action): typed
    digits mean nothing to a structured pane, so 1/2 map to Allow/Deny on
    the pane's OLDEST pending permission request. Anything else — no pending
    request, option 3+ — is a no-op that reports false. */
function answerAgentPane(paneId: number, option: number, note: string): boolean {
  const first = perms.get(paneId)?.values().next().value;
  if (!first || (option !== 1 && option !== 2)) return false;
  resolvePerm(paneId, first.request_id, option === 1 ? "allow" : "deny", note);
  return true;
}

link.on("state", (state: SessionState) => {
  // Keep the persisted token when talking to an old server that predates
  // the field — nulling it here would reopen the unauthenticated window.
  if (state.pairing_token) rememberSessionToken(state.pairing_token);
  const badge = state.panes.filter((p) => p.status === "needs_input").length;
  const live = new Set(state.panes.map((p) => p.id));
  for (const p of state.panes) {
    const prev = prevStatus.get(p.id);
    if (prev === "needs_input" && p.status !== "needs_input") {
      questions.delete(p.id);
      // The cooldown exists to stop repeat pushes about ONE dialog; a pane
      // that was answered and asks something new deserves a fresh push
      // even inside the window.
      lastPush.delete(`${p.id}:needs_input`);
    }
    if (!prev || prev === p.status) continue;
    if (p.status === "needs_input") {
      if (p.kind === "agent") {
        // Structured pane: NEVER screen-scrape — permission asks arrive as
        // T_PERM_REQ (which already pushed, with real tool names). Anything
        // else that flips an agent pane to needs_input gets the plain
        // notification body.
        if (!perms.get(p.id)?.size) {
          maybePush("needs_input", p, {
            title: `${p.name} needs you`,
            body: PUSH_REDACT
              ? "needs your attention"
              : p.subtitle || p.recap || p.title || undefined,
          }, badge);
        }
      } else {
        captureQuestion(p, badge).catch(() => {});
      }
    } else if (p.status === "done" && prev === "working") {
      maybePush("done", p, {
        title: `${p.name} finished`,
        body: PUSH_REDACT ? "finished" : p.recap || p.subtitle || undefined,
      }, badge);
    }
  }
  prevStatus = new Map(state.panes.map((p) => [p.id, p.status]));
  // Prune bookkeeping for panes that no longer exist, so weeks of uptime
  // don't accumulate ghosts.
  for (const id of [...questions.keys()]) if (!live.has(id)) questions.delete(id);
  for (const key of [...lastPush.keys()]) {
    const id = Number(key.split(":")[0]);
    if (!live.has(id)) lastPush.delete(key);
  }
});

// A dropped link means the next state can be a different zodiac process
// whose pane ids collide with the old ones — stale transition records
// would fire pushes for flips that never happened, and stale questions
// would dress a new pane in another pane's dialog. Start clean.
link.on("link", (up: boolean) => {
  if (!up) {
    prevStatus = new Map();
    questions.clear();
    lastPush.clear();
    // The server re-sends still-pending permissions on the next watch hello;
    // anything not re-sent was settled (or belongs to a dead process).
    perms.clear();
  }
});

link.on("pane_closed", (pane: number) => {
  questions.delete(pane);
  perms.delete(pane);
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

/** A slow-but-alive phone (backgrounded browser, weak link) answers pings
    but stops draining its socket — ws buffers every send in memory with no
    limit, the one unbounded-memory path in the process. Past this cap the
    client is terminated; its reconnect gets a clean hello + replay. */
const MAX_WS_BUFFERED = 4 * 1024 * 1024;

function wsWritable(c: Client): boolean {
  if (c.ws.readyState !== WebSocket.OPEN) return false;
  if (c.ws.bufferedAmount > MAX_WS_BUFFERED) {
    c.ws.terminate();
    clients.delete(c);
    refreshViewed();
    return false;
  }
  return true;
}

function sendJson(c: Client, msg: unknown) {
  if (wsWritable(c)) c.ws.send(JSON.stringify(msg));
}

function broadcast(msg: unknown, pane?: number) {
  const json = JSON.stringify(msg);
  for (const c of [...clients]) {
    if (pane !== undefined && c.viewing !== pane) continue;
    if (wsWritable(c)) c.ws.send(json);
  }
}

/** Whether anyone is looking at this pane — checked BEFORE base64+JSON
    encoding a pty frame, so busy background panes cost nothing here. */
function paneViewed(pane: number): boolean {
  for (const c of clients) if (c.viewing === pane) return true;
  return false;
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
link.on("output", (pane: number, data: Buffer) => {
  if (!paneViewed(pane)) return;
  broadcast({ t: "output", pane, data: data.toString("base64") }, pane);
});
link.on("replay", (pane: number, data: Buffer) => {
  if (!paneViewed(pane)) return;
  broadcast({ t: "replay", pane, data: data.toString("base64") }, pane);
});
link.on("screen", (pane: number, text: string) => {
  if (!paneViewed(pane)) return;
  broadcast({ t: "screen", pane, text }, pane);
});

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
          // Seen on the phone counts as seen: a finished (green) pane the
          // user opened flips to idle instead of staying green until it's
          // focused on the desktop someday.
          if (link.up) link.seen(c.viewing);
          if (paneOf(c.viewing)?.kind === "agent") {
            // Agent panes have no pty ring: history comes from
            // /api/transcript, live lines follow as `agent_event`
            // broadcasts now that the pane counts as viewed.
          } else if (link.watchSupported) {
            sendJson(c, {
              t: "replay",
              pane: c.viewing,
              data: link.ringOf(c.viewing).toString("base64"),
            });
          } else if (link.up) {
            link.readScreen(c.viewing); // poll-mode: fetch a screen right away
          }
        }
        break;
      }
      case "rename": {
        if (
          paneExists(msg.pane) &&
          typeof msg.name === "string" &&
          msg.name.length <= 80
        ) {
          link.rename(msg.pane, msg.name);
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
          // Agent panes take the prompt structurally (T_AGENT_INPUT, text
          // verbatim — the web client skips its newline wiring for them);
          // pty panes keep typed keystrokes + Enter.
          if (paneOf(msg.pane)?.kind === "agent") link.agentInput(msg.pane, msg.text);
          else link.prompt(msg.pane, msg.text);
        }
        break;
      }
      case "answer": {
        if (
          paneExists(msg.pane) &&
          Number.isInteger(msg.option) && msg.option >= 1 && msg.option <= 9
        ) {
          const note = typeof msg.note === "string" ? msg.note.trim() : "";
          if (paneOf(msg.pane)?.kind === "agent") {
            answerAgentPane(msg.pane, msg.option, note);
          } else {
            answerPane(msg.pane, msg.option, note);
          }
        }
        break;
      }
      case "perm": {
        // Allow/Deny from a phone's permission card → T_PERM_RESP.
        if (
          paneExists(msg.pane) &&
          typeof msg.request_id === "string" && msg.request_id.length <= 256 &&
          (msg.behavior === "allow" || msg.behavior === "deny")
        ) {
          resolvePerm(
            msg.pane,
            msg.request_id,
            msg.behavior,
            typeof msg.note === "string" ? msg.note.trim() : "",
          );
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
  if (!haveToken()) {
    json(res, 503, { error: "not ready" });
    return true;
  }
  if (!tokenOk(supplied)) {
    json(res, 401, { error: "unauthorized" });
    return true;
  }
  // Vitals for the phone's title bar — a read, so a plain GET.
  if (url === "/api/host" && req.method === "GET") {
    json(res, 200, await hostStats());
    return true;
  }
  // Pane summary for the home-screen widget: a snapshot poll, not the
  // WebSocket — a widget timeline provider wakes, fetches once, sleeps.
  if (url === "/api/panes" && req.method === "GET") {
    const st = publicState(link.state);
    json(res, 200, {
      link: link.up,
      session: st?.session ?? SESSION,
      panes: (st?.panes ?? []).map((p) => ({
        // id is what /api/answer wants back; index is the human-facing number
        id: p.id,
        index: p.index,
        name: p.name,
        status: p.status,
        agent: p.agent ?? null,
        question: (p as any).question ?? null,
        // The widget's answer buttons submit a 1-based index into these;
        // without them it could only answer blind.
        options: (p as any).options ?? null,
      })),
    });
    return true;
  }
  // The pane's conversation from the agent's session transcript — read
  // mode's history. The pty ring can't provide this (agent TUIs repaint
  // in place, so replay reconstructs barely a screenful); the server
  // parses the transcript file into entries on request.
  if (url.startsWith("/api/transcript") && req.method === "GET") {
    const pane = Number(new URL(url, "http://x").searchParams.get("pane"));
    if (!Number.isInteger(pane) || pane < 0) {
      json(res, 400, { error: "need ?pane=<id>" });
      return true;
    }
    if (!link.up) {
      json(res, 503, { error: "zodiac link down" });
      return true;
    }
    const raw = await link.transcriptOnce(pane);
    if (raw === null) {
      // Link dropped mid-request, or the zodiac server predates
      // T_TRANSCRIPT_REQ (it silently ignores unknown frames).
      json(res, 503, { error: "no transcript answer from zodiac" });
      return true;
    }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(raw === "" ? "[]" : raw);
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
    // newlines are encoded the way the pane's agent expects.
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
      if (pane.kind === "agent") link.agentInput(pane.id, text);
      else link.prompt(pane.id, wireNewlines(pane.agent, text));
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
      const note = typeof body.note === "string" ? body.note.trim() : "";
      if (pane.kind === "agent") {
        if (!answerAgentPane(pane.id, body.option, note)) {
          json(res, 409, { error: "no pending permission request" });
          return true;
        }
      } else {
        answerPane(pane.id, body.option, note);
      }
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
    // Unauthenticated liveness probe — booleans only. The detailed fields
    // (session name, pane/device counts) are reconnaissance for the tailnet,
    // so they're gated behind the token.
    const detail = tokenOk(new URL(req.url || "/", "http://x").searchParams.get("t"));
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify({
        ok: true,
        link: link.up,
        watch: link.watchSupported,
        push: apns.enabled,
        ...(detail
          ? {
              session: SESSION,
              panes: link.state?.panes.length ?? 0,
              devices: apns.deviceCount,
            }
          : {}),
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
  // One stat, no exists/stat race — a file deleted between the two calls
  // (mid web rebuild) threw in an async handler and killed the process.
  let stat: fs.Stats | undefined;
  try {
    stat = fs.statSync(file);
  } catch {
    /* missing — SPA fallback below */
  }
  if (!stat || stat.isDirectory()) {
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
  // Compared against the bridge's own bound address, not the client's Host
  // header — the old `http://${req.headers.host}` check was satisfied by
  // any DNS-rebinding page (attacker controls both headers), i.e. no check
  // at all.
  const origin = req.headers.origin;
  if (origin && origin !== endpointURL) {
    ws.close(4003, "origin not allowed");
    return;
  }
  const supplied = new URL(req.url || "", "http://x").searchParams.get("t");
  if (!haveToken()) {
    // No pairing token learned yet (bridge just started / zodiac link down).
    // Transient: the client should keep retrying, not show a token error.
    ws.close(4002, "not ready");
    return;
  }
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
    // Pending agent-pane permission requests, so a phone that connects
    // mid-request still shows the card (live ones arrive as `perm_req`).
    perms: pendingPerms(),
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
setInterval(() => {
  try {
    publishEndpoint(endpointURL);
  } catch {
    /* full disk etc. — a bare throw in an interval kills the process */
  }
}, 60_000).unref();
link.start();
server.listen(PORT, host, () => {
  console.log(`astrolabe: serving http://${host}:${PORT} → zodiac session '${SESSION}'`);
});
