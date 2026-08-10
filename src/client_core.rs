//! Client-side core, shared between frontends (roadmap 3.1).
//!
//! Everything a client needs that does not touch ratatui/crossterm-TUI
//! rendering lives here: connection setup to the session socket, the
//! per-pane client-side terminal state (vt100 parser wrapper + pane
//! bookkeeping), agent transcript state, gfx snapshot tracking (T_GFX_IMG
//! chunk reassembly), and input encoding. The TUI client (`client.rs`)
//! consumes these via `use`; a future GUI client shares them without
//! depending on ratatui. Frame *dispatch* (which frame mutates what) stays
//! in each frontend — it is entangled with focus, notifications, and quit
//! handling — but every per-pane mutation target is defined here.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::protocol::{socket_path, state_dir, GfxImgHdr, PermRequest};

pub const CLIENT_SCROLLBACK: usize = 10_000;

/// A pane image mirrored from the server, plus the id it was transmitted
/// to the outer terminal under (None until first needed on screen).
pub struct CImg {
    pub ver: u32,
    pub format: u8,
    pub zlib: bool,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
    pub outer: Option<u32>,
}

/// Transcript entry roles for an agent pane (`kind == "agent"`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ARole {
    User,
    Assistant,
    Tool,
    Error,
}

/// Client-side view state for one agent pane: the parsed transcript, the
/// streaming tail, pending permission requests, and the prompt editor.
#[derive(Default)]
pub struct AgentUi {
    pub log: Vec<(ARole, String)>,
    /// Assistant text still streaming in (shown at the transcript tail
    /// until the completed block replaces it).
    pub stream: String,
    /// A thinking block is open — shown as a dim marker, never as text.
    pub thinking: bool,
    /// Wrapped-line offset from the bottom; 0 = follow the tail.
    pub scroll: usize,
    pub perms: Vec<PermRequest>,
    pub input: String,
    pub cursor: usize, // char index into `input`
}

impl AgentUi {
    /// Fold one agent-native NDJSON line (ADR 0002) into the transcript.
    /// Handles both claude stream-json and pi rpc shapes; unknown types
    /// are ignored — the event stream only ever grows.
    pub fn apply_line(&mut self, v: &serde_json::Value) {
        let s = |v: &serde_json::Value, k: &str| -> Option<String> {
            v.get(k).and_then(|x| x.as_str()).map(str::to_string)
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("zodiac_user") => {
                if let Some(t) = s(v, "text") {
                    self.log.push((ARole::User, t));
                }
            }
            Some("assistant") => {
                let blocks = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array());
                for b in blocks.into_iter().flatten() {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = s(b, "text") {
                                self.log.push((ARole::Assistant, t));
                            }
                            // The completed block replaces the partial.
                            self.stream.clear();
                            self.thinking = false;
                        }
                        Some("tool_use") => {
                            let name = s(b, "name").unwrap_or_else(|| "tool".into());
                            let arg = b.get("input").map(tool_compact).unwrap_or_default();
                            self.log.push((ARole::Tool, format!("{name}({arg})")));
                        }
                        _ => {}
                    }
                }
            }
            Some("stream_event") => {
                let Some(ev) = v.get("event") else { return };
                match ev.get("type").and_then(|t| t.as_str()) {
                    Some("content_block_start") => {
                        self.thinking = ev
                            .get("content_block")
                            .and_then(|b| b.get("type"))
                            .and_then(|t| t.as_str())
                            == Some("thinking");
                    }
                    Some("content_block_delta") => {
                        let Some(d) = ev.get("delta") else { return };
                        match d.get("type").and_then(|t| t.as_str()) {
                            Some("text_delta") => {
                                if let Some(t) = d.get("text").and_then(|t| t.as_str()) {
                                    self.stream.push_str(t);
                                }
                                self.thinking = false;
                            }
                            Some("thinking_delta") => self.thinking = true,
                            _ => {}
                        }
                    }
                    Some("message_stop") => {
                        self.stream.clear();
                        self.thinking = false;
                    }
                    _ => {}
                }
            }
            Some("result") => {
                if v.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
                    let msg = s(v, "result").unwrap_or_else(|| "turn failed".into());
                    self.log.push((ARole::Error, msg));
                }
                self.stream.clear();
                self.thinking = false;
            }
            Some("zodiac_perm_resolved") => {
                if let Some(rid) = s(v, "request_id") {
                    self.perms.retain(|p| p.request_id != rid);
                }
            }
            // pi rpc shapes (ADR 0002).
            Some("message_end") => {
                let Some(m) = v.get("message") else { return };
                if m.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                    return;
                }
                for b in m
                    .get("content")
                    .and_then(|c| c.as_array())
                    .into_iter()
                    .flatten()
                {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = s(b, "text") {
                            self.log.push((ARole::Assistant, t));
                        }
                    }
                }
                self.stream.clear();
                self.thinking = false;
            }
            Some("message_update") => {
                let Some(ev) = v.get("assistantMessageEvent") else {
                    return;
                };
                if ev.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                    if let Some(t) = ev.get("delta").and_then(|d| d.as_str()) {
                        self.stream.push_str(t);
                    }
                }
            }
            Some("turn_end") | Some("agent_end") => {
                self.stream.clear();
                self.thinking = false;
            }
            _ => {}
        }
    }
}

