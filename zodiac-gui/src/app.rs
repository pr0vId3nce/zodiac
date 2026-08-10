//! GUI application state + winit event handling (roadmap 3.4/3.6): the
//! frame-dispatch mirror of the TUI's `handle_frame` (every per-pane
//! mutation goes through `zodiac::client_core::CPane`, exactly like the
//! TUI), plus input translation into `encode_key`/`encode_mouse` bytes.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEvent as CKeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use zodiac::client_core::{encode_key, encode_key_kitty, encode_mouse, CPane};
use zodiac::protocol::*;

use crate::anim::AnimStore;
use crate::font::Fonts;
use crate::render::Renderer;

/// How often the GUI polls the server for pane state (status/titles). The
/// server only sends `T_STATE` in reply to `T_QUERY`; this cadence sets how
/// quickly the tab spinner/glow reflect an agent starting or stopping work.
const QUERY_INTERVAL: Duration = Duration::from_millis(600);

/// Events injected into the winit loop from outside: server frames read on
/// the socket thread.
pub enum UserEvent {
    Srv(Frame),
    SrvGone,
}

pub struct GuiApp {
    session: String,
    sock: UnixStream,
    fonts: Option<Fonts>,
    renderer: Option<Renderer>,
    panes: Vec<CPane>,
    active: usize,
    state: Option<SessionState>,
    mouse_gate: bool,
    mods: ModifiersState,
    cursor_px: (f64, f64),
    wheel_accum: f32,
    tab_hits: Vec<(usize, [f32; 4])>,
    /// Persisted settings (config.json); currently just the tab placement,
    /// a placeholder for more to come.
    settings: zodiac::settings::Settings,
    /// Fullscreen settings page open (Ctrl+S), with the highlighted row.
    settings_open: bool,
    settings_row: usize,
    sent_grid: (u16, u16),
    want_quit: bool,
    pub exit_msg: Option<String>,
    /// Testing hook: `ZODIAC_GUI_EXIT_AFTER_MS` closes the window after a
    /// delay so smoke tests can run unattended.
    exit_at: Option<Instant>,
    /// Animation frame store + playheads (roadmap 4.2).
    anim: AnimStore,
    /// Next animation frame-flip deadline from the last redraw; drives the
    /// WaitUntil timer — absent when nothing visible animates.
    next_anim: Option<Instant>,
    /// Next `T_QUERY` state poll. The server only sends `T_STATE` in reply
    /// to a query, so this steady poll is how the GUI learns a pane's
    /// status (working/idle) for the tab spinner + glow.
    next_query: Option<Instant>,
    /// System clipboard handle for OSC 52 write-through (roadmap 4.7),
    /// opened lazily on the first T_CLIPBOARD.
    clipboard: Option<arboard::Clipboard>,
    /// egui context (ADR 0006): the native GUI layer runs every redraw.
    egui_ctx: egui::Context,
    /// winit→egui input bridge; created in `resumed` once the window exists.
    egui_state: Option<egui_winit::State>,
    /// Which screen the GUI is showing (app-shell router, task #24).
    screen: crate::ui::Screen,
    /// Pane ids currently showing terminal mode (vs. the native transcript);
    /// per-pane and remembered, per the handoff (task #26).
    term_mode: std::collections::HashSet<u64>,
    /// Mutable UI state egui edits in place (composer buffer, open overlay,
    /// palette query/selection).
    ui_state: crate::ui::UiState,
}

impl GuiApp {
    pub fn new(session: String, sock: UnixStream, fonts: Fonts) -> Self {
        let exit_at = std::env::var("ZODIAC_GUI_EXIT_AFTER_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        Self {
            session,
            sock,
            fonts: Some(fonts),
            renderer: None,
            panes: Vec::new(),
            active: 0,
            state: None,
            mouse_gate: false,
            mods: ModifiersState::default(),
            cursor_px: (0.0, 0.0),
            wheel_accum: 0.0,
            tab_hits: Vec::new(),
            settings: zodiac::settings::Settings::load(),
            settings_open: false,
            settings_row: 0,
            sent_grid: (0, 0),
            want_quit: false,
            exit_msg: None,
            exit_at,
            anim: AnimStore::default(),
            next_anim: None,
            next_query: Some(Instant::now()), // poll state right away
            clipboard: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            screen: crate::ui::Screen::Observatory,
            term_mode: std::collections::HashSet::new(),
            ui_state: crate::ui::UiState::default(),
        }
    }

    fn send(&mut self, typ: u8, id: u64, data: &[u8]) {
        if write_frame(&mut self.sock, typ, id, data).is_err() {
            self.exit_msg = Some("zodiac-gui: lost connection to server".into());
            self.want_quit = true;
        }
    }

    fn send_input(&mut self, id: u64, bytes: &[u8]) {
        if let Some(p) = self.panes.iter_mut().find(|p| p.id == id) {
            if p.scroll != 0 {
                p.set_scroll(0);
            }
        }
        self.send(T_INPUT, id, bytes);
    }

