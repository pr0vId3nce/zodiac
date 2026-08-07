use std::collections::HashMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::pane::SrvPane;
use crate::protocol::*;
use crate::snapshot::{self, SnapPane, Snapshot};

pub enum SrvEvent {
    Conn(UnixStream),
    Client(u64, Frame),
    ClientGone(u64),
    Output(u64, Vec<u8>),
    /// Input for a pane from a server-side helper thread (autoresume pacing).
    Deliver(u64, Vec<u8>),
    /// Result of an async `<agent> --version` probe (agent name, first line).
    AgentVersion(String, String),
    /// Result of an async background classification (pane id, pane name at
    /// the time it was sent, the model's verdict). See `monitor.rs`.
    MonitorVerdict(u64, String, crate::monitor::Verdict),
    /// Result of an async background subtitle summarization (pane id, the
    /// ≤6-word summary). See `monitor.rs`.
    MonitorSubtitle(u64, String),
    Exited(u64),
}

#[derive(Serialize, Deserialize, Default)]
struct SavedState {
    active: usize,
    panes: Vec<SavedPane>,
}

#[derive(Serialize, Deserialize)]
struct SavedPane {
    name: String,
    cwd: Option<String>,
    #[serde(default = "default_true")]
    auto_resume: bool,
    /// Agent running here at the last save, if any — decides whether the
    /// restore banner is worth showing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    /// The user pinned this name with Alt+R — auto-naming skips it.
    #[serde(default)]
    renamed: bool,
}

fn default_true() -> bool {
    true
}

/// Tail of every restored pane's replayed scrollback. The recorded bytes can
/// end mid-alt-screen (vim, less, an agent TUI), and the client's parser would
/// otherwise stay there and paint the new shell's prompt over the old frame.
const RESTORE_RESET: &[u8] = b"\r\n\x1b[0m\x1b[?1049l\x1b[?25h";

/// Only panes that were running an agent get a visible banner: a restored
/// shell reads as a fresh shell anyway, but a restored agent looks exactly
/// like one still sitting at its transcript, waiting for input.
fn restore_banner(agent: &str) -> Vec<u8> {
    format!("\x1b[7m zodiac: restored session — {agent} was not preserved \x1b[0m\r\n").into_bytes()
}

/// One client connection's write side: frames go to a per-connection
/// writer thread through a bounded queue, so a stalled peer (suspended
/// bridge, asleep phone mid-transfer) can never block the event loop —
/// it just fills its queue and gets dropped.
struct ConnWriter {
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Kept for shutdown(): closing both halves unblocks the writer
    /// thread mid-`write_all` and stops the reader thread.
    stream: UnixStream,
}

/// Frames a connection may fall behind by before it's declared dead. An
/// attach replay is ~a dozen frames; steady state is a handful in flight —
/// hundreds queued means the peer stopped reading.
const CONN_QUEUE: usize = 512;

struct Server {
    session: String,
    panes: Vec<SrvPane>,
    active: u64,
    next_id: u64,
    size: (u16, u16),
    conns: HashMap<u64, ConnWriter>,
    ui: Option<u64>,
    /// Read-only observers (T_WATCH): they receive replay + live output +
    /// pane open/close, but never graphics, and attach never kicks them.
    watchers: std::collections::HashSet<u64>,
    next_gen: u64,
    tx: Sender<SrvEvent>,
    dirty: bool,
    quit: bool,
    resized_at: Option<Instant>,
    /// Agent name → first line of `--version`. None = probe in flight.
    versions: HashMap<String, Option<String>>,
    /// Outer-terminal cell size (px) and kitty-graphics capability, as
    /// reported by the attached client. Kept across detach so background
    /// panes keep tracking graphics for the next attach.
    client_cell: (u16, u16),
    client_gfx: bool,
    /// Image payloads already delivered this attach: (pane, image, ver).
    gfx_sent: std::collections::HashSet<(u64, u32, u32)>,
    /// Random secret minted on this session's first-ever launch and
    /// persisted in the state dir — see `load_or_mint_pairing_token`. Rides
    /// along on every `SessionState` broadcast so the astrolabe bridge (and,
    /// via it, the phone) can authenticate; surviving restarts means a phone
    /// paired once stays paired.
    pairing_token: String,
}

