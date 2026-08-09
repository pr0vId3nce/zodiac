//! Kitty graphics protocol inside panes: the APC `_G` splitter, the image
//! store, and the placement engine that mirrors kitty's implicit lifecycle
//! (scrolling, scroll regions, clears, alt screen). See GRAPHICS.md.
//!
//! The engine is server-authoritative: it consumes graphics commands split
//! out of the PTY stream plus the vendored vt100's `TermEvent`s, answers
//! protocol queries on the pane's input, and produces serializable
//! placement snapshots the client compositor renders through the outer
//! terminal. Pixels are relayed verbatim — never decoded.

use std::collections::HashMap;

use crate::engine::TermEvent;
use serde::{Deserialize, Serialize};

/// Placements whose anchor line falls more than this many lines above the
/// live screen are dropped — matches the client's scrollback depth.
pub const SCROLLBACK_KEEP: i64 = 10_000;

/// Per-pane stored image data cap; single image cap. Breaches get ENOSPC.
const STORE_MAX: usize = 64 * 1024 * 1024;
const IMAGE_MAX: usize = 32 * 1024 * 1024;

/// An APC graphics escape too large to be a legal chunk (4096 B of base64
/// plus control data) is abandoned rather than carried forever.
const CARRY_MAX: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Stream splitter

/// One piece of a PTY output chunk: plain text (fed to vt100 / ring /
/// clients) or a complete graphics command.
pub enum Seg {
    Text(Vec<u8>),
    Cmd(GfxCmd),
}

/// Splits `APC _G … ST` sequences out of the raw PTY stream, joining
/// sequences that span read boundaries. Everything else passes through
/// untouched (including non-graphics APC).
pub struct GfxSplitter {
    carry: Vec<u8>,
}

impl Default for GfxSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl GfxSplitter {
    pub fn new() -> Self {
        Self { carry: Vec::new() }
    }

    pub fn split(&mut self, chunk: &[u8]) -> Vec<Seg> {
        let buf: Vec<u8> = if self.carry.is_empty() {
            chunk.to_vec()
        } else {
            let mut c = std::mem::take(&mut self.carry);
            c.extend_from_slice(chunk);
            c
        };
        let mut segs = Vec::new();
        let mut text_start = 0;
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                i += 1;
                continue;
            }
            match buf.get(i + 1) {
                // sequence may still become ESC _ G — wait for more bytes
                None => {
                    self.push_text(&mut segs, &buf[text_start..i]);
                    self.carry = buf[i..].to_vec();
                    return segs;
                }
                Some(b'_') => match buf.get(i + 2) {
                    None => {
                        self.push_text(&mut segs, &buf[text_start..i]);
                        self.carry = buf[i..].to_vec();
                        return segs;
                    }
                    Some(b'G') => {
                        // find ST (ESC \)
                        let mut j = i + 3;
                        let end = loop {
                            match buf.get(j) {
                                None => break None,
                                Some(0x1b) if buf.get(j + 1) == Some(&b'\\') => {
                                    break Some(j);
                                }
                                // lone ESC not followed by backslash: if at
                                // the very end, we can't know yet
                                Some(0x1b) if buf.get(j + 1).is_none() => {
                                    break None;
                                }
                                Some(_) => j += 1,
                            }
                        };
                        match end {
                            Some(st) => {
                                self.push_text(&mut segs, &buf[text_start..i]);
                                segs.push(Seg::Cmd(GfxCmd::parse(&buf[i + 3..st])));
                                i = st + 2;
                                text_start = i;
                            }
                            None => {
                                self.push_text(&mut segs, &buf[text_start..i]);
                                if buf.len() - i <= CARRY_MAX {
                                    self.carry = buf[i..].to_vec();
                                } // else: oversized garbage — drop it
                                return segs;
                            }
                        }
                    }
                    // non-graphics APC: leave for vt100 (which ignores it)
                    Some(_) => i += 2,
                },
                Some(_) => i += 1,
            }
        }
        self.push_text(&mut segs, &buf[text_start..]);
        segs
    }

    fn push_text(&self, segs: &mut Vec<Seg>, t: &[u8]) {
        if !t.is_empty() {
            segs.push(Seg::Text(t.to_vec()));
        }
    }
}

// ---------------------------------------------------------------------------
// Command parsing

/// A parsed `_G<keys>;<payload>` command. Values are numeric or a single
/// letter (t=, d=, o=); payload stays raw base64/path bytes.
pub struct GfxCmd {
    keys: HashMap<u8, Vec<u8>>,
    pub payload: Vec<u8>,
}

impl GfxCmd {
    pub fn parse(body: &[u8]) -> Self {
        let (ctl, payload) = match body.iter().position(|b| *b == b';') {
            Some(p) => (&body[..p], body[p + 1..].to_vec()),
            None => (body, Vec::new()),
        };
        let mut keys = HashMap::new();
        for kv in ctl.split(|b| *b == b',') {
            let mut it = kv.splitn(2, |b| *b == b'=');
            if let (Some(k), Some(v)) = (it.next(), it.next()) {
                if k.len() == 1 {
                    keys.insert(k[0], v.to_vec());
                }
            }
        }
        Self { keys, payload }
    }

    pub fn num(&self, k: u8) -> Option<i64> {
        let v = self.keys.get(&k)?;
        std::str::from_utf8(v).ok()?.parse().ok()
    }

    fn ch(&self, k: u8) -> Option<u8> {
        self.keys.get(&k).and_then(|v| v.first().copied())
    }

    fn action(&self) -> u8 {
        self.ch(b'a').unwrap_or(b't')
    }

    fn quiet(&self) -> i64 {
        self.num(b'q').unwrap_or(0)
    }

    /// A chunk continuation carries only m (and possibly q).
    fn is_continuation(&self) -> bool {
        self.keys.keys().all(|k| matches!(k, b'm' | b'q')) && self.keys.contains_key(&b'm')
    }
}

// ---------------------------------------------------------------------------
// Engine state

#[derive(Clone)]
pub struct Img {
    pub format: u8, // 24 | 32 | 100 (PNG)
    pub zlib: bool,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
    /// Bumped when the same id is retransmitted, so the compositor knows
    /// to refresh the outer terminal's copy.
    pub ver: u32,
    /// Animation frames beyond the root frame (kitty a=f), in receive
    /// order (roadmap 4.2). The root data is frame 0; the GUI plays,
    /// the TUI renders frame 0. The client times playback.
    pub frames: Vec<AnimFrame>,
    /// a=a state: whether playback runs and how many loops (0 = forever).
    pub anim_running: bool,
    pub anim_loops: u32,
    lru: u64,
}

