//! Kitty graphics protocol support: the painted layer of the brass
//! instrument. Card fills, the orrery arc, sparklines, reticle brackets,
//! the scrollbar, mascot/orb/HAL portraits, and the pairing QR are all
//! generated procedurally as raw RGBA and transmitted with `f=32`.
//! Decorations place at `z=-1` so they sit *under* the text layer:
//! ratatui keeps drawing normally and the image shows through wherever
//! glyphs don't cover. Terminals without the protocol simply never get
//! these escapes — the text layer stands alone there.

use std::io::Write;

/// Cell size in pixels from the tty, if the terminal reports it.
pub fn cell_size() -> Option<(u16, u16)> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_xpixel > 0 && ws.ws_ypixel > 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row))
    } else {
        None
    }
}

/// Whether the outer terminal speaks the kitty graphics protocol. Detected
/// by identity rather than probing (a probe reply would race the input
/// parser): ghostty, kitty, and wezterm all support it.
pub fn enabled() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let known = term.contains("kitty")
        || term.contains("ghostty")
        || term.contains("wezterm")
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
        || std::env::var("TERM_PROGRAM").is_ok_and(|p| p == "WezTerm");
    known && cell_size().is_some()
}

fn hash(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^ (x >> 33)
}

/// Soft-edged filled dot, alpha-blended over the buffer.
fn stamp(px: &mut [u8], w: u32, h: u32, x: f32, y: f32, r: f32, col: (u8, u8, u8)) {
    let x0 = (x - r - 1.0).floor().max(0.0) as u32;
    let x1 = ((x + r + 1.0).ceil() as u32).min(w.saturating_sub(1));
    let y0 = (y - r - 1.0).floor().max(0.0) as u32;
    let y1 = ((y + r + 1.0).ceil() as u32).min(h.saturating_sub(1));
    for py in y0..=y1 {
        for pxx in x0..=x1 {
            let d = ((pxx as f32 - x).powi(2) + (py as f32 - y).powi(2)).sqrt();
            let a = (r - d + 0.7).clamp(0.0, 1.0);
            if a > 0.0 {
                let o = ((py * w + pxx) * 4) as usize;
                px[o] = (px[o] as f32 + (col.0 as f32 - px[o] as f32) * a) as u8;
                px[o + 1] = (px[o + 1] as f32 + (col.1 as f32 - px[o + 1] as f32) * a) as u8;
                px[o + 2] = (px[o + 2] as f32 + (col.2 as f32 - px[o + 2] as f32) * a) as u8;
                px[o + 3] = px[o + 3].max((a * 255.0) as u8);
            }
        }
    }
}

/// Thick line segment drawn as overlapping stamps.
fn seg(px: &mut [u8], w: u32, h: u32, a: (f32, f32), b: (f32, f32), th: f32, col: (u8, u8, u8)) {
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let steps = (len * 2.0).ceil().max(1.0) as u32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        stamp(
            px,
            w,
            h,
            a.0 + (b.0 - a.0) * t,
            a.1 + (b.1 - a.1) * t,
            th,
            col,
        );
    }
}

/// Image-id namespace for the flat "brass instrument" cards, clear of the
/// tarot-card range.
pub const FLAT_BASE: u32 = 0x464C_5400; // "FLT\0"

pub fn flat_card_id(accent_idx: usize, size_idx: usize, selected: bool, theme_idx: usize) -> u32 {
    FLAT_BASE
        + theme_idx as u32 * 256
        + size_idx as u32 * 32
        + accent_idx as u32 * 2
        + selected as u32
}

/// Everything that shapes one flat card's pixels. Colors arrive
/// pre-resolved (theme + status already applied) so the painter stays dumb.
pub struct FlatCard {
    /// Card body fill; a whisper of vertical falloff is added on top.
    pub fill: (u8, u8, u8),
    /// 1px edge line.
    pub edge: (u8, u8, u8),
    /// 2px status rail along the left edge.
    pub rail: (u8, u8, u8),
    /// Reticle corner brackets (the selection language) when selected.
    pub brackets: Option<(u8, u8, u8)>,
    /// Sigil roundel ring: center px, radius, color. The glyph itself is
    /// text — only the ring is painted, so the sigil stays font-crisp.
    #[allow(clippy::type_complexity)]
    pub roundel: Option<(f32, f32, f32, (u8, u8, u8))>,
}

