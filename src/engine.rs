//! `TermEngine` — the seam between zodiac's server and its terminal
//! emulator (docs/roadmap.md, Phase 1 task 1.1).
//!
//! Every server-side consumer (`pane.rs`, `gfx.rs`, `query.rs`, the golden
//! runner) reads the emulator exclusively through these traits, so swapping
//! the engine is a `type ActiveEngine` flip gated by the corpus goldens —
//! and a candidate engine can be dual-run against vt100 by implementing
//! them in a scratch bin (Spike S1).
//!
//! The module is deliberately self-contained (vendored vt100 only, no
//! crate-internal deps) so integration tests include it via `#[path]`,
//! like `corpus.rs`. The client keeps its own direct vt100 use until the
//! Phase 3 `client_core` extraction.

// The golden runner includes this file via #[path] and doesn't exercise
// every accessor; the main crate does.
#![allow(dead_code)]

/// Engine-agnostic aliases: consumers name these, never `vt100::` directly.
/// A future adapter maps its native types into them (they are plain enums,
/// constructible from outside the vendored crate).
pub type Color = vt100::Color;
pub type TermEvent = vt100::TermEvent;

/// One cell's render-relevant state, copied out of the engine.
pub struct CellView {
    /// Grapheme contents; empty for a blank cell.
    pub contents: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// Read-only view of the current screen.
pub trait TermScreen {
    /// (rows, cols)
    fn size(&self) -> (u16, u16);
    /// 0-based (row, col)
    fn cursor_position(&self) -> (u16, u16);
    fn title(&self) -> String;
    /// Plain-text screen contents (rows joined with newlines, trailing
    /// whitespace per row trimmed by the underlying engine).
    fn contents(&self) -> String;
    /// Each visible row as plain text, full width.
    fn rows_text(&self) -> Vec<String>;
    fn cell(&self, row: u16, col: u16) -> Option<CellView>;
    fn audible_bell_count(&self) -> usize;
    // Mode introspection (QueryScanner's DECRQM report).
    fn application_cursor(&self) -> bool;
    fn hide_cursor(&self) -> bool;
    fn alternate_screen(&self) -> bool;
    fn bracketed_paste(&self) -> bool;
    /// Inside a DECSET 2026 synchronized update (present atomically).
    fn synchronized_update(&self) -> bool;
    /// Kitty keyboard protocol flags in effect (0 = legacy; roadmap 4.4).
    fn kitty_keyboard_flags(&self) -> u8;
}

/// The emulator itself: feed bytes, observe screens and semantic events.
pub trait TermEngine {
    fn process(&mut self, bytes: &[u8]);
    fn resize(&mut self, rows: u16, cols: u16);
    /// Ordered semantic events since the last drain (scrolls, erases,
    /// alt-screen switches…) — the contract `GfxEngine::apply_event`
    /// replays kitty placements against.
    fn drain_events(&mut self) -> Vec<TermEvent>;
    fn screen(&self) -> &dyn TermScreen;
}

/// The engine zodiac currently runs on: the vendored, instrumented vt100.
pub struct Vt100Engine {
    parser: vt100::Parser,
}

impl Vt100Engine {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let mut parser = vt100::Parser::new(rows, cols, scrollback);
        parser.enable_events();
        Self { parser }
    }
}

impl TermEngine for Vt100Engine {
    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.set_size(rows, cols);
    }

    fn drain_events(&mut self) -> Vec<TermEvent> {
        self.parser.drain_events()
    }

    fn screen(&self) -> &dyn TermScreen {
        self.parser.screen()
    }
}

impl TermScreen for vt100::Screen {
    fn size(&self) -> (u16, u16) {
        vt100::Screen::size(self)
    }

    fn cursor_position(&self) -> (u16, u16) {
        vt100::Screen::cursor_position(self)
    }

    fn title(&self) -> String {
        vt100::Screen::title(self).to_string()
    }

    fn contents(&self) -> String {
        vt100::Screen::contents(self)
    }

    fn rows_text(&self) -> Vec<String> {
        let (_, cols) = vt100::Screen::size(self);
        self.rows(0, cols).collect()
    }

    fn cell(&self, row: u16, col: u16) -> Option<CellView> {
        vt100::Screen::cell(self, row, col).map(|c| CellView {
            contents: c.contents(),
            fg: c.fgcolor(),
            bg: c.bgcolor(),
            bold: c.bold(),
            dim: c.dim(),
            italic: c.italic(),
            underline: c.underline(),
            inverse: c.inverse(),
        })
    }

    fn audible_bell_count(&self) -> usize {
        vt100::Screen::audible_bell_count(self)
    }

    fn application_cursor(&self) -> bool {
        vt100::Screen::application_cursor(self)
    }

