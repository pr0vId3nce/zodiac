/// Structural changes to the grid that an out-of-band layer (zodiac's
/// graphics placement engine) needs to mirror. Events are recorded in exact
/// occurrence order on the screen and drained via
/// [`Parser::drain_events`](crate::Parser::drain_events); recording is off
/// until [`Parser::enable_events`](crate::Parser::enable_events) so parsers
/// that never drain don't accumulate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// Rows top..=bottom moved up by `n`: linefeed at the bottom, wrap,
    /// CSI S, or DL. Whether the departing rows entered scrollback follows
    /// vt100's own rule (full-screen region only).
    ScrollUp { top: u16, bottom: u16, n: u16 },
    /// Rows top..=bottom moved down by `n`: RI at the top, CSI T, or IL.
    ScrollDown { top: u16, bottom: u16, n: u16 },
    /// ED 2/3 — the whole drawing area erased.
    EraseScreen,
    /// Entered the alternate screen (DECSET 47/1047/1049).
    AltEnter,
    /// Left the alternate screen.
    AltExit,
    /// The terminal was resized.
    Resize { rows: u16, cols: u16 },
    /// RIS — full reset.
    Reset,
    /// OSC 52 write: the application asked the terminal to set a clipboard
    /// (`c`/`p`/... selection, base64 payload — passed through unparsed for
    /// the embedder to gate and decode).
    Clipboard {
        selection: Vec<u8>,
        payload: Vec<u8>,
    },
}