/// Paint one flat brass-instrument card: solid night fill, hairline edge,
/// status rail, roundel ring, and reticle brackets when selected.
pub fn flat_card_rgba(w: u32, h: u32, s: &FlatCard) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        // Barely-there falloff toward the bottom so the fill doesn't read
        // as dead-flat plastic.
        let k = 1.0 - 0.10 * (y as f32 / h as f32);
        let (r, g, b) = (
            (s.fill.0 as f32 * k) as u8,
            (s.fill.1 as f32 * k) as u8,
            (s.fill.2 as f32 * k) as u8,
        );
        for x in 0..w {
            let o = ((y * w + x) * 4) as usize;
            let on_edge = x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let (cr, cg, cb) = if x < 2 {
                s.rail
            } else if on_edge {
                s.edge
            } else {
                (r, g, b)
            };
            px[o] = cr;
            px[o + 1] = cg;
            px[o + 2] = cb;
            px[o + 3] = 255;
        }
    }
    if let Some((cx, cy, rad, col)) = s.roundel {
        // Antialiased ring, ~1.5px stroke, with a faint interior wash.
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let ring = (0.75 - (d - rad).abs() + 0.7).clamp(0.0, 1.0);
                let wash = if d < rad { 0.08 } else { 0.0 };
                let a = ring.max(wash);
                if a > 0.01 {
                    let o = ((y * w + x) * 4) as usize;
                    px[o] = (px[o] as f32 + (col.0 as f32 - px[o] as f32) * a) as u8;
                    px[o + 1] = (px[o + 1] as f32 + (col.1 as f32 - px[o + 1] as f32) * a) as u8;
                    px[o + 2] = (px[o + 2] as f32 + (col.2 as f32 - px[o + 2] as f32) * a) as u8;
                }
            }
        }
    }
    if let Some(col) = s.brackets {
        let arm = (w.min(h) as f32 * 0.10).clamp(8.0, 13.0);
        let th = 1.1;
        let inset = 3.0;
        let (fw, fh) = (w as f32, h as f32);
        for (cx, cy, dx, dy) in [
            (inset, inset, 1.0, 1.0),
            (fw - 1.0 - inset, inset, -1.0, 1.0),
            (inset, fh - 1.0 - inset, 1.0, -1.0),
            (fw - 1.0 - inset, fh - 1.0 - inset, -1.0, -1.0),
        ] {
            seg(&mut px, w, h, (cx, cy), (cx + dx * arm, cy), th, col);
            seg(&mut px, w, h, (cx, cy), (cx, cy + dy * arm), th, col);
        }
    }
    px
}

/// Image-id namespace for the observatory's orrery arc. The id hashes the
/// dot layout so any status/geometry change lands on a fresh id; the two
/// low bits carry the needs-input pulse frame.
pub const ORRERY_BASE: u32 = 0x4F525200; // "ORR\0"

pub fn orrery_id(dots: &[(f32, f32, usize)], w: u32, h: u32, theme_idx: usize, frame: u8) -> u32 {
    let mut key = (w as u64) << 40 | (h as u64) << 20 | theme_idx as u64;
    for &(x, y, a) in dots {
        key = hash(key ^ ((x as u64) << 24 | (y as u64) << 8 | a as u64));
    }
    ORRERY_BASE + ((hash(key) as u32 & 0x3FFF) << 2) + (frame as u32 & 3)
}

/// Straight-alpha overlay write: color wins where its alpha beats what's
/// already there, alpha only ever grows. Right for sparse strokes on a
/// transparent ground (stamp() would double-dim through its lerp-from-black).
fn put_a(px: &mut [u8], w: u32, h: u32, x: i64, y: i64, col: (u8, u8, u8), a: f32) {
    if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h || a <= 0.003 {
        return;
    }
    let o = ((y as u32 * w + x as u32) * 4) as usize;
    let na = (a * 255.0).min(255.0) as u8;
    if na >= px[o + 3] {
        px[o] = col.0;
        px[o + 1] = col.1;
        px[o + 2] = col.2;
    }
    px[o + 3] = px[o + 3].max(na);
}

/// The observatory's orrery: a half-ellipse brass arc with an inner dashed
/// ring, one status-colored dot per pane along it. Dot positions arrive in
/// pixels (the text layer computes them too, for the numeral labels).
/// needs_input dots ride an alpha pulse driven by `pulse` in [0,1).
pub fn orrery_rgba(
    w: u32,
    h: u32,
    dots: &[(f32, f32, usize)],
    rails: &[(u8, u8, u8); 5],
    accent: (u8, u8, u8),
    pulse: f32,
) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let (cx, cy) = (fw / 2.0, fh * 0.92);
    let rx = (fw * 0.42).min(fh * 4.5);
    let ry = fh * 0.72;
    let steps = (rx * 3.2) as u32;
    for k in 0..=steps {
        let t = std::f32::consts::PI * (0.10 + 0.80 * k as f32 / steps as f32);
        let (x, y) = (cx - rx * t.cos(), cy - ry * t.sin());
        // Solid outer arc at .30, dashed inner ring at .12.
        for dy in -1..=1i64 {
            for dx in -1..=1i64 {
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                let a = (1.0 - d * 0.55).max(0.0);
                put_a(
                    &mut px,
                    w,
                    h,
                    x as i64 + dx,
                    y as i64 + dy,
                    accent,
                    a * 0.30,
                );
                if (k / 9) % 2 == 0 {
                    let (ix, iy) = (cx - rx * 0.84 * t.cos(), cy - ry * 0.80 * t.sin());
                    put_a(
                        &mut px,
                        w,
                        h,
                        ix as i64 + dx,
                        iy as i64 + dy,
                        accent,
                        a * 0.12,
                    );
                }
            }
        }
    }
    for &(x, y, acc) in dots {
        let col = rails[acc.min(4)];
        let needs = acc == 0;
        let r = if needs { 5.5 } else { 4.5 };
        let halo = if needs {
            r + 4.0 + 2.0 * (pulse * std::f32::consts::TAU).sin()
        } else {
            r
        };
        let span = halo.ceil() as i64 + 1;
        for dy in -span..=span {
            for dx in -span..=span {
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                let core = (r - d + 0.7).clamp(0.0, 1.0);
                let glow = if needs {
                    ((1.0 - (d - r).max(0.0) / (halo - r).max(1.0)).max(0.0)).powi(2)
                        * (0.22 + 0.18 * (pulse * std::f32::consts::TAU).sin().abs())
                } else {
                    0.0
                };
                put_a(
                    &mut px,
                    w,
                    h,
                    x as i64 + dx,
                    y as i64 + dy,
                    col,
                    core.max(glow),
                );
            }
        }
    }
    px
}