    fn grid(&self) -> (u16, u16) {
        self.renderer
            .as_ref()
            .map(|r| r.grid_size())
            .unwrap_or((24, 80))
    }

    fn request_redraw(&self) {
        if let Some(r) = &self.renderer {
            r.window.request_redraw();
        }
    }

    fn focus(&mut self, idx: usize) {
        if idx >= self.panes.len() {
            return;
        }
        self.active = idx;
        let id = {
            let p = &mut self.panes[idx];
            p.clear_flags();
            p.id
        };
        self.send(T_FOCUS, id, &[]);
    }

    /// Announce the current grid size (and cell px) to the server. Called
    /// whenever the window or font metrics change.
    fn sync_size(&mut self) {
        let Some(r) = &self.renderer else { return };
        let (rows, cols) = r.grid_size();
        let cell = r.cell;
        if (rows, cols) == self.sent_grid {
            return;
        }
        self.sent_grid = (rows, cols);
        for p in &mut self.panes {
            p.resize(rows, cols);
        }
        let (cw, ch) = (cell.0.round() as u16, cell.1.round() as u16);
        let mut data = [0u8; 8];
        data[..2].copy_from_slice(&rows.to_le_bytes());
        data[2..4].copy_from_slice(&cols.to_le_bytes());
        data[4..6].copy_from_slice(&cw.to_le_bytes());
        data[6..8].copy_from_slice(&ch.to_le_bytes());
        self.send(T_RESIZE, 0, &data);
    }

