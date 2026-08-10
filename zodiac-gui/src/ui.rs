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

/// Things the UI wants the app to do after a frame. Applied by `redraw`
/// once egui's borrows are released.
pub enum UiAction {
    /// Focus the pane at this index (T_FOCUS + active), staying on the grid.
    Focus(usize),
    /// Focus the pane and open the focused-pane screen.
    Open(usize),
    /// Return to the Observatory.
    Back,
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
pub fn build(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    title_bar(ui, d);
    match d.screen {
        Screen::Observatory => observatory(ui, d, actions),
        Screen::Focused => focused(ui, d, actions),
    }
}

/// The focused-pane screen (task #24 shell; the full sidebar + transcript +
/// activity rail land in task #26). For now: a pane header with a back
/// affordance and a transcript-tail preview from live state. Esc returns.
fn focused(root: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    if root.input(|i| i.key_pressed(egui::Key::Escape)) {
        actions.push(UiAction::Back);
    }
    egui::CentralPanel::default()
        .frame(
            Frame::NONE
                .fill(theme::BG_WINDOW)
                .inner_margin(Margin::same(0)),
        )
        .show(root, |ui| {
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
                            if ui
                                .button(RichText::new("← observatory").size(12.5))
                                .clicked()
                            {
                                actions.push(UiAction::Back);
                            }
                        });
                    });
                });
            // Body: transcript-tail preview (full transcript/terminal: #26).
            egui::Frame::NONE
                .inner_margin(Margin::same(18))
                .show(ui, |ui| {
                    let tail: Vec<&String> =
                        ps.map(|s| s.tail.iter().collect()).unwrap_or_default();
                    if tail.is_empty() {
                        ui.label(
                            RichText::new("No transcript yet.")
                                .color(theme::TEXT_FAINT)
                                .size(13.5),
                        );
                    } else {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for line in tail {
                                ui.add(
                                    Label::new(
                                        RichText::new(line)
                                            .color(theme::TEXT_BODY)
                                            .size(13.0)
                                            .monospace(),
                                    )
                                    .truncate(),
                                );
                            }
                        });
                    }
                });
        });
}

/// The 52px title bar: amber mark, "zodiac", session chip, host vitals.
fn title_bar(root: &mut egui::Ui, d: &UiData) {
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
                    if let Some(h) = d.state.and_then(|s| s.host.as_ref()) {
                        vitals(ui, h);
                    }
                });
            });
        });
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