/// Image-id namespace for the attached view's chrome: the focused pane's
/// reticle brackets and the scrollback scrollbar.
pub const CHROME_BASE: u32 = 0x52544C00; // "RTL\0"

pub fn reticle_id(w: u32, h: u32, theme_idx: usize) -> u32 {
    CHROME_BASE + ((hash((w as u64) << 24 ^ (h as u64) << 4 ^ theme_idx as u64) as u32) & 0x3FF)
}

pub fn scrollbar_id(h: u32, theme_idx: usize, bucket: u8) -> u32 {
    CHROME_BASE
        + 0x400
        + (((hash((h as u64) << 8 ^ theme_idx as u64) as u32) & 0x3F) << 4)
        + (bucket as u32 & 0xF)
}

/// Four corner brackets on a transparent ground — the selection reticle at
/// pane scale.
pub fn reticle_rgba(w: u32, h: u32, col: (u8, u8, u8), alpha: f32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let arm = (w.min(h) as f32 * 0.08).clamp(10.0, 18.0) as i64;
    let (wi, hi) = (w as i64, h as i64);
    for (cx, cy, dx, dy) in [
        (1i64, 1i64, 1i64, 1i64),
        (wi - 2, 1, -1, 1),
        (1, hi - 2, 1, -1),
        (wi - 2, hi - 2, -1, -1),
    ] {
        for k in 0..arm {
            for t in 0..2i64 {
                put_a(&mut px, w, h, cx + dx * k, cy + dy * t, col, alpha);
                put_a(&mut px, w, h, cx + dx * t, cy + dy * k, col, alpha);
            }
        }
    }
    px
}

/// The scrollback scrollbar: a faint 2px track down the right edge of a
/// one-cell-wide strip, with the thumb at `frac` (0 = bottom, 1 = top).
pub fn scrollbar_rgba(w: u32, h: u32, col: (u8, u8, u8), frac: f32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let x0 = w.saturating_sub(2);
    for y in 0..h {
        for x in x0..w {
            put_a(&mut px, w, h, x as i64, y as i64, col, 0.12);
        }
    }
    let th = (h / 8).max(18).min(h);
    let center = (1.0 - frac.clamp(0.0, 1.0)) * h as f32;
    let top = (center - th as f32 / 2.0).clamp(0.0, (h - th) as f32) as u32;
    for y in top..(top + th).min(h) {
        for x in x0..w {
            put_a(&mut px, w, h, x as i64, y as i64, col, 0.55);
        }
    }
    px
}

/// Image-id namespace for the per-pane card sparklines. The version byte
/// rolls whenever the bucket history changes, so stale terminal-side
/// caches can never show old bars.
pub const SPARK_BASE: u32 = 0x53504B00; // "SPK\0"

pub fn spark_id(pane_id: u64, ver: u32) -> u32 {
    // 12 bits of pane id: two panes colliding needs 4096 pane ids in one
    // server lifetime (the old 8 bits made that a plausible 256).
    SPARK_BASE + ((pane_id as u32 & 0xFFF) << 8) + (ver & 0xFF)
}

/// A pane's output-rate sparkline: up to 12 bars (50s buckets, newest at
/// the right edge) in the pane's status color, quiet buckets as faint
/// 1px stubs. Transparent ground; height normalized to the loudest bucket.
pub fn spark_rgba(w: u32, h: u32, buckets: &[u32], col: (u8, u8, u8)) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    const SLOTS: u32 = 12;
    let max = buckets.iter().copied().max().unwrap_or(0).max(1) as f32;
    let slot_w = (w / SLOTS).max(1);
    let bar_w = slot_w.saturating_sub(slot_w / 4).max(1);
    let first_slot = SLOTS.saturating_sub(buckets.len() as u32);
    for (i, &b) in buckets.iter().enumerate() {
        let slot = first_slot + i as u32;
        // log-ish scale so a chatty compile doesn't flatten everything else
        let v = ((b as f32).ln_1p() / max.ln_1p()).clamp(0.0, 1.0);
        let bar_h = ((v * (h as f32 - 1.0)) as u32).max(1);
        let a = if b == 0 { 0.18 } else { 0.35 + 0.45 * v };
        let x0 = slot * slot_w;
        for y in h.saturating_sub(bar_h)..h {
            for x in x0..(x0 + bar_w).min(w) {
                let o = ((y * w + x) * 4) as usize;
                px[o] = col.0;
                px[o + 1] = col.1;
                px[o + 2] = col.2;
                px[o + 3] = (a * 255.0) as u8;
            }
        }
    }
    px
}

