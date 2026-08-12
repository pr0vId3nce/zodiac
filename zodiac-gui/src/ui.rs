//! egui UI for the native GUI (ADR 0006). Screen 1 of the rebuild — the
//! Observatory (`draw_home`'s successor): the session's panes as a
//! responsive card grid drawn from the server's `T_STATE`, with the design
//! tokens and the five status colors. The remaining screens (focused pane,
//! oracle, palette, settings, dialogs) grow from here per tasks #24–#29.

use egui::{
    Align, Color32, CornerRadius, Frame, Label, Layout, Margin, Rect, RichText, Sense, Stroke,
};
use zodiac::client_core::CPane;
use zodiac::protocol::{PaneState, SessionState};

use crate::theme;

/// Label a chord with the platform's command modifier. egui binds these to
/// `Modifiers::command`, which is Cmd on macOS and Ctrl elsewhere — so a
/// hardcoded "⌘K" was telling Linux users the wrong key.
macro_rules! concat_cmd {
    ($rest:literal) => {{
        #[cfg(target_os = "macos")]
        {
            concat!("⌘", $rest)
        }
        #[cfg(not(target_os = "macos"))]
        {
            concat!("Ctrl+", $rest)
        }
    }};
}

/// Which screen the GUI is showing (app-shell router, task #24).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// The pane-card home (`draw_home`'s successor).
    Observatory,
    /// One focused pane (the active pane).
    Focused,
}

/// A modal overlay open over the current screen.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Overlay {
    #[default]
    None,
    /// Command palette (⌘K): fuzzy pane/action jump.
    Palette,
    /// Settings dialog (⌘,): grouped config editor.
    Settings,
    /// Pair-a-phone dialog: astrolabe pairing QR.
    Pairing,
    /// The Oracle chat panel (Alt+O).
    Oracle,
    /// Raise-the-last-session dialog (Alt+Shift+R).
    Raise,
    /// New-agent picker: choose a harness, then a model (Alt+N).
    NewAgent,
    /// Session picker for `/resume` in a structured claude pane.
    Resume,
}

/// One selectable model for a harness in the new-agent picker.
#[derive(Clone)]
pub struct ModelChoice {
    /// Passed to the harness as its `--model` value; `None` = harness default.
    pub value: Option<String>,
    pub label: String,
}

/// A locally-available agent harness and the models it can run.
#[derive(Clone)]
pub struct HarnessInfo {
    /// The `agent` value for `T_NEW_PANE` (e.g. "claude", "pi").
    pub name: String,
    /// Display label (e.g. "Claude").
    pub label: String,
    pub models: Vec<ModelChoice>,
}

/// New-agent picker state: the detected harnesses and where the user is in
/// the two-step (harness → model) flow. Populated when the picker opens.
#[derive(Default)]
pub struct AgentPicker {
    pub harnesses: Vec<HarnessInfo>,
    /// 0 = choosing a harness, 1 = choosing a model.
    pub step: usize,
    pub h_sel: usize,
    pub m_sel: usize,
}

/// Mutable UI state egui edits in place across frames (buffers + overlays).
#[derive(Default)]
pub struct UiState {
    /// Per-pane composer drafts (keyed by pane id), so a draft survives
    /// switching panes and each agent keeps its own in-progress message.
    pub composers: std::collections::HashMap<u64, String>,
    /// The focused terminal's drawn geometry, for mouse reporting.
    pub term_rect: Option<TermRect>,
    /// Probes the e2e harness asserts on (see `app.rs::e2e_tick`). Recording
    /// them costs a couple of stores per frame and keeps the harness honest:
    /// it checks what was actually laid out, not what we think was.
    pub probe: Probe,
    /// Highlighted row in the slash-command picker.
    pub slash_sel: usize,
    /// `/resume` session picker: the candidates and the highlighted row.
    pub resume_list: Vec<crate::slash::SessionEntry>,
    pub resume_sel: usize,
    /// Model the pane being resumed was launched with.
    pub resume_model: Option<String>,
    /// The pane `/resume` was invoked from — it resumes in place.
    pub resume_pane: Option<u64>,
    /// Measured transcript item heights per pane, used to cull off-screen
    /// turns. Rebuilt lazily as items render; cleared on a width change.
    pub item_heights: std::collections::HashMap<u64, Vec<f32>>,
    /// Which modal overlay is open.
    pub overlay: Overlay,
    /// Command-palette query + selected row.
    pub palette_query: String,
    pub palette_sel: usize,
    /// Selected option in the permission popup (0 = allow, 1 = deny).
    pub perm_sel: usize,
    /// An in-progress/last terminal text selection (cleared on pane switch).
    pub term_sel: Option<TermSel>,
    /// New-agent picker state (harness + model), populated when it opens.
    pub agent_picker: AgentPicker,
    /// The focused pane's measured terminal-area grid: (rows, cols, cell_w_px,
    /// cell_h_px). The app sends this as `T_RESIZE` so the pty matches the
    /// actual egui terminal widget, not the legacy full-window grid.
    pub term_grid: Option<(u16, u16, u16, u16)>,
    /// Decoded kitty-image textures for terminal mode, keyed by (pane, img,
    /// ver). egui owns the GPU upload; we just cache the handle.
    pub tex_cache: std::collections::HashMap<(u64, u32, u32), egui::TextureHandle>,
}

/// Layout facts recorded during a frame for the e2e harness to assert on.
#[derive(Clone, Copy, Default)]
pub struct Probe {
    /// The transcript's viewport rect, and the widest user-bubble drawn in it.
    pub transcript: Option<egui::Rect>,
    pub user_bubble: Option<egui::Rect>,
    /// How many slash-command rows the picker offered (0 = closed).
    pub slash_rows: usize,
}

/// Where a terminal's grid is actually drawn, in egui points: its top-left,
/// its cell size, and its dimensions. Mouse reporting maps pointer positions
/// through this so clicks land on the cell the user sees.
#[derive(Clone, Copy)]
pub struct TermRect {
    pub origin: (f32, f32),
    pub cell: (f32, f32),
    pub size: (u16, u16),
}

/// A terminal text selection: visible-grid cell coords (row, col) in pane
/// `pane`, from `anchor` (where the drag began) to `head` (the cursor now).
#[derive(Clone, Copy)]
pub struct TermSel {
    pub pane: u64,
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl TermSel {
    /// The selection as a normalized (start, end) pair in reading order.
    pub fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
    /// Is cell (row, col) within the selection?
    pub fn contains(&self, row: usize, col: usize) -> bool {
        let (s, e) = self.ordered();
        (row, col) >= s && (row, col) <= e
    }
}

/// Things the UI wants the app to do after a frame. Applied by `redraw`
/// once egui's borrows are released.
pub enum UiAction {
    /// Focus the pane at this index (T_FOCUS + active), staying on the grid.
    Focus(usize),
    /// Focus the pane and open the focused-pane screen.
    Open(usize),
    /// Return to the Observatory.
    Back,
    /// Toggle transcript/terminal display for the pane id.
    ToggleTerm(u64),
    /// Send the composer buffer to the agent pane id as T_AGENT_INPUT.
    SendAgent(u64),
    /// Answer the pane's first pending permission request (true = allow).
    Perm(u64, bool),
    /// A settings control changed — persist config.json.
    SaveSettings,
    /// Raise the last session (server reopens missing panes) — T_RESTORE.
    Raise,
    /// Copy text to the system clipboard (e.g. the pairing URL).
    CopyText(String),
    /// Custom window chrome: start an interactive window move.
    DragWindow,
    /// Custom window chrome: minimize the window.
    Minimize,
    /// Custom window chrome: close the window (detach + exit).
    Quit,
    /// Spawn a new shell pane.
    NewShell,
    /// Open the new-agent picker (choose harness + model).
    NewAgent,
    /// Create an agent pane with the chosen harness + model (from the picker).
    CreateAgent {
        agent: String,
        model: Option<String>,
        /// Resume this harness session id instead of starting fresh.
        session: Option<String>,
    },
    /// `/resume` was submitted: open the session picker for this pane.
    OpenResume(u64),
    /// Resume `session` in the pane that asked, keeping its identity.
    ResumeHere(u64, String),
    /// Advance this agent pane to the next permission mode (shift+tab).
    CyclePermMode(u64),
}

/// Immutable view of the app state the UI reads for one frame.
pub struct UiData<'a> {
    pub session: &'a str,
    pub panes: &'a [CPane],
    pub state: Option<&'a SessionState>,
    pub active: usize,
    pub screen: Screen,
    /// The active pane is showing terminal mode (vs. native transcript).
    pub term_active: bool,
    /// The server's current pairing token (for the Pair-phone QR).
    pub pairing_token: &'a str,
    /// Animations enabled (Motion setting != "off").
    pub motion: bool,
    /// Sigil numeral style (card_numeral): "roman" | "arabic" | "zodiac".
    pub numeral: &'a str,
    /// Observatory layout (home_view): "cards" | "list".
    pub home_view: &'a str,
    /// Terminal font scale relative to the GUI zoom (terminal size ÷ gui size),
    /// so the terminal view sizes independently of the chrome zoom.
    pub term_scale: f32,
    /// Structured-transcript font scale relative to the GUI zoom (agent size ÷
    /// gui size), so the agent transcript sizes independently of the chrome.
    pub agent_scale: f32,
}

impl UiData<'_> {
    /// The server's `PaneState` for a client pane, matched by id.
    fn ps(&self, p: &CPane) -> Option<&PaneState> {
        self.state
            .and_then(|s| s.panes.iter().find(|sp| sp.id == p.id))
    }

    /// Server status string (`idle` when unknown).
    fn status(&self, p: &CPane) -> &str {
        self.ps(p).map(|sp| sp.status.as_str()).unwrap_or("idle")
    }

    /// The 0..5 status index, honoring the live thinking flag.
    fn si(&self, p: &CPane) -> usize {
        let thinking = self.ps(p).map(|sp| sp.thinking).unwrap_or(false);
        theme::status_index(self.status(p), thinking)
    }
}

/// Roman numeral for a 1-based index (the sigil in the card tiles).
fn roman(mut n: usize) -> String {
    const M: &[(usize, &str)] = &[(10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut s = String::new();
    for &(v, r) in M {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if s.is_empty() {
        s.push('·');
    }
    s
}

/// The sigil label for tab/card index `n` in the chosen numeral style.
fn numeral(style: &str, n: usize) -> String {
    match style {
        "arabic" => n.to_string(),
        "zodiac" => crate::palette::marker("zodiac", n),
        _ => roman(n),
    }
}

/// Compact uptime like the header's `↑2h 14m`.
fn fmt_age(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m) = (s / 3600, (s % 3600) / 60);
    if h > 0 {
        format!("↑{h}h {m}m")
    } else if m > 0 {
        format!("↑{m}m")
    } else {
        format!("↑{s}s")
    }
}

/// One line, clipped to `max` chars with an ellipsis.
fn clip(s: &str, max: usize) -> String {
    let s = s.replace(['\n', '\r'], " ");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Clip multi-line text to at most `max` lines, noting how many were dropped
/// (keeps a huge tool result from blowing up the transcript).
fn clip_lines(s: &str, max: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max {
        return s.to_string();
    }
    let extra = lines.len() - max;
    let mut out = lines[..max].join("\n");
    out.push_str(&format!("\n… (+{extra} more lines)"));
    out
}

/// Build the frame's UI, pushing any resulting actions. egui 0.36 hands the
/// integration a root `&mut Ui`; panels are shown into it.
pub fn build(
    ui: &mut egui::Ui,
    d: &UiData,
    st: &mut UiState,
    settings: &mut zodiac::settings::Settings,
    actions: &mut Vec<UiAction>,
) {
    // Chrome shortcuts: ⌘K palette, ⌘, settings.
    if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K)) {
        st.overlay = Overlay::Palette;
        st.palette_query.clear();
        st.palette_sel = 0;
    }
    if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Comma)) {
        st.overlay = Overlay::Settings;
    }
    if ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::O)) {
        st.overlay = Overlay::Oracle;
    }
    if ui.input(|i| i.modifiers.alt && i.modifiers.shift && i.key_pressed(egui::Key::R)) {
        st.overlay = Overlay::Raise;
    }
    if d.screen != Screen::Focused {
        st.term_grid = None;
        st.term_rect = None;
    }
    title_bar(ui, d, st, actions);
    match d.screen {
        Screen::Observatory => observatory(ui, d, actions),
        Screen::Focused => focused(ui, d, st, actions),
    }
    match st.overlay {
        Overlay::Palette => palette(ui, d, st, actions),
        Overlay::Settings => settings_dialog(ui, st, settings, actions),
        Overlay::Pairing => pairing_dialog(ui, d, st, actions),
        Overlay::Oracle => oracle_dialog(ui, st, d.motion),
        Overlay::Raise => raise_dialog(ui, d, st, actions),
        Overlay::NewAgent => new_agent_dialog(ui, st, actions),
        Overlay::Resume => resume_dialog(ui, st, actions),
        Overlay::None => {}
    }
    // A 1px edge border to define the borderless window (decorations are off).
    let sr = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_stroke(
        sr.shrink(0.5),
        CornerRadius::ZERO,
        Stroke::new(1.0, theme::LINE_BORDER_STRONG),
        egui::StrokeKind::Inside,
    );
}

/// Raise-the-last-session dialog (Alt+Shift+R): reads the on-disk snapshot
/// (`zodiac::snapshot::best`) and lists the panes it would reopen; "Raise
/// them" sends one `T_RESTORE` (the server reopens all missing panes and
/// replays each pane's restore command — it's all-or-nothing at the protocol
/// level, so this is a preview + commit, not per-pane selection).
fn raise_dialog(ui: &mut egui::Ui, d: &UiData, st: &mut UiState, actions: &mut Vec<UiAction>) {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.overlay = Overlay::None;
    }
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );
    let snap = zodiac::snapshot::best(d.session);
    let mut close = false;
    egui::Area::new(egui::Id::new("raise"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(theme::bg_card())
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(620.0);
                    ui.label(
                        RichText::new("Raise the last session")
                            .color(theme::TEXT_PRIMARY)
                            .size(18.0)
                            .strong(),
                    );
                    match &snap {
                        Some(s) if !s.panes.is_empty() => {
                            let age = now_secs().saturating_sub(s.saved_at);
                            ui.label(
                                RichText::new(format!(
                                    "Snapshot from {} ago · panes already running are left alone.",
                                    fmt_uptime(age * 1000)
                                ))
                                .color(theme::TEXT_FAINT)
                                .size(12.5),
                            );
                            ui.add_space(12.0);
                            egui::ScrollArea::vertical()
                                .max_height(340.0)
                                .show(ui, |ui| {
                                    for sp in &s.panes {
                                        Frame::NONE.inner_margin(Margin::symmetric(4, 6)).show(
                                            ui,
                                            |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        RichText::new(numeral(
                                                            d.numeral,
                                                            sp.index.max(1),
                                                        ))
                                                        .color(theme::accent())
                                                        .size(13.0)
                                                        .monospace(),
                                                    );
                                                    ui.add_space(8.0);
                                                    ui.vertical(|ui| {
                                                        ui.label(
                                                            RichText::new(clip(&sp.name, 40))
                                                                .color(theme::TEXT_PRIMARY)
                                                                .size(13.5)
                                                                .strong(),
                                                        );
                                                        let cmd = sp
                                                            .restore_command(None)
                                                            .unwrap_or_else(|| {
                                                                sp.cwd.clone().unwrap_or_default()
                                                            });
                                                        ui.add(
                                                            Label::new(
                                                                RichText::new(clip(&cmd, 76))
                                                                    .color(theme::TEXT_DIM)
                                                                    .size(11.5)
                                                                    .monospace(),
                                                            )
                                                            .truncate(),
                                                        );
                                                    });
                                                });
                                            },
                                        );
                                    }
                                });
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{} panes", s.panes.len()))
                                        .color(theme::TEXT_FAINT)
                                        .size(12.0),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if amber_button(ui, "Raise them").clicked() {
                                        actions.push(UiAction::Raise);
                                        close = true;
                                    }
                                    ui.add_space(8.0);
                                    if ui.button(RichText::new("Cancel").size(13.0)).clicked() {
                                        close = true;
                                    }
                                });
                            });
                        }
                        _ => {
                            ui.add_space(20.0);
                            ui.label(
                                RichText::new("no saved session to raise")
                                    .color(theme::TEXT_FAINT)
                                    .size(14.0),
                            );
                            ui.add_space(20.0);
                            ui.vertical_centered(|ui| {
                                if amber_button(ui, "Close").clicked() {
                                    close = true;
                                }
                            });
                        }
                    }
                });
        });
    if close {
        st.overlay = Overlay::None;
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The Oracle panel (Alt+O): the CSS-gradient orb + status + slash chips and
/// an input row. Presentational — the GUI has no chat wiring yet and makes no
/// network calls (chat send/receive is a follow-on).
fn oracle_dialog(ui: &mut egui::Ui, st: &mut UiState, motion: bool) {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.overlay = Overlay::None;
    }
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 150),
    );
    let mut close = false;
    egui::Area::new(egui::Id::new("oracle"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(Color32::from_rgb(0x0a, 0x0f, 0x14))
                .stroke(Stroke::new(1.0, fade(theme::VIOLET, 0.35)))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(0))
                .show(ui, |ui| {
                    ui.set_width(520.0);
                    // Orb.
                    orb(ui, 180.0, motion);
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("The Oracle")
                                .color(theme::TEXT_PRIMARY)
                                .size(15.0)
                                .strong(),
                        );
                        ui.label(
                            RichText::new("not configured · set chat_endpoint / chat_model")
                                .color(theme::TEXT_FAINT)
                                .size(12.0),
                        );
                    });
                    ui.add_space(12.0);
                    Frame::NONE
                        .inner_margin(Margin::symmetric(18, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new(
                                        "The Oracle answers over an OpenAI-compatible endpoint on \
                                         your tailnet. Set chat_endpoint and chat_model, then \
                                         enable the chat panel.",
                                    )
                                    .color(theme::TEXT_FAINT)
                                    .size(12.5),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("commands · /why  /read  /wake  /sleep")
                                        .color(theme::TEXT_GHOST)
                                        .size(12.0)
                                        .monospace(),
                                );
                                ui.add_space(12.0);
                                ui.horizontal(|ui| {
                                    ui.add_space((ui.available_width() - 190.0).max(0.0) / 2.0);
                                    if amber_button(ui, "Open Settings").clicked() {
                                        st.overlay = Overlay::Settings;
                                    }
                                    ui.add_space(8.0);
                                    if ui.button(RichText::new("Close").size(13.0)).clicked() {
                                        close = true;
                                    }
                                });
                            });
                        });
                });
        });
    if close {
        st.overlay = Overlay::None;
    }
}

