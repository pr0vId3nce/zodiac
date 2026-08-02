use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::Result;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::gfx::{GfxEngine, GfxSplitter, Seg};
use crate::query::QueryScanner;
use crate::server::SrvEvent;

/// Max raw output kept per pane, replayed to clients on attach and persisted
/// across reboots as restored scrollback.
pub const RING_CAP: usize = 512 * 1024;

pub struct SrvPane {
    pub id: u64,
    pub name: String,
    pub ring: Vec<u8>,
    parser: vt100::Parser, // bell/status tracking + graphics event source
    splitter: GfxSplitter,
    /// Kitty-graphics state for this pane (images + placements).
    pub gfx: GfxEngine,
    /// `gfx.version` as of the last snapshot pushed to the UI client.
    pub gfx_pushed: u64,
    queries: QueryScanner,
    bell_count: usize,
    pub last_output: Option<Instant>,
    pub activity: bool,
    pub attention: bool,
    pub auto_resume: bool,
    /// `status()` as of the last finish-tick, for working→done edge detection.
    pub prev_status: &'static str,
    created_at: Instant,
    stall_since: Option<Instant>,
    stall_fired: Option<Instant>,
    stall_latched: bool,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: Option<u32>,
    size: (u16, u16),
    /// Outer terminal cell size in px — reported to the inner PTY so apps
    /// compute image geometry that maps 1:1 onto the outer terminal.
    cell: (u16, u16),
}

