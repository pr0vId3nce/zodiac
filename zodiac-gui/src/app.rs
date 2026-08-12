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

/// Shortest gap between animation frames while the window is focused (~60fps).
/// egui requests zero-delay repaints while anything animates; this is what
/// keeps that from free-running the event loop on CPU cost alone.
const MIN_FRAME: Duration = Duration::from_millis(16);

/// Animation cadence when the window is visible but unfocused (~15fps): a
/// background pane still streams visibly, at a fraction of the CPU.
const UNFOCUSED_FRAME: Duration = Duration::from_millis(66);

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
    /// `ZODIAC_GUI_SELFTEST`: walk every screen + overlay (and open a pane)
    /// unattended, then exit — a headless click-through for CI/bug sweeps.
    selftest: bool,
    selftest_step: usize,
    selftest_next: Option<Instant>,
    /// Window visibility/focus, from winit — used to throttle (unfocused) or
    /// stop (occluded) the animation timer. Redraws here are timer-driven, not
    /// frame-callback-driven, so a hidden window would otherwise keep burning
    /// CPU. Assume visible+focused until told otherwise.
    occluded: bool,
    focused: bool,
    /// One-shot guard for the `ZODIAC_GUI_SEED_ITEMS` soak fixture.
    seeded: bool,
}

