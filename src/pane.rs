use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::Result;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::engine::TermEngine;
use crate::gfx::{GfxEngine, GfxSplitter};
use crate::query::QueryScanner;
use crate::server::SrvEvent;

/// Max raw output kept per pane, replayed to clients on attach and persisted
/// across reboots as restored scrollback. Sized in raw pty bytes, and those
/// are mostly churn for an agent pane: ~70% of a busy claude ring is
/// repaint escapes (spinner/status frames), so scrollback depth is set by
/// how many newline-scrolls fit the window — at 2 MiB that was roughly one
/// long reply. 6 MiB reaches several replies back and still fits a single
/// T_REPLAY under the protocol's 8 MiB frame cap with room to spare.
pub const RING_CAP: usize = 6 * 1024 * 1024;

/// DECSET 2026 coalescing: max buffered bytes and max hold time before the
/// safety valves flush anyway (matching the ~150 ms grace real terminals
/// give a stuck synchronized update).
const SYNC_BUF_MAX: usize = 2 * 1024 * 1024;
const SYNC_DEADLINE: Duration = Duration::from_millis(150);

/// What sits behind a pane: a PTY child (shell/TUI, the default) or a
/// structured agent process on pipes (roadmap Phase 2, ADR 0002).
pub enum PaneIo {
    Pty {
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        killer: Box<dyn ChildKiller + Send + Sync>,
        pid: Option<u32>,
    },
    Agent(crate::agent::AgentRuntime),
}

