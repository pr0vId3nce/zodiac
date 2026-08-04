// APNs sender — token-based auth (ES256 JWT signed with the .p8 key from the
// developer portal), HTTP/2 straight to Apple. No dependencies.
//
// Env:
//   ASTROLABE_APNS_KEY      path to AuthKey_XXXXXXXXXX.p8
//   ASTROLABE_APNS_KEY_ID   10-char key id (from the portal's Keys page)
//   ASTROLABE_APNS_TEAM_ID  10-char team id
//   ASTROLABE_APNS_TOPIC    app bundle id (e.g. dev.d3s.Astrolabe)
//   ASTROLABE_APNS_ENV      "sandbox" (default — Xcode/dev-signed builds) or
//                           "production" (TestFlight / App Store)
//
// All four of KEY/KEY_ID/TEAM_ID/TOPIC must be set or push stays disabled;
// the registry endpoints still work so devices can enroll ahead of time.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as http2 from "node:http2";
import * as crypto from "node:crypto";

export interface PushAlert {
  title: string;
  body?: string;
}

export interface PushOpts {
  badge?: number;
  category?: string;
  threadId?: string;
  timeSensitive?: boolean;
  /** Custom top-level payload keys (e.g. { pane: 3 }). */
  extra?: Record<string, unknown>;
}

const JWT_MAX_AGE_MS = 45 * 60 * 1000; // Apple allows 20–60 min; refresh at 45

function stateDir(): string {
  const base =
    process.env.XDG_STATE_HOME || path.join(os.homedir(), ".local", "state");
  return path.join(base, "astrolabe");
}

export class Apns {
  readonly enabled: boolean;
  private keyId = "";
  private teamId = "";
  private topic = "";
  private hostname = "api.sandbox.push.apple.com";
  private key: crypto.KeyObject | null = null;

  private jwtCache = "";
  private jwtBorn = 0;
  private session: http2.ClientHttp2Session | null = null;

  private devicesFile = path.join(stateDir(), "devices.json");
  private devices: { token: string; added: string }[] = [];

  constructor() {
    const keyPath = process.env.ASTROLABE_APNS_KEY;
    this.keyId = process.env.ASTROLABE_APNS_KEY_ID || "";
    this.teamId = process.env.ASTROLABE_APNS_TEAM_ID || "";
    this.topic = process.env.ASTROLABE_APNS_TOPIC || "";
    if ((process.env.ASTROLABE_APNS_ENV || "sandbox") === "production") {
      this.hostname = "api.push.apple.com";
    }
    let key: crypto.KeyObject | null = null;
    if (keyPath) {
      try {
        key = crypto.createPrivateKey(fs.readFileSync(keyPath));
      } catch (e) {
        console.error(`astrolabe: cannot load APNs key ${keyPath}:`, e);
      }
    }
    this.key = key;
    this.enabled = !!(key && this.keyId && this.teamId && this.topic);
    this.loadDevices();
  }

  get deviceCount(): number {
    return this.devices.length;
  }

  register(token: string): boolean {
    if (!/^[0-9a-fA-F]{16,}$/.test(token)) return false;
    token = token.toLowerCase();
    if (!this.devices.some((d) => d.token === token)) {
      this.devices.push({ token, added: new Date().toISOString() });
      this.saveDevices();
    }
    return true;
  }

  unregister(token: string) {
    const before = this.devices.length;
    this.devices = this.devices.filter((d) => d.token !== token.toLowerCase());
    if (this.devices.length !== before) this.saveDevices();
  }

  /** Send to every registered device; prunes tokens Apple reports dead. */
  async push(alert: PushAlert, opts: PushOpts = {}): Promise<{ sent: number; failed: number }> {
    if (!this.enabled || this.devices.length === 0) return { sent: 0, failed: 0 };
    const aps: Record<string, unknown> = { alert, sound: "default" };
    if (opts.badge !== undefined) aps.badge = opts.badge;
    if (opts.category) aps.category = opts.category;
    if (opts.threadId) aps["thread-id"] = opts.threadId;
    if (opts.timeSensitive) aps["interruption-level"] = "time-sensitive";
    const body = JSON.stringify({ aps, ...opts.extra });

    let sent = 0;
    const dead: string[] = [];
    for (const d of [...this.devices]) {
      try {
        const status = await this.send(d.token, body);
        if (status === 200) sent++;
        else if (status === 410 || status === 400) dead.push(d.token);
      } catch (e) {
        console.error("astrolabe: APNs send failed:", e);
      }
    }
    for (const t of dead) this.unregister(t);
    if (dead.length) console.log(`astrolabe: pruned ${dead.length} dead APNs token(s)`);
    return { sent, failed: this.devices.length - sent };
  }

  // ------------------------------------------------------------- internals

  private send(token: string, body: string): Promise<number> {
    return new Promise((resolve, reject) => {
      const session = this.connect();
      const req = session.request({
        ":method": "POST",
        ":path": `/3/device/${token}`,
        authorization: `bearer ${this.jwt()}`,
        "apns-topic": this.topic,
        "apns-push-type": "alert",
        "apns-priority": "10",
      });
      const timer = setTimeout(() => {
        req.close(http2.constants.NGHTTP2_CANCEL);
        reject(new Error("APNs request timeout"));
      }, 10000);
      let status = 0;
      let resp = "";
      req.on("response", (h) => (status = Number(h[":status"] || 0)));
      req.on("data", (c: Buffer) => (resp += c.toString()));
      req.on("end", () => {
        clearTimeout(timer);
        if (status !== 200 && status !== 410) {
          console.error(`astrolabe: APNs ${status} ${resp}`);
        }
        // 400 only kills the token when Apple says the token itself is bad
        if (status === 400 && !resp.includes("BadDeviceToken")) status = 0;
        resolve(status);
      });
      req.on("error", (e) => {
        clearTimeout(timer);
        reject(e);
      });
      req.end(body);
    });
  }

  private connect(): http2.ClientHttp2Session {
    if (this.session && !this.session.closed && !this.session.destroyed) {
      return this.session;
    }
    const s = http2.connect(`https://${this.hostname}:443`);
    s.on("error", () => (this.session = null));
    s.on("close", () => (this.session = null));
    s.on("goaway", () => (this.session = null));
    this.session = s;
    return s;
  }

  private jwt(): string {
    const now = Date.now();
    if (this.jwtCache && now - this.jwtBorn < JWT_MAX_AGE_MS) return this.jwtCache;
    const header = Buffer.from(
      JSON.stringify({ alg: "ES256", kid: this.keyId })
    ).toString("base64url");
    const claims = Buffer.from(
      JSON.stringify({ iss: this.teamId, iat: Math.floor(now / 1000) })
    ).toString("base64url");
    const sig = crypto
      .sign("sha256", Buffer.from(`${header}.${claims}`), {
        key: this.key!,
        dsaEncoding: "ieee-p1363",
      })
      .toString("base64url");
    this.jwtCache = `${header}.${claims}.${sig}`;
    this.jwtBorn = now;
    return this.jwtCache;
  }

  private loadDevices() {
    try {
      const raw = JSON.parse(fs.readFileSync(this.devicesFile, "utf8"));
      if (Array.isArray(raw)) {
        this.devices = raw.filter((d) => typeof d?.token === "string");
      }
    } catch {
      /* first run */
    }
  }

  private saveDevices() {
    fs.mkdirSync(path.dirname(this.devicesFile), { recursive: true });
    fs.writeFileSync(this.devicesFile, JSON.stringify(this.devices, null, 2));
  }
}
