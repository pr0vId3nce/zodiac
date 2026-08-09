//! Terminal color mapping: vt100 cell attributes -> RGB, including the
//! xterm 256-color palette and the SGR modifier fold (bold-brightens,
//! inverse swaps, dim darkens). Pure functions — unit tested offscreen.

/// Default foreground/background of the GUI theme (matches the dark
/// clear color the S3 prototype settled on).
pub const DEFAULT_FG: [u8; 3] = [220, 220, 210];
pub const DEFAULT_BG: [u8; 3] = [13, 13, 18];
/// Chrome accents.
pub const ACCENT: [u8; 3] = [130, 170, 255];
pub const CHROME_BG: [u8; 3] = [24, 24, 32];
pub const CHROME_FG: [u8; 3] = [150, 150, 160];

// --- Named colors (ported from the TUI's COLOR_CHOICES) -------------------
// Spinner + glow color pickers cycle through these in order.
pub const COLOR_NAMES: &[&str] = &[
    "orange", "gold", "cyan", "blue", "violet", "pink", "green", "red", "white", "gray", "dark",
];

/// A named color's RGB, defaulting to `default` when unknown.
pub fn named_color(name: &str, default: [u8; 3]) -> [u8; 3] {
    match name {
        "orange" => [255, 135, 0],
        "gold" => [255, 215, 0],
        "cyan" => [0, 215, 255],
        "blue" => [95, 175, 255],
        "violet" => [175, 95, 255],
        "pink" => [255, 95, 175],
        "green" => [135, 215, 135],
        "red" => [255, 95, 135],
        "white" => [255, 255, 255],
        "gray" => [138, 138, 138],
        "dark" => [98, 98, 98],
        _ => default,
    }
}

// --- Background presets ----------------------------------------------------
/// GUI backdrop presets (name, RGB). OLED true-black is the default.
pub const BG_PRESETS: &[(&str, [u8; 3])] = &[
    ("oled", [0, 0, 0]),
    ("charcoal", [13, 13, 18]),
    ("midnight", [10, 12, 20]),
    ("slate", [20, 22, 28]),
];

/// A background preset's RGB (default OLED black).
pub fn bg_color(name: &str) -> [u8; 3] {
    BG_PRESETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or([0, 0, 0])
}

// --- Working-tab braille spinner (ported from the TUI "dots") -------------
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_INTERVAL_MS: u64 = 80;

/// The spinner glyph for a given elapsed time.
pub fn spinner_frame(elapsed_ms: u64) -> &'static str {
    SPINNER_FRAMES[(elapsed_ms / SPINNER_INTERVAL_MS) as usize % SPINNER_FRAMES.len()]
}

// --- Tab markers ----------------------------------------------------------
const ZODIAC: &[&str] = &[
    "♈", "♉", "♊", "♋", "♌", "♍", "♎", "♏", "♐", "♑", "♒", "♓",
];