impl SrvPane {
    pub fn spawn(
        id: u64,
        name: Option<String>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        preload: Vec<u8>,
        tx: Sender<SrvEvent>,
    ) -> Result<Self> {
        let rows = rows.max(2);
        let cols = cols.max(10);
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let name =
            name.unwrap_or_else(|| shell.rsplit('/').next().unwrap_or("shell").to_string());
        let mut cmd = CommandBuilder::new(&shell);
        // Login shell: rebuilds env/prompt (starship etc.) from profile files
        // even if the long-lived server's inherited env has gone stale.
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(dir) = cwd
            .filter(|d| d.is_dir())
            .or_else(|| std::env::current_dir().ok())
        {
            cmd.cwd(dir);
        }
        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let pid = child.process_id();

        {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = child.wait();
                let _ = tx.send(SrvEvent::Exited(id));
            });
        }

        let mut reader = pair.master.try_clone_reader()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(SrvEvent::Output(id, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let writer = pair.master.take_writer()?;
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.enable_events();
        Ok(Self {
            id,
            name,
            ring: preload,
            parser,
            splitter: GfxSplitter::new(),
            gfx: GfxEngine::new(rows, cols),
            gfx_pushed: 0,
            queries: QueryScanner::new(),
            bell_count: 0,
            last_output: None,
            activity: false,
            attention: false,
            auto_resume: true,
            prev_status: "idle",
            created_at: Instant::now(),
            stall_since: None,
            stall_fired: None,
            stall_latched: false,
            master: pair.master,
            writer,
            killer,
            pid,
            size: (rows, cols),
            cell: (0, 0),
        })
    }

    /// Feed pty output through the graphics splitter into the ring buffer,
    /// emulator, query scanner, and graphics engine. Returns the processed
    /// stream to forward to the UI (graphics stripped, cursor advances
    /// synthesized) and whether the bell rang in this chunk.
    pub fn process_output(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut replies: Vec<u8> = Vec::new();
        for seg in self.splitter.split(bytes) {
            match seg {
                Seg::Text(t) => {
                    self.parser.process(&t);
                    replies.extend(self.queries.scan(&t, self.parser.screen(), self.cell));
                    for ev in self.parser.drain_events() {
                        self.gfx.apply_event(ev);
                    }
                    out.extend_from_slice(&t);
                }
                Seg::Cmd(cmd) => {
                    let cursor = self.parser.screen().cursor_position();
                    let res = self.gfx.handle(cmd, cursor);
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
                            self.parser.process(mv.as_bytes());
                            out.extend_from_slice(mv.as_bytes());
                        }
                    }
                }
            }
        }
        self.ring.extend_from_slice(&out);
        if self.ring.len() > RING_CAP {
            let cut = self.ring.len() - RING_CAP;
            self.ring.drain(..cut);
        }
        if !replies.is_empty() {
            self.write_input(&replies);
        }
        self.last_output = Some(Instant::now());
        let count = self.parser.screen().audible_bell_count();
        let new = count > self.bell_count;
        self.bell_count = count;
        (out, new)
    }

    /// Outer-terminal cell size (px) learned from the attached client:
    /// reported to the inner PTY (SIGWINCH) and used for image geometry.
    pub fn set_cell(&mut self, cell: (u16, u16)) {
        if self.cell == cell || cell.0 == 0 || cell.1 == 0 {
            return;
        }
        self.cell = cell;
        self.gfx.cell = cell;
        let (rows, cols) = self.size;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: cols * cell.0,
            pixel_height: rows * cell.1,
        });
    }

    pub fn title(&self) -> String {
        self.parser.screen().title().to_string()
    }

    pub fn screen_text(&self) -> String {
        self.parser.screen().contents()
    }

    /// Semantic agent status, herdr-style. A braille title frame means
    /// working, but "✳" proves nothing: Claude Code's title spinner cycles
    /// ✳/⠂/⠐/… while working and merely rests on ✳ when idle — so every
    /// non-braille title falls through to output recency (safe for TUI
    /// agents: their spinners keep emitting output while working). Recency
    /// only counts for panes running a known agent, though — an ordinary
    /// TUI (htop, a music player…) emits output forever and would otherwise
    /// read as permanently working.
    pub fn status(&self) -> &'static str {
        use crate::protocol::{title_state, TitleState};
        if self.attention {
            return "needs_input";
        }
        let recent = self
            .last_output
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(5));
        if title_state(&self.title()) == TitleState::Working
            || (recent && self.agent().is_some())
        {
            "working"
        } else if self.activity {
            "done"
        } else {
            "idle"
        }
    }

    fn stall_match(&self, conn_watch: bool) -> Option<Duration> {
        stall_match(self.parser.screen(), conn_watch)
    }

    /// Which agent runs in this pane: title patterns first, then a /proc
    /// walk over the shell's descendants for known agent binaries.
    pub fn agent(&self) -> Option<String> {
        if let Some(a) = crate::protocol::agent_from_title(&self.title()) {
            return Some(a.to_string());
        }
        self.pid.and_then(detect_agent_process)
    }

    /// API-stall watchdog: true when this pane should be auto-resumed now.
    /// Call roughly once per second. A stall phrase must sit in the bottom
    /// rows of the screen for its dwell time (the "Waiting" phrase also shows
    /// briefly on healthy requests, hence its long dwell), the pane must be
    /// running claude, and after firing the phrase must clear from the screen
    /// before it can trigger again — except a long-persisting phrase retries,
    /// in case the first Esc/--resume didn't take.
    pub fn stall_due(&mut self, conn_watch: bool) -> bool {
        if !self.auto_resume {
            self.stall_since = None;
            self.stall_latched = false;
            return false;
        }
        let Some(dwell) = self.stall_match(conn_watch) else {
            self.stall_since = None;
            self.stall_latched = false;
            return false;
        };
        let since = *self.stall_since.get_or_insert_with(Instant::now);
        if self.stall_latched {
            if since.elapsed() < STALL_RETRY {
                return false;
            }
        } else if since.elapsed() < dwell
            || self.stall_fired.is_some_and(|t| t.elapsed() < STALL_COOLDOWN)
        {
            return false;
        }
        if self.agent().as_deref() != Some("claude") {
            return false;
        }
        self.stall_since = Some(Instant::now());
        self.stall_fired = Some(Instant::now());
        self.stall_latched = true;
        true
    }

    /// Interrupt (Esc), clear the input box (Ctrl+U), and submit `--resume`
    /// — recovery for Claude Code's "Response stalled mid-stream" API error.
    /// The clear matters on retries: an earlier `--resume` that never
    /// submitted would otherwise still sit in the input box and the new text
    /// would append to it ("--resume--resume"). The pauses keep each step in
    /// a separate read so the TUI treats them as keystrokes, not one paste —
    /// paced on a helper thread that routes each step back through the
    /// server's event queue, so the ~800 ms sequence doesn't freeze every
    /// pane's I/O. If the pane dies mid-sequence the Deliver events find no
    /// pane and are dropped.
    pub fn fire_autoresume(&self, tx: Sender<SrvEvent>) {
        let id = self.id;
        std::thread::spawn(move || {
            let steps: &[(&[u8], u64)] = &[
                (b"\x1b", 300),
                (b"\x15", 250),
                (b"--resume", 250),
                (b"\r", 0),
            ];
            for (bytes, pause) in steps {
                if tx.send(SrvEvent::Deliver(id, bytes.to_vec())).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(*pause));
            }
        });
    }

    pub fn cwd(&self) -> Option<String> {
        let pid = self.pid?;
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    pub fn last_ms(&self) -> Option<u64> {
        self.last_output.map(|t| t.elapsed().as_millis() as u64)
    }

    pub fn uptime_ms(&self) -> u64 {
        self.created_at.elapsed().as_millis() as u64
    }

    /// Claude Code's live status spinner ("✳ Thinking… (esc to interrupt)")
    /// is on screen: a bottom row led by a spinner glyph with the
    /// "esc to interrupt" suffix. Same visual signature the stall watchdog
    /// uses for the waiting phrase, minus the phrase.
    pub fn thinking(&self) -> bool {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        for r in rows.saturating_sub(STALL_ROWS)..rows {
            let mut text = String::new();
            for c in 0..cols {
                if let Some(cell) = screen.cell(r, c) {
                    for ch in cell.contents().chars() {
                        if !ch.is_whitespace() {
                            text.push(ch);
                        }
                    }
                }
            }
            if text.contains("esctointerrupt")
                && text.chars().next().is_some_and(is_spinner_char)
            {
                return true;
            }
        }
        false
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows < 2 || cols < 10 || self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: cols * self.cell.0,
            pixel_height: rows * self.cell.1,
        });
        self.parser.set_size(rows, cols);
        for ev in self.parser.drain_events() {
            self.gfx.apply_event(ev);
        }
    }

    pub fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn clear_flags(&mut self) {
        self.activity = false;
        self.attention = false;
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }
}