    fn hide_cursor(&self) -> bool {
        vt100::Screen::hide_cursor(self)
    }

    fn alternate_screen(&self) -> bool {
        vt100::Screen::alternate_screen(self)
    }

    fn bracketed_paste(&self) -> bool {
        vt100::Screen::bracketed_paste(self)
    }

    fn synchronized_update(&self) -> bool {
        vt100::Screen::synchronized_update(self)
    }

    fn kitty_keyboard_flags(&self) -> u8 {
        vt100::Screen::kitty_keyboard_flags(self)
    }
}

/// The engine the server runs. Swapping engines (roadmap 1.4A) means
/// pointing this alias at the new adapter behind a cargo feature and
/// holding the corpus goldens byte-identical.
pub type ActiveEngine = Vt100Engine;

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(bytes: &[u8]) -> Vt100Engine {
        let mut e = Vt100Engine::new(6, 10, 0);
        e.process(bytes);
        e
    }

    /// OSC 52 write surfaces a Clipboard event for the server to gate;
    /// the query form (payload "?") must not.
    #[test]
    fn osc52_surfaces_clipboard_event() {
        let mut e = fed(b"\x1b]52;c;aGVsbG8=\x07");
        let evs = e.drain_events();
        assert!(evs.iter().any(|ev| matches!(
            ev,
            TermEvent::Clipboard { selection, payload }
                if selection == b"c" && payload == b"aGVsbG8="
        )));
        let mut e = fed(b"\x1b]52;c;?\x07");
        assert!(e
            .drain_events()
            .iter()
            .all(|ev| !matches!(ev, TermEvent::Clipboard { .. })));
    }

    /// OSC 8 hyperlinks are parse+drop: no event, no residue on screen.
    #[test]
    fn osc8_hyperlink_consumed() {
        let mut e = fed(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\");
        assert_eq!(e.screen().rows_text()[0].trim_end(), "link");
        assert!(e
            .drain_events()
            .iter()
            .all(|ev| !matches!(ev, TermEvent::Clipboard { .. })));
    }

    /// SGR 2 sets dim; SGR 22 clears both intensities; SGR 58 in params
    /// form swallows its color arguments instead of reparsing them.
    #[test]
    fn sgr_dim_and_underline_color_args() {
        let e = fed(b"\x1b[2mdim\x1b[22m \x1b[1mB\x1b[22m \x1b[58;2;255;0;0mx");
        let c = e.screen().cell(0, 0).unwrap();
        assert!(c.dim && !c.bold);
        let x = e.screen().cell(0, 6).unwrap();
        assert!(!x.dim && !x.bold, "58's args must not reparse as SGR 2");
        assert_eq!(x.contents, "x");
    }

    /// 4:x underline-style subparams: any style is our underline, 4:0 off.
    #[test]
    fn sgr_underline_subparam_styles() {
        let e = fed(b"\x1b[4:3mcurly\x1b[4:0m off");
        assert!(e.screen().cell(0, 0).unwrap().underline);
        assert!(!e.screen().cell(0, 7).unwrap().underline);
    }

    /// Kitty keyboard flag stack (4.4): push/pop, per-screen stacks,
    /// set with or/and-not modes.
    #[test]
    fn kitty_keyboard_flag_stack() {
        let mut e = fed(b"\x1b[>1u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 1);
        e.process(b"\x1b[>5u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 5);
        e.process(b"\x1b[<u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 1);
        // The alt screen has its own stack.
        e.process(b"\x1b[?1049h");
        assert_eq!(e.screen().kitty_keyboard_flags(), 0);
        e.process(b"\x1b[>3u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 3);
        e.process(b"\x1b[?1049l");
        assert_eq!(e.screen().kitty_keyboard_flags(), 1);
        // Set modes: 2 = or, 3 = clear bits, default replace.
        e.process(b"\x1b[=4;2u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 5);
        e.process(b"\x1b[=1;3u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 4);
        e.process(b"\x1b[=2u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 2);
        // Pop past the bottom clamps to the legacy protocol.
        e.process(b"\x1b[<9u");
        assert_eq!(e.screen().kitty_keyboard_flags(), 0);
    }

    /// DECAWM reset (?7l): the cursor pins at the right margin and
    /// overwrites; set (?7h) restores wrapping.
    #[test]
    fn decawm_off_overwrites_at_margin() {
        let e = fed(b"\x1b[?7labcdefghijKLM");
        assert_eq!(e.screen().rows_text()[0], "abcdefghiM");
        assert_eq!(e.screen().rows_text()[1].trim_end(), "");
        let e = fed(b"\x1b[?7habcdefghijKLM");
        assert_eq!(e.screen().rows_text()[0], "abcdefghij");
        assert_eq!(e.screen().rows_text()[1].trim_end(), "KLM");
    }
}