#[derive(Clone)]
pub struct AnimFrame {
    pub gap_ms: u32,
    pub format: u8,
    pub zlib: bool,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Placement {
    /// Stable identity across moves — the compositor's diff key.
    pub key: u64,
    /// Unicode-placeholder placement (kitty `U=1`, roadmap 4.3): the app
    /// draws U+10EEEE cells and clients resolve tiles from the grid — the
    /// anchor row/col here are informational only.
    #[serde(default)]
    pub virt: bool,
    pub img: u32,
    pub img_ver: u32,
    pub pid: u32, // 0 = anonymous
    /// Anchor line in absolute line space (`total_scrolled + screen_row`).
    pub row: i64,
    pub col: u16,
    /// Source rectangle in image pixels (x, y, w, h); w/h 0 = full.
    pub src: (u32, u32, u32, u32),
    pub cols: u16,
    pub rows: u16,
    pub z: i32,
    pub offx: u16,
    pub offy: u16,
}

/// What the client needs to composite one pane: placements in screen
/// coordinates (row may be negative = that many lines into scrollback) and
/// the ids+versions of every image still alive.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct GfxSnapshot {
    pub placements: Vec<VisPlacement>,
    pub images: Vec<(u32, u32)>, // (id, ver)
    /// Animation metadata for images in this layer that carry frames
    /// (roadmap 4.2): the client owns the playback clock.
    #[serde(default)]
    pub anim: Vec<SnapAnim>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapAnim {
    pub img: u32,
    /// Per-frame display gaps, ms; the root frame plays first.
    pub gaps: Vec<u32>,
    pub running: bool,
    pub loops: u32,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct VisPlacement {
    pub key: u64,
    #[serde(default)]
    pub virt: bool,
    pub img: u32,
    pub img_ver: u32,
    pub row: i32,
    pub col: u16,
    pub src: (u32, u32, u32, u32),
    pub cols: u16,
    pub rows: u16,
    pub z: i32,
    pub offx: u16,
    pub offy: u16,
}

struct Pending {
    cmd: GfxCmd,
    data: Vec<u8>,
}

pub struct GfxEngine {
    images: HashMap<u32, Img>,
    numbers: HashMap<u32, u32>,     // I= number → id
    placements: Vec<Placement>,     // main screen, absolute rows
    alt_placements: Vec<Placement>, // alt screen, screen rows
    total_scrolled: i64,
    in_alt: bool,
    rows: u16,
    cols: u16,
    pub cell: (u16, u16), // px, from the attached client
    pending: Option<Pending>,
    store_bytes: usize,
    next_key: u64,
    next_auto_id: u32,
    lru_tick: u64,
    /// Whether to answer protocol traffic at all — off when the attached
    /// client's outer terminal can't render (apps then detect "unsupported"
    /// via the DA1 fallback instead of timing out).
    pub active: bool,
    /// Whether the capability floor advertises animation (ADR 0005):
    /// gates a=f / a=a acceptance.
    pub anim: bool,
    /// OSC 52 clipboard writes seen since the last drain (roadmap 4.7):
    /// (selection, base64 payload). Surfaced by process_output for the
    /// server to gate behind the permission inbox.
    clipboard: Vec<(Vec<u8>, Vec<u8>)>,
    /// Bumped on every visible change; the server pushes a snapshot when it
    /// differs from the last pushed value.
    pub version: u64,
}

/// Result of handling one command: bytes to write back to the pane's input
/// (query/transmit replies) and a cursor advance to synthesize.
#[derive(Default)]
pub struct CmdResult {
    pub reply: Vec<u8>,
    pub advance: Option<(u16, u16)>, // (rows down, cols right)
}

impl GfxEngine {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            images: HashMap::new(),
            numbers: HashMap::new(),
            placements: Vec::new(),
            alt_placements: Vec::new(),
            total_scrolled: 0,
            in_alt: false,
            rows,
            cols,
            cell: (0, 0),
            pending: None,
            store_bytes: 0,
            next_key: 1,
            next_auto_id: 0x0eee_0000,
            lru_tick: 0,
            active: false,
            anim: false,
            clipboard: Vec::new(),
            version: 0,
        }
    }

    pub fn image(&self, id: u32) -> Option<&Img> {
        self.images.get(&id)
    }

    fn bump(&mut self) {
        self.version += 1;
    }

    fn layer_mut(&mut self) -> &mut Vec<Placement> {
        if self.in_alt {
            &mut self.alt_placements
        } else {
            &mut self.placements
        }
    }

    /// Screen row of a placement anchor (alt rows are already screen rows).
    fn screen_row(&self, p: &Placement) -> i64 {
        if self.in_alt {
            p.row
        } else {
            p.row - self.total_scrolled
        }
    }

    // -- protocol commands ---------------------------------------------------

    /// Handle one split-out graphics command. `cursor` is the inner
    /// emulator's (row, col) at this point in the stream.
    pub fn handle(&mut self, cmd: GfxCmd, cursor: (u16, u16)) -> CmdResult {
        if !self.active {
            return CmdResult::default();
        }
        // Chunked transmission in flight: only continuations are legal.
        if self.pending.is_some() {
            if cmd.is_continuation() {
                let more = cmd.num(b'm').unwrap_or(0) == 1;
                let mut pending = self.pending.take().unwrap();
                pending.data.extend_from_slice(&cmd.payload);
                if more {
                    self.pending = Some(pending);
                    return CmdResult::default();
                }
                let Pending { cmd: first, data } = pending;
                return self.dispatch(first, Some(data), cursor);
            }
            self.pending = None; // spec violation — abort the transmission
        }
        if cmd.num(b'm') == Some(1) && matches!(cmd.action(), b't' | b'T' | b'q' | b'f') {
            self.pending = Some(Pending {
                data: cmd.payload.clone(),
                cmd,
            });
            return CmdResult::default();
        }
        self.dispatch(cmd, None, cursor)
    }

    fn dispatch(&mut self, cmd: GfxCmd, joined: Option<Vec<u8>>, cursor: (u16, u16)) -> CmdResult {
        let payload = joined.unwrap_or_else(|| cmd.payload.clone());
        match cmd.action() {
            b'q' => self.cmd_query(&cmd, &payload),
            b't' => self.cmd_transmit(&cmd, &payload, false, cursor),
            b'T' => self.cmd_transmit(&cmd, &payload, true, cursor),
            b'p' => self.cmd_place(&cmd, cursor),
            b'd' => self.cmd_delete(&cmd, cursor),
            b'f' if self.anim => self.cmd_frame(&cmd, &payload),
            b'a' if self.anim => self.cmd_animate(&cmd),
            b'f' | b'a' | b'c' => reply_err(&cmd, "ENOTSUP:animation is not supported"),
            _ => reply_err(&cmd, "EINVAL:unknown action"),
        }
    }

    fn cmd_query(&mut self, cmd: &GfxCmd, payload: &[u8]) -> CmdResult {
        match self.load_data(cmd, payload) {
            Ok(_) => reply_ok(cmd),
            Err(e) => reply_err(cmd, &e),
        }
    }