/// The Oracle orb: a radial gradient approximated by concentric filled
/// circles (edge → core), with the highlight offset up-left like the mock.
fn orb(ui: &mut egui::Ui, h: f32, motion: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        CornerRadius::ZERO,
        Color32::from_rgb(0x05, 0x08, 0x0c),
    );
    let r = h * 0.72;
    let center = egui::pos2(rect.center().x, rect.center().y + 6.0);
    let highlight = egui::pos2(center.x - r * 0.24, center.y - r * 0.30);
    // Gradient stops (t, rgb): edge → core.
    let stops = [
        (1.00, [0x16, 0x0f, 0x2c]),
        (0.72, [0x5b, 0x3f, 0xa8]),
        (0.42, [0xa8, 0x74, 0xf0]),
        (0.00, [0xe7, 0xd9, 0xff]),
    ];
    let lerp = |a: u8, b: u8, t: f32| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    let steps = 40;
    for i in (0..steps).rev() {
        let t = i as f32 / (steps - 1) as f32; // 1 at edge, 0 at core
                                               // Find bracketing stops.
        let mut col = stops[0].1;
        for w in stops.windows(2) {
            let (t0, c0) = w[0];
            let (t1, c1) = w[1];
            if t <= t0 && t >= t1 {
                let f = if (t0 - t1).abs() < f32::EPSILON {
                    0.0
                } else {
                    (t0 - t) / (t0 - t1)
                };
                col = [
                    lerp(c0[0], c1[0], f),
                    lerp(c0[1], c1[1], f),
                    lerp(c0[2], c1[2], f),
                ];
                break;
            }
        }
        // Interpolate center toward the highlight as we approach the core.
        let c = egui::pos2(
            center.x + (highlight.x - center.x) * (1.0 - t),
            center.y + (highlight.y - center.y) * (1.0 - t),
        );
        painter.circle_filled(c, r * t, Color32::from_rgb(col[0], col[1], col[2]));
    }
    // Rim.
    painter.circle_stroke(
        center,
        r,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 34)),
    );
    // Keep animating (drift is a follow-on) unless Motion is off.
    if motion {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(66));
    }
}

/// Read the astrolabe bridge endpoint `(url, cid, name)` from
/// `~/.local/state/astrolabe/endpoint.json` (mirrors the TUI).
fn read_bridge_endpoint() -> Option<(String, String, String)> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/state")
        });
    let raw = std::fs::read_to_string(base.join("astrolabe").join("endpoint.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("url")?.as_str()?.to_string(),
        v.get("cid")?.as_str()?.to_string(),
        v.get("name")?.as_str()?.to_string(),
    ))
}

/// Percent-encode a query-param value (mirrors the TUI's `url_encode`).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Pair-a-phone dialog: renders the astrolabe pairing URL as a QR (qrcode →
/// egui rects), with the URL below. "no bridge detected" when the endpoint
/// file is absent. No network calls.
fn pairing_dialog(ui: &mut egui::Ui, d: &UiData, st: &mut UiState, actions: &mut Vec<UiAction>) {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.overlay = Overlay::None;
    }
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );
    let endpoint = read_bridge_endpoint();
    let mut close = false;
    egui::Area::new(egui::Id::new("pairing"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(theme::bg_card())
                .stroke(Stroke::new(1.0, fade(theme::accent(), 0.3)))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(420.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Pair a phone")
                                .color(theme::TEXT_PRIMARY)
                                .size(18.0)
                                .strong(),
                        );
                        ui.label(
                            RichText::new("astrolabe · over your tailnet")
                                .color(theme::TEXT_FAINT)
                                .size(12.0),
                        );
                        ui.add_space(14.0);
                        match (&endpoint, d.pairing_token.is_empty()) {
                            (Some((url, cid, name)), false) => {
                                let pair_url = format!(
                                    "{url}/?t={}&cid={}&name={}",
                                    url_encode(d.pairing_token),
                                    url_encode(cid),
                                    url_encode(name),
                                );
                                qr(ui, &pair_url);
                                ui.add_space(12.0);
                                ui.add(
                                    Label::new(
                                        RichText::new(&pair_url)
                                            .color(theme::TEXT_DIM)
                                            .size(11.0)
                                            .monospace(),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(8.0);
                                if amber_button(ui, "Copy link").clicked() {
                                    actions.push(UiAction::CopyText(pair_url.clone()));
                                }
                            }
                            _ => {
                                ui.add_space(30.0);
                                ui.label(
                                    RichText::new("no bridge detected")
                                        .color(theme::TEXT_FAINT)
                                        .size(14.0),
                                );
                                ui.label(
                                    RichText::new("start astrolabe on this machine, then re-open")
                                        .color(theme::TEXT_GHOST)
                                        .size(12.0),
                                );
                                ui.add_space(30.0);
                            }
                        }
                        ui.add_space(14.0);
                        if amber_button(ui, "Done").clicked() {
                            close = true;
                        }
                    });
                });
        });
    if close {
        st.overlay = Overlay::None;
    }
}

/// Render a QR of `data` as black modules on a white quiet-zone tile.
fn qr(ui: &mut egui::Ui, data: &str) {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        ui.label(RichText::new("QR encode failed").color(theme::TEXT_FAINT));
        return;
    };
    let colors = code.to_colors();
    let n = code.width();
    let quiet = 2usize;
    let side = n + quiet * 2;
    let px = 6.0f32;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(side as f32 * px, side as f32 * px),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
    for y in 0..n {
        for x in 0..n {
            if colors[y * n + x] == qrcode::Color::Dark {
                let px0 = rect.min.x + (x + quiet) as f32 * px;
                let py0 = rect.min.y + (y + quiet) as f32 * px;
                painter.rect_filled(
                    Rect::from_min_size(egui::pos2(px0, py0), egui::vec2(px, px)),
                    CornerRadius::ZERO,
                    Color32::BLACK,
                );
            }
        }
    }
}

/// Subsequence fuzzy score: all pattern chars must appear in order; bonuses
/// for consecutive runs and word-boundary starts. None = no match. (A small
/// local scorer — the TUI's `fuzzy_score` lives in the binary, not the lib.)
fn fuzzy_score(text: &str, pattern: &str) -> Option<i32> {
    if pattern.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut ti = 0usize;
    let mut prev_match = false;
    let mut prev_char = ' ';
    for &pc in &p {
        let mut found = false;
        while ti < t.len() {
            let c = t[ti];
            ti += 1;
            if c == pc {
                score += 1;
                if prev_match {
                    score += 3;
                }
                if prev_char == ' ' || prev_char == '/' || prev_char == '-' || prev_char == '_' {
                    score += 5;
                }
                prev_match = true;
                prev_char = c;
                found = true;
                break;
            }
            prev_match = false;
            prev_char = c;
        }
        if !found {
            return None;
        }
    }
    Some(score)
}

/// Command palette (⌘K): a centered modal — query row + fuzzy-ranked pane
/// list; ↑/↓ move, Enter opens, Esc closes.
fn palette(ui: &mut egui::Ui, d: &UiData, st: &mut UiState, actions: &mut Vec<UiAction>) {
    // Ranked matches (index, score), best first.
    let mut hits: Vec<(usize, i32)> = d
        .panes
        .iter()
        .enumerate()
        .filter_map(|(i, p)| fuzzy_score(&p.name, &st.palette_query).map(|s| (i, s)))
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if st.palette_sel >= hits.len() {
        st.palette_sel = hits.len().saturating_sub(1);
    }
    // Keys.
    let (up, down, enter, esc) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    if down && !hits.is_empty() {
        st.palette_sel = (st.palette_sel + 1).min(hits.len() - 1);
    }
    if up {
        st.palette_sel = st.palette_sel.saturating_sub(1);
    }
    if esc {
        st.overlay = Overlay::None;
    }
    if enter {
        if let Some(&(idx, _)) = hits.get(st.palette_sel) {
            actions.push(UiAction::Open(idx));
        }
        st.overlay = Overlay::None;
    }

    // Dim scrim.
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );
    egui::Area::new(egui::Id::new("palette"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 120.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(theme::bg_card())
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(600.0);
                    // Query row.
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⌕").color(theme::accent()).size(16.0));
                        let edit = egui::TextEdit::singleline(&mut st.palette_query)
                            .frame(Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .hint_text("jump to pane…")
                            .font(egui::FontId::proportional(16.0))
                            .text_color(theme::TEXT_PRIMARY);
                        let r = ui.add(edit);
                        r.request_focus();
                    });
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    // Results.
                    for (row, &(idx, _)) in hits.iter().enumerate() {
                        let p = &d.panes[idx];
                        let si = d.si(p);
                        let sel = row == st.palette_sel;
                        let rr = Frame::NONE
                            .fill(if sel {
                                theme::bg_selected()
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(CornerRadius::same(9))
                            .inner_margin(Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(numeral(d.numeral, idx + 1))
                                            .color(theme::accent())
                                            .size(13.0)
                                            .monospace(),
                                    );
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(clip(&p.name, 40))
                                            .color(theme::TEXT_PRIMARY)
                                            .size(14.0),
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(
                                            RichText::new(theme::STATUS_WORD[si])
                                                .color(theme::STATUS_TEXT[si])
                                                .size(12.0),
                                        );
                                    });
                                });
                            });
                        if rr.response.interact(Sense::click()).clicked() {
                            actions.push(UiAction::Open(idx));
                            st.overlay = Overlay::None;
                        }
                    }
                    if hits.is_empty() {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("no matching panes")
                                .color(theme::TEXT_FAINT)
                                .size(13.0),
                        );
                        ui.add_space(6.0);
                    }
                });
        });
}

/// The focused-pane screen (task #26): sidebar · main (header + transcript
/// or terminal + composer) · activity rail. Esc returns to the Observatory.
/// Terminal mode's real grid/kitty compositing lands in task #26b; for now
/// it shows the rendered-screen tail from `T_STATE`.
fn focused(root: &mut egui::Ui, d: &UiData, st: &mut UiState, actions: &mut Vec<UiAction>) {
    // Esc closes only when the composer isn't focused (so it can clear text).
    if root.input(|i| i.key_pressed(egui::Key::Escape)) && root.memory(|m| m.focused().is_none()) {
        actions.push(UiAction::Back);
    }
    egui::Panel::left("sidebar")
        .exact_size(268.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(theme::bg_chrome())
                .inner_margin(Margin::same(10)),
        )
        .show(root, |ui| sidebar(ui, d, actions));
    egui::Panel::right("rail")
        .exact_size(288.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(theme::bg_panel())
                .inner_margin(Margin::same(14)),
        )
        .show(root, |ui| activity_rail(ui, d));
    egui::CentralPanel::default()
        .frame(
            Frame::NONE
                .fill(theme::bg_window())
                .inner_margin(Margin::same(0)),
        )
        .show(root, |ui| main_pane(ui, d, st, actions));
}

/// The left sidebar: masthead, pane rows (click to switch), footer.
fn sidebar(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    ui.label(
        RichText::new("PANES")
            .color(theme::TEXT_GHOST)
            .size(11.0)
            .strong(),
    );
    ui.add_space(8.0);
    egui::ScrollArea::vertical()
        .id_salt("sidebar_scroll")
        .show(ui, |ui| {
            for (i, p) in d.panes.iter().enumerate() {
                let si = d.si(p);
                let sel = i == d.active;
                let fr = Frame::NONE
                    .fill(if sel {
                        theme::bg_selected()
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(CornerRadius::same(9))
                    .inner_margin(Margin::symmetric(10, 9))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(numeral(d.numeral, i + 1))
                                    .color(if sel {
                                        theme::accent()
                                    } else {
                                        theme::STATUS_TEXT[si]
                                    })
                                    .size(13.0)
                                    .monospace(),
                            );
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(clip(&p.name, 22))
                                        .color(if sel {
                                            theme::TEXT_PRIMARY
                                        } else {
                                            theme::TEXT_BODY
                                        })
                                        .size(13.5)
                                        .strong(),
                                );
                                let meta = d
                                    .ps(p)
                                    .and_then(|s| s.agent.clone())
                                    .unwrap_or_else(|| "shell".into());
                                ui.label(
                                    RichText::new(format!("{meta} · {}", theme::STATUS_WORD[si]))
                                        .color(theme::TEXT_GHOST)
                                        .size(11.5),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let (dot, _) =
                                    ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
                                ui.painter().circle_filled(
                                    dot.center(),
                                    3.5,
                                    theme::STATUS_RAIL[si],
                                );
                            });
                        });
                    });
                if fr
                    .response
                    .interact(Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    actions.push(UiAction::Focus(i));
                }
                ui.add_space(4.0);
            }
        });
}

/// The main column: pane header, then transcript or terminal, then composer.
fn main_pane(ui: &mut egui::Ui, d: &UiData, st: &mut UiState, actions: &mut Vec<UiAction>) {
    let Some(p) = d.panes.get(d.active) else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("no pane").color(theme::TEXT_FAINT));
        });
        return;
    };
    let ps = d.ps(p);
    let si = d.si(p);
    // Header.
    Frame::NONE
        .fill(theme::bg_chrome())
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                sigil_tile(ui, d.active + 1, si, d.numeral);
                ui.add_space(10.0);
                ui.label(
                    RichText::new(clip(&p.name, 40))
                        .color(theme::TEXT_PRIMARY)
                        .size(16.0)
                        .strong(),
                );
                if let Some(agent) = ps.and_then(|s| s.agent.as_deref()) {
                    agent_chip(ui, agent, ps.and_then(|s| s.version.as_deref()));
                }
                status_pill(ui, si, theme::STATUS_WORD[si]);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Agent panes show their model (they're headless structured
                    // NDJSON — no pty); pty panes show "terminal". Prefer the
                    // model read live from the stream (claude); fall back to the
                    // model the pane was launched with (pi doesn't report one),
                    // else "structured" until it's known.
                    let (label, col): (String, Color32) = if p.is_agent() {
                        let model = p.agent.model.clone().or_else(|| {
                            ps.and_then(|s| s.model.as_deref())
                                .map(zodiac::client_core::short_model)
                        });
                        (
                            model.unwrap_or_else(|| "structured".into()),
                            theme::VIOLET_TEXT,
                        )
                    } else {
                        ("terminal".to_string(), theme::TEXT_DIM)
                    };
                    Frame::NONE
                        .fill(theme::bg_raised())
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(RichText::new(label).color(col).size(11.0));
                        });
                    // Permission mode, next to the model. Claude announces no
                    // mode at startup, so until it reports one we show its
                    // documented default rather than an empty slot.
                    if p.is_agent() && ps.and_then(|s| s.agent.as_deref()) == Some("claude") {
                        let mode = p.agent.perm_mode.as_deref().unwrap_or("manual");
                        let label = zodiac::protocol::perm_mode_label(mode);
                        let col = match label {
                            "plan" => theme::VIOLET_TEXT,
                            "auto" => theme::accent(),
                            "bypass" => theme::STATUS_RAIL[0],
                            _ => theme::TEXT_DIM,
                        };
                        ui.add_space(6.0);
                        let r = Frame::NONE
                            .fill(theme::bg_raised())
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.label(RichText::new(label).color(col).size(11.0));
                            });
                        r.response
                            .interact(Sense::hover())
                            .on_hover_text("permission mode — shift+tab to cycle");
                    }
                });
            });
        });
    // Measure the body area (below the header) in cells at the terminal
    // font, so the pty is sized to the actual egui terminal widget rather
    // than the legacy full-window grid. Recorded for the app to send as
    // T_RESIZE after the frame.
    // (The pty grid is measured inside `terminal_view`, against the real
    // content rect. Measuring here — before the terminal frame's margins —
    // over-reported the size by a couple of rows and columns, so the child
    // drew into space that was never painted: a full-width TUI like Claude
    // Code lost its right border and had its bottom line clipped in half.)
    // Shift+Tab cycles the permission mode. Consumed here in the egui frame
    // because egui treats Shift+Tab as reverse focus traversal and the
    // composer usually holds focus, so the raw key path never sees it.
    if p.is_agent() && d.ps(p).and_then(|s| s.agent.as_deref()) == Some("claude") {
        let hit = ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) && i.modifiers.shift
        });
        if hit {
            actions.push(UiAction::CyclePermMode(p.id));
        }
    }
    // Body: the view is determined by the pane kind (see the header note).
    let show_term = !p.is_agent();
    if show_term {
        terminal_view(ui, st, p, d.term_scale);
    } else {
        // A pending permission takes over the bottom bar and raises a modal
        // question popup; otherwise the composer sits there. Transcript fills
        // the space above either way.
        let has_perm = !p.agent.perms.is_empty();
        // PageUp/PageDown scroll the transcript. Consume them here — before
        // the composer's text field renders — so they drive the scroll view
        // rather than moving the composer cursor. ~90% of a viewport per press.
        let scroll_dy = ui.input_mut(|i| {
            let page = i.raw.screen_rect.map_or(600.0, |r| r.height()) * 0.9;
            let mut dy = 0.0;
            if i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                dy += page;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                dy -= page;
            }
            dy
        });
        egui::Panel::bottom("composer")
            .resizable(false)
            .frame(Frame::NONE.fill(theme::bg_chrome()))
            .show(ui, |ui| {
                if has_perm {
                    perm_hint(ui);
                } else {
                    // Slash commands are a Claude Code feature; only offer the
                    // picker for panes actually running that harness.
                    let cmds = if d.ps(p).and_then(|s| s.agent.as_deref()) == Some("claude") {
                        crate::slash::commands(
                            d.ps(p)
                                .and_then(|s| s.cwd.as_deref())
                                .map(std::path::Path::new),
                        )
                    } else {
                        Vec::new()
                    };
                    // Disjoint field borrows so the picker's selection and the
                    // pane's draft can both be edited.
                    let (composers, slash_sel) = (&mut st.composers, &mut st.slash_sel);
                    composer_bar(
                        ui,
                        p,
                        composers.entry(p.id).or_default(),
                        slash_sel,
                        actions,
                        &cmds,
                    );
                }
            });
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(theme::bg_window()))
            .show(ui, |ui| {
                transcript_view(
                    ui,
                    p,
                    scroll_dy,
                    d.agent_scale,
                    st.item_heights.entry(p.id).or_default(),
                )
            });
        if has_perm {
            perm_popup(ui, p, st, actions);
        }
    }
}

