//! egui UI for the native GUI (ADR 0006). This is the first screen of the
//! rebuild — an Observatory-lite that lists the session's panes from live
//! state — and, more importantly, the proof that the egui layer paints into
//! our wgpu surface with the design tokens and reports interactions back.
//! The richer screens (full Observatory cards, focused pane, oracle,
//! palette, settings, dialogs) grow from here per tasks #24–#29.

use egui::{Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke};
use zodiac::client_core::CPane;
use zodiac::protocol::SessionState;

use crate::theme;

/// Things the UI wants the app to do after a frame. Applied by `redraw`
/// once egui's borrows are released.
pub enum UiAction {
    /// Focus the pane at this index.
    Focus(usize),
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
}

impl UiData<'_> {
    /// Server status string for a pane (`idle` when unknown).
    fn status(&self, p: &CPane) -> &str {
        self.state
            .and_then(|s| s.panes.iter().find(|sp| sp.id == p.id))
            .map(|sp| sp.status.as_str())
            .unwrap_or("idle")
    }
}

/// Roman numeral for a 1-based index (the sigil in the handoff's card tiles).
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

/// Build the frame's UI, pushing any resulting actions. egui 0.36 hands the
/// integration a root `&mut Ui`; panels are shown into it.
pub fn build(ui: &mut egui::Ui, d: &UiData, actions: &mut Vec<UiAction>) {
    title_bar(ui, d);
    observatory(ui, d, actions);
}

/// The 52px title bar: amber mark, "zodiac", the session chip.
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
            });
        });
}

/// The 22px amber-gradient mark with a `❯` — approximated for now with a
/// flat amber tile (the gradient mesh lands with the app-shell task #24).
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

/// Observatory-lite: pane count + status tally, then a clickable list of
/// pane cards drawn from live state.
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
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, p) in d.panes.iter().enumerate() {
                    pane_card(ui, d, i, p, actions);
                    ui.add_space(10.0);
                }
            });
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
            counts[theme::status_index(d.status(p), false)] += 1;
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

/// One pane card: sigil tile, name, agent, status pill; click to focus.
fn pane_card(ui: &mut egui::Ui, d: &UiData, i: usize, p: &CPane, actions: &mut Vec<UiAction>) {
    let status = d.status(p);
    let si = theme::status_index(status, false);
    let sel = i == d.active;
    let fill = if sel {
        theme::BG_SELECTED
    } else if si == 0 {
        theme::BG_CARD_ALERT
    } else if si == 4 {
        theme::BG_CARD_IDLE
    } else {
        theme::BG_CARD
    };
    let border = if si == 0 {
        fade(theme::STATUS_RAIL[0], 0.25)
    } else {
        theme::LINE_BORDER
    };
    let inner = Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                sigil_tile(ui, i + 1, si);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(&p.name)
                            .color(if si == 4 {
                                theme::TEXT_DIM
                            } else {
                                theme::TEXT_PRIMARY
                            })
                            .size(15.0)
                            .strong(),
                    );
                    let sub = if p.is_agent() {
                        p.kind.clone()
                    } else {
                        "shell".into()
                    };
                    ui.label(
                        RichText::new(sub)
                            .color(theme::TEXT_FAINT)
                            .size(11.5)
                            .monospace(),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    status_pill(ui, si, theme::STATUS_WORD[si]);
                });
            });
        });
    if inner
        .response
        .interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        actions.push(UiAction::Focus(i));
    }
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