/// 16 random bytes, hex-encoded. `/dev/urandom` rather than pulling in a
/// crate — this only ever runs on the Linux hosts zodiac actually deploys
/// to.
fn gen_pairing_token() -> String {
    use std::io::Read as _;
    let mut buf = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_err()
    {
        // Never observed in practice, but a broken source is better than a
        // panic — falls back to a low-entropy but still per-process value.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seed = (std::process::id() as u128) ^ nanos;
        buf.copy_from_slice(&seed.to_le_bytes()[..16]);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The session's pairing token, persisted at
/// `<state_dir>/<session>/pairing_token` (0600) so phones paired once stay
/// paired across server restarts. Minted on first launch; deleting the file
/// revokes every paired phone — the next launch mints a fresh token and
/// each phone re-scans the Alt+P QR.
fn load_or_mint_pairing_token(session: &str) -> String {
    let path = crate::protocol::state_dir(session).join("pairing_token");
    if let Ok(tok) = std::fs::read_to_string(&path) {
        let tok = tok.trim().to_string();
        if tok.len() >= 32 && tok.chars().all(|c| c.is_ascii_hexdigit()) {
            return tok;
        }
    }
    let tok = gen_pairing_token();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, &tok);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tok
}

/// Output arriving this soon after a resize is treated as the SIGWINCH
/// repaint storm — every inner app redraws at once — not as agent activity,
/// or a sidebar toggle would light up every pane's working spinner.
const RESIZE_SQUELCH: Duration = Duration::from_millis(1200);

pub fn run(session: &str) -> Result<()> {
    let sock = socket_path(session);
    if let Some(dir) = sock.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if UnixStream::connect(&sock).is_ok() {
        bail!("zodiac server already running for session '{session}'");
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;

    // Save state before dying on reboot/logout.
    let term = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, term.clone())?;
    }

    let (tx, rx) = channel();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                if tx.send(SrvEvent::Conn(conn)).is_err() {
                    break;
                }
            }
        });
    }

    let mut srv = Server {
        session: session.to_string(),
        panes: Vec::new(),
        active: 0,
        next_id: 0,
        size: (24, 80),
        conns: HashMap::new(),
        ui: None,
        watchers: std::collections::HashSet::new(),
        next_gen: 0,
        tx,
        dirty: false,
        quit: false,
        resized_at: None,
        versions: HashMap::new(),
        client_cell: (0, 0),
        client_gfx: false,
        gfx_sent: std::collections::HashSet::new(),
        pairing_token: load_or_mint_pairing_token(session),
    };
    srv.restore()?;
    srv.save_meta();
    // The snapshot on disk describes the session as it was before this
    // process started — exactly what Alt+⇧R wants. Keep it as
    // `snapshot.prev.json` before the fresh (agent-less) one overwrites it.
    snapshot::rotate(&srv.session);
    srv.save_snapshot();

    let mut last_ring_save = Instant::now();
    let mut last_stall_check = Instant::now();
    while !srv.quit && !term.load(Ordering::Relaxed) {
        // 1s cap so the watchdog/finish ticks stay timely even when every
        // pane is quiet (no events to wake the loop).
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(ev) => {
                srv.handle(ev);
                let mut drained = 0;
                while let Ok(ev) = rx.try_recv() {
                    srv.handle(ev);
                    drained += 1;
                    if srv.quit || drained > 4096 {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_stall_check.elapsed() >= Duration::from_secs(1) {
            last_stall_check = Instant::now();
            srv.autoresume_tick();
            srv.finish_tick();
            srv.monitor_tick();
            srv.subtitle_tick();
            srv.auto_name_tick();
        }
        if srv.dirty {
            srv.save_meta();
            // Rings are keyed by pane *position*, so any structural change
            // (close/move/rename marks dirty) must rewrite them in the same
            // breath — meta saved now + rings from a minute ago restores
            // the wrong scrollback into the wrong pane after a hard death.
            srv.save_rings();
            last_ring_save = Instant::now();
        }
        if last_ring_save.elapsed() > Duration::from_secs(60) {
            srv.save_rings();
            // Re-save meta alongside the rings: `dirty` only tracks pane
            // add/remove/rename, so an agent launched inside an existing pane
            // would otherwise be missing from state.json if we died hard.
            srv.save_meta();
            srv.save_snapshot();
            last_ring_save = Instant::now();
        }
    }

    srv.save_meta();
    srv.save_rings();
    srv.save_snapshot();
    srv.send_ui(T_SERVER_EXIT, 0, &[]);
    srv.send_watchers(T_SERVER_EXIT, 0, &[]);
    for p in &mut srv.panes {
        p.kill();
    }
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

impl Server {
    fn pane_mut(&mut self, id: u64) -> Option<&mut SrvPane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn idx(&self, id: u64) -> Option<usize> {
        self.panes.iter().position(|p| p.id == id)
    }

    fn drop_conn(&mut self, gen: u64) {
        if let Some(c) = self.conns.remove(&gen) {
            // Dropping `tx` ends the writer thread once its queue drains;
            // the shutdown unblocks it immediately if it's stuck in
            // write_all, and stops the reader thread too.
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
        if self.ui == Some(gen) {
            self.ui = None;
        }
        self.watchers.remove(&gen);
    }

    fn reply(&mut self, gen: u64, typ: u8, id: u64, data: &[u8]) {
        let dead = match self.conns.get(&gen) {
            // try_send never blocks: a peer that stopped reading fills its
            // queue and is dropped here instead of wedging the event loop.
            Some(c) => c.tx.try_send(encode_frame(typ, id, data)).is_err(),
            None => false,
        };
        if dead {
            self.drop_conn(gen);
        }
    }

    fn send_ui(&mut self, typ: u8, id: u64, data: &[u8]) {
        if let Some(gen) = self.ui {
            self.reply(gen, typ, id, data);
        }
    }

    fn send_watchers(&mut self, typ: u8, id: u64, data: &[u8]) {
        for gen in self.watchers.iter().copied().collect::<Vec<_>>() {
            self.reply(gen, typ, id, data);
        }
    }

    fn handle(&mut self, ev: SrvEvent) {
        match ev {
            SrvEvent::Conn(stream) => {
                self.next_gen += 1;
                let gen = self.next_gen;
                let Ok(mut rd) = stream.try_clone() else {
                    return;
                };
                let Ok(mut wr) = stream.try_clone() else {
                    return;
                };
                let tx = self.tx.clone();
                std::thread::spawn(move || loop {
                    match read_frame(&mut rd) {
                        Ok(f) => {
                            if tx.send(SrvEvent::Client(gen, f)).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(SrvEvent::ClientGone(gen));
                            break;
                        }
                    }
                });
                // Writer thread: drains this connection's bounded queue so
                // socket writes never run on the event loop. Exits when the
                // queue's sender is dropped (drop_conn) or a write fails.
                let (wtx, wrx) = std::sync::mpsc::sync_channel::<Vec<u8>>(CONN_QUEUE);
                let wtx_evt = self.tx.clone();
                std::thread::spawn(move || {
                    use std::io::Write as _;
                    while let Ok(buf) = wrx.recv() {
                        if wr.write_all(&buf).and_then(|_| wr.flush()).is_err() {
                            let _ = wtx_evt.send(SrvEvent::ClientGone(gen));
                            break;
                        }
                    }
                });
                self.conns.insert(gen, ConnWriter { tx: wtx, stream });
            }
            SrvEvent::ClientGone(gen) => self.drop_conn(gen),
            SrvEvent::Client(gen, frame) => self.client_frame(gen, frame),
            SrvEvent::Output(id, bytes) => {
                let watched = self.ui.is_some() && self.active == id;
                let detached = self.ui.is_none();
                let session = self.session.clone();
                let squelch = self
                    .resized_at
                    .is_some_and(|t| t.elapsed() < RESIZE_SQUELCH);
                let mut stream = Vec::new();
                if let Some(p) = self.pane_mut(id) {
                    let prev = p.last_output;
                    let (out, bell) = p.process_output(&bytes);
                    stream = out;
                    if squelch {
                        p.last_output = prev;
                    }
                    if !watched {
                        p.activity = !squelch || p.activity;
                        if bell && !p.attention {
                            p.attention = true;
                            if detached {
                                notify(
                                    &format!("{} needs attention", p.name),
                                    &format!("zodiac session '{session}' (detached)"),
                                );
                            }
                        }
                    }
                }
                if !stream.is_empty() {
                    self.send_ui(T_OUTPUT, id, &stream);
                    self.send_watchers(T_OUTPUT, id, &stream);
                }
                self.push_gfx(id);
            }
            SrvEvent::Deliver(id, bytes) => {
                if let Some(p) = self.pane_mut(id) {
                    p.write_input(&bytes);
                }
            }
            SrvEvent::AgentVersion(agent, version) => {
                self.versions.insert(agent, Some(version));
            }
            SrvEvent::MonitorVerdict(id, name, verdict) => {
                self.handle_monitor_verdict(id, name, verdict)
            }
            SrvEvent::MonitorSubtitle(id, subtitle) => {
                if let Some(p) = self.pane_mut(id) {
                    p.subtitle = Some(subtitle);
                }
            }
            SrvEvent::Exited(id) => self.remove_pane(id),
        }
    }

    fn client_frame(&mut self, gen: u64, f: Frame) {
        match f.typ {
            T_ATTACH => {
                // payload (new clients): [gfx capable u8, cell w u16, cell h u16]
                if f.data.len() >= 5 {
                    self.client_gfx = f.data[0] != 0;
                    self.client_cell = (
                        u16::from_le_bytes([f.data[1], f.data[2]]),
                        u16::from_le_bytes([f.data[3], f.data[4]]),
                    );
                }
                self.attach(gen);
            }
            T_QUERY => {
                self.probe_versions();
                let data = serde_json::to_vec(&self.state()).unwrap_or_default();
                self.reply(gen, T_STATE, 0, &data);
            }
            T_WATCH => {
                // Observer hello, mirroring attach(): state, then every
                // pane's replay ring. No graphics, no flag clearing, and
                // the connection stays alongside the attached UI.
                self.watchers.insert(gen);
                self.probe_versions();
                let data = serde_json::to_vec(&self.state()).unwrap_or_default();
                self.reply(gen, T_STATE, 0, &data);
                let rings: Vec<(u64, Vec<u8>)> =
                    self.panes.iter().map(|p| (p.id, p.ring.clone())).collect();
                for (id, ring) in rings {
                    self.reply(gen, T_REPLAY, id, &ring);
                }
            }
            T_READ_SCREEN => {
                let text = self
                    .panes
                    .iter()
                    .find(|p| p.id == f.id)
                    .map(|p| p.screen_text())
                    .unwrap_or_default();
                self.reply(gen, T_SCREEN, f.id, text.as_bytes());
            }
            T_INPUT => {
                if let Some(p) = self.pane_mut(f.id) {
                    p.write_input(&f.data);
                }
            }
            T_RESTORE => self.restore_agents(),
            T_MOUSE => {
                // Mouse reports reach a pane only while a real application
                // owns its pty. The client already checks that the pane's
                // emulator asked for mouse reporting, but that flag is
                // sticky: a program that enabled tracking and died without
                // turning it back off (crash, kill -9) would leave the pane
                // in mouse mode for good, and every pointer twitch would be
                // typed at the shell prompt as `35;8;19M`-shaped junk.
                if let Some(p) = self.pane_mut(f.id) {
                    if p.app_foreground() {
                        p.write_input(&f.data);
                    }
                }
            }
            T_RESIZE => {
                if f.data.len() >= 4 {
                    let rows = u16::from_le_bytes([f.data[0], f.data[1]]);
                    let cols = u16::from_le_bytes([f.data[2], f.data[3]]);
                    if f.data.len() >= 8 {
                        self.client_cell = (
                            u16::from_le_bytes([f.data[4], f.data[5]]),
                            u16::from_le_bytes([f.data[6], f.data[7]]),
                        );
                    }
                    if self.size != (rows, cols) {
                        self.resized_at = Some(Instant::now());
                    }
                    self.size = (rows, cols);
                    let cell = self.client_cell;
                    for p in &mut self.panes {
                        p.set_cell(cell);
                        p.resize(rows, cols);
                    }
                }
            }
            T_NEW_PANE => {
                let _ = self.new_pane(None, None, Vec::new(), true);
            }
            T_CLOSE_PANE => {
                if let Some(p) = self.pane_mut(f.id) {
                    p.kill();
                }
                self.remove_pane(f.id);
            }
            T_RENAME => {
                if let Ok(name) = String::from_utf8(f.data.clone()) {
                    let mut announce: Option<String> = None;
                    if let Some(p) = self.pane_mut(f.id) {
                        let trimmed = name.trim();
                        if trimmed.is_empty() {
                            // Empty rename clears the pin — back to auto,
                            // effective immediately.
                            p.renamed = false;
                            if let Some(auto) = p.auto_name() {
                                p.name = auto.clone();
                                announce = Some(auto);
                            }
                        } else {
                            p.name = trimmed.to_string();
                            p.renamed = true;
                        }
                        self.dirty = true;
                    }
                    if let Some(n) = announce {
                        self.send_ui(T_PANE_RENAMED, f.id, n.as_bytes());
                        self.send_watchers(T_PANE_RENAMED, f.id, n.as_bytes());
                    }
                }
            }
            T_MOVE => {
                if let (Some(i), Some(&dir)) = (self.idx(f.id), f.data.first()) {
                    if dir == 0 && i > 0 {
                        self.panes.swap(i, i - 1);
                        self.dirty = true;
                    } else if dir == 1 && i + 1 < self.panes.len() {
                        self.panes.swap(i, i + 1);
                        self.dirty = true;
                    }
                }
            }
            T_FOCUS => {
                self.active = f.id;
                self.dirty = true;
                if let Some(p) = self.pane_mut(f.id) {
                    p.clear_flags();
                }
            }
            T_AUTORESUME => {
                if let Some(on) = f.data.first().map(|b| *b != 0) {
                    if let Some(p) = self.pane_mut(f.id) {
                        p.auto_resume = on;
                        self.dirty = true;
                    }
                }
            }
            T_DETACH => self.drop_conn(gen),
            T_SHUTDOWN => self.quit = true,
            _ => {}
        }
    }

    /// Scan panes for Claude Code API stalls and auto-send Esc + `--resume`.
    fn autoresume_tick(&mut self) {
        let session = self.session.clone();
        let tx = self.tx.clone();
        // Re-read each tick so a settings-page toggle applies live.
        let conn_watch = crate::settings::Settings::cached().connection_watch;
        for p in &mut self.panes {
            if p.stall_due(conn_watch) {
                notify(
                    &format!("auto-resumed '{}'", p.name),
                    &format!("zodiac '{session}': API stall detected — sent Esc + --resume"),
                );
                p.fire_autoresume(tx.clone());
            }
        }
    }

    /// Ring the finish sound when a pane's agent goes working → done.
    /// `status()` only reads "done" from the sticky activity flag, which is
    /// never set for the pane the attached UI is watching — so this fires
    /// exactly for background panes, and for every pane when detached,
    /// matching the sidebar's green "finished" state. The sound file is
    /// re-resolved from settings each time, so the settings-page picker
    /// applies live.
    fn finish_tick(&mut self) {
        let mut finished = false;
        for p in &mut self.panes {
            let status = p.status();
            finished |= p.prev_status == "working" && status == "done";
            p.prev_status = status;
        }
        if finished {
            if let Some(path) = crate::settings::Settings::cached().finish_sound_path() {
                play_sound(&path);
            }
        }
    }

    /// Headless sentinel: notice panes that are stuck (needs_input, or
    /// "working" with a screen that hasn't actually moved in a while) and
    /// kick off a background classification for each. Advisory only — this
    /// never writes to a pane, only logs and notifies. Re-read from settings
    /// every tick so the on/off toggle applies live.
    fn monitor_tick(&mut self) {
        let settings = crate::settings::Settings::cached();
        if !settings.pane_monitor {
            return;
        }
        // Blank endpoint = monitor disabled entirely — a fresh install must
        // never phone anyone's private model box.
        let Some(cfg) = crate::monitor::cfg_from_settings(&settings) else {
            return;
        };
        let now = Instant::now();
        let policy = crate::monitor::policy_text();
        let session = self.session.clone();
        let tx = self.tx.clone();
        for p in &mut self.panes {
            let status = p.status();
            let hash = p.screen_hash();
            let stable_since = match p.monitor_screen_hash {
                Some(h) if h == hash => p.monitor_screen_since,
                _ => {
                    p.monitor_screen_hash = Some(hash);
                    p.monitor_screen_since = Some(now);
                    Some(now)
                }
            };
            let stalled_working = status == "working"
                && stable_since.is_some_and(|t| now.duration_since(t) >= crate::monitor::STALL_WORKING_AFTER);
            if status != "needs_input" && !stalled_working {
                continue;
            }
            if p.monitor_checked_at
                .is_some_and(|t| now.duration_since(t) < crate::monitor::RECHECK_INTERVAL)
            {
                continue;
            }
            let Some(screen) = p.tail_text(20) else { continue };
            p.monitor_checked_at = Some(now);
            crate::monitor::classify_async(
                cfg.clone(),
                tx.clone(),
                session.clone(),
                p.id,
                p.name.clone(),
                screen,
                policy.clone(),
            );
        }
    }

    /// A background classification came back: log already happened on the
    /// worker thread (see `monitor::classify_async`); here we just decide
    /// whether it's worth interrupting you about, deduping on the pane's
    /// last reason so a still-stuck pane doesn't renotify every 90s with the
    /// same text.
    fn handle_monitor_verdict(&mut self, id: u64, name: String, v: crate::monitor::Verdict) {
        if matches!(v.classification.as_str(), "actually_working" | "finished") {
            return;
        }
        let quiet = self
            .pane_mut(id)
            .is_some_and(|p| p.monitor_last_reason.as_deref() == Some(v.reason.as_str()));
        if quiet {
            return;
        }
        if let Some(p) = self.pane_mut(id) {
            p.monitor_last_reason = Some(v.reason.clone());
        }
        let body = match v.policy_verdict.as_str() {
            "safe" => format!("{} — matches your policy: {}", v.reason, v.policy_reason),
            "unsafe" => format!("{} — NOT on your safe list: {}", v.reason, v.policy_reason),
            _ => v.reason.clone(),
        };
        notify(&format!("{name}: {}", v.classification), &body);
    }

    /// Tier 3.1: keep each active pane's card subtitle fresh. Only panes
    /// running an agent get one; a pane's screen hash is cached so an idle
    /// pane costs nothing beyond a local hash compute once every
    /// `SUBTITLE_INTERVAL` — no request goes out unless the screen actually
    /// changed since the last summarize.
    /// Auto-naming: un-pinned panes track what's happening inside them —
    /// the agent, the ssh destination, the foreground app, or the shell's
    /// cwd basename (see `SrvPane::auto_name`). Runs on the 1s tick; a
    /// changed name is announced to the UI client and every watcher via
    /// T_PANE_RENAMED.
    fn auto_name_tick(&mut self) {
        let mut announce: Vec<(u64, String)> = Vec::new();
        for p in &mut self.panes {
            if p.renamed {
                continue;
            }
            let Some(name) = p.auto_name() else { continue };
            if name != p.name {
                p.name = name.clone();
                announce.push((p.id, name));
            }
        }
        if !announce.is_empty() {
            self.dirty = true;
        }
        for (id, name) in announce {
            self.send_ui(T_PANE_RENAMED, id, name.as_bytes());
            self.send_watchers(T_PANE_RENAMED, id, name.as_bytes());
        }
    }

    fn subtitle_tick(&mut self) {
        let settings = crate::settings::Settings::cached();
        if !settings.pane_monitor {
            return;
        }
        let Some(cfg) = crate::monitor::cfg_from_settings(&settings) else {
            return;
        };
        let now = Instant::now();
        let tx = self.tx.clone();
        for p in &mut self.panes {
            if p.agent().is_none() {
                continue;
            }
            if p.subtitle_checked_at
                .is_some_and(|t| now.duration_since(t) < crate::monitor::SUBTITLE_INTERVAL)
            {
                continue;
            }
            p.subtitle_checked_at = Some(now);
            let hash = p.screen_hash();
            if p.subtitle_hash == Some(hash) {
                continue;
            }
            p.subtitle_hash = Some(hash);
            let Some(screen) = p.tail_text(20) else { continue };
            crate::monitor::summarize_async(cfg.clone(), tx.clone(), p.id, screen);
        }
    }

    fn attach(&mut self, gen: u64) {
        if let Some(old) = self.ui {
            if old != gen {
                self.drop_conn(old);
            }
        }
        self.ui = Some(gen);

        let hello = Hello {
            active: self.active,
            mouse_gate: true,
            panes: self
                .panes
                .iter()
                .map(|p| HelloPane {
                    id: p.id,
                    name: p.name.clone(),
                    activity: p.activity,
                    attention: p.attention,
                    last_ms: p.last_ms(),
                })
                .collect(),
        };
        let data = serde_json::to_vec(&hello).unwrap_or_default();
        self.reply(gen, T_HELLO, 0, &data);
        let rings: Vec<(u64, Vec<u8>)> =
            self.panes.iter().map(|p| (p.id, p.ring.clone())).collect();
        for (id, ring) in rings {
            self.reply(gen, T_REPLAY, id, &ring);
        }
        // Fresh client: propagate its cell size / capability and resend
        // graphics state from scratch (image payloads + snapshots).
        self.gfx_sent.clear();
        let cell = self.client_cell;
        let gfx = self.client_gfx;
        let ids: Vec<u64> = self.panes.iter().map(|p| p.id).collect();
        for id in &ids {
            if let Some(p) = self.pane_mut(*id) {
                p.set_cell(cell);
                p.gfx.active = gfx;
                p.gfx_pushed = u64::MAX;
            }
        }
        for id in ids {
            self.push_gfx(id);
        }
        let active = self.active;
        if let Some(p) = self.pane_mut(active) {
            p.clear_flags();
        }
    }

    /// Deliver pane `id`'s graphics state to the UI when it changed: first
    /// any image payloads the client hasn't seen this attach (chunked), then
    /// the placement snapshot. Pixels flow only here — never in T_OUTPUT.
    fn push_gfx(&mut self, id: u64) {
        let Some(gen) = self.ui else { return };
        if !self.client_gfx {
            return;
        }
        let Some(p) = self.panes.iter().find(|p| p.id == id) else {
            return;
        };
        if p.gfx.version == p.gfx_pushed
            || (p.gfx_pushed == u64::MAX && p.gfx.version == 0)
        {
            if p.gfx.version == 0 {
                if let Some(p) = self.pane_mut(id) {
                    p.gfx_pushed = 0;
                }
            }
            return;
        }
        let snap = p.gfx.snapshot();
        let version = p.gfx.version;
        let need: Vec<(u32, u32)> = snap
            .images
            .iter()
            .copied()
            .filter(|(img, ver)| !self.gfx_sent.contains(&(id, *img, *ver)))
            .collect();
        for (img_id, ver) in need {
            let mut off = 0usize;
            loop {
                // re-borrow per chunk: reply() needs &mut self
                let Some(p) = self.panes.iter().find(|p| p.id == id) else {
                    return;
                };
                let Some(img) = p.gfx.image(img_id) else { break };
                let total = img.data.len();
                let end = (off + GFX_CHUNK).min(total);
                let hdr = GfxImgHdr {
                    img: img_id,
                    ver,
                    format: img.format,
                    zlib: img.zlib,
                    w: img.w,
                    h: img.h,
                    off: off as u32,
                    total: total as u32,
                }
                .encode();
                let mut data = Vec::with_capacity(GFX_IMG_HDR + end - off);
                data.extend_from_slice(&hdr);
                data.extend_from_slice(&img.data[off..end]);
                self.reply(gen, T_GFX_IMG, id, &data);
                off = end;
                if off >= total {
                    break;
                }
            }
            self.gfx_sent.insert((id, img_id, ver));
        }
        let json = serde_json::to_vec(&snap).unwrap_or_default();
        self.reply(gen, T_GFX_STATE, id, &json);
        if let Some(p) = self.pane_mut(id) {
            p.gfx_pushed = version;
        }
    }

    /// Kick off `--version` probes for any agent seen in a pane that we
    /// haven't asked yet. Runs on a thread so a slow binary can't stall the
    /// server; the result comes back as an AgentVersion event.
    fn probe_versions(&mut self) {
        let agents: Vec<String> = self.panes.iter().filter_map(|p| p.agent()).collect();
        for agent in agents {
            if self.versions.contains_key(&agent) {
                continue;
            }
            self.versions.insert(agent.clone(), None);
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let out = std::process::Command::new(&agent)
                    .arg("--version")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default();
                let line = out.lines().next().unwrap_or("").trim().to_string();
                let _ = tx.send(SrvEvent::AgentVersion(agent, line));
            });
        }
    }

    fn state(&self) -> SessionState {
        SessionState {
            session: self.session.clone(),
            attached: self.ui.is_some(),
            rows: self.size.0,
            cols: self.size.1,
            panes: self
                .panes
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    // Resolve the agent once and render the screen once per
                    // pane per broadcast — recap and the transcript well
                    // both read from the same text.
                    let agent = p.agent();
                    let text = agent.is_some().then(|| p.screen_text());
                    PaneState {
                        index: i + 1,
                        id: p.id,
                        name: p.name.clone(),
                        title: p.title(),
                        status: p.status().to_string(),
                        cwd: p.cwd(),
                        focused: self.ui.is_some() && self.active == p.id,
                        auto_resume: p.auto_resume,
                        uptime_ms: p.uptime_ms(),
                        version: agent
                            .as_ref()
                            .and_then(|a| self.versions.get(a).cloned().flatten())
                            .filter(|v| !v.is_empty()),
                        thinking: p.thinking(),
                        recap: text.as_deref().and_then(crate::pane::recap_in),
                        subtitle: p.subtitle.clone(),
                        ssh: p.ssh_target(),
                        tail: text
                            .as_deref()
                            .map(|t| crate::pane::tail_lines_in(t, 3))
                            .unwrap_or_default(),
                        agent,
                    }
                })
                .collect(),
            host: host_vitals(),
            pairing_token: self.pairing_token.clone(),
        }
    }

    fn new_pane(
        &mut self,
        name: Option<String>,
        cwd: Option<PathBuf>,
        preload: Vec<u8>,
        announce: bool,
    ) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let (rows, cols) = self.size;
        let mut pane = SrvPane::spawn(id, name, rows, cols, cwd, preload, self.tx.clone())?;
        pane.set_cell(self.client_cell);
        pane.gfx.active = self.client_gfx;
        let pname = pane.name.clone();
        self.panes.push(pane);
        self.dirty = true;
        if announce {
            self.active = id;
            self.send_ui(T_PANE_OPENED, id, pname.as_bytes());
            self.send_watchers(T_PANE_OPENED, id, &pname.clone().into_bytes());
        }
        Ok(id)
    }

    fn remove_pane(&mut self, id: u64) {
        if let Some(i) = self.idx(id) {
            self.panes.remove(i);
            self.dirty = true;
            self.send_ui(T_PANE_CLOSED, id, &[]);
            self.send_watchers(T_PANE_CLOSED, id, &[]);
            if self.panes.is_empty() {
                self.quit = true;
            }
        }
    }

    fn restore(&mut self) -> Result<()> {
        let dir = state_dir(&self.session);
        let meta: SavedState = std::fs::read(dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        for (i, sp) in meta.panes.iter().enumerate() {
            let mut preload = std::fs::read(dir.join("scrollback").join(format!("{i}.bin")))
                .unwrap_or_default();
            if !preload.is_empty() {
                preload.extend_from_slice(RESTORE_RESET);
                if let Some(agent) = sp.agent.as_deref() {
                    preload.extend_from_slice(&restore_banner(agent));
                }
            }
            if let Ok(id) = self.new_pane(
                Some(sp.name.clone()),
                sp.cwd.clone().map(PathBuf::from),
                preload,
                false,
            ) {
                if let Some(p) = self.pane_mut(id) {
                    p.auto_resume = sp.auto_resume;
                    p.renamed = sp.renamed;
                }
            }
        }
        if self.panes.is_empty() {
            self.new_pane(None, None, Vec::new(), false)?;
        }
        let idx = meta.active.min(self.panes.len() - 1);
        self.active = self.panes[idx].id;
        Ok(())
    }

    fn save_meta(&mut self) {
        self.dirty = false;
        let dir = state_dir(&self.session);
        let _ = std::fs::create_dir_all(&dir);
        let state = SavedState {
            active: self.idx(self.active).unwrap_or(0),
            panes: self
                .panes
                .iter()
                .map(|p| SavedPane {
                    name: p.name.clone(),
                    cwd: p.cwd(),
                    auto_resume: p.auto_resume,
                    agent: p.agent(),
                    renamed: p.renamed,
                })
                .collect(),
        };
        if let Ok(json) = serde_json::to_vec_pretty(&state) {
            let tmp = dir.join("state.json.tmp");
            if std::fs::write(&tmp, &json).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join("state.json"));
            }
        }
    }

    /// Write `snapshot.json` — see `Snapshot`. Written whole to a temp file
    /// and renamed, so a restore script never reads a half-written file.
    fn save_snapshot(&mut self) {
        let session = self.session.clone();
        let active = self.idx(self.active).unwrap_or(0) + 1;
        let saved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let panes = self
            .panes
            .iter_mut()
            .enumerate()
            .map(|(i, p)| {
                let agent = p.agent();
                SnapPane {
                    index: i + 1,
                    name: p.name.clone(),
                    cwd: p.cwd(),
                    chat_id: match agent.as_deref() {
                        Some("claude") => p.chat_id(),
                        _ => None,
                    },
                    model: p.model(),
                    agent,
                    auto_resume: p.auto_resume,
                    renamed: p.renamed,
                }
            })
            .collect();
        snapshot::write(
            &self.session,
            &Snapshot {
                session,
                saved_at,
                active,
                panes,
            },
        );
    }

    /// Raise the agents from the last snapshot — Alt+⇧R in the UI, or
    /// `zodiac restore`. Each pane that was running an agent gets the
    /// command typed back into it (claude on the same conversation).
    ///
    /// Panes the snapshot had but this session doesn't are opened first.
    /// A pane is left strictly alone when it already runs that agent or has
    /// anything else in the foreground — typing a command into someone's
    /// vim is worse than skipping them.
    fn restore_agents(&mut self) {
        let Some(snap) = snapshot::best(&self.session) else {
            return;
        };
        for sp in snap.panes {
            if sp.agent.is_none() {
                continue;
            }
            // A freshly opened pane's shell needs a moment to reach its
            // prompt; an existing one is already at it.
            let (id, delay) = match self.panes.get(sp.index.saturating_sub(1)) {
                Some(p) => (p.id, 0),
                None => match self.new_pane(
                    None,
                    sp.cwd.clone().map(PathBuf::from),
                    Vec::new(),
                    true,
                ) {
                    Ok(id) => (id, 900),
                    Err(_) => continue,
                },
            };
            let tx = self.tx.clone();
            let Some(pane) = self.pane_mut(id) else { continue };
            if pane.agent().is_some() || pane.fg_app().is_some() {
                continue;
            }
            let cwd = pane.cwd();
            if let Some(cmd) = sp.restore_command(cwd.as_deref()) {
                pane.type_command(tx, cmd, delay);
            }
        }
    }

    fn save_rings(&self) {
        let dir = state_dir(&self.session).join("scrollback");
        let _ = std::fs::create_dir_all(&dir);
        for (i, p) in self.panes.iter().enumerate() {
            let _ = std::fs::write(dir.join(format!("{i}.bin")), &p.ring);
        }
        let mut i = self.panes.len();
        while std::fs::remove_file(dir.join(format!("{i}.bin"))).is_ok() {
            i += 1;
        }
    }
}

