// Persistent machine identity for QR pairing. zodiac's own Alt+P overlay
// (src/kitty.rs's qr_rgba caller, in src/client.rs) reads the file this
// writes to compose a full pairing URL — the two processes never talk to
// each other directly for this, just the filesystem. See
// astrolabe/README.md's Security section for the pairing flow end to end.

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { stateDir } from "./apns.ts";

/** A random id, persisted once and reused across every bridge/zodiac
    restart — this is what lets the phone app tell "the same computer,
    rotated token" apart from "a new computer" when it re-scans a QR. */
function computerId(): string {
  const file = path.join(stateDir(), "computer_id");
  try {
    const id = fs.readFileSync(file, "utf8").trim();
    if (id) return id;
  } catch {
    // first run, or the file's gone — mint a fresh one below
  }
  const id = crypto.randomUUID();
  fs.mkdirSync(stateDir(), { recursive: true });
  fs.writeFileSync(file, id + "\n");
  return id;
}

export interface Identity {
  url: string;
  cid: string;
  name: string;
}

/** Writes `~/.local/state/astrolabe/endpoint.json` = `{url, cid, name}` and
    returns the same doc, so callers that also need `cid` (e.g. to tag push
    notifications with which computer they're from) don't have to read the
    file back or duplicate `computerId()`. No secret lives in this file —
    the pairing token itself only ever travels over the zodiac↔bridge unix
    socket and the QR image, never to disk. Safe to call on every startup:
    `url` is the only field that can actually change (a different
    `ASTROLABE_HOST`/tailscale IP), `cid` is stable once minted, `name` just
    tracks the current hostname. */
export function publishEndpoint(url: string): Identity {
  const dir = stateDir();
  fs.mkdirSync(dir, { recursive: true });
  const doc: Identity = { url, cid: computerId(), name: os.hostname() };
  fs.writeFileSync(path.join(dir, "endpoint.json"), JSON.stringify(doc));
  return doc;
}