    fn cmd_transmit(
        &mut self,
        cmd: &GfxCmd,
        payload: &[u8],
        display: bool,
        cursor: (u16, u16),
    ) -> CmdResult {
        let (data, w, h) = match self.load_data(cmd, payload) {
            Ok(v) => v,
            Err(e) => return reply_err(cmd, &e),
        };
        if data.len() > IMAGE_MAX {
            return reply_err(cmd, "ENOSPC:image too large");
        }
        // id: explicit i=, else allocated for I=, else 0 (transient-ish)
        let number = cmd.num(b'I').map(|n| n as u32);
        let id = match cmd.num(b'i') {
            Some(i) => i as u32,
            None => match number {
                Some(_) => {
                    while self.images.contains_key(&self.next_auto_id) {
                        self.next_auto_id = self.next_auto_id.wrapping_add(1);
                    }
                    self.next_auto_id
                }
                None => 0,
            },
        };
        if let Some(n) = number {
            self.numbers.insert(n, id);
        }
        // Quota: evict unplaced images LRU-first, then refuse. `old`
        // counts the replaced image's frames too — retransmission drops
        // them (animation restarts from scratch).
        let img_total =
            |i: &Img| i.data.len() + i.frames.iter().map(|f| f.data.len()).sum::<usize>();
        let old = self.images.get(&id).map(img_total).unwrap_or(0);
        if self.store_bytes - old + data.len() > STORE_MAX {
            self.evict(data.len().saturating_sub(old));
            let cur = self.images.get(&id).map(img_total).unwrap_or(0);
            if self.store_bytes - cur + data.len() > STORE_MAX {
                return reply_err(cmd, "ENOSPC:image store full");
            }
        }
        self.store_bytes = self.store_bytes - old + data.len();
        self.lru_tick += 1;
        let ver = self.images.get(&id).map(|i| i.ver + 1).unwrap_or(1);
        self.images.insert(
            id,
            Img {
                format: cmd.num(b'f').unwrap_or(32) as u8,
                zlib: cmd.ch(b'o') == Some(b'z'),
                w,
                h,
                data,
                ver,
                // Retransmission restarts any animation from scratch.
                frames: Vec::new(),
                anim_running: false,
                anim_loops: 0,
                lru: self.lru_tick,
            },
        );
        // Retransmission refreshes existing placements of this id.
        let layers = [&mut self.placements, &mut self.alt_placements];
        for layer in layers {
            for p in layer.iter_mut().filter(|p| p.img == id) {
                p.img_ver = ver;
            }
        }
        self.bump();
        if display {
            let mut res = self.place_at(cmd, id, cursor);
            if res.reply.is_empty() {
                let ok = reply_ok(cmd);
                res.reply = ok.reply;
            }
            return res;
        }
        reply_ok(cmd)
    }

    /// kitty a=f: append one animation frame to an existing image
    /// (roadmap 4.2). Frame data rides the same store quota as images.
    fn cmd_frame(&mut self, cmd: &GfxCmd, payload: &[u8]) -> CmdResult {
        let Some(id) = cmd.num(b'i').map(|i| i as u32) else {
            return reply_err(cmd, "ENOENT:frame needs i=");
        };
        if !self.images.contains_key(&id) {
            return reply_err(cmd, "ENOENT:no such image");
        }
        let (data, w, h) = match self.load_data(cmd, payload) {
            Ok(t) => t,
            Err(e) => return reply_err(cmd, &e),
        };
        if self.store_bytes + data.len() > STORE_MAX {
            self.evict(data.len());
            if self.store_bytes + data.len() > STORE_MAX {
                return reply_err(cmd, "ENOSPC:store full");
            }
        }
        // The image may have been evicted while making room.
        let Some(img) = self.images.get_mut(&id) else {
            return reply_err(cmd, "ENOENT:no such image");
        };
        self.store_bytes += data.len();
        img.frames.push(AnimFrame {
            gap_ms: cmd.num(b'z').filter(|z| *z > 0).unwrap_or(40) as u32,
            format: cmd.num(b'f').unwrap_or(32) as u8,
            zlib: cmd.ch(b'o') == Some(b'z'),
            w,
            h,
            data,
        });
        self.bump();
        reply_ok(cmd)
    }

    /// kitty a=a: playback control. s=1 stops, s=2/3 runs; v = loops
    /// (0/absent = forever). The server only stores state — clients own
    /// the clock.
    fn cmd_animate(&mut self, cmd: &GfxCmd) -> CmdResult {
        let Some(id) = cmd.num(b'i').map(|i| i as u32) else {
            return reply_err(cmd, "ENOENT:animate needs i=");
        };
        let Some(img) = self.images.get_mut(&id) else {
            return reply_err(cmd, "ENOENT:no such image");
        };
        match cmd.num(b's').unwrap_or(0) {
            1 => img.anim_running = false,
            2 | 3 => img.anim_running = true,
            _ => {}
        }
        if let Some(v) = cmd.num(b'v') {
            img.anim_loops = v.max(0) as u32;
        }
        self.bump();
        reply_ok(cmd)
    }

    fn cmd_place(&mut self, cmd: &GfxCmd, cursor: (u16, u16)) -> CmdResult {
        let id = match cmd.num(b'i') {
            Some(i) => i as u32,
            None => match cmd.num(b'I').and_then(|n| self.numbers.get(&(n as u32))) {
                Some(id) => *id,
                None => return reply_err(cmd, "ENOENT:no such image"),
            },
        };
        if !self.images.contains_key(&id) {
            return reply_err(cmd, "ENOENT:no such image");
        }
        let mut res = self.place_at(cmd, id, cursor);
        if res.reply.is_empty() {
            res.reply = reply_ok(cmd).reply;
        }
        res
    }

    /// Create/replace a placement of `id` anchored at the cursor.
    fn place_at(&mut self, cmd: &GfxCmd, id: u32, cursor: (u16, u16)) -> CmdResult {
        let img = self.images.get(&id).expect("checked by callers");
        let img_ver = img.ver;
        let (iw, ih) = (img.w, img.h);
        let src = (
            cmd.num(b'x').unwrap_or(0).max(0) as u32,
            cmd.num(b'y').unwrap_or(0).max(0) as u32,
            cmd.num(b'w').unwrap_or(0).max(0) as u32,
            cmd.num(b'h').unwrap_or(0).max(0) as u32,
        );
        let (sw, sh) = (
            if src.2 > 0 {
                src.2
            } else {
                iw.saturating_sub(src.0)
            },
            if src.3 > 0 {
                src.3
            } else {
                ih.saturating_sub(src.1)
            },
        );
        let (cw, ch) = if self.cell.0 > 0 && self.cell.1 > 0 {
            (self.cell.0 as u32, self.cell.1 as u32)
        } else {
            (10, 20) // no attached client yet — a sane guess, refreshed later
        };
        let cols = cmd
            .num(b'c')
            .filter(|c| *c > 0)
            .map(|c| c as u16)
            .unwrap_or_else(|| sw.div_ceil(cw).max(1).min(u16::MAX as u32) as u16);
        let rows = cmd
            .num(b'r')
            .filter(|r| *r > 0)
            .map(|r| r as u16)
            .unwrap_or_else(|| sh.div_ceil(ch).max(1).min(u16::MAX as u32) as u16);
        let pid = cmd.num(b'p').unwrap_or(0) as u32;
        let row_abs = if self.in_alt {
            cursor.0 as i64
        } else {
            self.total_scrolled + cursor.0 as i64
        };
        let virt = cmd.num(b'U') == Some(1);
        let place = Placement {
            key: 0, // fixed below
            virt,
            img: id,
            img_ver,
            pid,
            row: row_abs,
            col: cursor.1,
            src,
            cols,
            rows,
            z: cmd.num(b'z').unwrap_or(0) as i32,
            offx: cmd.num(b'X').unwrap_or(0).max(0) as u16,
            offy: cmd.num(b'Y').unwrap_or(0).max(0) as u16,
        };
        let key = {
            let layer = if self.in_alt {
                &mut self.alt_placements
            } else {
                &mut self.placements
            };
            // Identified placements replace on (image, placement id);
            // anonymous ones always accumulate.
            let existing = (pid != 0)
                .then(|| layer.iter_mut().find(|p| p.img == id && p.pid == pid))
                .flatten();
            match existing {
                Some(p) => {
                    let key = p.key;
                    *p = Placement { key, ..place };
                    key
                }
                None => {
                    self.next_key += 1;
                    let key = self.next_key;
                    layer.push(Placement { key, ..place });
                    key
                }
            }
        };
        let _ = key;
        self.bump();
        let advance = if virt || cmd.num(b'C').unwrap_or(0) == 1 {
            None
        } else {
            Some((rows.saturating_sub(1), cols))
        };
        CmdResult {
            reply: Vec::new(),
            advance,
        }
    }

