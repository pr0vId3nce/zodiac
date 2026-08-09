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
    tab_hits: Vec<(usize, std::ops::Range<f32>)>,
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
    /// System clipboard handle for OSC 52 write-through (roadmap 4.7),
    /// opened lazily on the first T_CLIPBOARD.
    clipboard: Option<arboard::Clipboard>,
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
            sent_grid: (0, 0),
            want_quit: false,
            exit_msg: None,
            exit_at,
            anim: AnimStore::default(),
            next_anim: None,
            clipboard: None,
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

    fn on_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
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
        let col = ((self.cursor_px.0 / cw as f64).floor() as i64).clamp(0, cols as i64 - 1);
        let row =
            (((self.cursor_px.1 - ch as f64) / ch as f64).floor() as i64).clamp(0, rows as i64 - 1);
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
        let ch = self.renderer.as_ref().map(|r| r.cell.1).unwrap_or(20.0) as f64;
        // Tab bar click switches pane.
        if state == ElementState::Pressed
            && button == winit::event::MouseButton::Left
            && self.cursor_px.1 < ch
        {
            let x = self.cursor_px.0 as f32;
            if let Some(idx) = self
                .tab_hits
                .iter()
                .find(|(_, r)| r.contains(&x))
                .map(|(i, _)| *i)
            {
                self.focus(idx);
                self.request_redraw();
            }
            return;
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
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        let out = r.render(
            &self.panes,
            self.active,
            self.state.as_ref(),
            &self.session,
            &mut self.anim,
            Instant::now(),
        );
        self.tab_hits = out.tab_hits;
        // Animation timer (4.2): armed only while a visible image runs.
        self.next_anim = out.next_anim;
        if let Some(s) = out.agent_scroll {
            if let Some(p) = self.panes.get_mut(self.active) {
                p.agent.scroll = s;
            }
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
                self.sync_size();
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
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event),
            WindowEvent::Ime(Ime::Commit(text)) => self.on_ime(text),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_px = (position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::MouseInput { state, button, .. } => self.on_click(state, button),
            WindowEvent::DroppedFile(path) => self.on_drop(path),
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
        if self.want_quit {
            event_loop.exit();
        }
    }

    /// A WaitUntil deadline fired: if it was the animation timer, redraw —
    /// the render pass picks the new frame and re-arms the timer.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. })
            && self.next_anim.is_some_and(|t| Instant::now() >= t)
        {
            self.request_redraw();
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
        // Sleep until the earliest deadline (test-exit or animation frame
        // flip); plain Wait when neither is armed — no idle wakeups.
        let next = match (self.exit_at, self.next_anim) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
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