/// Playful gerunds shown next to the spinner while a turn is thinking —
/// the Claude Code "orange sayings" ported to the GUI.
const THINKING_WORDS: &[&str] = &[
    "Cogitating",
    "Percolating",
    "Pondering",
    "Ruminating",
    "Musing",
    "Deliberating",
    "Contemplating",
    "Noodling",
    "Conjuring",
    "Synthesizing",
    "Distilling",
    "Puzzling",
    "Marinating",
    "Simmering",
    "Scheming",
];

/// A braille spinner frame for the current egui time (80ms cadence).
fn spinner_frame(t: f64) -> char {
    const F: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    F[((t * 12.5) as usize) % F.len()]
}

thread_local! {
    /// Frame-local probe, drained into `UiState` at the end of the frame.
    static PROBE: std::cell::Cell<Probe> = const { std::cell::Cell::new(Probe {
        transcript: None,
        user_bubble: None,
        slash_rows: 0,
    }) };

    /// The agent-pane font scale (structured-transcript size ÷ GUI zoom), set
    /// only for the duration of [`transcript_view`]. 1.0 everywhere else, so
    /// shared markdown helpers render at chrome size when used off-transcript.
    static TRANSCRIPT_SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };

    /// Per-item table counter, reset for each transcript item, and the index
    /// of the item being drawn. Together they give every table a unique but
    /// position-stable egui id — a header-text id would collide when two
    /// tables share a header, and a whole-render counter would shift whenever
    /// virtualization culled an item above.
    static TABLE_SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static TABLE_ITEM: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Width the cached item heights were measured at; a change invalidates.
    static TRANSCRIPT_WIDTH: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// A stable id for the next table in the item being drawn: the item's index
/// paired with its own table counter. Derived from the index rather than a
/// running per-render counter so ids don't shift when virtualization culls
/// items above (which would reset each table's scroll state).
fn next_table_seq() -> u64 {
    let item = TABLE_ITEM.with(|c| c.get());
    let n = TABLE_SEQ.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    (item << 20) | n
}

/// Draw transcript item `i`, returning the vertical extent it consumed
/// including the gap that follows — the value the height cache stores.
fn draw_item(ui: &mut egui::Ui, p: &CPane, i: usize, inner: f32) -> f32 {
    use zodiac::client_core::ChatItem;
    TABLE_ITEM.with(|c| c.set(i as u64));
    TABLE_SEQ.with(|c| c.set(0));
    // Measure the cursor advance, not the drawn rect: egui inserts
    // `item_spacing` between widgets, so a rect-based height would under-count
    // and the spacers standing in for culled items would drift out of step.
    let before = ui.cursor().top();
    ui.scope(|ui| match &p.agent.items[i] {
        ChatItem::User(t) => turn_user(ui, t, inner),
        ChatItem::Assistant(t) => turn_assistant(ui, t, inner),
        ChatItem::Thinking(t) => turn_thinking(ui, p.id, i, t, inner),
        ChatItem::Tool(tc) => turn_tool_box(ui, p.id, i, tc, inner),
        ChatItem::Error(t) => turn_agent(ui, "✗", theme::STATUS_RAIL[0], t, inner),
    });
    ui.add_space(10.0);
    ui.cursor().top() - before
}

thread_local! {
    /// Laid-out text, reused across frames.
    ///
    /// Completed transcript turns are immutable, but the renderer used to
    /// re-parse their Markdown and rebuild a `LayoutJob` — fresh `String`s and
    /// `Vec`s — *every frame*, for *every* item. With agents streaming (which
    /// pins the repaint clock) that allocation churn was the single largest
    /// cost in the GUI: a profile of a streaming session put ~35-40% of samples
    /// in malloc/free/memmove. Keying the finished galley by content + layout
    /// geometry makes a redraw of unchanged text a hash lookup.
    static GALLEY_CACHE: std::cell::RefCell<
        std::collections::HashMap<u64, (std::sync::Arc<egui::Galley>, u64)>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());

    /// Frame counter for cache aging (bumped once per transcript render).
    static GALLEY_FRAME: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Entries untouched for this many transcript frames are dropped. Generous
/// enough that scrolling back and forth stays warm, small enough that a huge
/// scrollback doesn't pin every galley it ever laid out.
const GALLEY_TTL: u64 = 240;

/// Age the galley cache one frame and evict what hasn't been used recently.
fn galley_cache_tick() {
    let frame = GALLEY_FRAME.with(|c| {
        let v = c.get() + 1;
        c.set(v);
        v
    });
    if !frame.is_multiple_of(120) {
        return; // sweep occasionally, not every frame
    }
    GALLEY_CACHE.with(|c| {
        c.borrow_mut()
            .retain(|_, (_, last)| frame.saturating_sub(*last) < GALLEY_TTL)
    });
}

/// Lay `job` out once and reuse it while the same text is drawn at the same
/// geometry. `key` must capture everything the layout depends on (content,
/// size, color, wrap width, scale, pixels-per-point) — `galley_key` builds one.
fn cached_galley(
    ui: &mut egui::Ui,
    key: u64,
    job: impl FnOnce() -> egui::text::LayoutJob,
) -> std::sync::Arc<egui::Galley> {
    if perf_off() {
        return ui.fonts_mut(|f| f.layout_job(job()));
    }
    let frame = GALLEY_FRAME.with(|c| c.get());
    if let Some(hit) = GALLEY_CACHE.with(|c| {
        c.borrow_mut().get_mut(&key).map(|(g, last)| {
            *last = frame;
            g.clone()
        })
    }) {
        return hit;
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job()));
    GALLEY_CACHE.with(|c| {
        c.borrow_mut().insert(key, (galley.clone(), frame));
    });
    galley
}

/// Hash everything a laid-out run depends on into a cache key: the text, the
/// wrap width, the device scale, the transcript font scale, and a caller-
/// supplied `tag` covering style choices (font size, color, which renderer).
fn galley_key(ui: &egui::Ui, text: &str, width: f32, tag: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    tag.hash(&mut h);
    // Quantize the floats — exact bits would miss on sub-pixel jitter.
    ((width * 4.0) as i64).hash(&mut h);
    ((ui.ctx().pixels_per_point() * 100.0) as i64).hash(&mut h);
    ((TRANSCRIPT_SCALE.with(|c| c.get()) * 1000.0) as i64).hash(&mut h);
    h.finish()
}

/// Distinguishes monospace-box galleys from proportional ones with the same
/// text/size/color.
const MONO_TAG: u64 = 0x6d6f_6e6f_0000_0001;

/// A cached monospace label — the code/result boxes, whose text is immutable
/// once a tool call completes. `wrap` is the wrap width (`INFINITY` to let long
/// lines run under a horizontal scroll). `build` produces the display string
/// and only runs on a cache miss, so per-frame clipping/allocation disappears.
fn mono_label(
    ui: &mut egui::Ui,
    cache_on: &str,
    size: f32,
    color: Color32,
    wrap: f32,
    build: impl FnOnce() -> String,
) {
    let key = galley_key(ui, cache_on, wrap, style_tag(size, color) ^ MONO_TAG);
    let galley = cached_galley(ui, key, || {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap;
        job.append(
            &build(),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::monospace(size),
                color,
                ..Default::default()
            },
        );
        job
    });
    ui.add(egui::Label::new(galley));
}

/// Pack a font size + color into the `tag` half of a galley key.
fn style_tag(size: f32, color: Color32) -> u64 {
    let s = (size * 100.0) as u64 & 0xffff_ffff;
    let c = u32::from_be_bytes(color.to_array()) as u64;
    (s << 32) | c
}

/// `ZODIAC_GUI_PERF_OFF=1` restores the pre-optimization render path (no
/// virtualization, no galley cache, no frame clamp). Kept as an A/B switch so
/// the CPU-burn fix can be re-measured on one binary — see
/// zodiac-gui/HANDOFF.md.
pub fn perf_off() -> bool {
    use std::sync::OnceLock;
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var("ZODIAC_GUI_PERF_OFF").is_ok())
}

fn st_probe_transcript(ui: &egui::Ui) {
    PROBE.with(|c| {
        let mut p = c.get();
        p.transcript = Some(ui.max_rect());
        c.set(p);
    });
}

fn st_probe_bubble(rect: egui::Rect) {
    PROBE.with(|c| {
        let mut p = c.get();
        // Keep the widest one: that's the one most likely to overflow.
        if p.user_bubble.is_none_or(|r| rect.width() > r.width()) {
            p.user_bubble = Some(rect);
        }
        c.set(p);
    });
}

fn st_probe_slash(rows: usize) {
    PROBE.with(|c| {
        let mut p = c.get();
        p.slash_rows = rows;
        c.set(p);
    });
}

/// Take and reset the frame-local probe.
pub fn take_probe() -> Probe {
    PROBE.replace(Probe {
        transcript: None,
        user_bubble: None,
        slash_rows: 0,
    })
}

/// Scale a transcript font size by the current agent-pane factor.
fn tsize(base: f32) -> f32 {
    TRANSCRIPT_SCALE.with(|c| base * c.get())
}

/// Sets the transcript scale for its lifetime and restores 1.0 on drop (so a
/// panic mid-render can't leak the factor into later chrome).
struct TranscriptScaleGuard;
impl Drop for TranscriptScaleGuard {
    fn drop(&mut self) {
        TRANSCRIPT_SCALE.with(|c| c.set(1.0));
    }
}

/// Native agent transcript: typed turns (user/assistant/thinking/tool/error)
/// from [`zodiac::client_core::ChatItem`] plus the live streaming tail. This
/// is the rich view — expandable tool boxes, collapsible thinking prose,
/// fenced-code blocks, and the orange thinking sayings.
fn transcript_view(
    ui: &mut egui::Ui,
    p: &CPane,
    scroll_dy: f32,
    agent_scale: f32,
    heights: &mut Vec<f32>,
) {
    let _scale_guard = TranscriptScaleGuard;
    TRANSCRIPT_SCALE.with(|c| c.set(agent_scale));
    galley_cache_tick();
    // Item heights are measured at one width; a resize invalidates them all.
    let width = ui.available_width();
    let last_w = TRANSCRIPT_WIDTH.with(|c| c.replace(width));
    if (last_w - width).abs() > 0.5 {
        heights.clear();
    }
    heights.truncate(p.agent.items.len());
    egui::ScrollArea::vertical()
        .id_salt("transcript")
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            // PageUp/PageDown delta from focused(); a positive dy scrolls up
            // (toward earlier turns), disengaging stick-to-bottom until the
            // user pages back down to the tail.
            if scroll_dy != 0.0 {
                ui.scroll_with_delta(egui::vec2(0.0, scroll_dy));
            }
            // Pin the content to the visible width. `auto_shrink([false,false])`
            // leaves the content ui effectively unbounded horizontally, so
            // width-derived sizing (the user bubble's max width) computed off
            // a huge `available_width()`: the text never wrapped, the bubble
            // grew past the viewport, and because it lays out right-to-left the
            // overflow ran off the LEFT edge and under the sidebar.
            ui.set_max_width(viewport.width());
            st_probe_transcript(ui);
            ui.add_space(12.0);
            let inner = 22.0;
            // Own the gaps between items: with the container's automatic
            // spacing zeroed, an item's extent is exactly what `draw_item`
            // measures, so a spacer can stand in for it precisely.
            ui.spacing_mut().item_spacing.y = 0.0;
            // Virtualized draw. Laying out every turn every frame made cost
            // grow with transcript length (the handoff's dominant finding), so
            // items whose cached extent falls outside the viewport (plus an
            // overscan margin) collapse into a single spacer. Heights are
            // measured as items render; the tail past `heights.len()` is always
            // drawn, which is how newly-arrived items get measured.
            const OVERSCAN: f32 = 600.0;
            let (vis0, vis1) = (viewport.min.y - OVERSCAN, viewport.max.y + OVERSCAN);
            let mut y = 12.0;
            let mut skipped = 0.0;
            let known = if perf_off() {
                0
            } else {
                heights.len().min(p.agent.items.len())
            };
            for (i, slot) in heights.iter_mut().enumerate().take(known) {
                let h = *slot;
                if y + h >= vis0 && y <= vis1 {
                    if skipped > 0.0 {
                        ui.add_space(skipped);
                        skipped = 0.0;
                    }
                    let measured = draw_item(ui, p, i, inner);
                    // Advance by what was actually emitted, so a stale cached
                    // height self-corrects instead of skewing later items.
                    y += measured;
                    *slot = measured;
                } else {
                    skipped += h;
                    y += h;
                }
            }
            if skipped > 0.0 {
                ui.add_space(skipped);
            }
            for i in known..p.agent.items.len() {
                let measured = draw_item(ui, p, i, inner);
                heights.push(measured);
            }
            // Live tail: reasoning still streaming, then the thinking sayings,
            // then in-flight assistant text.
            if !p.agent.think_stream.is_empty() {
                turn_thinking_live(ui, &p.agent.think_stream, inner);
                ui.add_space(10.0);
            } else if p.agent.thinking {
                thinking_indicator(ui, inner);
                ui.add_space(10.0);
            }
            if !p.agent.stream.is_empty() {
                turn_assistant(ui, &p.agent.stream, inner);
            }
            ui.add_space(12.0);
        });
}

/// The live "· Cogitating…" saying in warm orange, with a braille spinner
/// that advances off egui's clock (repaint scheduled so it animates).
fn thinking_indicator(ui: &mut egui::Ui, inner: f32) {
    let t = ui.input(|i| i.time);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(80));
    let word = THINKING_WORDS[((t / 1.6) as usize) % THINKING_WORDS.len()];
    indent(ui, inner, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(spinner_frame(t))
                    .color(theme::ORANGE)
                    .size(tsize(14.0))
                    .monospace(),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("{word}…"))
                    .color(theme::ORANGE)
                    .italics()
                    .size(tsize(13.5)),
            );
        });
    });
}

/// A left-indented block.
fn indent(ui: &mut egui::Ui, x: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(x);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - x);
            add(ui);
        });
    });
}

/// User turn: a right-aligned selected-fill bubble.
fn turn_user(ui: &mut egui::Ui, text: &str, inner: f32) {
    // Measure the cap from the *vertical* container, before entering the
    // right-to-left layout: `right_to_left` is a horizontal layout, and egui
    // neither wraps text nor bounds `available_width` along a horizontal
    // layout's main axis. Sizing the bubble from inside it produced a single
    // unwrapped line wider than the pane, which — laying out right-to-left —
    // ran off the LEFT edge and under the sidebar.
    let room = (ui.available_width() - inner).max(120.0);
    let cap = (room * 0.64).clamp(120.0, room);
    let r = ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
        ui.add_space(inner);
        Frame::NONE
            .fill(theme::bg_selected())
            .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_max_width(cap);
                // `.wrap()` is required: a plain label inside a horizontal
                // layout would not wrap however narrow the cap is.
                ui.add(
                    Label::new(
                        RichText::new(text)
                            .color(theme::TEXT_BODY)
                            .size(tsize(14.0)),
                    )
                    .wrap(),
                );
            });
    });
    st_probe_bubble(r.response.rect);
}

/// Assistant/error turn: an avatar glyph + prose column.
fn turn_agent(ui: &mut egui::Ui, glyph: &str, col: Color32, text: &str, inner: f32) {
    ui.horizontal_top(|ui| {
        ui.add_space(inner - 18.0);
        ui.label(RichText::new(glyph).color(col).size(tsize(15.0)));
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            ui.label(
                RichText::new(text)
                    .color(theme::TEXT_BODY)
                    .size(tsize(14.5)),
            );
        });
    });
}