    fn cmd_delete(&mut self, cmd: &GfxCmd, cursor: (u16, u16)) -> CmdResult {
        let spec = cmd.ch(b'd').unwrap_or(b'a');
        let hard = spec.is_ascii_uppercase();
        let rows = self.rows;
        let total = self.total_scrolled;
        let in_alt = self.in_alt;
        let x = cmd.num(b'x').unwrap_or(0);
        let y = cmd.num(b'y').unwrap_or(0);
        let z = cmd.num(b'z').unwrap_or(0) as i32;
        let id = cmd.num(b'i').unwrap_or(0) as u32;
        let pid = cmd.num(b'p').unwrap_or(0) as u32;
        let number = cmd.num(b'I').map(|n| n as u32);
        let nid = number.and_then(|n| self.numbers.get(&n).copied());

        let srow = |p: &Placement| -> i64 {
            if in_alt {
                p.row
            } else {
                p.row - total
            }
        };
        // Placement rect intersects a cell / column / row (1-based args).
        let hits_cell = |p: &Placement, cx: i64, cy: i64| -> bool {
            let r = srow(p);
            cy > r
                && cy - 1 < r + p.rows as i64
                && cx > p.col as i64
                && cx - 1 < (p.col + p.cols) as i64
        };
        let on_screen = |p: &Placement| -> bool {
            let r = srow(p);
            r + (p.rows as i64) > 0 && r < rows as i64
        };

        let matcher: Box<dyn Fn(&Placement) -> bool> = match spec.to_ascii_lowercase() {
            b'a' => Box::new(on_screen),
            b'i' => Box::new(move |p| p.img == id && (pid == 0 || p.pid == pid)),
            b'n' => match nid {
                Some(nid) => Box::new(move |p| p.img == nid && (pid == 0 || p.pid == pid)),
                None => Box::new(|_| false),
            },
            b'c' => {
                let (cr, cc) = (cursor.0 as i64 + 1, cursor.1 as i64 + 1);
                Box::new(move |p| hits_cell(p, cc, cr))
            }
            b'p' => Box::new(move |p| hits_cell(p, x, y)),
            b'q' => Box::new(move |p| hits_cell(p, x, y) && p.z == z),
            b'x' => Box::new(move |p| x > p.col as i64 && x - 1 < (p.col + p.cols) as i64),
            b'y' => Box::new(move |p| {
                let r = srow(p);
                y > r && y - 1 < r + p.rows as i64
            }),
            b'z' => Box::new(move |p| p.z == z),
            b'r' => Box::new(move |p| (p.img as i64) >= x && (p.img as i64) <= y),
            b'f' => Box::new(|_| false), // animation frames: nothing to do
            _ => Box::new(|_| false),
        };

        let layer = if in_alt {
            &mut self.alt_placements
        } else {
            &mut self.placements
        };
        let mut affected: Vec<u32> = Vec::new();
        layer.retain(|p| {
            if matcher(p) {
                affected.push(p.img);
                false
            } else {
                true
            }
        });
        // Uppercase specifier also frees the image data.
        if hard {
            // d=I / d=R style direct forms free data even with no placement.
            match spec {
                b'I' => affected.push(id),
                b'N' => affected.extend(nid),
                b'R' => affected.extend(
                    self.images
                        .keys()
                        .copied()
                        .filter(|k| (*k as i64) >= x && (*k as i64) <= y),
                ),
                _ => {}
            }
            for img in affected.iter() {
                if let Some(i) = self.images.remove(img) {
                    self.store_bytes -=
                        i.data.len() + i.frames.iter().map(|f| f.data.len()).sum::<usize>();
                }
            }
        }
        self.pending = None; // any delete aborts an in-flight transmission
        if !affected.is_empty() || hard {
            self.bump();
        }
        CmdResult::default() // deletes get no response
    }

    /// Evict unplaced images (LRU) until `need` bytes are free or nothing
    /// evictable remains.
    fn evict(&mut self, need: usize) {
        let placed: std::collections::HashSet<u32> = self
            .placements
            .iter()
            .chain(self.alt_placements.iter())
            .map(|p| p.img)
            .collect();
        let mut candidates: Vec<(u64, u32, usize)> = self
            .images
            .iter()
            .filter(|(id, _)| !placed.contains(id))
            .map(|(id, img)| {
                let total = img.data.len() + img.frames.iter().map(|f| f.data.len()).sum::<usize>();
                (img.lru, *id, total)
            })
            .collect();
        candidates.sort_unstable();
        let mut freed = 0;
        for (_, id, len) in candidates {
            if freed >= need {
                break;
            }
            self.images.remove(&id);
            self.store_bytes -= len;
            freed += len;
        }
    }

    // -- media loading -------------------------------------------------------

