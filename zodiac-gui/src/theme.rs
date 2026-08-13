//! egui design tokens for the native GUI (ADR 0006), ported from the
//! "Zodiac TUI → GUI Overhaul" handoff. Ground/chrome/text/accent tokens
//! plus the five status colors carried verbatim from the TUI's
//! `src/theme.rs` (`STATUS_RAIL` / `STATUS_TEXT`), with the handoff's two
//! GUI overrides (thinking rail → the violet `#a874f0`, idle text
//! → `#7f9296`). `apply()` folds them into egui's dark `Visuals`.

use egui::{Color32, CornerRadius, Stroke, Visuals};

// --- Ground & structure ---------------------------------------------------
pub const BG_WINDOW: Color32 = Color32::from_rgb(0x09, 0x10, 0x12);
pub const BG_CHROME: Color32 = Color32::from_rgb(0x0b, 0x14, 0x16);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x0a, 0x12, 0x14);
pub const BG_CARD: Color32 = Color32::from_rgb(0x0d, 0x16, 0x18);
pub const BG_CARD_IDLE: Color32 = Color32::from_rgb(0x0a, 0x10, 0x12);
pub const BG_CARD_ALERT: Color32 = Color32::from_rgb(0x10, 0x0f, 0x14);
pub const BG_RAISED: Color32 = Color32::from_rgb(0x0e, 0x17, 0x19);
pub const BG_SELECTED: Color32 = Color32::from_rgb(0x14, 0x1f, 0x22);

pub const LINE_HAIRLINE: Color32 = Color32::from_rgb(0x16, 0x22, 0x25);
pub const LINE_BORDER: Color32 = Color32::from_rgb(0x1b, 0x28, 0x2b);
pub const LINE_BORDER_STRONG: Color32 = Color32::from_rgb(0x24, 0x34, 0x37);
pub const LINE_HOVER: Color32 = Color32::from_rgb(0x3a, 0x4f, 0x53);

// --- Text tiers -----------------------------------------------------------
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xe8, 0xee, 0xf0);
pub const TEXT_BODY: Color32 = Color32::from_rgb(0xd4, 0xde, 0xe0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x92, 0xa4, 0xa8);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5f, 0x72, 0x76);
pub const TEXT_GHOST: Color32 = Color32::from_rgb(0x4d, 0x5f, 0x63);

// --- Accents --------------------------------------------------------------
pub const AMBER: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x48);
pub const AMBER_HOVER: Color32 = Color32::from_rgb(0xf0, 0xc6, 0x7e);
pub const VIOLET: Color32 = Color32::from_rgb(0xa8, 0x74, 0xf0);
pub const VIOLET_TEXT: Color32 = Color32::from_rgb(0xc5, 0xa3, 0xf7);
/// Claude Code's warm thinking orange — the rotating status word and the
/// `⏺` recap bullet. `ORANGE_DIM` is the softer tone for thinking prose.
pub const ORANGE: Color32 = Color32::from_rgb(0xff, 0x8c, 0x2b);
pub const ORANGE_DIM: Color32 = Color32::from_rgb(0xc9, 0x8a, 0x54);

// --- Status colors --------------------------------------------------------
// Index order: needs_input, thinking, working, finished, idle — the same
// order as `card_status()`'s accent index in the TUI.
pub const STATUS_RAIL: [Color32; 5] = [
    Color32::from_rgb(0xf8, 0x71, 0x71), // needs you
    Color32::from_rgb(0xa8, 0x74, 0xf0), // thinking (GUI override)
    Color32::from_rgb(0xfb, 0x92, 0x3c), // working
    Color32::from_rgb(0x34, 0xd3, 0x99), // finished
    Color32::from_rgb(0x52, 0x52, 0x5b), // idle
];
pub const STATUS_TEXT: [Color32; 5] = [
    Color32::from_rgb(0xfc, 0xa5, 0xa5),
    Color32::from_rgb(0xc5, 0xa3, 0xf7),
    Color32::from_rgb(0xfd, 0xba, 0x74),
    Color32::from_rgb(0x6e, 0xe7, 0xb3),
    Color32::from_rgb(0x7f, 0x92, 0x96), // idle (GUI override)
];