/// A tool call's first input value, compacted to one short run for the
/// transcript's "⏺ Tool(…)" line — `Bash({"command":"ls"})` reads as
/// `Bash(ls)`.
pub fn tool_compact(input: &serde_json::Value) -> String {
    let first = match input {
        serde_json::Value::Object(m) => match m.values().next() {
            Some(v) => v,
            None => return String::new(),
        },
        v => v,
    };
    let text = match first {
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    truncate(&text.replace(['\n', '\r'], " "), 48)
}

pub struct CPane {
    pub id: u64,
    pub name: String,
    /// "pty" or "agent" — mirrors `PaneState.kind`. Synced from T_STATE and
    /// inferred from agent frames arriving for the pane, whichever is first.
    pub kind: String,
    /// Per-pane kitty-keyboard kill switch (roadmap 4.4): when set, this
    /// client encodes legacy keys for the pane regardless of its flag
    /// stack. Client-local; toggled from the UI.
    pub kitty_kill: bool,
    /// Transcript/permission/input state; only meaningful for agent panes.
    pub agent: AgentUi,
    pub parser: vt100::Parser,
    pub scroll: usize,
    pub last_output: Option<Instant>,
    /// When the title last showed a braille (working) spinner frame — lets
    /// the ✳ rest frames mid-work read as working without fresh output
    /// alone doing so (see `working()`).
    pub last_title_working: Option<Instant>,
    /// Output-rate history for the card sparkline: bytes per 50s bucket,
    /// newest last, ~10 minutes deep. Client-side only — it starts fresh
    /// on attach.
    pub rate: std::collections::VecDeque<u32>,
    pub rate_cur: u32,
    pub rate_bucket_start: Option<Instant>,
    /// Sparkline image version + content hash — a changed history gets a
    /// fresh image id so the terminal-side cache never shows stale bars.
    pub spark_ver: u32,
    pub spark_hash: u64,
    pub activity: bool,
    pub attention: bool,
    pub bell_count: usize,
    pub size: (u16, u16),
    /// Latest graphics snapshot from the server (placements + live images).
    pub gfx: crate::gfx::GfxSnapshot,
    pub images: std::collections::HashMap<u32, CImg>,
    /// Chunked T_GFX_IMG payloads still assembling.
    pub partial: std::collections::HashMap<u32, Vec<u8>>,
}

impl CPane {
    pub fn new(id: u64, name: String, rows: u16, cols: u16) -> Self {
        let rows = rows.max(2);
        let cols = cols.max(10);
        Self {
            id,
            name,
            kind: "pty".into(),
            kitty_kill: false,
            agent: AgentUi::default(),
            parser: vt100::Parser::new(rows, cols, CLIENT_SCROLLBACK),
            scroll: 0,
            last_output: None,
            last_title_working: None,
            rate: std::collections::VecDeque::new(),
            rate_cur: 0,
            rate_bucket_start: None,
            spark_ver: 0,
            spark_hash: 0,
            activity: false,
            attention: false,
            bell_count: 0,
            size: (rows, cols),
            gfx: crate::gfx::GfxSnapshot::default(),
            images: std::collections::HashMap::new(),
            partial: std::collections::HashMap::new(),
        }
    }

    pub fn is_agent(&self) -> bool {
        self.kind == "agent"
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows < 2 || cols < 10 || self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        self.parser.set_size(rows, cols);
    }

    /// Roll the sparkline's 50s buckets forward to now, filling quiet
    /// stretches with zeros. Called on output and before each render.
    pub fn rate_tick(&mut self) {
        const BUCKET: Duration = Duration::from_secs(50);
        const DEPTH: usize = 12; // ~10 minutes
        let now = Instant::now();
        let start = *self.rate_bucket_start.get_or_insert(now);
        let mut elapsed = now.duration_since(start);
        while elapsed >= BUCKET {
            self.rate.push_back(self.rate_cur);
            self.rate_cur = 0;
            if self.rate.len() > DEPTH {
                self.rate.pop_front();
            }
            self.rate_bucket_start = Some(self.rate_bucket_start.unwrap() + BUCKET);
            elapsed -= BUCKET;
        }
    }

    pub fn poll_bell(&mut self) -> bool {
        let count = self.parser.screen().audible_bell_count();
        let new = count > self.bell_count;
        self.bell_count = count;
        new
    }

    pub fn clear_flags(&mut self) {
        self.activity = false;
        self.attention = false;
        let _ = self.poll_bell();
    }

    pub fn set_scroll(&mut self, offset: usize) {
        self.scroll = offset;
        self.parser.set_scrollback(offset);
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let new = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            (self.scroll + delta as usize).min(CLIENT_SCROLLBACK)
        };
        self.set_scroll(new);
    }

    /// Fold a `T_GFX_STATE` snapshot in: adopt it and drop mirrored images
    /// the server no longer lists, pushing the outer-terminal ids they were
    /// transmitted under onto `dead` for the frontend to free.
    pub fn apply_gfx_state(&mut self, snap: crate::gfx::GfxSnapshot, dead: &mut Vec<u32>) {
        let live: std::collections::HashSet<(u32, u32)> = snap.images.iter().copied().collect();
        self.images.retain(|id, img| {
            let keep = live.contains(&(*id, img.ver));
            if !keep {
                dead.extend(img.outer);
            }
            keep
        });
        self.gfx = snap;
    }

    /// Fold one `T_GFX_IMG` chunk into the reassembly buffer; a completed
    /// image replaces the mirrored copy, pushing an obsoleted outer-terminal
    /// id onto `dead` for the frontend to free.
    pub fn apply_gfx_chunk(&mut self, hdr: &GfxImgHdr, chunk: &[u8], dead: &mut Vec<u32>) {
        let buf = self.partial.entry(hdr.img).or_default();
        if hdr.off == 0 {
            buf.clear();
        }
        buf.extend_from_slice(chunk);
        if buf.len() as u32 >= hdr.total {
            let data = std::mem::take(buf);
            self.partial.remove(&hdr.img);
            // a retransmitted image obsoletes its outer copy
            if let Some(old) = self.images.get(&hdr.img).and_then(|i| i.outer) {
                dead.push(old);
            }
            self.images.insert(
                hdr.img,
                CImg {
                    ver: hdr.ver,
                    format: hdr.format,
                    zlib: hdr.zlib,
                    w: hdr.w,
                    h: hdr.h,
                    data,
                    outer: None,
                },
            );
        }
    }
}

