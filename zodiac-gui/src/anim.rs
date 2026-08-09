//! Animation playback (roadmap 4.2): client-side reassembly of
//! `T_GFX_FRAME` chunks plus the wall-clock playhead. The server stores
//! frames and playback state (`SnapAnim`); this module owns the clock —
//! the server never schedules a frame.
//!
//! Display-frame convention: frame 0 is the root image data (`T_GFX_IMG`);
//! frames 1.. are the `T_GFX_FRAME` frames in `idx` order. `SnapAnim.gaps`
//! is indexed by display frame; a missing or zero gap falls back to the
//! last non-zero gap (then 40 ms), which also keeps timing sane if the
//! server's gap list turns out to exclude the root frame.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zodiac::gfx::SnapAnim;
use zodiac::protocol::GfxFrameHdr;

/// One reassembled animation frame, still in wire format (decoded to RGBA
/// lazily by the renderer, exactly like the root image).
pub struct GFrame {
    pub format: u8,
    pub zlib: bool,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// Per-(pane, img, ver) frame store + playheads.
#[derive(Default)]
pub struct AnimStore {
    /// Completed frames beyond the root, in `idx` order.
    frames: HashMap<(u64, u32, u32), Vec<GFrame>>,
    /// Chunked `T_GFX_FRAME` payloads still assembling, keyed with idx.
    partial: HashMap<(u64, u32, u32, u32), Vec<u8>>,
    /// Wall-clock playback start per animating image.
    started: HashMap<(u64, u32, u32), Instant>,
}

impl AnimStore {
    /// Fold one `T_GFX_FRAME` chunk in; a frame is complete when the
    /// reassembly buffer reaches `hdr.total` (mirrors `apply_gfx_chunk`).
    pub fn apply_chunk(&mut self, pane: u64, hdr: &GfxFrameHdr, chunk: &[u8]) {
        let pkey = (pane, hdr.img, hdr.ver, hdr.idx);
        let buf = self.partial.entry(pkey).or_default();
        if hdr.off == 0 {
            buf.clear();
        }
        buf.extend_from_slice(chunk);
        if buf.len() as u32 >= hdr.total {
            let data = std::mem::take(buf);
            self.partial.remove(&pkey);
            let v = self.frames.entry((pane, hdr.img, hdr.ver)).or_default();
            let idx = hdr.idx as usize;
            // Frames arrive in idx order; pad defensively if one went missing
            // (an empty frame decodes to None and falls back to the root).
            while v.len() < idx {
                v.push(GFrame {
                    format: 0,
                    zlib: false,
                    w: 0,
                    h: 0,
                    data: Vec::new(),
                });
            }
            let f = GFrame {
                format: hdr.format,
                zlib: hdr.zlib,
                w: hdr.w,
                h: hdr.h,
                data,
            };
            if idx < v.len() {
                v[idx] = f;
            } else {
                v.push(f);
            }
        }
    }

    /// Frame `idx` beyond the root (display frame idx+1), if reassembled.
    pub fn frame(&self, pane: u64, img: u32, ver: u32, idx: usize) -> Option<&GFrame> {
        self.frames.get(&(pane, img, ver)).and_then(|v| v.get(idx))
    }

    pub fn has_frame(&self, pane: u64, img: u32, ver: u32, idx: usize) -> bool {
        self.frame(pane, img, ver, idx).is_some()
    }

    /// Drop state for images the server no longer lists for this pane —
    /// mirrors `CPane::apply_gfx_state`'s image retention.
    pub fn retain_pane(&mut self, pane: u64, live: &[(u32, u32)]) {
        self.frames
            .retain(|(p, i, v), _| *p != pane || live.contains(&(*i, *v)));
        self.partial
            .retain(|(p, i, v, _), _| *p != pane || live.contains(&(*i, *v)));
        self.started
            .retain(|(p, i, v), _| *p != pane || live.contains(&(*i, *v)));
    }

    pub fn drop_pane(&mut self, pane: u64) {
        self.frames.retain(|(p, ..), _| *p != pane);
        self.partial.retain(|(p, ..), _| *p != pane);
        self.started.retain(|(p, ..), _| *p != pane);
    }

