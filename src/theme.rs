//! The "brass instrument" design language: night navy surfaces, brass
//! hairlines, gold-soft highlights — the same palette as the astrolabe
//! phone app, so the TUI and the phone read as one instrument. Status
//! colors are fixed across themes (they carry meaning); everything else
//! swaps with the `theme` setting.

// gold_soft/phosphor come into use as later reskin phases land.
#[allow(dead_code)]
pub struct Palette {
    pub name: &'static str,
    /// Brass accent: hairlines, sigils, reticle brackets, headers.
    pub accent: (u8, u8, u8),
    /// Softened gold for questions and highlighted prose.
    pub gold_soft: (u8, u8, u8),
    pub fg: (u8, u8, u8),
    pub dim: (u8, u8, u8),
    /// Faintest hints (keybinding lines, placeholders).
    pub faint: (u8, u8, u8),
    /// Painted card fill (flattened against the deep background) and its
    /// slightly brighter selected variant.
    pub card: (u8, u8, u8),
    pub card_sel: (u8, u8, u8),
    /// 1px card edge line.
    pub edge: (u8, u8, u8),
    /// Terminal prompt green.
    pub phosphor: (u8, u8, u8),
}

pub const THEMES: &[&str] = &["night", "oled-orange", "oled-green"];

const NIGHT: Palette = Palette {
    name: "night",
    accent: (212, 175, 55),
    gold_soft: (245, 230, 184),
    fg: (230, 232, 240),
    dim: (127, 139, 163),
    faint: (90, 100, 120),
    card: (13, 17, 34),
    card_sel: (20, 26, 50),
    edge: (36, 46, 85),
    phosphor: (35, 209, 139),
};

const OLED_ORANGE: Palette = Palette {
    name: "oled-orange",
    accent: (255, 159, 10),
    gold_soft: (255, 217, 160),
    fg: (242, 233, 222),
    dim: (158, 140, 117),
    faint: (112, 98, 80),
    card: (15, 13, 9),
    card_sel: (26, 22, 14),
    edge: (61, 42, 14),
    phosphor: (255, 159, 10),
};

const OLED_GREEN: Palette = Palette {
    name: "oled-green",
    accent: (35, 209, 139),
    gold_soft: (169, 240, 209),
    fg: (224, 242, 233),
    dim: (115, 160, 135),
    faint: (78, 110, 92),
    card: (11, 15, 12),
    card_sel: (18, 25, 20),
    edge: (16, 57, 31),
    phosphor: (35, 209, 139),
};

pub fn palette(name: &str) -> &'static Palette {
    match name {
        "oled-orange" => &OLED_ORANGE,
        "oled-green" => &OLED_GREEN,
        _ => &NIGHT,
    }
}

/// Status rail colors, indexed like `card_status`'s accent index:
/// needs_input, thinking, working, finished, idle.
pub const STATUS_RAIL: [(u8, u8, u8); 5] = [
    (248, 113, 113),
    (175, 95, 255),
    (251, 146, 60),
    (52, 211, 153),
    (82, 82, 91),
];

/// Matching lighter tints for status words in text.
pub const STATUS_TEXT: [(u8, u8, u8); 5] = [
    (252, 165, 165),
    (175, 143, 255),
    (253, 186, 116),
    (110, 231, 183),
    (127, 139, 163),
];

pub fn color(rgb: (u8, u8, u8)) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2)
}
