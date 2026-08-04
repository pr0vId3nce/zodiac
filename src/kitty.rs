//! Kitty graphics protocol support for the home page's tarot cards.
//!
//! Card art is generated procedurally (night-sky gradient, gold frame,
//! stars, crescent moon, status-colored glow) as raw RGBA and transmitted
//! with `f=32`. Placements use `z=-1` so they sit *under* the text layer:
//! ratatui keeps drawing the card text normally and the image shows through
//! wherever glyphs don't cover. Terminals without the protocol simply never
//! get these escapes — the Unicode card layer stands alone there.

use std::io::Write;

/// Image ids are namespaced so we never collide with another app's ids in
/// the same terminal. One image per status accent; retransmitted (same id)
/// when the card pixel size changes.
const ID_BASE: u32 = 0x57444700; // "WDG\0"

/// The emblem painted at the top of the card: a `>_` terminal prompt for
/// plain shells, Claude's ✳ starburst for idle claude panes, or the
/// bouncing mascot (4 animation frames) while claude is working/thinking.
#[derive(Clone, Copy, PartialEq)]
pub enum CardMark {
    Terminal,
    Claude,
    ClaudeRun(u8),
}

pub fn image_id(accent_idx: usize, mark: CardMark, size_idx: usize, selected: bool) -> u32 {
    let code = match mark {
        CardMark::Terminal => 0,
        CardMark::Claude => 1,
        CardMark::ClaudeRun(f) => 2 + (f as u32 % 4),
    };
    ID_BASE + selected as u32 * 8192 + size_idx as u32 * 512 + accent_idx as u32 * 16 + code
}

/// Everything that shapes one card's pixels.
pub struct CardStyle {
    pub accent: (u8, u8, u8),
    pub mark: CardMark,
    pub icon_scale: f32,
    /// Painted gold frame rings: 2 (classic double), 1, or 0.
    pub rings: u8,
    /// Selection outline at the card's outer edge: (color, thickness px).
    pub sel: Option<((u8, u8, u8), f32)>,
    /// Fancy selection ring: rounded corners + soft glow (vs a hard
    /// square ring).
    pub sel_glow: bool,
    /// Mascot body shape: soft rounded blob vs boxy (closer to the real
    /// Clawd).
    pub mascot_soft: bool,
}

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
        stamp(px, w, h, a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, th, col);
    }
}