    /// Resolve the playhead for one image at `now`: which display frame to
    /// show (0 = root) and the wall-clock deadline of the next flip (None =
    /// static — stopped, single-frame, or loops exhausted).
    pub fn playhead(
        &mut self,
        pane: u64,
        img: u32,
        ver: u32,
        sa: &SnapAnim,
        now: Instant,
    ) -> (usize, Option<Instant>) {
        let key = (pane, img, ver);
        let nframes = 1 + self.frames.get(&key).map_or(0, Vec::len);
        if !sa.running || nframes <= 1 {
            self.started.remove(&key);
            return (0, None);
        }
        let start = *self.started.entry(key).or_insert(now);
        let elapsed = now.duration_since(start).as_millis() as u64;
        let (idx, next_ms) = frame_at(elapsed, &sa.gaps, nframes, sa.loops);
        (idx, next_ms.map(|ms| now + Duration::from_millis(ms)))
    }
}

/// Pure playhead math: given elapsed ms since playback start, per-display-
/// frame gaps, the frame count and the loop budget (0 = forever), return
/// (display frame to show, ms until the next flip — None when static).
/// Exhausted loops hold the final frame.
pub fn frame_at(elapsed_ms: u64, gaps: &[u32], nframes: usize, loops: u32) -> (usize, Option<u64>) {
    if nframes <= 1 {
        return (0, None);
    }
    let fallback = gaps
        .iter()
        .rev()
        .copied()
        .find(|g| *g > 0)
        .unwrap_or(40)
        .max(1) as u64;
    let gap = |i: usize| -> u64 {
        gaps.get(i)
            .copied()
            .filter(|g| *g > 0)
            .map_or(fallback, u64::from)
    };
    let cycle: u64 = (0..nframes).map(gap).sum();
    if loops > 0 && elapsed_ms >= cycle * loops as u64 {
        return (nframes - 1, None);
    }
    let mut t = elapsed_ms % cycle;
    for i in 0..nframes {
        let g = gap(i);
        if t < g {
            return (i, Some(g - t));
        }
        t -= g;
    }
    (0, Some(gap(0))) // unreachable: t < cycle by construction
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_advance_follows_gaps() {
        let gaps = [100u32, 50, 25];
        assert_eq!(frame_at(0, &gaps, 3, 0), (0, Some(100)));
        assert_eq!(frame_at(99, &gaps, 3, 0), (0, Some(1)));
        assert_eq!(frame_at(100, &gaps, 3, 0), (1, Some(50)));
        assert_eq!(frame_at(160, &gaps, 3, 0), (2, Some(15)));
        // cycle = 175: wraps back to frame 0 forever when loops = 0
        assert_eq!(frame_at(175, &gaps, 3, 0), (0, Some(100)));
        assert_eq!(frame_at(175 * 1000 + 120, &gaps, 3, 0), (1, Some(30)));
    }

    #[test]
    fn loops_exhaust_and_hold_last_frame() {
        let gaps = [100u32, 50, 25];
        // one loop = 175 ms
        assert_eq!(frame_at(174, &gaps, 3, 1), (2, Some(1)));
        assert_eq!(frame_at(175, &gaps, 3, 1), (2, None));
        assert_eq!(frame_at(349, &gaps, 3, 2), (2, Some(1)));
        assert_eq!(frame_at(350, &gaps, 3, 2), (2, None));
    }

    #[test]
    fn missing_or_zero_gaps_fall_back() {
        // Fewer gaps than frames (root-exclusive server list): the last
        // non-zero gap covers the tail.
        let gaps = [120u32, 40];
        assert_eq!(frame_at(0, &gaps, 3, 0), (0, Some(120)));
        assert_eq!(frame_at(120, &gaps, 3, 0), (1, Some(40)));
        assert_eq!(frame_at(160, &gaps, 3, 0), (2, Some(40)));
        // No usable gaps at all: 40 ms default.
        assert_eq!(frame_at(0, &[0, 0], 2, 0), (0, Some(40)));
        // Single frame never animates.
        assert_eq!(frame_at(1234, &gaps, 1, 0), (0, None));
    }

    #[test]
    fn chunked_frames_reassemble_in_order() {
        let mut st = AnimStore::default();
        let hdr = |idx: u32, off: u32, total: u32| GfxFrameHdr {
            img: 7,
            ver: 1,
            idx,
            gap_ms: 40,
            format: 32,
            zlib: false,
            w: 1,
            h: 1,
            off,
            total,
        };
        st.apply_chunk(9, &hdr(0, 0, 4), &[1, 2]);
        assert!(!st.has_frame(9, 7, 1, 0));
        st.apply_chunk(9, &hdr(0, 2, 4), &[3, 4]);
        assert_eq!(st.frame(9, 7, 1, 0).unwrap().data, vec![1, 2, 3, 4]);
        st.apply_chunk(9, &hdr(1, 0, 1), &[5]);
        assert_eq!(st.frame(9, 7, 1, 1).unwrap().data, vec![5]);
        // Retention drops frames for images the server no longer lists.
        st.retain_pane(9, &[(7, 2)]);
        assert!(!st.has_frame(9, 7, 1, 0));
    }
}