/// Human-facing status word for a status index.
pub const STATUS_WORD: [&str; 5] = ["needs you", "thinking", "working", "finished", "idle"];

/// Map a server status string (+ thinking flag) to the 0..5 status index.
/// The server emits `needs_input` / `working` / `done` / `idle`; `thinking`
/// is a flag layered over `working`, and `done` reads as `finished`.
pub fn status_index(status: &str, thinking: bool) -> usize {
    match status {
        "needs_input" => 0,
        "working" => {
            if thinking {
                1
            } else {
                2
            }
        }
        "done" | "finished" => 3,
        _ => 4,
    }
}

// --- Theme palette (the `theme` config key) ------------------------------
// Ground colors + accent vary by theme; text/line/status stay constant (they
// read on every ground). The active palette lives in a thread-local (egui is
// single-threaded) that `apply()` sets and the `bg_*()` / `accent()`
// accessors read, so a theme change takes effect live everywhere.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg_window: Color32,
    pub bg_chrome: Color32,
    pub bg_panel: Color32,
    pub bg_card: Color32,
    pub bg_card_idle: Color32,
    pub bg_card_alert: Color32,
    pub bg_raised: Color32,
    pub bg_selected: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
}

/// "slate · brass" — the default (the module constants).
const NIGHT: Palette = Palette {
    bg_window: BG_WINDOW,
    bg_chrome: BG_CHROME,
    bg_panel: BG_PANEL,
    bg_card: BG_CARD,
    bg_card_idle: BG_CARD_IDLE,
    bg_card_alert: BG_CARD_ALERT,
    bg_raised: BG_RAISED,
    bg_selected: BG_SELECTED,
    accent: AMBER,
    accent_hover: AMBER_HOVER,
};

/// True-black grounds shared by the OLED themes (accent filled in per theme).
///
/// Every *background* surface is `#000` — window, chrome (top bar, sidebar)
/// and panel (the rail) alike. Near-blacks defeat the point: on an OLED those
/// pixels are lit, and against a black window they read as a grey wash rather
/// than as structure. Structure comes from the hairline separators the focused
/// view draws instead. Cards and the raised/selected states keep a faint lift
/// so an interactive surface is still distinguishable from the ground.
const fn oled(accent: Color32, accent_hover: Color32) -> Palette {
    Palette {
        bg_window: Color32::BLACK,
        bg_chrome: Color32::BLACK,
        bg_panel: Color32::BLACK,
        bg_card: Color32::from_rgb(0x0d, 0x0d, 0x10),
        bg_card_idle: Color32::BLACK,
        bg_card_alert: Color32::from_rgb(0x14, 0x0a, 0x0a),
        bg_raised: Color32::from_rgb(0x12, 0x12, 0x16),
        bg_selected: Color32::from_rgb(0x1c, 0x1c, 0x22),
        accent,
        accent_hover,
    }
}

/// The palette for a `theme` config value.
pub fn palette_for(name: &str) -> Palette {
    match name {
        "oled-orange" => oled(
            Color32::from_rgb(0xff, 0x87, 0x00),
            Color32::from_rgb(0xff, 0xa9, 0x4d),
        ),
        "oled-green" => oled(
            Color32::from_rgb(0x34, 0xd3, 0x99),
            Color32::from_rgb(0x6e, 0xe7, 0xb3),
        ),
        _ => NIGHT,
    }
}

thread_local! {
    static CUR: std::cell::Cell<Palette> = const { std::cell::Cell::new(NIGHT) };
}

fn cur() -> Palette {
    CUR.with(|c| c.get())
}

