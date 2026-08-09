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
    fn dim_darkens_the_resolved_fg() {
        let s = CellStyle {
            fg: vt100::Color::Rgb(90, 90, 90),
            dim: true,
            ..Default::default()
        };
        assert_eq!(cell_colors(&s).0, [60, 60, 60]);
    }
}