fn roman(mut n: usize) -> String {
    const M: &[(usize, &str)] = &[(10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut s = String::new();
    for &(v, r) in M {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// The leading marker glyph(s) for tab number `n1` (1-based) in the given
/// style. `zodiac` uses the white/text-presentation sigils (U+FE0E) — the
/// outline glyphs, never the emoji ones.
pub fn marker(style: &str, n1: usize) -> String {
    let n = n1.max(1);
    match style {
        "arabic" => n.to_string(),
        "roman" => roman(n),
        "zodiac" => format!("{}\u{FE0E}", ZODIAC[(n - 1) % ZODIAC.len()]),
        _ => "●".to_string(),
    }
}

// --- Glow (moving shimmer band over a working title) ----------------------
/// Shimmer period in ms for a speed name; `None` = glow off (static base).
pub fn glow_period_ms(speed: &str) -> Option<u64> {
    match speed {
        "off" => None,
        "slow" => Some(3200),
        "fast" => Some(1200),
        "zippy" => Some(700),
        _ => Some(2000), // "normal"
    }
}

/// Color of character `i` of `n` under the sweeping glow band. `phase` is
/// `elapsed_ms % period / period` in 0..1. A smooth version of the TUI's
/// three-tier sweep (band = 2.5 chars): the moving center reaches `glow`,
/// falling back toward `base` with distance.
pub fn glow_color_at(i: usize, n: usize, phase: f32, base: [u8; 3], glow: [u8; 3]) -> [u8; 3] {
    let band = 2.5f32;
    let center = phase * (n as f32 + 2.0 * band) - band;
    let d = (i as f32 - center).abs();
    // 1.0 at the center, ~0 by the band edge; smooth falloff.
    let t = (1.0 - (d / band)).clamp(0.0, 1.0);
    let t = t * t; // ease so the core reads brighter than the fringe
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    [
        lerp(base[0], glow[0]),
        lerp(base[1], glow[1]),
        lerp(base[2], glow[2]),
    ]
}

/// xterm 256-color palette entry -> RGB.
pub fn xterm256(i: u8) -> [u8; 3] {
    match i {
        0..=15 => {
            const BASE: [[u8; 3]; 16] = [
                [0, 0, 0],
                [205, 0, 0],
                [0, 205, 0],
                [205, 205, 0],
                [0, 0, 238],
                [205, 0, 205],
                [0, 205, 205],
                [229, 229, 229],
                [127, 127, 127],
                [255, 0, 0],
                [0, 255, 0],
                [255, 255, 0],
                [92, 92, 255],
                [255, 0, 255],
                [0, 255, 255],
                [255, 255, 255],
            ];
            BASE[i as usize]
        }
        16..=231 => {
            let i = i - 16;
            let lv = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            [lv(i / 36), lv((i / 6) % 6), lv(i % 6)]
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            [v, v, v]
        }
    }
}

fn resolve(c: vt100::Color, default: [u8; 3]) -> [u8; 3] {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => xterm256(i),
        vt100::Color::Rgb(r, g, b) => [r, g, b],
    }
}

/// The SGR attributes of one cell that affect its colors.
#[derive(Clone, Copy, Default)]
pub struct CellStyle {
    pub fg: vt100::Color,
    pub bg: vt100::Color,
    pub bold: bool,
    pub dim: bool,
    pub inverse: bool,
}

/// Resolve a cell's style to (foreground RGB, Some(background RGB)).
/// `None` background means "the default backdrop — no quad needed".
pub fn cell_colors(s: &CellStyle) -> ([u8; 3], Option<[u8; 3]>) {
    // Bold on a base-palette color selects the bright variant, the classic
    // 16-color terminal behavior (in addition to the heavier glyph weight).
    let fg_src = match s.fg {
        vt100::Color::Idx(i @ 0..=7) if s.bold => vt100::Color::Idx(i + 8),
        c => c,
    };
    let (mut fg, bg) = if s.inverse {
        (resolve(s.bg, DEFAULT_BG), Some(resolve(fg_src, DEFAULT_FG)))
    } else {
        let bg = match s.bg {
            vt100::Color::Default => None,
            c => Some(resolve(c, DEFAULT_BG)),
        };
        (resolve(fg_src, DEFAULT_FG), bg)
    };
    if s.dim {
        fg = fg.map(|c| ((c as u16) * 2 / 3) as u8);
    }
    (fg, bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_corners_and_grayscale() {
        assert_eq!(xterm256(16), [0, 0, 0]);
        assert_eq!(xterm256(231), [255, 255, 255]);
        assert_eq!(xterm256(196), [255, 0, 0]); // 16 + 36*5
        assert_eq!(xterm256(21), [0, 0, 255]); // 16 + 5
        assert_eq!(xterm256(232), [8, 8, 8]);
        assert_eq!(xterm256(255), [238, 238, 238]);
        assert_eq!(xterm256(1), [205, 0, 0]);
        assert_eq!(xterm256(9), [255, 0, 0]);
    }

    #[test]
    fn default_cell_needs_no_bg_quad() {
        let (fg, bg) = cell_colors(&CellStyle::default());
        assert_eq!(fg, DEFAULT_FG);
        assert_eq!(bg, None);
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let s = CellStyle {
            fg: vt100::Color::Rgb(10, 20, 30),
            bg: vt100::Color::Rgb(1, 2, 3),
            inverse: true,
            ..Default::default()
        };
        let (fg, bg) = cell_colors(&s);
        assert_eq!(fg, [1, 2, 3]);
        assert_eq!(bg, Some([10, 20, 30]));
        // Inverse on a fully-default cell still paints: theme bg as fg,
        // theme fg as the quad — that's how block cursors read.
        let s = CellStyle {
            inverse: true,
            ..Default::default()
        };
        let (fg, bg) = cell_colors(&s);
        assert_eq!(fg, DEFAULT_BG);
        assert_eq!(bg, Some(DEFAULT_FG));
    }

    #[test]
    fn bold_brightens_base_palette_only() {
        let s = CellStyle {
            fg: vt100::Color::Idx(1),
            bold: true,
            ..Default::default()
        };
        assert_eq!(cell_colors(&s).0, xterm256(9));
        // Already-bright and cube colors are left alone.
        let s = CellStyle {
            fg: vt100::Color::Idx(196),
            bold: true,
            ..Default::default()
        };
        assert_eq!(cell_colors(&s).0, xterm256(196));
    }

    #[test]
    fn markers_render_per_style() {
        assert_eq!(marker("dots", 3), "●");
        assert_eq!(marker("arabic", 3), "3");
        assert_eq!(marker("roman", 4), "IV");
        assert_eq!(marker("roman", 12), "XII");
        // zodiac = white/text-presentation sigil (U+FE0E), never emoji.
        assert_eq!(marker("zodiac", 1), "♈\u{FE0E}");
        assert!(marker("zodiac", 13).ends_with('\u{FE0E}')); // wraps at 12
    }

    #[test]
    fn named_and_bg_lookup() {
        assert_eq!(named_color("orange", DEFAULT_FG), [255, 135, 0]);
        assert_eq!(named_color("nope", DEFAULT_FG), DEFAULT_FG);
        assert_eq!(bg_color("oled"), [0, 0, 0]);
        assert_eq!(bg_color("charcoal"), [13, 13, 18]);
        assert_eq!(bg_color("bogus"), [0, 0, 0]); // defaults to OLED
    }

    #[test]
    fn spinner_advances_and_wraps() {
        assert_eq!(spinner_frame(0), SPINNER_FRAMES[0]);
        assert_eq!(spinner_frame(80), SPINNER_FRAMES[1]);
        assert_eq!(spinner_frame(80 * 10), SPINNER_FRAMES[0]); // wraps
    }

    #[test]
    fn glow_off_and_center() {
        assert_eq!(glow_period_ms("off"), None);
        assert_eq!(glow_period_ms("slow"), Some(3200));
        let base = [50, 50, 50];
        let glow = [255, 255, 255];
        // A char far from the center stays near base.
        let far = glow_color_at(0, 20, 0.9, base, glow);
        assert!(far[0] < 90, "far char should stay dim: {far:?}");
        // Some phase lights char 5 strongly.
        let lit = (0..20)
            .map(|p| glow_color_at(5, 20, p as f32 / 20.0, base, glow)[0])
            .max()
            .unwrap();
        assert!(lit > 200, "the band should brighten char 5: {lit}");
    }

    #[test]
    fn dim_darkens_the_resolved_fg() {
        let s = CellStyle {
            fg: vt100::Color::Rgb(90, 90, 90),
            dim: true,
            ..Default::default()
        };
        assert_eq!(cell_colors(&s).0, [60, 60, 60]);
    }
}