pub struct SrvPane {
    pub id: u64,
    pub name: String,
    /// The user pinned this name (Alt+R / `zodiac rename`) — auto-naming
    /// leaves it alone. An empty Alt+R rename clears the pin.
    pub renamed: bool,
    /// Auto-naming's cache of the agent's model, keyed by the session
    /// transcript's mtime (see `claude_model`).
    model_mtime: Option<std::time::SystemTime>,
    model_name: Option<String>,
    pub ring: Vec<u8>,
    term: crate::engine::ActiveEngine, // bell/status tracking + graphics event source
    splitter: GfxSplitter,
    /// Kitty-graphics state for this pane (images + placements).
    pub gfx: GfxEngine,
    /// `gfx.version` as of the last snapshot pushed to the UI client.
    pub gfx_pushed: u64,
    queries: QueryScanner,
    bell_count: usize,
    pub last_output: Option<Instant>,
    /// When the title last showed a braille (working) spinner frame — lets
    /// ✳ rest frames mid-work read as working without fresh output alone
    /// doing so (see `status()`).
    last_title_working: Option<Instant>,
    /// Process-walk memos: (when probed, result). `agent()`/`ssh_target()`
    /// run several times per pane per state broadcast, and each fresh walk
    /// reads the whole process table — the tree doesn't change fast enough
    /// to deserve that. RefCell: panes live on the single event-loop thread.
    agent_memo: std::cell::RefCell<Option<(Instant, Option<String>)>>,
    ssh_memo: std::cell::RefCell<Option<(Instant, Option<String>)>>,
    /// When a spinner status line was last seen on screen — how a pi pane
    /// reads as working (see `pi_working`).
    busy_memo: std::cell::RefCell<Option<Instant>>,
    /// Which transcript file this pane's session is, memoized like
    /// `agent_memo`: resolving it walks the process table for the agent's
    /// argv, and both the model name and the recap ask once a second. Keyed
    /// by agent and cwd so swapping either invalidates it at once, rather
    /// than describing the previous occupant for a second.
    #[allow(clippy::type_complexity)]
    transcript_memo: std::cell::RefCell<
        Option<(
            Instant,
            (String, String),
            Option<(PathBuf, std::time::SystemTime)>,
        )>,
    >,
    /// Transcript-derived recap, keyed by the transcript's mtime (see
    /// `pi_recap`).
    recap_mtime: std::cell::RefCell<Option<std::time::SystemTime>>,
    recap_cache: std::cell::RefCell<Option<String>>,
    pub activity: bool,
    pub attention: bool,
    pub auto_resume: bool,
    /// `status()` as of the last finish-tick, for working→done edge detection.
    pub prev_status: &'static str,
    created_at: Instant,
    stall_since: Option<Instant>,
    stall_fired: Option<Instant>,
    stall_latched: bool,
    /// Background pane-monitor bookkeeping (see `monitor.rs`) — purely
    /// server-side tracking, not part of the wire protocol.
    /// DECSET 2026: output buffered while the child holds a synchronized
    /// update open, flushed as one T_OUTPUT at ?2026l or the deadline.
    sync_buf: Vec<u8>,
    sync_since: Option<Instant>,
    pub monitor_screen_hash: Option<u64>,
    pub monitor_screen_since: Option<Instant>,
    pub monitor_checked_at: Option<Instant>,
    pub monitor_last_reason: Option<String>,
    /// Tier 3.1 card subtitle bookkeeping — same idea, separate cadence and
    /// cache key from the monitor above (a subtitle refreshes on any screen
    /// change, not just stalls).
    pub subtitle: Option<String>,
    pub subtitle_hash: Option<u64>,
    pub subtitle_checked_at: Option<Instant>,
    io: PaneIo,
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
        let name = name.unwrap_or_else(|| shell.rsplit('/').next().unwrap_or("shell").to_string());
        let mut cmd = CommandBuilder::new(&shell);
        // Login shell: rebuilds env/prompt (starship etc.) from profile files
        // even if the long-lived server's inherited env has gone stale.
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // A zodiac server started from inside a claude session would pass
        // claude's session markers into every pane — and a claude launched
        // in such a pane then thinks it's a child session and stops saving
        // transcripts (which also breaks model naming and Alt+⇧R resume).
        // Panes are top-level terminals; scrub the inherited markers.
        cmd.env_remove("CLAUDECODE");
        cmd.env_remove("CLAUDE_CODE_CHILD_SESSION");
        cmd.env_remove("CLAUDE_CODE_SSE_PORT");
        cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
        // Same for pi: it marks child processes with PI_CODING_AGENT and
        // hands its bash tool the live session's id/file/model. Inherited
        // into a pane, those describe a *different* conversation — a pi
        // started here would report its parent's session when asked.
        cmd.env_remove("PI_CODING_AGENT");
        cmd.env_remove("PI_SESSION_ID");
        cmd.env_remove("PI_SESSION_FILE");
        cmd.env_remove("PI_PROVIDER");
        cmd.env_remove("PI_MODEL");
        cmd.env_remove("PI_REASONING_LEVEL");
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
        let term = crate::engine::ActiveEngine::new(rows, cols, 0);
        Ok(Self {
            id,
            name,
            renamed: false,
            model_mtime: None,
            model_name: None,
            ring: preload,
            term,
            splitter: GfxSplitter::new(),
            gfx: GfxEngine::new(rows, cols),
            gfx_pushed: 0,
            queries: QueryScanner::new(),
            bell_count: 0,
            last_output: None,
            last_title_working: None,
            agent_memo: std::cell::RefCell::new(None),
            ssh_memo: std::cell::RefCell::new(None),
            busy_memo: std::cell::RefCell::new(None),
            transcript_memo: std::cell::RefCell::new(None),
            recap_mtime: std::cell::RefCell::new(None),
            recap_cache: std::cell::RefCell::new(None),
            activity: false,
            attention: false,
            auto_resume: true,
            prev_status: "idle",
            created_at: Instant::now(),
            stall_since: None,
            stall_fired: None,
            stall_latched: false,
            sync_buf: Vec::new(),
            sync_since: None,
            monitor_screen_hash: None,
            monitor_screen_since: None,
            monitor_checked_at: None,
            monitor_last_reason: None,
            subtitle: None,
            subtitle_hash: None,
            subtitle_checked_at: None,
            io: PaneIo::Pty {
                master: pair.master,
                writer,
                killer,
                pid,
            },
            size: (rows, cols),
            cell: (0, 0),
        })
    }

    /// Spawn a structured agent pane (roadmap 2.2, ADR 0002): the agent
    /// process on pipes, its NDJSON relayed as `T_AGENT_EVENT`. The VT
    /// engine fields stay allocated (tiny) but idle — no bytes ever reach
    /// them.
    pub fn spawn_agent(
        id: u64,
        name: Option<String>,
        kind: crate::agent::AgentKind,
        cwd: Option<PathBuf>,
        tx: Sender<SrvEvent>,
    ) -> Result<Self> {
        let mut rt = crate::agent::AgentRuntime::new(
            kind,
            cwd.filter(|d| d.is_dir())
                .or_else(|| std::env::current_dir().ok()),
        );
        rt.spawn_process(id, &tx)?;
        Ok(Self {
            id,
            name: name.unwrap_or_else(|| kind.name().to_string()),
            renamed: false,
            model_mtime: None,
            model_name: None,
            ring: Vec::new(),
            term: crate::engine::ActiveEngine::new(2, 10, 0),
            splitter: GfxSplitter::new(),
            gfx: GfxEngine::new(2, 10),
            gfx_pushed: 0,
            queries: QueryScanner::new(),
            bell_count: 0,
            last_output: None,
            last_title_working: None,
            agent_memo: std::cell::RefCell::new(None),
            ssh_memo: std::cell::RefCell::new(None),
            busy_memo: std::cell::RefCell::new(None),
            transcript_memo: std::cell::RefCell::new(None),
            recap_mtime: std::cell::RefCell::new(None),
            recap_cache: std::cell::RefCell::new(None),
            activity: false,
            attention: false,
            auto_resume: true,
            prev_status: "idle",
            created_at: Instant::now(),
            stall_since: None,
            stall_fired: None,
            stall_latched: false,
            sync_buf: Vec::new(),
            sync_since: None,
            monitor_screen_hash: None,
            monitor_screen_since: None,
            monitor_checked_at: None,
            monitor_last_reason: None,
            subtitle: None,
            subtitle_hash: None,
            subtitle_checked_at: None,
            io: PaneIo::Agent(rt),
            size: (2, 10),
            cell: (0, 0),
        })
    }

    /// Feed pty output through the graphics splitter into the ring buffer,
    /// emulator, query scanner, and graphics engine. Returns the processed
    /// stream to forward to the UI (graphics stripped, cursor advances
    /// synthesized) and whether the bell rang in this chunk.
    pub fn process_output(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        let queries = &mut self.queries;
        let cell = self.cell;
        let (out, replies) = crate::gfx::process_chunk(
            &mut self.splitter,
            &mut self.term,
            &mut self.gfx,
            bytes,
            |t, screen, replies| replies.extend(queries.scan(t, screen, cell)),
        );
        // Child-side DECSET 2026 (roadmap 1.6): while the child holds a
        // synchronized update open, hold its output back so every attached
        // client repaints once, at the closing ?2026l (or the deadline /
        // size safety valves — a child that never closes must not freeze
        // or bloat the pane). The ring is appended on flush so T_REPLAY
        // and live T_OUTPUT stay ordered identically.
        let out = if self.term.screen().synchronized_update() {
            self.sync_buf.extend_from_slice(&out);
            self.sync_since.get_or_insert_with(Instant::now);
            if self.sync_buf.len() > SYNC_BUF_MAX {
                self.take_sync_buf()
            } else {
                Vec::new()
            }
        } else if !self.sync_buf.is_empty() {
            self.sync_buf.extend_from_slice(&out);
            self.take_sync_buf()
        } else {
            out
        };
        self.ring_extend(&out);
        if !replies.is_empty() {
            self.write_input(&replies);
        }
        self.last_output = Some(Instant::now());
        if crate::protocol::title_state(&self.title()) == crate::protocol::TitleState::Working {
            self.last_title_working = Some(Instant::now());
        }
        let count = self.term.screen().audible_bell_count();
        let new = count > self.bell_count;
        self.bell_count = count;
        (out, new)
    }

    fn ring_extend(&mut self, out: &[u8]) {
        if out.is_empty() {
            return;
        }
        self.ring.extend_from_slice(out);
        // Trim with hysteresis: draining to CAP on every 8 KiB read is a
        // ~2 MiB memmove per chunk once full (~250× write amplification
        // during a big cat). Letting it overshoot by a block first makes
        // the copy amortized-rare; replay consumers only ever want "about
        // the last RING_CAP bytes" anyway.
        const RING_SLACK: usize = 256 * 1024;
        if self.ring.len() > RING_CAP + RING_SLACK {
            let mut cut = self.ring.len() - RING_CAP;
            // Nudge the cut to just past the next newline so a replayed or
            // restored ring doesn't begin mid-escape-sequence — the client
            // parser eats stray parameter bytes and misrenders the first
            // line otherwise.
            if let Some(nl) = self.ring[cut..cut + 4096.min(self.ring.len() - cut)]
                .iter()
                .position(|&b| b == b'\n')
            {
                cut += nl + 1;
            }
            self.ring.drain(..cut);
        }
    }

    fn take_sync_buf(&mut self) -> Vec<u8> {
        self.sync_since = None;
        std::mem::take(&mut self.sync_buf)
    }

    /// A synchronized update is being held open; the server shortens its
    /// event-loop timeout while any pane reports true.
    pub fn sync_pending(&self) -> bool {
        self.sync_since.is_some()
    }

    /// Deadline valve for 1.6: a child that opened ?2026h but hasn't closed
    /// it within SYNC_DEADLINE gets its buffered output flushed anyway.
    pub fn sync_flush_due(&mut self) -> Option<Vec<u8>> {
        if self.sync_since?.elapsed() < SYNC_DEADLINE {
            return None;
        }
        let out = self.take_sync_buf();
        self.ring_extend(&out);
        Some(out)
    }

    /// Outer-terminal cell size (px) learned from the attached client:
    /// reported to the inner PTY (SIGWINCH) and used for image geometry.
    pub fn set_cell(&mut self, cell: (u16, u16)) {
        // Cells arrive off the wire; a malformed frame must not overflow
        // the u16 pixel products below (a debug-build panic). 100 px per
        // cell dwarfs any real font.
        let cell = (cell.0.min(100), cell.1.min(100));
        if self.cell == cell || cell.0 == 0 || cell.1 == 0 {
            return;
        }
        self.cell = cell;
        self.gfx.cell = cell;
        let (rows, cols) = self.size;
        if let PaneIo::Pty { master, .. } = &self.io {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: cols.saturating_mul(cell.0),
                pixel_height: rows.saturating_mul(cell.1),
            });
        }
    }

    /// Wire value for `PaneState.kind`.
    pub fn kind_str(&self) -> &'static str {
        match &self.io {
            PaneIo::Pty { .. } => "pty",
            PaneIo::Agent(_) => "agent",
        }
    }

    pub fn title(&self) -> String {
        match &self.io {
            PaneIo::Pty { .. } => self.term.screen().title(),
            PaneIo::Agent(rt) => {
                if rt.working {
                    format!("⠿ {}", rt.kind.name())
                } else {
                    rt.kind.name().to_string()
                }
            }
        }
    }

    pub fn screen_text(&self) -> String {
        self.term.screen().contents()
    }

    /// The tail of the current screen as plain text, trailing blank rows
    /// dropped — a compact excerpt for the background classifier. `None` if
    /// the tail is blank. The server-side parser keeps no scrollback, so
    /// this only ever sees what's currently visible.
    pub fn tail_text(&self, want: usize) -> Option<String> {
        let rows: Vec<String> = self
            .term
            .screen()
            .rows_text()
            .iter()
            .map(|r| r.trim_end().to_string())
            .collect();
        let end = rows
            .iter()
            .rposition(|r| !r.is_empty())
            .map_or(0, |i| i + 1);
        let start = end.saturating_sub(want);
        let text = rows[start..end].join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// Cheap fingerprint of the current screen, for noticing a "working"
    /// pane whose output has actually gone quiet (stalled or looping).
    pub fn screen_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.screen_text().hash(&mut h);
        h.finish()
    }

    /// Semantic agent status, herdr-style. A braille title frame means
    /// working; "✳" is ambiguous: Claude Code's title spinner cycles
    /// ✳/⠂/⠐/… while working and merely rests on ✳ when idle. Mid-work rest
    /// frames are bridged by a braille frame seen moments ago — fresh output
    /// alone doesn't count, or the pane reads "working" for seconds after
    /// the answer's final render (or any idle repaint). Titles carrying no
    /// state still fall back to output recency (safe for TUI agents: their
    /// spinners keep emitting output while working), but never for unknown
    /// programs — an ordinary TUI (htop, a music player…) emits output
    /// forever and would otherwise read as permanently working.
    pub fn status(&self) -> &'static str {
        use crate::protocol::{title_state, TitleState};
        // Agent panes (2.6): status derives from structured events — no
        // titles, spinners, or output recency involved.
        if let PaneIo::Agent(rt) = &self.io {
            return if !rt.pending.is_empty() {
                "needs_input"
            } else if rt.working {
                "working"
            } else if self.activity {
                "done"
            } else {
                "idle"
            };
        }
        if self.attention {
            return "needs_input";
        }
        let recent = self
            .last_output
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(5));
        let working = match title_state(&self.title()) {
            TitleState::Working => true,
            TitleState::Idle => {
                recent
                    && self
                        .last_title_working
                        .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2))
            }
            TitleState::Unknown => match self.agent().as_deref() {
                Some("pi") => self.pi_working(),
                Some(_) => recent,
                None => false,
            },
        };
        if working {
            "working"
        } else if self.activity {
            "done"
        } else {
            "idle"
        }
    }

    fn stall_match(&self, conn_watch: bool) -> Option<Duration> {
        stall_match(self.term.screen(), conn_watch)
    }

    /// Which agent runs in this pane: title patterns first (cheap, always
    /// fresh), then a process-tree walk memoized for a couple of seconds.
    pub fn agent(&self) -> Option<String> {
        if let PaneIo::Agent(rt) = &self.io {
            return Some(rt.kind.name().to_string());
        }
        if let Some(a) = crate::protocol::agent_from_title(&self.title()) {
            return Some(a.to_string());
        }
        if let Some((t, v)) = self.agent_memo.borrow().clone() {
            if t.elapsed() < Duration::from_secs(2) {
                return v;
            }
        }
        let v = self.shell_pid().and_then(detect_agent_process);
        *self.agent_memo.borrow_mut() = Some((Instant::now(), v.clone()));
        v
    }

    /// The host this pane's shell is `ssh`'d into, if any — a process walk
    /// over its descendants, same shape as `agent()`, memoized the same way.
    pub fn ssh_target(&self) -> Option<String> {
        if let Some((t, v)) = self.ssh_memo.borrow().clone() {
            if t.elapsed() < Duration::from_secs(2) {
                return v;
            }
        }
        let v = self.shell_pid().and_then(detect_ssh_process);
        *self.ssh_memo.borrow_mut() = Some((Instant::now(), v.clone()));
        v
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
            || self
                .stall_fired
                .is_some_and(|t| t.elapsed() < STALL_COOLDOWN)
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

    /// Type a command line into this pane after `delay_ms` — how the
    /// snapshot restore puts an agent back. Paced on a helper thread (like
    /// `fire_autoresume`) so waiting for a newly spawned shell to reach its
    /// prompt doesn't block every other pane's I/O.
    pub fn type_command(&self, tx: Sender<SrvEvent>, cmd: String, delay_ms: u64) {
        let id = self.id;
        std::thread::spawn(move || {
            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            let mut line = cmd.into_bytes();
            line.push(b'\r');
            let _ = tx.send(SrvEvent::Deliver(id, line));
        });
    }

    pub fn cwd(&self) -> Option<String> {
        if let PaneIo::Agent(rt) = &self.io {
            if let Some(dir) = &rt.cwd {
                return Some(dir.to_string_lossy().into_owned());
            }
        }
        let pid = self.shell_pid()?;
        #[cfg(target_os = "linux")]
        {
            std::fs::read_link(format!("/proc/{pid}/cwd"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        }
        #[cfg(target_os = "macos")]
        {
            macos_cwd(pid)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = pid;
            None
        }
    }

    /// The agent's session ("chat") id for this pane — the handle that
    /// reopens exactly this conversation (`claude --resume <id>`, `pi
    /// --session <id>`), which is what makes the session snapshot
    /// restorable across a reboot. None for agents with no readable
    /// session store.
    pub fn chat_id(&self) -> Option<String> {
        let agent = self.agent()?;
        let cwd = self.cwd()?;
        let (path, _) = self.session_transcript(&agent, &cwd)?;
        let stem = path.file_stem()?.to_str()?;
        Some(match agent.as_str() {
            // pi names transcripts `<timestamp>_<uuid>.jsonl`; the uuid is
            // the session id its `--session` flag wants.
            "pi" => stem.rsplit_once('_').map_or(stem, |(_, id)| id).to_string(),
            _ => stem.to_string(),
        })
    }

    /// The pane's conversation as rendered entries, JSON-encoded for
    /// `T_TRANSCRIPT` — the phone's read-mode history. The pty ring can't
    /// provide this (agent TUIs repaint their viewport in place, so almost
    /// nothing ever scrolls into replayable history); the session
    /// transcript on disk holds the whole conversation. Reads the file's
    /// last `TRANSCRIPT_TAIL` bytes and renders user text, assistant text,
    /// and tool calls as one-liners. Empty when the pane's agent keeps no
    /// transcript we can read, or hasn't written one yet.
    pub fn transcript_json(&self) -> Vec<u8> {
        // Agent panes (2.3): entries come from the server-side event ring —
        // no JSONL scraping (that path stays for pty panes).
        if let PaneIo::Agent(rt) = &self.io {
            return serde_json::to_vec(&agent_ring_entries(rt)).unwrap_or_default();
        }
        let Some(agent) = self.agent() else {
            return Vec::new();
        };
        let Some(cwd) = self.cwd() else {
            return Vec::new();
        };
        let Some((path, _)) = self.session_transcript(&agent, &cwd) else {
            return Vec::new();
        };
        let entries = match agent.as_str() {
            "claude" => transcript_entries(&path),
            "pi" => pi_transcript_entries(&path),
            _ => None,
        };
        entries
            .map(|e| serde_json::to_vec(&e).unwrap_or_default())
            .unwrap_or_default()
    }

    /// One line saying what the agent in this pane last did — the card
    /// subtitle's fallback. Claude Code marks every response and tool call
    /// with a "⏺" bullet, so its recap is scraped straight off the
    /// already-rendered screen `text`. pi paints no such marker (every tool
    /// renders its own way: `$ cargo build`, `find … in …`), so its recap
    /// comes from the session transcript instead.
    pub fn recap(&self, agent: Option<&str>, text: Option<&str>) -> Option<String> {
        match agent {
            Some("pi") => self.pi_recap(),
            _ => text.and_then(recap_in),
        }
    }

    /// pi's last conversation beat: the newest tool call or assistant
    /// paragraph in its session transcript. Cached by the transcript's
    /// mtime — the caller's cadence is one broadcast a second and an idle
    /// pane's transcript doesn't move — and only the last `RECAP_TAIL`
    /// bytes are parsed, which is several turns' worth.
    fn pi_recap(&self) -> Option<String> {
        let cwd = self.cwd()?;
        let (path, mtime) = self.session_transcript("pi", &cwd)?;
        if *self.recap_mtime.borrow() == Some(mtime) {
            return self.recap_cache.borrow().clone();
        }
        let recap = self.pi_recap_uncached(&path);
        *self.recap_mtime.borrow_mut() = Some(mtime);
        *self.recap_cache.borrow_mut() = recap.clone();
        recap
    }

    fn pi_recap_uncached(&self, path: &std::path::Path) -> Option<String> {
        let entries = pi_entries_in_tail(path, RECAP_TAIL)?;
        let last = entries
            .iter()
            .rev()
            .find(|e| e.role != "user" && !e.text.trim().is_empty())?;
        let one = last.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out: String = one.chars().take(120).collect();
        if one.chars().nth(120).is_some() {
            out.push('…');
        }
        Some(out)
    }

    /// The short model name for whatever agent is running here, if any.
    pub fn model(&mut self) -> Option<String> {
        let agent = self.agent()?;
        self.agent_model(&agent)
    }

    /// The model the agent in this pane currently runs, as a short display
    /// name (`fable 5`, `sonnet 4.5`) — only for agents whose model is
    /// discoverable: claude and pi via their session transcripts, opencode
    /// via its on-screen footer.
    fn agent_model(&mut self, agent: &str) -> Option<String> {
        match agent {
            "claude" | "pi" => self.transcript_model(agent).or_else(|| {
                if agent == "pi" {
                    self.pi_footer_model()
                } else {
                    None
                }
            }),
            "opencode" => self.opencode_model(),
            _ => None,
        }
    }

    /// Both claude and pi write each assistant turn's `model` into the
    /// session transcript for the pane's cwd (~/.claude/projects/<munged>/
    /// and ~/.pi/agent/sessions/--<munged>--/ respectively). The transcript
    /// this pane owns is picked by `session_transcript`; multiple panes in
    /// one directory share a folder, so the pick is a best effort that's
    /// exact whenever argv or birth time gives it away. Cached by file
    /// mtime so the 1s auto-name tick doesn't re-read an idle transcript.
    fn transcript_model(&mut self, agent: &str) -> Option<String> {
        let cwd = self.cwd()?;
        let (path, mtime) = self.session_transcript(agent, &cwd)?;
        if self.model_mtime == Some(mtime) {
            return self.model_name.clone();
        }
        let name = model_from_transcript(&path);
        self.model_mtime = Some(mtime);
        self.model_name = name.clone();
        name
    }

    /// The agent process serving this pane, with its argv — found among
    /// the shell's descendants, same match as agent detection.
    fn agent_proc(&self, agent: &str) -> Option<(u32, Vec<String>)> {
        let root = self.shell_pid()?;
        descendant_procs(root).into_iter().find(|(_, args)| {
            args.iter()
                .take(2)
                .any(|a| a.rsplit('/').next() == Some(agent))
        })
    }

    /// Which transcript is *this pane's* session? Two agent panes in the
    /// same directory share a session folder, and "newest by mtime" labels
    /// both panes with whichever session wrote last — wrong whenever they
    /// run different models. Stronger signals first:
    ///
    ///  1. The session id in argv (`claude --resume <id>`, `pi --session
    ///     <id>`) — exact.
    ///  2. A fresh agent creates its transcript within moments of the
    ///     process starting — match file birth time to process start.
    ///  3. Otherwise (resume via the interactive picker, filesystems
    ///     without birth times): newest by mtime, the old near-miss.
    fn session_transcript(
        &self,
        agent: &str,
        cwd: &str,
    ) -> Option<(std::path::PathBuf, std::time::SystemTime)> {
        if let Some((t, key, v)) = self.transcript_memo.borrow().clone() {
            if t.elapsed() < Duration::from_secs(2) && key.0 == agent && key.1 == cwd {
                return v;
            }
        }
        let v = self.resolve_transcript(agent, cwd);
        let key = (agent.to_string(), cwd.to_string());
        *self.transcript_memo.borrow_mut() = Some((Instant::now(), key, v.clone()));
        v
    }

    fn resolve_transcript(
        &self,
        agent: &str,
        cwd: &str,
    ) -> Option<(std::path::PathBuf, std::time::SystemTime)> {
        let proc = self.agent_proc(agent);
        let args = proc.as_ref().map(|(_, a)| a.as_slice());
        let dir = session_dir(agent, cwd, args)?;
        if let Some(args) = args {
            if let Some(hit) = session_arg_transcript(agent, &dir, args) {
                return Some(hit);
            }
        }
        if let Some((pid, _)) = &proc {
            if let Some(started) = process_start_time(*pid) {
                if let Some(hit) = jsonl_created_near(&dir, started) {
                    return Some(hit);
                }
            }
        }
        newest_jsonl(&dir)
    }

    /// pi's footer carries `<model> • <thinking level>` at the bottom right
    /// — the fallback when the session transcript is unavailable (`pi
    /// --no-session`, or before the first assistant turn is written).
    fn pi_footer_model(&self) -> Option<String> {
        let tail = self.tail_text(3)?;
        for line in tail.lines().rev() {
            if let Some((left, _)) = line.rsplit_once('•') {
                let id = left.split_whitespace().next_back()?;
                let plausible = id.len() >= 3
                    && id.chars().any(|c| c.is_ascii_digit())
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '/'));
                if plausible {
                    let id = id.rsplit('/').next().unwrap_or(id);
                    return Some(short_model_name(id));
                }
            }
        }
        None
    }

    /// opencode shows `provider/model` in its TUI footer — parse it out of
    /// the bottom rows of the screen. Conservative: the model half must
    /// carry a digit so ordinary paths don't match.
    fn opencode_model(&self) -> Option<String> {
        let tail = self.tail_text(3)?;
        for token in tail.split_whitespace().rev() {
            if let Some((provider, model)) = token.split_once('/') {
                let provider_ok = !provider.is_empty()
                    && provider
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                let model_ok = model.len() >= 3
                    && model.chars().any(|c| c.is_ascii_digit())
                    && model
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_');
                if provider_ok && model_ok {
                    return Some(short_model_name(model));
                }
            }
        }
        None
    }

    /// The name of the app in this pane's foreground process group — `nvim`,
    /// `htop`, … — or None when the shell itself is at the prompt (or the
    /// answer is unknowable). Asks the PTY who owns the terminal
    /// (`tcgetpgrp`), which is exactly the "an app is open here" signal.
    pub fn fg_app(&self) -> Option<String> {
        let PaneIo::Pty { master, pid, .. } = &self.io else {
            return None;
        };
        let fd = master.as_raw_fd()?;
        let pgid = unsafe { libc::tcgetpgrp(fd) };
        if pgid <= 0 {
            return None;
        }
        if Some(pgid as u32) == *pid {
            return None; // the login shell itself is foreground
        }
        let name = process_name(pgid as u32)?;
        if SHELL_NAMES.contains(&name.as_str()) {
            return None;
        }
        Some(name)
    }

    /// Whether a real application — rather than the shell sitting at its
    /// prompt — currently owns this pane's terminal. Gates mouse reports:
    /// see the `T_MOUSE` arm in `server::Server::handle`.
    pub fn app_foreground(&self) -> bool {
        self.fg_app().is_some()
    }

    /// What this pane should be called when the user hasn't named it:
    /// the agent running here (with its selected model, when known — e.g.
    /// `fable 5` instead of `claude`), else the ssh destination, else the
    /// foreground app, else the shell's working directory (basename,
    /// `~` for home, `/` for root).
    pub fn auto_name(&mut self) -> Option<String> {
        if let Some(agent) = self.agent() {
            if let Some(model) = self.agent_model(&agent) {
                return Some(model);
            }
            return Some(agent);
        }
        if let Some(host) = self.ssh_target() {
            return Some(host);
        }
        if let Some(app) = self.fg_app() {
            return Some(app);
        }
        let cwd = self.cwd()?;
        if let Ok(home) = std::env::var("HOME") {
            if cwd == home {
                return Some("~".to_string());
            }
        }
        Some(
            std::path::Path::new(&cwd)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string()),
        )
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
    /// Is a turn in flight? Both agents draw a braille spinner at the head
    /// of a status line low on the screen. Claude Code always hangs an "esc
    /// to interrupt" hint off it, and requiring that hint is what keeps a
    /// pane merely *quoting* a spinner glyph from reading as busy.
    ///
    /// pi's line is usually bare — ` ⠏ Working...` — and only sometimes
    /// carries a hint ("Esc to cancel" while retrying or compacting), so
    /// there the leading spinner has to stand alone. That's a looser test,
    /// and deliberately: for pi this only ever decides a status label,
    /// while claude's stricter reading also arms the watchdog's keystrokes.
    pub fn thinking(&self) -> bool {
        let hint: Option<&[&str]> = match self.agent().as_deref() {
            Some("pi") => None,
            _ => Some(&["esctointerrupt"]),
        };
        spinner_row(self.term.screen(), hint)
    }

    /// Is pi mid-turn? Its terminal title carries no state (`π - <dir>`), so
    /// the spinner line is the signal. That line blinks out for a moment
    /// between a tool finishing and the next one starting, so a recent
    /// sighting bridges the gap — the same job claude's `last_title_working`
    /// does for its ✳ rest frames. Memoized rather than tracked in
    /// `update()`: this scans rows, and `status()` runs once per broadcast
    /// while `update()` runs on every pty chunk.
    fn pi_working(&self) -> bool {
        if self.thinking() {
            *self.busy_memo.borrow_mut() = Some(Instant::now());
            return true;
        }
        self.busy_memo
            .borrow()
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
            && self
                .last_output
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        // Wire values again — cap to something a real terminal could be
        // and keep the pixel products from overflowing u16.
        let (rows, cols) = (rows.min(1000), cols.min(1000));
        if rows < 2 || cols < 10 || self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        if let PaneIo::Pty { master, .. } = &self.io {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: cols.saturating_mul(self.cell.0),
                pixel_height: rows.saturating_mul(self.cell.1),
            });
        }
        self.term.resize(rows, cols);
        for ev in self.term.drain_events() {
            self.gfx.apply_event(ev);
        }
    }

    pub fn write_input(&mut self, bytes: &[u8]) {
        // Raw pty bytes are meaningless to an agent process — its input
        // arrives structured via T_AGENT_INPUT (see Server::agent_input).
        if let PaneIo::Pty { writer, .. } = &mut self.io {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    pub fn clear_flags(&mut self) {
        self.activity = false;
        self.attention = false;
    }

    pub fn kill(&mut self) {
        match &mut self.io {
            PaneIo::Pty { killer, .. } => {
                let _ = killer.kill();
            }
            PaneIo::Agent(rt) => rt.kill(),
        }
    }

    /// The pane's root process: the login shell (pty) or the agent itself.
    fn shell_pid(&self) -> Option<u32> {
        match &self.io {
            PaneIo::Pty { pid, .. } => *pid,
            PaneIo::Agent(rt) => rt.pid,
        }
    }

    pub fn agent_rt(&self) -> Option<&crate::agent::AgentRuntime> {
        match &self.io {
            PaneIo::Agent(rt) => Some(rt),
            PaneIo::Pty { .. } => None,
        }
    }

    pub fn agent_rt_mut(&mut self) -> Option<&mut crate::agent::AgentRuntime> {
        match &mut self.io {
            PaneIo::Agent(rt) => Some(rt),
            PaneIo::Pty { .. } => None,
        }
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
fn stall_match(screen: &dyn crate::engine::TermScreen, conn_watch: bool) -> Option<Duration> {
    let (rows, cols) = screen.size();
    for r in rows.saturating_sub(STALL_ROWS)..rows {
        let mut text = String::new();
        let mut col_of: Vec<u16> = Vec::new();
        for c in 0..cols {
            let Some(cell) = screen.cell(r, c) else {
                continue;
            };
            for ch in cell.contents.chars() {
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
                    .is_some_and(|cell| is_error_color(cell.fg))
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
fn is_error_color(c: crate::engine::Color) -> bool {
    use crate::engine::Color;
    match c {
        Color::Idx(i) => matches!(i, 1 | 9) || (160..=203).contains(&i),
        Color::Rgb(r, g, b) => {
            r > 120 && i32::from(r) - i32::from(g) > 50 && i32::from(r) - i32::from(b) > 50
        }
        Color::Default => false,
    }
}

/// Claude Code's status-line spinner frames: ✳ or a braille pattern. pi
/// animates braille too, from its own frame list.
fn is_spinner_char(c: char) -> bool {
    c == '✳' || ('\u{2800}'..='\u{28ff}').contains(&c)
}

/// Is there a spinner-led status line in the bottom rows? `hint`, when
/// given, is a set of whitespace-stripped lowercase phrases of which the
/// row must carry one — the "this is a live status line, not quoted text"
/// evidence.
fn spinner_row(screen: &dyn crate::engine::TermScreen, hint: Option<&[&str]>) -> bool {
    let (rows, cols) = screen.size();
    for r in rows.saturating_sub(STALL_ROWS)..rows {
        let mut text = String::new();
        for c in 0..cols {
            if let Some(cell) = screen.cell(r, c) {
                for ch in cell.contents.chars() {
                    if !ch.is_whitespace() {
                        text.push(ch);
                    }
                }
            }
        }
        if !text.chars().next().is_some_and(is_spinner_char) {
            continue;
        }
        match hint {
            None => return true,
            Some(phrases) => {
                let text = text.to_ascii_lowercase();
                if phrases.iter().any(|p| text.contains(p)) {
                    return true;
                }
            }
        }
    }
    false
}
/// Only the bottom rows are scanned — that's where Claude Code renders the
/// error/status area; old conversation text scrolled above doesn't count.
const STALL_ROWS: u16 = 15;
const STALL_COOLDOWN: Duration = Duration::from_secs(30);
/// If the phrase never leaves the screen after an intervention, try again.
const STALL_RETRY: Duration = Duration::from_secs(90);

const AGENT_BINARIES: &[&str] = &[
    "claude", "pi", "opencode", "codex", "aider", "gemini", "goose",
];

/// Where an agent keeps the session transcripts for a working directory.
/// `args` is the agent process's argv, for flags that move the directory.
fn session_dir(agent: &str, cwd: &str, args: Option<&[String]>) -> Option<std::path::PathBuf> {
    match agent {
        "claude" => claude_project_dir(cwd),
        "pi" => pi_session_dir(cwd, args),
        _ => None,
    }
}

/// Claude Code's project directory for a working directory:
/// ~/.claude/projects/<munged>, where the name is the cwd with every
/// non-alphanumeric character turned into `-`.
fn claude_project_dir(cwd: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let munged: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(
        std::path::Path::new(&home)
            .join(".claude/projects")
            .join(munged),
    )
}

/// pi's session directory for a working directory. Its own rule (from
/// `getDefaultSessionDirPath`): drop the leading slash, map `/`, `\` and
/// `:` to `-`, wrap the result in `--`, and hang it under
/// <agent dir>/sessions. Everything else in the path — dots, underscores —
/// survives, unlike claude's flatten-everything munging.
///
/// `--session-dir` and `PI_CODING_AGENT_SESSION_DIR` name a directory
/// outright (no per-project subdirectory); `PI_CODING_AGENT_DIR` moves the
/// ~/.pi/agent root that the default is computed from.
fn pi_session_dir(cwd: &str, args: Option<&[String]>) -> Option<std::path::PathBuf> {
    if let Some(dir) = args.and_then(|a| flag_value(a, &["--session-dir"])) {
        return Some(std::path::PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("PI_CODING_AGENT_SESSION_DIR") {
        if !dir.is_empty() {
            return Some(std::path::PathBuf::from(dir));
        }
    }
    let agent_dir = match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => std::path::Path::new(&std::env::var("HOME").ok()?).join(".pi/agent"),
    };
    Some(agent_dir.join("sessions").join(pi_project_name(cwd)))
}

/// The `--home-d3s-claude-zodiac--` directory name pi derives from a cwd.
fn pi_project_name(cwd: &str) -> String {
    let trimmed = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    let munged: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{munged}--")
}

/// The transcript named outright by the agent's argv, if any: claude's
/// `--resume <id>` is the file's stem, while pi's `--session`/`--fork` take
/// a path or a *partial* uuid that has to be matched against the directory.
fn session_arg_transcript(
    agent: &str,
    dir: &std::path::Path,
    args: &[String],
) -> Option<(std::path::PathBuf, std::time::SystemTime)> {
    let path = match agent {
        "claude" => dir.join(format!("{}.jsonl", resume_id(args)?)),
        "pi" => {
            let id = pi_session_arg(args)?;
            // A path (absolute, relative, or just a file name) is used as
            // given; anything else is a session id to look up.
            if id.contains('/') || id.ends_with(".jsonl") {
                std::path::PathBuf::from(id)
            } else {
                jsonl_matching(dir, id)?
            }
        }
        _ => return None,
    };
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    Some((path, mtime))
}

/// The session id from a `claude --resume <id>` argv, if present.
fn resume_id(args: &[String]) -> Option<&str> {
    let at = args.iter().position(|a| a == "--resume" || a == "-r")?;
    let id = args.get(at + 1)?;
    // A session id, not a following flag or prompt text.
    (id.len() >= 8 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then_some(id.as_str())
}

/// The session pi was pointed at: `--session`/`--session-id`/`--fork` take
/// a session file path or a (possibly partial) uuid. `-r`/`--resume` is
/// deliberately not read — in pi those take no argument and open the
/// interactive picker, so the session isn't knowable from argv.
fn pi_session_arg(args: &[String]) -> Option<&str> {
    let v = flag_value(args, &["--session", "--session-id", "--fork"])?;
    // Not a following flag, and long enough to be a session handle.
    (v.len() >= 4 && !v.starts_with('-')).then_some(v)
}

/// The value after the first of `flags` present in argv.
fn flag_value<'a>(args: &'a [String], flags: &[&str]) -> Option<&'a str> {
    let at = args.iter().position(|a| flags.contains(&a.as_str()))?;
    args.get(at + 1).map(String::as_str)
}

/// The transcript in `dir` whose file name carries `id` — how pi resolves
/// a partial uuid, its file names being `<timestamp>_<uuid>.jsonl`. Newest
/// by mtime wins if a short id somehow matches more than one.
fn jsonl_matching(dir: &std::path::Path, id: &str) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(id))
        {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(_, t)| mtime > *t) {
            best = Some((path, mtime));
        }
    }
    best.map(|(p, _)| p)
}

/// The transcript whose creation time sits within a couple of minutes of
/// `started` — a fresh claude writes its session file moments after launch.
/// None when birth times are unavailable (some Linux filesystems) or no
/// file matches (a resumed session appends to an old file).
fn jsonl_created_near(
    dir: &std::path::Path,
    started: std::time::SystemTime,
) -> Option<(std::path::PathBuf, std::time::SystemTime)> {
    const SLACK: std::time::Duration = std::time::Duration::from_secs(180);
    let mut best: Option<(
        std::path::PathBuf,
        std::time::SystemTime,
        std::time::Duration,
    )> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let (Ok(created), Ok(mtime)) = (meta.created(), meta.modified()) else {
            continue;
        };
        let gap = match created.duration_since(started) {
            Ok(d) => d,
            Err(e) => e.duration(),
        };
        if gap <= SLACK && best.as_ref().is_none_or(|(_, _, g)| gap < *g) {
            best = Some((path, mtime, gap));
        }
    }
    best.map(|(p, m, _)| (p, m))
}

/// Newest transcript by mtime — the last-resort pick.
fn newest_jsonl(dir: &std::path::Path) -> Option<(std::path::PathBuf, std::time::SystemTime)> {
    let mut newest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(_, t)| mtime > *t) {
            newest = Some((path, mtime));
        }
    }
    newest
}

/// When a process started, for pairing it with the session file it created.
#[cfg(target_os = "macos")]
fn process_start_time(pid: u32) -> Option<std::time::SystemTime> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got != size {
        return None;
    }
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(info.pbi_start_tvsec))
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<std::time::SystemTime> {
    // The /proc/<pid> directory is created when the process is — its
    // metadata timestamps are the start time, without jiffies arithmetic.
    std::fs::metadata(format!("/proc/{pid}"))
        .ok()?
        .modified()
        .ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_time(_pid: u32) -> Option<std::time::SystemTime> {
    None
}

/// Render an agent pane's event ring into read-mode entries: synthetic
/// user prompts, assistant text blocks, and tool calls as one-liners
/// (the same shapes `transcript_entries` distills from disk JSONL).
fn agent_ring_entries(rt: &crate::agent::AgentRuntime) -> Vec<TranscriptEntry> {
    let mut out = Vec::new();
    for line in rt.ring_lines() {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("zodiac_user") => {
                if let Some(s) = v.get("text").and_then(|t| t.as_str()) {
                    push_entry(&mut out, "user", s);
                }
            }
            Some("assistant") => {
                if let Some(serde_json::Value::Array(blocks)) =
                    v.get("message").and_then(|m| m.get("content"))
                {
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                                    push_entry(&mut out, "assistant", s);
                                }
                            }
                            Some("tool_use") => {
                                let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                                out.push(TranscriptEntry {
                                    role: "tool",
                                    text: tool_line(name, b.get("input")),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            // pi: complete assistant messages arrive as message_end.
            Some("message_end") => {
                let Some(m) = v.get("message") else { continue };
                let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
                if role != "assistant" {
                    continue;
                }
                if let Some(serde_json::Value::Array(blocks)) = m.get("content") {
                    for b in blocks {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                                push_entry(&mut out, "assistant", s);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if out.len() > TRANSCRIPT_MAX_ENTRIES {
        let cut = out.len() - TRANSCRIPT_MAX_ENTRIES;
        out.drain(..cut);
    }
    out
}

/// One rendered conversation entry for the phone's read mode.
#[derive(serde::Serialize)]
pub struct TranscriptEntry {
    pub role: &'static str, // "user" | "assistant" | "tool"
    pub text: String,
}

/// Transcript tail parsed per `T_TRANSCRIPT_REQ`. A 1 MiB tail of JSONL
/// renders to a few hundred KB of entries at most — well under the 8 MiB
/// frame cap, and hours of conversation on a phone screen.
const TRANSCRIPT_TAIL: u64 = 1024 * 1024;
const TRANSCRIPT_MAX_ENTRIES: usize = 500;

/// Parse a Claude Code session transcript's tail into readable entries:
/// user text, assistant text blocks, and tool calls as one-liners.
/// Sidechain (subagent) and meta lines are skipped, as is harness markup
/// riding the user channel (`<command-…>`, `<system-reminder>`, caveats).
fn transcript_entries(path: &std::path::Path) -> Option<Vec<TranscriptEntry>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 && len > TRANSCRIPT_TAIL {
            continue; // the seek almost certainly landed mid-line
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if typ != "user" && typ != "assistant" {
            continue;
        }
        if v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true)
            || v.get("isMeta").and_then(|b| b.as_bool()) == Some(true)
        {
            continue;
        }
        let role: &'static str = if typ == "user" { "user" } else { "assistant" };
        match v.get("message").and_then(|m| m.get("content")) {
            Some(serde_json::Value::String(s)) => push_entry(&mut out, role, s),
            Some(serde_json::Value::Array(blocks)) => {
                for b in blocks {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                                push_entry(&mut out, role, s);
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            out.push(TranscriptEntry {
                                role: "tool",
                                text: tool_line(name, b.get("input")),
                            });
                        }
                        // thinking and tool_result aren't part of the
                        // readable conversation flow.
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if out.len() > TRANSCRIPT_MAX_ENTRIES {
        out.drain(..out.len() - TRANSCRIPT_MAX_ENTRIES);
    }
    Some(out)
}

/// Parse a pi session transcript's tail into the same readable entries.
/// pi's JSONL is a *tree* of typed entries rather than claude's flat log:
/// conversation lines are `{"type":"message","message":{"role":…}}` with
/// `text`, `thinking` and `toolCall` content blocks, interleaved with
/// bookkeeping lines (`session`, `model_change`, `compaction`, …) that are
/// skipped. Branching is ignored: the tail is read in file order, which is
/// the order the user actually saw things happen.
fn pi_transcript_entries(path: &std::path::Path) -> Option<Vec<TranscriptEntry>> {
    pi_entries_in_tail(path, TRANSCRIPT_TAIL)
}

/// How much of a pi transcript the card recap parses — several turns, far
/// cheaper than the full read-mode tail on a once-a-second path.
const RECAP_TAIL: u64 = 64 * 1024;

fn pi_entries_in_tail(path: &std::path::Path, tail: u64) -> Option<Vec<TranscriptEntry>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(tail))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 && len > tail {
            continue; // the seek almost certainly landed mid-line
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let Some(message) = v.get("message") else {
            continue;
        };
        // "toolResult" is the third role pi uses; like claude's tool_result
        // blocks it isn't part of the readable conversation flow.
        let role: &'static str = match message.get("role").and_then(|r| r.as_str()) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => continue,
        };
        match message.get("content") {
            Some(serde_json::Value::String(s)) => push_pi_entry(&mut out, role, s),
            Some(serde_json::Value::Array(blocks)) => {
                for b in blocks {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                                push_pi_entry(&mut out, role, s);
                            }
                        }
                        Some("toolCall") => {
                            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            out.push(TranscriptEntry {
                                role: "tool",
                                text: tool_line(name, b.get("arguments")),
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if out.len() > TRANSCRIPT_MAX_ENTRIES {
        out.drain(..out.len() - TRANSCRIPT_MAX_ENTRIES);
    }
    Some(out)
}

/// pi carries no harness markup on the user channel — what the transcript
/// shows as a user turn is what the user typed.
fn push_pi_entry(out: &mut Vec<TranscriptEntry>, role: &'static str, s: &str) {
    let t = s.trim();
    if t.is_empty() {
        return;
    }
    out.push(TranscriptEntry {
        role,
        text: t.to_string(),
    });
}

fn push_entry(out: &mut Vec<TranscriptEntry>, role: &'static str, s: &str) {
    let t = s.trim();
    if t.is_empty() {
        return;
    }
    if role == "user" && (t.starts_with('<') || t.starts_with("Caveat:")) {
        return;
    }
    out.push(TranscriptEntry {
        role,
        text: t.to_string(),
    });
}

/// `Bash(cargo build --release)` — the tool call as the one-liner Claude
/// Code itself prints, using the most informative input field.
fn tool_line(name: &str, input: Option<&serde_json::Value>) -> String {
    let arg = input
        .and_then(|i| {
            [
                "command",
                "file_path",
                "path",
                "pattern",
                "url",
                "query",
                "prompt",
                "description",
                "skill",
                "subject",
            ]
            .iter()
            .find_map(|k| i.get(k).and_then(|v| v.as_str()))
        })
        .unwrap_or("");
    let one = arg.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = one.chars().take(120).collect();
    if one.chars().count() > 120 {
        short.push('…');
    }
    if short.is_empty() {
        name.to_string()
    } else {
        format!("{name}({short})")
    }
}

/// The last `"model": "..."` value in the tail of a session transcript —
/// assistant turns carry the model that produced them, so the last one is
/// the session's current selection (and tracks /model switches). Reads at
/// most the final 64 KB.
fn model_from_transcript(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    last_model_in(&text)
}

fn last_model_in(text: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    let mut at = 0;
    while let Some(pos) = text[at..].find("\"model\":") {
        let rest = &text[at + pos + 8..];
        let rest = rest.trim_start();
        at += pos + 8;
        if let Some(rest) = rest.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                let m = &rest[..end];
                if !m.is_empty() && !m.starts_with('<') {
                    best = Some(m);
                }
            }
        }
    }
    best.map(short_model_name)
}

/// `claude-sonnet-4-5-20250929` → `sonnet 4.5`, `claude-fable-5` →
/// `fable 5`: strip the vendor prefix and date suffix, join version digits
/// with dots. Ids that don't fit the family-then-numbers shape just get
/// their dashes spaced out.
fn short_model_name(id: &str) -> String {
    let id = id.strip_prefix("claude-").unwrap_or(id);
    let id = match id.len().checked_sub(9) {
        Some(cut)
            if id.as_bytes()[cut] == b'-' && id[cut + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &id[..cut]
        }
        _ => id,
    };
    let mut parts = id.split('-');
    let family = parts.next().unwrap_or(id);
    let nums: Vec<&str> = parts.collect();
    if !nums.is_empty() && nums.iter().all(|n| n.chars().all(|c| c.is_ascii_digit())) {
        format!("{} {}", family, nums.join("."))
    } else {
        id.replace('-', " ")
    }
}

/// Foreground names that mean "nothing is really running here" — the pane
/// falls through to cwd-based auto-naming.
const SHELL_NAMES: &[&str] = &[
    "zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu", "nushell", "login",
];

/// The executable name of an arbitrary process, for auto-naming.
#[cfg(target_os = "linux")]
fn process_name(pid: u32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(target_os = "macos")]
fn process_name(pid: u32) -> Option<String> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    path.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_name(_pid: u32) -> Option<String> {
    None
}

/// A process's working directory on macOS, via `proc_pidinfo`'s
/// PROC_PIDVNODEPATHINFO flavor. The struct is passed as an opaque
/// correctly-sized byte buffer: two `vnode_info_path`s (a 152-byte
/// `vnode_info` followed by a 1024-byte path) — cwd first, root second —
/// 2352 bytes total, matching XNU's `struct proc_vnodepathinfo`.
#[cfg(target_os = "macos")]
fn macos_cwd(pid: u32) -> Option<String> {
    const PROC_PIDVNODEPATHINFO: libc::c_int = 9;
    const VNODE_INFO_SIZE: usize = 152;
    const PATH_SIZE: usize = 1024;
    const TOTAL: usize = 2 * (VNODE_INFO_SIZE + PATH_SIZE);
    let mut buf = [0u8; TOTAL];
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDVNODEPATHINFO,
            0,
            buf.as_mut_ptr() as *mut libc::c_void,
            TOTAL as libc::c_int,
        )
    };
    if n < (VNODE_INFO_SIZE + PATH_SIZE) as libc::c_int {
        return None;
    }
    let path = &buf[VNODE_INFO_SIZE..VNODE_INFO_SIZE + PATH_SIZE];
    let end = path.iter().position(|b| *b == 0).unwrap_or(PATH_SIZE);
    let cwd = String::from_utf8_lossy(&path[..end]).into_owned();
    (!cwd.is_empty()).then_some(cwd)
}
/// Every descendant process of `root` as (pid, argv), breadth-first —
/// the raw material for "what is actually running in this pane".
#[cfg(target_os = "linux")]
fn descendant_procs(root: u32) -> Vec<(u32, Vec<String>)> {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
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
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if pid != root {
            let args = std::fs::read(format!("/proc/{pid}/cmdline"))
                .map(|cmdline| {
                    cmdline
                        .split(|b| *b == 0)
                        .filter(|a| !a.is_empty())
                        .map(|a| String::from_utf8_lossy(a).into_owned())
                        .collect()
                })
                .unwrap_or_default();
            out.push((pid, args));
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids);
        }
    }
    out
}

/// macOS has no /proc: the process table comes from `proc_listpids` +
/// `proc_pidinfo`, and each descendant's argv from a `KERN_PROCARGS2`
/// sysctl. When argv is unreadable the accounting name stands in for it,
/// which is enough for the agent match.
#[cfg(target_os = "macos")]
fn descendant_procs(root: u32) -> Vec<(u32, Vec<String>)> {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for (pid, ppid) in macos_proc_table() {
        children.entry(ppid).or_default().push(pid);
    }
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if pid != root {
            let mut args = macos_args(pid);
            if args.is_empty() {
                args = process_name(pid).into_iter().collect();
            }
            out.push((pid, args));
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids);
        }
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn descendant_procs(_root: u32) -> Vec<(u32, Vec<String>)> {
    Vec::new()
}

/// (pid, ppid) for every process this user can see. `proc_listpids` reports
/// how many *bytes* it wrote, hence the division.
#[cfg(target_os = "macos")]
fn macos_proc_table() -> Vec<(u32, u32)> {
    const PROC_ALL_PIDS: u32 = 1;
    let want = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if want <= 0 {
        return Vec::new();
    }
    // Headroom for processes spawned between the sizing call and this one.
    let cap = want as usize / std::mem::size_of::<libc::c_int>() + 64;
    let mut pids = vec![0 as libc::c_int; cap];
    let n = unsafe {
        libc::proc_listpids(
            PROC_ALL_PIDS,
            0,
            pids.as_mut_ptr() as *mut libc::c_void,
            (cap * std::mem::size_of::<libc::c_int>()) as libc::c_int,
        )
    };
    if n <= 0 {
        return Vec::new();
    }
    pids.truncate(n as usize / std::mem::size_of::<libc::c_int>());
    let mut out = Vec::with_capacity(pids.len());
    for pid in pids {
        if pid <= 0 {
            continue;
        }
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if got == size {
            out.push((pid as u32, info.pbi_ppid));
        }
    }
    out
}

/// A process's argv via `KERN_PROCARGS2`, whose buffer is: argc (u32), the
/// executable path, NUL padding, then argc NUL-terminated arguments (the
/// environment follows, and is ignored — argc bounds the walk).
#[cfg(target_os = "macos")]
fn macos_args(pid: u32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || size < 4 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size];
    let filled = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if filled != 0 || size < 4 {
        return Vec::new();
    }
    buf.truncate(size);
    let argc = u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;
    let mut chunks = buf[4..].split(|b| *b == 0);
    chunks.next(); // the exec path, repeated ahead of argv
    let mut args = Vec::with_capacity(argc);
    for chunk in chunks {
        if args.len() == argc {
            break;
        }
        if chunk.is_empty() {
            continue; // alignment padding between the path and argv[0]
        }
        args.push(String::from_utf8_lossy(chunk).into_owned());
    }
    args
}

/// The agent's most recent transcript bullet out of an already-rendered
/// screen: the lowest line starting with "⏺"/"●" (Claude Code's response/
/// tool markers). Free function so `state()` can render each pane's screen
/// once and feed both this and `tail_lines_in`.
pub fn recap_in(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('⏺').or_else(|| t.strip_prefix('●')) {
            let s = rest.split_whitespace().collect::<Vec<_>>().join(" ");
            if s.is_empty() {
                continue;
            }
            let mut out: String = s.chars().take(120).collect();
            if s.chars().nth(120).is_some() {
                out.push('…');
            }
            return Some(out);
        }
    }
    None
}

/// The last `n` non-blank lines of an already-rendered screen, oldest
/// first — the charts view's transcript well.
pub fn tail_lines_in(text: &str, n: usize) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .rev()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .map(|l| {
            let mut s: String = l.chars().take(120).collect();
            if l.chars().nth(120).is_some() {
                s.push('…');
            }
            s
        })
        .collect();
    out.reverse();
    out
}

/// Find a known agent binary among the descendants of `root`. Matches the
/// basename of argv[0] or argv[1] (argv[1] catches interpreter-run agents
/// like `node .../bin/claude`).
fn detect_agent_process(root: u32) -> Option<String> {
    for (_, args) in descendant_procs(root) {
        for arg in args.iter().take(2) {
            let base = arg.rsplit('/').next().unwrap_or(arg);
            if let Some(hit) = AGENT_BINARIES.iter().find(|a| **a == base) {
                return Some(hit.to_string());
            }
        }
    }
    None
}

/// Same descendant walk as `detect_agent_process`, but looking for an `ssh`
/// process — returns the destination host it's connecting to.
fn detect_ssh_process(root: u32) -> Option<String> {
    for (_, args) in descendant_procs(root) {
        let base = args.first().map(|a| a.rsplit('/').next().unwrap_or(a));
        if base == Some("ssh") {
            if let Some(dest) = ssh_destination(&args[1..]) {
                return Some(dest);
            }
        }
    }
    None
}

/// ssh's destination is its first non-flag argument. Flags that take a
/// separate value (`-p 2222`, `-o Foo=bar`, ...) have that value skipped
/// too; inline forms (`-p2222`, `-oFoo=bar`) are already a single argv
/// entry and need no special handling. Strips `ssh://`, `user@` and a
/// URI-style `:port` suffix from whatever's left.
fn ssh_destination(args: &[String]) -> Option<String> {
    const VALUE_FLAGS: &str = "BbcDEeFIiJLlmOopQRSWw";
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            i += 1;
            break;
        }
        let Some(flag) = arg.strip_prefix('-') else {
            break;
        };
        i += if flag.len() == 1 && VALUE_FLAGS.contains(flag) {
            2
        } else {
            1
        };
    }
    let dest = args.get(i)?;
    let dest = dest.strip_prefix("ssh://").unwrap_or(dest);
    let dest = dest.rsplit('@').next().unwrap_or(dest);
    let host = match dest.split_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => dest,
    };
    (!host.is_empty()).then(|| host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// Child-side DECSET 2026 (roadmap 1.6): output inside ?2026h..?2026l
    /// is held and flushed as one piece; the deadline valve frees a stuck
    /// hold. Uses a real spawned pane (the bytes fed are synthetic; the
    /// shell child underneath is just a live PTY to construct against).
    #[test]
    fn sync_2026_coalesces_output() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut p = SrvPane::spawn(9999, None, 24, 80, None, Vec::new(), tx).unwrap();
        let (out, _) = p.process_output(b"\x1b[?2026hheld");
        assert!(out.is_empty(), "output must be held during a sync update");
        assert!(p.sync_pending());
        assert!(p.sync_flush_due().is_none(), "deadline must not fire early");
        let (out, _) = p.process_output(b" more\x1b[?2026l tail");
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("held") && s.contains("more") && s.contains("tail"));
        assert!(!p.sync_pending());

        // Deadline valve: a hold left open flushes after SYNC_DEADLINE.
        let (out, _) = p.process_output(b"\x1b[?2026hstuck");
        assert!(out.is_empty());
        std::thread::sleep(SYNC_DEADLINE + Duration::from_millis(20));
        let flushed = p.sync_flush_due().expect("deadline flush");
        assert!(String::from_utf8_lossy(&flushed).contains("stuck"));
        assert!(!p.sync_pending());
        p.kill();
    }

    #[test]
    fn ssh_destination_plain_host() {
        assert_eq!(ssh_destination(&args("bigbox")), Some("bigbox".into()));
    }

    #[test]
    fn ssh_destination_strips_user() {
        assert_eq!(ssh_destination(&args("des@bigbox")), Some("bigbox".into()));
    }

    #[test]
    fn ssh_destination_strips_uri_scheme_and_port() {
        assert_eq!(
            ssh_destination(&args("ssh://des@bigbox:2222")),
            Some("bigbox".into())
        );
    }

    #[test]
    fn ssh_destination_skips_value_taking_flags() {
        assert_eq!(
            ssh_destination(&args("-p 2222 -o StrictHostKeyChecking=no bigbox")),
            Some("bigbox".into())
        );
    }

    #[test]
    fn ssh_destination_skips_flag_only_options() {
        assert_eq!(
            ssh_destination(&args("-tt -v des@bigbox")),
            Some("bigbox".into())
        );
    }

    #[test]
    fn ssh_destination_ignores_a_trailing_remote_command() {
        assert_eq!(
            ssh_destination(&args("bigbox zodiac main")),
            Some("bigbox".into())
        );
    }

    #[test]
    fn ssh_destination_none_when_only_flags() {
        assert_eq!(ssh_destination(&args("-p 2222")), None);
    }

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
        lines.extend(std::iter::repeat_n("filler", 20));
        let p = feed(&lines);
        assert_eq!(stall_match(p.screen(), true), None);
    }
}

