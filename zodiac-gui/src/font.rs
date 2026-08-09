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
}

impl Fonts {
    pub fn load() -> Self {
        let system = FontSystem::new();
        let family = pick_family(&system);
        Self { system, family }
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

fn fc_match_mono() -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["monospace", "--format", "%{family}"])
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