/// Host vitals for the home page's top strip, cached a few seconds — the
/// state broadcast fires ~1/s while a home page or observer is watching
/// and none of these numbers move faster than that.
fn host_vitals() -> Option<crate::protocol::HostVitals> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, crate::protocol::HostVitals)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, v)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(5) {
            return Some(v.clone());
        }
    }
    // A transient probe hiccup (vm_stat spawn failure, /proc blip) keeps
    // the last good number instead of broadcasting a plausible-looking 0%.
    let last = cache.as_ref().map(|(_, v)| v.clone());
    let v = crate::protocol::HostVitals {
        uptime_ms: host_uptime_ms()?,
        cpu_pct: host_cpu_pct()
            .or(last.as_ref().map(|v| v.cpu_pct))
            .unwrap_or(0),
        mem_pct: host_mem_pct()
            .or(last.as_ref().map(|v| v.mem_pct))
            .unwrap_or(0),
    };
    *cache = Some((Instant::now(), v.clone()));
    Some(v)
}

#[cfg(target_os = "linux")]
fn host_uptime_ms() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/uptime").ok()?;
    let secs: f64 = s.split_whitespace().next()?.parse().ok()?;
    Some((secs * 1000.0) as u64)
}

#[cfg(not(target_os = "linux"))]
fn host_uptime_ms() -> Option<u64> {
    // sysctl kern.boottime — a timeval of when the machine came up.
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    let mut len = std::mem::size_of::<libc::timeval>();
    let name = std::ffi::CString::new("kern.boottime").ok()?;
    let ok = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut tv as *mut _ as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    if !ok || tv.tv_sec == 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(((now - tv.tv_sec).max(0) as u64) * 1000)
}