    /// Fetch and validate payload data per the transmission medium.
    /// Returns (bytes, width, height).
    fn load_data(&mut self, cmd: &GfxCmd, payload: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
        let format = cmd.num(b'f').unwrap_or(32);
        let zlib = cmd.ch(b'o') == Some(b'z');
        let medium = cmd.ch(b't').unwrap_or(b'd');
        let data = match medium {
            b'd' => b64_decode(payload).ok_or("EINVAL:bad base64")?,
            b'f' | b't' | b's' => {
                let raw = b64_decode(payload).ok_or("EINVAL:bad base64")?;
                let path = String::from_utf8(raw).map_err(|_| "EINVAL:bad path")?;
                let path = if medium == b's' {
                    format!("/dev/shm/{}", path.trim_start_matches('/'))
                } else {
                    path
                };
                let meta = std::fs::metadata(&path).map_err(|_| "ENOENT:cannot read file")?;
                if !meta.is_file() {
                    return Err("EINVAL:not a regular file".into());
                }
                let bytes = std::fs::read(&path).map_err(|_| "ENOENT:cannot read file")?;
                let off = cmd.num(b'O').unwrap_or(0).max(0) as usize;
                let size = cmd.num(b'S').unwrap_or(0).max(0) as usize;
                let end = if size > 0 {
                    (off + size).min(bytes.len())
                } else {
                    bytes.len()
                };
                let bytes = bytes.get(off..end).unwrap_or(&[]).to_vec();
                if medium == b's' || (medium == b't' && temp_deletable(&path)) {
                    let _ = std::fs::remove_file(&path);
                }
                bytes
            }
            _ => return Err("EINVAL:unknown medium".into()),
        };
        match format {
            100 => {
                if zlib {
                    return Err("EINVAL:compressed png not supported".into());
                }
                let (w, h) = png_size(&data).ok_or("EINVAL:bad png")?;
                Ok((data, w, h))
            }
            24 | 32 => {
                let w = cmd.num(b's').unwrap_or(0).max(0) as u32;
                let h = cmd.num(b'v').unwrap_or(0).max(0) as u32;
                if w == 0 || h == 0 {
                    return Err("EINVAL:missing dimensions".into());
                }
                let bpp = if format == 24 { 3 } else { 4 };
                if !zlib && data.len() < (w as usize) * (h as usize) * bpp {
                    return Err("EINVAL:payload too small".into());
                }
                Ok((data, w, h))
            }
            _ => Err("EINVAL:unknown format".into()),
        }
    }

    // -- terminal events -----------------------------------------------------

    pub fn apply_event(&mut self, ev: TermEvent) {
        match ev {
            TermEvent::ScrollUp { top, bottom, n } => self.scroll(top, bottom, n as i64),
            TermEvent::ScrollDown { top, bottom, n } => self.scroll(top, bottom, -(n as i64)),
            TermEvent::EraseScreen => {
                let rows = self.rows as i64;
                let total = self.total_scrolled;
                let in_alt = self.in_alt;
                let before = self.layer_mut().len();
                if in_alt {
                    self.alt_placements.clear();
                } else {
                    self.placements.retain(|p| {
                        let r = p.row - total;
                        // keep placements fully inside scrollback
                        r + (p.rows as i64) <= 0 && r < rows
                    });
                }
                if self.layer_mut().len() != before {
                    self.bump();
                }
            }
            TermEvent::AltEnter => {
                self.in_alt = true;
                self.bump();
            }
            TermEvent::AltExit => {
                self.in_alt = false;
                self.alt_placements.clear();
                self.bump();
            }
            TermEvent::Resize { rows, cols } => {
                self.rows = rows;
                self.cols = cols;
                self.bump();
            }
            // OSC 52 (roadmap 4.7): stash for the server to gate behind
            // the permission inbox — the placement engine ignores it, but
            // it must not be dropped.
            TermEvent::Clipboard { selection, payload } => {
                self.clipboard.push((selection, payload));
            }
            TermEvent::Reset => {
                self.placements.clear();
                self.alt_placements.clear();
                self.images.clear();
                self.numbers.clear();
                self.store_bytes = 0;
                self.pending = None;
                self.in_alt = false;
                self.bump();
            }
        }
    }

    /// Move placements for a region scroll of `n` lines (positive = up).
    fn scroll(&mut self, top: u16, bottom: u16, n: i64) {
        if n == 0 {
            return;
        }
        let full = top == 0 && bottom == self.rows.saturating_sub(1);
        let total = self.total_scrolled;
        let in_alt = self.in_alt;
        let mut changed = false;
        if full && !in_alt && n > 0 {
            // Whole-screen scroll up: anchors keep their absolute line —
            // content (and images) move into scrollback together.
            self.total_scrolled += n;
            let cutoff = self.total_scrolled - SCROLLBACK_KEEP;
            let before = self.placements.len();
            self.placements.retain(|p| p.row >= cutoff);
            changed = true;
            let _ = before;
        } else {
            let (top, bottom) = (top as i64, bottom as i64);
            let layer = if in_alt {
                &mut self.alt_placements
            } else {
                &mut self.placements
            };
            layer.retain_mut(|p| {
                let r = if in_alt { p.row } else { p.row - total };
                if r < top || r > bottom {
                    return true; // outside the scrolled region
                }
                let nr = r - n;
                if nr < top || nr > bottom {
                    changed = true;
                    return false; // scrolled out of the region
                }
                p.row -= n;
                changed = true;
                true
            });
        }
        if changed {
            self.bump();
        }
    }

    // -- snapshot ------------------------------------------------------------

    /// Take the OSC 52 clipboard writes seen since the last call (4.7).
    pub fn drain_clipboard(&mut self) -> Vec<(Vec<u8>, Vec<u8>)> {
        std::mem::take(&mut self.clipboard)
    }

    pub fn snapshot(&self) -> GfxSnapshot {
        let layer = if self.in_alt {
            &self.alt_placements
        } else {
            &self.placements
        };
        let placements = layer
            .iter()
            .map(|p| VisPlacement {
                key: p.key,
                virt: p.virt,
                img: p.img,
                img_ver: p.img_ver,
                row: self.screen_row(p).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                col: p.col,
                src: p.src,
                cols: p.cols,
                rows: p.rows,
                z: p.z,
                offx: p.offx,
                offy: p.offy,
            })
            .collect();
        let mut images: Vec<(u32, u32)> = layer.iter().map(|p| (p.img, p.img_ver)).collect();
        images.sort_unstable();
        images.dedup();
        let anim = images
            .iter()
            .filter_map(|(id, _)| {
                let img = self.images.get(id)?;
                (!img.frames.is_empty()).then(|| SnapAnim {
                    img: *id,
                    gaps: img.frames.iter().map(|f| f.gap_ms).collect(),
                    running: img.anim_running,
                    loops: img.anim_loops,
                })
            })
            .collect();
        GfxSnapshot {
            placements,
            images,
            anim,
        }
    }
}

// ---------------------------------------------------------------------------
// Replies

fn echo_keys(cmd: &GfxCmd) -> String {
    let mut s = format!("i={}", cmd.num(b'i').unwrap_or(0));
    if let Some(n) = cmd.num(b'I') {
        s.push_str(&format!(",I={n}"));
    }
    if let Some(p) = cmd.num(b'p') {
        s.push_str(&format!(",p={p}"));
    }
    s
}

fn reply_ok(cmd: &GfxCmd) -> CmdResult {
    if cmd.quiet() >= 1 {
        return CmdResult::default();
    }
    CmdResult {
        reply: format!("\x1b_G{};OK\x1b\\", echo_keys(cmd)).into_bytes(),
        advance: None,
    }
}

fn reply_err(cmd: &GfxCmd, msg: &str) -> CmdResult {
    if cmd.quiet() >= 2 {
        return CmdResult::default();
    }
    CmdResult {
        reply: format!("\x1b_G{};{msg}\x1b\\", echo_keys(cmd)).into_bytes(),
        advance: None,
    }
}

// ---------------------------------------------------------------------------
// Helpers