/// Image-id base for the orb cursor's animation frames ("WOR\0"-ish),
/// clear of the card range and the pane-image range.
pub const ORB_BASE: u32 = 0x574F_5200;
pub const ORB_FRAMES: u32 = 8;
/// Frame index whose phase sits at the pulse peak — the steady-cursor frame.
pub const ORB_STEADY: u32 = 2;

/// Which graphics-rendered cursor to paint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OrbShape {
    Orb,
    Circle,
    /// A thicker bar than the hardware one terminals draw for DECSCUSR 5.
    Bar,
    /// Halo only — the breathing aura behind the aleph glyph, placed under
    /// the text layer so the letter itself stays font-crisp.
    Halo,
}

/// Paint a graphics cursor into a cell-sized straight-alpha RGBA buffer.
/// The body is translucent so the glyph underneath stays readable;
/// `phase` in [0,1) drives the glow pulse.
pub fn orb_rgba(w: u32, h: u32, col: (u8, u8, u8), shape: OrbShape, phase: f32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let (cx, cy) = (fw / 2.0, fh / 2.0);
    let r = (fw.min(fh)) / 2.0 - 0.6;
    let glow = 0.5 + 0.5 * (phase * std::f32::consts::TAU).sin();
    let (cr, cg, cb) = (col.0 as f32, col.1 as f32, col.2 as f32);
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let o = ((y * w + x) * 4) as usize;
            let (mut rr, mut gg, mut bb, mut a);
            if shape == OrbShape::Halo {
                // Soft radial aura behind the glyph, breathing with the
                // pulse. The buffer spans 3x3 cells; the falloff is a
                // quadratic window that reaches exactly zero at the buffer
                // edge — a gaussian would still be visible there and cut
                // off as a hard rectangle.
                let rmax = fw.min(fh) / 2.0 - 0.5;
                let fall = (1.0 - d / rmax).clamp(0.0, 1.0);
                let aura = fall * fall;
                let inner = (-(d / (rmax * 0.22)).powi(2)).exp();
                rr = cr + (255.0 - cr) * inner * 0.15;
                gg = cg + (255.0 - cg) * inner * 0.15;
                bb = cb + (255.0 - cb) * inner * 0.15;
                a = (aura * (0.16 + 0.20 * glow) + inner * 0.10).min(0.55);
            } else if shape == OrbShape::Bar {
                // Left-aligned like a hardware bar, just meatier: ~28% of
                // the cell wide, antialiased right edge, faint rounding at
                // the ends, alpha breathing with the pulse.
                let th = (fw * 0.28).max(2.6);
                let edge = (th - (x as f32 + 0.5)).clamp(0.0, 1.0);
                let fy = y as f32 + 0.5;
                let cap = fy.min(fh - fy).clamp(0.0, 1.0);
                rr = cr;
                gg = cg;
                bb = cb;
                a = edge * cap * (0.70 + 0.30 * glow);
            } else if shape == OrbShape::Orb {
                if d <= r {
                    let nd = d / r;
                    // Glassy body: sparse at the core, denser at the rim,
                    // with a breathing inner glow — the palantir's fire.
                    let core = (1.0 - nd).powi(2) * (0.30 + 0.70 * glow);
                    let rim = (-((d - r * 0.90) / (r * 0.18)).powi(2)).exp();
                    a = (0.18 + 0.40 * nd * nd + 0.35 * core + 0.25 * rim).min(0.92);
                    let lit = (core + rim * 0.8).min(1.0);
                    rr = 18.0 + (cr - 18.0) * lit;
                    gg = 14.0 + (cg - 14.0) * lit;
                    bb = 30.0 + (cb - 30.0) * lit;
                    // Specular glint, upper left — glass catching the light.
                    let sx = dx + r * 0.35;
                    let sy = dy + r * 0.40;
                    let spec = (-((sx * sx + sy * sy) / (r * r * 0.06))).exp() * 0.85;
                    rr += (255.0 - rr) * spec;
                    gg += (255.0 - gg) * spec;
                    bb += (255.0 - bb) * spec;
                    a = (a + spec * 0.4).min(0.95);
                } else {
                    // Soft halo bleeding past the sphere, pulsing.
                    let halo = glow * (-((d - r) / (r * 0.55)).powi(2)).exp() * 0.35;
                    rr = cr;
                    gg = cg;
                    bb = cb;
                    a = halo;
                }
            } else {
                // Plain circle: an antialiased ring, gently pulsing.
                let th = (fw * 0.13).max(1.1);
                let ring = (th * 0.5 - (d - r).abs() + 0.7).clamp(0.0, 1.0);
                rr = cr;
                gg = cg;
                bb = cb;
                a = ring * (0.60 + 0.40 * glow);
            }
            px[o] = rr.min(255.0) as u8;
            px[o + 1] = gg.min(255.0) as u8;
            px[o + 2] = bb.min(255.0) as u8;
            px[o + 3] = (a * 255.0).min(255.0) as u8;
        }
    }
    px
}