/// Assistant turn: Claude Code's warm `⏺` recap bullet + prose, with any
/// fenced code broken out into its own monospace box.
fn turn_assistant(ui: &mut egui::Ui, text: &str, inner: f32) {
    ui.horizontal_top(|ui| {
        ui.add_space(inner - 18.0);
        ui.label(RichText::new("⏺").color(theme::ORANGE).size(tsize(14.0)));
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            render_body(ui, text);
        });
    });
}

/// Render assistant prose as Markdown: headings, bullet/ordered lists, block
/// quotes, rules, and inline **bold** / *italic* / `code` / [links]; ```
/// fenced blocks break out into their own monospace box.
fn render_body(ui: &mut egui::Ui, text: &str) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Fenced code block: gather to the closing fence.
        if line.trim_start().starts_with("```") {
            let mut buf: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                buf.push(lines[i]);
                i += 1;
            }
            i += 1; // consume the closing fence (or run off the end)
            let code = buf.join("\n");
            if !code.trim().is_empty() {
                code_box(ui, &code);
            }
            continue;
        }
        // GitHub-flavored table: a row followed by a `|---|---|` separator.
        // The separator gate keeps ordinary prose containing a '|' from being
        // mistaken for a table.
        if is_table_row(line) && lines.get(i + 1).is_some_and(|n| is_table_separator(n)) {
            let start = i;
            i += 2; // header + separator
            while i < lines.len() && is_table_row(lines[i]) {
                i += 1;
            }
            md_table(ui, &lines[start..i]);
            continue;
        }
        md_block(ui, line);
        i += 1;
    }
}

/// One Markdown block line (heading, list item, quote, rule, or paragraph).
fn md_block(ui: &mut egui::Ui, line: &str) {
    let t = line.trim_start();
    if t.is_empty() {
        ui.add_space(4.0);
        return;
    }
    if t == "---" || t == "***" || t == "___" {
        ui.add_space(2.0);
        ui.separator();
        return;
    }
    // Headings, longest marker first so `####` isn't caught by `###`.
    for (marker, size) in [
        ("###### ", 13.0),
        ("##### ", 13.5),
        ("#### ", 14.0),
        ("### ", 14.5),
        ("## ", 16.5),
        ("# ", 19.0),
    ] {
        if let Some(h) = t.strip_prefix(marker) {
            md_heading(ui, h, size);
            return;
        }
    }
    if let Some(q) = t.strip_prefix("> ") {
        ui.horizontal_top(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), Sense::hover());
            ui.painter()
                .rect_filled(r, CornerRadius::same(1), theme::LINE_BORDER_STRONG);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width());
                md_line(ui, q, 14.5, theme::TEXT_DIM);
            });
        });
        return;
    }
    // Nesting: sub-lists are indented in the source; carry that inset through.
    let indent = list_indent_px(line);
    for pre in ["- ", "* ", "+ "] {
        if let Some(item) = t.strip_prefix(pre) {
            // GFM task list: `- [ ] todo` / `- [x] done`. Render a checkbox
            // glyph instead of the bullet; done items get a dim/struck feel.
            if let Some(rest) = item.strip_prefix("[ ] ") {
                md_task_row(ui, false, rest, indent);
                return;
            }
            if item
                .get(..4)
                .is_some_and(|p| p.eq_ignore_ascii_case("[x] "))
            {
                md_task_row(ui, true, &item[4..], indent);
                return;
            }
            md_bullet_row(ui, "•".to_string(), item, indent);
            return;
        }
    }
    if let Some((num, rest)) = md_split_ordered(t) {
        md_bullet_row(ui, format!("{num}."), rest, indent);
        return;
    }
    md_line(ui, t.trim_end(), 14.5, theme::TEXT_BODY);
}

/// A line that could be a GFM table row: non-blank and containing a pipe.
/// (The table only actually renders when the next line is a separator.)
fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.contains('|')
}

/// The `|---|:--:|--:|` divider under a table header: pipes, dashes, colons and
/// spaces only, with at least one dash and one pipe.
fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty()
        && t.contains('|')
        && t.contains('-')
        && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

/// Split a table row into trimmed cells, dropping the optional leading/trailing
/// pipes so both `| a | b |` and `a | b` yield the same cells.
fn table_cells(row: &str) -> Vec<String> {
    let t = row.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// Per-column alignment parsed from the separator row.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ColAlign {
    Left,
    Center,
    Right,
}

fn parse_aligns(sep: &str) -> Vec<ColAlign> {
    table_cells(sep)
        .iter()
        .map(|c| {
            let left = c.starts_with(':');
            let right = c.ends_with(':');
            match (left, right) {
                (true, true) => ColAlign::Center,
                (false, true) => ColAlign::Right,
                _ => ColAlign::Left,
            }
        })
        .collect()
}

/// Render a GFM pipe table (`rows[0]` header, `rows[1]` separator, rest body)
/// as an aligned, striped grid. Wrapped in a horizontal scroll so a very wide
/// table scrolls instead of stretching the pane off-screen; each cell is width
/// -capped and wraps, and supports inline markdown.
fn md_table(ui: &mut egui::Ui, rows: &[&str]) {
    if rows.len() < 2 {
        return;
    }
    let header = table_cells(rows[0]);
    let aligns = parse_aligns(rows[1]);
    let body: Vec<Vec<String>> = rows[2..].iter().map(|r| table_cells(r)).collect();
    let ncols = header
        .len()
        .max(body.iter().map(|r| r.len()).max().unwrap_or(0));
    let align = |c: usize| aligns.get(c).copied().unwrap_or(ColAlign::Left);
    let cap = (ui.available_width() / ncols as f32 - 24.0).clamp(60.0, 340.0);
    let seq = next_table_seq();
    Frame::NONE
        .stroke(Stroke::new(1.0, theme::LINE_BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(4, 2))
        .outer_margin(Margin::symmetric(0, 4))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(("md_table", seq))
                .show(ui, |ui| {
                    egui::Grid::new(("md_table_grid", seq))
                        .striped(true)
                        .spacing(egui::vec2(14.0, 6.0))
                        .show(ui, |ui| {
                            for c in 0..ncols {
                                let txt = header.get(c).map(String::as_str).unwrap_or("");
                                md_cell(ui, txt, align(c), true, cap);
                            }
                            ui.end_row();
                            for row in &body {
                                for c in 0..ncols {
                                    let txt = row.get(c).map(String::as_str).unwrap_or("");
                                    md_cell(ui, txt, align(c), false, cap);
                                }
                                ui.end_row();
                            }
                        });
                });
        });
}

/// One table cell: inline-formatted, width-capped/wrapped, aligned per column.
fn md_cell(ui: &mut egui::Ui, text: &str, align: ColAlign, header: bool, cap: f32) {
    let base = if header {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_BODY
    };
    let spans = parse_inline(text);
    let layout = match align {
        ColAlign::Left => Layout::left_to_right(Align::Center),
        ColAlign::Center => Layout::top_down(Align::Center),
        ColAlign::Right => Layout::right_to_left(Align::Center),
    };
    ui.with_layout(layout, |ui| {
        ui.set_min_width(cap.min(ui.available_width()));
        if spans.iter().all(|sp| sp.url.is_none()) {
            // Fast path: a single non-interactive label, so column alignment
            // reads cleanly. `span_format` applies the transcript scale itself,
            // so pass the base size unscaled.
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = cap;
            for sp in &spans {
                let mut fmt = span_format(sp, 13.5, base);
                if header {
                    fmt.color = theme::TEXT_PRIMARY;
                }
                job.append(&sp.text, 0.0, fmt);
            }
            ui.label(job);
        } else {
            // A cell with a link renders its runs individually so the link is
            // clickable; the outer layout still applies the column alignment.
            render_inline_spans(ui, &spans, 13.5, base);
        }
    });
}

fn md_heading(ui: &mut egui::Ui, text: &str, size: f32) {
    ui.add_space(3.0);
    ui.label(
        RichText::new(text)
            .color(theme::TEXT_PRIMARY)
            .size(tsize(size))
            .strong(),
    );
    ui.add_space(1.0);
}

/// A list row: an accent marker + the item's inline-formatted text.
/// Leading-whitespace indent of a list item, mapped to a pixel inset for
/// nesting (2 spaces or 1 tab per level, capped so deep nesting stays sane).
fn list_indent_px(line: &str) -> f32 {
    let mut cols = 0usize;
    for c in line.chars() {
        match c {
            ' ' => cols += 1,
            '\t' => cols += 4,
            _ => break,
        }
    }
    ((cols / 2).min(6) as f32) * 16.0
}

/// A GFM task-list item: a checkbox glyph (checked = accent, unchecked = dim)
/// followed by the item text (dimmed when done).
fn md_task_row(ui: &mut egui::Ui, done: bool, item: &str, indent: f32) {
    ui.horizontal_top(|ui| {
        ui.add_space(6.0 + indent);
        let (glyph, col) = if done {
            ("\u{2611}", theme::accent()) // ballot box with check
        } else {
            ("\u{2610}", theme::TEXT_DIM) // empty ballot box
        };
        ui.label(RichText::new(glyph).color(col).size(tsize(14.0)));
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            let color = if done {
                theme::TEXT_FAINT
            } else {
                theme::TEXT_BODY
            };
            md_line(ui, item, 14.5, color);
        });
    });
}

fn md_bullet_row(ui: &mut egui::Ui, marker: String, item: &str, indent: f32) {
    ui.horizontal_top(|ui| {
        ui.add_space(6.0 + indent);
        ui.label(
            RichText::new(marker)
                .color(theme::accent())
                .size(tsize(14.0))
                .monospace(),
        );
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            md_line(ui, item, 14.5, theme::TEXT_BODY);
        });
    });
}

/// `N. rest` → (N, rest) for an ordered-list item.
fn md_split_ordered(t: &str) -> Option<(String, &str)> {
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    // GFM allows either `1. ` or `1) ` as the delimiter.
    let after = &t[digits.len()..];
    let rest = after
        .strip_prefix(". ")
        .or_else(|| after.strip_prefix(") "))?;
    Some((digits, rest))
}

/// A run of inline Markdown as a single wrapped, mixed-format label.
fn md_line(ui: &mut egui::Ui, line: &str, size: f32, base: Color32) {
    let spans = parse_inline(line);
    if spans.is_empty() {
        return;
    }
    // Fast path: a line with no links lays out once and is cached — the same
    // text at the same width/scale redraws without re-parsing or reallocating.
    if spans.iter().all(|sp| sp.url.is_none()) {
        let width = ui.available_width();
        let key = galley_key(ui, line, width, style_tag(size, base));
        let galley = cached_galley(ui, key, || {
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = width;
            for sp in &spans {
                job.append(&sp.text, 0.0, span_format(sp, size, base));
            }
            job
        });
        ui.add(egui::Label::new(galley));
        return;
    }
    // Link path: lay the runs out with wrapping so link runs can be clickable.
    render_inline_spans(ui, &spans, size, base);
}

/// Render inline spans left-to-right with wrapping, drawing link runs as
/// individual clickable widgets — a click opens the URL in the OS default
/// browser (via egui's open_url command, handled in app.rs' output loop).
/// Shared by [`md_line`] and table cells. `size` is unscaled; `span_format`
/// and the link font both apply the transcript scale.
fn render_inline_spans(ui: &mut egui::Ui, spans: &[MdSpan], size: f32, base: Color32) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for sp in spans {
            if let Some(url) = &sp.url {
                let text = RichText::new(&sp.text)
                    .color(theme::accent())
                    .underline()
                    .font(egui::FontId::proportional(tsize(size)));
                let resp = ui
                    .add(egui::Label::new(text).sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            } else {
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = ui.available_width();
                job.append(&sp.text, 0.0, span_format(sp, size, base));
                ui.label(job);
            }
        }
    });
}

/// The egui `TextFormat` for a non-link inline run (code / bold / italic).
fn span_format(sp: &MdSpan, size: f32, base: Color32) -> egui::TextFormat {
    let color = if sp.code {
        theme::AMBER
    } else if sp.bold {
        theme::TEXT_PRIMARY
    } else {
        base
    };
    let font = if sp.code {
        egui::FontId::monospace(tsize(size - 1.0))
    } else {
        egui::FontId::proportional(tsize(size))
    };
    let mut fmt = egui::TextFormat {
        font_id: font,
        color,
        italics: sp.italic,
        ..Default::default()
    };
    if sp.code {
        fmt.background = theme::bg_raised();
    }
    if sp.strike {
        fmt.strikethrough = Stroke::new(1.0, color);
    }
    fmt
}

/// One inline-formatted run of Markdown text.
struct MdSpan {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    /// Strikethrough run (`~~text~~`).
    strike: bool,
    /// `Some(url)` marks a clickable link (opens in the OS default browser);
    /// the run is also styled with the accent color + underline.
    url: Option<String>,
}

/// Split a line into inline runs: `` `code` ``, **bold**, *italic*/_italic_,
/// and `[text](url)` links (the URL is dropped; the text is styled).
fn parse_inline(s: &str) -> Vec<MdSpan> {
    fn flush(cur: &mut String, spans: &mut Vec<MdSpan>, bold: bool, italic: bool, strike: bool) {
        if !cur.is_empty() {
            spans.push(MdSpan {
                text: std::mem::take(cur),
                bold,
                italic,
                strike,
                code: false,
                url: None,
            });
        }
    }
    let chars: Vec<char> = s.chars().collect();
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            flush(&mut cur, &mut spans, bold, italic, strike);
            let mut j = i + 1;
            let mut code = String::new();
            while j < chars.len() && chars[j] != '`' {
                code.push(chars[j]);
                j += 1;
            }
            if j < chars.len() {
                spans.push(MdSpan {
                    text: code,
                    bold,
                    italic,
                    strike,
                    code: true,
                    url: None,
                });
                i = j + 1;
                continue;
            }
            cur.push('`');
            i += 1;
            continue;
        }
        if c == '[' {
            if let Some((text, url, adv)) = parse_link(&chars, i) {
                flush(&mut cur, &mut spans, bold, italic, strike);
                spans.push(MdSpan {
                    text,
                    bold,
                    italic,
                    strike,
                    code: false,
                    url: Some(url),
                });
                i += adv;
                continue;
            }
        }
        // Bare URL (http:// or https://) not wrapped in Markdown link syntax —
        // agents often print raw links. Consume up to whitespace, trimming
        // trailing punctuation so "see https://x.com." doesn't eat the period.
        if (c == 'h' || c == 'H') && starts_with_url(&chars, i) {
            if let Some((url, adv)) = parse_bare_url(&chars, i) {
                flush(&mut cur, &mut spans, bold, italic, strike);
                spans.push(MdSpan {
                    text: url.clone(),
                    bold,
                    italic,
                    strike,
                    code: false,
                    url: Some(url),
                });
                i += adv;
                continue;
            }
        }
        // Bare `www.` host with no scheme — link it with an https:// href.
        if (c == 'w' || c == 'W') && starts_with_www(&chars, i) {
            if let Some((text, href, adv)) = parse_www_url(&chars, i) {
                flush(&mut cur, &mut spans, bold, italic, strike);
                spans.push(MdSpan {
                    text,
                    bold,
                    italic,
                    strike,
                    code: false,
                    url: Some(href),
                });
                i += adv;
                continue;
            }
        }
        if c == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            flush(&mut cur, &mut spans, bold, italic, strike);
            strike = !strike;
            i += 2;
            continue;
        }
        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            flush(&mut cur, &mut spans, bold, italic, strike);
            bold = !bold;
            i += 2;
            continue;
        }
        if c == '*' || c == '_' {
            // Leave `_` inside a word alone (snake_case, file_names).
            let word_inner = c == '_'
                && i > 0
                && chars[i - 1].is_alphanumeric()
                && i + 1 < chars.len()
                && chars[i + 1].is_alphanumeric();
            if word_inner {
                cur.push(c);
                i += 1;
                continue;
            }
            flush(&mut cur, &mut spans, bold, italic, strike);
            italic = !italic;
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    flush(&mut cur, &mut spans, bold, italic, strike);
    spans
}

