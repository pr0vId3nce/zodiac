//! Kitty Unicode-placeholder (`U=1`) tiling math (roadmap 4.3).
//!
//! v1 scheme subset (documented limitation, see docs/icebox.md): a virtual
//! placement's tiles are resolved from the *shape* of the placeholder cells
//! the app drew — the bounding box of its U+10EEEE cells is treated as a
//! contiguous rectangular block filled row-major, and the image's source
//! rect is tiled across that box. Image identity comes from the cell's fg
//! color (lower 24 bits of the image id; indexed colors cover ids <= 255),
//! or from being the only virtual placement when the fg is default. The
//! full combining-diacritic row/column encoding (arbitrary tile placement,
//! partially scrolled blocks showing the correct crop) is deferred; a block
//! whose top rows scrolled off screen renders the image squeezed into the
//! visible remainder instead of cropped.

/// U+10EEEE — the kitty image-placeholder codepoint.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// One horizontal run of placeholder cells: screen cells
/// `[col, col+len)` on `row`, showing tiles `[tile_col, tile_col+len)` of
/// tile row `tile_row` (tile coordinates relative to the block's bbox).
#[derive(Debug, PartialEq)]
pub struct Run {
    pub row: u16,
    pub col: u16,
    pub len: u16,
    pub tile_row: u16,
    pub tile_col: u16,
}

/// Resolve unordered placeholder cell coordinates `(row, col)` into the
/// block's tile-grid size `(tile_cols, tile_rows)` plus merged horizontal
/// runs. Returns None when there are no cells.
pub fn runs(cells: &[(u16, u16)]) -> Option<(u16, u16, Vec<Run>)> {
    let (mut r0, mut r1, mut c0, mut c1) = (u16::MAX, 0u16, u16::MAX, 0u16);
    for &(r, c) in cells {
        r0 = r0.min(r);
        r1 = r1.max(r);
        c0 = c0.min(c);
        c1 = c1.max(c);
    }
    if r0 == u16::MAX {
        return None;
    }
    let (tile_cols, tile_rows) = (c1 - c0 + 1, r1 - r0 + 1);
    let mut sorted: Vec<(u16, u16)> = cells.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out: Vec<Run> = Vec::new();
    for (r, c) in sorted {
        match out.last_mut() {
            Some(run) if run.row == r && run.col + run.len == c => run.len += 1,
            _ => out.push(Run {
                row: r,
                col: c,
                len: 1,
                tile_row: r - r0,
                tile_col: c - c0,
            }),
        }
    }
    Some((tile_cols, tile_rows, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangular_block_tiles_row_major() {
        // 2 rows x 3 cols anchored at (5, 10), given out of order.
        let cells = [(6, 11), (5, 10), (5, 12), (6, 10), (5, 11), (6, 12)];
        let (tc, tr, rs) = runs(&cells).unwrap();
        assert_eq!((tc, tr), (3, 2));
        assert_eq!(
            rs,
            vec![
                Run {
                    row: 5,
                    col: 10,
                    len: 3,
                    tile_row: 0,
                    tile_col: 0
                },
                Run {
                    row: 6,
                    col: 10,
                    len: 3,
                    tile_row: 1,
                    tile_col: 0
                },
            ]
        );
    }

    #[test]
    fn gaps_split_runs_and_keep_tile_offsets() {
        // Row 3: cols 4,5 then 7 (hole at 6) — two runs, tile cols 0 and 3.
        let cells = [(3, 4), (3, 5), (3, 7)];
        let (tc, tr, rs) = runs(&cells).unwrap();
        assert_eq!((tc, tr), (4, 1));
        assert_eq!(rs.len(), 2);
        assert_eq!((rs[0].col, rs[0].len, rs[0].tile_col), (4, 2, 0));
        assert_eq!((rs[1].col, rs[1].len, rs[1].tile_col), (7, 1, 3));
    }

    #[test]
    fn empty_and_single_cell() {
        assert!(runs(&[]).is_none());
        let (tc, tr, rs) = runs(&[(0, 0)]).unwrap();
        assert_eq!((tc, tr), (1, 1));
        assert_eq!(rs.len(), 1);
        assert_eq!((rs[0].tile_row, rs[0].tile_col, rs[0].len), (0, 0, 1));
    }
}