/// Draw the hopping Clawd mascot into an RGBA buffer. Shared between the
/// painted cards (opaque night sky) and the standalone transparent sprite
/// used by the blocks home view — every write also raises alpha so it
/// composites cleanly over the terminal background.
#[allow(clippy::too_many_arguments)]
fn draw_clawd(
    px: &mut [u8],
    w: u32,
    h: u32,
    mx: f32,
    my: f32,
    s: f32,
    fh: f32,
    frame: u8,
    soft: bool,
) {
    // Clawd mid-bounce: coral rounded blob, dark eyes, ground
    // shadow. phase 0 = top of the arc, 1 = squashed on the ground.
    let coral = (222.0f32, 122.0f32, 88.0f32);
    let eye = (46u8, 26u8, 22u8);
    let phase = [0.15f32, 0.55, 1.0, 0.55][frame as usize % 4];
    let amp = s * 1.1;
    let (bw0, bh0) = (s * 1.5, s * 1.25);
    let (bw2, bh2) = (bw0 * (1.0 + 0.22 * phase), bh0 * (1.10 - 0.32 * phase));
    let base_y = my + amp;
    let cy = base_y - bh2 - amp * (1.0 - phase);
    // Ground shadow first, body over it.
    let (srx, sry) = (bw0 * (0.55 + 0.45 * phase), (fh * 0.006).max(1.5));
    let sa = 0.20 + 0.25 * phase;
    for y in (base_y - sry * 3.0).max(0.0) as u32..((base_y + sry * 3.0) as u32).min(h) {
        for x in (mx - srx - 2.0).max(0.0) as u32..((mx + srx + 2.0) as u32).min(w) {
            let ex = (x as f32 - mx) / srx;
            let ey = (y as f32 - (base_y + sry)) / sry;
            let a = ((1.0 - (ex * ex + ey * ey)).max(0.0)).sqrt() * sa;
            if a > 0.01 {
                let o = ((y * w + x) * 4) as usize;
                px[o] = (px[o] as f32 * (1.0 - a)) as u8;
                px[o + 1] = (px[o + 1] as f32 * (1.0 - a)) as u8;
                px[o + 2] = (px[o + 2] as f32 * (1.0 - a)) as u8;
                px[o + 3] = px[o + 3].max((a * 255.0) as u8);
            }
        }
    }
    // Legs before the body so the joints hide under it; arms after
    // so they overlap the body edge. Legs tuck as he lands, arms
    // fly up while airborne.
    let coral8 = (222u8, 122u8, 88u8);
    let limb_th = (s * 0.16).max(1.0);
    let leg_l = s * 0.55 * (1.0 - 0.6 * phase);
    for side in [-1.0f32, 1.0] {
        let x0 = mx + side * bw2 * 0.45;
        let y0 = cy + bh2 * 0.85;
        seg(
            px,
            w,
            h,
            (x0, y0),
            (x0, y0 + leg_l + bh2 * 0.15),
            limb_th,
            coral8,
        );
    }
    let rad = bh2 * if soft { 0.55 } else { 0.20 };
    for y in (cy - bh2 - 2.0).max(0.0) as u32..((cy + bh2 + 2.0) as u32).min(h) {
        for x in (mx - bw2 - 2.0).max(0.0) as u32..((mx + bw2 + 2.0) as u32).min(w) {
            let qx = (x as f32 - mx).abs() - (bw2 - rad);
            let qy = (y as f32 - cy).abs() - (bh2 - rad);
            let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - rad;
            let a = (-d + 0.7).clamp(0.0, 1.0);
            if a > 0.01 {
                let o = ((y * w + x) * 4) as usize;
                px[o] = (px[o] as f32 + (coral.0 - px[o] as f32) * a) as u8;
                px[o + 1] = (px[o + 1] as f32 + (coral.1 - px[o + 1] as f32) * a) as u8;
                px[o + 2] = (px[o + 2] as f32 + (coral.2 - px[o + 2] as f32) * a) as u8;
                px[o + 3] = px[o + 3].max((a * 255.0) as u8);
            }
        }
    }
    // Arms: stubby segments angled up while airborne, level on
    // the squash.
    let arm_l = s * 0.7;
    let lift = 1.0 - phase;
    for side in [-1.0f32, 1.0] {
        let x0 = mx + side * bw2 * 0.92;
        let y0 = cy - bh2 * 0.05;
        let x1 = x0 + side * arm_l * 0.9;
        let y1 = y0 - arm_l * (0.85 * lift - 0.15);
        seg(px, w, h, (x0, y0), (x1, y1), limb_th, coral8);
    }
    // Eyes: two vertical ovals (stacked stamps), squashing with the
    // body so the bounce reads.
    let er = (bh2 * 0.16).max(1.0);
    for side in [-1.0f32, 1.0] {
        let ex = mx + side * bw2 * 0.40;
        let ey0 = cy - bh2 * 0.12;
        let stretch = bh2 * 0.14 * (1.0 - 0.5 * phase);
        stamp(px, w, h, ex, ey0 - stretch, er, eye);
        stamp(px, w, h, ex, ey0, er, eye);
        stamp(px, w, h, ex, ey0 + stretch, er, eye);
    }
}

