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

/// Which screen the GUI is showing (app-shell router, task #24).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The pane-card home (`draw_home`'s successor).
    Observatory,
    /// One focused pane (the active pane).
    Focused,
}

/// A modal overlay open over the current screen.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    None,
    /// Command palette (⌘K): fuzzy pane/action jump.
    Palette,
    /// Settings dialog (⌘,): grouped config editor.
    Settings,
    /// Pair-a-phone dialog: astrolabe pairing QR.
    Pairing,
}

/// Mutable UI state egui edits in place across frames (buffers + overlays).
#[derive(Default)]
pub struct UiState {
    /// The focused agent pane's composer buffer.
    pub composer: String,
    /// Which modal overlay is open.
    pub overlay: Overlay,
    /// Command-palette query + selected row.
    pub palette_query: String,
    pub palette_sel: usize,
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
    /// Spawn a new shell pane.
    NewShell,
    /// Spawn a new claude agent pane.
    NewAgent,
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
    title_bar(ui, d, st);
    match d.screen {
        Screen::Observatory => observatory(ui, d, actions),
        Screen::Focused => focused(ui, d, &mut st.composer, actions),
    }
    match st.overlay {
        Overlay::Palette => palette(ui, d, st, actions),
        Overlay::Settings => settings_dialog(ui, st, settings, actions),
        Overlay::Pairing => pairing_dialog(ui, d, st),
        Overlay::None => {}
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
fn pairing_dialog(ui: &mut egui::Ui, d: &UiData, st: &mut UiState) {
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
                .fill(theme::BG_CARD)
                .stroke(Stroke::new(1.0, fade(theme::AMBER, 0.3)))
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
                .fill(theme::BG_CARD)
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(600.0);
                    // Query row.
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⌕").color(theme::AMBER).size(16.0));
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
                                theme::BG_SELECTED
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(CornerRadius::same(9))
                            .inner_margin(Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(roman(idx + 1))
                                            .color(theme::AMBER)
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
fn focused(root: &mut egui::Ui, d: &UiData, composer: &mut String, actions: &mut Vec<UiAction>) {
    // Esc closes only when the composer isn't focused (so it can clear text).
    if root.input(|i| i.key_pressed(egui::Key::Escape)) && root.memory(|m| m.focused().is_none()) {
        actions.push(UiAction::Back);
    }
    egui::Panel::left("sidebar")
        .exact_size(268.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(theme::BG_CHROME)
                .inner_margin(Margin::same(10)),
        )
        .show(root, |ui| sidebar(ui, d, actions));
    egui::Panel::right("rail")
        .exact_size(288.0)
        .resizable(false)
        .frame(
            Frame::NONE
                .fill(theme::BG_PANEL)
                .inner_margin(Margin::same(14)),
        )
        .show(root, |ui| activity_rail(ui, d));
    egui::CentralPanel::default()
        .frame(
            Frame::NONE
                .fill(theme::BG_WINDOW)
                .inner_margin(Margin::same(0)),
        )
        .show(root, |ui| main_pane(ui, d, composer, actions));
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
                        theme::BG_SELECTED
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(CornerRadius::same(9))
                    .inner_margin(Margin::symmetric(10, 9))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(roman(i + 1))
                                    .color(if sel {
                                        theme::AMBER
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
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
    if ui
        .button(
            RichText::new("+ new pane")
                .color(theme::TEXT_DIM)
                .size(13.0),
        )
        .clicked()
    {
        actions.push(UiAction::NewShell);
    }
    if ui
        .button(
            RichText::new("← observatory")
                .color(theme::TEXT_DIM)
                .size(13.0),
        )
        .clicked()
    {
        actions.push(UiAction::Back);
    }
}

/// The main column: pane header, then transcript or terminal, then composer.
fn main_pane(ui: &mut egui::Ui, d: &UiData, composer: &mut String, actions: &mut Vec<UiAction>) {
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
        .fill(theme::BG_CHROME)
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                sigil_tile(ui, d.active + 1, si);
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
                    // transcript | terminal segmented toggle.
                    let is_agent = p.is_agent();
                    let term = d.term_active || !is_agent;
                    if seg(ui, "terminal", term) && !term && is_agent {
                        actions.push(UiAction::ToggleTerm(p.id));
                    }
                    if seg(ui, "transcript", !term) && term && is_agent {
                        actions.push(UiAction::ToggleTerm(p.id));
                    }
                });
            });
        });
    // Body.
    let show_term = d.term_active || !p.is_agent();
    if show_term {
        terminal_view(ui, p);
    } else {
        // Composer + approvals pinned to the bottom; transcript fills above.
        egui::Panel::bottom("composer")
            .resizable(false)
            .frame(Frame::NONE.fill(theme::BG_CHROME))
            .show(ui, |ui| {
                approvals(ui, p, actions);
                composer_bar(ui, p, composer, actions);
            });
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(theme::BG_WINDOW))
            .show(ui, |ui| transcript_view(ui, p));
    }
}