#[cfg(test)]
mod model_name_tests {
    use super::{last_model_in, short_model_name};

    #[test]
    fn short_names_strip_vendor_and_date() {
        assert_eq!(short_model_name("claude-fable-5"), "fable 5");
        assert_eq!(short_model_name("claude-opus-5"), "opus 5");
        assert_eq!(short_model_name("claude-sonnet-4-5-20250929"), "sonnet 4.5");
        assert_eq!(short_model_name("claude-haiku-4-5-20251001"), "haiku 4.5");
        assert_eq!(short_model_name("gpt-5.2"), "gpt 5.2");
    }

    #[test]
    fn last_model_wins_and_synthetic_is_skipped() {
        let t = r#"{"message":{"model":"claude-opus-5"}}
{"message":{"model":"<synthetic>"}}
{"message":{"model":"claude-fable-5"}}"#;
        assert_eq!(last_model_in(t), Some("fable 5".to_string()));
        assert_eq!(last_model_in("no model here"), None);
    }
}

#[cfg(test)]
mod pi_session_tests {
    use super::{pi_project_name, pi_session_arg};

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn project_name_keeps_everything_but_separators() {
        // pi's own rule: drop the leading slash, map separators to `-`,
        // wrap in `--`. Unlike claude's munging, dots and underscores live.
        assert_eq!(pi_project_name("/home/d3s"), "--home-d3s--");
        assert_eq!(
            pi_project_name("/home/d3s/claude/zodiac"),
            "--home-d3s-claude-zodiac--"
        );
        assert_eq!(
            pi_project_name("/tmp/pi.test.dir/a_b.c"),
            "--tmp-pi.test.dir-a_b.c--"
        );
        // Both the drive colon and its separator map, as they do in pi.
        assert_eq!(pi_project_name("C:\\src\\app"), "--C--src-app--");
    }

