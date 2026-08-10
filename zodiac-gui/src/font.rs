//! Monospace font selection + cell metrics. The GUI loads the system font
//! database (fontconfig dirs) and picks a monospace family: the
//! `ZODIAC_GUI_FONT` env var wins, then `fc-match monospace`, then the
//! first monospaced face in the database. Cell width is the measured
//! advance of the chosen face at the configured pixel size — the same
//! number cosmic-text uses to lay rows out, so background quads and glyphs
//! stay aligned across a 200-column row (S3 lesson).

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Wrap};

pub const FONT_PX: f32 = 15.0;
pub const LINE_FACTOR: f32 = 1.30;

pub struct Fonts {
    pub system: FontSystem,
    pub family: String,
    /// Proportional (sans-serif) family for agent transcripts (roadmap
    /// 4.5): prose reads better proportionally, while grid panes stay
    /// monospace. `ZODIAC_GUI_UI_FONT` overrides; else `fc-match
    /// sans-serif`; else the monospace family (always a safe fallback).
    pub ui_family: String,
}

impl Fonts {
    pub fn load() -> Self {
        let system = FontSystem::new();
        let family = pick_family(&system);
        let ui_family = pick_ui_family(&system, &family);
        Self {
            system,
            family,
            ui_family,
        }
    }

    /// (cell_w, cell_h) in physical px for `FONT_PX * scale`.
    pub fn cell_size(&mut self, scale: f32) -> (f32, f32) {
        let font_px = FONT_PX * scale;
        let line_h = (font_px * LINE_FACTOR).round();
        let mut buf = Buffer::new(&mut self.system, Metrics::new(font_px, line_h));
        buf.set_size(Some(4096.0), Some(line_h * 2.0));
        buf.set_wrap(Wrap::None);
        let mut attrs = Attrs::new();
        let family = self.family.clone();
        attrs.family = Family::Name(&family);
        buf.set_text("MM", &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.system, false);
        let advance = buf
            .layout_runs()
            .next()
            .and_then(|run| {
                let g = run.glyphs;
                if g.len() >= 2 {
                    Some(g[1].x - g[0].x)
                } else {
                    g.first().map(|g| g.w)
                }
            })
            .filter(|a| *a > 1.0)
            .unwrap_or(font_px * 0.6);
        (advance, line_h)
    }
}

fn pick_family(fs: &FontSystem) -> String {
    let db = fs.db();
    let exists = |name: &str| {
        db.faces()
            .any(|f| f.families.iter().any(|(fam, _)| fam == name))
    };
    if let Ok(name) = std::env::var("ZODIAC_GUI_FONT") {
        if exists(&name) {
            return name;
        }
        eprintln!("zodiac-gui: ZODIAC_GUI_FONT '{name}' not found, falling back");
    }
    if let Some(name) = fc_match_mono() {
        if exists(&name) {
            return name;
        }
    }
    // Last resort: first monospaced face fontdb knows about.
    db.faces()
        .find(|f| f.monospaced)
        .and_then(|f| f.families.first().map(|(fam, _)| fam.clone()))
        .unwrap_or_else(|| "monospace".into())
}

fn pick_ui_family(fs: &FontSystem, mono_fallback: &str) -> String {
    let db = fs.db();
    let exists = |name: &str| {
        db.faces()
            .any(|f| f.families.iter().any(|(fam, _)| fam == name))
    };
    if let Ok(name) = std::env::var("ZODIAC_GUI_UI_FONT") {
        if exists(&name) {
            return name;
        }
    }
    if let Some(name) = fc_match("sans-serif") {
        if exists(&name) {
            return name;
        }
    }
    // No proportional face found — monospace still renders correctly.
    mono_fallback.to_string()
}

fn fc_match_mono() -> Option<String> {
    fc_match("monospace")
}

fn fc_match(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args([pattern, "--format", "%{family}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // fc-match may print "Family A,Family B" — take the first.
    let s = String::from_utf8_lossy(&out.stdout);
    let first = s.split(',').next()?.trim().to_string();
    (!first.is_empty()).then_some(first)
}

/// The TTF file `fc-match` resolves for a family/pattern.
fn fc_match_file(pattern: &str) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("fc-match")
        .args([pattern, "--format", "%{file}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| std::path::PathBuf::from(s))
}

/// The TTF bytes for the egui UI/screen font. JetBrains Mono Nerd Font by
/// default (`ZODIAC_GUI_UI_FONT` overrides the family); the Nerd glyphs cover
/// the UI's symbols. `None` if fontconfig can't resolve or the file can't be
/// read — egui then keeps its built-in font.
pub fn egui_ui_font() -> Option<Vec<u8>> {
    let fam =
        std::env::var("ZODIAC_GUI_UI_FONT").unwrap_or_else(|_| "JetBrainsMono Nerd Font".into());
    let path = fc_match_file(&fam).or_else(|| fc_match_file("monospace"))?;
    std::fs::read(path).ok()
}