/// Parse `[text](url)` at `open` ('['); returns (text, url, chars_consumed).
fn parse_link(chars: &[char], open: usize) -> Option<(String, String, usize)> {
    let close = chars[open + 1..].iter().position(|&c| c == ']')? + open + 1;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    // Find the closing `)`, allowing balanced parens inside the URL so links
    // like `.../Foo_(bar)` aren't truncated at the inner `)`.
    let mut depth = 1usize;
    let mut end = None;
    for (k, &c) in chars[close + 2..].iter().enumerate() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(close + 2 + k);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let text: String = chars[open + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    Some((text, url, end - open + 1))
}

/// Does the char slice at `i` begin with an `http://` / `https://` scheme?
fn starts_with_url(chars: &[char], i: usize) -> bool {
    let rest: String = chars[i..].iter().take(8).collect();
    let low = rest.to_ascii_lowercase();
    low.starts_with("http://") || low.starts_with("https://")
}

/// Does the slice at `i` start a bare `www.` host?
fn starts_with_www(chars: &[char], i: usize) -> bool {
    let rest: String = chars[i..].iter().take(4).collect();
    rest.eq_ignore_ascii_case("www.")
}

/// Consume a URL-ish token from `i` to the next whitespace, trimming trailing
/// sentence punctuation / a closing bracket. Returns (token, chars_consumed).
fn consume_url_token(chars: &[char], i: usize) -> (String, usize) {
    let mut j = i;
    while j < chars.len() && !chars[j].is_whitespace() {
        j += 1;
    }
    let mut tok: String = chars[i..j].iter().collect();
    let n = tok.len();
    let trimmed = tok.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
    let consumed = j - i - (n - trimmed.len());
    tok.truncate(trimmed.len());
    (tok, consumed)
}

/// Parse a bare URL at `i`; returns (url, chars_consumed). Runs to the first
/// whitespace, then trims trailing sentence punctuation and a single closing
/// bracket so surrounding prose isn't swallowed.
fn parse_bare_url(chars: &[char], i: usize) -> Option<(String, usize)> {
    let (url, consumed) = consume_url_token(chars, i);
    if url.len() <= "https://".len() {
        return None;
    }
    Some((url, consumed))
}

/// Parse a bare `www.host/…` at `i`; returns (display, href, chars_consumed).
/// The href gets an `https://` scheme so it opens; the display keeps `www.`.
fn parse_www_url(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let (text, consumed) = consume_url_token(chars, i);
    // Require a dot after the `www.` and something after it (a real host).
    if text.len() < 6 || !text[4..].contains('.') {
        return None;
    }
    let href = format!("https://{text}");
    Some((text, href, consumed))
}

/// A monospace code box: bordered, horizontally scrollable so long lines
/// don't wrap.
fn code_box(ui: &mut egui::Ui, code: &str) {
    let r = Frame::NONE
        .fill(theme::bg_raised())
        .stroke(Stroke::new(1.0, theme::LINE_BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .outer_margin(Margin::symmetric(0, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::horizontal()
                .id_salt(egui::Id::new(("code", code.len(), code.as_ptr() as usize)))
                .show(ui, |ui| {
                    // No wrap: long lines run under the horizontal scroll.
                    mono_label(
                        ui,
                        code,
                        tsize(12.5),
                        theme::TEXT_BODY,
                        f32::INFINITY,
                        || code.to_string(),
                    );
                });
        });
    copy_overlay(ui, r.response.rect, code);
}

/// A small "copy" button shown in the top-right of `rect` while it's hovered;
/// clicking copies `text` to the system clipboard (arboard-backed path).
fn copy_overlay(ui: &mut egui::Ui, rect: egui::Rect, text: &str) {
    if !ui.rect_contains_pointer(rect) {
        return;
    }
    let size = egui::vec2(44.0, 19.0);
    let at = egui::Rect::from_min_size(
        egui::pos2(rect.max.x - size.x - 6.0, rect.min.y + 5.0),
        size,
    );
    let btn = egui::Button::new(RichText::new("copy").size(10.5).color(theme::TEXT_DIM))
        .fill(theme::bg_chrome())
        .stroke(Stroke::new(1.0, theme::LINE_BORDER))
        .small();
    if ui
        .put(at, btn)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        ui.ctx().copy_text(text.to_string());
    }
}

/// A tool result box: capped height with a scroll, red-tinted on error.
fn result_box(ui: &mut egui::Ui, text: &str, is_error: bool) {
    let col = if is_error {
        theme::STATUS_TEXT[0]
    } else {
        theme::TEXT_DIM
    };
    let r = Frame::NONE
        .fill(theme::bg_panel())
        .stroke(Stroke::new(1.0, theme::LINE_HAIRLINE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt(egui::Id::new(("res", text.len(), text.as_ptr() as usize)))
                .max_height(240.0)
                .show(ui, |ui| {
                    // clip_lines() only runs on a cache miss — it used to
                    // rebuild a (potentially huge) String every frame.
                    let w = ui.available_width();
                    mono_label(ui, text, tsize(12.0), col, w, || clip_lines(text, 400));
                });
        });
    // Copy the full (unclipped) result.
    copy_overlay(ui, r.response.rect, text);
}

/// A finished reasoning block: a collapsible orange panel, closed by default.
fn turn_thinking(ui: &mut egui::Ui, pane: u64, idx: usize, text: &str, inner: f32) {
    indent(ui, inner, |ui| {
        let id = ui.make_persistent_id(("think", pane, idx));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| {
                ui.label(
                    RichText::new("✳ thinking")
                        .color(theme::ORANGE)
                        .size(tsize(12.5)),
                );
            })
            .body(|ui| {
                ui.label(
                    RichText::new(text)
                        .color(theme::ORANGE_DIM)
                        .italics()
                        .size(tsize(12.5)),
                );
            });
    });
}

/// Reasoning still streaming in: orange prose under a live spinner header.
fn turn_thinking_live(ui: &mut egui::Ui, text: &str, inner: f32) {
    let t = ui.input(|i| i.time);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(80));
    indent(ui, inner, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(spinner_frame(t))
                    .color(theme::ORANGE)
                    .size(tsize(13.0))
                    .monospace(),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("thinking")
                    .color(theme::ORANGE)
                    .size(tsize(12.5)),
            );
        });
        ui.label(
            RichText::new(text)
                .color(theme::ORANGE_DIM)
                .italics()
                .size(tsize(12.5)),
        );
    });
}

/// A tool call as an expandable box: the header shows the tool + a one-line
/// command teaser; expanding reveals the full command and its output
/// (red-tinted on error). Collapsed by default.
fn turn_tool_box(
    ui: &mut egui::Ui,
    pane: u64,
    idx: usize,
    tc: &zodiac::client_core::ToolCall,
    inner: f32,
) {
    indent(ui, inner, |ui| {
        let edge = if tc.is_error {
            fade(theme::STATUS_RAIL[0], 0.5)
        } else {
            theme::LINE_BORDER
        };
        Frame::NONE
            .fill(theme::bg_chrome())
            .stroke(Stroke::new(1.0, edge))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let id = ui.make_persistent_id(("tool", pane, idx));
                let accent = if tc.is_error {
                    theme::STATUS_RAIL[0]
                } else {
                    theme::accent()
                };
                // Errors start expanded so a failure is visible without a
                // click; successful calls stay collapsed to keep things tidy.
                egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    tc.is_error,
                )
                .show_header(ui, |ui| {
                    ui.label(RichText::new("⏺").color(accent).size(tsize(12.0)));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(&tc.name)
                            .color(theme::TEXT_DIM)
                            .size(tsize(12.5))
                            .strong()
                            .monospace(),
                    );
                    ui.add_space(8.0);
                    let teaser = clip(tc.command.lines().next().unwrap_or(""), 72);
                    ui.add(
                        Label::new(
                            RichText::new(teaser)
                                .color(theme::TEXT_FAINT)
                                .size(tsize(12.0))
                                .monospace(),
                        )
                        .truncate(),
                    );
                })
                .body(|ui| {
                    ui.add_space(4.0);
                    if !tc.command.trim().is_empty() {
                        code_box(ui, tc.command.trim_matches('\n'));
                    }
                    if let Some(res) = &tc.result {
                        if !res.trim().is_empty() {
                            ui.add_space(6.0);
                            result_box(ui, res.trim_matches('\n'), tc.is_error);
                        }
                    }
                });
            });
    });
}

/// `/resume`: pick an earlier Claude Code session for this directory. The CLI
/// can't run its own picker over a pipe ("/resume isn't available in this
/// environment"), so zodiac lists the sessions and resumes the chosen one by
/// spawning a pane with `--resume <id>`.
fn resume_dialog(ui: &mut egui::Ui, st: &mut UiState, actions: &mut Vec<UiAction>) {
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 150),
    );
    let n = st.resume_list.len();
    let mut chosen: Option<usize> = None;
    ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            st.overlay = Overlay::None;
        }
        if n > 0 {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                st.resume_sel = (st.resume_sel + 1) % n;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                st.resume_sel = (st.resume_sel + n - 1) % n;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                chosen = Some(st.resume_sel);
            }
        }
    });
    if st.overlay != Overlay::Resume {
        return;
    }
    let sel = st.resume_sel.min(n.saturating_sub(1));
    egui::Area::new(egui::Id::new("resume"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(theme::bg_card())
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(720.0);
                    ui.label(
                        RichText::new("Resume a session")
                            .color(theme::TEXT_PRIMARY)
                            .size(17.0)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Continues the chosen conversation in this pane.")
                            .color(theme::TEXT_DIM)
                            .size(12.0),
                    );
                    ui.add_space(12.0);
                    if n == 0 {
                        ui.label(
                            RichText::new("No past sessions found for this directory.")
                                .color(theme::TEXT_FAINT)
                                .size(13.0),
                        );
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("resume_rows")
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (i, s) in st.resume_list.iter().enumerate() {
                                let on = i == sel;
                                let r = Frame::NONE
                                    .fill(if on {
                                        theme::bg_selected()
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .corner_radius(CornerRadius::same(8))
                                    .inner_margin(Margin::symmetric(10, 6))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.add(
                                                Label::new(
                                                    RichText::new(&s.summary)
                                                        .color(if on {
                                                            theme::TEXT_PRIMARY
                                                        } else {
                                                            theme::TEXT_BODY
                                                        })
                                                        .size(13.0),
                                                )
                                                .truncate(),
                                            );
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(&s.age)
                                                            .color(theme::TEXT_GHOST)
                                                            .size(11.0),
                                                    );
                                                },
                                            );
                                        });
                                    });
                                if r.response.interact(Sense::click()).clicked() {
                                    chosen = Some(i);
                                }
                            }
                        });
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("↑↓ select · enter resume · esc cancel")
                            .color(theme::TEXT_GHOST)
                            .size(11.0),
                    );
                });
        });
    if let Some(i) = chosen {
        if let (Some(s), Some(pane)) = (st.resume_list.get(i), st.resume_pane) {
            actions.push(UiAction::ResumeHere(pane, s.id.clone()));
        }
        st.overlay = Overlay::None;
    }
}

/// The slash-command menu drawn above the composer. Returns the index of a
/// row the user clicked, if any.
fn slash_menu(
    ui: &mut egui::Ui,
    matches: &[crate::slash::SlashCmd],
    sel: usize,
    anchor: egui::Rect,
) -> Option<usize> {
    const MAX_ROWS: usize = 8;
    let mut clicked = None;
    egui::Area::new(egui::Id::new("slash_menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(anchor.left() + 18.0, anchor.top() - 6.0))
        .pivot(egui::Align2::LEFT_BOTTOM)
        .constrain(true)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(anchor.width() - 36.0);
            slash_menu_body(ui, matches, sel, &mut clicked);
        });
    clicked
}

fn slash_menu_body(
    ui: &mut egui::Ui,
    matches: &[crate::slash::SlashCmd],
    sel: usize,
    clicked: &mut Option<usize>,
) {
    const MAX_ROWS: usize = 8;
    Frame::NONE
        .fill(theme::bg_card())
        .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 6))
        .outer_margin(Margin::symmetric(18, 0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Keep the selected row on screen when the list is long.
            let first = sel.saturating_sub(MAX_ROWS - 1).min(matches.len());
            let shown = matches.iter().enumerate().skip(first).take(MAX_ROWS);
            for (i, c) in shown {
                let on = i == sel;
                let r = Frame::NONE
                    .fill(if on {
                        theme::bg_selected()
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("/{}", c.name))
                                    .color(if on {
                                        theme::accent()
                                    } else {
                                        theme::TEXT_BODY
                                    })
                                    .size(13.0)
                                    .monospace()
                                    .strong(),
                            );
                            if !c.desc.is_empty() {
                                ui.add_space(8.0);
                                ui.add(
                                    Label::new(
                                        RichText::new(&c.desc).color(theme::TEXT_FAINT).size(11.5),
                                    )
                                    .truncate(),
                                );
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(c.origin).color(theme::TEXT_GHOST).size(10.5),
                                );
                            });
                        });
                    });
                if r.response.interact(Sense::click()).clicked() {
                    *clicked = Some(i);
                }
            }
            if matches.len() > MAX_ROWS {
                ui.label(
                    RichText::new(format!("+{} more — keep typing", matches.len() - MAX_ROWS))
                        .color(theme::TEXT_GHOST)
                        .size(10.5),
                );
            }
            ui.label(
                RichText::new("↑↓ select · enter complete")
                    .color(theme::TEXT_GHOST)
                    .size(10.5),
            );
        });
}

/// The composer: a live one-line prompt editor. Enter (or Send) submits to
/// the agent as T_AGENT_INPUT and clears the buffer.
fn composer_bar(
    ui: &mut egui::Ui,
    p: &CPane,
    composer: &mut String,
    slash_sel: &mut usize,
    actions: &mut Vec<UiAction>,
    cmds: &[crate::slash::SlashCmd],
) {
    Frame::NONE
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut submit = false;
            // Slash-command picker. Shown while the composer holds a bare
            // `/token`, above the prompt (this panel lays out top-down, so
            // drawing it first puts it above the field). Arrow keys move the
            // selection and Enter accepts — all consumed before the TextEdit
            // sees them, so Enter completes instead of sending.
            let edit_id = egui::Id::new(("composer", p.id));
            let composer_focused = ui.memory(|m| m.has_focus(edit_id));
            let matches = crate::slash::active_query(composer)
                .map(|q| crate::slash::matching(cmds, q))
                .unwrap_or_default();
            let picking = composer_focused && !matches.is_empty();
            st_probe_slash(if picking { matches.len() } else { 0 });
            if !picking {
                *slash_sel = 0;
            } else {
                *slash_sel = (*slash_sel).min(matches.len() - 1);
                let n = matches.len();
                let mut accept = false;
                ui.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        *slash_sel = (*slash_sel + 1) % n;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        *slash_sel = (*slash_sel + n - 1) % n;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                    {
                        accept = true;
                    }
                });
                if accept {
                    // Complete the token (with a trailing space, which also
                    // dismisses the picker); Enter again sends it.
                    *composer = format!("/{} ", matches[*slash_sel].name);
                    *slash_sel = 0;
                }
            }
            Frame::NONE
                .fill(theme::bg_raised())
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(11))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        // Enter sends; Shift+Enter inserts a newline. Consume a
                        // plain Enter *before* the field sees it (checking last
                        // frame's focus) so it submits instead of typing a
                        // newline — Shift+Enter passes through to wrap the line.
                        // (If the slash picker took the Enter above, this sees
                        // nothing and the keystroke completes rather than sends.)
                        let send_enter = composer_focused
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                            });
                        // Multiline + wrap: a long message flows onto new lines
                        // and the box grows downward, so it can never stretch the
                        // pane horizontally off-screen. Capped at ~7 lines with
                        // an inner scroll so a long paste can't swallow the
                        // transcript either.
                        let resp = egui::ScrollArea::vertical()
                            .id_salt(("composer_scroll", p.id))
                            .max_height(132.0)
                            // Shrink to the text's height. Without this the
                            // scroll area expands to the panel's available
                            // height, and since the panel sizes itself from its
                            // content that feeds back into a composer several
                            // hundred pixels tall covering the transcript.
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let edit = egui::TextEdit::multiline(composer)
                                    .id(edit_id)
                                    .frame(Frame::NONE)
                                    .desired_width(ui.available_width() - 64.0)
                                    .desired_rows(1)
                                    .hint_text("message the agent…")
                                    .font(egui::FontId::proportional(14.0))
                                    .text_color(theme::TEXT_BODY);
                                ui.add(edit)
                            })
                            .inner;
                        // A bare "/" (when you're not already typing here) jumps
                        // to the composer so you can start typing. It only moves
                        // focus — the "/" isn't inserted: focus takes effect next
                        // frame, by which point this frame's "/" text event is
                        // gone, so the field never types it.
                        if !resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Slash)) {
                            resp.request_focus();
                        }
                        // Esc while composing blurs the field, so the next Esc
                        // (now nothing focused) returns to the Observatory —
                        // otherwise Esc did nothing while the composer held
                        // focus (the Back handler is gated on no focus).
                        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            resp.surrender_focus();
                        }
                        if send_enter {
                            submit = true;
                            resp.request_focus();
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if amber_button(ui, "Send").clicked() {
                                submit = true;
                            }
                        });
                    });
                });
            // Draw the picker as a floating overlay anchored just above the
            // composer. Rendering it inline made it part of this panel, and the
            // panel sizes from its content, which inflated the composer.
            if picking {
                let anchor = ui.min_rect();
                if let Some(hit) = slash_menu(ui, &matches, *slash_sel, anchor) {
                    *composer = format!("/{} ", matches[hit].name);
                    *slash_sel = 0;
                }
            }
            if submit && !composer.trim().is_empty() {
                // `/resume` can't run in a piped session, so zodiac answers it
                // itself with a session picker instead of forwarding it.
                let cmd = composer.trim().strip_prefix('/').unwrap_or("");
                if crate::slash::is_zodiac_handled(cmd) {
                    composer.clear();
                    actions.push(UiAction::OpenResume(p.id));
                } else {
                    actions.push(UiAction::SendAgent(p.id));
                }
            }
        });
}

/// A slim bar shown in place of the composer while a decision is pending —
/// the actual choices live in the modal `perm_popup`.
fn perm_hint(ui: &mut egui::Ui) {
    Frame::NONE
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(theme::STATUS_RAIL[0]).size(11.0));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("waiting for your decision…")
                        .color(theme::TEXT_DIM)
                        .size(13.0),
                );
            });
        });
}