    #[test]
    fn session_arg_reads_the_flags_that_carry_one() {
        assert_eq!(
            pi_session_arg(&args("pi --session 019fe295-3523-7d14")),
            Some("019fe295-3523-7d14")
        );
        assert_eq!(
            pi_session_arg(&args("pi --session-id abcd1234")),
            Some("abcd1234")
        );
        assert_eq!(
            pi_session_arg(&args("pi --fork abcd1234")),
            Some("abcd1234")
        );
        assert_eq!(
            pi_session_arg(&args("pi --session /tmp/s/019f.jsonl")),
            Some("/tmp/s/019f.jsonl")
        );
        // -r/--resume open pi's picker and take no argument, so argv says
        // nothing about which session ends up loaded.
        assert_eq!(pi_session_arg(&args("pi -r")), None);
        assert_eq!(pi_session_arg(&args("pi --resume")), None);
        assert_eq!(pi_session_arg(&args("pi --session --continue")), None);
        assert_eq!(pi_session_arg(&args("pi --session")), None);
        assert_eq!(pi_session_arg(&args("pi")), None);
    }
}

#[cfg(test)]
mod transcript_entry_tests {
    use super::transcript_entries;

    #[test]
    fn renders_conversation_and_skips_noise() {
        let dir = std::env::temp_dir().join("zodiac-transcript-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"fix the bug"}}"#, "\n",
                r#"{"type":"user","message":{"role":"user","content":"<system-reminder>noise</system-reminder>"}}"#, "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo   build\n--release"}}]}}"#, "\n",
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#, "\n",
                r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"subagent chatter"}]}}"#, "\n",
                r#"{"type":"ai-title","aiTitle":"whatever"}"#, "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Fixed."}]}}"#, "\n",
            ),
        )
        .unwrap();
        let e = transcript_entries(&path).unwrap();
        let flat: Vec<(&str, &str)> = e.iter().map(|x| (x.role, x.text.as_str())).collect();
        assert_eq!(
            flat,
            vec![
                ("user", "fix the bug"),
                ("assistant", "On it."),
                ("tool", "Bash(cargo build --release)"),
                ("assistant", "Fixed."),
            ]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pi_entries_skip_bookkeeping_and_tool_results() {
        use crate::pane::{model_from_transcript, pi_transcript_entries};
        let dir = std::env::temp_dir().join("zodiac-pi-transcript-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session","version":3,"id":"019f","cwd":"/src"}"#, "\n",
                r#"{"type":"model_change","provider":"anthropic","modelId":"claude-opus-5"}"#, "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"fix the bug"}]}}"#, "\n",
                r#"{"type":"message","message":{"role":"assistant","model":"claude-opus-5","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"On it."},{"type":"toolCall","id":"t1","name":"bash","arguments":{"command":"cargo   build\n--release"}}]}}"#, "\n",
                r#"{"type":"message","message":{"role":"toolResult","toolCallId":"t1","content":[{"type":"text","text":"ok"}]}}"#, "\n",
                r#"{"type":"thinking_level_change","thinkingLevel":"medium"}"#, "\n",
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"Fixed."}]}}"#, "\n",
            ),
        )
        .unwrap();
        let e = pi_transcript_entries(&path).unwrap();
        let flat: Vec<(&str, &str)> = e.iter().map(|x| (x.role, x.text.as_str())).collect();
        assert_eq!(
            flat,
            vec![
                ("user", "fix the bug"),
                ("assistant", "On it."),
                ("tool", "bash(cargo build --release)"),
                ("assistant", "Fixed."),
            ]
        );
        // The same file answers the model question.
        assert_eq!(model_from_transcript(&path).as_deref(), Some("opus 5"));
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod transcript_pick_tests {
    use super::resume_id;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn resume_id_from_argv() {
        assert_eq!(
            resume_id(&args(
                "claude --resume 7e32fca3-76ed-4878-861f-396bcfb7071f"
            )),
            Some("7e32fca3-76ed-4878-861f-396bcfb7071f")
        );
        assert_eq!(resume_id(&args("claude -r deadbeef01")), Some("deadbeef01"));
    }

    #[test]
    fn resume_without_id_is_none() {
        // Bare --resume opens the interactive picker; the next token (if
        // any) is not a session id.
        assert_eq!(resume_id(&args("claude --resume")), None);
        assert_eq!(resume_id(&args("claude --resume --continue")), None);
        assert_eq!(resume_id(&args("claude")), None);
        // Prompt text after -r isn't an id either.
        assert_eq!(resume_id(&args("claude -r fix_the_tests")), None);
    }
}