    /// Mirror of the TUI's `handle_frame`, minus TUI-only concerns
    /// (notifications, kitty re-emission, home page). Unknown frames are
    /// ignored — the protocol only grows additively.
    fn handle_frame(&mut self, f: Frame) {
        match f.typ {
            T_HELLO => {
                if let Ok(h) = serde_json::from_slice::<Hello>(&f.data) {
                    self.mouse_gate = h.mouse_gate;
                    let (rows, cols) = self.grid();
                    self.panes = h
                        .panes
                        .into_iter()
                        .map(|hp| {
                            let mut p = CPane::new(hp.id, hp.name, rows, cols);
                            p.activity = hp.activity;
                            p.attention = hp.attention;
                            p
                        })
                        .collect();
                    self.active = self
                        .panes
                        .iter()
                        .position(|p| p.id == h.active)
                        .unwrap_or(0);
                }
            }
            T_REPLAY => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                    p.parser.process(&f.data);
                    let _ = p.poll_bell();
                }
            }
            T_OUTPUT => {
                let active_id = self.panes.get(self.active).map(|p| p.id);
                if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                    p.parser.process(&f.data);
                    p.last_output = Some(Instant::now());
                    let bell = p.poll_bell();
                    if Some(f.id) != active_id {
                        p.activity = true;
                        if bell {
                            p.attention = true;
                        }
                    }
                }
            }
            T_STATE => {
                if let Ok(s) = serde_json::from_slice::<SessionState>(&f.data) {
                    for sp in &s.panes {
                        if let Some(cp) = self.panes.iter_mut().find(|cp| cp.id == sp.id) {
                            cp.kind = sp.kind.clone();
                        }
                    }
                    self.state = Some(s);
                }
            }
            T_PANE_OPENED => {
                let name = String::from_utf8_lossy(&f.data).into_owned();
                let (rows, cols) = self.grid();
                self.panes.push(CPane::new(f.id, name, rows, cols));
                self.active = self.panes.len() - 1;
            }
            T_PANE_RENAMED => {
                let name = String::from_utf8_lossy(&f.data).into_owned();
                if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                    p.name = name;
                }
            }
            T_PANE_CLOSED => {
                self.anim.drop_pane(f.id);
                if let Some(i) = self.panes.iter().position(|p| p.id == f.id) {
                    self.panes.remove(i);
                    if self.panes.is_empty() {
                        self.exit_msg = Some("zodiac-gui: session ended (last pane closed)".into());
                        self.want_quit = true;
                    } else if self.active >= self.panes.len() {
                        self.focus(self.panes.len() - 1);
                    } else if i < self.active {
                        self.active -= 1;
                    }
                }
            }
            T_GFX_STATE => {
                if let Ok(snap) = serde_json::from_slice::<zodiac::gfx::GfxSnapshot>(&f.data) {
                    let mut dead = Vec::new(); // outer-terminal ids: TUI-only
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                        p.apply_gfx_state(snap, &mut dead);
                        // Frame store follows the server's image lifetime.
                        self.anim.retain_pane(f.id, &p.gfx.images);
                    }
                }
            }
            T_GFX_IMG => {
                if let Some(hdr) = GfxImgHdr::decode(&f.data) {
                    let chunk = &f.data[GFX_IMG_HDR..];
                    let mut dead = Vec::new();
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                        p.apply_gfx_chunk(&hdr, chunk, &mut dead);
                    }
                }
            }
            T_GFX_FRAME => {
                // Animation frame chunk (4.2): reassembled client-side,
                // keyed per (pane, img, ver, idx).
                if let Some(hdr) = GfxFrameHdr::decode(&f.data) {
                    let chunk = &f.data[GFX_FRAME_HDR..];
                    if self.panes.iter().any(|p| p.id == f.id) {
                        self.anim.apply_chunk(f.id, &hdr, chunk);
                    }
                }
            }
            T_CLIPBOARD => {
                // OSC 52 write-through (4.7): the server only forwards
                // writes the user enabled (`clipboard_write` setting).
                if let Ok(cwr) = serde_json::from_slice::<ClipboardWrite>(&f.data) {
                    self.write_clipboard(&cwr.selection, cwr.text);
                }
            }
            T_AGENT_EVENT => {
                let text = String::from_utf8_lossy(&f.data).into_owned();
                let active_id = self.panes.get(self.active).map(|p| p.id);
                let replay = f.data.contains(&b'\n');
                if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                    p.kind = "agent".into(); // may beat the first T_STATE
                    for line in text.split('\n').filter(|l| !l.trim().is_empty()) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                            p.agent.apply_line(&v);
                        }
                    }
                    if Some(f.id) != active_id && !replay {
                        p.activity = true;
                    }
                }
            }
            T_PERM_REQ => {
                if let Ok(pr) = serde_json::from_slice::<PermRequest>(&f.data) {
                    let active_id = self.panes.get(self.active).map(|p| p.id);
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                        p.kind = "agent".into();
                        if !p.agent.perms.iter().any(|x| x.request_id == pr.request_id) {
                            p.agent.perms.push(pr);
                        }
                        if Some(f.id) != active_id {
                            p.attention = true;
                        }
                    }
                }
            }
            T_SERVER_EXIT => {
                self.exit_msg = Some("zodiac-gui: server shut down".into());
                self.want_quit = true;
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- input

    /// The settings rows and their presets. Each row maps to a persisted
    /// field via `setting_sel`/`set_setting` (matched by index).
    const SETTING_SPECS: &'static [(&'static str, &'static [&'static str])] = &[
        ("Pane tabs", &["top", "side"]),
        ("Background", &["oled", "charcoal", "midnight", "slate"]),
        ("Tab bar bg", &["oled", "charcoal", "midnight", "slate"]),
        (
            "Scale",
            &["75%", "90%", "100%", "110%", "125%", "150%", "175%", "200%"],
        ),
        ("Tab markers", &["dots", "arabic", "roman", "zodiac"]),
        ("Spinner position", &["replace", "both", "end"]),
        ("Spinner color", crate::palette::COLOR_NAMES),
        ("Glow color", crate::palette::COLOR_NAMES),
        ("Glow speed", &["off", "slow", "normal", "fast", "zippy"]),
    ];

    /// Index of `cur` in row `row`'s preset list, falling back to `default`.
    fn spec_idx(row: usize, cur: &str, default: usize) -> usize {
        Self::SETTING_SPECS[row]
            .1
            .iter()
            .position(|v| *v == cur)
            .unwrap_or(default)
    }

    /// Current preset index for setting `row`.
    fn setting_sel(&self, row: usize) -> usize {
        let s = &self.settings;
        match row {
            0 => usize::from(s.gui_tabs() == "side"),
            1 => Self::spec_idx(1, s.gui_bg(), 0),     // oled
            2 => Self::spec_idx(2, s.gui_tab_bg(), 3), // slate
            3 => Self::spec_idx(3, s.gui_scale.as_str(), 2), // 100%
            4 => Self::spec_idx(4, s.gui_tab_marker(), 0), // dots
            5 => Self::spec_idx(5, s.gui_spinner_pos(), 0), // replace
            6 => Self::spec_idx(6, s.spinner_color.as_str(), 0), // orange
            7 => Self::spec_idx(7, s.shimmer_color.as_str(), 8), // white
            8 => Self::spec_idx(8, s.shimmer_speed.as_str(), 2), // normal
            _ => 0,
        }
    }

    /// Store preset `sel` for setting `row` (does not persist/apply).
    fn set_setting(&mut self, row: usize, sel: usize) {
        let val = Self::SETTING_SPECS[row].1[sel].to_string();
        let s = &mut self.settings;
        match row {
            0 => s.gui_tabs = val,
            1 => s.gui_bg = val,
            2 => s.gui_tab_bg = val,
            3 => s.gui_scale = val,
            4 => s.gui_tab_marker = val,
            5 => s.gui_spinner_pos = val,
            6 => s.spinner_color = val,
            7 => s.shimmer_color = val,
            8 => s.shimmer_speed = val,
            _ => {}
        }
    }

    /// Snapshot of the rows for the renderer.
    fn settings_view_rows(&self) -> Vec<crate::render::SettingRow> {
        Self::SETTING_SPECS
            .iter()
            .enumerate()
            .map(|(i, (label, values))| crate::render::SettingRow {
                label,
                values: values.to_vec(),
                sel: self.setting_sel(i),
            })
            .collect()
    }

    /// Cycle the selected row's preset, persist, and apply live.
    fn cycle_setting(&mut self, dir: isize) {
        let row = self.settings_row;
        let n = Self::SETTING_SPECS[row].1.len() as isize;
        if n == 0 {
            return;
        }
        let next = (self.setting_sel(row) as isize + dir).rem_euclid(n) as usize;
        self.set_setting(row, next);
        self.settings.save();
        self.apply_settings();
    }

    /// Push the current settings into the renderer and resend the grid size
    /// (tab placement and scale both change the usable grid area).
    fn apply_settings(&mut self) {
        let s = &self.settings;
        let side = s.gui_tabs() == "side";
        let bg = crate::palette::bg_color(s.gui_bg());
        let tab_bg = crate::palette::bg_color(s.gui_tab_bg());
        let scale = s.gui_scale();
        let style = crate::render::TabStyle {
            marker: s.gui_tab_marker().to_string(),
            spinner_pos: s.gui_spinner_pos().to_string(),
            spinner_color: crate::palette::named_color(&s.spinner_color, [255, 135, 0]),
            glow_color: crate::palette::named_color(&s.shimmer_color, [255, 255, 255]),
            glow_period: crate::palette::glow_period_ms(&s.shimmer_speed),
        };
        if let Some(r) = self.renderer.as_mut() {
            r.set_tab_side(side);
            r.set_bg(bg);
            r.set_tab_bg(tab_bg);
            r.set_user_scale(scale);
            r.set_tab_style(style);
        }
        // Grid dimensions may have changed under the new chrome/scale.
        self.sent_grid = (0, 0);
        self.sync_size();
        self.request_redraw();
    }

    fn settings_key(&mut self, key: &Key) {
        let rows = Self::SETTING_SPECS.len();
        match key {
            Key::Named(NamedKey::Escape) => {
                self.settings_open = false;
            }
            Key::Named(NamedKey::ArrowUp) if rows > 0 => {
                self.settings_row = (self.settings_row + rows - 1) % rows;
            }
            Key::Named(NamedKey::ArrowDown) if rows > 0 => {
                self.settings_row = (self.settings_row + 1) % rows;
            }
            Key::Named(NamedKey::ArrowLeft) => self.cycle_setting(-1),
            Key::Named(NamedKey::ArrowRight | NamedKey::Enter | NamedKey::Space) => {
                self.cycle_setting(1)
            }
            _ => {}
        }
        self.request_redraw();
    }

    fn on_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
        }
        // Ctrl+S toggles the fullscreen settings page.
        if self.mods.control_key() {
            if let Key::Character(s) = &ev.logical_key {
                if s.eq_ignore_ascii_case("s") {
                    self.settings_open = !self.settings_open;
                    self.settings_row = 0;
                    self.request_redraw();
                    return;
                }
            }
        }
        // While the settings page is open it owns the keyboard.
        if self.settings_open {
            self.settings_key(&ev.logical_key);
            return;
        }
        // Alt tab navigation + new-pane shortcuts.
        if self.mods.alt_key() {
            let n = self.panes.len();
            let side = self.settings.gui_tabs() == "side";
            let prev = |me: &mut Self| {
                if n > 0 {
                    me.focus((me.active + n - 1) % n);
                    me.request_redraw();
                }
            };
            let next = |me: &mut Self| {
                if n > 0 {
                    me.focus((me.active + 1) % n);
                    me.request_redraw();
                }
            };
            match &ev.logical_key {
                // Arrow nav: horizontal for top tabs, vertical for side.
                Key::Named(NamedKey::ArrowLeft) if !side => return prev(self),
                Key::Named(NamedKey::ArrowRight) if !side => return next(self),
                Key::Named(NamedKey::ArrowUp) if side => return prev(self),
                Key::Named(NamedKey::ArrowDown) if side => return next(self),
                // Alt+N new shell pane; Alt+Shift+N new claude agent pane.
                Key::Character(s) if s.eq_ignore_ascii_case("n") => {
                    if self.mods.shift_key() {
                        self.send(T_NEW_PANE, 0, br#"{"kind":"agent","agent":"claude"}"#);
                    } else {
                        self.send(T_NEW_PANE, 0, &[]);
                    }
                    return;
                }
                // Alt+W closes the active pane (kills its process).
                Key::Character(s) if s.eq_ignore_ascii_case("w") => {
                    if let Some(id) = self.panes.get(self.active).map(|p| p.id) {
                        self.send(T_CLOSE_PANE, id, &[]);
                    }
                    return;
                }
                _ => {}
            }
        }
        // Alt+1..9 jumps straight to a pane (Alt+1 = first).
        if self.mods.alt_key() {
            if let Key::Character(s) = &ev.logical_key {
                if let Some(d) = s.chars().next().and_then(|c| c.to_digit(10)) {
                    if d >= 1 && (d as usize) <= self.panes.len() {
                        self.focus(d as usize - 1);
                        self.request_redraw();
                    }
                    return;
                }
            }
        }
        // Pane-switch shortcuts (documented in README): Ctrl+PageUp/Down.
        if self.mods.control_key() {
            match ev.logical_key {
                Key::Named(NamedKey::PageUp) => {
                    let n = self.panes.len();
                    if n > 0 {
                        self.focus((self.active + n - 1) % n);
                    }
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::PageDown) => {
                    let n = self.panes.len();
                    if n > 0 {
                        self.focus((self.active + 1) % n);
                    }
                    self.request_redraw();
                    return;
                }
                _ => {}
            }
        }
        let Some(ck) = crate::keys::to_key_event(&ev.logical_key, self.mods) else {
            return;
        };
        let Some(p) = self.panes.get(self.active) else {
            return;
        };
        if p.is_agent() {
            self.agent_key(ck);
        } else {
            let id = p.id;
            let screen = p.parser.screen();
            let app_cursor = screen.application_cursor();
            // Kitty keyboard (4.4): when the pane's flag stack is on, the
            // GUI always synthesizes CSI-u — it is not gated on a host
            // probe like the TUI. `encode_key_kitty` returns None for
            // events whose legacy bytes are already unambiguous (and when
            // flags are 0), so plain text stays plain text.
            let flags = if p.kitty_kill {
                0
            } else {
                screen.kitty_keyboard_flags()
            };
            if let Some(bytes) =
                encode_key_kitty(&ck, flags).or_else(|| encode_key(&ck, app_cursor))
            {
                self.send_input(id, &bytes);
            }
        }
        self.request_redraw();
    }

    /// Drag-and-drop (4.6): dropped files land in the active pane as their
    /// (shell-quoted) path text — content attachment is iceboxed. Agent
    /// pane: appended to the local prompt editor so the user reviews before
    /// Enter submits it as T_AGENT_INPUT. Pty pane: sent as T_INPUT,
    /// bracketed-paste-wrapped when the app enabled paste mode, with a
    /// trailing space so consecutive drops stay word-separated.
    fn on_drop(&mut self, path: std::path::PathBuf) {
        let quoted = shell_quote(&path.to_string_lossy());
        let Some(p) = self.panes.get_mut(self.active) else {
            return;
        };
        if p.is_agent() {
            if !p.agent.input.is_empty() && !p.agent.input.ends_with(' ') {
                p.agent.input.push(' ');
            }
            p.agent.input.push_str(&quoted);
            p.agent.cursor = p.agent.input.chars().count();
        } else {
            let id = p.id;
            let text = format!("{quoted} ");
            let bytes = if p.parser.screen().bracketed_paste() {
                format!("\x1b[200~{text}\x1b[201~").into_bytes()
            } else {
                text.into_bytes()
            };
            self.send_input(id, &bytes);
        }
        self.request_redraw();
    }

    /// OSC 52 write-through (4.7): selection strings containing 'p' target
    /// the primary selection (arboard supports it on Linux); anything else
    /// — including combined "pc" — also lands on the regular clipboard.
    fn write_clipboard(&mut self, selection: &str, text: String) {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let Some(cb) = self.clipboard.as_mut() else {
            return;
        };
        let primary = selection.contains('p');
        let regular = !primary || selection.chars().any(|c| c != 'p');
        if primary {
            let _ = cb
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text.clone());
        }
        if regular {
            let _ = cb.set().clipboard(LinuxClipboardKind::Clipboard).text(text);
        }
    }

    /// Keys for an active agent pane — mirrors the TUI's `handle_agent_key`:
    /// the permission modal takes y/n, PageUp/PageDown/Home/End scroll the
    /// transcript, everything else edits the local one-line prompt (Enter
    /// sends it as `T_AGENT_INPUT`).
    fn agent_key(&mut self, key: CKeyEvent) {
        if key.modifiers.contains(KeyModifiers::ALT) {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.grid().0 / 2).max(1) as usize;
        let Some(p) = self.panes.get_mut(self.active) else {
            return;
        };
        let id = p.id;
        if let Some(rid) = p.agent.perms.first().map(|pr| pr.request_id.clone()) {
            let behavior = match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') if !ctrl => Some("allow"),
                KeyCode::Char('n') | KeyCode::Char('N') if !ctrl => Some("deny"),
                _ => None,
            };
            if let Some(behavior) = behavior {
                p.agent.perms.retain(|pr| pr.request_id != rid);
                let resp = serde_json::to_vec(&PermResponse {
                    request_id: rid,
                    behavior: behavior.into(),
                    message: None,
                })
                .unwrap_or_default();
                self.send(T_PERM_RESP, id, &resp);
                return;
            }
        }
        let mut submit: Option<String> = None;
        match key.code {
            KeyCode::PageUp => p.agent.scroll = p.agent.scroll.saturating_add(half),
            KeyCode::PageDown => p.agent.scroll = p.agent.scroll.saturating_sub(half),
            KeyCode::Home => p.agent.scroll = usize::MAX, // clamped at draw
            KeyCode::End => p.agent.scroll = 0,
            KeyCode::Enter => {
                let text = p.agent.input.trim().to_string();
                p.agent.input.clear();
                p.agent.cursor = 0;
                p.agent.scroll = 0;
                if !text.is_empty() {
                    submit = Some(text);
                }
            }
            KeyCode::Esc => {
                p.agent.input.clear();
                p.agent.cursor = 0;
            }
            KeyCode::Backspace => {
                if p.agent.cursor > 0 {
                    let at = char_byte(&p.agent.input, p.agent.cursor - 1);
                    p.agent.input.remove(at);
                    p.agent.cursor -= 1;
                }
            }
            KeyCode::Left => p.agent.cursor = p.agent.cursor.saturating_sub(1),
            KeyCode::Right => {
                p.agent.cursor = (p.agent.cursor + 1).min(p.agent.input.chars().count());
            }
            KeyCode::Char(c) if !ctrl && p.agent.input.chars().count() < 4000 => {
                let at = char_byte(&p.agent.input, p.agent.cursor);
                p.agent.input.insert(at, c);
                p.agent.cursor += 1;
            }
            _ => {}
        }
        if let Some(text) = submit {
            self.send(T_AGENT_INPUT, id, text.as_bytes());
        }
    }

    fn on_ime(&mut self, text: String) {
        let Some(p) = self.panes.get_mut(self.active) else {
            return;
        };
        if p.is_agent() {
            for c in text.chars() {
                let at = char_byte(&p.agent.input, p.agent.cursor);
                p.agent.input.insert(at, c);
                p.agent.cursor += 1;
            }
        } else {
            let id = p.id;
            self.send_input(id, text.as_bytes());
        }
        self.request_redraw();
    }

    /// Pointer position -> pane-relative cell (0-based), clamped to the
    /// grid (which starts one line below the tab bar).
    fn grid_cell(&self) -> (u16, u16) {
        let Some(r) = &self.renderer else {
            return (0, 0);
        };
        let (cw, ch) = r.cell;
        let (rows, cols) = r.grid_size();
        let (ox, oy) = r.grid_origin();
        let col =
            (((self.cursor_px.0 - ox as f64) / cw as f64).floor() as i64).clamp(0, cols as i64 - 1);
        let row =
            (((self.cursor_px.1 - oy as f64) / ch as f64).floor() as i64).clamp(0, rows as i64 - 1);
        (col as u16, row as u16)
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let steps = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => {
                let ch = self.renderer.as_ref().map(|r| r.cell.1).unwrap_or(20.0);
                p.y as f32 / ch
            }
        };
        self.wheel_accum += steps;
        let n = self.wheel_accum.trunc();
        self.wheel_accum -= n;
        let up = n > 0.0;
        for _ in 0..(n.abs() as i32) {
            self.scroll_step(up);
        }
        if n != 0.0 {
            self.request_redraw();
        }
    }

    fn scroll_step(&mut self, up: bool) {
        let (col, row) = self.grid_cell();
        let mods = crate::keys::modifiers(self.mods);
        let Some(p) = self.panes.get_mut(self.active) else {
            return;
        };
        let id = p.id;
        if p.is_agent() {
            p.agent.scroll = if up {
                p.agent.scroll.saturating_add(3)
            } else {
                p.agent.scroll.saturating_sub(3)
            };
            return;
        }
        let m = MouseEvent {
            kind: if up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
            column: col,
            row,
            modifiers: mods,
        };
        let screen = p.parser.screen();
        let encoded = encode_mouse(
            &m,
            col,
            row,
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding(),
        );
        let alt = screen.alternate_screen();
        let app_cursor = screen.application_cursor();
        if let Some(bytes) = encoded {
            let typ = if self.mouse_gate { T_MOUSE } else { T_INPUT };
            self.send(typ, id, &bytes);
        } else if alt {
            // Alternate scroll: wheel becomes arrow keys for fullscreen
            // apps that never asked for mouse reporting.
            let one = match (up, app_cursor) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.send_input(id, one.repeat(3).as_bytes());
        } else {
            p.scroll_by(if up { 3 } else { -3 });
        }
    }

    fn on_click(&mut self, state: ElementState, button: winit::event::MouseButton) {
        // Clicks do nothing on the settings page (keyboard-driven).
        if self.settings_open {
            return;
        }
        // Tab click switches pane — hit rects work for top and side tabs.
        if state == ElementState::Pressed && button == winit::event::MouseButton::Left {
            let (x, y) = (self.cursor_px.0 as f32, self.cursor_px.1 as f32);
            if let Some(idx) = self
                .tab_hits
                .iter()
                .find(|(_, r)| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
                .map(|(i, _)| *i)
            {
                self.focus(idx);
                self.request_redraw();
                return;
            }
        }
        let btn = match button {
            winit::event::MouseButton::Left => MouseButton::Left,
            winit::event::MouseButton::Right => MouseButton::Right,
            winit::event::MouseButton::Middle => MouseButton::Middle,
            _ => return,
        };
        let (col, row) = self.grid_cell();
        let mods = crate::keys::modifiers(self.mods);
        let Some(p) = self.panes.get(self.active) else {
            return;
        };
        if p.is_agent() {
            return;
        }
        let id = p.id;
        let m = MouseEvent {
            kind: match state {
                ElementState::Pressed => MouseEventKind::Down(btn),
                ElementState::Released => MouseEventKind::Up(btn),
            },
            column: col,
            row,
            modifiers: mods,
        };
        let screen = p.parser.screen();
        if let Some(bytes) = encode_mouse(
            &m,
            col,
            row,
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding(),
        ) {
            let typ = if self.mouse_gate { T_MOUSE } else { T_INPUT };
            self.send(typ, id, &bytes);
        }
    }

    fn redraw(&mut self) {
        // The native GUI layer (ADR 0006): run egui for one frame, then hand
        // its tessellated jobs to the renderer's egui present path.
        let win = match self.renderer.as_ref() {
            Some(r) => r.window.clone(),
            None => return,
        };
        if self.egui_state.is_none() {
            return;
        }
        let raw = self.egui_state.as_mut().unwrap().take_egui_input(&win);
        let mut actions: Vec<crate::ui::UiAction> = Vec::new();
        let full = self.egui_ctx.run_ui(raw, |ui| {
            let active_id = self.panes.get(self.active).map(|p| p.id);
            let data = crate::ui::UiData {
                session: &self.session,
                panes: &self.panes,
                state: self.state.as_ref(),
                active: self.active,
                screen: self.screen,
                term_active: active_id.is_some_and(|id| self.term_mode.contains(&id)),
                pairing_token: self
                    .state
                    .as_ref()
                    .map(|s| s.pairing_token.as_str())
                    .unwrap_or(""),
            };
            crate::ui::build(
                ui,
                &data,
                &mut self.ui_state,
                &mut self.settings,
                &mut actions,
            );
        });
        self.egui_state
            .as_mut()
            .unwrap()
            .handle_platform_output(&win, full.platform_output);
        let ppp = full.pixels_per_point;
        let jobs = self.egui_ctx.tessellate(full.shapes, ppp);

        // Drive the WaitUntil timer from egui's repaint request: zero delay
        // means "animate now", a bounded delay schedules the next frame, and
        // anything longer (egui's idle sentinel) disarms the timer.
        let delay = full
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay);
        self.next_anim = match delay {
            Some(d) if d.is_zero() => Some(Instant::now()),
            Some(d) if d < Duration::from_secs(24 * 3600) => Instant::now().checked_add(d),
            _ => None,
        };

        // Apply UI actions now that egui's borrows are released.
        for a in actions {
            match a {
                crate::ui::UiAction::Focus(i) => {
                    if i != self.active {
                        self.ui_state.composer.clear();
                    }
                    self.focus(i);
                }
                crate::ui::UiAction::Open(i) => {
                    if i != self.active {
                        self.ui_state.composer.clear();
                    }
                    self.focus(i);
                    self.screen = crate::ui::Screen::Focused;
                }
                crate::ui::UiAction::Back => self.screen = crate::ui::Screen::Observatory,
                crate::ui::UiAction::SaveSettings => self.settings.save(),
                crate::ui::UiAction::ToggleTerm(id) => {
                    if !self.term_mode.remove(&id) {
                        self.term_mode.insert(id);
                    }
                }
                crate::ui::UiAction::SendAgent(id) => {
                    let text = self.ui_state.composer.trim().to_string();
                    self.ui_state.composer.clear();
                    if !text.is_empty() {
                        self.send(T_AGENT_INPUT, id, text.as_bytes());
                        if let Some(p) = self.panes.iter_mut().find(|p| p.id == id) {
                            p.agent.scroll = 0;
                        }
                    }
                }
                crate::ui::UiAction::Perm(id, allow) => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == id) {
                        if let Some(req) = p.agent.perms.first() {
                            let rid = req.request_id.clone();
                            p.agent.perms.retain(|r| r.request_id != rid);
                            let resp = serde_json::to_vec(&PermResponse {
                                request_id: rid,
                                behavior: if allow { "allow" } else { "deny" }.into(),
                                message: None,
                            })
                            .unwrap_or_default();
                            self.send(T_PERM_RESP, id, &resp);
                        }
                    }
                }
                crate::ui::UiAction::NewShell => self.send(T_NEW_PANE, 0, &[]),
                crate::ui::UiAction::NewAgent => {
                    self.send(T_NEW_PANE, 0, br#"{"kind":"agent","agent":"claude"}"#)
                }
            }
        }

        if let Some(r) = self.renderer.as_mut() {
            r.paint_egui(&jobs, full.textures_delta, ppp);
        }
    }
}

fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Minimal POSIX single-quoting for dropped paths: pass clean paths
/// through untouched, wrap anything else in single quotes (embedded quotes
/// become `'\''`) so the shell — or an agent running one — sees one word.
fn shell_quote(s: &str) -> String {
    let clean = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~' | '+'));
    if clean {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

impl ApplicationHandler<UserEvent> for GuiApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attrs = winit::window::Window::default_attributes()
            .with_title(format!("zodiac — {}", self.session))
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.exit_msg = Some(format!("zodiac-gui: window creation failed: {e}"));
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);
        let fonts = self.fonts.take().expect("fonts consumed once");
        match Renderer::new(window, fonts) {
            Ok(r) => {
                self.renderer = Some(r);
                // Bring up the egui layer (ADR 0006): theme + winit input
                // bridge, bound to the just-created window.
                let win = self.renderer.as_ref().unwrap().window.clone();
                crate::theme::apply(&self.egui_ctx);
                self.egui_state = Some(egui_winit::State::new(
                    self.egui_ctx.clone(),
                    egui::ViewportId::ROOT,
                    win.as_ref(),
                    Some(win.scale_factor() as f32),
                    None,
                    None,
                ));
                // Honor all saved settings (tabs, bg, scale, tab style)
                // before the first sync/draw; also sends the grid size.
                self.apply_settings();
                self.request_redraw();
            }
            Err(e) => {
                self.exit_msg = Some(format!("zodiac-gui: GPU init failed: {e}"));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Srv(f) => {
                self.handle_frame(f);
                self.request_redraw();
            }
            UserEvent::SrvGone => {
                if self.exit_msg.is_none() {
                    self.exit_msg = Some("zodiac-gui: lost connection to server".into());
                }
                self.want_quit = true;
            }
        }
        if self.want_quit {
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        // Feed the event to egui first (ADR 0006). When egui consumes it,
        // the keystroke/click belongs to a widget, not to a pane's pty.
        let win = self.renderer.as_ref().map(|r| r.window.clone());
        let mut consumed = false;
        if let (Some(st), Some(win)) = (self.egui_state.as_mut(), win.as_ref()) {
            let resp = st.on_window_event(win, &event);
            if resp.repaint {
                win.request_redraw();
            }
            consumed = resp.consumed;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.send(T_DETACH, 0, &[]);
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                self.sync_size();
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(r) = self.renderer.as_mut() {
                    r.set_scale(scale_factor as f32);
                }
                self.sync_size();
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if !consumed => self.on_key(event),
            WindowEvent::Ime(Ime::Commit(text)) if !consumed => self.on_ime(text),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_px = (position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } if !consumed => self.on_wheel(delta),
            WindowEvent::MouseInput { state, button, .. } if !consumed => {
                self.on_click(state, button)
            }
            WindowEvent::DroppedFile(path) => self.on_drop(path),
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
        if self.want_quit {
            event_loop.exit();
        }
    }

    /// A WaitUntil deadline fired: redraw if it was the animation timer, and
    /// poll `T_QUERY` if the state-poll deadline elapsed.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
            return;
        }
        let now = Instant::now();
        if self.next_anim.is_some_and(|t| now >= t) {
            self.request_redraw();
        }
        if self.next_query.is_some_and(|t| now >= t) {
            // The T_STATE reply arrives as a Srv frame and triggers a redraw.
            self.send(T_QUERY, 0, &[]);
            self.next_query = Some(now + QUERY_INTERVAL);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(t) = self.exit_at {
            if Instant::now() >= t {
                self.send(T_DETACH, 0, &[]);
                event_loop.exit();
                return;
            }
        }
        // Sleep until the earliest armed deadline: test-exit, animation
        // frame flip, or the next state poll. (The poll is always armed, so
        // this is effectively WaitUntil at the poll cadence.)
        let next = [self.exit_at, self.next_anim, self.next_query]
            .into_iter()
            .flatten()
            .min();
        event_loop.set_control_flow(match next {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn dropped_paths_are_shell_quoted() {
        assert_eq!(shell_quote("/tmp/a-b_c.png"), "/tmp/a-b_c.png");
        assert_eq!(shell_quote("/tmp/with space"), "'/tmp/with space'");
        assert_eq!(shell_quote("/tmp/o'brien"), r"'/tmp/o'\''brien'");
        assert_eq!(shell_quote(""), "''");
    }
}