/// Native agent transcript: user/assistant/tool/error turns + streaming tail.
fn transcript_view(ui: &mut egui::Ui, p: &CPane) {
    use zodiac::client_core::ARole;
    egui::ScrollArea::vertical()
        .id_salt("transcript")
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(12.0);
            let inner = 22.0;
            for (role, text) in &p.agent.log {
                match role {
                    ARole::User => turn_user(ui, text, inner),
                    ARole::Assistant => turn_agent(ui, "✦", theme::VIOLET, text, inner),
                    ARole::Tool => turn_tool(ui, text, inner),
                    ARole::Error => turn_agent(ui, "✗", theme::STATUS_RAIL[0], text, inner),
                }
                ui.add_space(10.0);
            }
            if p.agent.thinking {
                indent(ui, inner, |ui| {
                    ui.label(
                        RichText::new("● ● ●  thinking")
                            .color(theme::VIOLET_TEXT)
                            .size(13.0),
                    );
                });
            }
            if !p.agent.stream.is_empty() {
                turn_agent(ui, "✦", theme::VIOLET, &p.agent.stream, inner);
            }
            ui.add_space(12.0);
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
    ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
        ui.add_space(inner);
        Frame::NONE
            .fill(theme::BG_SELECTED)
            .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.64);
                ui.label(RichText::new(text).color(theme::TEXT_BODY).size(14.0));
            });
    });
}

/// Assistant/error turn: an avatar glyph + prose column.
fn turn_agent(ui: &mut egui::Ui, glyph: &str, col: Color32, text: &str, inner: f32) {
    ui.horizontal_top(|ui| {
        ui.add_space(inner - 18.0);
        ui.label(RichText::new(glyph).color(col).size(15.0));
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            ui.label(RichText::new(text).color(theme::TEXT_BODY).size(14.5));
        });
    });
}

/// Tool-call: a compact mono card.
fn turn_tool(ui: &mut egui::Ui, text: &str, inner: f32) {
    indent(ui, inner, |ui| {
        Frame::NONE
            .fill(theme::BG_CHROME)
            .stroke(Stroke::new(1.0, theme::LINE_BORDER))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⏺").color(theme::AMBER).size(12.0));
                    ui.add_space(6.0);
                    ui.add(
                        Label::new(
                            RichText::new(text)
                                .color(theme::TEXT_DIM)
                                .size(12.0)
                                .monospace(),
                        )
                        .truncate(),
                    );
                });
            });
    });
}

/// The composer: a live one-line prompt editor. Enter (or Send) submits to
/// the agent as T_AGENT_INPUT and clears the buffer.
fn composer_bar(ui: &mut egui::Ui, p: &CPane, composer: &mut String, actions: &mut Vec<UiAction>) {
    Frame::NONE
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut submit = false;
            Frame::NONE
                .fill(theme::BG_RAISED)
                .stroke(Stroke::new(1.0, theme::LINE_BORDER_STRONG))
                .corner_radius(CornerRadius::same(11))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let edit = egui::TextEdit::singleline(composer)
                            .frame(Frame::NONE)
                            .desired_width(ui.available_width() - 64.0)
                            .hint_text("message the agent…")
                            .font(egui::FontId::proportional(14.0))
                            .text_color(theme::TEXT_BODY);
                        let resp = ui.add(edit);
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submit = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if amber_button(ui, "Send").clicked() {
                                submit = true;
                            }
                        });
                    });
                });
            if submit && !composer.trim().is_empty() {
                actions.push(UiAction::SendAgent(p.id));
            }
        });
}

/// Pending-permission card: the tool + input, with Approve / Deny. Answering
/// writes a T_PERM_RESP for the pane exactly as a keystroke would.
fn approvals(ui: &mut egui::Ui, p: &CPane, actions: &mut Vec<UiAction>) {
    let Some(req) = p.agent.perms.first() else {
        return;
    };
    Frame::NONE
        .fill(fade(theme::STATUS_RAIL[0], 0.10))
        .stroke(Stroke::new(1.0, fade(theme::STATUS_RAIL[0], 0.28)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .outer_margin(Margin::symmetric(18, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let tool = req
                .display_name
                .clone()
                .unwrap_or_else(|| req.tool_name.clone());
            ui.label(
                RichText::new(format!("needs you · {tool}"))
                    .color(theme::STATUS_TEXT[0])
                    .size(13.5)
                    .strong(),
            );
            let arg = zodiac::client_core::tool_compact(&req.input);
            if !arg.is_empty() {
                ui.add(
                    Label::new(
                        RichText::new(clip(&arg, 120))
                            .color(theme::TEXT_DIM)
                            .size(12.0)
                            .monospace(),
                    )
                    .truncate(),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if amber_button(ui, "Approve").clicked() {
                    actions.push(UiAction::Perm(p.id, true));
                }
                ui.add_space(8.0);
                if ui.button(RichText::new("Deny").size(13.0)).clicked() {
                    actions.push(UiAction::Perm(p.id, false));
                }
            });
        });
}

/// Terminal-mode body: the pane's live vt100 screen, painted cell-by-cell
/// as a fixed monospace grid (bg quads + glyphs + block cursor) reusing
/// `palette::cell_colors`. Kitty graphics are not composited here yet — that
/// needs the GPU grid renderer drawn into egui (task #26b, follow-on).
fn terminal_view(ui: &mut egui::Ui, p: &CPane) {
    use crate::palette::{cell_colors, CellStyle};
    let c32 = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
    let font = egui::FontId::monospace(13.0);
    let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let screen = p.parser.screen();
    let (rows, cols) = screen.size();
    egui::Frame::NONE
        .fill(c32(crate::palette::DEFAULT_BG))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::both()
                .id_salt("term")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(cols as f32 * cw, rows as f32 * ch),
                        Sense::hover(),
                    );
                    let painter = ui.painter_at(rect);
                    let o = rect.min;
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
                });
        });
}