/// 1-minute load average over the core count, as a percentage. A
/// steadiness read rather than an instant sample, which suits a strip
/// that refreshes every few seconds.
fn host_cpu_pct() -> Option<u8> {
    let mut loads = [0f64; 3];
    if unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) } < 1 {
        return None;
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    Some(((loads[0] / cores as f64) * 100.0).clamp(0.0, 100.0) as u8)
}

#[cfg(target_os = "linux")]
fn host_mem_pct() -> Option<u8> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |k: &str| -> Option<f64> {
        s.lines()
            .find(|l| l.starts_with(k))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let total = field("MemTotal:")?;
    let avail = field("MemAvailable:")?;
    Some((((total - avail) / total) * 100.0).clamp(0.0, 100.0) as u8)
}

#[cfg(not(target_os = "linux"))]
fn host_mem_pct() -> Option<u8> {
    // macOS: page counts from `vm_stat` against hw.memsize. Spawning a
    // process is fine at the cache's 5s cadence.
    let mut memsize: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = std::ffi::CString::new("hw.memsize").ok()?;
    let ok = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut memsize as *mut _ as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    if !ok || memsize == 0 {
        return None;
    }
    let out = std::process::Command::new("vm_stat").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let count = |k: &str| -> u64 {
        text.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.rsplit_once(' '))
            .and_then(|(_, n)| n.trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let used = (count("Pages active") + count("Pages wired down") + count("Pages occupied by compressor")) * page;
    Some(((used as f64 / memsize as f64) * 100.0).clamp(0.0, 100.0) as u8)
}