/// The pending permission as a centered modal question: the tool + its input,
/// then the choices. Navigable by mouse, number keys, ↑/↓ (or j/k), and
/// Enter; Esc denies. Answering writes a T_PERM_RESP exactly as before.
fn perm_popup(ui: &mut egui::Ui, p: &CPane, st: &mut UiState, actions: &mut Vec<UiAction>) {
    let Some(req) = p.agent.perms.first() else {
        return;
    };
    // (label, allow?) — the two outcomes the protocol supports.
    let opts: [(&str, bool); 2] = [("Allow", true), ("Deny", false)];
    let n = opts.len();
    if st.perm_sel >= n {
        st.perm_sel = 0;
    }
    // Keyboard navigation.
    let mut confirm: Option<usize> = None;
    ui.input(|i| {
        if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::J) {
            st.perm_sel = (st.perm_sel + 1).min(n - 1);
        }
        if i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::K) {
            st.perm_sel = st.perm_sel.saturating_sub(1);
        }
        if i.key_pressed(egui::Key::Num1) {
            confirm = Some(0);
        }
        if i.key_pressed(egui::Key::Num2) {
            confirm = Some(1);
        }
        if i.key_pressed(egui::Key::Enter) {
            confirm = Some(st.perm_sel);
        }
        if i.key_pressed(egui::Key::Escape) {
            confirm = Some(1); // deny
        }
    });
    // Dim the screen behind the modal.
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 150),
    );
    let tool = req
        .display_name
        .clone()
        .unwrap_or_else(|| req.tool_name.clone());
    let cmd = zodiac::client_core::tool_command(&req.input);
    egui::Area::new(egui::Id::new("perm_popup"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(theme::bg_card())
                .stroke(Stroke::new(1.0, fade(theme::STATUS_RAIL[0], 0.5)))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(520.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⏺").color(theme::STATUS_RAIL[0]).size(15.0));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Permission needed")
                                .color(theme::TEXT_PRIMARY)
                                .size(17.0)
                                .strong(),
                        );
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Claude wants to use {tool}"))
                            .color(theme::TEXT_DIM)
                            .size(13.0),
                    );
                    if !cmd.trim().is_empty() {
                        ui.add_space(10.0);
                        code_box(ui, &clip_lines(cmd.trim_matches('\n'), 12));
                    }
                    ui.add_space(14.0);
                    for (i, (label, _)) in opts.iter().enumerate() {
                        let resp = perm_option_row(ui, i + 1, label, i == st.perm_sel);
                        if resp.hovered() {
                            st.perm_sel = i;
                        }
                        if resp.clicked() {
                            confirm = Some(i);
                        }
                        ui.add_space(6.0);
                    }
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("↑↓ move · 1–2 pick · enter confirm · esc deny")
                            .color(theme::TEXT_FAINT)
                            .size(11.0),
                    );
                });
        });
    if let Some(sel) = confirm {
        actions.push(UiAction::Perm(p.id, opts[sel].1));
        st.perm_sel = 0;
    }
}

/// One selectable row in the permission popup: a numbered, highlightable
/// button. Returns its response (hover selects, click confirms).
fn perm_option_row(ui: &mut egui::Ui, num: usize, label: &str, selected: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
    let hovered = resp.hovered();
    let bg = if selected {
        theme::bg_selected()
    } else if hovered {
        theme::bg_raised()
    } else {
        theme::bg_panel()
    };
    let edge = if selected {
        theme::accent()
    } else {
        theme::LINE_BORDER
    };
    let paint = ui.painter();
    paint.rect(
        rect,
        CornerRadius::same(9),
        bg,
        Stroke::new(1.0, edge),
        egui::StrokeKind::Inside,
    );
    paint.text(
        rect.left_center() + egui::vec2(14.0, 0.0),
        egui::Align2::LEFT_CENTER,
        num.to_string(),
        egui::FontId::monospace(13.0),
        theme::accent(),
    );
    paint.text(
        rect.left_center() + egui::vec2(40.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.0),
        if selected {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_BODY
        },
    );
    resp
}

/// Terminal-mode body: the pane's live vt100 screen, painted cell-by-cell
/// as a fixed monospace grid (bg quads + glyphs + block cursor) reusing
/// `palette::cell_colors`. Kitty graphics are not composited here yet — that
/// needs the GPU grid renderer drawn into egui (task #26b, follow-on).
/// Extract the text inside a terminal selection — one line per grid row with
/// trailing blanks trimmed, rows joined by newlines.
pub(crate) fn term_selection_text(p: &CPane, sel: Option<TermSel>) -> Option<String> {
    let sel = sel?;
    if sel.pane != p.id {
        return None;
    }
    let (s, e) = sel.ordered();
    let screen = p.parser.screen();
    let (rows, cols) = screen.size();
    if cols == 0 || rows == 0 {
        return None;
    }
    let last_col = cols as usize - 1;
    let last_row = (e.0).min(rows.saturating_sub(1) as usize);
    let mut out = String::new();
    for row in s.0..=last_row {
        let (c0, c1) = match (row == s.0, row == e.0) {
            (true, true) => (s.1, e.1),
            (true, false) => (s.1, last_col),
            (false, true) => (0, e.1),
            (false, false) => (0, last_col),
        };
        let mut line = String::new();
        for col in c0..=c1.min(last_col) {
            if let Some(cell) = screen.cell(row as u16, col as u16) {
                if !cell.is_wide_continuation() {
                    let ch = cell.contents();
                    line.push_str(if ch.is_empty() { " " } else { &ch });
                }
            }
        }
        while line.ends_with(' ') {
            line.pop();
        }
        if row != last_row {
            line.push('\n');
        }
        out.push_str(&line);
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn terminal_view(ui: &mut egui::Ui, st: &mut UiState, p: &CPane, scale: f32) {
    use crate::palette::{cell_colors, CellStyle};
    // The pty owns the keyboard here: drop any widget focus egui may still be
    // holding (a Tab press is focus traversal to egui, and a focused widget
    // would keep swallowing keys that belong to the terminal).
    ui.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id);
        }
    });
    let c32 = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
    let font = egui::FontId::monospace(13.0 * scale);
    let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let screen = p.parser.screen();
    let (rows, cols) = screen.size();
    egui::Frame::NONE
        .fill(c32(crate::palette::DEFAULT_BG))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Size the pty to the area actually painted — inside this frame's
            // margins. This is the number the child lays its UI out against, so
            // it has to match the drawn grid exactly or the last row/column is
            // cut off (Claude Code's prompt box loses its border and its
            // bottom line is clipped).
            let avail = ui.available_size();
            if cw > 1.0 && ch > 1.0 {
                let c = ((avail.x / cw).floor() as i32).clamp(10, 1000) as u16;
                let r = ((avail.y / ch).floor() as i32).clamp(2, 1000) as u16;
                st.term_grid = Some((r, c, cw.round() as u16, ch.round() as u16));
            }
            egui::ScrollArea::both()
                .id_salt("term")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Text selection is available when the app isn't grabbing
                    // the mouse — or, like a real terminal, when Shift is held.
                    let shift = ui.input(|i| i.modifiers.shift);
                    let mouse_app = screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None;
                    let selectable = shift || !mouse_app;
                    let sense = if selectable {
                        Sense::click_and_drag()
                    } else {
                        Sense::hover()
                    };
                    let (rect, resp) = ui
                        .allocate_exact_size(egui::vec2(cols as f32 * cw, rows as f32 * ch), sense);
                    let o = rect.min;
                    // Publish the grid's real geometry (egui points) so mouse
                    // reporting maps clicks with the same numbers the glyphs
                    // are drawn with.
                    st.term_rect = Some(TermRect {
                        origin: (o.x, o.y),
                        cell: (cw, ch),
                        size: (rows, cols),
                    });
                    let cell_at = |pos: egui::Pos2| -> (usize, usize) {
                        // `.max(0)` guards a 0-sized grid: clamp(0, -1) panics.
                        let c = (((pos.x - o.x) / cw).floor() as i32)
                            .clamp(0, (cols as i32 - 1).max(0));
                        let r = (((pos.y - o.y) / ch).floor() as i32)
                            .clamp(0, (rows as i32 - 1).max(0));
                        (r as usize, c as usize)
                    };
                    if selectable {
                        if resp.drag_started() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let a = cell_at(pos);
                                st.term_sel = Some(TermSel {
                                    pane: p.id,
                                    anchor: a,
                                    head: a,
                                });
                            }
                        } else if resp.dragged() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                if let Some(sel) = st.term_sel.as_mut() {
                                    if sel.pane == p.id {
                                        sel.head = cell_at(pos);
                                    }
                                }
                            }
                        }
                        if resp.drag_stopped() {
                            if let Some(text) = term_selection_text(p, st.term_sel) {
                                ui.ctx().copy_text(text);
                            }
                        }
                        // A plain click (no drag) clears the selection.
                        if resp.clicked() {
                            st.term_sel = None;
                        }
                    }
                    let painter = ui.painter_at(rect);
                    for row in 0..rows {
                        for col in 0..cols {
                            let Some(cell) = screen.cell(row, col) else {
                                continue;
                            };
                            if cell.is_wide_continuation() {
                                continue;
                            }
                            let style = CellStyle {
                                fg: cell.fgcolor(),
                                bg: cell.bgcolor(),
                                bold: cell.bold(),
                                dim: cell.dim(),
                                inverse: cell.inverse(),
                            };
                            let (fg, bg) = cell_colors(&style);
                            let x = o.x + col as f32 * cw;
                            let y = o.y + row as f32 * ch;
                            if let Some(bg) = bg {
                                painter.rect_filled(
                                    Rect::from_min_size(
                                        egui::pos2(x, y),
                                        egui::vec2(cw + 0.5, ch + 0.5),
                                    ),
                                    CornerRadius::ZERO,
                                    c32(bg),
                                );
                            }
                            if st.term_sel.is_some_and(|s| {
                                s.pane == p.id && s.contains(row as usize, col as usize)
                            }) {
                                painter.rect_filled(
                                    Rect::from_min_size(
                                        egui::pos2(x, y),
                                        egui::vec2(cw + 0.5, ch + 0.5),
                                    ),
                                    CornerRadius::ZERO,
                                    fade(theme::accent(), 0.30),
                                );
                            }
                            let contents = cell.contents();
                            if !contents.is_empty() && contents != " " {
                                painter.text(
                                    egui::pos2(x, y),
                                    egui::Align2::LEFT_TOP,
                                    contents,
                                    font.clone(),
                                    c32(fg),
                                );
                            }
                            if cell.underline() {
                                painter.rect_filled(
                                    Rect::from_min_size(
                                        egui::pos2(x, y + ch - 1.5),
                                        egui::vec2(cw, 1.0),
                                    ),
                                    CornerRadius::ZERO,
                                    c32(fg),
                                );
                            }
                        }
                    }
                    // Block cursor.
                    if !screen.hide_cursor() && p.scroll == 0 {
                        let (r, c) = screen.cursor_position();
                        if r < rows && c < cols {
                            painter.rect_filled(
                                Rect::from_min_size(
                                    egui::pos2(o.x + c as f32 * cw, o.y + r as f32 * ch),
                                    egui::vec2(cw, ch),
                                ),
                                CornerRadius::ZERO,
                                Color32::from_rgba_unmultiplied(230, 230, 230, 90),
                            );
                        }
                    }
                    // Kitty images (task #32): decode each placement to an
                    // egui-managed texture (cached by pane/img/ver) and blit it
                    // over the grid at its cell rect. Unicode-placeholder
                    // (`virt`) tiling and z-ordering under text are follow-ons.
                    let scroll = p.scroll as i32;
                    for vp in &p.gfx.placements {
                        if vp.virt || vp.cols == 0 || vp.rows == 0 {
                            continue;
                        }
                        let Some(img) = p.images.get(&vp.img) else {
                            continue;
                        };
                        if img.ver != vp.img_ver {
                            continue;
                        }
                        let key = (p.id, vp.img, img.ver);
                        let tex = if let Some(t) = st.tex_cache.get(&key) {
                            Some(t.clone())
                        } else if let Some((iw, ih, rgba)) =
                            crate::img::decode_rgba(img.format, img.zlib, img.w, img.h, &img.data)
                        {
                            let ci = egui::ColorImage::from_rgba_unmultiplied(
                                [iw as usize, ih as usize],
                                &rgba,
                            );
                            let h = ui.ctx().load_texture(
                                format!("kitty-{}-{}-{}", p.id, vp.img, img.ver),
                                ci,
                                egui::TextureOptions::LINEAR,
                            );
                            st.tex_cache.insert(key, h.clone());
                            Some(h)
                        } else {
                            None
                        };
                        let Some(tex) = tex else { continue };
                        let x = o.x + vp.col as f32 * cw + vp.offx as f32;
                        let y = o.y + (vp.row + scroll) as f32 * ch + vp.offy as f32;
                        let rect = Rect::from_min_size(
                            egui::pos2(x, y),
                            egui::vec2(vp.cols as f32 * cw, vp.rows as f32 * ch),
                        );
                        let (sx, sy, sw, sh) = vp.src;
                        let (iw, ih) = (img.w.max(1) as f32, img.h.max(1) as f32);
                        let uv = if sw > 0 && sh > 0 {
                            Rect::from_min_size(
                                egui::pos2(sx as f32 / iw, sy as f32 / ih),
                                egui::vec2(sw as f32 / iw, sh as f32 / ih),
                            )
                        } else {
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                        };
                        painter.image(tex.id(), rect, uv, Color32::WHITE);
                    }
                });
        });
}

const PICKER_NUM_KEYS: [egui::Key; 9] = [
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Num5,
    egui::Key::Num6,
    egui::Key::Num7,
    egui::Key::Num8,
    egui::Key::Num9,
];

/// Apply a picker selection: on the harness step, advance to the model step
/// (or create straight away when the harness has a single model); on the
/// model step, emit `CreateAgent` and close.
fn agent_activate(
    st: &mut UiState,
    actions: &mut Vec<UiAction>,
    step: usize,
    h_sel: usize,
    i: usize,
) {
    let n_h = st.agent_picker.harnesses.len();
    if n_h == 0 {
        return;
    }
    if step == 0 {
        let i = i.min(n_h - 1);
        st.agent_picker.h_sel = i;
        let h = &st.agent_picker.harnesses[i];
        if h.models.len() <= 1 {
            let model = h.models.first().and_then(|m| m.value.clone());
            let agent = h.name.clone();
            actions.push(UiAction::CreateAgent {
                agent,
                model,
                session: None,
            });
            st.overlay = Overlay::None;
        } else {
            st.agent_picker.step = 1;
            st.agent_picker.m_sel = 0;
        }
    } else {
        let h = &st.agent_picker.harnesses[h_sel.min(n_h - 1)];
        let model = h.models.get(i).and_then(|m| m.value.clone());
        let agent = h.name.clone();
        actions.push(UiAction::CreateAgent {
            agent,
            model,
            session: None,
        });
        st.overlay = Overlay::None;
    }
}

/// New-agent picker (Alt+N): choose a harness, then a model. Navigable by
/// mouse, number keys, ↑/↓ (or j/k), and Enter; Esc backs out a step or
/// closes. Emits `UiAction::CreateAgent` with the chosen harness + model.
fn new_agent_dialog(ui: &mut egui::Ui, st: &mut UiState, actions: &mut Vec<UiAction>) {
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 150),
    );
    if st.agent_picker.harnesses.is_empty() {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            st.overlay = Overlay::None;
        }
        egui::Area::new(egui::Id::new("new_agent"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                Frame::NONE
                    .fill(theme::bg_card())
                    .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_width(440.0);
                        ui.label(
                            RichText::new("No agent harnesses found")
                                .color(theme::TEXT_PRIMARY)
                                .size(16.0)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Install claude or pi on your PATH to start a structured agent.",
                            )
                            .color(theme::TEXT_DIM)
                            .size(12.5),
                        );
                    });
            });
        return;
    }

    let step = st.agent_picker.step;
    let n_h = st.agent_picker.harnesses.len();
    let h_sel = st.agent_picker.h_sel.min(n_h - 1);
    let (title, rows): (String, Vec<(String, String)>) = if step == 0 {
        (
            "New agent · choose a harness".to_string(),
            st.agent_picker
                .harnesses
                .iter()
                .map(|h| {
                    let n = h.models.len();
                    (
                        h.label.clone(),
                        format!("{n} model{}", if n == 1 { "" } else { "s" }),
                    )
                })
                .collect(),
        )
    } else {
        let h = &st.agent_picker.harnesses[h_sel];
        (
            format!("{} · choose a model", h.label),
            h.models
                .iter()
                .map(|m| {
                    (
                        m.label.clone(),
                        m.value.clone().unwrap_or_else(|| "harness default".into()),
                    )
                })
                .collect(),
        )
    };
    let count = rows.len();

    let mut activate: Option<usize> = None;
    let mut back = false;
    ui.input(|i| {
        let down = i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::J);
        let up = i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::K);
        let enter = i.key_pressed(egui::Key::Enter);
        if i.key_pressed(egui::Key::Escape) {
            back = true;
        }
        let mut num = None;
        for (n, key) in PICKER_NUM_KEYS.iter().enumerate().take(count.min(9)) {
            if i.key_pressed(*key) {
                num = Some(n);
            }
        }
        let cur = if step == 0 {
            &mut st.agent_picker.h_sel
        } else {
            &mut st.agent_picker.m_sel
        };
        if count > 0 {
            if down {
                *cur = (*cur + 1).min(count - 1);
            }
            if up {
                *cur = cur.saturating_sub(1);
            }
            if let Some(n) = num {
                *cur = n;
                activate = Some(n);
            }
            if enter {
                activate = Some((*cur).min(count - 1));
            }
        }
    });
    if back {
        if step == 1 && n_h > 1 {
            st.agent_picker.step = 0;
        } else {
            st.overlay = Overlay::None;
        }
        return;
    }
    if let Some(i) = activate {
        agent_activate(st, actions, step, h_sel, i);
        return;
    }

    let sel = if step == 0 {
        st.agent_picker.h_sel
    } else {
        st.agent_picker.m_sel
    }
    .min(count.saturating_sub(1));
    let mut clicked: Option<usize> = None;
    egui::Area::new(egui::Id::new("new_agent"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(theme::bg_card())
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(480.0);
                    ui.label(
                        RichText::new(&title)
                            .color(theme::TEXT_PRIMARY)
                            .size(17.0)
                            .strong(),
                    );
                    ui.add_space(12.0);
                    for (i, (label, sub)) in rows.iter().enumerate() {
                        if picker_row(ui, i + 1, label, sub, i == sel).clicked() {
                            clicked = Some(i);
                        }
                        ui.add_space(6.0);
                    }
                    ui.add_space(2.0);
                    let hint = if step == 0 && n_h > 1 {
                        "↑↓ move · 1–9 pick · enter choose · esc cancel"
                    } else {
                        "↑↓ move · 1–9 pick · enter start · esc back"
                    };
                    ui.label(RichText::new(hint).color(theme::TEXT_FAINT).size(11.0));
                });
        });
    if let Some(i) = clicked {
        agent_activate(st, actions, step, h_sel, i);
    }
}