/// A small segmented-control button; returns true when clicked.
fn seg(ui: &mut egui::Ui, label: &str, on: bool) -> bool {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(if on {
                theme::BG_CHROME
            } else {
                theme::TEXT_DIM
            })
            .size(12.0),
    )
    .fill(if on { theme::AMBER } else { theme::BG_RAISED })
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
                .fill(theme::BG_CARD)
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
fn title_bar(root: &mut egui::Ui, d: &UiData, st: &mut UiState) {
    egui::Panel::top("titlebar")
        .exact_size(52.0)
        .frame(
            Frame::NONE
                .fill(theme::BG_CHROME)
                .inner_margin(Margin::symmetric(18, 8)),
        )
        .show(root, |ui| {
            ui.horizontal_centered(|ui| {
                mark(ui);
                ui.add_space(8.0);
                ui.label(
                    RichText::new("zodiac")
                        .color(theme::TEXT_PRIMARY)
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(12.0);
                session_chip(ui, d.session);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if chrome_btn(ui, "settings") {
                        st.overlay = Overlay::Settings;
                    }
                    if chrome_btn(ui, "pair phone") {
                        st.overlay = Overlay::Pairing;
                    }
                    if chrome_btn(ui, "⌘K find pane") {
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

/// A bordered chrome button (title-bar controls). Returns true on click.
fn chrome_btn(ui: &mut egui::Ui, label: &str) -> bool {
    let btn = egui::Button::new(RichText::new(label).color(theme::TEXT_DIM).size(12.5))
        .fill(theme::BG_CHROME)
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
        .rect_filled(rect, CornerRadius::same(6), theme::AMBER);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "❯",
        egui::FontId::proportional(13.0),
        theme::BG_CHROME,
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
                .fill(theme::BG_WINDOW)
                .inner_margin(Margin::same(20)),
        )
        .show(root, |ui| {
            summary_strip(ui, d, actions);
            ui.add_space(14.0);
            if d.panes.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(
                        RichText::new("connecting…")
                            .color(theme::TEXT_FAINT)
                            .size(15.0),
                    );
                });
                return;
            }
            card_grid(ui, d, actions);
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
            .color(theme::BG_CHROME)
            .size(13.0)
            .strong(),
    )
    .fill(theme::AMBER)
    .corner_radius(CornerRadius::same(8));
    ui.add(btn)
}

/// Responsive card grid: as many ~300px columns as fit, row-major, so card
/// order matches pane order (and thus the focus index).
fn card_grid(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    let gap = 14.0;
    let avail = ui.available_width();
    let min_card = 300.0;
    let cols = (((avail + gap) / (min_card + gap)).floor() as usize).max(1);
    let card_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).max(min_card.min(avail));
    egui::ScrollArea::vertical().show(ui, |ui| {
        let items: Vec<(usize, &CPane)> = d.panes.iter().enumerate().collect();
        for row in items.chunks(cols) {
            ui.horizontal_top(|ui| {
                for (i, p) in row {
                    ui.allocate_ui(egui::vec2(card_w, 0.0), |ui| {
                        ui.set_width(card_w);
                        pane_card(ui, d, *i, p, actions);
                    });
                    ui.add_space(gap);
                }
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
        theme::BG_SELECTED
    } else if si == 0 {
        theme::BG_CARD_ALERT
    } else if idle {
        theme::BG_CARD_IDLE
    } else {
        theme::BG_CARD
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
                sigil_tile(ui, i + 1, si);
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
fn sigil_tile(ui: &mut egui::Ui, n1: usize, si: usize) {
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
        theme::AMBER
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        roman(n1),
        egui::FontId::proportional(13.0),
        col,
    );
}

/// Blend a status color toward the ground at the given alpha, so pills read
/// as a tint rather than a solid fill (the handoff's `#f8717118` style).
fn fade(c: Color32, a: f32) -> Color32 {
    let bg = theme::BG_WINDOW;
    let mix = |x: u8, y: u8| ((y as f32) + ((x as f32) - (y as f32)) * a).round() as u8;
    Color32::from_rgb(mix(c.r(), bg.r()), mix(c.g(), bg.g()), mix(c.b(), bg.b()))
}