/// Image ids for the standalone mascot sprite (4 bounce frames x 2 body
/// styles).
pub const CLAWD_BASE: u32 = 0x434C_5700;

pub fn clawd_id(frame: u8, soft: bool) -> u32 {
    CLAWD_BASE + if soft { 16 } else { 0 } + (frame as u32 % 4)
}

/// The hopping mascot alone on a transparent background, sized to fill the
/// given pixel box (the blocks view places it beside active claude panes).
pub fn clawd_rgba(w: u32, h: u32, frame: u8, soft: bool) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let s = (fh / 4.2).min(fw / 5.0);
    draw_clawd(&mut px, w, h, fw / 2.0, fh - 1.6 * s, s, fh, frame, soft);
    px
}

/// Draw the bouncing π mascot — pi's counterpart to Clawd, on the same
/// four-frame arc so a home view holding both agents bounces in step. The
/// glyph is struck rather than blobbed: a top bar and two splayed legs,
/// squashing on the landing frame the way the coral body does.
#[allow(clippy::too_many_arguments)]
fn draw_pi_mascot(px: &mut [u8], w: u32, h: u32, mx: f32, my: f32, s: f32, fh: f32, frame: u8) {
    let teal = (94u8, 205u8, 196u8);
    let phase = [0.15f32, 0.55, 1.0, 0.55][frame as usize % 4];
    let amp = s * 1.1;
    let (bw0, bh0) = (s * 1.35, s * 1.15);
    let (bw2, bh2) = (bw0 * (1.0 + 0.20 * phase), bh0 * (1.10 - 0.30 * phase));
    let base_y = my + amp;
    let cy = base_y - bh2 - amp * (1.0 - phase);
    // Ground shadow first, glyph over it — same ellipse as the other mascot.
    let (srx, sry) = (bw0 * (0.55 + 0.45 * phase), (fh * 0.006).max(1.5));
    let sa = 0.20 + 0.25 * phase;
    for y in (base_y - sry * 3.0).max(0.0) as u32..((base_y + sry * 3.0) as u32).min(h) {
        for x in (mx - srx - 2.0).max(0.0) as u32..((mx + srx + 2.0) as u32).min(w) {
            let ex = (x as f32 - mx) / srx;
            let ey = (y as f32 - (base_y + sry)) / sry;
            let a = ((1.0 - (ex * ex + ey * ey)).max(0.0)).sqrt() * sa;
            if a > 0.01 {
                let o = ((y * w + x) * 4) as usize;
                px[o] = (px[o] as f32 * (1.0 - a)) as u8;
                px[o + 1] = (px[o + 1] as f32 * (1.0 - a)) as u8;
                px[o + 2] = (px[o + 2] as f32 * (1.0 - a)) as u8;
                px[o + 3] = px[o + 3].max((a * 255.0) as u8);
            }
        }
    }
    let th = (s * 0.22).max(1.5);
    let top = cy - bh2 * 0.70;
    let bottom = cy + bh2 * 0.95;
    // The bar, overhanging both legs, with a small downward flick on the
    // left end — what makes the shape read as π and not as Н.
    seg(px, w, h, (mx - bw2, top), (mx + bw2, top), th, teal);
    seg(
        px,
        w,
        h,
        (mx - bw2, top),
        (mx - bw2, top + bh2 * 0.28),
        th * 0.8,
        teal,
    );
    // Legs: the left one near-upright, the right one curling outward, both
    // splaying a little further as the landing squashes the glyph.
    let splay = 0.10 + 0.10 * phase;
    seg(
        px,
        w,
        h,
        (mx - bw2 * 0.45, top),
        (mx - bw2 * (0.45 + splay), bottom),
        th,
        teal,
    );
    seg(
        px,
        w,
        h,
        (mx + bw2 * 0.42, top),
        (mx + bw2 * (0.42 + splay), bottom),
        th,
        teal,
    );
}

/// Image ids for the standalone π sprite (4 bounce frames). No soft/hard
/// split: the `claude style` setting reshapes Clawd's body, and a struck
/// glyph has no body to round off.
pub const PI_MASCOT_BASE: u32 = 0x5049_4D00; // "PIM\0"

pub fn pi_mascot_id(frame: u8) -> u32 {
    PI_MASCOT_BASE + (frame as u32 % 4)
}

/// The bouncing π alone on a transparent background, sized to fill the
/// given pixel box — the blocks view's counterpart to `clawd_rgba`.
pub fn pi_mascot_rgba(w: u32, h: u32, frame: u8) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let s = (fh / 4.2).min(fw / 5.0);
    draw_pi_mascot(&mut px, w, h, fw / 2.0, fh - 1.6 * s, s, fh, frame);
    px
}

/// Image ids for the HAL 9000 chat portrait (status x blink frame).
/// Image-id base for the chat panel's portrait, clear of the card, orb and
/// pane-image ranges.
pub const CHAT_BASE: u32 = 0x5749_5A00;

pub fn hal_id(status_idx: usize, frame: u32) -> u32 {
    CHAT_BASE + 0x80 + status_idx as u32 * 8 + frame
}

