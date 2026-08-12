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

/// macOS font resolution, by path rather than by fontconfig.
///
/// A stock macOS has no `fc-match` at all, and — worse — when Homebrew has
/// installed one it never *fails*: it answers every unknown family with its
/// best guess. On this machine `fc-match "JetBrainsMono Nerd Font"` returns
/// Andale Mono and `fc-match "DejaVu Sans"` returns Hiragino Sans, a CJK
/// face. The Linux path's "resolve or fall back" contract therefore turns
/// into "silently render the entire UI in the wrong font" here, which is
/// why macOS asks the filesystem instead.
#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};

    /// Monospace faces present on every Mac, best first. SF Mono is Apple's
    /// developer face (shipped as the system `SFNSMono`, and inside
    /// Terminal.app); Menlo and Monaco are the long-standing fallbacks.
    pub const MONO_FILES: &[&str] = &[
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Regular.otf",
        "/Library/Fonts/SF-Mono-Regular.otf",
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Monaco.ttf",
    ];

    /// Family names fontdb actually exposes on macOS. The SF faces are
    /// system-hidden (their family reads `.SF NS Mono`), so they can't be
    /// asked for by name — these can, and are used for cell metrics.
    pub const MONO_FAMILIES: &[&str] = &["Menlo", "Monaco", "Courier New"];
    pub const UI_FAMILIES: &[&str] = &["SF Pro Text", "Helvetica Neue", "Menlo"];

    /// Monochrome faces broadening glyph coverage. Apple Color Emoji is
    /// deliberately absent: it is a color (sbix) face that egui's
    /// rasterizer renders as blanks.
    pub const FALLBACK_FILES: &[(&str, &str)] = &[
        ("Menlo", "/System/Library/Fonts/Menlo.ttc"),
        ("Monaco", "/System/Library/Fonts/Monaco.ttf"),
        ("Apple Symbols", "/System/Library/Fonts/Apple Symbols.ttf"),
    ];

    pub fn first_existing(paths: &[&str]) -> Option<PathBuf> {
        paths.iter().map(PathBuf::from).find(|p| p.is_file())
    }

    fn dirs() -> Vec<PathBuf> {
        let mut v = vec![
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/System/Library/Fonts/Supplemental"),
            PathBuf::from("/Library/Fonts"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            v.push(Path::new(&home).join("Library/Fonts"));
        }
        v
    }

    fn squash(s: &str) -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_lowercase()
    }

    /// Find a user-named family by filename, so someone who installed, say,
    /// JetBrainsMono Nerd Font still gets it via `ZODIAC_GUI_UI_FONT` —
    /// the env override keeps working, it just isn't fontconfig doing it.
    pub fn find_family_file(family: &str) -> Option<PathBuf> {
        let want = squash(family);
        if want.is_empty() {
            return None;
        }
        for dir in dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut loose: Option<PathBuf> = None;
            for e in entries.flatten() {
                let p = e.path();
                let is_font = p
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| matches!(x.to_lowercase().as_str(), "ttf" | "ttc" | "otf"));
                if !is_font {
                    continue;
                }
                let stem = squash(p.file_stem().and_then(|s| s.to_str()).unwrap_or(""));
                if stem.is_empty() || !stem.contains(&want) {
                    continue;
                }
                // Prefer the regular cut over Bold/Italic/Heavy variants.
                if stem == want || stem.ends_with("regular") {
                    return Some(p);
                }
                loose.get_or_insert(p);
            }
            if loose.is_some() {
                return loose;
            }
        }
        None
    }
}

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
    #[cfg(target_os = "macos")]
    for name in mac::MONO_FAMILIES {
        if exists(name) {
            return (*name).to_string();
        }
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
    #[cfg(target_os = "macos")]
    for name in mac::UI_FAMILIES {
        if exists(name) {
            return (*name).to_string();
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
#[cfg(not(target_os = "macos"))]
pub fn egui_ui_font() -> Option<Vec<u8>> {
    let fam =
        std::env::var("ZODIAC_GUI_UI_FONT").unwrap_or_else(|_| "JetBrainsMono Nerd Font".into());
    let path = fc_match_file(&fam).or_else(|| fc_match_file("monospace"))?;
    std::fs::read(path).ok()
}

/// macOS: SF Mono, the face a Mac developer expects to be looking at.
/// `ZODIAC_GUI_UI_FONT` still wins when the named family is installed.
#[cfg(target_os = "macos")]
pub fn egui_ui_font() -> Option<Vec<u8>> {
    if let Ok(fam) = std::env::var("ZODIAC_GUI_UI_FONT") {
        if let Some(bytes) = mac::find_family_file(&fam).and_then(|p| std::fs::read(p).ok()) {
            return Some(bytes);
        }
        eprintln!(
            "zodiac-gui: ZODIAC_GUI_UI_FONT '{fam}' not installed, using the system mono face"
        );
    }
    std::fs::read(mac::first_existing(mac::MONO_FILES)?).ok()
}

/// Extra fallback faces appended after the primary font so the terminal can
/// render prompt glyphs the main font lacks — broad Nerd/symbol coverage
/// (Symbols Nerd Font), a wide Unicode net (DejaVu Sans), and a *monochrome*
/// emoji face if one is installed. Color-emoji faces (e.g. Noto Color Emoji)
/// are skipped: egui's text engine can't rasterize color glyphs, so a color
/// face would still render a blank/box — install a monochrome emoji font and
/// it is picked up here automatically.
pub fn egui_fallback_fonts() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    #[cfg(target_os = "macos")]
    for (name, path) in mac::FALLBACK_FILES {
        let path = std::path::PathBuf::from(path);
        if !path.is_file() || !seen.insert(path.clone()) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            out.push((format!("fallback-{name}"), bytes));
        }
    }
    // Monochrome emoji faces are tried by several names: egui rasterizes
    // outlines only, so a colour font (NotoColorEmoji is CBDT bitmaps) is
    // useless to it however well fontconfig scores it. If none of these is
    // installed, emoji outside egui's small built-in subset simply have no
    // glyph — that is a missing system font, not something the client can fix.
    #[cfg(not(target_os = "macos"))]
    for fam in [
        "Symbols Nerd Font",
        "DejaVu Sans",
        "Noto Emoji",
        "Noto Sans Symbols 2",
        "Symbola",
        "OpenMoji",
        "Twemoji Mono",
    ] {
        let Some(path) = fc_match_file(fam) else {
            continue;
        };
        let low = path.to_string_lossy().to_lowercase();
        if low.contains("color") || low.contains("coloremoji") {
            continue; // a colour-emoji face egui can't rasterize
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            out.push((format!("fallback-{fam}"), bytes));
        }
    }
    out
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    /// The macOS UI font must resolve to real bytes. Returning `None` here
    /// is not a visible failure — egui silently keeps its built-in face and
    /// the whole app renders in the wrong font, which is exactly how the
    /// fontconfig path failed before (`fc-match` answers every unknown
    /// family with an arbitrary one instead of failing).
    #[test]
    fn ui_font_resolves_to_a_real_face() {
        let bytes = super::egui_ui_font().expect("no macOS system mono face found");
        assert!(bytes.len() > 4096, "font file suspiciously small");
        // sfnt magic: 0x00010000 (TrueType), "true"/"ttcf", or "OTTO" (CFF).
        let magic = &bytes[..4];
        assert!(
            matches!(
                magic,
                [0x00, 0x01, 0x00, 0x00] | b"true" | b"ttcf" | b"OTTO"
            ),
            "not a font container: {magic:02x?}"
        );
    }

    /// An unknown family must fall through to the system face rather than
    /// resolving to something arbitrary.
    #[test]
    fn unknown_family_is_not_silently_substituted() {
        assert!(super::mac::find_family_file("NoSuchFontFamilyXYZ").is_none());
    }
}

/// Is a monochrome emoji face installed that egui could actually use? The e2e
/// harness reports emoji coverage against this: with none installed, glyphs
/// outside egui's built-in subset cannot render and that is a system gap.
#[cfg(not(target_os = "macos"))]
pub fn mono_emoji_font() -> Option<String> {
    // Ask fontconfig which installed fonts actually cover an emoji codepoint
    // (U+1F980 CRAB) — `fc-match` is no good here because it always
    // substitutes *something*, so a missing family silently answers with
    // DejaVu Sans, which has no emoji at all. Colour fonts are excluded:
    // egui rasterizes outlines and cannot draw CBDT/COLR bitmaps.
    let out = std::process::Command::new("fc-list")
        .args([":charset=1f980", "file"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let path = line.trim().trim_end_matches(':').trim();
        if path.is_empty() {
            continue;
        }
        if path.to_lowercase().contains("color") {
            continue;
        }
        return Some(path.to_string());
    }
    None
}

#[cfg(target_os = "macos")]
pub fn mono_emoji_font() -> Option<String> {
    None
}