/// Stall phrases (whitespace stripped, so line wrapping can't split a match)
/// and how long each must stay on screen before the watchdog intervenes.
// Phrases are whitespace-stripped (line wrap can't hide them). The waiting
// phrase shows briefly on healthy requests too, hence its long dwell.
const STALL_ERR: &str = "APIError:Responsestalledmid-stream";
const STALL_ERR_DWELL: Duration = Duration::from_secs(6);
const STALL_WAIT: &str = "WaitingforAPIresponse";
const STALL_WAIT_DWELL: Duration = Duration::from_secs(30);
/// "Connection closed mid-response" is never transient — resume immediately.
/// Gated by the global `connection_watch` setting (settings page, default on).
const STALL_CONN: &str = "APIError:Connectionclosedmid-response";
const STALL_CONN_DWELL: Duration = Duration::ZERO;

/// Look for a genuine stall status line in the bottom rows, returning its
/// dwell time. Merely *quoting* the phrases in conversation text (discussing
/// the watchdog with claude…) must not match, so a row only counts when it
/// also carries the visual signature of the real UI line: the API-error
/// phrase must start the row (short decoration prefix like "⎿" allowed) and
/// be painted in an error color; the waiting phrase must sit on a spinner
/// status line — leading spinner glyph and the live "esc to interrupt"
/// suffix in the same row.
fn stall_match(screen: &vt100::Screen, conn_watch: bool) -> Option<Duration> {
    let (rows, cols) = screen.size();
    for r in rows.saturating_sub(STALL_ROWS)..rows {
        let mut text = String::new();
        let mut col_of: Vec<u16> = Vec::new();
        for c in 0..cols {
            let Some(cell) = screen.cell(r, c) else { continue };
            for ch in cell.contents().chars() {
                if !ch.is_whitespace() {
                    text.push(ch);
                    col_of.push(c);
                }
            }
        }
        // Both API-error phrases share the same visual signature check.
        let err_at = |phrase: &str| -> bool {
            let Some(byte_pos) = text.find(phrase) else {
                return false;
            };
            let pos = text[..byte_pos].chars().count();
            pos <= 4
                && screen
                    .cell(r, col_of[pos])
                    .is_some_and(|cell| is_error_color(cell.fgcolor()))
        };
        if err_at(STALL_ERR) {
            return Some(STALL_ERR_DWELL);
        }
        if conn_watch && err_at(STALL_CONN) {
            return Some(STALL_CONN_DWELL);
        }
        if text.contains(STALL_WAIT)
            && text.contains("esctointerrupt")
            && text.chars().next().is_some_and(is_spinner_char)
        {
            return Some(STALL_WAIT_DWELL);
        }
    }
    None
}

/// Red-ish foreground — Claude Code paints its API error lines in an error
/// color, while conversation text (even inline code) is not red.
fn is_error_color(c: vt100::Color) -> bool {
    match c {
        vt100::Color::Idx(i) => matches!(i, 1 | 9) || (160..=203).contains(&i),
        vt100::Color::Rgb(r, g, b) => {
            r > 120 && i32::from(r) - i32::from(g) > 50 && i32::from(r) - i32::from(b) > 50
        }
        vt100::Color::Default => false,
    }
}