/// HAL 9000: a red camera eye in a metal bezel on a dark faceplate.
/// `open` is eyelid openness (1 = wide, 0 = shut) for the blink frames;
/// status dims the lamp when the model is sleeping or away.
pub fn hal_rgba(w: u32, h: u32, status_idx: usize, open: f32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let (cx, cy) = (fw / 2.0, fh / 2.0);
    let r = fw.min(fh) * 0.34;
    let lvl: f32 = match status_idx {
        0 => 1.0,
        1 => 0.75,
        2 => 0.45,
        _ => 0.25,
    };
    let bez_in = r * 1.04;
    let bez_out = r * 1.18;
    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f32, y as f32);
            let (dx, dy) = (xf - cx, yf - cy);
            let d = (dx * dx + dy * dy).sqrt();
            // Brushed faceplate, a touch lighter up top.
            let base = 16.0 + 12.0 * (1.0 - yf / fh);
            let (mut rr, mut gg, mut bb) = (base, base, base + 2.0);
            if d <= bez_out && d >= bez_in {
                // Metallic bezel with a top-left shine.
                let shine = ((-dx - dy) / d.max(1.0)).max(0.0);
                let m = 70.0 + 110.0 * shine;
                rr = m;
                gg = m;
                bb = m + 6.0;
            } else if d < bez_in {
                // Eyelid: a dark shutter closing in from above and below.
                let lid = 1.0 - ((dy.abs() - open * bez_in) / (r * 0.10)).clamp(0.0, 1.0) * 0.95;
                let glow = (-(d / (r * 0.62)).powi(2) * 1.8).exp();
                let core = (-(d / (r * 0.15)).powi(2)).exp();
                rr = 10.0 + (225.0 * glow * lvl + 45.0 * core) * lid;
                gg = 5.0 + (26.0 * glow * lvl + 205.0 * core * lvl) * lid;
                bb = 7.0 + (16.0 * glow * lvl + 110.0 * core * lvl) * lid;
                // Specular glint on the upper-left of the lens.
                let gd = ((dx + r * 0.38).powi(2) + (dy + r * 0.42).powi(2)).sqrt();
                let glint = (-(gd / (r * 0.10)).powi(2)).exp() * 0.85 * lid;
                rr += 200.0 * glint;
                gg += 200.0 * glint;
                bb += 200.0 * glint;
            }
            let o = ((y * w + x) * 4) as usize;
            px[o] = rr.min(255.0) as u8;
            px[o + 1] = gg.min(255.0) as u8;
            px[o + 2] = bb.min(255.0) as u8;
            px[o + 3] = 255;
        }
    }
    px
}

/// Transmit image data (a=t): chunked base64 of raw RGBA. `q=2` suppresses
/// terminal responses, which would otherwise land in the input stream.
pub fn transmit(out: &mut impl Write, id: u32, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let b64 = crate::client::b64(rgba);
    let mut rest = b64.as_str();
    let mut first = true;
    while !rest.is_empty() {
        let take = rest.len().min(4096);
        let (chunk, tail) = rest.split_at(take);
        rest = tail;
        let more = if rest.is_empty() { 0 } else { 1 };
        if first {
            write!(
                out,
                "\x1b_Ga=t,t=d,f=32,i={id},s={w},v={h},q=2,m={more};{chunk}\x1b\\"
            )?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};{chunk}\x1b\\")?;
        }
    }
    Ok(())
}

/// Place image `id` as placement `pid` at the current cursor cell, scaled
/// to cols×rows cells, under the text layer. `C=1` leaves the cursor put.
/// Re-placing the same (id, pid) replaces that placement atomically.
pub fn place(out: &mut impl Write, id: u32, pid: u32, cols: u16, rows: u16) -> std::io::Result<()> {
    write!(
        out,
        "\x1b_Ga=p,i={id},p={pid},c={cols},r={rows},z=-1,C=1,q=2\x1b\\"
    )
}

/// Delete one placement of one image (data stays cached).
pub fn delete_placement(out: &mut impl Write, id: u32, pid: u32) -> std::io::Result<()> {
    write!(out, "\x1b_Ga=d,d=i,i={id},p={pid},q=2\x1b\\")
}

/// Delete an image's placements *and* free its stored data.
pub fn delete_image(out: &mut impl Write, id: u32) -> std::io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// Relay a pane image's stored payload to the outer terminal verbatim:
/// raw RGB/RGBA (optionally zlib) with dimensions, or PNG (f=100) whose
/// dimensions the terminal reads itself.
pub fn transmit_data(
    out: &mut impl Write,
    id: u32,
    format: u8,
    zlib: bool,
    w: u32,
    h: u32,
    data: &[u8],
) -> std::io::Result<()> {
    let b64 = crate::client::b64(data);
    let mut rest = b64.as_str();
    let mut first = true;
    let dims = if format == 100 {
        String::new()
    } else {
        format!(",s={w},v={h}")
    };
    let comp = if zlib { ",o=z" } else { "" };
    loop {
        let take = rest.len().min(4096);
        let (chunk, tail) = rest.split_at(take);
        rest = tail;
        let more = if rest.is_empty() { 0 } else { 1 };
        if first {
            write!(
                out,
                "\x1b_Ga=t,t=d,f={format},i={id}{dims}{comp},q=2,m={more};{chunk}\x1b\\"
            )?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};{chunk}\x1b\\")?;
        }
        if rest.is_empty() {
            break;
        }
    }
    Ok(())
}

