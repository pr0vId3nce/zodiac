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

/// Fold the design tokens into egui's dark visuals: grounds, borders,
/// selection, rounded widgets, and body text as the default color.
pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = BG_WINDOW;
    v.window_fill = BG_CHROME;
    v.extreme_bg_color = BG_RAISED;
    v.faint_bg_color = BG_RAISED;
    v.override_text_color = Some(TEXT_BODY);
    v.window_stroke = Stroke::new(1.0, LINE_BORDER);
    v.selection.bg_fill = BG_SELECTED;
    v.selection.stroke = Stroke::new(1.0, AMBER);

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
    w.inactive.bg_fill = BG_RAISED;
    w.inactive.weak_bg_fill = BG_RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, LINE_BORDER_STRONG);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    w.hovered.bg_stroke = Stroke::new(1.0, LINE_HOVER);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.hovered.weak_bg_fill = BG_SELECTED;
    w.active.bg_stroke = Stroke::new(1.0, LINE_HOVER);
    w.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.active.weak_bg_fill = BG_SELECTED;

    ctx.set_visuals(v);
}