/// The spec's rule for when a `t=t` temp file may be deleted after reading.
fn temp_deletable(path: &str) -> bool {
    if !path.contains("tty-graphics-protocol") {
        return false;
    }
    let tmp = std::env::var("TMPDIR").unwrap_or_default();
    path.starts_with("/tmp/")
        || path.starts_with("/dev/shm/")
        || (!tmp.is_empty() && path.starts_with(&tmp))
}

/// Width/height from a PNG IHDR header — no decode.
fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || &data[1..4] != b"PNG" || &data[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(data[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(data[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

pub fn b64_decode(s: &[u8]) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some((b - b'A') as u32),
            b'a'..=b'z' => Some((b - b'a' + 26) as u32),
            b'0'..=b'9' => Some((b - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s: Vec<u8> = s
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let s = match s.iter().position(|b| *b == b'=') {
        Some(p) => &s[..p],
        None => &s[..],
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut n: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            n |= val(*b)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
        if chunk.len() == 1 {
            return None;
        }
    }
    Some(out)
}

/// One splitter → term-engine → gfx pass over a chunk of pty output — the
/// single composition shared by SrvPane::process_output and the gfx tests
/// so the two can't silently diverge. `on_text` runs after each text
/// segment reaches the engine (SrvPane hooks its QueryScanner there) and
/// may append protocol replies. Returns the stripped stream to forward to
/// the UI (graphics removed, cursor advances synthesized) and the replies
/// owed to the child.
pub fn process_chunk(
    splitter: &mut GfxSplitter,
    term: &mut dyn crate::engine::TermEngine,
    engine: &mut GfxEngine,
    bytes: &[u8],
    mut on_text: impl FnMut(&[u8], &dyn crate::engine::TermScreen, &mut Vec<u8>),
) -> (Vec<u8>, Vec<u8>) {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut replies: Vec<u8> = Vec::new();
    for seg in splitter.split(bytes) {
        match seg {
            Seg::Text(t) => {
                term.process(&t);
                on_text(&t, term.screen(), &mut replies);
                for ev in term.drain_events() {
                    engine.apply_event(ev);
                }
                out.extend_from_slice(&t);
            }
            Seg::Cmd(cmd) => {
                let cursor = term.screen().cursor_position();
                let res = engine.handle(cmd, cursor);
                replies.extend(res.reply);
                if let Some((dr, dc)) = res.advance {
                    // Synthesize the cursor move a real kitty terminal
                    // performs after a placement, so every emulator of
                    // this stream agrees on the cursor.
                    let mut mv = String::new();
                    if dr > 0 {
                        mv.push_str(&format!("\x1b[{dr}B"));
                    }
                    if dc > 0 {
                        mv.push_str(&format!("\x1b[{dc}C"));
                    }
                    if !mv.is_empty() {
                        term.process(mv.as_bytes());
                        out.extend_from_slice(mv.as_bytes());
                    }
                }
            }
        }
    }
    (out, replies)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> GfxEngine {
        let mut e = GfxEngine::new(24, 80);
        e.active = true;
        e.cell = (10, 20);
        e
    }

    fn cmd(s: &str) -> GfxCmd {
        GfxCmd::parse(s.as_bytes())
    }

    fn rgba(w: usize, h: usize) -> String {
        crate::client::b64(&vec![0u8; w * h * 4])
    }

    /// 4.2: frames rejected below the animation floor, accepted at it;
    /// snapshot carries gaps + playback state; quota counts frame bytes.
    #[test]
    fn animation_frames_and_state() {
        let mut e = engine();
        let px = rgba(1, 1);
        // Root image.
        let r = e.handle(cmd(&format!("a=t,i=7,s=1,v=1,f=32;{px}")), (0, 0));
        assert!(r.reply.windows(2).any(|w| w == b"OK"));
        // Floor off: ENOTSUP, exactly as before.
        let r = e.handle(cmd(&format!("a=f,i=7,s=1,v=1,f=32;{px}")), (0, 0));
        assert!(String::from_utf8_lossy(&r.reply).contains("ENOTSUP"));
        // Floor on: frame stored with its gap.
        e.anim = true;
        let r = e.handle(cmd(&format!("a=f,i=7,s=1,v=1,f=32,z=120;{px}")), (0, 0));
        assert!(r.reply.windows(2).any(|w| w == b"OK"));
        let r = e.handle(cmd(&format!("a=f,i=7,s=1,v=1,f=32;{px}")), (0, 0));
        assert!(r.reply.windows(2).any(|w| w == b"OK"));
        assert_eq!(e.image(7).unwrap().frames.len(), 2);
        assert_eq!(e.image(7).unwrap().frames[0].gap_ms, 120);
        assert_eq!(e.image(7).unwrap().frames[1].gap_ms, 40);
        // Playback control + snapshot metadata (needs a placement).
        e.handle(cmd("a=p,i=7"), (2, 3));
        e.handle(cmd("a=a,i=7,s=3,v=2"), (0, 0));
        let snap = e.snapshot();
        assert_eq!(snap.anim.len(), 1);
        assert_eq!(snap.anim[0].img, 7);
        assert_eq!(snap.anim[0].gaps, vec![120, 40]);
        assert!(snap.anim[0].running);
        assert_eq!(snap.anim[0].loops, 2);
        // Stop.
        e.handle(cmd("a=a,i=7,s=1"), (0, 0));
        assert!(!e.snapshot().anim[0].running);
        // Retransmission drops frames (animation restarts from scratch).
        e.handle(cmd(&format!("a=t,i=7,s=1,v=1,f=32;{px}")), (0, 0));
        assert!(e.image(7).unwrap().frames.is_empty());
        assert!(e.snapshot().anim.is_empty());
    }

    /// 4.3: a U=1 placement is virtual — no cursor advance, flagged in
    /// the snapshot for clients to resolve from placeholder cells.
    #[test]
    fn unicode_placeholder_placement_is_virtual() {
        let mut e = engine();
        let px = rgba(20, 40);
        e.handle(cmd(&format!("a=t,i=9,s=2,v=2,f=32;{px}")), (0, 0));
        let r = e.handle(cmd("a=p,i=9,U=1"), (5, 5));
        assert!(
            r.advance.is_none(),
            "virtual placements never move the cursor"
        );
        let snap = e.snapshot();
        assert_eq!(snap.placements.len(), 1);
        assert!(snap.placements[0].virt);
        // A normal placement still advances (C=1 suppresses).
        let r = e.handle(cmd("a=p,i=9"), (5, 5));
        assert!(r.advance.is_some());
        assert!(!e.snapshot().placements.iter().all(|p| p.virt));
    }

    #[test]
    fn splitter_extracts_apc_and_keeps_text() {
        let mut sp = GfxSplitter::new();
        let segs = sp.split(b"hello\x1b_Ga=q,i=1,s=1,v=1;AAAA\x1b\\world");
        assert_eq!(segs.len(), 3);
        assert!(matches!(&segs[0], Seg::Text(t) if t == b"hello"));
        assert!(matches!(&segs[1], Seg::Cmd(_)));
        assert!(matches!(&segs[2], Seg::Text(t) if t == b"world"));
    }

    #[test]
    fn splitter_joins_across_chunks() {
        let mut sp = GfxSplitter::new();
        let a = sp.split(b"x\x1b_Ga=q,i=1,s=1,v=1;AA");
        assert_eq!(a.len(), 1); // just "x"
        let b = sp.split(b"AA\x1b\\y");
        assert_eq!(b.len(), 2);
        assert!(matches!(&b[0], Seg::Cmd(_)));
        assert!(matches!(&b[1], Seg::Text(t) if t == b"y"));
    }

    #[test]
    fn splitter_carry_split_at_esc_boundary() {
        let mut sp = GfxSplitter::new();
        assert!(sp
            .split(b"ab\x1b")
            .iter()
            .all(|s| matches!(s, Seg::Text(t) if t == b"ab")));
        let segs = sp.split(b"_Ga=q,i=5,s=1,v=1;AAAA\x1b\\");
        assert!(matches!(&segs[0], Seg::Cmd(c) if c.num(b'i') == Some(5)));
    }

    #[test]
    fn splitter_passes_non_graphics_apc() {
        let mut sp = GfxSplitter::new();
        let segs = sp.split(b"\x1b_Xother\x1b\\tail");
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0], Seg::Text(t) if t.starts_with(b"\x1b_X")));
    }

    #[test]
    fn query_ok_and_error() {
        let mut e = engine();
        let px = rgba(1, 1);
        let r = e.handle(cmd(&format!("a=q,i=31,s=1,v=1,f=32;{px}")), (0, 0));
        assert_eq!(r.reply, b"\x1b_Gi=31;OK\x1b\\");
        assert!(e.image(31).is_none()); // queries don't store
        let r = e.handle(cmd("a=q,i=32,s=1,v=1;!!"), (0, 0));
        assert!(String::from_utf8_lossy(&r.reply).contains("EINVAL"));
    }

    #[test]
    fn transmit_display_places_and_advances_cursor() {
        let mut e = engine();
        let px = rgba(20, 40); // 2 cols x 2 rows at 10x20 cell
        let r = e.handle(cmd(&format!("a=T,i=7,s=20,v=40,f=32;{px}")), (3, 5));
        assert_eq!(r.advance, Some((1, 2)));
        let snap = e.snapshot();
        assert_eq!(snap.placements.len(), 1);
        let p = &snap.placements[0];
        assert_eq!((p.row, p.col, p.cols, p.rows), (3, 5, 2, 2));
    }

    #[test]
    fn chunked_transmission_assembles() {
        let mut e = engine();
        let px = rgba(1, 1); // "AAAAAA==" (8 chars)
        let (a, b) = px.split_at(4);
        let r = e.handle(cmd(&format!("a=t,i=9,s=1,v=1,f=32,m=1;{a}")), (0, 0));
        assert!(r.reply.is_empty());
        let r = e.handle(cmd(&format!("m=0;{b}")), (0, 0));
        assert_eq!(r.reply, b"\x1b_Gi=9;OK\x1b\\");
        assert!(e.image(9).is_some());
    }

    #[test]
    fn full_screen_scroll_moves_into_scrollback() {
        let mut e = engine();
        let px = rgba(10, 20);
        e.handle(cmd(&format!("a=T,i=1,s=10,v=20,f=32;{px}")), (10, 0));
        for _ in 0..12 {
            e.apply_event(TermEvent::ScrollUp {
                top: 0,
                bottom: 23,
                n: 1,
            });
        }
        let p = &e.snapshot().placements[0];
        assert_eq!(p.row, -2); // 2 lines into scrollback, still alive
    }

    #[test]
    fn region_scroll_moves_and_clips() {
        let mut e = engine();
        let px = rgba(10, 20);
        e.handle(cmd(&format!("a=T,i=1,s=10,v=20,f=32;{px}")), (5, 0));
        // region 3..=10 scrolls up 1: anchor 5 -> 4
        e.apply_event(TermEvent::ScrollUp {
            top: 3,
            bottom: 10,
            n: 1,
        });
        assert_eq!(e.snapshot().placements[0].row, 4);
        // outside-region placements don't move
        e.apply_event(TermEvent::ScrollUp {
            top: 8,
            bottom: 10,
            n: 3,
        });
        assert_eq!(e.snapshot().placements[0].row, 4);
        // scrolling it past the region top deletes it
        e.apply_event(TermEvent::ScrollUp {
            top: 3,
            bottom: 10,
            n: 2,
        });
        assert!(e.snapshot().placements.is_empty());
    }

    #[test]
    fn erase_screen_spares_scrollback() {
        let mut e = engine();
        let px = rgba(10, 20);
        e.handle(cmd(&format!("a=T,i=1,s=10,v=20,f=32;{px}")), (0, 0));
        e.handle(cmd(&format!("a=T,i=2,s=10,v=20,f=32;{px}")), (20, 0));
        for _ in 0..5 {
            e.apply_event(TermEvent::ScrollUp {
                top: 0,
                bottom: 23,
                n: 1,
            });
        }
        // image 1 now fully in scrollback (row -5+1<=0), image 2 on screen
        e.apply_event(TermEvent::EraseScreen);
        let snap = e.snapshot();
        assert_eq!(snap.placements.len(), 1);
        assert_eq!(snap.placements[0].img, 1);
    }

    #[test]
    fn alt_screen_layers_are_separate() {
        let mut e = engine();
        let px = rgba(10, 20);
        e.handle(cmd(&format!("a=T,i=1,s=10,v=20,f=32;{px}")), (2, 2));
        e.apply_event(TermEvent::AltEnter);
        assert!(e.snapshot().placements.is_empty());
        e.handle(cmd(&format!("a=T,i=2,s=10,v=20,f=32;{px}")), (0, 0));
        assert_eq!(e.snapshot().placements.len(), 1);
        e.apply_event(TermEvent::AltExit);
        let snap = e.snapshot();
        assert_eq!(snap.placements.len(), 1);
        assert_eq!(snap.placements[0].img, 1);
    }

    #[test]
    fn delete_matrix_by_id_and_position() {
        let mut e = engine();
        let px = rgba(10, 20);
        e.handle(cmd(&format!("a=T,i=1,p=1,s=10,v=20,f=32;{px}")), (0, 0));
        e.handle(cmd(&format!("a=T,i=1,p=2,s=10,v=20,f=32;{px}")), (5, 5));
        e.handle(cmd(&format!("a=T,i=2,s=10,v=20,f=32;{px}")), (9, 9));
        // by id+placement
        e.handle(cmd("a=d,d=i,i=1,p=1"), (0, 0));
        assert_eq!(e.snapshot().placements.len(), 2);
        // by cell intersection (1-based): (10,6) hits the i=1,p=2 placement
        e.handle(cmd("a=d,d=p,x=6,y=6"), (0, 0));
        assert_eq!(e.snapshot().placements.len(), 1);
        // uppercase frees data too
        e.handle(cmd("a=d,d=I,i=2"), (0, 0));
        assert!(e.snapshot().placements.is_empty());
        assert!(e.image(2).is_none());
        assert!(e.image(1).is_some()); // lowercase deletes left data alone
    }

    #[test]
    fn identified_placement_replaces_anonymous_accumulates() {
        let mut e = engine();
        let px = rgba(10, 20);
        e.handle(cmd(&format!("a=t,i=4,s=10,v=20,f=32;{px}")), (0, 0));
        e.handle(cmd("a=p,i=4,p=9"), (1, 1));
        e.handle(cmd("a=p,i=4,p=9"), (2, 2)); // replaces
        e.handle(cmd("a=p,i=4"), (3, 3)); // anonymous
        e.handle(cmd("a=p,i=4"), (4, 4)); // anonymous
        assert_eq!(e.snapshot().placements.len(), 3);
    }

    #[test]
    fn number_allocation_replies_with_id() {
        let mut e = engine();
        let px = rgba(1, 1);
        let r = e.handle(cmd(&format!("a=t,I=42,s=1,v=1,f=32;{px}")), (0, 0));
        let reply = String::from_utf8_lossy(&r.reply).into_owned();
        assert!(reply.contains(",I=42;OK"));
        let r = e.handle(cmd("a=p,I=42"), (0, 0));
        assert!(String::from_utf8_lossy(&r.reply).contains("OK"));
    }

    #[test]
    fn png_header_parsed() {
        // minimal PNG header: signature + IHDR for 3x2
        let mut png = vec![0x89];
        png.extend_from_slice(b"PNG\r\n\x1a\n");
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&3u32.to_be_bytes());
        png.extend_from_slice(&2u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_size(&png), Some((3, 2)));
    }

    #[test]
    fn b64_roundtrip() {
        for len in [0usize, 1, 2, 3, 4, 5, 300] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 256) as u8).collect();
            let enc = crate::client::b64(&data);
            assert_eq!(b64_decode(enc.as_bytes()).unwrap(), data, "len {len}");
        }
    }

    /// The same composition SrvPane::process_output runs: splitter →
    /// vt100 (events) → engine, producing the stripped stream. Delegates
    /// to the shared process_chunk so it cannot diverge from the real path.
    struct PaneSim {
        splitter: GfxSplitter,
        term: crate::engine::Vt100Engine,
        engine: GfxEngine,
    }

    impl PaneSim {
        fn new(rows: u16, cols: u16) -> Self {
            let mut engine = GfxEngine::new(rows, cols);
            engine.active = true;
            engine.cell = (10, 20);
            Self {
                splitter: GfxSplitter::new(),
                term: crate::engine::Vt100Engine::new(rows, cols, 100),
                engine,
            }
        }

        fn feed(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
            process_chunk(
                &mut self.splitter,
                &mut self.term,
                &mut self.engine,
                bytes,
                |_, _, _| {},
            )
        }
    }

    /// Corpus replay: the real timg kitty-graphics recording through the
    /// shared pipeline, engine state snapshotted against a JSON golden.
    /// UPDATE_GOLDEN=1 re-baselines (REBASELINE: note required).
    #[test]
    fn corpus_timg_kitty_gfx_golden() {
        use crate::corpus;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let file = root.join("tests/corpus/timg-kitty.ptyrec");
        let rec = corpus::read(std::fs::File::open(file).unwrap()).unwrap();
        let mut sim = PaneSim::new(rec.rows, rec.cols);
        let mut stripped = 0usize;
        for chunk in &rec.chunks {
            if chunk.kind == corpus::ChunkKind::Output {
                let (out, _) = sim.feed(&chunk.payload);
                assert!(
                    !out.windows(2).any(|w| w == b"\x1b_"),
                    "graphics APC leaked into the forwarded stream"
                );
                stripped += chunk.payload.len().saturating_sub(out.len());
            }
        }
        assert!(stripped > 0, "recording contained no graphics to strip");
        let json = serde_json::to_string_pretty(&sim.engine.snapshot()).unwrap();
        let path = root.join("tests/golden/timg-kitty.gfx.json");
        if std::env::var_os("UPDATE_GOLDEN").is_some() || !path.exists() {
            let update = std::env::var_os("UPDATE_GOLDEN").is_some();
            std::fs::write(&path, &json).unwrap();
            assert!(
                update,
                "golden {} did not exist; wrote it — review and commit, then re-run",
                path.display()
            );
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            json,
            "gfx snapshot drifted from golden (UPDATE_GOLDEN=1 re-baselines)"
        );
    }

    #[test]
    fn end_to_end_strip_anchor_and_scroll() {
        let mut sim = PaneSim::new(5, 40);
        let px = rgba(20, 40); // 2x2 cells
                               // two lines of text, then an inline image, then more text
        let (out, replies) = sim
            .feed(format!("one\r\ntwo \x1b_Ga=T,i=3,s=20,v=40,f=32;{px}\x1b\\done\r\n").as_bytes());
        // graphics stripped from the forwarded stream, cursor move injected
        let s = String::from_utf8_lossy(&out);
        assert!(!s.contains("\x1b_G"), "graphics bytes leaked: {s:?}");
        assert!(
            s.contains("\x1b[1B\x1b[2C"),
            "cursor advance missing: {s:?}"
        );
        assert_eq!(replies, b"\x1b_Gi=3;OK\x1b\\");
        // anchored at the cursor cell (row 1, col 4)
        let snap = sim.engine.snapshot();
        assert_eq!((snap.placements[0].row, snap.placements[0].col), (1, 4));
        // "done" landed after the synthesized advance (row 2, col 6+)
        assert!(crate::engine::TermEngine::screen(&sim.term)
            .contents()
            .contains("done"));
        // stream 5 more lines: the image scrolls into scrollback with text
        sim.feed(b"a\r\nb\r\nc\r\nd\r\ne\r\nf");
        let snap = sim.engine.snapshot();
        assert!(
            snap.placements[0].row < 0,
            "row: {}",
            snap.placements[0].row
        );
        // and the text the image was anchored to agrees: scrollback holds it
        assert_eq!(snap.placements.len(), 1);
    }

    #[test]
    fn end_to_end_alt_screen_app_cleans_up() {
        let mut sim = PaneSim::new(5, 40);
        let px = rgba(10, 20);
        sim.feed(format!("\x1b_Ga=T,i=1,s=10,v=20,f=32,C=1;{px}\x1b\\").as_bytes());
        // a fullscreen app opens, shows an image, then exits
        sim.feed(b"\x1b[?1049h");
        sim.feed(format!("\x1b_Ga=T,i=2,s=10,v=20,f=32,C=1;{px}\x1b\\").as_bytes());
        assert_eq!(sim.engine.snapshot().placements[0].img, 2);
        sim.feed(b"\x1b[?1049l");
        let snap = sim.engine.snapshot();
        assert_eq!(snap.placements.len(), 1);
        assert_eq!(snap.placements[0].img, 1); // main-screen image restored
    }

    #[test]
    fn inactive_engine_stays_silent() {
        let mut e = GfxEngine::new(24, 80);
        e.cell = (10, 20);
        let px = rgba(1, 1);
        let r = e.handle(cmd(&format!("a=q,i=1,s=1,v=1;{px}")), (0, 0));
        assert!(r.reply.is_empty());
    }
}