/// A two-line option row (label + dim sub) for the new-agent picker.
fn picker_row(
    ui: &mut egui::Ui,
    num: usize,
    label: &str,
    sub: &str,
    selected: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 46.0), Sense::click());
    let hovered = resp.hovered();
    let bg = if selected {
        theme::bg_selected()
    } else if hovered {
        theme::bg_raised()
    } else {
        theme::bg_panel()
    };
    let edge = if selected {
        theme::accent()
    } else {
        theme::LINE_BORDER
    };
    let paint = ui.painter();
    paint.rect(
        rect,
        CornerRadius::same(9),
        bg,
        Stroke::new(1.0, edge),
        egui::StrokeKind::Inside,
    );
    paint.text(
        rect.left_center() + egui::vec2(14.0, 0.0),
        egui::Align2::LEFT_CENTER,
        num.to_string(),
        egui::FontId::monospace(13.0),
        theme::accent(),
    );
    paint.text(
        rect.left_center() + egui::vec2(40.0, -8.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.5),
        if selected {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_BODY
        },
    );
    paint.text(
        rect.left_center() + egui::vec2(40.0, 10.0),
        egui::Align2::LEFT_CENTER,
        sub,
        egui::FontId::monospace(11.5),
        theme::TEXT_FAINT,
    );
    resp
}

/// A small segmented-control button; returns true when clicked.
fn seg(ui: &mut egui::Ui, label: &str, on: bool) -> bool {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(if on {
                theme::bg_chrome()
            } else {
                theme::TEXT_DIM
            })
            .size(12.0),
    )
    .fill(if on {
        theme::accent()
    } else {
        theme::bg_raised()
    })
    .corner_radius(CornerRadius::same(6));
    ui.add(btn).clicked()
}

/// The right activity rail: session facts (the output histogram needs
/// server-side rate buckets — deferred).
fn activity_rail(ui: &mut egui::Ui, d: &UiData) {
    let Some(p) = d.panes.get(d.active) else {
        return;
    };
    let ps = d.ps(p);
    // Activity: output-rate histogram (last ~10 min of buckets).
    ui.label(
        RichText::new("ACTIVITY")
            .color(theme::TEXT_GHOST)
            .size(11.0)
            .strong(),
    );
    ui.add_space(8.0);
    let vals = rate_vals(p);
    if vals.iter().any(|v| *v > 0) {
        sparkline(ui, &vals, theme::STATUS_RAIL[2], 44.0, 6.0, 2.0);
        ui.label(
            RichText::new("output rate · last 10m")
                .color(theme::TEXT_GHOST)
                .size(11.0),
        );
    } else {
        ui.label(RichText::new("idle").color(theme::TEXT_GHOST).size(12.0));
    }
    ui.add_space(16.0);
    ui.label(
        RichText::new("SESSION")
            .color(theme::TEXT_GHOST)
            .size(11.0)
            .strong(),
    );
    ui.add_space(10.0);
    let row = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(k).color(theme::TEXT_FAINT).size(12.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    Label::new(
                        RichText::new(v)
                            .color(theme::TEXT_DIM)
                            .size(12.0)
                            .monospace(),
                    )
                    .truncate(),
                );
            });
        });
        ui.add_space(6.0);
    };
    row(ui, "session", d.session.to_string());
    if let Some(s) = ps {
        if let Some(cwd) = &s.cwd {
            row(ui, "cwd", clip(cwd, 26));
        }
        if let Some(host) = &s.ssh {
            row(ui, "ssh", clip(host, 20));
        }
        row(ui, "uptime", fmt_age(s.uptime_ms));
        row(
            ui,
            "auto-resume",
            if s.auto_resume {
                "on".into()
            } else {
                "off".into()
            },
        );
    }
    // The agent's live plan (from TodoWrite), pinned to the bottom of the rail
    // — the empty space below the session facts.
    if p.is_agent() && !p.agent.todos.is_empty() {
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 0.0),
                Layout::top_down(Align::Min),
                |ui| todo_panel(ui, &p.agent.todos),
            );
        });
    }
}

/// The PLAN block: a header with a done/total count, a progress bar, and the
/// todo list (finished struck-through, the in-progress one highlighted).
fn todo_panel(ui: &mut egui::Ui, todos: &[zodiac::client_core::TodoItem]) {
    let done = todos.iter().filter(|t| t.done()).count();
    let total = todos.len();
    ui.separator();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("PLAN")
                .color(theme::TEXT_GHOST)
                .size(11.0)
                .strong(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{done}/{total}"))
                    .color(if done == total {
                        theme::STATUS_TEXT[3]
                    } else {
                        theme::TEXT_DIM
                    })
                    .size(11.0)
                    .monospace(),
            );
        });
    });
    ui.add_space(7.0);
    let frac = if total > 0 {
        done as f32 / total as f32
    } else {
        0.0
    };
    progress_bar(ui, frac);
    ui.add_space(10.0);
    egui::ScrollArea::vertical()
        .id_salt("plan_scroll")
        .max_height(300.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for t in todos {
                todo_row(ui, t);
            }
        });
}

/// One plan row: a status glyph + the item text.
fn todo_row(ui: &mut egui::Ui, t: &zodiac::client_core::TodoItem) {
    let (glyph, gcol, tcol) = if t.done() {
        ("✓", theme::STATUS_RAIL[3], theme::TEXT_FAINT)
    } else if t.active() {
        ("▸", theme::accent(), theme::TEXT_PRIMARY)
    } else {
        ("○", theme::TEXT_GHOST, theme::TEXT_DIM)
    };
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(glyph).color(gcol).size(12.0).monospace());
        ui.add_space(7.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            let mut rt = RichText::new(t.content.as_str()).color(tcol).size(12.5);
            if t.done() {
                rt = rt.strikethrough();
            }
            if t.active() {
                rt = rt.strong();
            }
            ui.label(rt);
        });
    });
    ui.add_space(6.0);
}

/// A thin accent progress bar filled to `frac` (0..1).
fn progress_bar(ui: &mut egui::Ui, frac: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), Sense::hover());
    let paint = ui.painter();
    paint.rect_filled(rect, CornerRadius::same(3), theme::bg_raised());
    if frac > 0.0 {
        let fill = Rect::from_min_size(
            rect.min,
            egui::vec2(rect.width() * frac.clamp(0.0, 1.0), rect.height()),
        );
        paint.rect_filled(fill, CornerRadius::same(3), theme::accent());
    }
}

/// Settings dialog (⌘,): grouped editor over the real `config.json` keys.
/// Every control persists (SaveSettings) so the server/TUI pick it up; the
/// keys are unchanged, per the handoff. (Full 33-row parity + live GUI theme
/// re-application are follow-ons.)
fn settings_dialog(
    ui: &mut egui::Ui,
    st: &mut UiState,
    s: &mut zodiac::settings::Settings,
    actions: &mut Vec<UiAction>,
) {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.overlay = Overlay::None;
    }
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.max_rect());
    ui.painter().rect_filled(
        screen,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );
    let mut close = false;
    egui::Area::new(egui::Id::new("settings"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(theme::bg_card())
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(560.0);
                    ui.label(
                        RichText::new("Settings")
                            .color(theme::TEXT_PRIMARY)
                            .size(19.0)
                            .strong(),
                    );
                    ui.add_space(12.0);
                    // Cap the scrolling body so the dialog never runs off a
                    // short window (or at a large GUI font zoom); the title and
                    // Done footer stay pinned outside the scroll.
                    let body_h = (screen.height() - 200.0).max(220.0);
                    egui::ScrollArea::vertical()
                        .id_salt("settings_body")
                        .max_height(body_h)
                        .show(ui, |ui| {
                            group_label(ui, "APPEARANCE");
                            choice_row(
                                ui,
                                "Theme",
                                &mut s.theme,
                                "night",
                                &[
                                    ("night", "slate·brass"),
                                    ("oled-orange", "oled·orange"),
                                    ("oled-green", "oled·green"),
                                ],
                                actions,
                            );
                            choice_row(
                                ui,
                                "Card numerals",
                                &mut s.card_numeral,
                                "zodiac",
                                &[
                                    ("zodiac", "zodiac"),
                                    ("roman", "roman"),
                                    ("arabic", "arabic"),
                                ],
                                actions,
                            );
                            choice_row(
                                ui,
                                "Home view",
                                &mut s.home_view,
                                "cards",
                                &[("cards", "cards"), ("list", "list")],
                                actions,
                            );
                            choice_row(
                                ui,
                                "Motion",
                                &mut s.gui_motion,
                                "full",
                                &[("full", "full"), ("reduced", "reduced"), ("off", "off")],
                                actions,
                            );
                            ui.add_space(14.0);
                            group_label(ui, "FONT SIZE");
                            let sizes = &[
                                ("80%", "80%"),
                                ("90%", "90%"),
                                ("100%", "100%"),
                                ("110%", "110%"),
                                ("125%", "125%"),
                                ("150%", "150%"),
                            ];
                            choice_row(ui, "Terminal", &mut s.term_font, "100%", sizes, actions);
                            choice_row(ui, "GUI", &mut s.gui_font, "100%", sizes, actions);
                            choice_row(ui, "Agent chat", &mut s.agent_font, "100%", sizes, actions);
                            ui.add_space(14.0);
                            group_label(ui, "BEHAVIOR");
                            toggle_row(ui, "Connection watchdog", &mut s.connection_watch, actions);
                            toggle_row(
                                ui,
                                "Kitty keyboard protocol",
                                &mut s.kitty_keyboard,
                                actions,
                            );
                            choice_row(
                                ui,
                                "Capability floor",
                                &mut s.capability_floor,
                                "images",
                                &[
                                    ("off", "off"),
                                    ("images", "images"),
                                    ("animation", "animation"),
                                ],
                                actions,
                            );
                        });
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("~/.config/zodiac/config.json")
                                .color(theme::TEXT_GHOST)
                                .size(11.5)
                                .monospace(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if amber_button(ui, "Done").clicked() {
                                close = true;
                            }
                        });
                    });
                });
        });
    if close {
        st.overlay = Overlay::None;
    }
}

/// An uppercase group heading in a settings dialog.
fn group_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .color(theme::TEXT_GHOST)
            .size(11.0)
            .strong(),
    );
    ui.add_space(6.0);
}

/// A labelled segmented choice bound to a String config key (empty = default).
fn choice_row(
    ui: &mut egui::Ui,
    label: &str,
    field: &mut String,
    default: &str,
    opts: &[(&str, &str)],
    actions: &mut Vec<UiAction>,
) {
    let cur = if field.is_empty() {
        default
    } else {
        field.as_str()
    };
    let cur = cur.to_string();
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(theme::TEXT_BODY).size(13.5));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for (val, disp) in opts.iter().rev() {
                if seg(ui, disp, cur == *val) && cur != *val {
                    *field = (*val).to_string();
                    actions.push(UiAction::SaveSettings);
                }
                ui.add_space(4.0);
            }
        });
    });
    ui.add_space(8.0);
}

/// A labelled boolean toggle bound to a bool config key.
fn toggle_row(ui: &mut egui::Ui, label: &str, field: &mut bool, actions: &mut Vec<UiAction>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(theme::TEXT_BODY).size(13.5));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.checkbox(field, "").changed() {
                actions.push(UiAction::SaveSettings);
            }
        });
    });
    ui.add_space(8.0);
}

/// The 52px title bar: amber mark, "zodiac", session chip, chrome buttons,
/// host vitals.
fn title_bar(root: &mut egui::Ui, d: &UiData, st: &mut UiState, actions: &mut Vec<UiAction>) {
    egui::Panel::top("titlebar")
        .exact_size(52.0)
        .frame(
            Frame::NONE
                .fill(theme::bg_chrome())
                .inner_margin(Margin::symmetric(18, 8)),
        )
        .show(root, |ui| {
            ui.horizontal_centered(|ui| {
                // Clear the real traffic lights, which macOS draws over our
                // content because the window uses a fullsize content view.
                #[cfg(target_os = "macos")]
                ui.add_space(72.0);
                mark(ui);
                ui.add_space(8.0);
                // The wordmark doubles as the window drag handle (custom
                // chrome — no OS titlebar).
                let word = ui.add(
                    egui::Label::new(
                        RichText::new("zodiac")
                            .color(theme::TEXT_PRIMARY)
                            .size(15.0)
                            .strong(),
                    )
                    .sense(Sense::click_and_drag()),
                );
                if word.drag_started() {
                    actions.push(UiAction::DragWindow);
                }
                ui.add_space(12.0);
                session_chip(ui, d.session);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Window controls (right-most). macOS has real traffic
                    // lights on the left, so drawing our own would be a
                    // second, wrong-side set of the same buttons.
                    #[cfg(not(target_os = "macos"))]
                    {
                        if win_btn(ui, "✕", theme::STATUS_RAIL[0]) {
                            actions.push(UiAction::Quit);
                        }
                        if win_btn(ui, "–", theme::TEXT_DIM) {
                            actions.push(UiAction::Minimize);
                        }
                        ui.add_space(6.0);
                    }
                    if chrome_btn(ui, "settings") {
                        st.overlay = Overlay::Settings;
                    }
                    if chrome_btn(ui, "oracle") {
                        st.overlay = Overlay::Oracle;
                    }
                    if chrome_btn(ui, "pair phone") {
                        st.overlay = Overlay::Pairing;
                    }
                    if chrome_btn(ui, concat_cmd!("K find pane")) {
                        st.overlay = Overlay::Palette;
                        st.palette_query.clear();
                        st.palette_sel = 0;
                    }
                    ui.add_space(8.0);
                    if let Some(h) = d.state.and_then(|s| s.host.as_ref()) {
                        vitals(ui, h);
                    }
                });
            });
        });
}

/// A borderless window-control glyph button (minimize / close).
fn win_btn(ui: &mut egui::Ui, glyph: &str, color: Color32) -> bool {
    let btn = egui::Button::new(RichText::new(glyph).color(color).size(15.0))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(6));
    let r = ui.add(btn);
    ui.add_space(4.0);
    r.clicked()
}

/// A bordered chrome button (title-bar controls). Returns true on click.
fn chrome_btn(ui: &mut egui::Ui, label: &str) -> bool {
    let btn = egui::Button::new(RichText::new(label).color(theme::TEXT_DIM).size(12.5))
        .fill(theme::bg_chrome())
        .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
        .corner_radius(CornerRadius::same(7));
    let r = ui.add(btn);
    ui.add_space(8.0);
    r.clicked()
}

/// The 22px amber-gradient mark with a `❯` — a flat amber tile for now (the
/// gradient mesh lands with the app-shell task #24).
fn mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(6), theme::accent());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "❯",
        egui::FontId::proportional(13.0),
        theme::bg_chrome(),
    );
}

/// `● main` session pill.
fn session_chip(ui: &mut egui::Ui, session: &str) {
    Frame::NONE
        .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            ui.painter()
                .circle_filled(dot.center(), 3.0, theme::STATUS_RAIL[3]);
            ui.label(RichText::new(session).color(theme::TEXT_PRIMARY).size(13.0));
        });
}

/// Host vitals on the right of the title bar: uptime, cpu%, mem%.
fn vitals(ui: &mut egui::Ui, h: &zodiac::protocol::HostVitals) {
    let meta = |ui: &mut egui::Ui, s: String| {
        ui.label(
            RichText::new(s)
                .color(theme::TEXT_DIM)
                .size(12.0)
                .monospace(),
        );
    };
    meta(ui, format!("mem {}%", h.mem_pct));
    ui.add_space(10.0);
    meta(ui, format!("cpu {}%", h.cpu_pct));
    ui.add_space(10.0);
    meta(ui, format!("up {}", fmt_uptime(h.uptime_ms)));
}

/// Coarse host uptime like `3d 4h` / `4h 12m` / `12m`.
fn fmt_uptime(ms: u64) -> String {
    let s = ms / 1000;
    let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Observatory: summary strip + responsive card grid from live state.
fn observatory(root: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    egui::CentralPanel::default()
        .frame(
            Frame::NONE
                .fill(theme::bg_window())
                .inner_margin(Margin::same(20)),
        )
        .show(root, |ui| {
            summary_strip(ui, d, actions);
            ui.add_space(14.0);
            if d.panes.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    if d.state.is_some() {
                        // Connected, just nothing running — guide the user
                        // instead of implying we're still connecting.
                        ui.label(
                            RichText::new("No panes yet")
                                .color(theme::TEXT_DIM)
                                .size(16.0)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Alt+N for an agent · Alt+Shift+N for a shell · Alt+Z here",
                            )
                            .color(theme::TEXT_FAINT)
                            .size(13.0),
                        );
                    } else {
                        ui.label(
                            RichText::new("connecting…")
                                .color(theme::TEXT_FAINT)
                                .size(15.0),
                        );
                    }
                });
                return;
            }
            observatory_nav(ui, d, actions);
            if d.home_view == "list" {
                card_list(ui, d, actions);
            } else {
                card_grid(ui, d, actions);
            }
        });
}