/// Place image `id` as placement `pid` at 1-based screen cell (row, col)
/// with an explicit source rectangle and cell span. `z`/`X`/`Y` pass the
/// inner app's layering and pixel offsets through; `C=1` leaves the outer
/// cursor alone (the compositor saves/restores around its writes anyway).
#[allow(clippy::too_many_arguments)]
pub fn place_at(
    out: &mut impl Write,
    row: u16,
    col: u16,
    id: u32,
    pid: u32,
    src: (u32, u32, u32, u32),
    cols: u16,
    rows: u16,
    z: i32,
    offx: u16,
    offy: u16,
) -> std::io::Result<()> {
    write!(out, "\x1b[{row};{col}H\x1b_Ga=p,i={id},p={pid}")?;
    let (x, y, w, h) = src;
    if x > 0 {
        write!(out, ",x={x}")?;
    }
    if y > 0 {
        write!(out, ",y={y}")?;
    }
    if w > 0 {
        write!(out, ",w={w}")?;
    }
    if h > 0 {
        write!(out, ",h={h}")?;
    }
    if offx > 0 {
        write!(out, ",X={offx}")?;
    }
    if offy > 0 {
        write!(out, ",Y={offy}")?;
    }
    write!(out, ",c={cols},r={rows},z={z},C=1,q=2\x1b\\")
}

/// Delete every placement (image data stays cached terminal-side).
pub fn delete_placements(out: &mut impl Write) -> std::io::Result<()> {
    write!(out, "\x1b_Ga=d,d=a,q=2\x1b\\")
}

/// Image id namespace for the pairing QR overlay (Alt+P). Only one QR is
/// ever shown at a time, but the id still varies with the encoded string so
/// kitty_overlay's placement diff naturally retransmits + swaps when the
/// token (or bridge endpoint) changes while the overlay happens to be open.
const QR_BASE: u32 = 0x5152_4331; // "QRC1"

pub fn qr_id(data: &str) -> u32 {
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for b in data.bytes() {
        seed ^= b as u64;
        seed = seed.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Only the low 16 bits vary: XORing the full hash escaped the QR
    // namespace entirely and could land on (and clobber) a card, chrome,
    // or pane image id.
    (QR_BASE & 0xFFFF_0000) | (hash(seed) as u32 & 0xFFFF)
}

/// Rasterizes `data` as a QR code: hard-edged black modules on white (no
/// anti-aliasing — a scanner wants crisp edges, not soft ones), with the
/// spec's minimum 4-module quiet zone. `target_px` is a hint for the
/// overall side length; the actual returned size is the closest multiple of
/// a whole-pixel module size, always square. `None` if `data` doesn't fit
/// in a QR code at all (never expected for a short pairing URL, but the
/// overlay falls back to a text message rather than unwrapping a panic).
pub fn qr_rgba(data: &str, target_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let n = code.width() as u32;
    let colors = code.to_colors();
    let quiet = 4u32;
    let module_px = (target_px / (n + quiet * 2)).max(1);
    let side = (n + quiet * 2) * module_px;
    let mut px = vec![255u8; (side * side * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            if colors[(y * n + x) as usize] == qrcode::Color::Dark {
                let px0 = (x + quiet) * module_px;
                let py0 = (y + quiet) * module_px;
                for dy in 0..module_px {
                    for dx in 0..module_px {
                        let o = (((py0 + dy) * side + (px0 + dx)) * 4) as usize;
                        px[o] = 0;
                        px[o + 1] = 0;
                        px[o + 2] = 0;
                    }
                }
            }
        }
    }
    Some((px, side, side))
}

#[cfg(test)]
mod qr_tests {
    use super::*;

    #[test]
    fn qr_rgba_encodes_a_pairing_url_square_and_opaque() {
        let url = "http://192.0.2.1:7979/?t=deadbeefdeadbeef&cid=abc123&name=examplehost";
        let (rgba, w, h) = qr_rgba(url, 400).expect("short URL always fits a QR code");
        assert_eq!(w, h, "qr_rgba must return a square image");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // Every pixel is fully opaque and either black or white — no
        // half-transmitted alpha that'd make a real terminal show a hole.
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 255
            && ((p[0], p[1], p[2]) == (0, 0, 0) || (p[0], p[1], p[2]) == (255, 255, 255))));
        // At least some modules are dark — a blank/all-white buffer would
        // still "round-trip" through every assertion above.
        assert!(rgba.chunks_exact(4).any(|p| p[0] == 0));
    }

    #[test]
    fn qr_id_varies_with_content_but_is_stable_for_the_same_string() {
        let a = qr_id("http://host/?t=aaaa");
        let b = qr_id("http://host/?t=bbbb");
        assert_ne!(a, b, "a rotated token must produce a different image id");
        assert_eq!(a, qr_id("http://host/?t=aaaa"));
    }
}
