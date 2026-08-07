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
/// across reboots as restored scrollback. 2 MiB ≈ 15–50k rendered lines of
/// typical agent output — the phone's terminal engine renders history
/// incrementally, so a bigger ring costs replay bandwidth, not frame time.
pub const RING_CAP: usize = 2 * 1024 * 1024;

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
    parser: vt100::Parser, // bell/status tracking + graphics event source
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
        // A zodiac server started from inside a claude session would pass
        // claude's session markers into every pane — and a claude launched
        // in such a pane then thinks it's a child session and stops saving
        // transcripts (which also breaks model naming and Alt+⇧R resume).
        // Panes are top-level terminals; scrub the inherited markers.
        cmd.env_remove("CLAUDECODE");
        cmd.env_remove("CLAUDE_CODE_CHILD_SESSION");
        cmd.env_remove("CLAUDE_CODE_SSE_PORT");
        cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
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
            renamed: false,
            model_mtime: None,
            model_name: None,
            ring: preload,
            parser,
            splitter: GfxSplitter::new(),
            gfx: GfxEngine::new(rows, cols),
            gfx_pushed: 0,
            queries: QueryScanner::new(),
            bell_count: 0,
            last_output: None,
            last_title_working: None,
            agent_memo: std::cell::RefCell::new(None),
            ssh_memo: std::cell::RefCell::new(None),
            activity: false,
            attention: false,
            auto_resume: true,
            prev_status: "idle",
            created_at: Instant::now(),
            stall_since: None,
            stall_fired: None,
            stall_latched: false,
            monitor_screen_hash: None,
            monitor_screen_since: None,
            monitor_checked_at: None,
            monitor_last_reason: None,
            subtitle: None,
            subtitle_hash: None,
            subtitle_checked_at: None,
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
        if !replies.is_empty() {
            self.write_input(&replies);
        }
        self.last_output = Some(Instant::now());
        if crate::protocol::title_state(&self.title()) == crate::protocol::TitleState::Working {
            self.last_title_working = Some(Instant::now());
        }
        let count = self.parser.screen().audible_bell_count();
        let new = count > self.bell_count;
        self.bell_count = count;
        (out, new)
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
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(cell.0),
            pixel_height: rows.saturating_mul(cell.1),
        });
    }

    pub fn title(&self) -> String {
        self.parser.screen().title().to_string()
    }

    pub fn screen_text(&self) -> String {
        self.parser.screen().contents()
    }

    /// The tail of the current screen as plain text, trailing blank rows
    /// dropped — a compact excerpt for the background classifier. `None` if
    /// the tail is blank. The server-side parser keeps no scrollback, so
    /// this only ever sees what's currently visible.
    pub fn tail_text(&self, want: usize) -> Option<String> {
        let screen = self.parser.screen();
        let (_, cols) = screen.size();
        let rows: Vec<String> = screen.rows(0, cols).map(|r| r.trim_end().to_string()).collect();
        let end = rows.iter().rposition(|r| !r.is_empty()).map_or(0, |i| i + 1);
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
            TitleState::Unknown => recent && self.agent().is_some(),
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
        stall_match(self.parser.screen(), conn_watch)
    }

    /// Which agent runs in this pane: title patterns first (cheap, always
    /// fresh), then a process-tree walk memoized for a couple of seconds.
    pub fn agent(&self) -> Option<String> {
        if let Some(a) = crate::protocol::agent_from_title(&self.title()) {
            return Some(a.to_string());
        }
        if let Some((t, v)) = self.agent_memo.borrow().clone() {
            if t.elapsed() < Duration::from_secs(2) {
                return v;
            }
        }
        let v = self.pid.and_then(detect_agent_process);
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
        let v = self.pid.and_then(detect_ssh_process);
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
        let pid = self.pid?;
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

    /// Claude Code's session ("chat") id for this pane: the newest transcript
    /// file's stem in the project dir for the pane's cwd. `claude --resume
    /// <id>` in that directory reopens exactly this conversation, which is
    /// what makes the session snapshot restorable across a reboot.
    pub fn chat_id(&self) -> Option<String> {
        let cwd = self.cwd()?;
        let (path, _) = self.claude_transcript(&cwd)?;
        Some(path.file_stem()?.to_str()?.to_string())
    }

    /// The short model name for whatever agent is running here, if any.
    pub fn model(&mut self) -> Option<String> {
        let agent = self.agent()?;
        self.agent_model(&agent)
    }

    /// The model the agent in this pane currently runs, as a short display
    /// name (`fable 5`, `sonnet 4.5`) — only for agents whose model is
    /// discoverable: claude via its session transcripts, opencode via its
    /// on-screen footer.
    fn agent_model(&mut self, agent: &str) -> Option<String> {
        match agent {
            "claude" => self.claude_model(),
            "opencode" => self.opencode_model(),
            _ => None,
        }
    }

    /// Claude Code writes each assistant turn's `model` into its session
    /// transcript under ~/.claude/projects/<munged-cwd>/. The newest .jsonl
    /// for the pane's cwd is almost certainly this pane's session (multiple
    /// claude panes in one directory share a project dir — the most recently
    /// written one wins, which is right whenever they run the same model and
    /// a harmless near-miss when they don't). Cached by file mtime so the
    /// 1s auto-name tick doesn't re-read an idle transcript.
    fn claude_model(&mut self) -> Option<String> {
        let cwd = self.cwd()?;
        let (path, mtime) = self.claude_transcript(&cwd)?;
        if self.model_mtime == Some(mtime) {
            return self.model_name.clone();
        }
        let name = model_from_transcript(&path);
        self.model_mtime = Some(mtime);
        self.model_name = name.clone();
        name
    }

    /// The claude process serving this pane, with its argv — found among
    /// the shell's descendants, same match as agent detection.
    fn claude_proc(&self) -> Option<(u32, Vec<String>)> {
        let root = self.pid?;
        descendant_procs(root).into_iter().find(|(_, args)| {
            args.iter()
                .take(2)
                .any(|a| a.rsplit('/').next() == Some("claude"))
        })
    }

    /// Which transcript is *this pane's* claude session? Two claude panes
    /// in the same directory share a project folder, and "newest by mtime"
    /// labels both panes with whichever session wrote last — wrong whenever
    /// they run different models. Stronger signals first:
    ///
    ///  1. `claude --resume <id>` carries the session id in argv — exact.
    ///  2. A fresh `claude` creates its transcript within moments of the
    ///     process starting — match file birth time to process start.
    ///  3. Otherwise (resume via the interactive picker, filesystems
    ///     without birth times): newest by mtime, the old near-miss.
    fn claude_transcript(&self, cwd: &str) -> Option<(std::path::PathBuf, std::time::SystemTime)> {
        let dir = claude_project_dir(cwd)?;
        let proc = self.claude_proc();
        if let Some((_, args)) = &proc {
            if let Some(id) = resume_id(args) {
                let path = dir.join(format!("{id}.jsonl"));
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(mtime) = meta.modified() {
                        return Some((path, mtime));
                    }
                }
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
        let fd = self.master.as_raw_fd()?;
        let pgid = unsafe { libc::tcgetpgrp(fd) };
        if pgid <= 0 {
            return None;
        }
        if Some(pgid as u32) == self.pid {
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
        // Wire values again — cap to something a real terminal could be
        // and keep the pixel products from overflowing u16.
        let (rows, cols) = (rows.min(1000), cols.min(1000));
        if rows < 2 || cols < 10 || self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(self.cell.0),
            pixel_height: rows.saturating_mul(self.cell.1),
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

/// Claude Code's project directory for a working directory:
/// ~/.claude/projects/<munged>, where the name is the cwd with every
/// non-alphanumeric character turned into `-`.
fn claude_project_dir(cwd: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let munged: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(std::path::Path::new(&home).join(".claude/projects").join(munged))
}

/// The session id from a `claude --resume <id>` argv, if present.
fn resume_id(args: &[String]) -> Option<&str> {
    let at = args.iter().position(|a| a == "--resume" || a == "-r")?;
    let id = args.get(at + 1)?;
    // A session id, not a following flag or prompt text.
    (id.len() >= 8 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .then_some(id.as_str())
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
    let mut best: Option<(std::path::PathBuf, std::time::SystemTime, std::time::Duration)> = None;
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
    std::fs::metadata(format!("/proc/{pid}")).ok()?.modified().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_time(_pid: u32) -> Option<std::time::SystemTime> {
    None
}

/// The last `"model": "..."` value in the tail of a session transcript —
/// assistant turns carry the model that produced them, so the last one is
/// the session's current selection (and tracks /model switches). Reads at
/// most the final 64 KB.
fn model_from_transcript(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(64 * 1024))).ok()?;
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
            if id.as_bytes()[cut] == b'-'
                && id[cut + 1..].chars().all(|c| c.is_ascii_digit()) =>
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
        libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if n <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    path.rsplit('/').next().filter(|s| !s.is_empty()).map(str::to_string)
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
        let base = args
            .first()
            .map(|a| a.rsplit('/').next().unwrap_or(a));
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
        let Some(flag) = arg.strip_prefix('-') else { break };
        i += if flag.len() == 1 && VALUE_FLAGS.contains(flag) { 2 } else { 1 };
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
        assert_eq!(ssh_destination(&args("-tt -v des@bigbox")), Some("bigbox".into()));
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
mod transcript_pick_tests {
    use super::resume_id;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn resume_id_from_argv() {
        assert_eq!(
            resume_id(&args("claude --resume 7e32fca3-76ed-4878-861f-396bcfb7071f")),
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
