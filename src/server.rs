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

pub enum SrvEvent {
    Conn(UnixStream),
    Client(u64, Frame),
    ClientGone(u64),
    Output(u64, Vec<u8>),
    /// Input for a pane from a server-side helper thread (autoresume pacing).
    Deliver(u64, Vec<u8>),
    /// Result of an async `<agent> --version` probe (agent name, first line).
    AgentVersion(String, String),
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
}

fn default_true() -> bool {
    true
}

const RESTORE_BANNER: &[u8] =
    b"\r\n\x1b[0m\x1b[?1049l\x1b[?25h\x1b[7m zodiac: restored session \xe2\x80\x94 processes were not preserved \x1b[0m\r\n";

struct Server {
    session: String,
    panes: Vec<SrvPane>,
    active: u64,
    next_id: u64,
    size: (u16, u16),
    conns: HashMap<u64, UnixStream>,
    ui: Option<u64>,
    next_gen: u64,
    tx: Sender<SrvEvent>,
    dirty: bool,
    quit: bool,
    resized_at: Option<Instant>,
    /// Agent name → first line of `--version`. None = probe in flight.
    versions: HashMap<String, Option<String>>,
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
        next_gen: 0,
        tx,
        dirty: false,
        quit: false,
        resized_at: None,
        versions: HashMap::new(),
    };
    srv.restore()?;
    srv.save_meta();

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
        }
        if srv.dirty {
            srv.save_meta();
        }
        if last_ring_save.elapsed() > Duration::from_secs(60) {
            srv.save_rings();
            last_ring_save = Instant::now();
        }
    }

    srv.save_meta();
    srv.save_rings();
    srv.send_ui(T_SERVER_EXIT, 0, &[]);
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
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        if self.ui == Some(gen) {
            self.ui = None;
        }
    }

    fn reply(&mut self, gen: u64, typ: u8, id: u64, data: &[u8]) {
        let dead = match self.conns.get_mut(&gen) {
            Some(c) => write_frame(c, typ, id, data).is_err(),
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

    fn handle(&mut self, ev: SrvEvent) {
        match ev {
            SrvEvent::Conn(stream) => {
                self.next_gen += 1;
                let gen = self.next_gen;
                let Ok(mut rd) = stream.try_clone() else {
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
                self.conns.insert(gen, stream);
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
                if let Some(p) = self.pane_mut(id) {
                    let prev = p.last_output;
                    let bell = p.process_output(&bytes);
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
                self.send_ui(T_OUTPUT, id, &bytes);
            }
            SrvEvent::Deliver(id, bytes) => {
                if let Some(p) = self.pane_mut(id) {
                    p.write_input(&bytes);
                }
            }
            SrvEvent::AgentVersion(agent, version) => {
                self.versions.insert(agent, Some(version));
            }
            SrvEvent::Exited(id) => self.remove_pane(id),
        }
    }

    fn client_frame(&mut self, gen: u64, f: Frame) {
        match f.typ {
            T_ATTACH => self.attach(gen),
            T_QUERY => {
                self.probe_versions();
                let data = serde_json::to_vec(&self.state()).unwrap_or_default();
                self.reply(gen, T_STATE, 0, &data);
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
            T_RESIZE => {
                if f.data.len() >= 4 {
                    let rows = u16::from_le_bytes([f.data[0], f.data[1]]);
                    let cols = u16::from_le_bytes([f.data[2], f.data[3]]);
                    if self.size != (rows, cols) {
                        self.resized_at = Some(Instant::now());
                    }
                    self.size = (rows, cols);
                    for p in &mut self.panes {
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
                    if let Some(p) = self.pane_mut(f.id) {
                        p.name = name;
                        self.dirty = true;
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
        let conn_watch = crate::settings::Settings::load().connection_watch;
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
            if let Some(path) = crate::settings::Settings::load().finish_sound_path() {
                play_sound(&path);
            }
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
        let active = self.active;
        if let Some(p) = self.pane_mut(active) {
            p.clear_flags();
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
            panes: self
                .panes
                .iter()
                .enumerate()
                .map(|(i, p)| PaneState {
                    index: i + 1,
                    id: p.id,
                    name: p.name.clone(),
                    title: p.title(),
                    status: p.status().to_string(),
                    agent: p.agent(),
                    cwd: p.cwd(),
                    focused: self.ui.is_some() && self.active == p.id,
                    auto_resume: p.auto_resume,
                    uptime_ms: p.uptime_ms(),
                    version: p
                        .agent()
                        .and_then(|a| self.versions.get(&a).cloned().flatten())
                        .filter(|v| !v.is_empty()),
                    thinking: p.thinking(),
                })
                .collect(),
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
        let pane = SrvPane::spawn(id, name, rows, cols, cwd, preload, self.tx.clone())?;
        let pname = pane.name.clone();
        self.panes.push(pane);
        self.dirty = true;
        if announce {
            self.active = id;
            self.send_ui(T_PANE_OPENED, id, pname.as_bytes());
        }
        Ok(id)
    }

    fn remove_pane(&mut self, id: u64) {
        if let Some(i) = self.idx(id) {
            self.panes.remove(i);
            self.dirty = true;
            self.send_ui(T_PANE_CLOSED, id, &[]);
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
                preload.extend_from_slice(RESTORE_BANNER);
            }
            if let Ok(id) = self.new_pane(
                Some(sp.name.clone()),
                sp.cwd.clone().map(PathBuf::from),
                preload,
                false,
            ) {
                if let Some(p) = self.pane_mut(id) {
                    p.auto_resume = sp.auto_resume;
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