pub fn connect_or_spawn(session: &str) -> Result<UnixStream> {
    connect_or_spawn_with(session, std::env::current_exe()?)
}

/// Like [`connect_or_spawn`], but spawns `server_exe --server <session>` to
/// start the server when none is listening. The TUI passes its own exe (it
/// *is* the server binary); the GUI passes the sibling `zodiac` binary,
/// because `zodiac-gui` is a client only and cannot serve — spawning itself
/// would recurse on the `--server` argument.
pub fn connect_or_spawn_with(session: &str, server_exe: std::path::PathBuf) -> Result<UnixStream> {
    let path = socket_path(session);
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    let exe = server_exe;
    let logdir = state_dir(session);
    std::fs::create_dir_all(&logdir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logdir.join("server.log"))?;
    let log2 = log.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--server")
        .arg(session)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log2);
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(s) = UnixStream::connect(&path) {
            return Ok(s);
        }
    }
    bail!(
        "zodiac server did not start (see {})",
        logdir.join("server.log").display()
    )
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Encodes a key event into the byte sequence a real terminal would send.
pub fn encode_key(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let mods = key.modifiers;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);

    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl
    let modp = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi_mod = |ch: char| -> Vec<u8> {
        if modp == 1 {
            if app_cursor && matches!(ch, 'A' | 'B' | 'C' | 'D' | 'H' | 'F') {
                format!("\x1bO{ch}").into_bytes()
            } else {
                format!("\x1b[{ch}").into_bytes()
            }
        } else {
            format!("\x1b[1;{modp}{ch}").into_bytes()
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> {
        if modp == 1 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{modp}~").into_bytes()
        }
    };

    let bytes = match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    ch @ 'a'..='z' => ch as u8 - b'a' + 1,
                    ' ' | '@' => 0,
                    '[' => 27,
                    '\\' => 28,
                    ']' => 29,
                    '^' => 30,
                    '_' | '/' => 31,
                    _ => {
                        let mut b = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                        return Some(out);
                    }
                };
                out.push(b);
            } else {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            out
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if alt {
                vec![0x1b, 0x7f]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi_mod('A'),
        KeyCode::Down => csi_mod('B'),
        KeyCode::Right => csi_mod('C'),
        KeyCode::Left => csi_mod('D'),
        KeyCode::Home => csi_mod('H'),
        KeyCode::End => csi_mod('F'),
        KeyCode::PageUp => csi_tilde(5),
        KeyCode::PageDown => csi_tilde(6),
        KeyCode::Insert => csi_tilde(2),
        KeyCode::Delete => csi_tilde(3),
        KeyCode::F(n @ 1..=4) => {
            let ch = (b'P' + n - 1) as char;
            if modp == 1 {
                format!("\x1bO{ch}").into_bytes()
            } else {
                format!("\x1b[1;{modp}{ch}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            csi_tilde(code)
        }
        _ => return None,
    };
    Some(bytes)
}

/// Encode a mouse event for the inner application, honoring the mouse
/// protocol mode and encoding it requested via DECSET. `x`/`y` are 0-based
/// pane-relative cells. Returns None when the mode doesn't report this kind
/// of event (including mode None — mouse reporting off).
pub fn encode_mouse(
    ev: &crossterm::event::MouseEvent,
    x: u16,
    y: u16,
    mode: vt100::MouseProtocolMode,
    enc: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    use vt100::MouseProtocolMode as M;

    let allowed = match mode {
        M::None => false,
        M::Press => matches!(ev.kind, K::Down(_) | K::ScrollUp | K::ScrollDown),
        M::PressRelease => !matches!(ev.kind, K::Drag(_) | K::Moved),
        M::ButtonMotion => !matches!(ev.kind, K::Moved),
        M::AnyMotion => true,
    };
    if !allowed {
        return None;
    }
    let btn = |b: MouseButton| match b {
        MouseButton::Left => 0u16,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let base = match ev.kind {
        K::Down(b) | K::Up(b) => btn(b),
        K::Drag(b) => btn(b) + 32,
        K::Moved => 35,
        K::ScrollUp => 64,
        K::ScrollDown => 65,
        K::ScrollLeft => 66,
        K::ScrollRight => 67,
    };
    let mut mods = 0u16;
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        mods += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        mods += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        mods += 16;
    }
    let release = matches!(ev.kind, K::Up(_));
    Some(match enc {
        vt100::MouseProtocolEncoding::Sgr => {
            let fin = if release { 'm' } else { 'M' };
            format!("\x1b[<{};{};{}{fin}", base + mods, x + 1, y + 1).into_bytes()
        }
        // Default/Utf8: single-byte fields; release loses button identity.
        _ => {
            let cb = if release { 3 + mods } else { base + mods };
            let coord = |v: u16| (32 + v + 1).min(255) as u8;
            vec![
                0x1b,
                b'[',
                b'M',
                (32 + cb).min(255) as u8,
                coord(x),
                coord(y),
            ]
        }
    })
}

/// Kitty keyboard protocol synthesis (roadmap 4.4), for panes whose flag
/// stack enables it (bit 0b1 = disambiguate escape codes). Returns None
/// when the legacy encoder should handle the event instead — plain
/// printable text stays text, so only ambiguous combinations change on
/// the wire. The payoff: Ctrl+Shift+P and Ctrl+P become distinct.
pub fn encode_key_kitty(key: &crossterm::event::KeyEvent, flags: u8) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if flags & 0b1 == 0 {
        return None;
    }
    let mods = key.modifiers;
    let shift = mods.contains(KeyModifiers::SHIFT);
    let alt = mods.contains(KeyModifiers::ALT);
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let sup = mods.contains(KeyModifiers::SUPER);
    let modp = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8 + 8 * sup as u8;
    let csi_u = |cp: u32, modp: u8| -> Vec<u8> {
        if modp == 1 {
            format!("\x1b[{cp}u").into_bytes()
        } else {
            format!("\x1b[{cp};{modp}u").into_bytes()
        }
    };
    match key.code {
        // Esc is always CSI-u under disambiguation — that is the point.
        KeyCode::Esc => Some(csi_u(27, modp)),
        // Enter / Tab / Backspace change encoding only when modified
        // beyond what their legacy bytes can carry.
        KeyCode::Enter if modp > 1 => Some(csi_u(13, modp)),
        KeyCode::Tab if modp > 1 && !(modp == 2 && shift) => Some(csi_u(9, modp)),
        KeyCode::Backspace if modp > 1 => Some(csi_u(127, modp)),
        // Character keys: ctrl/alt/super combinations emit the lowercase
        // codepoint + modifiers, making shift visible; unmodified (or
        // shift-only) text stays plain text via the legacy encoder.
        KeyCode::Char(c) if ctrl || sup || (alt && !c.is_ascii()) => {
            let cp = c.to_ascii_lowercase() as u32;
            Some(csi_u(cp, modp))
        }
        _ => None,
    }
}

#[cfg(test)]
mod kitty_kbd_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    /// The Phase 4 exit criterion: Ctrl+Shift+P and Ctrl+P must be
    /// distinct on the wire under disambiguation.
    #[test]
    fn ctrl_shift_p_differs_from_ctrl_p() {
        let ctrl_p = encode_key_kitty(&key(KeyCode::Char('p'), KeyModifiers::CONTROL), 1).unwrap();
        let ctrl_shift_p = encode_key_kitty(
            &key(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            1,
        )
        .unwrap();
        assert_eq!(ctrl_p, b"\x1b[112;5u");
        assert_eq!(ctrl_shift_p, b"\x1b[112;6u");
        assert_ne!(ctrl_p, ctrl_shift_p);
    }

    #[test]
    fn plain_text_stays_legacy() {
        assert!(encode_key_kitty(&key(KeyCode::Char('a'), KeyModifiers::NONE), 1).is_none());
        assert!(encode_key_kitty(&key(KeyCode::Char('A'), KeyModifiers::SHIFT), 1).is_none());
        // Flags 0 = protocol off entirely.
        assert!(encode_key_kitty(&key(KeyCode::Esc, KeyModifiers::NONE), 0).is_none());
    }

    #[test]
    fn esc_and_modified_specials() {
        assert_eq!(
            encode_key_kitty(&key(KeyCode::Esc, KeyModifiers::NONE), 1).unwrap(),
            b"\x1b[27u"
        );
        assert_eq!(
            encode_key_kitty(&key(KeyCode::Enter, KeyModifiers::CONTROL), 1).unwrap(),
            b"\x1b[13;5u"
        );
        // Plain Enter stays legacy \r.
        assert!(encode_key_kitty(&key(KeyCode::Enter, KeyModifiers::NONE), 1).is_none());
    }
}