/// Keyboard navigation on the Observatory: arrow keys move the selection
/// (wrapping), Enter/Space opens the selected pane. Handled here in the egui
/// frame — which sees window events before the raw pty path — and only when no
/// text widget holds focus, so overlays and the composer keep their own keys.
/// Consumes the keys so the card ScrollArea doesn't also scroll on them.
fn observatory_nav(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    let n = d.panes.len();
    if n == 0 || ui.memory(|m| m.focused()).is_some() {
        return;
    }
    let mut sel = d.active.min(n - 1);
    let (mut moved, mut open) = (false, false);
    ui.input_mut(|i| {
        use egui::{Key, Modifiers};
        for k in [Key::ArrowDown, Key::ArrowRight] {
            if i.consume_key(Modifiers::NONE, k) {
                sel = (sel + 1) % n;
                moved = true;
            }
        }
        for k in [Key::ArrowUp, Key::ArrowLeft] {
            if i.consume_key(Modifiers::NONE, k) {
                sel = (sel + n - 1) % n;
                moved = true;
            }
        }
        for k in [Key::Enter, Key::Space] {
            if i.consume_key(Modifiers::NONE, k) {
                open = true;
            }
        }
    });
    if open {
        actions.push(UiAction::Open(sel));
    } else if moved {
        actions.push(UiAction::Focus(sel));
    }
}

/// Observatory list layout (home_view = "list"): one compact row per pane
/// (sigil, name, agent, cwd, status pill), click to open.
fn card_list(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    egui::ScrollArea::vertical()
        .id_salt("home_list")
        .show(ui, |ui| {
            for (i, p) in d.panes.iter().enumerate() {
                let si = d.si(p);
                let ps = d.ps(p);
                let fr = Frame::NONE
                    .fill(theme::bg_card())
                    .stroke(Stroke::new(1.0, theme::LINE_BORDER))
                    .corner_radius(CornerRadius::same(9))
                    .inner_margin(Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            sigil_tile(ui, i + 1, si, d.numeral);
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(clip(&p.name, 32))
                                    .color(theme::TEXT_PRIMARY)
                                    .size(14.0)
                                    .strong(),
                            );
                            if let Some(agent) = ps.and_then(|s| s.agent.as_deref()) {
                                agent_chip(ui, agent, ps.and_then(|s| s.version.as_deref()));
                            }
                            if let Some(cwd) = ps.and_then(|s| s.cwd.as_deref()) {
                                ui.label(
                                    RichText::new(clip(cwd, 40))
                                        .color(theme::TEXT_FAINT)
                                        .size(11.5)
                                        .monospace(),
                                );
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                status_pill(ui, si, theme::STATUS_WORD[si]);
                            });
                        });
                    });
                let r = fr.response.rect;
                ui.painter().rect_filled(
                    Rect::from_min_size(r.min, egui::vec2(2.0, r.height())),
                    CornerRadius::same(2),
                    theme::STATUS_RAIL[si],
                );
                if fr
                    .response
                    .interact(Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    actions.push(UiAction::Open(i));
                }
                ui.add_space(6.0);
            }
        });
}

/// Pane count, per-status pill tally, and the New-pane buttons.
fn summary_strip(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(d.panes.len().to_string())
                .color(theme::TEXT_PRIMARY)
                .size(24.0)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(RichText::new("panes").color(theme::TEXT_FAINT).size(13.0));
        ui.add_space(14.0);

        let mut counts = [0usize; 5];
        for p in d.panes {
            counts[d.si(p)] += 1;
        }
        for (idx, n) in counts.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            status_pill(ui, idx, &format!("{n} {}", theme::STATUS_WORD[idx]));
            ui.add_space(6.0);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if amber_button(ui, "New pane").clicked() {
                actions.push(UiAction::NewShell);
            }
            ui.add_space(8.0);
            if ui.button(RichText::new("New agent").size(13.0)).clicked() {
                actions.push(UiAction::NewAgent);
            }
        });
    });
}

/// A rounded status pill: a colored dot + word, tinted per status.
fn status_pill(ui: &mut egui::Ui, idx: usize, text: &str) {
    Frame::NONE
        .fill(fade(theme::STATUS_RAIL[idx], 0.10))
        .stroke(Stroke::new(1.0, fade(theme::STATUS_RAIL[idx], 0.28)))
        .corner_radius(CornerRadius::same(99))
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
            ui.painter()
                .circle_filled(dot.center(), 3.0, theme::STATUS_RAIL[idx]);
            ui.label(
                RichText::new(text)
                    .color(theme::STATUS_TEXT[idx])
                    .size(12.5),
            );
        });
}

/// The amber primary button (dark text on amber).
fn amber_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(text)
            .color(theme::bg_chrome())
            .size(13.0)
            .strong(),
    )
    .fill(theme::accent())
    .corner_radius(CornerRadius::same(8));
    ui.add(btn)
}

/// Responsive card grid: as many ~300px columns as fit, row-major, so card
/// order matches pane order (and thus the focus index).
fn card_grid(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    let gap = 14.0;
    egui::ScrollArea::vertical().show(ui, |ui| {
        // One card per row, stacked top-to-bottom. Each card spans the full
        // available width (measured inside the scroll area so the scrollbar is
        // already excluded), so nothing can run off the right edge regardless
        // of window size or long inner content.
        let avail = ui.available_width();
        for (i, p) in d.panes.iter().enumerate() {
            ui.allocate_ui(egui::vec2(avail, 0.0), |ui| {
                ui.set_width(avail);
                pane_card(ui, d, i, p, actions);
            });
            ui.add_space(gap);
        }
    });
}

/// One pane card: sigil tile, name, agent+version chip, status pill, cwd,
/// one-line subtitle, and a transcript-tail well. Click to focus; a 2px
/// status rail runs down the left edge.
fn pane_card(ui: &mut egui::Ui, d: &UiData, i: usize, p: &CPane, actions: &mut Vec<UiAction>) {
    let ps = d.ps(p);
    let si = d.si(p);
    let sel = i == d.active;
    let idle = si == 4;
    let fill = if sel {
        theme::bg_selected()
    } else if si == 0 {
        theme::bg_card_alert()
    } else if idle {
        theme::bg_card_idle()
    } else {
        theme::bg_card()
    };
    let border = if si == 0 {
        fade(theme::STATUS_RAIL[0], 0.25)
    } else {
        theme::LINE_BORDER
    };
    let fr = Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Header row.
            ui.horizontal(|ui| {
                sigil_tile(ui, i + 1, si, d.numeral);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(clip(&p.name, 22))
                                .color(if idle {
                                    theme::TEXT_DIM
                                } else {
                                    theme::TEXT_PRIMARY
                                })
                                .size(15.0)
                                .strong(),
                        );
                        if let Some(agent) = ps.and_then(|s| s.agent.as_deref()) {
                            agent_chip(ui, agent, ps.and_then(|s| s.version.as_deref()));
                            // Running model, when known (claude reports it on
                            // the stream, pi via PaneState).
                            let model = p.agent.model.clone().or_else(|| {
                                ps.and_then(|s| s.model.as_deref())
                                    .map(zodiac::client_core::short_model)
                            });
                            if let Some(m) = model {
                                ui.label(
                                    RichText::new(m)
                                        .color(theme::VIOLET_TEXT)
                                        .size(11.0)
                                        .monospace(),
                                );
                            }
                        }
                    });
                    if let Some(cwd) = ps.and_then(|s| s.cwd.as_deref()) {
                        ui.label(
                            RichText::new(clip(cwd, 40))
                                .color(theme::TEXT_FAINT)
                                .size(11.5)
                                .monospace(),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let age = ps.map(|s| fmt_age(s.uptime_ms)).unwrap_or_default();
                    status_pill(ui, si, &format!("{} {age}", theme::STATUS_WORD[si]));
                });
            });
            ui.add_space(8.0);
            // Summary line.
            let summary = ps
                .and_then(|s| s.subtitle.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if idle {
                        "Idle shell — nothing running.".into()
                    } else {
                        String::new()
                    }
                });
            if !summary.is_empty() {
                ui.label(
                    RichText::new(clip(&summary, 80))
                        .color(if idle {
                            theme::TEXT_FAINT
                        } else {
                            theme::TEXT_BODY
                        })
                        .size(13.5),
                );
                ui.add_space(8.0);
            }
            // Transcript-tail well.
            let tail: Vec<&String> = ps
                .map(|s| s.tail.iter().rev().take(4).collect())
                .unwrap_or_default();
            if !tail.is_empty() {
                tail_well(ui, &tail, si, border);
            }
            // Footer: output-rate sparkline.
            let vals = rate_vals(p);
            if vals.iter().any(|v| *v > 0) {
                ui.add_space(8.0);
                sparkline(ui, &vals, theme::STATUS_RAIL[si], 16.0, 3.0, 1.0);
            }
        });

    // 2px status rail down the left edge of the card.
    let r = fr.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(r.min, egui::vec2(2.0, r.height())),
        CornerRadius::same(2),
        theme::STATUS_RAIL[si],
    );

    if fr
        .response
        .interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        actions.push(UiAction::Open(i));
    }
}

/// The agent+version chip in mono, in a bordered box.
fn agent_chip(ui: &mut egui::Ui, agent: &str, version: Option<&str>) {
    let label = match version {
        Some(v) if !v.is_empty() => format!("{agent} {}", clip(v, 8)),
        _ => agent.to_string(),
    };
    Frame::NONE
        .stroke(Stroke::new(1.0, theme::LINE_BORDER))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(
                RichText::new(label)
                    .color(theme::TEXT_DIM)
                    .size(11.0)
                    .monospace(),
            );
        });
}

/// The transcript excerpt well: a faint panel with a status-colored left
/// border; earlier rows dim, the last row in the status text color.
fn tail_well(ui: &mut egui::Ui, tail_rev: &[&String], si: usize, border: Color32) {
    Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 5))
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // tail_rev is newest-first; show oldest-first with the newest last.
            let n = tail_rev.len();
            for (k, line) in tail_rev.iter().rev().enumerate() {
                let last = k + 1 == n;
                let col = if last {
                    theme::STATUS_TEXT[si]
                } else {
                    theme::TEXT_FAINT
                };
                ui.add(
                    Label::new(
                        RichText::new(clip(line, 64))
                            .color(col)
                            .size(11.5)
                            .monospace(),
                    )
                    .truncate(),
                );
            }
        });
}

/// The 30px rounded sigil tile with the roman numeral, tinted by status.
fn sigil_tile(ui: &mut egui::Ui, n1: usize, si: usize, style: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        Color32::from_rgba_unmultiplied(255, 255, 255, 6),
        Stroke::new(1.0, theme::LINE_BORDER),
        egui::StrokeKind::Inside,
    );
    let col = if si == 4 {
        theme::TEXT_FAINT
    } else {
        theme::accent()
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        numeral(style, n1),
        egui::FontId::proportional(13.0),
        col,
    );
}

/// An output-rate sparkline: `vals` newest-last, drawn as bottom-anchored
/// bars (opacity 0.35→1 by value) in `color`, right-aligned in a strip of
/// the given height. Nothing is drawn when there's no output.
fn sparkline(ui: &mut egui::Ui, vals: &[u32], color: Color32, height: f32, bar_w: f32, gap: f32) {
    let want = ((ui.available_width() + gap) / (bar_w + gap))
        .floor()
        .max(1.0) as usize;
    let start = vals.len().saturating_sub(want);
    let shown = &vals[start..];
    let max = shown.iter().copied().max().unwrap_or(0).max(1) as f32;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    for (i, &v) in shown.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let frac = (v as f32 / max).clamp(0.0, 1.0);
        let h = (frac * height).max(1.0);
        let x = rect.left() + i as f32 * (bar_w + gap);
        let a = (0.35 + 0.65 * frac).clamp(0.0, 1.0);
        let mut c = color;
        c = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0) as u8);
        painter.rect_filled(
            Rect::from_min_size(egui::pos2(x, rect.bottom() - h), egui::vec2(bar_w, h)),
            CornerRadius::ZERO,
            c,
        );
    }
}

/// Collect a pane's rate buckets plus the in-progress bucket, newest last.
fn rate_vals(p: &CPane) -> Vec<u32> {
    let mut v: Vec<u32> = p.rate.iter().copied().collect();
    v.push(p.rate_cur);
    v
}

/// Blend a status color toward the ground at the given alpha, so pills read
/// as a tint rather than a solid fill (the handoff's `#f8717118` style).
fn fade(c: Color32, a: f32) -> Color32 {
    let bg = theme::bg_window();
    let mix = |x: u8, y: u8| ((y as f32) + ((x as f32) - (y as f32)) * a).round() as u8;
    Color32::from_rgb(mix(c.r(), bg.r()), mix(c.g(), bg.g()), mix(c.b(), bg.b()))
}

#[cfg(test)]
mod inline_link_tests {
    use super::*;

    fn urls(s: &str) -> Vec<(String, String)> {
        parse_inline(s)
            .into_iter()
            .filter_map(|sp| sp.url.map(|u| (sp.text, u)))
            .collect()
    }

    #[test]
    fn markdown_link_keeps_text_and_url() {
        assert_eq!(
            urls("see [the docs](https://example.com/x) now"),
            vec![("the docs".to_string(), "https://example.com/x".to_string())]
        );
    }

    #[test]
    fn bare_url_is_linked() {
        assert_eq!(
            urls("visit https://example.com/a here"),
            vec![(
                "https://example.com/a".to_string(),
                "https://example.com/a".to_string()
            )]
        );
    }

    #[test]
    fn bare_url_trailing_punctuation_trimmed() {
        // The period is prose, not part of the URL.
        assert_eq!(
            urls("go to https://example.com."),
            vec![(
                "https://example.com".to_string(),
                "https://example.com".to_string()
            )]
        );
        // And a closing paren from "(see https://x.io)".
        assert_eq!(
            urls("(see https://x.io)"),
            vec![("https://x.io".to_string(), "https://x.io".to_string())]
        );
    }

    #[test]
    fn text_after_trimmed_url_survives() {
        // Char accounting must be exact: the trailing "." is trimmed from the
        // link yet the following word must still render.
        let spans = parse_inline("a https://x.io. b");
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "a https://x.io. b");
        assert_eq!(
            urls("a https://x.io. b"),
            vec![("https://x.io".to_string(), "https://x.io".to_string())]
        );
    }

    #[test]
    fn non_http_scheme_not_linked() {
        assert!(urls("run file:///etc/passwd please").is_empty());
        assert!(urls("no links here at all").is_empty());
    }

    #[test]
    fn markdown_link_allows_balanced_parens_in_url() {
        assert_eq!(
            urls("[wiki](https://en.wikipedia.org/wiki/Foo_(bar)) done"),
            vec![(
                "wiki".to_string(),
                "https://en.wikipedia.org/wiki/Foo_(bar)".to_string()
            )]
        );
    }

    #[test]
    fn strikethrough_marks_the_run() {
        let spans = parse_inline("keep ~~drop this~~ end");
        let struck: Vec<&str> = spans
            .iter()
            .filter(|s| s.strike)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(struck, vec!["drop this"]);
        // Text outside the ~~ is not struck.
        assert!(spans.iter().any(|s| s.text.contains("keep") && !s.strike));
    }

    #[test]
    fn bare_www_gets_https_href() {
        // Display keeps www.; the href gains a scheme so it opens.
        assert_eq!(
            urls("see www.example.com for more"),
            vec![(
                "www.example.com".to_string(),
                "https://www.example.com".to_string()
            )]
        );
        // "www." with no host is not a link.
        assert!(urls("the www. prefix").is_empty());
    }
}

#[cfg(test)]
mod md_table_tests {
    use super::*;

    #[test]
    fn separator_is_distinguished_from_prose_and_hr() {
        assert!(is_table_separator("|---|---|"));
        assert!(is_table_separator("| :-- | --: | :-: |"));
        assert!(is_table_separator("---|---"));
        // A horizontal rule has no pipe — not a table separator.
        assert!(!is_table_separator("---"));
        // Prose with a pipe is not a separator.
        assert!(!is_table_separator("| a | b |"));
        assert!(!is_table_separator("cats | dogs"));
    }

    #[test]
    fn row_detection_needs_a_pipe() {
        assert!(is_table_row("| a | b |"));
        assert!(is_table_row("a | b"));
        assert!(!is_table_row("just text"));
        assert!(!is_table_row("   "));
    }

    #[test]
    fn cells_split_with_or_without_edge_pipes() {
        assert_eq!(table_cells("| a | b | c |"), vec!["a", "b", "c"]);
        assert_eq!(table_cells("a | b | c"), vec!["a", "b", "c"]);
        assert_eq!(table_cells("|  x  |  y  |"), vec!["x", "y"]);
    }

    #[test]
    fn alignments_parse_from_separator() {
        let a = parse_aligns("| :-- | :-: | --: | --- |");
        assert_eq!(
            a,
            vec![
                ColAlign::Left,
                ColAlign::Center,
                ColAlign::Right,
                ColAlign::Left
            ]
        );
    }
}