/// Paint one tarot card: indigo night-sky gradient with vignette, double
/// gold frame, star field, the pane's emblem up top (`>_` prompt or Claude
/// starburst), and a soft glow from the bottom in the status accent color.
pub fn card_rgba(w: u32, h: u32, style: &CardStyle) -> Vec<u8> {
    let accent = style.accent;
    let mark = style.mark;
    let scale = style.icon_scale;
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let moon_r = fh * scale * 1.5; // emblem zone: kept clear of stars
    let (mx, my) = (fw / 2.0, fh * 0.14);
    let bw = (w / 220).max(1); // frame line thickness in px

    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f32, y as f32);
            let t = yf / fh;
            // Night sky: deep indigo up top fading to near-black.
            let mut r = 22.0 + 26.0 * (1.0 - t);
            let mut g = 17.0 + 19.0 * (1.0 - t);
            let mut b = 44.0 + 46.0 * (1.0 - t);
            // Horizontal vignette.
            let dx = (xf / fw - 0.5).abs() * 2.0;
            let vig = 1.0 - 0.30 * dx * dx;
            r *= vig;
            g *= vig;
            b *= vig;
            // Accent glow rising from below the bottom edge.
            let gx = xf / fw - 0.5;
            let gy = yf / fh - 1.08;
            let d = (gx * gx * 1.7 + gy * gy).sqrt();
            let glow = (1.0 - d / 0.60).max(0.0);
            let glow = glow * glow * 0.50;
            r += accent.0 as f32 * glow;
            g += accent.1 as f32 * glow;
            b += accent.2 as f32 * glow;
            // Double gold frame.
            let on_ring = |inset: u32| -> bool {
                let (i, o) = (inset, inset + bw);
                let hx = (x >= i && x < o) || (x >= w.saturating_sub(o) && x < w - i);
                let hy = (y >= i && y < o) || (y >= h.saturating_sub(o) && y < h - i);
                (hx && y >= i && y < h - i) || (hy && x >= i && x < w - i)
            };
            let framed = match style.rings {
                0 => false,
                1 => on_ring(2 + bw),
                _ => on_ring(2 + bw) || on_ring(6 + 3 * bw),
            };
            if framed {
                r = r * 0.25 + 186.0 * 0.75;
                g = g * 0.25 + 154.0 * 0.75;
                b = b * 0.25 + 82.0 * 0.75;
            }
            let o = ((y * w + x) * 4) as usize;
            px[o] = r.min(255.0) as u8;
            px[o + 1] = g.min(255.0) as u8;
            px[o + 2] = b.min(255.0) as u8;
            px[o + 3] = 255;
        }
    }

    // Star field: deterministic, denser toward the top, a few with glints.
    let stars = (w * h / 2200).max(8);
    for k in 0..stars {
        let hx = hash(k as u64 * 7919 + 17);
        let sx = (hx % w as u64) as u32;
        let sy = ((hash(hx) % (h as u64 * 3 / 4)) as f32 * 0.9) as u32 + h / 20;
        let md = ((sx as f32 - mx).powi(2) + (sy as f32 - my).powi(2)).sqrt();
        if md < moon_r * 1.6 {
            continue; // keep the moon's halo clean
        }
        let bright = 120 + (hash(hx ^ 0xbeef) % 120) as u8;
        let mut put = |x: i64, y: i64, v: u8| {
            if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                let o = ((y as u32 * w + x as u32) * 4) as usize;
                px[o] = px[o].max(v);
                px[o + 1] = px[o + 1].max(v);
                px[o + 2] = px[o + 2].max((v as u32 * 9 / 10) as u8);
            }
        };
        put(sx as i64, sy as i64, bright);
        if k % 5 == 0 {
            let dim = bright / 2;
            put(sx as i64 - 1, sy as i64, dim);
            put(sx as i64 + 1, sy as i64, dim);
            put(sx as i64, sy as i64 - 1, dim);
            put(sx as i64, sy as i64 + 1, dim);
        }
    }

    // The emblem, drawn last so it sits over any stray star pixels.
    let s = fh * scale;
    match mark {
        CardMark::Terminal => {
            // `>_` prompt in the moon's old pale gold.
            let gold = (232, 218, 176);
            let th = (s * 0.18).max(1.2);
            seg(&mut px, w, h, (mx - 2.1 * s, my - 1.3 * s), (mx - 0.5 * s, my), th, gold);
            seg(&mut px, w, h, (mx - 0.5 * s, my), (mx - 2.1 * s, my + 1.3 * s), th, gold);
            seg(
                &mut px,
                w,
                h,
                (mx + 0.2 * s, my + 1.3 * s),
                (mx + 2.1 * s, my + 1.3 * s),
                th,
                gold,
            );
        }
        CardMark::Claude => {
            // Claude's ✳: eight spokes in Anthropic coral.
            let coral = (222, 122, 88);
            let th = (s * 0.20).max(1.3);
            for k in 0..8 {
                let ang = k as f32 * std::f32::consts::FRAC_PI_4;
                let (dx, dy) = (ang.cos(), ang.sin());
                seg(
                    &mut px,
                    w,
                    h,
                    (mx + dx * 0.35 * s, my + dy * 0.35 * s),
                    (mx + dx * 1.9 * s, my + dy * 1.9 * s),
                    th,
                    coral,
                );
            }
        }
        CardMark::ClaudeRun(frame) => {
            draw_clawd(&mut px, w, h, mx, my, s, fh, frame, style.mascot_soft)
        }
    }
    // Selection outline hugging the outer edge, so the highlight genuinely
    // surrounds the card instead of running through cell centers like a
    // text border would. In glow mode it's a rounded-rect ring (signed
    // distance field) with a soft halo; otherwise a hard square ring.
    if let Some((col, th)) = style.sel {
        if style.sel_glow {
            let (cx, cy) = (fw / 2.0, fh / 2.0);
            let inset = th * 0.5 + 1.5;
            let rad = (fh * 0.06).clamp(3.0, 16.0);
            let (hw, hh) = (fw / 2.0 - inset, fh / 2.0 - inset);
            let glow_r = th * 2.0 + 5.0;
            for y in 0..h {
                for x in 0..w {
                    let qx = (x as f32 - cx).abs() - (hw - rad);
                    let qy = (y as f32 - cy).abs() - (hh - rad);
                    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                        + qx.max(qy).min(0.0)
                        - rad;
                    let ad = d.abs();
                    let core = (th * 0.5 - ad + 0.7).clamp(0.0, 1.0);
                    let halo = (1.0 - (ad - th * 0.5).max(0.0) / glow_r).clamp(0.0, 1.0);
                    let a = core.max(halo * halo * 0.45);
                    if a > 0.01 {
                        let o = ((y * w + x) * 4) as usize;
                        px[o] = (px[o] as f32 + (col.0 as f32 - px[o] as f32) * a) as u8;
                        px[o + 1] =
                            (px[o + 1] as f32 + (col.1 as f32 - px[o + 1] as f32) * a) as u8;
                        px[o + 2] =
                            (px[o + 2] as f32 + (col.2 as f32 - px[o + 2] as f32) * a) as u8;
                    }
                }
            }
        } else {
            let th = th.max(1.0) as u32;
            for y in 0..h {
                for x in 0..w {
                    if x < th || y < th || x >= w - th || y >= h - th {
                        let o = ((y * w + x) * 4) as usize;
                        px[o] = col.0;
                        px[o + 1] = col.1;
                        px[o + 2] = col.2;
                    }
                }
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
                    let spec =
                        (-((sx * sx + sy * sy) / (r * r * 0.06))).exp() * 0.85;
                    rr += (255.0 - rr) * spec;
                    gg += (255.0 - gg) * spec;
                    bb += (255.0 - bb) * spec;
                    a = (a + spec * 0.4).min(0.95);
                } else {
                    // Soft halo bleeding past the sphere, pulsing.
                    let halo =
                        glow * (-((d - r) / (r * 0.55)).powi(2)).exp() * 0.35;
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

/// Image-id base for the Wizard chat panel's portrait ("WIZ\0"), clear of
/// the card, orb, and pane-image ranges.
pub const WIZ_BASE: u32 = 0x5749_5A00;
/// Pulse frames for the awake (streaming) and waking portraits; the other
/// states are still images (frame 0).
pub const WIZ_FRAMES: u32 = 6;
/// The frame whose pulse phase peaks — used as the steady awake portrait.
pub const WIZ_STEADY: u32 = 2;

pub fn wizard_id(status_idx: usize, frame: u32) -> u32 {
    WIZ_BASE + status_idx as u32 * 16 + frame
}

/// Paint the Wizard's portrait card: the same night-sky treatment as the
/// tarot cards, with a robed figure, staff and orb whose mood tracks the
/// model's status. `status_idx`: 0 awake, 1 waking, 2 sleeping, 3 away.
/// Draw the hopping Clawd mascot into an RGBA buffer. Shared between the
/// painted cards (opaque night sky) and the standalone transparent sprite
/// used by the blocks home view — every write also raises alpha so it
/// composites cleanly over the terminal background.
fn draw_clawd(px: &mut [u8], w: u32, h: u32, mx: f32, my: f32, s: f32, fh: f32, frame: u8, soft: bool) {

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
        seg(px, w, h, (x0, y0), (x0, y0 + leg_l + bh2 * 0.15), limb_th, coral8);
    }
    let rad = bh2 * if soft { 0.55 } else { 0.20 };
    for y in (cy - bh2 - 2.0).max(0.0) as u32..((cy + bh2 + 2.0) as u32).min(h) {
        for x in (mx - bw2 - 2.0).max(0.0) as u32..((mx + bw2 + 2.0) as u32).min(w) {
            let qx = (x as f32 - mx).abs() - (bw2 - rad);
            let qy = (y as f32 - cy).abs() - (bh2 - rad);
            let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                + qx.max(qy).min(0.0)
                - rad;
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

/// Image ids for the HAL 9000 chat portrait (status x blink frame).
pub fn hal_id(status_idx: usize, frame: u32) -> u32 {
    WIZ_BASE + 0x80 + status_idx as u32 * 8 + frame
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
                let lid =
                    1.0 - ((dy.abs() - open * bez_in) / (r * 0.10)).clamp(0.0, 1.0) * 0.95;
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

/// `phase` in [0,1) drives the orb's glow pulse.
pub fn wizard_rgba(w: u32, h: u32, status_idx: usize, phase: f32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let pulse = 0.5 + 0.5 * (phase * std::f32::consts::TAU).sin();
    // Palette per status: bottom glow accent, robe, trim, orb core.
    let (accent, robe, trim, orb): (
        (u8, u8, u8),
        (f32, f32, f32),
        (u8, u8, u8),
        (f32, f32, f32),
    ) = match status_idx {
        0 => ((150, 110, 235), (88.0, 62.0, 168.0), (186, 154, 82), (190.0, 225.0, 255.0)),
        1 => ((255, 180, 60), (104.0, 72.0, 150.0), (186, 154, 82), (255.0, 205.0, 120.0)),
        2 => ((60, 80, 180), (46.0, 40.0, 92.0), (110, 100, 80), (78.0, 88.0, 128.0)),
        _ => ((120, 60, 70), (55.0, 58.0, 78.0), (90, 92, 104), (86.0, 92.0, 112.0)),
    };
    let bw = (w / 220).max(1);

    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f32, y as f32);
            let t = yf / fh;
            let mut r = 22.0 + 26.0 * (1.0 - t);
            let mut g = 17.0 + 19.0 * (1.0 - t);
            let mut b = 44.0 + 46.0 * (1.0 - t);
            // Away: the sky itself goes gray and thin.
            if status_idx == 3 {
                let avg = (r + g + b) / 3.0;
                r = r * 0.4 + avg * 0.6;
                g = g * 0.4 + avg * 0.6;
                b = b * 0.4 + avg * 0.6;
            }
            let dx = (xf / fw - 0.5).abs() * 2.0;
            let vig = 1.0 - 0.30 * dx * dx;
            r *= vig;
            g *= vig;
            b *= vig;
            let gx = xf / fw - 0.5;
            let gy = yf / fh - 1.08;
            let d = (gx * gx * 1.7 + gy * gy).sqrt();
            let glow = (1.0 - d / 0.60).max(0.0);
            let glow = glow * glow * 0.40;
            r += accent.0 as f32 * glow;
            g += accent.1 as f32 * glow;
            b += accent.2 as f32 * glow;
            // Single gold frame ring, as on the cards.
            let (i, o) = (2 + bw, 2 + 2 * bw);
            let hx = (x >= i && x < o) || (x >= w.saturating_sub(o) && x < w - i);
            let hy = (y >= i && y < o) || (y >= h.saturating_sub(o) && y < h - i);
            if (hx && y >= i && y < h - i) || (hy && x >= i && x < w - i) {
                r = r * 0.25 + trim.0 as f32 * 0.75;
                g = g * 0.25 + trim.1 as f32 * 0.75;
                b = b * 0.25 + trim.2 as f32 * 0.75;
            }
            let off = ((y * w + x) * 4) as usize;
            px[off] = r.min(255.0) as u8;
            px[off + 1] = g.min(255.0) as u8;
            px[off + 2] = b.min(255.0) as u8;
            px[off + 3] = 255;
        }
    }

    // Stars, sparse, kept away from the figure.
    let cx = fw / 2.0;
    let stars = (w * h / 2600).max(6);
    for k in 0..stars {
        let hx = hash(k as u64 * 6271 + 5);
        let sx = (hx % w as u64) as f32;
        let sy = (hash(hx) % (h as u64 * 3 / 4)) as f32;
        if (sx - cx).abs() < fh * 0.40 && sy > fh * 0.18 {
            continue;
        }
        let bright = 110 + (hash(hx ^ 0xfeed) % 110) as u8;
        stamp(&mut px, w, h, sx, sy, 0.7, (bright, bright, bright));
    }

    // Sleeping: a crescent moon in the corner.
    if status_idx == 2 {
        let (mx, my, mr) = (fw * 0.16, fh * 0.24, fh * 0.10);
        stamp(&mut px, w, h, mx, my, mr, (222, 214, 178));
        stamp(&mut px, w, h, mx + mr * 0.55, my - mr * 0.25, mr * 0.95, (24, 20, 48));
    }

    // The figure. All proportions hang off the canvas height.
    let base_y = fh * 0.92;
    let sh_y = base_y - fh * 0.40;
    let head = (cx, sh_y - fh * 0.085);
    let head_r = fh * 0.095;
    let robe8 = (robe.0 as u8, robe.1 as u8, robe.2 as u8);

    // Robe: a shaded triangle from shoulders to hem.
    for y in sh_y as u32..(base_y as u32).min(h) {
        let t = (y as f32 - sh_y) / (base_y - sh_y);
        let hw = fh * (0.07 + 0.11 * t);
        let shade = 1.0 - 0.30 * t;
        for x in ((cx - hw).max(0.0)) as u32..((cx + hw) as u32).min(w) {
            let edge = (hw - (x as f32 - cx).abs()).clamp(0.0, 1.0);
            let o = ((y * w + x) * 4) as usize;
            px[o] = (px[o] as f32 + (robe.0 * shade - px[o] as f32) * edge) as u8;
            px[o + 1] = (px[o + 1] as f32 + (robe.1 * shade - px[o + 1] as f32) * edge) as u8;
            px[o + 2] = (px[o + 2] as f32 + (robe.2 * shade - px[o + 2] as f32) * edge) as u8;
        }
    }
    // Hood + face shadow.
    stamp(&mut px, w, h, head.0, head.1, head_r, robe8);
    stamp(
        &mut px,
        w,
        h,
        head.0,
        head.1 + head_r * 0.15,
        head_r * 0.62,
        (22, 18, 38),
    );
    // Beard.
    seg(
        &mut px,
        w,
        h,
        (head.0, head.1 + head_r * 0.55),
        (head.0, head.1 + head_r * 1.45),
        head_r * 0.34,
        (198, 198, 210),
    );
    // Hat: brim line + leaning cone.
    let brim_y = head.1 - head_r * 0.45;
    let apex = (cx + fh * 0.055, brim_y - fh * 0.26);
    seg(
        &mut px,
        w,
        h,
        (cx - fh * 0.155, brim_y),
        (cx + fh * 0.155, brim_y),
        fh * 0.016,
        robe8,
    );
    let steps = (fh * 0.26) as u32;
    for i in 0..=steps {
        let t = i as f32 / steps.max(1) as f32;
        let y = brim_y + (apex.1 - brim_y) * t;
        let xm = cx + (apex.0 - cx) * t;
        let hw = fh * 0.11 * (1.0 - t);
        seg(&mut px, w, h, (xm - hw, y), (xm + hw, y), 1.0, robe8);
    }
    // Trim band where the hat meets the brim.
    seg(
        &mut px,
        w,
        h,
        (cx - fh * 0.10, brim_y - fh * 0.02),
        (cx + fh * 0.10, brim_y - fh * 0.02),
        fh * 0.012,
        trim,
    );
    // Eyes: bright when awake/waking, closed dashes asleep, none when away.
    match status_idx {
        0 | 1 => {
            for side in [-1.0f32, 1.0] {
                stamp(
                    &mut px,
                    w,
                    h,
                    head.0 + side * head_r * 0.32,
                    head.1 + head_r * 0.05,
                    head_r * 0.11,
                    (255, 226, 150),
                );
            }
        }
        2 => {
            for side in [-1.0f32, 1.0] {
                let ex = head.0 + side * head_r * 0.32;
                let ey = head.1 + head_r * 0.08;
                seg(
                    &mut px,
                    w,
                    h,
                    (ex - head_r * 0.14, ey),
                    (ex + head_r * 0.14, ey),
                    head_r * 0.06,
                    (170, 170, 190),
                );
            }
        }
        _ => {}
    }
    // Staff and its orb.
    let stx = cx + fh * 0.26;
    let top = (stx + fh * 0.035, base_y - fh * 0.64);
    seg(
        &mut px,
        w,
        h,
        (stx, base_y),
        top,
        fh * 0.018,
        (116, 88, 60),
    );
    let orb_r = fh * 0.055;
    let lit = match status_idx {
        0 => 0.55 + 0.45 * pulse,
        1 => 0.30 + 0.45 * pulse,
        _ => 0.25,
    };
    // Halo first, then the core.
    let halo_r = orb_r * (2.2 + 1.3 * lit);
    for y in ((top.1 - halo_r).max(0.0)) as u32..((top.1 + halo_r) as u32 + 1).min(h) {
        for x in ((top.0 - halo_r).max(0.0)) as u32..((top.0 + halo_r) as u32 + 1).min(w) {
            let d = ((x as f32 - top.0).powi(2) + (y as f32 - top.1).powi(2)).sqrt();
            let a = (1.0 - d / halo_r).clamp(0.0, 1.0);
            let a = a * a * 0.55 * lit;
            if a > 0.01 {
                let o = ((y * w + x) * 4) as usize;
                px[o] = (px[o] as f32 + (orb.0 - px[o] as f32) * a) as u8;
                px[o + 1] = (px[o + 1] as f32 + (orb.1 - px[o + 1] as f32) * a) as u8;
                px[o + 2] = (px[o + 2] as f32 + (orb.2 - px[o + 2] as f32) * a) as u8;
            }
        }
    }
    let core = (
        (orb.0 * (0.5 + 0.5 * lit)).min(255.0) as u8,
        (orb.1 * (0.5 + 0.5 * lit)).min(255.0) as u8,
        (orb.2 * (0.5 + 0.5 * lit)).min(255.0) as u8,
    );
    stamp(&mut px, w, h, top.0, top.1, orb_r, core);

    // Sleeping: drifting z's rising from the hood.
    if status_idx == 2 {
        let gold = (210, 200, 160);
        for (i, k) in [0.024f32, 0.034, 0.046].iter().enumerate() {
            let s = fh * k;
            let zx = head.0 + fh * (0.16 + 0.05 * i as f32);
            let zy = head.1 - fh * (0.10 + 0.11 * i as f32);
            let th = (s * 0.22).max(0.8);
            seg(&mut px, w, h, (zx - s, zy - s), (zx + s, zy - s), th, gold);
            seg(&mut px, w, h, (zx + s, zy - s), (zx - s, zy + s), th, gold);
            seg(&mut px, w, h, (zx - s, zy + s), (zx + s, zy + s), th, gold);
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
    write!(out, "\x1b_Ga=p,i={id},p={pid},c={cols},r={rows},z=-1,C=1,q=2\x1b\\")
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
