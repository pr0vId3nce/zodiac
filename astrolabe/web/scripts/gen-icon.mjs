// Generates public/icon-512.png and icon-192.png at build time — a tiny
// dependency-free rasterizer + PNG encoder (zlib is in node), so no binary
// assets need to live in the repo. Same motif as icon.svg: night sky,
// gold ring, eight-point star.
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

function lerp(a, b, t) {
  return a + (b - a) * t;
}

function pixel(x, y, S) {
  const c = S / 2;
  const dx = x - c;
  const dy = y - c;
  const r = Math.hypot(dx, dy);
  const t = y / S;
  // night-sky vertical gradient
  let [R, G, B] = [lerp(11, 34, t), lerp(16, 20, t), lerp(32, 64, t)];
  // faint outer ring
  const ring2 = Math.abs(r - S * 0.332);
  if (ring2 < S * 0.004) [R, G, B] = [140, 118, 52];
  // gold ring
  const ring = Math.abs(r - S * 0.293);
  if (ring < S * 0.011) [R, G, B] = [212, 175, 55];
  // eight-point star: fat diamond + square core
  const diamond = Math.abs(dx) + Math.abs(dy) < S * 0.215;
  const core = Math.max(Math.abs(dx), Math.abs(dy)) < S * 0.115;
  if (diamond || core) [R, G, B] = [245, 230, 184];
  // sprinkle of fixed stars
  const stars = [
    [0.23, 0.23, 0.012],
    [0.78, 0.29, 0.008],
    [0.74, 0.76, 0.01],
    [0.25, 0.74, 0.008],
  ];
  for (const [sx, sy, sr] of stars) {
    if (Math.hypot(x - sx * S, y - sy * S) < sr * S) [R, G, B] = [245, 230, 184];
  }
  return [R | 0, G | 0, B | 0, 255];
}

const CRC_TABLE = new Int32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c;
});

function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

function png(S) {
  const raw = Buffer.alloc(S * (S * 4 + 1));
  for (let y = 0; y < S; y++) {
    const row = y * (S * 4 + 1);
    raw[row] = 0; // filter: none
    for (let x = 0; x < S; x++) {
      const [R, G, B, A] = pixel(x, y, S);
      raw.set([R, G, B, A], row + 1 + x * 4);
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(S, 0);
  ihdr.writeUInt32BE(S, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

for (const size of [512, 192]) {
  writeFileSync(join(here, "..", "public", `icon-${size}.png`), png(size));
}
console.log("icons generated");