impl GuiApp {
    pub fn new(session: String, sock: UnixStream, fonts: Fonts) -> Self {
        // Warm the pi model list in the background so the new-agent picker
        // (which lists claude-bridge/* models via `pi --list-models`) never
        // blocks the UI on its first open.
        crate::agents::prewarm_pi_models();
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
            selftest: std::env::var("ZODIAC_GUI_SELFTEST").is_ok(),
            selftest_step: 0,
            selftest_next: None,
            occluded: false,
            focused: true,
            seeded: false,
        }
    }

    /// One step of the self-test walk (`ZODIAC_GUI_SELFTEST`): drive the GUI
    /// through every screen and overlay, opening a pane so the focused /
    /// terminal paths render, then exit. Any panic in a render path crashes
    /// the process (nonzero exit) — that's the bug sweep.
    fn selftest_tick(&mut self, event_loop: &ActiveEventLoop) {
        use crate::ui::{Overlay, Screen};
        let step = self.selftest_step;
        self.selftest_step += 1;
        match step {
            0 => self.send(T_NEW_PANE, 0, &[]), // open a shell pane
            1 => self.send(T_QUERY, 0, &[]),    // refresh state
            2 => {
                self.active = self.panes.len().saturating_sub(1);
                self.screen = Screen::Focused; // focused (terminal for a shell)
            }
            3 => self.ui_state.overlay = Overlay::Palette,
            4 => self.ui_state.overlay = Overlay::Settings,
            5 => self.ui_state.overlay = Overlay::Oracle,
            6 => self.ui_state.overlay = Overlay::Pairing,
            7 => self.ui_state.overlay = Overlay::Raise,
            8 => self.open_agent_picker(), // new-agent picker (harness + model)
            9 => {
                self.ui_state.overlay = Overlay::None;
                self.screen = Screen::Observatory;
            }
            10 => {
                // Seed a synthetic agent pane with the full range of rich
                // transcript items + a pending permission, then focus it in
                // transcript mode so every new render path (thinking, tool
                // boxes, code, error, the question popup) is exercised.
                self.seed_agent_pane();
                self.active = self.panes.len().saturating_sub(1);
                self.screen = Screen::Focused;
            }
            _ => {
                let panes = self.panes.len();
                self.exit_msg = Some(format!(
                    "zodiac-gui: selftest ok — {panes} pane(s), all screens + overlays rendered"
                ));
                self.send(T_DETACH, 0, &[]);
                event_loop.exit();
                return;
            }
        }
        self.request_redraw();
        self.selftest_next = Some(Instant::now() + Duration::from_millis(250));
    }

    /// Build an in-memory agent pane covering every `ChatItem` kind and a
    /// pending permission — used only by the self-test to render the rich
    /// transcript + question popup without spawning a real agent.
    fn seed_agent_pane(&mut self) {
        let (rows, cols) = self.grid();
        let mut p = CPane::new(u64::MAX, "selftest agent".into(), rows, cols);
        p.kind = "agent".into();
        let lines = [
            serde_json::json!({"type": "zodiac_user", "text": "run the tests"}),
            serde_json::json!({"type": "assistant", "message": {"model": "claude-sonnet-4-5-20250929", "content": [
                {"type": "thinking", "thinking": "Let me look at the test runner first."},
                {"type": "text", "text": "I'll run the suite now."},
            ]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Bash",
                 "input": {"command": "cargo test --all"}},
            ]}}),
            serde_json::json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "test result: ok. 42 passed"},
            ]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "false"}},
            ]}}),
            serde_json::json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t2",
                 "content": "exit code 1", "is_error": true},
            ]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t3", "name": "TodoWrite", "input": {"todos": [
                    {"content": "Read the failing test", "status": "completed"},
                    {"content": "Patch the parser", "status": "in_progress"},
                    {"content": "Re-run the suite", "status": "pending"},
                ]}},
            ]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "## Summary\n\nHere's the **fix** with a `helper`, ~~and a typo~~. See [docs](https://example.com/a_(b)) and www.example.com.\n\n```rust\nfn main() {}\n```\n\n#### Details\n\n- one item\n  - nested item\n- [x] done task\n- [ ] pending task\n\n| Name | Size | OK |\n|:-----|-----:|:--:|\n| alpha | 12kB | y |\n| beta | 3kB | n |\n\nDone."},
            ]}}),
        ];
        // `ZODIAC_GUI_SEED_ITEMS=<n>` repeats the seed to build a long
        // transcript — the soak/perf fixture for the virtualized transcript
        // (a long pane and a fresh one should cost about the same per frame).
        let reps = std::env::var("ZODIAC_GUI_SEED_ITEMS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        for _ in 0..reps {
            for l in &lines {
                p.agent.apply_line(l);
            }
        }
        p.agent.perms.push(PermRequest {
            request_id: "selftest".into(),
            tool_name: "Bash".into(),
            display_name: Some("Bash".into()),
            input: serde_json::json!({"command": "rm -rf ./build"}),
            age_ms: 0,
        });
        self.panes.push(p);
    }

    fn send(&mut self, typ: u8, id: u64, data: &[u8]) {
        if write_frame(&mut self.sock, typ, id, data).is_err() {
            // Don't clobber a message already set (e.g. the self-test's
            // "ok" summary just before its final T_DETACH).
            if self.exit_msg.is_none() {
                self.exit_msg = Some("zodiac-gui: lost connection to server".into());
            }
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

    /// Open the new-agent picker: detect locally-available harnesses + their
    /// models, then show the modal (starting at the model step when there's
    /// only one harness).
    fn open_agent_picker(&mut self) {
        let harnesses = crate::agents::harnesses();
        let step = if harnesses.len() > 1 { 0 } else { 1 };
        self.ui_state.agent_picker = crate::ui::AgentPicker {
            harnesses,
            step,
            h_sel: 0,
            m_sel: 0,
        };
        self.ui_state.overlay = crate::ui::Overlay::NewAgent;
        self.request_redraw();
    }

    /// Is a pane's terminal the thing on screen and taking keys? When true the
    /// pty owns the keyboard, not egui.
    fn in_terminal(&self) -> bool {
        self.screen == crate::ui::Screen::Focused
            && self.ui_state.overlay == crate::ui::Overlay::None
            && self
                .panes
                .get(self.active)
                .is_some_and(|p| !p.is_agent() || self.term_mode.contains(&p.id))
    }

    fn focus(&mut self, idx: usize) {
        if idx >= self.panes.len() {
            return;
        }
        // A terminal selection belongs to the pane it was made in.
        self.ui_state.term_sel = None;
        self.active = idx;
        let id = {
            let p = &mut self.panes[idx];
            p.clear_flags();
            p.id
        };
        self.send(T_FOCUS, id, &[]);
    }

    /// Send a grid size + cell px to the server (deduped on `sent_grid`) and
    /// resize the local panes to match.
    fn apply_grid(&mut self, rows: u16, cols: u16, cw: u16, ch: u16) {
        if (rows, cols) == self.sent_grid {
            return;
        }
        self.sent_grid = (rows, cols);
        for p in &mut self.panes {
            p.resize(rows, cols);
        }
        let mut data = [0u8; 8];
        data[..2].copy_from_slice(&rows.to_le_bytes());
        data[2..4].copy_from_slice(&cols.to_le_bytes());
        data[4..6].copy_from_slice(&cw.to_le_bytes());
        data[6..8].copy_from_slice(&ch.to_le_bytes());
        self.send(T_RESIZE, 0, &data);
    }

    /// Fallback grid announce from the renderer's window-derived size, used
    /// before a focused pane has measured its egui terminal widget.
    fn sync_size(&mut self) {
        let Some(r) = &self.renderer else { return };
        let (rows, cols) = r.grid_size();
        let (cw, ch) = (r.cell.0.round() as u16, r.cell.1.round() as u16);
        self.apply_grid(rows, cols, cw, ch);
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
                    // Output-rate buckets for the activity sparklines (task
                    // #33): mirror the TUI — roll the bucket, then count bytes.
                    p.rate_tick();
                    p.rate_cur = p.rate_cur.saturating_add(f.data.len() as u32);
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
                self.ui_state.composers.remove(&f.id); // drop the closed pane's draft
                self.ui_state.item_heights.remove(&f.id);
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
        // Tab navigation + new-pane shortcuts, on Alt (Linux) / Cmd (macOS).
        if crate::keys::cmd_held(self.mods) {
            let n = self.panes.len();
            let side = self.settings.gui_tabs() == "side";
            let prev = |me: &mut Self| {
                if n > 0 {
                    me.focus((me.active + n - 1) % n);
                    me.screen = crate::ui::Screen::Focused;
                    me.request_redraw();
                }
            };
            let next = |me: &mut Self| {
                if n > 0 {
                    me.focus((me.active + 1) % n);
                    me.screen = crate::ui::Screen::Focused;
                    me.request_redraw();
                }
            };
            match &ev.logical_key {
                // Arrow nav: horizontal for top tabs, vertical for side.
                Key::Named(NamedKey::ArrowLeft) if !side => return prev(self),
                Key::Named(NamedKey::ArrowRight) if !side => return next(self),
                Key::Named(NamedKey::ArrowUp) if side => return prev(self),
                Key::Named(NamedKey::ArrowDown) if side => return next(self),
                // Alt+N opens the new-agent picker (harness + model, the
                // default); Alt+Shift+N opens a shell pane directly.
                Key::Character(s) if s.eq_ignore_ascii_case("n") => {
                    if self.mods.shift_key() {
                        self.send(T_NEW_PANE, 0, &[]);
                    } else {
                        self.open_agent_picker();
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
                // Alt+Z returns to the Observatory.
                Key::Character(s) if s.eq_ignore_ascii_case("z") => {
                    self.ui_state.overlay = crate::ui::Overlay::None;
                    self.screen = crate::ui::Screen::Observatory;
                    self.request_redraw();
                    return;
                }
                _ => {}
            }
        }
        // Alt/Cmd+1..9 jumps straight to a pane (1 = first).
        if crate::keys::cmd_held(self.mods) {
            if let Key::Character(s) = &ev.logical_key {
                if let Some(d) = s.chars().next().and_then(|c| c.to_digit(10)) {
                    if d >= 1 && (d as usize) <= self.panes.len() {
                        self.focus(d as usize - 1);
                        // Jumping to a pane should show it, not just mark it
                        // active behind the Observatory.
                        self.screen = crate::ui::Screen::Focused;
                        self.request_redraw();
                    }
                    return;
                }
            }
        }
        // macOS: ⌘⇧[ / ⌘⇧] is the system-wide tab-switch chord (Safari,
        // Terminal, Xcode). Mac laptop keyboards have no PageUp/PageDown at
        // all, so Ctrl+PageUp/Down below is unreachable without fn-chording.
        #[cfg(target_os = "macos")]
        if self.mods.super_key() {
            if let Key::Character(s) = &ev.logical_key {
                let n = self.panes.len();
                match s.as_str() {
                    "[" | "{" if n > 0 => {
                        self.focus((self.active + n - 1) % n);
                        self.request_redraw();
                        return;
                    }
                    "]" | "}" if n > 0 => {
                        self.focus((self.active + 1) % n);
                        self.request_redraw();
                        return;
                    }
                    _ => {}
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
        // Raw keys reach a pane's pty ONLY in the focused screen's terminal
        // view. Everywhere else — Observatory, agent transcript (the egui
        // composer owns input), and while any overlay is open — egui owns the
        // keyboard, so unfocused keystrokes must not leak into a pty.
        let in_terminal = self.in_terminal();
        if !in_terminal {
            return;
        }
        // Terminal clipboard: Ctrl/Cmd+Shift+C copies the current selection;
        // Ctrl/Cmd+Shift+V, Cmd+V, or Shift+Insert pastes into the pty
        // (bracketed when the app enabled paste mode). These are handled here
        // rather than sent to the pty, so Ctrl+C stays SIGINT and Ctrl+V stays
        // literal-next inside the terminal.
        {
            let ctrl_or_cmd = self.mods.control_key() || self.mods.super_key();
            let shift = self.mods.shift_key();
            let is = |lit: &str| matches!(&ev.logical_key, Key::Character(s) if s.eq_ignore_ascii_case(lit));
            if ctrl_or_cmd && shift && is("c") {
                if let Some(text) = self.terminal_selection() {
                    self.egui_ctx.copy_text(text);
                    self.request_redraw();
                }
                return;
            }
            let paste = (ctrl_or_cmd && shift && is("v"))
                || (self.mods.super_key() && is("v"))
                || (shift && matches!(ev.logical_key, Key::Named(NamedKey::Insert)));
            if paste {
                if let Some(text) = self.read_clipboard() {
                    self.paste_to_pane(text);
                }
                return;
            }
        }
        let Some(ck) = crate::keys::to_key_event(&ev.logical_key, self.mods) else {
            return;
        };
        let Some(p) = self.panes.get(self.active) else {
            return;
        };
        let id = p.id;
        let screen = p.parser.screen();
        let app_cursor = screen.application_cursor();
        // Kitty keyboard (4.4): when the pane's flag stack is on, the GUI
        // always synthesizes CSI-u. `encode_key_kitty` returns None when the
        // legacy bytes are already unambiguous (and when flags are 0), so
        // plain text stays plain text.
        let flags = if p.kitty_kill {
            0
        } else {
            screen.kitty_keyboard_flags()
        };
        if let Some(bytes) = encode_key_kitty(&ck, flags).or_else(|| encode_key(&ck, app_cursor)) {
            self.send_input(id, &bytes);
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
        let Some(p) = self.panes.get(self.active) else {
            return;
        };
        let id = p.id;
        let is_agent = p.is_agent();
        let bracketed = p.parser.screen().bracketed_paste();
        // Agent pane: append to the GUI composer the user actually sees/sends
        // (`ui_state.composers`), not the TUI-side `agent.input` the GUI never
        // reads — otherwise the dropped path silently vanished.
        if is_agent {
            let c = self.ui_state.composers.entry(id).or_default();
            if !c.is_empty() && !c.ends_with(' ') {
                c.push(' ');
            }
            c.push_str(&quoted);
            self.request_redraw();
            return;
        }
        let text = format!("{quoted} ");
        let bytes = if bracketed {
            format!("\x1b[200~{text}\x1b[201~").into_bytes()
        } else {
            text.into_bytes()
        };
        self.send_input(id, &bytes);
        self.request_redraw();
    }

    /// The text of the current terminal selection (active pane), if any.
    fn terminal_selection(&self) -> Option<String> {
        let p = self.panes.get(self.active)?;
        crate::ui::term_selection_text(p, self.ui_state.term_sel)
    }

    /// Read the system clipboard (lazy-initializing the handle).
    fn read_clipboard(&mut self) -> Option<String> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
    }

    /// Send `text` to the active pty pane as a paste (bracketed when the app
    /// enabled paste mode). No-op for agent panes.
    fn paste_to_pane(&mut self, text: String) {
        let Some(p) = self.panes.get(self.active) else {
            return;
        };
        if p.is_agent() {
            return;
        }
        let id = p.id;
        let bytes = if p.parser.screen().bracketed_paste() {
            format!("\x1b[200~{text}\x1b[201~").into_bytes()
        } else {
            text.into_bytes()
        };
        self.send_input(id, &bytes);
        self.request_redraw();
    }

    /// OSC 52 write-through (4.7): selection strings containing 'p' target
    /// the primary selection (arboard supports it on Linux); anything else
    /// — including combined "pc" — also lands on the regular clipboard.
    ///
    /// macOS/Windows have no primary selection, and `LinuxClipboardKind` is
    /// compiled out of arboard there, so those platforms fold every target
    /// onto the one system clipboard rather than dropping a 'p' copy.
    fn write_clipboard(&mut self, selection: &str, text: String) {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let Some(cb) = self.clipboard.as_mut() else {
            return;
        };
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        ))]
        {
            use arboard::{LinuxClipboardKind, SetExtLinux};
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
        #[cfg(not(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        )))]
        {
            let _ = selection;
            let _ = cb.set().text(text);
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
        // Same rule as on_key: IME text reaches a pty only in the focused
        // terminal view; elsewhere egui's own text widgets own IME.
        let in_terminal = self.in_terminal();
        if !in_terminal {
            return;
        }
        let Some(p) = self.panes.get(self.active) else {
            return;
        };
        let id = p.id;
        self.send_input(id, text.as_bytes());
        self.request_redraw();
    }

    /// Pointer position -> pane-relative cell (0-based), clamped to the
    /// grid (which starts one line below the tab bar).
    fn grid_cell(&self) -> (u16, u16) {
        // Map through the geometry the terminal was actually drawn with
        // (recorded by `terminal_view`). The legacy wgpu grid's cell size and
        // origin no longer describe what's on screen — the terminal is drawn
        // by egui, inside a frame with margins and a header above it — so
        // using them reported clicks on the wrong cell to mouse-driven TUIs.
        // `cursor_px` is physical; egui lays out in points.
        if let Some(t) = self.ui_state.term_rect {
            let ppp = self.egui_ctx.pixels_per_point().max(0.1) as f64;
            let (cw, ch) = (t.cell.0 as f64, t.cell.1 as f64);
            if cw > 0.5 && ch > 0.5 {
                let (rows, cols) = t.size;
                let x = self.cursor_px.0 / ppp - t.origin.0 as f64;
                let y = self.cursor_px.1 / ppp - t.origin.1 as f64;
                let col = ((x / cw).floor() as i64).clamp(0, cols as i64 - 1);
                let row = ((y / ch).floor() as i64).clamp(0, rows as i64 - 1);
                return (col as u16, row as u16);
            }
        }
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
        // Soak fixture: seed a long agent transcript and sit in it, so the
        // transcript's per-frame cost can be measured against a fresh pane.
        if !self.seeded {
            self.seeded = true;
            if !self.selftest && std::env::var("ZODIAC_GUI_SEED_ITEMS").is_ok() {
                self.seed_agent_pane();
                self.active = self.panes.len().saturating_sub(1);
                self.screen = crate::ui::Screen::Focused;
            }
        }
        // Roll output-rate buckets so idle panes decay toward zero even
        // without new output (task #33; time-gated inside rate_tick).
        for p in &mut self.panes {
            p.rate_tick();
        }
        // The native GUI layer (ADR 0006): run egui for one frame, then hand
        // its tessellated jobs to the renderer's egui present path.
        let win = match self.renderer.as_ref() {
            Some(r) => r.window.clone(),
            None => return,
        };
        if self.egui_state.is_none() {
            return;
        }
        let mut raw = self.egui_state.as_mut().unwrap().take_egui_input(&win);
        // egui-winit is built without its `clipboard` feature (copy is served
        // from our arboard handle), which also means egui never turns Ctrl/⌘+V
        // into a Paste event — so pasting into the composer/search fields did
        // nothing. Detect the paste chord in the egui event stream and inject
        // the system clipboard as an egui Paste. (Harmless in terminal mode:
        // no egui text widget is focused there, so egui ignores it and the
        // pty paste path still runs.)
        let wants_paste = raw.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. }
                    if modifiers.command || modifiers.ctrl
            ) || matches!(
                e,
                egui::Event::Key { key: egui::Key::Insert, pressed: true, modifiers, .. }
                    if modifiers.shift
            )
        });
        if wants_paste {
            if let Some(text) = self.read_clipboard() {
                if !text.is_empty() {
                    raw.events.push(egui::Event::Paste(text));
                }
            }
        }
        let mut actions: Vec<crate::ui::UiAction> = Vec::new();
        // Owned copies so `data` doesn't hold a borrow on `self.settings`
        // while `build` also takes `&mut self.settings`.
        let numeral = if self.settings.card_numeral.is_empty() {
            "roman".to_string()
        } else {
            self.settings.card_numeral.clone()
        };
        let home_view = if self.settings.home_view.is_empty() {
            "cards".to_string()
        } else {
            self.settings.home_view.clone()
        };
        let motion = self.settings.gui_motion != "off";
        // Per-view font sizes (all independent). The GUI/chrome size is applied
        // as egui's global zoom; the terminal and transcript then use scales
        // *relative* to that zoom (view ÷ gui), so after the global zoom each
        // nets its own absolute size — leaving the three fully independent.
        let gui_font = self.settings.gui_font();
        let term_scale = self.settings.term_font() / gui_font;
        let agent_scale = self.settings.agent_font() / gui_font;
        self.egui_ctx.set_zoom_factor(gui_font);
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
                motion,
                numeral: numeral.as_str(),
                home_view: home_view.as_str(),
                term_scale,
                agent_scale,
            };
            crate::ui::build(
                ui,
                &data,
                &mut self.ui_state,
                &mut self.settings,
                &mut actions,
            );
        });
        // egui-winit is built with `default-features = false`, which drops
        // its `clipboard` feature — without it egui's own copy path is an
        // in-app buffer that never reaches the system clipboard, on Linux
        // as much as macOS (so "copy pairing URL" silently did nothing).
        // Serve those commands from the same arboard handle the terminal
        // copy uses rather than linking a second clipboard implementation.
        let mut full = full;
        for cmd in &full.platform_output.commands {
            match cmd {
                egui::OutputCommand::CopyText(text) if !text.is_empty() => {
                    self.write_clipboard("c", text.clone());
                }
                // Clickable transcript links: egui_winit's default-features
                // are off, so nothing opens URLs for us — hand them to the OS
                // default browser (xdg-open on Linux, `open` on macOS).
                egui::OutputCommand::OpenUrl(u) if !u.url.is_empty() => {
                    open_in_browser(&u.url);
                }
                _ => {}
            }
        }
        full.platform_output.commands.retain(|c| {
            !matches!(
                c,
                egui::OutputCommand::CopyText(_) | egui::OutputCommand::OpenUrl(_)
            )
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
        // Clamp the animation cadence. egui asks for a *zero* delay whenever
        // something is animating (a streaming transcript, the thinking
        // spinner); honouring that literally turns `WaitUntil(now)` into a
        // busy-loop that redraws as fast as the CPU allows — a full core while
        // agents stream. Floor it at one frame instead.
        //
        // Occluded (hidden/minimized) windows get no animation frames at all:
        // these redraws are timer-driven, not Wayland frame-callback-driven,
        // so without this an invisible window keeps burning CPU. Unfocused but
        // visible windows keep animating, just slower — watching an agent
        // stream in a background window is the whole point of a multiplexer.
        let floor = if crate::ui::perf_off() {
            Some(Duration::ZERO)
        } else if self.occluded {
            None
        } else if self.focused {
            Some(MIN_FRAME)
        } else {
            Some(UNFOCUSED_FRAME)
        };
        self.next_anim = match (delay, floor) {
            (_, None) => None,
            (Some(d), Some(f)) if d < Duration::from_secs(24 * 3600) => {
                Instant::now().checked_add(d.max(f))
            }
            _ => None,
        };

        // Apply UI actions now that egui's borrows are released.
        for a in actions {
            match a {
                crate::ui::UiAction::Focus(i) => {
                    self.focus(i);
                }
                crate::ui::UiAction::Open(i) => {
                    self.focus(i);
                    self.screen = crate::ui::Screen::Focused;
                }
                crate::ui::UiAction::Back => self.screen = crate::ui::Screen::Observatory,
                crate::ui::UiAction::SaveSettings => {
                    self.settings.save();
                    // Apply theme live so the Theme row takes effect at once.
                    crate::theme::apply(&self.egui_ctx, &self.settings.theme);
                }
                crate::ui::UiAction::Raise => self.send(T_RESTORE, 0, &[]),
                crate::ui::UiAction::CopyText(s) => {
                    // Go through egui's clipboard (the one that works for
                    // transcript label copy) rather than a separate arboard
                    // handle, which doesn't hold a Wayland selection reliably.
                    self.egui_ctx.copy_text(s);
                    self.request_redraw();
                }
                crate::ui::UiAction::DragWindow => {
                    if let Some(r) = &self.renderer {
                        let _ = r.window.drag_window();
                    }
                }
                crate::ui::UiAction::Minimize => {
                    if let Some(r) = &self.renderer {
                        r.window.set_minimized(true);
                    }
                }
                crate::ui::UiAction::Quit => {
                    self.send(T_DETACH, 0, &[]);
                    self.want_quit = true;
                }
                crate::ui::UiAction::ToggleTerm(id) => {
                    if !self.term_mode.remove(&id) {
                        self.term_mode.insert(id);
                    }
                }
                crate::ui::UiAction::SendAgent(id) => {
                    let text = self
                        .ui_state
                        .composers
                        .get(&id)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    self.ui_state.composers.remove(&id);
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
                crate::ui::UiAction::NewAgent => self.open_agent_picker(),
                crate::ui::UiAction::CreateAgent {
                    agent,
                    model,
                    session,
                } => {
                    let mut obj = serde_json::json!({ "kind": "agent", "agent": agent });
                    if let Some(m) = model {
                        obj["model"] = serde_json::Value::String(m);
                    }
                    if let Some(s) = session {
                        obj["session"] = serde_json::Value::String(s);
                    }
                    // Resume in the active pane's directory, not the server's.
                    let active = self.panes.get(self.active).map(|p| p.id);
                    if let Some(cwd) = active.and_then(|id| {
                        self.state
                            .as_ref()
                            .and_then(|st| st.panes.iter().find(|ps| ps.id == id))
                            .and_then(|ps| ps.cwd.clone())
                    }) {
                        obj["cwd"] = serde_json::Value::String(cwd);
                    }
                    self.send(T_NEW_PANE, 0, obj.to_string().as_bytes());
                }
                crate::ui::UiAction::OpenResume(id) => {
                    let ps = self
                        .state
                        .as_ref()
                        .and_then(|st| st.panes.iter().find(|ps| ps.id == id));
                    let cwd = ps.and_then(|ps| ps.cwd.clone());
                    self.ui_state.resume_model =
                        self.panes.iter().find(|p| p.id == id).and_then(|p| {
                            p.agent
                                .model
                                .clone()
                                .or_else(|| ps.and_then(|s| s.model.clone()))
                        });
                    self.ui_state.resume_list = cwd
                        .as_deref()
                        .map(|c| crate::slash::sessions(std::path::Path::new(c)))
                        .unwrap_or_default();
                    self.ui_state.resume_sel = 0;
                    self.ui_state.overlay = crate::ui::Overlay::Resume;
                    self.request_redraw();
                }
            }
        }

        // Size the pty to the focused pane's measured terminal widget (else
        // the window-derived fallback).
        if let Some((rows, cols, cw, ch)) = self.ui_state.term_grid {
            self.apply_grid(rows, cols, cw, ch);
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

/// Open a URL in the OS default browser: `open` on macOS, `xdg-open` on
/// Linux/BSD. Only http(s) URLs are launched (a transcript can contain
/// arbitrary text — never hand `file://`, `javascript:`, etc. to a launcher).
/// Detached and non-blocking; failures are swallowed (best-effort).
fn open_in_browser(url: &str) {
    let low = url.to_ascii_lowercase();
    if !(low.starts_with("http://") || low.starts_with("https://")) {
        return;
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

impl ApplicationHandler<UserEvent> for GuiApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attrs = winit::window::Window::default_attributes()
            .with_title(format!("zodiac — {}", self.session))
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 720.0));
        // Linux/Windows: no decorations at all — our own title bar owns
        // move/minimize/close.
        #[cfg(not(target_os = "macos"))]
        let attrs = attrs.with_decorations(false);
        // macOS: keep the real window frame but make it transparent and let
        // content run full height under it. That is how a Mac app gets
        // genuine traffic lights, rounded corners, snapping and fullscreen
        // while still drawing its own chrome — stripping decorations
        // outright leaves a window with no traffic lights at all, which is
        // the single loudest "this is a Linux port" tell.
        #[cfg(target_os = "macos")]
        let attrs = {
            use winit::platform::macos::WindowAttributesExtMacOS as _;
            attrs
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.exit_msg = Some(format!("zodiac-gui: window creation failed: {e}"));
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);
        // Install once the event loop has an NSApplication to hang it off,
        // which is only true after a window exists. Idempotent: this path
        // is guarded by the `renderer.is_some()` return above.
        #[cfg(target_os = "macos")]
        crate::macos_menu::install();
        let fonts = self.fonts.take().expect("fonts consumed once");
        match Renderer::new(window, fonts) {
            Ok(r) => {
                self.renderer = Some(r);
                // Bring up the egui layer (ADR 0006): theme + winit input
                // bridge, bound to the just-created window.
                let win = self.renderer.as_ref().unwrap().window.clone();
                crate::theme::apply(&self.egui_ctx, &self.settings.theme);
                if let Some(ttf) = crate::font::egui_ui_font() {
                    crate::theme::set_fonts(
                        &self.egui_ctx,
                        ttf,
                        crate::font::egui_fallback_fonts(),
                    );
                }
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
                if self.selftest {
                    self.selftest_next = Some(Instant::now() + Duration::from_millis(400));
                }
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
        let in_terminal = self.in_terminal();
        if let (Some(st), Some(win)) = (self.egui_state.as_mut(), win.as_ref()) {
            let resp = st.on_window_event(win, &event);
            if resp.repaint {
                win.request_redraw();
            }
            consumed = resp.consumed;
            // While a terminal is on screen the pty owns the keyboard. egui
            // treats Tab as focus traversal, so one Tab moved focus onto a
            // widget and from then on egui reported every keystroke as
            // consumed — `on_key` stopped forwarding and the pane looked
            // frozen until a pane switch reset focus. (The pty was always
            // fine: input sent over the CLI still worked.) Overlays and the
            // agent composer are excluded by `in_terminal`, so they keep
            // their keys.
            if in_terminal && matches!(event, WindowEvent::KeyboardInput { .. }) {
                consumed = false;
            }
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
            // Visibility/focus drive the animation throttle (see `next_anim`).
            // Coming back into view needs an explicit redraw: the animation
            // timer was disarmed while hidden, so nothing else would wake us.
            WindowEvent::Occluded(hidden) => {
                self.occluded = hidden;
                if !hidden {
                    self.request_redraw();
                }
            }
            WindowEvent::Focused(has_focus) => {
                self.focused = has_focus;
                self.request_redraw();
            }
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
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
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
        if self.selftest && self.selftest_next.is_some_and(|t| now >= t) {
            self.selftest_next = None;
            self.selftest_tick(event_loop);
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
        let next = [
            self.exit_at,
            self.next_anim,
            self.next_query,
            self.selftest_next,
        ]
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