/// Characterization goldens for the screen-scraping status heuristics over
/// the agent corpus recordings. Two consumers: Phase 1's engine swap must
/// not silently change pty-pane detection, and Phase 2's "retire heuristics
/// for structured panes" is measured against these, not vibes.
///
/// claude.ptyrec / pi.ptyrec are flagged follow-ups in tests/corpus/MANIFEST
/// (they need a human driving the session); until they exist this test
/// covers whatever agent recordings are present and passes on none.
#[cfg(test)]
mod heuristic_characterization {
    use crate::corpus;
    use crate::protocol::{agent_from_title, title_state};

    const CLAUDE_HINT: &[&str] = &["esc to interrupt"];

    fn characterize(name: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let file = root.join(format!("tests/corpus/{name}.ptyrec"));
        if !file.exists() {
            return None;
        }
        let rec = corpus::read(std::fs::File::open(file).unwrap()).unwrap();
        let mut parser = vt100::Parser::new(rec.rows, rec.cols, 0);
        let marks: std::collections::BTreeSet<usize> =
            (1..8).map(|k| k * rec.chunks.len() / 8).collect();
        let mut out = String::new();
        for (i, chunk) in rec.chunks.iter().enumerate() {
            if chunk.kind == corpus::ChunkKind::Output {
                parser.process(&chunk.payload);
            }
            if marks.contains(&i) || i + 1 == rec.chunks.len() {
                let screen = parser.screen();
                let title = screen.title();
                out.push_str(&format!(
                    "@chunk {i:4}: title_state={:?} agent={:?} stall={:?} \
                     spinner(any)={} spinner(claude)={} bells={}\n",
                    title_state(title),
                    agent_from_title(title),
                    super::stall_match(screen, true),
                    super::spinner_row(screen, None),
                    super::spinner_row(screen, Some(CLAUDE_HINT)),
                    screen.audible_bell_count(),
                ));
            }
        }
        Some(out)
    }

    #[test]
    fn agent_corpus_heuristics_golden() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for name in ["claude", "pi"] {
            let Some(actual) = characterize(name) else {
                continue;
            };
            let path = root.join(format!("tests/golden/{name}.heuristics.txt"));
            let update = std::env::var_os("UPDATE_GOLDEN").is_some();
            if update || !path.exists() {
                std::fs::write(&path, &actual).unwrap();
                assert!(
                    update,
                    "golden {} did not exist; wrote it — review and commit, then re-run",
                    path.display()
                );
                continue;
            }
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                actual,
                "{name} heuristic verdicts drifted (UPDATE_GOLDEN=1 re-baselines)"
            );
        }
    }
}