/// Claude Code's status-line spinner frames: ✳ or a braille pattern.
fn is_spinner_char(c: char) -> bool {
    c == '✳' || ('\u{2800}'..='\u{28ff}').contains(&c)
}
/// Only the bottom rows are scanned — that's where Claude Code renders the
/// error/status area; old conversation text scrolled above doesn't count.
const STALL_ROWS: u16 = 15;
const STALL_COOLDOWN: Duration = Duration::from_secs(30);
/// If the phrase never leaves the screen after an intervention, try again.
const STALL_RETRY: Duration = Duration::from_secs(90);

const AGENT_BINARIES: &[&str] = &["claude", "opencode", "codex", "aider", "gemini", "goose"];

/// Find a known agent binary among the descendants of `root` by walking
/// /proc. Matches the basename of argv[0] or argv[1] (argv[1] catches
/// interpreter-run agents like `node .../bin/claude`).
fn detect_agent_process(root: u32) -> Option<String> {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // field 4 (ppid) sits after the parenthesized comm, which may
        // itself contain spaces/parens — split after the last ')'.
        let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            continue;
        };
        if let Some(ppid) = rest.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        if pid != root {
            if let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) {
                for arg in cmdline.split(|b| *b == 0).take(2) {
                    let base = String::from_utf8_lossy(arg);
                    let base = base.rsplit('/').next().unwrap_or_default();
                    if let Some(hit) = AGENT_BINARIES.iter().find(|a| **a == base) {
                        return Some(hit.to_string());
                    }
                }
            }
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> vt100::Parser {
        let mut p = vt100::Parser::new(24, 100, 0);
        // Real panes fill from the bottom (the watchdog only watches the
        // bottom rows); scroll the cursor down before printing.
        for _ in 0..24 {
            p.process(b"\r\n");
        }
        for l in lines {
            p.process(l.as_bytes());
            p.process(b"\r\n");
        }
        p
    }

    #[test]
    fn genuine_red_api_error_matches() {
        let p = feed(&["some output", "  \x1b[31m⎿ API Error: Response stalled mid-stream. The response above may be incomplete.\x1b[m"]);
        assert_eq!(stall_match(p.screen(), true), Some(STALL_ERR_DWELL));
    }

    #[test]
    fn quoted_api_error_in_plain_text_ignored() {
        let p = feed(&[
            "the watchdog looks for \"API Error: Response stalled mid-stream\" on screen",
            "- API Error: Response stalled mid-stream. The response above may be incomplete.",
        ]);
        assert_eq!(stall_match(p.screen(), true), None);
    }

    #[test]
    fn quoted_api_error_mid_row_even_colored_ignored() {
        let p = feed(&["it prints \x1b[36mAPI Error: Response stalled mid-stream\x1b[m sometimes"]);
        assert_eq!(stall_match(p.screen(), true), None);
    }

    #[test]
    fn genuine_waiting_spinner_line_matches() {
        let p = feed(&["✳ Waiting for API response… (32s · esc to interrupt)"]);
        assert_eq!(stall_match(p.screen(), true), Some(STALL_WAIT_DWELL));
    }

    #[test]
    fn quoted_waiting_phrase_ignored() {
        let p = feed(&[
            "\"Waiting for API response\" acts only after 30s",
            "Waiting for API response is the other phrase",
        ]);
        assert_eq!(stall_match(p.screen(), true), None);
    }

    #[test]
    fn waiting_without_esc_suffix_ignored() {
        let p = feed(&["✳ Waiting for API response…"]);
        assert_eq!(stall_match(p.screen(), true), None);
    }

    #[test]
    fn genuine_connection_closed_matches_immediately() {
        let p = feed(&["some output", "\x1b[31m● API Error: Connection closed mid-response. The response above may be incomplete.\x1b[m"]);
        assert_eq!(stall_match(p.screen(), true), Some(STALL_CONN_DWELL));
    }

    #[test]
    fn connection_closed_ignored_when_toggle_off() {
        let p = feed(&["\x1b[31m● API Error: Connection closed mid-response. The response above may be incomplete.\x1b[m"]);
        assert_eq!(stall_match(p.screen(), false), None);
    }

    #[test]
    fn quoted_connection_closed_plain_text_ignored() {
        let p = feed(&[
            "it watches for \"API Error: Connection closed mid-response\" on screen",
            "- API Error: Connection closed mid-response. The response above may be incomplete.",
        ]);
        assert_eq!(stall_match(p.screen(), true), None);
    }

    #[test]
    fn error_scrolled_above_watch_window_ignored() {
        let mut lines = vec!["\x1b[31mAPI Error: Response stalled mid-stream\x1b[m"];
        lines.extend(std::iter::repeat("filler").take(20));
        let p = feed(&lines);
        assert_eq!(stall_match(p.screen(), true), None);
    }
}
