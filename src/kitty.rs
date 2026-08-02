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
                seg(&mut px, w, h, (x0, y0), (x0, y0 + leg_l + bh2 * 0.15), limb_th, coral8);
            }
            let rad = bh2 * if style.mascot_soft { 0.55 } else { 0.20 };
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
                seg(&mut px, w, h, (x0, y0), (x1, y1), limb_th, coral8);
            }
            // Eyes: two vertical ovals (stacked stamps), squashing with the
            // body so the bounce reads.
            let er = (bh2 * 0.16).max(1.0);
            for side in [-1.0f32, 1.0] {
                let ex = mx + side * bw2 * 0.40;
                let ey0 = cy - bh2 * 0.12;
                let stretch = bh2 * 0.14 * (1.0 - 0.5 * phase);
                stamp(&mut px, w, h, ex, ey0 - stretch, er, eye);
                stamp(&mut px, w, h, ex, ey0, er, eye);
                stamp(&mut px, w, h, ex, ey0 + stretch, er, eye);
            }
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

/// Paint the orb ("palantir") or circle cursor into a cell-sized straight-
/// alpha RGBA buffer. The body is translucent so the glyph underneath stays
/// readable; `phase` in [0,1) drives the glow pulse.
pub fn orb_rgba(w: u32, h: u32, col: (u8, u8, u8), orb: bool, phase: f32) -> Vec<u8> {
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
            if orb {
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