pub fn bg_window() -> Color32 {
    cur().bg_window
}
pub fn bg_chrome() -> Color32 {
    cur().bg_chrome
}
pub fn bg_panel() -> Color32 {
    cur().bg_panel
}
pub fn bg_card() -> Color32 {
    cur().bg_card
}
pub fn bg_card_idle() -> Color32 {
    cur().bg_card_idle
}
pub fn bg_card_alert() -> Color32 {
    cur().bg_card_alert
}
pub fn bg_raised() -> Color32 {
    cur().bg_raised
}
pub fn bg_selected() -> Color32 {
    cur().bg_selected
}
pub fn accent() -> Color32 {
    cur().accent
}
pub fn accent_hover() -> Color32 {
    cur().accent_hover
}

/// Fold the design tokens into egui's dark visuals for the given `theme`:
/// grounds, borders, selection, rounded widgets, body text as default color.
pub fn apply(ctx: &egui::Context, theme: &str) {
    let p = palette_for(theme);
    CUR.with(|c| c.set(p));
    let mut v = Visuals::dark();
    v.panel_fill = p.bg_window;
    v.window_fill = p.bg_chrome;
    v.extreme_bg_color = p.bg_raised;
    v.faint_bg_color = p.bg_raised;
    v.override_text_color = Some(TEXT_BODY);
    v.window_stroke = Stroke::new(1.0, LINE_BORDER);
    v.selection.bg_fill = p.bg_selected;
    v.selection.stroke = Stroke::new(1.0, p.accent);

    let cr = CornerRadius::same(8);
    let w = &mut v.widgets;
    for wv in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        wv.corner_radius = cr;
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, LINE_HAIRLINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    w.inactive.bg_fill = p.bg_raised;
    w.inactive.weak_bg_fill = p.bg_raised;
    w.inactive.bg_stroke = Stroke::new(1.0, LINE_BORDER_STRONG);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    w.hovered.bg_stroke = Stroke::new(1.0, LINE_HOVER);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.hovered.weak_bg_fill = p.bg_selected;
    w.active.bg_stroke = Stroke::new(1.0, LINE_HOVER);
    w.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.active.weak_bg_fill = p.bg_selected;

    ctx.set_visuals(v);
}

/// Install `ttf` as egui's proportional *and* monospace family, so the whole
/// GUI renders in one face (JetBrains Mono Nerd Font by default). `fallbacks`
/// (name, bytes) are inserted just after it — broad Nerd/symbol/emoji coverage
/// for terminal prompts — and egui's built-in faces are kept after those.
pub fn set_fonts(ctx: &egui::Context, ttf: Vec<u8>, fallbacks: Vec<(String, Vec<u8>)>) {
    use std::sync::Arc;
    let mut defs = egui::FontDefinitions::default();
    defs.font_data.insert(
        "zodiac".to_owned(),
        Arc::new(egui::FontData::from_owned(ttf)),
    );
    let mut names = vec!["zodiac".to_owned()];
    for (name, bytes) in fallbacks {
        defs.font_data
            .insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
        names.push(name);
    }
    // Put our faces at the front (primary, then fallbacks), keeping egui's
    // built-ins after for anything still uncovered.
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = defs.families.entry(fam).or_default();
        for (i, name) in names.iter().enumerate() {
            list.insert(i, name.clone());
        }
    }
    ctx.set_fonts(defs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oled_themes_ground_on_true_black() {
        // The whole point of an OLED theme is unlit pixels; a near-black
        // background is both lit and, against a black window, a grey wash.
        for name in ["oled-orange", "oled-green"] {
            let p = palette_for(name);
            for (what, c) in [
                ("window", p.bg_window),
                ("chrome", p.bg_chrome),
                ("panel", p.bg_panel),
                ("idle card", p.bg_card_idle),
            ] {
                assert_eq!(c, Color32::BLACK, "{name}: {what} must be true black");
            }
            // …but the themes still differ from each other, and from night.
            assert_ne!(p.accent, palette_for("night").accent);
        }
        assert_ne!(
            palette_for("oled-orange").accent,
            palette_for("oled-green").accent
        );
        // The default theme is unaffected: its chrome still lifts off the
        // window, which is where its structure comes from.
        let night = palette_for("night");
        assert_ne!(night.bg_chrome, night.bg_window);
    }
}
