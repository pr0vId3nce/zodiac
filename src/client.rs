use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::protocol::{Frame as SrvFrame, *};
use crate::term::{encode_key, encode_mouse, TermView};

const SIDEBAR_WIDTH: u16 = 24;
const SIDEBAR_COLLAPSED: u16 = 4;
const CLIENT_SCROLLBACK: usize = 10_000;

/// How much terminal text the chat digest may carry, and how many rows
/// each card contributes. The cap keeps a spread of busy panes from eating
/// the model's context; the tail of a screen is where the answer lives.
const EXCERPT_BUDGET: usize = 4000;
const EXCERPT_FOCUSED: usize = 30;
const EXCERPT_OTHER: usize = 15;

/// How recently a background pane must have produced output to count as
/// "in progress" (orange) rather than "finished" (green).
const IN_PROGRESS_WINDOW: Duration = Duration::from_secs(5);

/// How long after the last braille title frame a resting ✳ title still
/// counts as mid-work. Claude Code's spinner animates continuously while
/// working, so real rest-frame gaps are well under this; after finishing,
/// braille frames stop and "working" clears within this window.
const TITLE_BRIDGE: Duration = Duration::from_secs(2);

const WORKING_COLOR: Color = Color::Indexed(208);

/// True black. `Color::Black` is ANSI index 0, which most terminal themes
/// tint dark grey rather than #000 — this asks for the RGB value directly.
const OLED_BLACK: Color = Color::Rgb(0, 0, 0);

/// Output arriving this soon after a resize is a SIGWINCH repaint, not
/// activity (matches the server-side squelch).
const RESIZE_SQUELCH: Duration = Duration::from_millis(1200);

/// The focused pane's sidebar row shows an eye instead of its number; it
/// blinks (closes briefly) once per period.
const EYE_PERIOD_MS: u64 = 4000;
const EYE_BLINK_MS: u64 = 160;
const SETTINGS_ROWS: usize = 30;

/// Settings rows at and beyond this index are free-text fields (edited via
/// `Mode::SettingsEdit`) rather than cyclable presets.
const TEXT_SETTINGS_START: usize = 26;

/// The key reference pinned to the settings page's Controls column — the
/// same bindings the bottom bar hints at (hideable there).
const CONTROLS: &[(&str, &str)] = &[
    ("Alt+N", "new pane"),
    ("Alt+W", "close pane"),
    ("Alt+R", "rename pane"),
    ("Alt+⇧R", "restore agents"),
    ("Alt+↑/↓", "switch pane"),
    ("Alt+1-9", "jump to pane"),
    ("Alt+PgUp/Dn", "move pane"),
    ("Alt+T", "toggle sidebar"),
    ("Alt+Z", "zoom pane"),
    ("Alt+~", "home page"),
    ("⇧PgUp/Dn", "scroll"),
    ("Ctrl+S", "settings"),
    ("Alt+G", "chat panel"),
    ("Alt+Q", "detach"),
    ("Alt+⇧Q", "kill session"),
];

/// Chat transcript roles.
const CHAT_USER: u8 = 0;
const CHAT_REPLY: u8 = 1;
const CHAT_NOTE: u8 = 2;
const CHAT_ERROR: u8 = 3;
/// A pending prompt_pane/send_keys consent chip — "▸ run: ... [y/n]".
const CHAT_ACTION: u8 = 4;

const CURSOR_TYPES: &[&str] =
    &["auto", "block", "underline", "bar", "orb", "circle", "aleph"];
const CURSOR_BLINKS: &[&str] = &["auto", "on", "off"];
/// Orb pulse period; frames are quantized so they can be pre-transmitted.
const ORB_PERIOD_MS: u64 = 1400;
/// Untinted orb: pale mystic blue-gray, a palantir at rest.
const ORB_DEFAULT_RGB: (u8, u8, u8) = (168, 178, 210);

/// Named colors offered for the spinner and the shimmer band.
const COLOR_CHOICES: &[(&str, Color, (u8, u8, u8))] = &[
    ("orange", Color::Indexed(208), (255, 135, 0)),
    ("gold", Color::Indexed(220), (255, 215, 0)),
    ("cyan", Color::Indexed(45), (0, 215, 255)),
    ("blue", Color::Indexed(75), (95, 175, 255)),
    ("violet", Color::Indexed(135), (175, 95, 255)),
    ("pink", Color::Indexed(205), (255, 95, 175)),
    ("green", Color::Indexed(114), (135, 215, 135)),
    ("red", Color::Indexed(203), (255, 95, 135)),
    ("white", Color::Indexed(15), (255, 255, 255)),
    ("gray", Color::Indexed(245), (138, 138, 138)),
    ("dark", Color::DarkGray, (98, 98, 98)),
];

const CARD_OUTLINES: &[&str] = &["double", "single", "none"];
const SELECT_STYLES: &[&str] = &["glow", "ring"];
const CARD_NUMERALS: &[&str] = &["roman", "arabic", "zodiac", "zodiac-white"];
const CLAUDE_STYLES: &[&str] = &["hard", "soft"];
const ZODIAC: &[&str] = &["♈", "♉", "♊", "♋", "♌", "♍", "♎", "♏", "♐", "♑", "♒", "♓"];

/// Selected-card outline thickness as a fraction of card pixel height.
const SELECT_WEIGHTS: &[(&str, f32)] = &[
    ("thin", 0.004),
    ("normal", 0.007),
    ("thick", 0.012),
    ("heavy", 0.018),
];

const SIDEBAR_FRAMES: &[&str] = &["separator", "surround", "rounded"];
/// Emblem sizes for the painted home-page cards, as a fraction of card
/// height (name, scale).
const CARD_ICON_SIZES: &[(&str, f32)] = &[
    ("small", 0.015),
    ("medium", 0.021),
    ("large", 0.028),
    ("huge", 0.035),
];
const BORDER_WEIGHTS: &[&str] = &["normal", "thick", "double"];

/// Resolve a plain named choice with a default, e.g. sidebar frame/weight.
fn pick<'a>(list: &[&'a str], cur: &str, default: &'a str) -> &'a str {
    list.iter().find(|n| **n == cur).copied().unwrap_or(default)
}

fn cycle_pick(list: &[&str], cur: &str, dir: isize) -> String {
    let i = list.iter().position(|n| *n == cur).unwrap_or(0) as isize;
    list[(i + dir).rem_euclid(list.len() as isize) as usize].to_string()
}

const SHIMMER_SPEEDS: &[(&str, u64)] = &[
    ("slow", 3200),
    ("normal", 2000),
    ("fast", 1200),
    ("zippy", 700),
];

/// `h`/`s`/`v` each 0.0-1.0. Used for the Oracle's drifting hues, where
/// picking a color by angle round a wheel reads far more naturally than
/// lerping RGB channels straight-line (which crosses through grey/brown).
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match (i as i64).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

/// Resolve a color-setting value, falling back to the given default name.
fn color_by_name(name: &str, default: &str) -> (&'static str, Color, (u8, u8, u8)) {
    COLOR_CHOICES
        .iter()
        .find(|(n, _, _)| *n == name)
        .or_else(|| COLOR_CHOICES.iter().find(|(n, _, _)| *n == default))
        .map(|(n, c, rgb)| (*n, *c, *rgb))
        .unwrap()
}

fn cycle_color_name(cur: &str, dir: isize) -> String {
    let i = COLOR_CHOICES
        .iter()
        .position(|(n, _, _)| *n == cur)
        .unwrap_or(0) as isize;
    COLOR_CHOICES[(i + dir).rem_euclid(COLOR_CHOICES.len() as isize) as usize]
        .0
        .to_string()
}

// Home-page tarot cards.
/// Card footprint in cells (name, width, height); medium is the default.
const CARD_SIZES: &[(&str, u16, u16)] = &[
    ("small", 20, 10),
    ("medium", 26, 13),
    ("large", 32, 16),
    ("huge", 40, 20),
];
/// Column cap for the home grid; "auto" packs as many as fit.
const CARD_COLUMNS: &[&str] = &["auto", "1", "2", "3", "4", "5", "6"];
const HOME_VIEWS: &[&str] = &["cards", "list", "blocks"];
/// Chat panel characters: a plain assistant, an ascii Oracle orb, or
/// HAL 9000's red eye.
const CHAT_FACES: &[&str] = &["assistant", "oracle", "hal"];
const HOME_GAP_X: u16 = 3;
const HOME_GAP_Y: u16 = 1;
/// Card-art glow colors by accent index: needs approval, thinking,
/// working, finished, idle (order matches `card_status`).
const ACCENT_RGB: [(u8, u8, u8); 5] = [
    (235, 90, 100),
    (150, 110, 235),
    (255, 150, 40),
    (90, 200, 120),
    (110, 110, 140),
];

/// Sidebar equalizer for working panes: each bar dips to half height and
/// back once per cycle, staggered — a one-cell-tall rendition of the CSS
/// bounce-and-stretch bars (3s cycle, 0.3s delay per bar).
const EQ_BARS: usize = 5;
const EQ_CYCLE_MS: u64 = 3000;
const EQ_STAGGER_MS: u64 = 300;
const EQ_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Working-animation styles selectable in settings (Ctrl+S). "equalizer" is
/// the procedural bounce-and-stretch bars; the rest are frame loops from
/// FGRibreau/spinners (the cli-spinners collection), the ones that fit
/// sidebar cells.
struct SpinnerDef {
    name: &'static str,
    frames: &'static [&'static str],
    interval: u64,
}

impl SpinnerDef {
    fn width(&self) -> usize {
        self.frames.first().map_or(EQ_BARS, |f| f.chars().count())
    }
}

#[rustfmt::skip]
const SPINNERS: &[SpinnerDef] = &[
    SpinnerDef { name: "equalizer", frames: &[], interval: 100 },
    SpinnerDef { name: "dots", frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"], interval: 80 },
    SpinnerDef { name: "line", frames: &["-", "\\", "|", "/"], interval: 130 },
    SpinnerDef { name: "pipe", frames: &["┤", "┘", "┴", "└", "├", "┌", "┬", "┐"], interval: 100 },
    SpinnerDef { name: "arc", frames: &["◜", "◠", "◝", "◞", "◡", "◟"], interval: 100 },
    SpinnerDef { name: "triangle", frames: &["◢", "◣", "◤", "◥"], interval: 50 },
    SpinnerDef { name: "circle-halves", frames: &["◐", "◓", "◑", "◒"], interval: 50 },
    SpinnerDef { name: "square-corners", frames: &["◰", "◳", "◲", "◱"], interval: 180 },
    SpinnerDef { name: "grow-vertical", frames: &["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"], interval: 120 },
    SpinnerDef { name: "noise", frames: &["▓", "▒", "░"], interval: 100 },
    SpinnerDef { name: "toggle", frames: &["⊶", "⊷"], interval: 250 },
    SpinnerDef { name: "star", frames: &["+", "x", "*"], interval: 80 },
    SpinnerDef { name: "point", frames: &["∙∙∙", "●∙∙", "∙●∙", "∙∙●", "∙∙∙"], interval: 125 },
    SpinnerDef { name: "arrow", frames: &["▹▹▹▹▹", "▸▹▹▹▹", "▹▸▹▹▹", "▹▹▸▹▹", "▹▹▹▸▹", "▹▹▹▹▸"], interval: 120 },
    SpinnerDef { name: "bouncing-bar", frames: &["[    ]", "[=   ]", "[==  ]", "[=== ]", "[ ===]", "[  ==]", "[   =]", "[    ]", "[   =]", "[  ==]", "[ ===]", "[====]", "[=== ]", "[==  ]", "[=   ]"], interval: 80 },
    SpinnerDef { name: "aesthetic", frames: &["▰▱▱▱▱▱▱", "▰▰▱▱▱▱▱", "▰▰▰▱▱▱▱", "▰▰▰▰▱▱▱", "▰▰▰▰▰▱▱", "▰▰▰▰▰▰▱", "▰▰▰▰▰▰▰", "▰▱▱▱▱▱▱"], interval: 80 },
    SpinnerDef { name: "bouncing-ball", frames: &["( ●    )", "(  ●   )", "(   ●  )", "(    ● )", "(     ●)", "(    ● )", "(   ●  )", "(  ●   )", "( ●    )", "(●     )"], interval: 80 },
];

/// The focused pane's eye: an open glyph and a closed (blink) glyph.
struct EyeDef {
    name: &'static str,
    open: char,
    closed: char,
}

const EYES: &[EyeDef] = &[
    EyeDef { name: "eye", open: 'ಠ', closed: '‿' },
    EyeDef { name: "dot", open: '◉', closed: '─' },
    EyeDef { name: "star", open: '✦', closed: '✧' },
    EyeDef { name: "heart", open: '♥', closed: '♡' },
    EyeDef { name: "diamond", open: '◆', closed: '◇' },
    EyeDef { name: "pulse", open: '●', closed: '○' },
    EyeDef { name: "flower", open: '✿', closed: '❀' },
    EyeDef { name: "note", open: '♪', closed: '♫' },
    EyeDef { name: "arrow", open: '▶', closed: '▷' },
];

use crate::settings::Settings;

enum AppEvent {
    Term(Event),
    Srv(SrvFrame),
    SrvGone,
    Chat(crate::chat::ChatEvent),
}

enum Mode {
    Normal,
    Rename { buf: String },
    Settings,
    /// Editing one free-text field on the Settings page (chat endpoint,
    /// model, ssh destination, or service name); `row` is the
    /// `settings_row` it was opened from, so Enter/Esc return there.
    SettingsEdit { row: usize, buf: String },
    /// Fuzzy-find a pane by name (Alt+/); `jump_sel` on `App` tracks which
    /// of the live-filtered matches is highlighted.
    Jump { buf: String },
    /// Alt+⇧R: what the last snapshot would raise, awaiting confirmation.
    /// One line per agent pane; empty when there's nothing to restore.
    Restore { lines: Vec<String>, age: String },
}

/// A mouse selection in the pane area, in pane-relative (row, col) cells.
/// Anchored where the drag started; `head` follows the pointer.
#[derive(Clone, Copy)]
struct Sel {
    pane: u64,
    anchor: (u16, u16),
    head: (u16, u16),
}

impl Sel {
    fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        if self.head < self.anchor {
            (self.head, self.anchor)
        } else {
            (self.anchor, self.head)
        }
    }
}

/// A pane image mirrored from the server, plus the id it was transmitted
/// to the outer terminal under (None until first needed on screen).
struct CImg {
    ver: u32,
    format: u8,
    zlib: bool,
    w: u32,
    h: u32,
    data: Vec<u8>,
    outer: Option<u32>,
}

/// One placement currently alive on the outer terminal.
struct OuterPlaced {
    pane: u64,
    key: u64,
    pid: u32,
    geom: OuterGeom,
}

/// Everything that positions a placement on the outer terminal; equality
/// means "nothing to redraw".
#[derive(Clone, PartialEq)]
struct OuterGeom {
    img: u32, // outer image id
    x: u16,   // 1-based screen cell
    y: u16,
    src: (u32, u32, u32, u32),
    c: u16,
    r: u16,
    z: i32,
    offx: u16,
    offy: u16,
}

struct CPane {
    id: u64,
    name: String,
    parser: vt100::Parser,
    scroll: usize,
    last_output: Option<Instant>,
    /// When the title last showed a braille (working) spinner frame — lets
    /// the ✳ rest frames mid-work read as working without fresh output
    /// alone doing so (see `working()`).
    last_title_working: Option<Instant>,
    activity: bool,
    attention: bool,
    bell_count: usize,
    size: (u16, u16),
    /// Latest graphics snapshot from the server (placements + live images).
    gfx: crate::gfx::GfxSnapshot,
    images: std::collections::HashMap<u32, CImg>,
    /// Chunked T_GFX_IMG payloads still assembling.
    partial: std::collections::HashMap<u32, Vec<u8>>,
}

impl CPane {
    fn new(id: u64, name: String, rows: u16, cols: u16) -> Self {
        let rows = rows.max(2);
        let cols = cols.max(10);
        Self {
            id,
            name,
            parser: vt100::Parser::new(rows, cols, CLIENT_SCROLLBACK),
            scroll: 0,
            last_output: None,
            last_title_working: None,
            activity: false,
            attention: false,
            bell_count: 0,
            size: (rows, cols),
            gfx: crate::gfx::GfxSnapshot::default(),
            images: std::collections::HashMap::new(),
            partial: std::collections::HashMap::new(),
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if rows < 2 || cols < 10 || self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        self.parser.set_size(rows, cols);
    }

    fn poll_bell(&mut self) -> bool {
        let count = self.parser.screen().audible_bell_count();
        let new = count > self.bell_count;
        self.bell_count = count;
        new
    }

    fn clear_flags(&mut self) {
        self.activity = false;
        self.attention = false;
        let _ = self.poll_bell();
    }

    fn set_scroll(&mut self, offset: usize) {
        self.scroll = offset;
        self.parser.set_scrollback(offset);
    }

    fn scroll_by(&mut self, delta: isize) {
        let new = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            (self.scroll + delta as usize).min(CLIENT_SCROLLBACK)
        };
        self.set_scroll(new);
    }
}

struct App {
    session: String,
    panes: Vec<CPane>,
    active: usize,
    collapsed: bool,
    zoom: bool,
    mode: Mode,
    main_size: (u16, u16),
    sent_size: (u16, u16),
    quit: bool,
    exit_msg: &'static str,
    anim_start: Instant,
    sidebar_rect: Rect,
    sidebar_inner: Rect,
    main_rect: Rect,
    selection: Option<Sel>,
    selecting: bool,
    copied_at: Option<Instant>,
    settings: Settings,
    settings_row: usize,
    /// Highlighted row among `Mode::Jump`'s live-filtered matches.
    jump_sel: usize,
    resized_at: Option<Instant>,
    home: bool,
    home_sel: usize,
    home_cols: usize,
    home_state: Option<SessionState>,
    home_queried: Option<Instant>,
    /// Card layout from the last home draw: (rect, pane id, accent index).
    home_cards: Vec<(Rect, u64, usize, bool)>,
    /// Blocks view only: (pane id, row, in-left-column) for every pane in
    /// visual order. The two columns fill independently, so once one runs
    /// long the flat ±cols grid math over home_cards skips cards; arrow
    /// keys navigate this instead. Empty in the other views.
    home_block_nav: Vec<(u64, u16, bool)>,
    /// Cells reserved beside active claude blocks (blocks view) where the
    /// hopping mascot sprite is placed by kitty_overlay.
    home_mascots: Vec<Rect>,
    kitty_on: bool,
    /// Card placements currently alive terminal-side (kitty graphics).
    kitty_placed: Vec<(Rect, u32)>,
    /// Card-icon size the current placements were painted with.
    kitty_last_icon: String,
    /// Image data transmitted this attach: (px_w, px_h, image id).
    kitty_sent: std::collections::HashSet<(u32, u32, u32)>,
    /// Pane-image placements currently alive on the outer terminal.
    placed_gfx: Vec<OuterPlaced>,
    /// Outer id allocation for pane images (namespaced away from card ids).
    next_outer: u32,
    next_pid: u32,
    pid_map: std::collections::HashMap<(u64, u64), u32>,
    /// Outer image ids whose data should be freed on the next overlay pass.
    outer_dead: Vec<u32>,
    /// Cursor (style param, tint) last applied to the outer terminal.
    cursor_applied: Option<(u8, Option<(u8, u8, u8)>)>,
    /// Orb-cursor frames transmitted for (shape, rgb, cell w, cell h).
    orb_cfg: Option<(crate::kitty::OrbShape, (u8, u8, u8), u16, u16)>,
    /// Orb placement currently on the terminal:
    /// (cell x, cell y, image id, col span, row span).
    orb_placed: Option<(u16, u16, u32, u16, u16)>,
    // The chat panel (home page, right side).
    chat_tx: Option<Sender<crate::chat::ChatCmd>>,
    /// Set to cut the in-flight stream short — Esc while streaming.
    chat_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// A prompt_pane/send_keys call awaiting a y/n — the worker is blocked
    /// on the sender's paired receiver until one arrives.
    chat_action: Option<(crate::chat::ActionRequest, std::sync::mpsc::Sender<crate::chat::ActionOutcome>)>,
    chat_status: Option<crate::chat::ChatStatus>,
    /// Transcript: (role, text) — see the WIZ_* role constants.
    chat_log: Vec<(u8, String)>,
    /// The reply currently streaming in, not yet committed to the log.
    chat_stream: String,
    chat_streaming: bool,
    chat_input: String,
    chat_focus: bool,
    /// Transcript scroll offset in wrapped lines, from the bottom.
    chat_scroll: usize,
    chat_rect: Rect,
    /// Where the portrait image goes (inside the chat panel).
    chat_art_rect: Rect,
    /// Portrait image data transmitted this attach: (px_w, px_h, image id).
    chat_sent: std::collections::HashSet<(u32, u32, u32)>,
    /// Portrait placement currently alive terminal-side.
    chat_placed: Option<(Rect, u32)>,
    /// This server gates mouse reports on the pane's foreground app (its
    /// hello said so). Older servers don't know `T_MOUSE` and would drop it
    /// on the floor, so against those we keep sending plain `T_INPUT`.
    server_mouse_gate: bool,
    /// Set when the QR pairing overlay (Alt+P) is open — hides card art via
    /// kitty_overlay() the same way Mode::Settings does, and draws one
    /// scannable QR of this machine's pairing URL over the home page.
    pair_open: bool,
    /// (bridge base url, computer id, hostname) read from the astrolabe
    /// bridge's endpoint.json on toggle-open — None if no bridge is
    /// installed/running here. Not re-read every frame: it only changes
    /// when the bridge itself restarts, which is rare enough to just
    /// re-read on the next open.
    pair_endpoint: Option<(String, String, String)>,
    /// The endpoint file points somewhere nothing answers — see
    /// `endpoint_alive`. Probed when the overlay opens.
    pair_dead: bool,
    /// The exact URL the current QR image encodes, so kitty_overlay only
    /// re-rasterizes when the token (or endpoint) actually changes.
    pair_qr_src: String,
    /// (px_w, px_h, rgba, image id) for the current pair_qr_src.
    pair_qr: Option<(u32, u32, Vec<u8>, u32)>,
    /// Where the QR image goes, from the last draw — kitty_overlay() places
    /// it there, same handoff shape as home_cards/home_mascots/chat_art_rect.
    pair_rect: Rect,
    sock: UnixStream,
    rx: Receiver<AppEvent>,
}

pub fn run(session: &str, terminal: &mut DefaultTerminal) -> Result<&'static str> {
    let sock = connect_or_spawn(session)?;
    let (tx, rx) = channel();
    {
        let mut rd = sock.try_clone()?;
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            match read_frame(&mut rd) {
                Ok(f) => {
                    if tx.send(AppEvent::Srv(f)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = tx.send(AppEvent::SrvGone);
                    break;
                }
            }
        });
    }
    // The chat panel's network worker reports through the same channel.
    let settings = Settings::load();
    let (chat_tx, chat_cancel) = if settings.chat_panel {
        let txw = tx.clone();
        let (tx, cancel) = crate::chat::spawn(
            crate::chat::ChatCfg::from_settings(&settings),
            session.to_string(),
            move |ev| {
                let _ = txw.send(AppEvent::Chat(ev));
            },
        );
        (Some(tx), Some(cancel))
    } else {
        (None, None)
    };
    std::thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(ev) => {
                if tx.send(AppEvent::Term(ev)).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    });

    let size = terminal.size()?;
    let mut app = App {
        session: session.to_string(),
        panes: Vec::new(),
        active: 0,
        collapsed: false,
        zoom: false,
        mode: Mode::Normal,
        main_size: (
            size.height.saturating_sub(1).max(2),
            size.width.saturating_sub(SIDEBAR_WIDTH).max(10),
        ),
        sent_size: (0, 0),
        quit: false,
        exit_msg: "zodiac: detached — session keeps running, run `zodiac` to reattach",
        anim_start: Instant::now(),
        sidebar_rect: Rect::default(),
        sidebar_inner: Rect::default(),
        main_rect: Rect::default(),
        selection: None,
        selecting: false,
        copied_at: None,
        settings,
        settings_row: 0,
        jump_sel: 0,
        resized_at: None,
        home: true, // always open to the home page
        home_sel: 0,
        home_cols: 1,
        home_state: None,
        home_queried: None,
        home_cards: Vec::new(),
        home_block_nav: Vec::new(),
        home_mascots: Vec::new(),
        kitty_on: crate::kitty::enabled(),
        kitty_placed: Vec::new(),
        kitty_last_icon: String::new(),
        kitty_sent: std::collections::HashSet::new(),
        placed_gfx: Vec::new(),
        next_outer: 0x5A00_0000, // 'Z' — clear of the card-art id range
        next_pid: 0,
        pid_map: std::collections::HashMap::new(),
        outer_dead: Vec::new(),
        cursor_applied: None,
        orb_cfg: None,
        orb_placed: None,
        chat_tx,
        chat_cancel,
        chat_action: None,
        chat_status: None,
        chat_log: Vec::new(),
        chat_stream: String::new(),
        chat_streaming: false,
        chat_input: String::new(),
        chat_focus: false,
        chat_scroll: 0,
        chat_rect: Rect::default(),
        chat_art_rect: Rect::default(),
        chat_sent: std::collections::HashSet::new(),
        chat_placed: None,
        server_mouse_gate: false,
        pair_open: false,
        pair_endpoint: None,
        pair_dead: false,
        pair_qr_src: String::new(),
        pair_qr: None,
        pair_rect: Rect::default(),
        sock,
        rx,
    };
    // Announce graphics capability + cell size so panes' PTYs report pixel
    // dimensions and the server engines start answering the protocol.
    let cell = crate::kitty::cell_size().unwrap_or((0, 0));
    let hello = [
        app.kitty_on as u8,
        cell.0.to_le_bytes()[0],
        cell.0.to_le_bytes()[1],
        cell.1.to_le_bytes()[0],
        cell.1.to_le_bytes()[1],
    ];
    app.send(T_ATTACH, 0, &hello);
    app.send_resize();
    app.send(T_QUERY, 0, &[]);
    app.home_queried = Some(Instant::now());

    while !app.quit {
        terminal.draw(|f| app.draw(f))?;
        app.kitty_overlay();
        app.pane_overlay();
        app.orb_overlay();
        app.chat_overlay();
        app.cursor_sync();
        if app.main_size != app.sent_size {
            app.send_resize();
        }
        // Refresh pane metadata ~1/s while the home page is open (it wants
        // to feel live), and a slow trickle otherwise — just enough that
        // the sidebar's ssh tag doesn't sit stale for minutes on end.
        let query_every =
            if app.home { Duration::from_secs(1) } else { Duration::from_secs(4) };
        if app.home_queried.is_none_or(|t| t.elapsed() > query_every) {
            app.send(T_QUERY, 0, &[]);
            app.home_queried = Some(Instant::now());
        }
        // Fast ticks only while an animation is on screen (working panes or
        // the settings preview); otherwise wake for the next eye blink.
        let tick = if app.home {
            if app.chat_streaming {
                50 // keep streamed tokens flowing onto the screen
            } else if app.chat_status == Some(crate::chat::ChatStatus::Waking) {
                120 // waking portrait pulse
            } else {
                250
            }
        } else if app.any_working() || matches!(app.mode, Mode::Settings | Mode::SettingsEdit { .. }) {
            50
        } else if app.orb_active() && app.orb_blinking() {
            80 // keep the palantir breathing
        } else if app.zoom {
            500 // sidebar hidden — no eye to animate
        } else {
            let t = app.anim_start.elapsed().as_millis() as u64 % EYE_PERIOD_MS;
            let next = if t < EYE_BLINK_MS {
                EYE_BLINK_MS - t
            } else {
                EYE_PERIOD_MS - t
            };
            next.clamp(16, 500)
        };
        match app.rx.recv_timeout(Duration::from_millis(tick)) {
            Ok(ev) => {
                app.handle(ev);
                let mut drained = 0;
                while let Ok(ev) = app.rx.try_recv() {
                    app.handle(ev);
                    drained += 1;
                    if app.quit || drained > 4096 {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    app.gfx_cleanup();
    // Hand the cursor back exactly as we found it: default style, no tint.
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[0 q\x1b]112\x07");
        let _ = out.flush();
    }
    Ok(app.exit_msg)
}

fn connect_or_spawn(session: &str) -> Result<UnixStream> {
    let path = socket_path(session);
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    let exe = std::env::current_exe()?;
    let logdir = state_dir(session);
    std::fs::create_dir_all(&logdir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logdir.join("server.log"))?;
    let log2 = log.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--server")
        .arg(session)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log2);
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(s) = UnixStream::connect(&path) {
            return Ok(s);
        }
    }
    bail!(
        "zodiac server did not start (see {})",
        logdir.join("server.log").display()
    )
}

/// Reads `(url, cid, name)` from the astrolabe bridge's
/// `~/.local/state/astrolabe/endpoint.json`, written by the bridge itself
/// on startup once it knows its bind host. `None` if no bridge has ever run
/// on this machine, or the file can't be parsed — the pairing overlay shows
/// a "no bridge detected" message in that case rather than a QR.
fn read_bridge_endpoint() -> Option<(String, String, String)> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    let raw = std::fs::read_to_string(base.join("astrolabe").join("endpoint.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let url = v.get("url")?.as_str()?.to_string();
    let cid = v.get("cid")?.as_str()?.to_string();
    let name = v.get("name")?.as_str()?.to_string();
    Some((url, cid, name))
}

/// Is anything actually listening where the bridge said it would be? The
/// pairing file is one per machine and only rewritten on a bridge's own
/// startup, so it can outlive the bridge that wrote it (a second bridge on
/// another port, then killed, leaves the file pointing at a dead one). A
/// QR for a dead address pairs fine and then shows the phone nothing, so
/// check before offering it. 400 ms: a tailnet round trip, not a stall.
fn endpoint_alive(url: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"));
    let Some(hostport) = rest.map(|r| r.split('/').next().unwrap_or(r)) else {
        return true; // unparseable — say nothing rather than cry wolf
    };
    let Ok(mut addrs) = hostport.to_socket_addrs() else {
        return true;
    };
    addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(400)).is_ok())
}

/// Percent-encodes a query-param value. The token is hex and the cid a
/// UUID (both always safe as-is), but the hostname isn't guaranteed to be —
/// encode all three uniformly rather than special-case.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl App {
    fn send(&mut self, typ: u8, id: u64, data: &[u8]) {
        if write_frame(&mut self.sock, typ, id, data).is_err() {
            self.exit_msg = "zodiac: lost connection to server";
            self.quit = true;
        }
    }

    fn send_resize(&mut self) {
        let (rows, cols) = self.main_size;
        self.sent_size = self.main_size;
        self.resized_at = Some(Instant::now());
        // Cell size rides along — it changes with font-size changes, which
        // always arrive as a resize.
        let cell = crate::kitty::cell_size().unwrap_or((0, 0));
        let mut data = [0u8; 8];
        data[..2].copy_from_slice(&rows.to_le_bytes());
        data[2..4].copy_from_slice(&cols.to_le_bytes());
        data[4..6].copy_from_slice(&cell.0.to_le_bytes());
        data[6..8].copy_from_slice(&cell.1.to_le_bytes());
        self.send(T_RESIZE, 0, &data.clone());
    }

    fn active_id(&self) -> Option<u64> {
        self.panes.get(self.active).map(|p| p.id)
    }

    fn pane_by_id(&mut self, id: u64) -> Option<&mut CPane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn focus(&mut self, idx: usize) {
        self.active = idx;
        if let Some(p) = self.panes.get_mut(idx) {
            p.clear_flags();
            let id = p.id;
            self.send(T_FOCUS, id, &[]);
        }
    }

    /// Panes whose name fuzzy-matches `pattern`, best match first, as
    /// (pane index, score). An empty pattern matches nothing — Jump is
    /// for typing a few characters, not browsing the full list.
    fn jump_matches(&self, pattern: &str) -> Vec<(usize, i32)> {
        if pattern.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(usize, i32)> = self
            .panes
            .iter()
            .enumerate()
            .filter_map(|(i, p)| fuzzy_score(&p.name, pattern).map(|s| (i, s)))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1));
        out
    }

    fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::SrvGone => {
                if !self.quit {
                    self.exit_msg = "zodiac: lost connection to server (another client attached, or the server exited)";
                    self.quit = true;
                }
            }
            AppEvent::Srv(f) => self.handle_frame(f),
            AppEvent::Chat(ev) => self.handle_chat(ev),
            AppEvent::Term(event) => match event {
                Event::Key(key) => self.handle_key(key),
                Event::Mouse(m) => self.handle_mouse(m),
                Event::Paste(text) => {
                    if self.home && self.chat_focus {
                        for c in text.chars() {
                            if self.chat_input.chars().count() >= 500 {
                                break;
                            }
                            self.chat_input.push(if c == '\n' { ' ' } else { c });
                        }
                        return;
                    }
                    if let Some(id) = self.active_id() {
                        let bracketed = self
                            .pane_by_id(id)
                            .map(|p| p.parser.screen().bracketed_paste())
                            .unwrap_or(false);
                        let out = if bracketed {
                            let mut v = b"\x1b[200~".to_vec();
                            v.extend_from_slice(text.as_bytes());
                            v.extend_from_slice(b"\x1b[201~");
                            v
                        } else {
                            text.replace('\n', "\r").into_bytes()
                        };
                        self.send_input(id, &out);
                    }
                }
                _ => {}
            },
        }
    }

    /// Mouse: selection + copy stays inside the pane area (the sidebar can
    /// never end up in the clipboard), sidebar clicks focus panes, and inner
    /// apps that asked for mouse reporting get events forwarded — hold Shift
    /// to select in those panes anyway (the usual terminal convention).
    fn handle_mouse(&mut self, m: MouseEvent) {
        use MouseEventKind as K;
        if matches!(self.mode, Mode::Settings | Mode::SettingsEdit { .. }) {
            return;
        }
        if self.home {
            let pos = Position::new(m.column, m.row);
            // Chat panel: click focuses, wheel scrolls the transcript.
            if self.chat_rect.width > 0 && self.chat_rect.contains(pos) {
                match m.kind {
                    K::Down(MouseButton::Left) => self.chat_focus = true,
                    K::ScrollUp => self.chat_scroll_by(3),
                    K::ScrollDown => self.chat_scroll_by(-3),
                    _ => {}
                }
                return;
            }
            if let K::Down(MouseButton::Left) = m.kind {
                self.chat_focus = false;
                if let Some(i) = self
                    .home_cards
                    .iter()
                    .position(|(r, _, _, _)| r.contains(pos))
                {
                    self.home_sel = i;
                    let id = self.home_cards[i].1;
                    if let Some(idx) = self.panes.iter().position(|p| p.id == id) {
                        self.focus(idx);
                    }
                    self.leave_home();
                }
            }
            return;
        }
        let pos = Position::new(m.column, m.row);
        let in_main = self.main_rect.contains(pos);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        match m.kind {
            K::Down(MouseButton::Left) => {
                self.copied_at = None;
                if self.sidebar_rect.contains(pos) {
                    // Row math uses the inner rect: a surround frame shifts
                    // the first row down by one; border clicks do nothing.
                    if self.sidebar_inner.contains(pos) {
                        let idx = (m.row - self.sidebar_inner.y) as usize;
                        if idx < self.panes.len() {
                            self.focus(idx);
                        }
                    }
                    self.selection = None;
                    return;
                }
                if !in_main {
                    self.selection = None;
                    return;
                }
                if !shift && self.forward_mouse(&m) {
                    self.selection = None;
                    return;
                }
                let cell = self.main_cell(m.column, m.row);
                self.selection = Some(Sel {
                    pane: self.active_id().unwrap_or(0),
                    anchor: cell,
                    head: cell,
                });
                self.selecting = true;
            }
            K::Drag(MouseButton::Left) => {
                if self.selecting {
                    let cell = self.main_cell(m.column, m.row);
                    if let Some(s) = &mut self.selection {
                        s.head = cell;
                    }
                } else if !shift {
                    self.forward_mouse(&m);
                }
            }
            K::Up(MouseButton::Left) => {
                if self.selecting {
                    self.selecting = false;
                    self.copy_selection();
                } else if !shift {
                    self.forward_mouse(&m);
                }
            }
            K::ScrollUp | K::ScrollDown => {
                if !in_main {
                    return;
                }
                if !shift && self.forward_mouse(&m) {
                    return;
                }
                let up = matches!(m.kind, K::ScrollUp);
                let Some(p) = self.panes.get_mut(self.active) else {
                    return;
                };
                if p.parser.screen().alternate_screen() {
                    // Fullscreen app without mouse reporting: emulate the
                    // terminals' "alternate scroll" — wheel becomes arrows.
                    let one = match (up, p.parser.screen().application_cursor()) {
                        (true, true) => "\x1bOA",
                        (true, false) => "\x1b[A",
                        (false, true) => "\x1bOB",
                        (false, false) => "\x1b[B",
                    };
                    let bytes = one.repeat(3).into_bytes();
                    if let Some(id) = self.active_id() {
                        self.send_input(id, &bytes);
                    }
                } else {
                    p.scroll_by(if up { 3 } else { -3 });
                }
            }
            _ => {
                if in_main && !shift {
                    self.forward_mouse(&m);
                }
            }
        }
    }

    /// Forward a mouse event to the active pane if the app inside requested
    /// mouse reporting. Returns true if the event was sent.
    fn forward_mouse(&mut self, m: &MouseEvent) -> bool {
        let Some(p) = self.panes.get(self.active) else {
            return false;
        };
        let screen = p.parser.screen();
        let (x, y) = (
            m.column.saturating_sub(self.main_rect.x),
            m.row.saturating_sub(self.main_rect.y),
        );
        let Some(bytes) = encode_mouse(
            m,
            x,
            y,
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding(),
        ) else {
            return false;
        };
        if let Some(id) = self.active_id() {
            let typ = if self.server_mouse_gate {
                T_MOUSE
            } else {
                T_INPUT
            };
            self.send(typ, id, &bytes);
        }
        true
    }

    /// Clamp an absolute screen position into the pane area and convert to
    /// pane-relative (row, col) — dragging past an edge selects to that edge.
    fn main_cell(&self, col: u16, row: u16) -> (u16, u16) {
        let r = self.main_rect;
        let x = col.clamp(r.x, r.x + r.width.saturating_sub(1).max(0)) - r.x;
        let y = row.clamp(r.y, r.y + r.height.saturating_sub(1).max(0)) - r.y;
        (y, x)
    }

    fn copy_selection(&mut self) {
        let Some(sel) = self.selection else {
            return;
        };
        let Some(p) = self.panes.iter().find(|p| p.id == sel.pane) else {
            self.selection = None;
            return;
        };
        let ((sr, sc), (er, ec)) = sel.normalized();
        let cols = p.parser.screen().size().1;
        let text = p.parser.screen().contents_between(sr, sc, er, (ec + 1).min(cols));
        if text.chars().all(char::is_whitespace) {
            self.selection = None;
            return;
        }
        copy_to_clipboard(&text);
        self.copied_at = Some(Instant::now());
    }

    fn send_input(&mut self, id: u64, bytes: &[u8]) {
        if let Some(p) = self.pane_by_id(id) {
            if p.scroll != 0 {
                p.set_scroll(0);
            }
        }
        self.send(T_INPUT, id, bytes);
    }

    fn handle_frame(&mut self, f: SrvFrame) {
        match f.typ {
            T_HELLO => {
                if let Ok(h) = serde_json::from_slice::<Hello>(&f.data) {
                    self.server_mouse_gate = h.mouse_gate;
                    let (rows, cols) = self.main_size;
                    self.panes = h
                        .panes
                        .into_iter()
                        .map(|hp| {
                            let mut p = CPane::new(hp.id, hp.name, rows, cols);
                            p.activity = hp.activity;
                            p.attention = hp.attention;
                            p.last_output = hp
                                .last_ms
                                .and_then(|ms| Instant::now().checked_sub(Duration::from_millis(ms)));
                            p
                        })
                        .collect();
                    self.active = self
                        .panes
                        .iter()
                        .position(|p| p.id == h.active)
                        .unwrap_or(0);
                }
            }
            T_REPLAY => {
                if let Some(p) = self.pane_by_id(f.id) {
                    p.parser.process(&f.data);
                    let _ = p.poll_bell();
                }
            }
            T_OUTPUT => {
                let active_id = self.active_id();
                // Output right after a resize is the SIGWINCH repaint storm
                // (every inner app redraws), not agent activity — don't let
                // a sidebar/zoom toggle light up every pane's spinner.
                let squelch = self
                    .resized_at
                    .is_some_and(|t| t.elapsed() < RESIZE_SQUELCH);
                if let Some(p) = self.pane_by_id(f.id) {
                    p.parser.process(&f.data);
                    if !squelch {
                        p.last_output = Some(Instant::now());
                    }
                    // Braille frames only appear mid-work, so this stamp is
                    // trustworthy even during a repaint storm.
                    if title_state(p.parser.screen().title()) == TitleState::Working {
                        p.last_title_working = Some(Instant::now());
                    }
                    let bell = p.poll_bell();
                    if Some(f.id) != active_id {
                        p.activity = !squelch || p.activity;
                        if bell && !p.attention {
                            p.attention = true;
                            notify(
                                &format!("{} needs attention", p.name),
                                &format!("zodiac session '{}'", self.session),
                            );
                        }
                    }
                }
            }
            T_STATE => {
                if let Ok(s) = serde_json::from_slice::<SessionState>(&f.data) {
                    self.home_sel = self.home_sel.min(s.panes.len().saturating_sub(1));
                    self.home_state = Some(s);
                }
            }
            T_PANE_OPENED => {
                let name = String::from_utf8_lossy(&f.data).into_owned();
                let (rows, cols) = self.main_size;
                self.panes.push(CPane::new(f.id, name, rows, cols));
                self.active = self.panes.len() - 1;
            }
            T_PANE_RENAMED => {
                // Server-side auto-naming — adopt unless the user is mid-
                // rename on this very pane (don't fight the editor).
                let editing = matches!(self.mode, Mode::Rename { .. })
                    && self.panes.get(self.active).is_some_and(|p| p.id == f.id);
                if !editing {
                    let name = String::from_utf8_lossy(&f.data).into_owned();
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == f.id) {
                        p.name = name;
                    }
                }
            }
            T_GFX_STATE => {
                if let Ok(snap) = serde_json::from_slice::<crate::gfx::GfxSnapshot>(&f.data) {
                    let mut dead = Vec::new();
                    if let Some(p) = self.pane_by_id(f.id) {
                        let live: std::collections::HashSet<(u32, u32)> =
                            snap.images.iter().copied().collect();
                        p.images.retain(|id, img| {
                            let keep = live.contains(&(*id, img.ver));
                            if !keep {
                                dead.extend(img.outer);
                            }
                            keep
                        });
                        p.gfx = snap;
                    }
                    self.outer_dead.extend(dead);
                }
            }
            T_GFX_IMG => {
                if let Some(hdr) = GfxImgHdr::decode(&f.data) {
                    let chunk = &f.data[GFX_IMG_HDR..];
                    let mut dead = Vec::new();
                    if let Some(p) = self.pane_by_id(f.id) {
                        let buf = p.partial.entry(hdr.img).or_default();
                        if hdr.off == 0 {
                            buf.clear();
                        }
                        buf.extend_from_slice(chunk);
                        if buf.len() as u32 >= hdr.total {
                            let data = std::mem::take(buf);
                            p.partial.remove(&hdr.img);
                            // a retransmitted image obsoletes its outer copy
                            if let Some(old) = p.images.get(&hdr.img).and_then(|i| i.outer) {
                                dead.push(old);
                            }
                            p.images.insert(
                                hdr.img,
                                CImg {
                                    ver: hdr.ver,
                                    format: hdr.format,
                                    zlib: hdr.zlib,
                                    w: hdr.w,
                                    h: hdr.h,
                                    data,
                                    outer: None,
                                },
                            );
                        }
                    }
                    self.outer_dead.extend(dead);
                }
            }
            T_PANE_CLOSED => {
                if let Some(i) = self.panes.iter().position(|p| p.id == f.id) {
                    let dead: Vec<u32> = self.panes[i]
                        .images
                        .values()
                        .filter_map(|img| img.outer)
                        .collect();
                    self.outer_dead.extend(dead);
                    self.pid_map.retain(|(pane, _), _| *pane != f.id);
                    self.panes.remove(i);
                    if self.panes.is_empty() {
                        self.exit_msg = "zodiac: session ended (last pane closed)";
                        self.quit = true;
                    } else if self.active >= self.panes.len() {
                        self.focus(self.panes.len() - 1);
                    } else if i < self.active {
                        self.active -= 1;
                    } else if i == self.active {
                        self.focus(self.active);
                    }
                }
            }
            T_SERVER_EXIT => {
                self.exit_msg = "zodiac: server shut down";
                self.quit = true;
            }
            _ => {}
        }
    }

    /// What Alt+⇧R would raise, read from the session snapshot: one line
    /// per pane that had an agent, plus how old the picture is. The server
    /// does the actual work (`T_RESTORE`) — this is only the confirmation.
    fn restore_preview(&self) -> Mode {
        let Some(snap) = crate::snapshot::best(&self.session) else {
            return Mode::Restore {
                lines: Vec::new(),
                age: String::new(),
            };
        };
        let home = std::env::var("HOME").unwrap_or_default();
        let lines = snap
            .panes
            .iter()
            .filter(|p| p.agent.is_some())
            .map(|p| {
                let what = match (p.agent.as_deref(), p.model.as_deref()) {
                    (Some(a), Some(m)) => format!("{a} ({m})"),
                    (Some(a), None) => a.to_string(),
                    _ => "agent".to_string(),
                };
                let where_ = p.cwd.as_deref().unwrap_or("");
                let where_ = match where_.strip_prefix(&home) {
                    Some(rest) if !home.is_empty() => format!("~{rest}"),
                    _ => where_.to_string(),
                };
                let chat = if p.chat_id.is_some() { " ↩ chat" } else { "" };
                format!("{:>2}  {what}{chat}  {where_}", p.index)
            })
            .collect();
        Mode::Restore {
            lines,
            age: ago(snap.saved_at),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.selection = None;
        self.selecting = false;

        if let Mode::Restore { lines, .. } = &self.mode {
            let anything = !lines.is_empty();
            match key.code {
                KeyCode::Enter if anything => {
                    self.send(T_RESTORE, 0, &[]);
                    self.mode = Mode::Normal;
                }
                _ => self.mode = Mode::Normal,
            }
            return;
        }

        if let Mode::Rename { buf } = &mut self.mode {
            match key.code {
                KeyCode::Enter => {
                    let name = buf.trim().to_string();
                    self.mode = Mode::Normal;
                    if let Some(p) = self.panes.get_mut(self.active) {
                        let id = p.id;
                        if name.is_empty() {
                            // Empty rename un-pins: the server reverts to
                            // auto-naming and answers with T_PANE_RENAMED.
                            self.send(T_RENAME, id, &[]);
                        } else {
                            p.name = name.clone();
                            self.send(T_RENAME, id, name.as_bytes());
                        }
                    }
                }
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if buf.chars().count() < 40 {
                        buf.push(c);
                    }
                }
                _ => {}
            }
            return;
        }

        if matches!(self.mode, Mode::Jump { .. }) {
            // Worked on an owned copy of the buffer: fuzzy-matching needs
            // `&self` (for `self.panes`), which a `&mut self.mode` borrow
            // of `buf` in place would block.
            let mut buf = match &self.mode {
                Mode::Jump { buf } => buf.clone(),
                _ => unreachable!(),
            };
            match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    return;
                }
                KeyCode::Enter => {
                    let matches = self.jump_matches(&buf);
                    if let Some(&(idx, _)) = matches.get(self.jump_sel) {
                        self.mode = Mode::Normal;
                        if self.home {
                            self.leave_home();
                        }
                        self.focus(idx);
                    }
                    return;
                }
                KeyCode::Backspace => {
                    buf.pop();
                    self.jump_sel = 0;
                }
                KeyCode::Up => self.jump_sel = self.jump_sel.saturating_sub(1),
                KeyCode::Down => {
                    let n = self.jump_matches(&buf).len();
                    if self.jump_sel + 1 < n {
                        self.jump_sel += 1;
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if buf.chars().count() < 40 {
                        buf.push(c);
                    }
                    self.jump_sel = 0;
                }
                _ => {}
            }
            self.mode = Mode::Jump { buf };
            return;
        }

        if let Mode::SettingsEdit { row, buf } = &mut self.mode {
            let row = *row;
            match key.code {
                KeyCode::Enter => {
                    let val = buf.trim().to_string();
                    self.apply_settings_edit(row, val);
                    self.mode = Mode::Settings;
                }
                KeyCode::Esc => self.mode = Mode::Settings,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if buf.chars().count() < 200 {
                        buf.push(c);
                    }
                }
                _ => {}
            }
            return;
        }

        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if let Mode::Settings = self.mode {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                KeyCode::Char('s') | KeyCode::Char('S') if ctrl => self.mode = Mode::Normal,
                KeyCode::Up => {
                    self.settings_row = (self.settings_row + SETTINGS_ROWS - 1) % SETTINGS_ROWS
                }
                KeyCode::Down | KeyCode::Tab => {
                    self.settings_row = (self.settings_row + 1) % SETTINGS_ROWS
                }
                KeyCode::Left => {
                    if self.settings_row < TEXT_SETTINGS_START {
                        self.cycle_setting(-1)
                    }
                }
                KeyCode::Right | KeyCode::Enter => {
                    if self.settings_row < TEXT_SETTINGS_START {
                        self.cycle_setting(1)
                    } else {
                        self.start_settings_edit()
                    }
                }
                _ => {}
            }
            return;
        }
        // The pairing QR overlay is modal: swallow every key but the ones
        // that close it, same shape as the Mode::Settings block above.
        if self.pair_open {
            match key.code {
                KeyCode::Esc => self.pair_open = false,
                KeyCode::Char('p') | KeyCode::Char('P') if alt => self.pair_open = false,
                _ => {}
            }
            return;
        }
        if ctrl && !alt && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S')) {
            self.mode = Mode::Settings;
            self.settings_row = 0;
            return;
        }
        // Alt+~ (or Alt+`) toggles the home page.
        if alt && !ctrl && matches!(key.code, KeyCode::Char('~') | KeyCode::Char('`')) {
            self.toggle_home();
            return;
        }
        // Alt+P: pairing QR for the astrolabe phone app — opens the home
        // page if needed (the overlay only makes sense there).
        if alt && !ctrl && matches!(key.code, KeyCode::Char('p') | KeyCode::Char('P')) {
            if !self.home {
                self.toggle_home();
            }
            self.open_pair_overlay();
            return;
        }
        // Alt+G: speak with the wizard (jumps to the home page if needed).
        if alt && !ctrl && matches!(key.code, KeyCode::Char('g') | KeyCode::Char('G')) {
            if self.chat_tx.is_some() {
                if !self.home {
                    self.toggle_home();
                    self.chat_focus = true;
                } else {
                    self.chat_focus = !self.chat_focus;
                }
            }
            return;
        }
        // A pending cast chip takes every key as y/n until it's answered —
        // no typing past it, no scrolling around it.
        if self.home && self.chat_focus && !alt && self.chat_action.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.chat_decide(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.chat_decide(false),
                _ => {}
            }
            return;
        }
        // A focused chatbox swallows all plain keys.
        if self.home && self.chat_focus && !alt {
            match key.code {
                KeyCode::Esc => {
                    // Mid-stream, Esc cuts the reply short instead of
                    // leaving the panel — a second Esc leaves it once the
                    // stream has actually stopped.
                    if self.chat_streaming {
                        if let Some(c) = &self.chat_cancel {
                            c.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    } else {
                        self.chat_focus = false;
                    }
                }
                KeyCode::Enter => self.chat_submit(),
                KeyCode::Backspace => {
                    self.chat_input.pop();
                }
                KeyCode::Up => self.chat_scroll_by(1),
                KeyCode::Down => self.chat_scroll_by(-1),
                KeyCode::PageUp => self.chat_scroll_by(10),
                KeyCode::PageDown => self.chat_scroll_by(-10),
                KeyCode::Char(c) if !ctrl => {
                    if self.chat_input.chars().count() < 500 {
                        self.chat_input.push(c);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.home {
            if self.handle_home_key(key) {
                return;
            }
            if !alt {
                return; // nothing else types into a pane from the home page
            }
            // Alt combos still work; the pane-focusing ones leave home.
            if matches!(key.code, KeyCode::Up | KeyCode::Down)
                || matches!(key.code, KeyCode::Char(c) if c.is_ascii_digit())
            {
                self.leave_home();
            }
        }

        if alt && !ctrl {
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.send(T_NEW_PANE, 0, &[]);
                    return;
                }
                KeyCode::Char('w') | KeyCode::Char('W') => {
                    if let Some(id) = self.active_id() {
                        self.send(T_CLOSE_PANE, id, &[]);
                    }
                    return;
                }
                KeyCode::Char('Q') => {
                    self.send(T_SHUTDOWN, 0, &[]);
                    return;
                }
                KeyCode::Char('q') => {
                    if shift {
                        self.send(T_SHUTDOWN, 0, &[]);
                    } else {
                        self.send(T_DETACH, 0, &[]);
                        self.quit = true;
                    }
                    return;
                }
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    self.collapsed = !self.collapsed;
                    return;
                }
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.zoom = !self.zoom;
                    return;
                }
                KeyCode::Char('R') => {
                    self.mode = self.restore_preview();
                    return;
                }
                KeyCode::Char('r') => {
                    if shift {
                        self.mode = self.restore_preview();
                    } else if let Some(p) = self.panes.get(self.active) {
                        self.mode = Mode::Rename {
                            buf: p.name.clone(),
                        };
                    }
                    return;
                }
                KeyCode::Char('/') => {
                    self.mode = Mode::Jump { buf: String::new() };
                    self.jump_sel = 0;
                    return;
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = c as usize - '1' as usize;
                    if idx < self.panes.len() {
                        self.focus(idx);
                    }
                    return;
                }
                KeyCode::Up => {
                    if self.active > 0 {
                        self.focus(self.active - 1);
                    }
                    return;
                }
                KeyCode::Down => {
                    if self.active + 1 < self.panes.len() {
                        self.focus(self.active + 1);
                    }
                    return;
                }
                KeyCode::PageUp => {
                    if self.active > 0 {
                        self.panes.swap(self.active, self.active - 1);
                        self.active -= 1;
                        if let Some(id) = self.active_id() {
                            self.send(T_MOVE, id, &[0]);
                        }
                    }
                    return;
                }
                KeyCode::PageDown => {
                    if self.active + 1 < self.panes.len() {
                        self.panes.swap(self.active, self.active + 1);
                        self.active += 1;
                        if let Some(id) = self.active_id() {
                            self.send(T_MOVE, id, &[1]);
                        }
                    }
                    return;
                }
                _ => {}
            }
        }

        if shift && !alt && !ctrl {
            match key.code {
                KeyCode::PageUp => {
                    let half = (self.main_size.0 / 2).max(1) as isize;
                    if let Some(p) = self.panes.get_mut(self.active) {
                        p.scroll_by(half);
                    }
                    return;
                }
                KeyCode::PageDown => {
                    let half = (self.main_size.0 / 2).max(1) as isize;
                    if let Some(p) = self.panes.get_mut(self.active) {
                        p.scroll_by(-half);
                    }
                    return;
                }
                _ => {}
            }
        }

        if let Some(id) = self.active_id() {
            let app_cursor = self
                .pane_by_id(id)
                .map(|p| p.parser.screen().application_cursor())
                .unwrap_or(false);
            if let Some(bytes) = encode_key(&key, app_cursor) {
                self.send_input(id, &bytes);
            }
        }
    }

    /// Whether the pane's agent is working right now, regardless of focus —
    /// the sidebar spinner shows on the active pane too. A braille title
    /// frame means working; "✳" is ambiguous: Claude Code's title spinner
    /// cycles ✳/⠂/⠐/… while working and merely rests on ✳ when idle. Those
    /// mid-work rest frames are bridged by a braille frame seen moments ago
    /// — fresh output alone doesn't count, or every claude pane reads
    /// "working" for seconds after its answer's final render (or any idle
    /// repaint). Titles carrying no state (opencode & friends) still fall
    /// back to plain output recency; recency never counts for unknown
    /// programs — an ordinary TUI emits output forever.
    fn working(&self, i: usize) -> bool {
        let p = &self.panes[i];
        let title = p.parser.screen().title();
        let recent = p.last_output.is_some_and(|t| t.elapsed() < IN_PROGRESS_WINDOW);
        match title_state(title) {
            TitleState::Working => true,
            TitleState::Idle => {
                recent
                    && p.last_title_working
                        .is_some_and(|t| t.elapsed() < TITLE_BRIDGE)
            }
            TitleState::Unknown => recent && agent_from_title(title).is_some(),
        }
    }

    /// Status color for a background pane: red = rang the bell (wants
    /// approval/input); orange = working (shown only as the spinner — the
    /// row text stays gray); green = finished since last viewed.
    /// Focused pane has no color status (its sticky state is cleared),
    /// though the working spinner still shows there via `working()`.
    fn status_color(&self, i: usize) -> Option<Color> {
        if i == self.active {
            return None;
        }
        let p = &self.panes[i];
        if p.attention {
            return Some(Color::Red);
        }
        if self.working(i) {
            Some(Color::Indexed(208))
        } else if p.activity {
            Some(Color::Green)
        } else {
            None
        }
    }

    fn any_working(&self) -> bool {
        (0..self.panes.len()).any(|i| self.working(i))
    }

    fn toggle_home(&mut self) {
        if self.home {
            self.leave_home();
        } else {
            self.home = true;
            self.home_sel = self.active;
            self.send(T_QUERY, 0, &[]);
            self.home_queried = Some(Instant::now());
        }
    }

    fn leave_home(&mut self) {
        self.home = false;
        self.selection = None;
        self.chat_focus = false;
    }

    /// Opens the pairing QR overlay, re-reading the astrolabe bridge's
    /// endpoint file. Cheap and rare enough to do synchronously on open
    /// rather than watch continuously — that file only changes when the
    /// bridge itself restarts.
    fn open_pair_overlay(&mut self) {
        self.pair_endpoint = read_bridge_endpoint();
        self.pair_dead = self
            .pair_endpoint
            .as_ref()
            .is_some_and(|(url, _, _)| !endpoint_alive(url));
        self.pair_open = true;
    }

    /// Home-page navigation. Returns true when the key was consumed.
    fn handle_home_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::ALT) {
            return false;
        }
        if !self.home_block_nav.is_empty() {
            return self.handle_home_blocks_key(key);
        }
        let n = self.home_cards.len();
        let cols = self.home_cols.max(1);
        match key.code {
            KeyCode::Esc => self.leave_home(),
            KeyCode::Left => self.home_sel = self.home_sel.saturating_sub(1),
            KeyCode::Right => {
                if n > 0 {
                    self.home_sel = (self.home_sel + 1).min(n - 1);
                }
            }
            KeyCode::Up => {
                if self.home_sel >= cols {
                    self.home_sel -= cols;
                }
            }
            KeyCode::Down => {
                // The last row is often shorter than `cols` — jump into
                // whatever card is there instead of refusing to move just
                // because there's nothing in the exact same column.
                if n > 0 {
                    let total_rows = n.div_ceil(cols);
                    let sel_row = self.home_sel / cols;
                    if sel_row + 1 < total_rows {
                        self.home_sel = (self.home_sel + cols).min(n - 1);
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(&(_, id, _, _)) = self.home_cards.get(self.home_sel) {
                    if let Some(idx) = self.panes.iter().position(|p| p.id == id) {
                        self.focus(idx);
                    }
                    self.leave_home();
                }
            }
            _ => return false,
        }
        true
    }

    /// Arrow keys over the blocks view, where shells and agents fill their
    /// columns independently: Up/Down stay in the selected pane's column,
    /// Left/Right hop to the nearest-row block in the other column.
    fn handle_home_blocks_key(&mut self, key: KeyEvent) -> bool {
        let nav = &self.home_block_nav;
        let n = nav.len();
        let sel = self.home_sel.min(n - 1);
        let (_, row, left) = nav[sel];
        match key.code {
            KeyCode::Esc => self.leave_home(),
            KeyCode::Up => {
                if let Some(i) = (0..sel).rev().find(|&i| nav[i].2 == left) {
                    self.home_sel = i;
                }
            }
            KeyCode::Down => {
                if let Some(i) = (sel + 1..n).find(|&i| nav[i].2 == left) {
                    self.home_sel = i;
                }
            }
            KeyCode::Left | KeyCode::Right => {
                let want_left = key.code == KeyCode::Left;
                if left != want_left {
                    if let Some(i) = (0..n)
                        .filter(|&i| nav[i].2 == want_left)
                        .min_by_key(|&i| nav[i].1.abs_diff(row))
                    {
                        self.home_sel = i;
                    }
                }
            }
            KeyCode::Enter => {
                // By id, not home_cards[home_sel]: home_cards holds only the
                // scrolled-into-view blocks, so its indices drift off nav's.
                let id = nav[sel].0;
                if let Some(idx) = self.panes.iter().position(|p| p.id == id) {
                    self.focus(idx);
                }
                self.leave_home();
            }
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------------------
    // The Wizard chat panel.

    fn chat_note(&mut self, role: u8, text: impl Into<String>) {
        self.chat_log.push((role, text.into()));
        if self.chat_log.len() > 400 {
            self.chat_log.drain(..self.chat_log.len() - 400);
        }
        self.chat_scroll = 0;
    }

    fn handle_chat(&mut self, ev: crate::chat::ChatEvent) {
        use crate::chat::{ChatEvent as E, ChatStatus as S};
        match ev {
            E::Status(s) => {
                let old = self.chat_status;
                self.chat_status = Some(s);
                // The status line above the transcript already shows the
                // current state, so waking up is silent there too — only
                // sleeping/away get a note, since those explain why a
                // message didn't go out.
                if old.is_some() && old != Some(s) {
                    let note = match s {
                        S::Awake => None,
                        S::Waking => Some("the wizard stirs…"),
                        S::Sleeping => Some("the wizard sleeps"),
                        S::Away => Some("the wizard has gone away"),
                    };
                    if let Some(note) = note {
                        self.chat_note(CHAT_NOTE, note);
                    }
                }
            }
            E::Token(t) => {
                self.chat_stream.push_str(&t);
                self.chat_scroll = 0;
            }
            E::Done => {
                self.chat_streaming = false;
                if !self.chat_stream.is_empty() {
                    let t = std::mem::take(&mut self.chat_stream);
                    self.chat_note(CHAT_REPLY, t);
                }
            }
            E::Note(t) => self.chat_note(CHAT_NOTE, t),
            E::Error(t) => {
                self.chat_streaming = false;
                self.chat_note(CHAT_ERROR, t);
            }
            E::Restored(turns) => {
                for (role, text) in turns {
                    let wiz_role = if role == "user" { CHAT_USER } else { CHAT_REPLY };
                    self.chat_note(wiz_role, text);
                }
            }
            E::Cast { desc, action, decision } => {
                self.chat_action = Some((action, decision));
                self.chat_note(CHAT_ACTION, format!("▸ run: {desc}  [y/n]"));
            }
        }
    }

    /// The tail of a pane's visible screen as plain text, trailing blank
    /// rows dropped. This is what lets the wizard advise on the actual
    /// command a card is blocked on rather than on its status label.
    fn pane_excerpt(&self, id: u64, want: usize) -> Option<String> {
        let p = self.panes.iter().find(|p| p.id == id)?;
        let screen = p.parser.screen();
        let (_, cols) = screen.size();
        let rows: Vec<String> = screen
            .rows(0, cols)
            .map(|r| r.trim_end().to_string())
            .collect();
        let end = rows.iter().rposition(|r| !r.is_empty()).map_or(0, |i| i + 1);
        let start = end.saturating_sub(want);
        let text = rows[start..end].join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// Card number (1-based, as shown on the spread) → pane id and name.
    fn card_pane(&self, n: usize) -> Option<(u64, String)> {
        let state = self.home_state.as_ref()?;
        let p = state.panes.iter().find(|p| p.index == n)?;
        Some((p.id, p.name.clone()))
    }

    /// The host a pane is ssh'd into, from the last `T_QUERY` snapshot —
    /// the sidebar has no server-polled data of its own (it's built from
    /// the client's local terminal state), so it borrows this one field
    /// from the same snapshot the home page uses.
    fn ssh_tag(&self, id: u64) -> Option<&str> {
        self.home_state
            .as_ref()?
            .panes
            .iter()
            .find(|p| p.id == id)?
            .ssh
            .as_deref()
    }

    /// A compact live description of the card spread for the model, with
    /// screen excerpts from the cards worth reading. `force` (a 1-based card
    /// number) always gets a generous excerpt — used by `/why`, where one
    /// card is the whole question.
    fn chat_digest_with(&self, force: Option<usize>) -> String {
        let Some(state) = &self.home_state else {
            return "(no card data yet)".into();
        };
        let mut s = format!(
            "session '{}': {} cards\n",
            state.session,
            state.panes.len()
        );
        for p in &state.panes {
            let agent = match (&p.agent, &p.version) {
                (Some(a), Some(v)) => format!("{a} {}", version_token(v)),
                (Some(a), None) => a.clone(),
                (None, _) => "shell".into(),
            };
            s.push_str(&format!(
                "card {}: '{}' — {agent} — {}{} — up {}{}\n",
                p.index,
                p.name,
                p.status,
                if p.thinking { " (thinking now)" } else { "" },
                fmt_uptime(p.uptime_ms),
                p.cwd
                    .as_deref()
                    .map(|d| format!(" — {}", short_dir(d, 40)))
                    .unwrap_or_default(),
            ));
        }

        // Screen excerpts, most useful first, until the budget runs out: the
        // card in question (if any), then whatever you're looking at, then
        // anything blocked or freshly finished.
        let mut picks: Vec<(usize, usize)> = Vec::new(); // (slot in state.panes, rows wanted)
        for (i, p) in state.panes.iter().enumerate() {
            let want = if force.is_some_and(|n| p.index == n) {
                EXCERPT_FOCUSED * 2
            } else if p.focused {
                EXCERPT_FOCUSED
            } else if p.status == "needs_input" || p.status == "done" {
                EXCERPT_OTHER
            } else {
                continue;
            };
            picks.push((i, want));
        }
        // Widest excerpt first, so the budget goes to the card that matters.
        picks.sort_by(|a, b| b.1.cmp(&a.1));

        let mut used = 0usize;
        for (slot, want) in picks {
            let p = &state.panes[slot];
            let left = EXCERPT_BUDGET.saturating_sub(used);
            // Not enough room left to say anything worth reading.
            if left < 200 {
                break;
            }
            let Some(text) = self.pane_excerpt(p.id, want) else {
                continue;
            };
            let head = format!("\n--- card {} '{}' — screen ---\n", p.index, p.name);
            let room = left.saturating_sub(head.len());
            // Keep the tail; the newest output is the part that matters.
            let text = if text.len() > room {
                let mut cut = text.len() - room;
                while cut < text.len() && !text.is_char_boundary(cut) {
                    cut += 1;
                }
                &text[cut..]
            } else {
                &text[..]
            };
            used += head.len() + text.len();
            s.push_str(&head);
            s.push_str(text);
            s.push('\n');
        }
        s
    }

    /// Execute (or decline) a pending cast — the only place the Wizard's
    /// prompt_pane/send_keys can actually reach a pane's PTY. `action`
    /// names a card by the 1-based number the model saw in the spread;
    /// resolved to a pane id here, since the worker has no socket of its
    /// own to send `T_INPUT` with.
    fn chat_decide(&mut self, yes: bool) {
        use crate::chat::{ActionRequest, ActionOutcome};
        let Some((action, decision)) = self.chat_action.take() else {
            return;
        };
        let outcome = if !yes {
            self.chat_note(CHAT_NOTE, "cast declined".to_string());
            ActionOutcome::Declined
        } else {
            match &action {
                ActionRequest::PromptPane { pane, text } => match self.card_pane(*pane) {
                    Some((id, _)) => {
                        self.send(T_INPUT, id, text.as_bytes());
                        // Same pacing `zodiac prompt` uses: give the pane's
                        // program a beat to register the pasted text as one
                        // block before the Enter that submits it.
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        self.send(T_INPUT, id, b"\r");
                        self.chat_note(CHAT_NOTE, format!("sent to pane {pane}"));
                        ActionOutcome::Sent
                    }
                    None => {
                        let msg = format!("no such card {pane}");
                        self.chat_note(CHAT_ERROR, format!("cast failed: {msg}"));
                        ActionOutcome::Failed(msg)
                    }
                },
                ActionRequest::SendKeys { pane, keys } => match self.card_pane(*pane) {
                    Some((id, _)) => {
                        self.send(T_INPUT, id, &decode_keys(keys));
                        self.chat_note(CHAT_NOTE, format!("sent to pane {pane}"));
                        ActionOutcome::Sent
                    }
                    None => {
                        let msg = format!("no such card {pane}");
                        self.chat_note(CHAT_ERROR, format!("cast failed: {msg}"));
                        ActionOutcome::Failed(msg)
                    }
                },
            }
        };
        let _ = decision.send(outcome);
    }

    fn chat_submit(&mut self) {
        use crate::chat::{ChatCmd, ChatStatus as S};
        let text = std::mem::take(&mut self.chat_input).trim().to_string();
        if text.is_empty() {
            return;
        }
        self.chat_scroll = 0;
        let Some(tx) = self.chat_tx.clone() else {
            self.chat_note(CHAT_ERROR, "the wizard is not configured");
            return;
        };
        let lower = text.to_ascii_lowercase();
        let (verb, rest) = match lower.split_once(char::is_whitespace) {
            Some((v, r)) => (v, r.trim()),
            None => (lower.as_str(), ""),
        };
        match verb {
            "/wake" | "/rise" => {
                let _ = tx.send(ChatCmd::Wake);
            }
            // /dispell and friends are the old names, still accepted.
            "/sleep" | "/dispell" | "/dispel" | "/banish" => {
                let _ = tx.send(ChatCmd::Sleep);
            }
            "/clear" => {
                self.chat_log.clear();
                self.chat_stream.clear();
            }
            // Scry a card into the transcript — purely local, no model.
            "/read" | "/scry" => {
                self.chat_note(CHAT_USER, text.clone());
                match rest.parse::<usize>().ok().and_then(|n| self.card_pane(n)) {
                    Some((id, name)) => match self.pane_excerpt(id, EXCERPT_FOCUSED) {
                        Some(t) => self.chat_note(CHAT_NOTE, format!("card '{name}':\n{t}")),
                        None => self.chat_note(CHAT_NOTE, format!("card '{name}' is blank")),
                    },
                    None => self.chat_note(CHAT_ERROR, "no such card — try /read 3"),
                }
            }
            _ => {
                // `/why <n>` asks about one card, so force its screen in.
                let force = (verb == "/why")
                    .then(|| rest.parse::<usize>().ok())
                    .flatten()
                    .filter(|n| self.card_pane(*n).is_some());
                let text = match force {
                    Some(n) => format!("What is card {n} doing, and what should I do about it?"),
                    None => text,
                };
                if verb == "/why" && force.is_none() {
                    self.chat_note(CHAT_ERROR, "no such card — try /why 3");
                    return;
                }
                self.chat_note(CHAT_USER, text.clone());
                match self.chat_status {
                    Some(S::Awake) => {
                        self.chat_streaming = true;
                        let digest = self.chat_digest_with(force);
                        // `/why` is a card question by construction; anything
                        // else has to earn the spread.
                        let _ = tx.send(ChatCmd::Chat {
                            text,
                            digest,
                            force_panes: force.is_some(),
                            face: self.chat_face().to_string(),
                        });
                    }
                    Some(S::Waking) => {
                        self.chat_note(CHAT_NOTE, "the wizard is still waking — a moment…")
                    }
                    Some(S::Sleeping) => self
                        .chat_note(CHAT_NOTE, "the wizard is sleeping — cast /wake to rouse him"),
                    Some(S::Away) | None => self
                        .chat_note(CHAT_NOTE, "the wizard is away — the tower does not answer"),
                }
            }
        }
    }

    fn chat_scroll_by(&mut self, delta: isize) {
        self.chat_scroll = if delta > 0 {
            // Clamped against the transcript length at draw time.
            self.chat_scroll.saturating_add(delta as usize)
        } else {
            self.chat_scroll.saturating_sub(delta.unsigned_abs())
        };
    }

    /// Panel width for the current terminal width; 0 hides the panel.
    fn chat_panel_width(&self, body_w: u16) -> u16 {
        if self.chat_tx.is_none() {
            return 0;
        }
        let pref: u16 = self
            .settings
            .chat_width
            .parse()
            .unwrap_or(40)
            .clamp(28, 70);
        // Keep at least one card column plus breathing room.
        if body_w < pref + self.card_dims().0 + 8 {
            0
        } else {
            pref
        }
    }

    fn eye_def(&self) -> &'static EyeDef {
        EYES.iter()
            .find(|e| e.name == self.settings.eye)
            .unwrap_or(&EYES[0])
    }

    fn eye(&self) -> char {
        let d = self.eye_def();
        let t = self.anim_start.elapsed().as_millis() as u64 % EYE_PERIOD_MS;
        if t < EYE_BLINK_MS {
            d.closed
        } else {
            d.open
        }
    }

    fn spinner_def(&self) -> &'static SpinnerDef {
        SPINNERS
            .iter()
            .find(|s| s.name == self.settings.spinner)
            .unwrap_or(&SPINNERS[0])
    }

    fn cycle_spinner(&mut self, dir: isize) {
        let cur = SPINNERS
            .iter()
            .position(|s| s.name == self.settings.spinner)
            .unwrap_or(0) as isize;
        let next = (cur + dir).rem_euclid(SPINNERS.len() as isize) as usize;
        self.settings.spinner = SPINNERS[next].name.to_string();
        self.settings.save();
    }

    fn spinner_color(&self) -> Color {
        color_by_name(&self.settings.spinner_color, "orange").1
    }

    fn shimmer_color(&self) -> Color {
        color_by_name(&self.settings.shimmer_color, "white").1
    }

    fn shimmer_ms(&self) -> u64 {
        SHIMMER_SPEEDS
            .iter()
            .find(|(n, _)| *n == self.settings.shimmer_speed)
            .map(|(_, ms)| *ms)
            .unwrap_or(2000)
    }

    fn shimmer_speed_name(&self) -> &'static str {
        SHIMMER_SPEEDS
            .iter()
            .find(|(n, _)| *n == self.settings.shimmer_speed)
            .map(|(n, _)| *n)
            .unwrap_or("normal")
    }

    fn sidebar_frame(&self) -> &'static str {
        pick(SIDEBAR_FRAMES, &self.settings.sidebar_frame, "separator")
    }

    fn sidebar_weight(&self) -> &'static str {
        pick(BORDER_WEIGHTS, &self.settings.sidebar_weight, "normal")
    }

    fn sidebar_color(&self) -> Color {
        color_by_name(&self.settings.sidebar_color, "dark").1
    }

    fn card_outline(&self) -> &'static str {
        pick(CARD_OUTLINES, &self.settings.card_outline, "double")
    }

    fn select_color(&self) -> Color {
        color_by_name(&self.settings.select_color, "gold").1
    }

    fn select_rgb(&self) -> (u8, u8, u8) {
        color_by_name(&self.settings.select_color, "gold").2
    }

    fn select_weight_idx(&self) -> usize {
        SELECT_WEIGHTS
            .iter()
            .position(|(n, _)| *n == self.settings.select_weight)
            .unwrap_or(1) // normal
    }

    fn select_style(&self) -> &'static str {
        pick(SELECT_STYLES, &self.settings.select_style, "glow")
    }

    fn claude_style(&self) -> &'static str {
        pick(CLAUDE_STYLES, &self.settings.claude_style, "hard")
    }

    fn card_numeral_style(&self) -> &'static str {
        pick(CARD_NUMERALS, &self.settings.card_numeral, "roman")
    }

    /// The card's number in the configured style; zodiac wraps after ♓.
    /// "zodiac-white" appends U+FE0E (text presentation) so terminals draw
    /// the plain monochrome symbol instead of the colored emoji disc.
    fn card_numeral(&self, n: usize) -> String {
        match self.card_numeral_style() {
            "arabic" => n.to_string(),
            "zodiac" => ZODIAC[(n - 1) % ZODIAC.len()].to_string(),
            "zodiac-white" => format!("{}\u{FE0E}", ZODIAC[(n - 1) % ZODIAC.len()]),
            _ => roman(n),
        }
    }

    fn cursor_type(&self) -> &'static str {
        pick(CURSOR_TYPES, &self.settings.cursor_style, "auto")
    }

    fn cursor_blink(&self) -> &'static str {
        pick(CURSOR_BLINKS, &self.settings.cursor_blink, "auto")
    }

    fn cursor_color_name(&self) -> &'static str {
        if self.settings.cursor_color == "off" {
            "off"
        } else {
            color_by_name(&self.settings.cursor_color, "orange").0
        }
    }

    /// The pane-cursor tint, or None when tinting is off.
    fn cursor_rgb(&self) -> Option<(u8, u8, u8)> {
        (self.cursor_color_name() != "off")
            .then(|| color_by_name(&self.settings.cursor_color, "orange").2)
    }

    /// Combine the cursor-type/blink settings with the pane's own DECSCUSR
    /// into the parameter sent to the outer terminal. 0 = terminal default;
    /// otherwise odd = blinking, even = steady (1/2 block, 3/4 underline,
    /// 5/6 bar).
    fn cursor_param(&self, pane_style: u8) -> u8 {
        let ty = self.cursor_type();
        let blink = self.cursor_blink();
        if ty == "auto" && blink == "auto" {
            return pane_style;
        }
        let base = match ty {
            "block" => 1,
            "underline" => 3,
            "bar" => 5,
            // orb/circle render via kitty graphics; this base is only the
            // fallback shape for terminals without the protocol. (aleph
            // never falls back — its glyph is text and always renders.)
            "orb" | "circle" | "aleph" => 1,
            // auto: keep the pane's shape; a default-style pane reads as
            // block, the classic terminal default.
            _ => match pane_style {
                3 | 4 => 3,
                5 | 6 => 5,
                _ => 1,
            },
        };
        let blinking = match blink {
            "on" => true,
            "off" => false,
            _ => pane_style % 2 == 1 && pane_style != 0,
        };
        base + u8::from(!blinking)
    }

    fn card_icon_idx(&self) -> usize {
        CARD_ICON_SIZES
            .iter()
            .position(|(n, _)| *n == self.settings.card_icon)
            .unwrap_or(1) // medium
    }

    fn card_size_idx(&self) -> usize {
        CARD_SIZES
            .iter()
            .position(|(n, _, _)| *n == self.settings.card_size)
            .unwrap_or(1) // medium
    }

    /// Card footprint in cells per the card-size setting.
    fn card_dims(&self) -> (u16, u16) {
        let (_, w, h) = CARD_SIZES[self.card_size_idx()];
        (w, h)
    }

    fn card_columns_name(&self) -> &'static str {
        pick(CARD_COLUMNS, &self.settings.card_columns, "auto")
    }

    fn home_view(&self) -> &'static str {
        pick(HOME_VIEWS, &self.settings.home_view, "cards")
    }

    fn home_sep_color(&self) -> Color {
        color_by_name(&self.settings.home_sep_color, "dark").1
    }

    fn chat_face(&self) -> &'static str {
        // "wizard" is the old name for the plain assistant; map it forward so an
        // existing config keeps working.
        if self.settings.chat_face == "wizard" {
            return "assistant";
        }
        pick(CHAT_FACES, &self.settings.chat_face, "assistant")
    }

    /// The sidebar's Block per the frame/weight/color settings. Rounding
    /// only exists for normal-weight corners (Unicode has no thick or
    /// double rounded corners), so thick/double render square either way.
    fn sidebar_block(&self) -> Block<'static> {
        let bt = match (self.sidebar_weight(), self.sidebar_frame()) {
            ("thick", _) => BorderType::Thick,
            ("double", _) => BorderType::Double,
            (_, "rounded") => BorderType::Rounded,
            _ => BorderType::Plain,
        };
        let borders = if self.sidebar_frame() == "separator" {
            Borders::RIGHT
        } else {
            Borders::ALL
        };
        Block::default()
            .borders(borders)
            .border_type(bt)
            .border_style(Style::default().fg(self.sidebar_color()))
    }

    fn cycle_eye(&mut self, dir: isize) {
        let cur = EYES
            .iter()
            .position(|e| e.name == self.settings.eye)
            .unwrap_or(0) as isize;
        let next = (cur + dir).rem_euclid(EYES.len() as isize) as usize;
        self.settings.eye = EYES[next].name.to_string();
        self.settings.save();
    }

    fn cycle_setting(&mut self, dir: isize) {
        match self.settings_row {
            0 => return self.cycle_spinner(dir),
            1 => {
                let cur = color_by_name(&self.settings.spinner_color, "orange").0;
                self.settings.spinner_color = cycle_color_name(cur, dir);
            }
            2 => {
                let cur = color_by_name(&self.settings.shimmer_color, "white").0;
                self.settings.shimmer_color = cycle_color_name(cur, dir);
            }
            3 => {
                let i = SHIMMER_SPEEDS
                    .iter()
                    .position(|(n, _)| *n == self.shimmer_speed_name())
                    .unwrap_or(1) as isize;
                self.settings.shimmer_speed = SHIMMER_SPEEDS
                    [(i + dir).rem_euclid(SHIMMER_SPEEDS.len() as isize) as usize]
                    .0
                    .to_string();
            }
            4 => return self.cycle_eye(dir),
            5 => {
                self.settings.sidebar_frame =
                    cycle_pick(SIDEBAR_FRAMES, self.sidebar_frame(), dir);
            }
            6 => {
                self.settings.sidebar_weight =
                    cycle_pick(BORDER_WEIGHTS, self.sidebar_weight(), dir);
            }
            7 => {
                let cur = color_by_name(&self.settings.sidebar_color, "dark").0;
                self.settings.sidebar_color = cycle_color_name(cur, dir);
            }
            8 => {
                self.settings.home_view = cycle_pick(HOME_VIEWS, self.home_view(), dir);
            }
            9 => {
                let i = self.card_size_idx() as isize;
                self.settings.card_size = CARD_SIZES
                    [(i + dir).rem_euclid(CARD_SIZES.len() as isize) as usize]
                    .0
                    .to_string();
            }
            10 => {
                self.settings.card_columns =
                    cycle_pick(CARD_COLUMNS, self.card_columns_name(), dir);
            }
            11 => {
                let cur = color_by_name(&self.settings.home_sep_color, "dark").0;
                self.settings.home_sep_color = cycle_color_name(cur, dir);
            }
            12 => {
                let i = self.card_icon_idx() as isize;
                self.settings.card_icon = CARD_ICON_SIZES
                    [(i + dir).rem_euclid(CARD_ICON_SIZES.len() as isize) as usize]
                    .0
                    .to_string();
            }
            13 => {
                self.settings.card_outline =
                    cycle_pick(CARD_OUTLINES, self.card_outline(), dir);
            }
            14 => {
                let cur = color_by_name(&self.settings.select_color, "gold").0;
                self.settings.select_color = cycle_color_name(cur, dir);
            }
            15 => {
                let i = self.select_weight_idx() as isize;
                self.settings.select_weight = SELECT_WEIGHTS
                    [(i + dir).rem_euclid(SELECT_WEIGHTS.len() as isize) as usize]
                    .0
                    .to_string();
            }
            16 => {
                self.settings.select_style =
                    cycle_pick(SELECT_STYLES, self.select_style(), dir);
            }
            17 => {
                self.settings.card_numeral =
                    cycle_pick(CARD_NUMERALS, self.card_numeral_style(), dir);
            }
            18 => {
                self.settings.claude_style =
                    cycle_pick(CLAUDE_STYLES, self.claude_style(), dir);
            }
            19 => return self.cycle_finish_sound(dir),
            20 => self.settings.connection_watch = !self.settings.connection_watch,
            21 => {
                self.settings.cursor_style = cycle_pick(CURSOR_TYPES, self.cursor_type(), dir);
            }
            22 => {
                self.settings.cursor_blink =
                    cycle_pick(CURSOR_BLINKS, self.cursor_blink(), dir);
            }
            23 => {
                let mut choices: Vec<&str> = vec!["off"];
                choices.extend(COLOR_CHOICES.iter().map(|(n, _, _)| *n));
                self.settings.cursor_color =
                    cycle_pick(&choices, self.cursor_color_name(), dir);
            }
            24 => self.settings.hide_controls = !self.settings.hide_controls,
            25 => {
                self.settings.chat_face = cycle_pick(CHAT_FACES, self.chat_face(), dir);
            }
            _ => {}
        }
        self.settings.save();
    }

    /// Enter free-text edit mode for the settings row under the cursor
    /// (rows 26-29: chat endpoint/model/ssh/service), seeded with the
    /// field's current value. No-op on cyclable rows.
    fn start_settings_edit(&mut self) {
        let buf = match self.settings_row {
            26 => self.settings.chat_endpoint.clone(),
            27 => self.settings.chat_model.clone(),
            28 => self.settings.chat_ssh.clone(),
            29 => self.settings.chat_service.clone(),
            _ => return,
        };
        self.mode = Mode::SettingsEdit {
            row: self.settings_row,
            buf,
        };
    }

    /// Commit a `Mode::SettingsEdit` buffer back into `Settings` and persist.
    /// Takes effect on the next zodiac launch — the chat worker's config is
    /// built once at startup, not re-read live.
    fn apply_settings_edit(&mut self, row: usize, val: String) {
        match row {
            26 => self.settings.chat_endpoint = val,
            27 => self.settings.chat_model = val,
            28 => self.settings.chat_ssh = val,
            29 => self.settings.chat_service = val,
            _ => {}
        }
        self.settings.save();
    }

    /// Step through off + every ringtone in ~/.config/zodiac/ringtones,
    /// previewing the newly selected sound.
    fn cycle_finish_sound(&mut self, dir: isize) {
        let mut choices = vec!["off".to_string()];
        choices.extend(crate::settings::list_ringtones());
        let cur = self.settings.effective_finish_sound();
        let i = choices.iter().position(|c| *c == cur).unwrap_or(0) as isize;
        let next = (i + dir).rem_euclid(choices.len() as isize) as usize;
        self.settings.finish_sound = choices[next].clone();
        self.settings.save();
        if let Some(path) = self.settings.finish_sound_path() {
            play_sound(&path);
        }
    }

    /// Settings-row label for the finish sound: the ringtone's file stem,
    /// or "off". Truncated to keep the value column aligned.
    fn finish_sound_name(&self) -> String {
        let name = self.settings.effective_finish_sound();
        let stem = std::path::Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(name);
        stem.chars().take(14).collect()
    }

    /// Current frame of the selected working animation. In the collapsed
    /// sidebar only one cell is available, so multi-cell spinners fall back
    /// to the single equalizer bar there.
    fn working_anim(&self, collapsed: bool) -> String {
        let def = self.spinner_def();
        if def.frames.is_empty() || (collapsed && def.width() > 1) {
            return self.equalizer(if collapsed { 1 } else { EQ_BARS });
        }
        let i = (self.anim_start.elapsed().as_millis() as u64 / def.interval) as usize
            % def.frames.len();
        def.frames[i].to_string()
    }

    /// Claude-style shimmer: a bright band sweeping left-to-right across
    /// the text. Characters far from the band render dimmed, the band core
    /// is white, with a normal-brightness fringe between — modifiers of the
    /// base style (bold, underline) are preserved throughout, so the
    /// focused row keeps its underline while shimmering.
    fn shimmer_spans(&self, text: &str, base: Style) -> Vec<Span<'static>> {
        let period = self.shimmer_ms();
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len() as f32;
        let band = 2.5f32;
        let t = (self.anim_start.elapsed().as_millis() as u64 % period) as f32 / period as f32;
        let center = t * (n + 2.0 * band) - band;
        chars
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let d = (i as f32 - center).abs();
                let s = if d < 0.9 {
                    base.fg(self.shimmer_color())
                } else if d < 1.9 {
                    base
                } else {
                    base.add_modifier(Modifier::DIM)
                };
                Span::styled(c.to_string(), s)
            })
            .collect()
    }

    /// One frame of the working animation. Follows the CSS keyframes: hold
    /// full height, shrink to half over the first 20% of the cycle, stretch
    /// back by 45%, hold; each bar's cycle starts 300ms after the previous.
    fn equalizer(&self, bars: usize) -> String {
        let now = self.anim_start.elapsed().as_millis() as u64;
        (0..bars)
            .map(|k| {
                let t = ((now + EQ_CYCLE_MS - k as u64 * EQ_STAGGER_MS) % EQ_CYCLE_MS) as f32
                    / EQ_CYCLE_MS as f32;
                let h = if t < 0.20 {
                    1.0 - (t / 0.20) * 0.5
                } else if t < 0.45 {
                    0.5 + ((t - 0.20) / 0.25) * 0.5
                } else {
                    1.0
                };
                EQ_GLYPHS[((h * 8.0).ceil() as usize).clamp(1, 8) - 1]
            })
            .collect()
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let [body, status] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        // A surround frame spends two extra columns on borders; widen so
        // the usable inner width stays the same as with the separator.
        let extra = if self.sidebar_frame() == "separator" { 0 } else { 2 };
        let sb_w = if self.zoom {
            0
        } else if self.collapsed {
            SIDEBAR_COLLAPSED + extra
        } else {
            SIDEBAR_WIDTH + extra
        };
        let [sidebar, main] =
            Layout::horizontal([Constraint::Length(sb_w), Constraint::Min(1)]).areas(body);

        self.main_size = (main.height.max(2), main.width.max(10));
        for p in &mut self.panes {
            p.resize(self.main_size.0, self.main_size.1);
        }

        if self.home {
            self.sidebar_rect = Rect::default();
            self.main_rect = Rect::default();
            let chat_w = self.chat_panel_width(body.width);
            if chat_w > 0 {
                let [cards, chat] =
                    Layout::horizontal([Constraint::Min(1), Constraint::Length(chat_w)])
                        .areas(body);
                self.draw_home(f, cards);
                self.draw_chat(f, chat);
            } else {
                self.chat_rect = Rect::default();
                self.chat_art_rect = Rect::default();
                self.draw_home(f, body);
            }
            self.draw_status(f, status);
            if matches!(self.mode, Mode::Settings | Mode::SettingsEdit { .. }) {
                self.draw_settings(f, area);
            }
            if matches!(self.mode, Mode::Jump { .. }) {
                self.draw_jump(f, area);
            }
            if matches!(self.mode, Mode::Restore { .. }) {
                self.draw_restore(f, area);
            }
            return;
        }

        self.draw_sidebar(f, sidebar);

        self.sidebar_rect = sidebar;
        self.main_rect = main;

        if let Some(p) = self.panes.get(self.active) {
            let screen = p.parser.screen();
            f.render_widget(TermView { screen }, main);
            // Orb/aleph cursors hide the hardware cursor — the overlay (or
            // the glyph below) marks the cell instead.
            let cursor_vis =
                p.scroll == 0 && !screen.hide_cursor() && matches!(self.mode, Mode::Normal);
            if cursor_vis && !self.orb_active() && !self.aleph_active() {
                let (r, c) = screen.cursor_position();
                if r < main.height && c < main.width {
                    f.set_cursor_position((main.x + c, main.y + r));
                }
            }
            // The aleph: א in the cursor color at the cursor cell — the
            // letter of the breath before speech, marking where the next
            // word will come into being. On a cell that already holds a
            // character, the letter yields: the char stays visible under a
            // block-style highlight instead, so editing never hides text.
            if cursor_vis && self.aleph_active() {
                let (r, c) = screen.cursor_position();
                if r < main.height && c < main.width {
                    let occupied = screen
                        .cell(r, c)
                        .is_some_and(|cell| !cell.contents().trim().is_empty());
                    let (ar, ag, ab) = self.orb_color();
                    let cell = &mut f.buffer_mut()[(main.x + c, main.y + r)];
                    if occupied {
                        cell.set_style(
                            Style::default()
                                .fg(Color::Rgb(20, 17, 28))
                                .bg(Color::Rgb(ar, ag, ab)),
                        );
                    } else {
                        cell.set_symbol("א");
                        cell.set_style(
                            Style::default()
                                .fg(Color::Rgb(ar, ag, ab))
                                .add_modifier(Modifier::BOLD),
                        );
                    }
                }
            }
        }

        // Selection highlight over the pane area.
        if let Some(sel) = self.selection {
            let on_active = self
                .panes
                .get(self.active)
                .is_some_and(|p| p.id == sel.pane);
            if on_active && main.width > 0 && main.height > 0 {
                let ((sr, sc), (er, ec)) = sel.normalized();
                let buf = f.buffer_mut();
                for r in sr..=er.min(main.height - 1) {
                    let (c0, c1) = if sr == er {
                        (sc, ec)
                    } else if r == sr {
                        (sc, main.width - 1)
                    } else if r == er {
                        (0, ec)
                    } else {
                        (0, main.width - 1)
                    };
                    for c in c0..=c1.min(main.width - 1) {
                        buf[(main.x + c, main.y + r)].modifier |= Modifier::REVERSED;
                    }
                }
            }
        }

        self.draw_status(f, status);

        if matches!(self.mode, Mode::Settings | Mode::SettingsEdit { .. }) {
            self.draw_settings(f, area);
        }
        if matches!(self.mode, Mode::Jump { .. }) {
            self.draw_jump(f, area);
        }
        if matches!(self.mode, Mode::Restore { .. }) {
            self.draw_restore(f, area);
        }
    }

    /// Floating overlay for `Mode::Restore`: what the last snapshot holds,
    /// awaiting Enter. Panes already running their agent are skipped
    /// server-side, so this lists the snapshot, not the delta.
    fn draw_restore(&self, f: &mut Frame, area: Rect) {
        let Mode::Restore { lines, age } = &self.mode else {
            return;
        };
        if area.width < 20 || area.height < 6 {
            return;
        }
        let w = 60.min(area.width.saturating_sub(4)).max(24);
        // Borders + the "snapshot from …" header, a blank row, and the key
        // hint: five rows that aren't the list itself.
        let h = (lines.len().max(1) as u16 + 5).min(area.height.saturating_sub(2));
        let rect = Rect {
            x: (area.width.saturating_sub(w)) / 2,
            y: (area.height.saturating_sub(h)) / 3,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" raise the last session ")
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let mut out: Vec<Line> = Vec::new();
        if lines.is_empty() {
            out.push(Line::from(Span::styled(
                "no snapshot with agents in it — nothing to raise",
                Style::default().fg(Color::DarkGray).italic(),
            )));
            out.push(Line::from(""));
            out.push(Line::from(Span::styled(
                "any key to close",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            out.push(Line::from(Span::styled(
                format!("snapshot from {age}"),
                Style::default().fg(Color::DarkGray).italic(),
            )));
            for l in lines.iter().take(inner.height.saturating_sub(3) as usize) {
                out.push(Line::from(Span::styled(
                    truncate(l, inner.width as usize),
                    Style::default().fg(Color::Gray),
                )));
            }
            out.push(Line::from(""));
            out.push(Line::from(vec![
                Span::styled("Enter", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" to raise them · ", Style::default().fg(Color::DarkGray)),
                Span::styled("Esc", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" to cancel", Style::default().fg(Color::DarkGray)),
            ]));
        }
        f.render_widget(Paragraph::new(out), inner);
    }

    /// Floating overlay for `Mode::Jump`: an input line plus the live
    /// fuzzy-filtered pane list, best match first, current pick
    /// highlighted. Sits in the upper third so it doesn't cover whatever
    /// the pane you're about to jump to is showing.
    fn draw_jump(&self, f: &mut Frame, area: Rect) {
        let Mode::Jump { buf } = &self.mode else { return };
        let matches = self.jump_matches(buf);
        let w = 50.min(area.width.saturating_sub(4)).max(20);
        let list_h = matches.len().min(10) as u16;
        let h = (list_h + 3).min(area.height.saturating_sub(2));
        if area.width < 6 || area.height < 4 {
            return;
        }
        let rect = Rect {
            x: (area.width.saturating_sub(w)) / 2,
            y: (area.height.saturating_sub(h)) / 3,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" jump to pane (Esc to cancel) ")
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let mut lines = vec![Line::from(vec![
            Span::styled("🔎 ", Style::default().fg(Color::Cyan)),
            Span::styled(buf.clone(), Style::default().fg(Color::Yellow)),
            Span::styled("▏", Style::default().fg(Color::Yellow).add_modifier(Modifier::SLOW_BLINK)),
        ])];
        if matches.is_empty() && !buf.is_empty() {
            lines.push(Line::from(Span::styled(
                "no matches",
                Style::default().fg(Color::DarkGray).italic(),
            )));
        }
        let rows = (inner.height as usize).saturating_sub(1);
        for (i, &(idx, _)) in matches.iter().take(rows).enumerate() {
            let p = &self.panes[idx];
            let style = if i == self.jump_sel {
                Style::default().fg(Color::Black).bg(Color::Cyan).bold()
            } else {
                Style::default().fg(Color::Gray)
            };
            let name = truncate(&p.name, inner.width.saturating_sub(4) as usize);
            lines.push(Line::from(Span::styled(format!(" {:>2} {name}", idx + 1), style)));
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_settings(&self, f: &mut Frame, area: Rect) {
        // Wide terminals get a second column: the Controls key reference,
        // always visible here even when the bottom-bar hints are hidden.
        let two_col = area.width >= 92;
        let w = if two_col { 90 } else { 60.min(area.width) };
        let h = 36.min(area.height);
        let rect = Rect {
            x: (area.width - w) / 2,
            y: (area.height - h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Settings ")
            .border_style(Style::default().fg(Color::Cyan));
        let outer = block.inner(rect);
        f.render_widget(block, rect);
        let (inner, controls) = if two_col {
            let [l, r] = Layout::horizontal([Constraint::Length(58), Constraint::Min(1)])
                .areas(outer);
            (l, Some(r))
        } else {
            (outer, None)
        };
        if let Some(r) = controls {
            let cblock = Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(Color::DarkGray));
            let cinner = cblock.inner(r);
            f.render_widget(cblock, r);
            let mut lines = vec![
                Line::default(),
                Line::from(Span::styled(
                    " Controls",
                    Style::default().fg(Color::Cyan).bold(),
                )),
                Line::default(),
            ];
            for (key, what) in CONTROLS {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {key:<12}"), Style::default().fg(Color::Yellow)),
                    Span::styled((*what).to_string(), Style::default().fg(Color::Gray)),
                ]));
            }
            f.render_widget(Paragraph::new(lines), cinner);
        }

        let row = |i: usize, label: &str, value: &str, preview: Vec<Span<'static>>| {
            let sel = self.settings_row == i;
            let marker = if sel { "›" } else { " " };
            let label_style = if sel {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default().bold()
            };
            let mut spans = vec![
                Span::styled(format!("{marker} {label:<18}"), label_style),
                Span::styled(
                    format!("‹ {value:<14} ›"),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(" "),
            ];
            spans.extend(preview);
            Line::from(spans)
        };
        let editing_row = match &self.mode {
            Mode::SettingsEdit { row, buf } => Some((*row, buf.as_str())),
            _ => None,
        };
        let text_row = |i: usize, label: &str, value: &str, placeholder: &str| {
            let sel = self.settings_row == i;
            let editing = editing_row.filter(|(r, _)| *r == i).map(|(_, b)| b);
            let marker = if sel { "›" } else { " " };
            let label_style = if sel {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default().bold()
            };
            let (shown, val_style) = match editing {
                Some(buf) => (
                    format!("{buf}▏"),
                    Style::default().fg(Color::White).bold(),
                ),
                None if value.is_empty() => (
                    placeholder.to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
                None => (value.to_string(), Style::default().fg(Color::Yellow)),
            };
            Line::from(vec![
                Span::styled(format!("{marker} {label:<18}"), label_style),
                Span::styled(shown, val_style),
            ])
        };
        let shimmer_preview = self.shimmer_spans("shimmer", Style::default().fg(Color::Gray));
        let lines = vec![
            Line::default(),
            row(
                0,
                "Working animation",
                self.spinner_def().name,
                vec![Span::styled(
                    self.working_anim(false),
                    Style::default().fg(self.spinner_color()).bold(),
                )],
            ),
            row(
                1,
                "Spinner color",
                color_by_name(&self.settings.spinner_color, "orange").0,
                vec![Span::styled(
                    "▆▆▆".to_string(),
                    Style::default().fg(self.spinner_color()),
                )],
            ),
            row(
                2,
                "Shimmer color",
                color_by_name(&self.settings.shimmer_color, "white").0,
                shimmer_preview.clone(),
            ),
            row(
                3,
                "Shimmer speed",
                self.shimmer_speed_name(),
                shimmer_preview,
            ),
            row(
                4,
                "Focus eye",
                self.eye_def().name,
                vec![Span::styled(
                    self.eye().to_string(),
                    Style::default().fg(Color::Cyan).bold(),
                )],
            ),
            row(
                5,
                "Sidebar frame",
                self.sidebar_frame(),
                vec![Span::styled(
                    match self.sidebar_frame() {
                        "surround" => "┌┐",
                        "rounded" => "╭╮",
                        _ => " │",
                    }
                    .to_string(),
                    Style::default().fg(self.sidebar_color()),
                )],
            ),
            row(
                6,
                "Sidebar weight",
                self.sidebar_weight(),
                vec![Span::styled(
                    match self.sidebar_weight() {
                        "thick" => "┃",
                        "double" => "║",
                        _ => "│",
                    }
                    .to_string(),
                    Style::default().fg(self.sidebar_color()),
                )],
            ),
            row(
                7,
                "Sidebar color",
                color_by_name(&self.settings.sidebar_color, "dark").0,
                vec![Span::styled(
                    "▆▆▆".to_string(),
                    Style::default().fg(self.sidebar_color()),
                )],
            ),
            row(
                8,
                "Home view",
                self.home_view(),
                vec![Span::styled(
                    match self.home_view() {
                        "list" => "≡",
                        "blocks" => "▥",
                        _ => "▦",
                    }
                    .to_string(),
                    Style::default().fg(Color::Indexed(179)).bold(),
                )],
            ),
            row(
                9,
                "Card size",
                CARD_SIZES[self.card_size_idx()].0,
                vec![Span::styled(
                    {
                        let (_, w, h) = CARD_SIZES[self.card_size_idx()];
                        format!("{w}×{h}")
                    },
                    Style::default().fg(Color::Indexed(246)),
                )],
            ),
            row(
                10,
                "Cards per row",
                self.card_columns_name(),
                vec![Span::styled(
                    match self.card_columns_name().parse::<usize>() {
                        Ok(n) => "▯".repeat(n),
                        Err(_) => "⟳".to_string(),
                    },
                    Style::default().fg(Color::Indexed(179)),
                )],
            ),
            row(
                11,
                "Separator color",
                color_by_name(&self.settings.home_sep_color, "dark").0,
                vec![Span::styled(
                    "─│─".to_string(),
                    Style::default().fg(self.home_sep_color()),
                )],
            ),
            row(
                12,
                "Card icon",
                CARD_ICON_SIZES[self.card_icon_idx()].0,
                vec![Span::styled(
                    "✳".to_string(),
                    Style::default().fg(Color::Indexed(209)).bold(),
                )],
            ),
            row(
                13,
                "Card outline",
                self.card_outline(),
                vec![Span::styled(
                    match self.card_outline() {
                        "double" => "▣",
                        "single" => "□",
                        _ => "·",
                    }
                    .to_string(),
                    Style::default().fg(Color::Indexed(179)),
                )],
            ),
            row(
                14,
                "Select color",
                color_by_name(&self.settings.select_color, "gold").0,
                vec![Span::styled(
                    "▆▆▆".to_string(),
                    Style::default().fg(self.select_color()),
                )],
            ),
            row(
                15,
                "Select weight",
                SELECT_WEIGHTS[self.select_weight_idx()].0,
                vec![Span::styled(
                    match SELECT_WEIGHTS[self.select_weight_idx()].0 {
                        "thin" => "─",
                        "normal" => "━",
                        "thick" => "▬",
                        _ => "█",
                    }
                    .to_string(),
                    Style::default().fg(self.select_color()),
                )],
            ),
            row(
                16,
                "Select style",
                self.select_style(),
                vec![Span::styled(
                    if self.select_style() == "glow" { "◜◝" } else { "┌┐" }.to_string(),
                    Style::default().fg(self.select_color()),
                )],
            ),
            row(
                17,
                "Card numeral",
                self.card_numeral_style(),
                vec![Span::styled(
                    match self.card_numeral_style() {
                        "arabic" => "1 2 3",
                        "zodiac" => "♈ ♉ ♊",
                        "zodiac-white" => "♈\u{FE0E} ♉\u{FE0E} ♊\u{FE0E}",
                        _ => "I II III",
                    }
                    .to_string(),
                    Style::default().fg(Color::Indexed(179)),
                )],
            ),
            row(
                18,
                "Claude style",
                self.claude_style(),
                vec![Span::styled(
                    if self.claude_style() == "soft" { "●" } else { "■" }.to_string(),
                    Style::default().fg(Color::Indexed(209)),
                )],
            ),
            row(
                19,
                "Finish sound",
                &self.finish_sound_name(),
                vec![Span::styled(
                    if self.settings.finish_sound_path().is_some() { "♪" } else { "✗" }.to_string(),
                    Style::default()
                        .fg(if self.settings.finish_sound_path().is_some() {
                            Color::Green
                        } else {
                            Color::DarkGray
                        })
                        .bold(),
                )],
            ),
            row(
                20,
                "Conn-error resume",
                if self.settings.connection_watch { "on" } else { "off" },
                vec![Span::styled(
                    if self.settings.connection_watch { "✓" } else { "✗" }.to_string(),
                    Style::default()
                        .fg(if self.settings.connection_watch {
                            Color::Green
                        } else {
                            Color::DarkGray
                        })
                        .bold(),
                )],
            ),
            row(
                21,
                "Cursor type",
                self.cursor_type(),
                vec![Span::styled(
                    match self.cursor_type() {
                        "block" => "█",
                        "underline" => "▁",
                        "bar" => "▎",
                        "orb" => "🔮",
                        "circle" => "○",
                        "aleph" => "א",
                        _ => "⟳", // follows the app in the pane
                    }
                    .to_string(),
                    Style::default().fg(Color::Cyan).bold(),
                )],
            ),
            row(
                22,
                "Cursor blink",
                self.cursor_blink(),
                vec![Span::styled(
                    match self.cursor_blink() {
                        "on" => "✓",
                        "off" => "✗",
                        _ => "⟳",
                    }
                    .to_string(),
                    Style::default().fg(Color::Cyan).bold(),
                )],
            ),
            row(
                23,
                "Cursor color",
                self.cursor_color_name(),
                vec![match self.cursor_rgb() {
                    Some((r, g, b)) => Span::styled(
                        "▆▆▆".to_string(),
                        Style::default().fg(Color::Rgb(r, g, b)),
                    ),
                    None => Span::styled(
                        "✗".to_string(),
                        Style::default().fg(Color::DarkGray).bold(),
                    ),
                }],
            ),
            row(
                24,
                "Bottom controls",
                if self.settings.hide_controls { "hidden" } else { "shown" },
                vec![Span::styled(
                    if self.settings.hide_controls { "✗" } else { "✓" }.to_string(),
                    Style::default()
                        .fg(if self.settings.hide_controls {
                            Color::DarkGray
                        } else {
                            Color::Green
                        })
                        .bold(),
                )],
            ),
            row(
                25,
                "Chat character",
                self.chat_face(),
                vec![match self.chat_face() {
                    "oracle" => Span::styled(
                        "(*)".to_string(),
                        Style::default().fg(Color::Indexed(135)).bold(),
                    ),
                    "hal" => Span::styled(
                        "◉".to_string(),
                        Style::default().fg(Color::Indexed(196)).bold(),
                    ),
                    _ => Span::styled(
                        "🧙".to_string(),
                        Style::default().fg(Color::Indexed(135)),
                    ),
                }],
            ),
            text_row(
                26,
                "Chat endpoint",
                &self.settings.chat_endpoint,
                "http://bigbox:8091",
            ),
            text_row(
                27,
                "Chat model",
                &self.settings.chat_model,
                "qwen3.6-35b-a3b",
            ),
            text_row(28, "Chat ssh", &self.settings.chat_ssh, "des@bigbox"),
            text_row(
                29,
                "Chat service",
                &self.settings.chat_service,
                "llama-server",
            ),
            Line::from(Span::styled(
                "  restart zodiac to apply chat connection changes",
                Style::default().fg(Color::DarkGray),
            )),
            Line::default(),
            Line::from(Span::styled(
                " ↑/↓ select · ←/→ change · Enter edits text · Esc close",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(lines), inner);
    }

    /// The home page: a spread of tarot cards, one per pane, selectable
    /// with the arrow keys. Card data comes from the server's T_STATE
    /// snapshot (refreshed ~1/s while the page is open).
    fn draw_home(&mut self, f: &mut Frame, area: Rect) {
        self.home_cards.clear();
        self.home_block_nav.clear();
        self.home_mascots.clear();
        if self.pair_open {
            return self.draw_pair_overlay(f, area);
        }
        let Some(state) = self.home_state.clone() else {
            let mid = Rect {
                x: area.x,
                y: area.y + area.height / 2,
                width: area.width,
                height: 1,
            };
            f.render_widget(
                Paragraph::new("☾ connecting…")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::DarkGray)),
                mid,
            );
            return;
        };
        let n = state.panes.len();
        if n == 0 {
            return;
        }
        self.home_sel = self.home_sel.min(n - 1);
        match self.home_view() {
            "list" => return self.draw_home_list(f, area, &state),
            "blocks" => return self.draw_home_blocks(f, area, &state),
            _ => {}
        }
        let (card_w, card_h) = self.card_dims();
        let mut cols = (((area.width.saturating_sub(2) + HOME_GAP_X) / (card_w + HOME_GAP_X))
            as usize)
            .clamp(1, n);
        if let Ok(cap) = self.card_columns_name().parse::<usize>() {
            cols = cols.min(cap);
        }
        self.home_cols = cols;
        let rows = n.div_ceil(cols);
        let vis_rows = (((area.height.saturating_sub(1) + HOME_GAP_Y)
            / (card_h + HOME_GAP_Y)) as usize)
            .max(1);
        let sel_row = self.home_sel / cols;
        let row_off = sel_row.saturating_sub(vis_rows - 1);
        let shown = rows.min(vis_rows) as u16;
        let grid_w = cols as u16 * card_w + (cols as u16 - 1) * HOME_GAP_X;
        let grid_h = shown * card_h + shown.saturating_sub(1) * HOME_GAP_Y;
        let x0 = area.x + area.width.saturating_sub(grid_w) / 2;
        let y0 = area.y + area.height.saturating_sub(grid_h) / 2;
        for (i, p) in state.panes.iter().enumerate() {
            let r = i / cols;
            if r < row_off || r >= row_off + vis_rows {
                continue;
            }
            let rect = Rect {
                x: x0 + (i % cols) as u16 * (card_w + HOME_GAP_X),
                y: y0 + (r - row_off) as u16 * (card_h + HOME_GAP_Y),
                width: card_w,
                height: card_h,
            };
            if rect.right() > area.right() || rect.bottom() > area.bottom() {
                continue;
            }
            self.draw_card(f, rect, i + 1, p, i == self.home_sel);
            self.home_cards.push((
                rect,
                p.id,
                card_status(p).3,
                p.agent.as_deref() == Some("claude"),
            ));
        }
    }

    /// The Alt+P overlay: one large QR encoding this machine's current
    /// astrolabe pairing URL (`http://<bridge>/?t=<token>&cid=<id>&name=<host>`
    /// — the same magic-link shape `web/src/auth.ts` already parses, plus
    /// two extra params the iOS scanner reads and the web app ignores), or
    /// an explanatory message in place of the code when there's nothing to
    /// show yet. `pair_rect` is left non-empty only when a QR was actually
    /// drawn — kitty_overlay() reads it to know whether/where to place the
    /// image.
    fn draw_pair_overlay(&mut self, f: &mut Frame, area: Rect) {
        self.pair_rect = Rect::default();
        let outer_w = area.width.min(area.height.saturating_mul(2)).min(70).max(30);
        let outer_h = area.height.min(30).max(14);
        let outer = Rect {
            x: area.x + area.width.saturating_sub(outer_w) / 2,
            y: area.y + area.height.saturating_sub(outer_h) / 2,
            width: outer_w,
            height: outer_h,
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(230, 190, 80)))
            .title(" pair a phone — astrolabe ")
            .title_alignment(Alignment::Center);
        f.render_widget(Clear, outer);
        let inner = block.inner(outer);
        f.render_widget(block, outer);
        let center_msg = |f: &mut Frame, msg: &str| {
            let mid = Rect {
                x: inner.x,
                y: inner.y + inner.height / 2,
                width: inner.width,
                height: (inner.height / 2).max(1),
            };
            f.render_widget(
                Paragraph::new(msg)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(Color::DarkGray)),
                mid,
            );
        };

        let Some(token) = self
            .home_state
            .as_ref()
            .map(|s| s.pairing_token.clone())
            .filter(|t| !t.is_empty())
        else {
            center_msg(f, "☾ waiting for session state…");
            return;
        };
        let Some((url, cid, name)) = self.pair_endpoint.clone() else {
            center_msg(
                f,
                "no astrolabe bridge detected on this machine\n\nis astrolabe.service running?",
            );
            return;
        };
        if self.pair_dead {
            center_msg(
                f,
                &format!(
                    "nothing is answering at {url}\n\nthe bridge it points to is gone — start it \
                     (astrolabe/install.sh, or `node bridge/main.ts`) and re-open this overlay",
                ),
            );
            return;
        }
        let pair_url = format!(
            "{url}/?t={}&cid={}&name={}",
            url_encode(&token),
            url_encode(&cid),
            url_encode(&name)
        );

        // Bottom row: hint text. Everything above it is reserved for the QR
        // (or, on a terminal without kitty graphics, the raw URL to copy).
        let hint_h = 2u16;
        let hint = Rect {
            x: inner.x,
            y: inner.bottom().saturating_sub(hint_h),
            width: inner.width,
            height: hint_h.min(inner.height),
        };
        f.render_widget(
            Paragraph::new("scan with Astrolabe on your phone · Esc to close")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            hint,
        );
        let qr_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: inner.height.saturating_sub(hint_h),
        };

        if !self.kitty_on {
            // No graphics protocol: fall back to the raw URL as text — not
            // scannable, but still pasteable into the app's manual-add form.
            f.render_widget(
                Paragraph::new(pair_url)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(Color::White)),
                qr_area,
            );
            return;
        }
        // Square-in-pixels box: pick a cell rect whose width/height convert
        // to (near) equal pixel dimensions, same reasoning card_dims() uses
        // elsewhere for non-square cells.
        let Some((cw, ch)) = crate::kitty::cell_size() else {
            center_msg(f, "terminal didn't report a cell pixel size");
            return;
        };
        let side_rows = qr_area.height.min(((qr_area.width as u32 * cw as u32) / ch as u32) as u16);
        let side_cols = ((side_rows as u32 * ch as u32) / cw as u32) as u16;
        let rect = Rect {
            x: qr_area.x + qr_area.width.saturating_sub(side_cols) / 2,
            y: qr_area.y + qr_area.height.saturating_sub(side_rows) / 2,
            width: side_cols,
            height: side_rows,
        };
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        if pair_url != self.pair_qr_src {
            let target_px = rect.height as u32 * ch as u32;
            self.pair_qr = crate::kitty::qr_rgba(&pair_url, target_px)
                .map(|(rgba, w, h)| (w, h, rgba, crate::kitty::qr_id(&pair_url)));
            self.pair_qr_src = pair_url;
        }
        if self.pair_qr.is_some() {
            self.pair_rect = rect;
        }
    }

    /// List view of the home page: one compact block per pane, stacked and
    /// centered, sharing the cards' selection/outline settings. Painted art
    /// stays out of this view — kitty_overlay skips it entirely.
    fn draw_home_list(&mut self, f: &mut Frame, area: Rect, state: &SessionState) {
        let n = state.panes.len();
        self.home_cols = 1;
        const ITEM_H: u16 = 4; // bordered block: 2 content lines
        const STRIDE: u16 = ITEM_H + 1;
        let w = area.width.saturating_sub(4).clamp(24, 76);
        let vis = (((area.height + STRIDE - ITEM_H) / STRIDE) as usize).max(1);
        let off = self.home_sel.saturating_sub(vis - 1);
        let shown = n.min(vis) as u16;
        let x0 = area.x + area.width.saturating_sub(w) / 2;
        let y0 = area.y
            + area
                .height
                .saturating_sub(shown * STRIDE - (STRIDE - ITEM_H))
                / 2;
        for (i, p) in state.panes.iter().enumerate() {
            if i < off || i >= off + vis {
                continue;
            }
            let rect = Rect {
                x: x0,
                y: y0 + (i - off) as u16 * STRIDE,
                width: w,
                height: ITEM_H,
            };
            if rect.bottom() > area.bottom() {
                continue;
            }
            self.draw_list_block(f, rect, i + 1, p, i == self.home_sel);
            self.home_cards.push((
                rect,
                p.id,
                card_status(p).3,
                p.agent.as_deref() == Some("claude"),
            ));
        }
    }

    fn draw_list_block(&self, f: &mut Frame, rect: Rect, num: usize, p: &PaneState, selected: bool) {
        let (label, glyph, scolor, _) = card_status(p);
        f.render_widget(Clear, rect);
        let bt = if selected {
            match SELECT_WEIGHTS[self.select_weight_idx()].0 {
                "thick" => BorderType::Thick,
                "heavy" => BorderType::Double,
                _ => BorderType::Rounded,
            }
        } else if self.card_outline() == "double" {
            BorderType::Double
        } else {
            BorderType::Rounded
        };
        let border_style = if selected {
            Style::default()
                .fg(self.select_color())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Indexed(101))
        };
        let title_style = if selected {
            Style::default()
                .fg(Color::Indexed(220))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Indexed(230))
                .add_modifier(Modifier::BOLD)
        };
        let name = truncate(&p.name, rect.width.saturating_sub(14) as usize);
        let block = Block::bordered()
            .border_type(bt)
            .border_style(border_style)
            .title_top(
                Line::from(Span::styled(
                    format!(" {} · {} ", self.card_numeral(num), name),
                    title_style,
                ))
                .left_aligned(),
            )
            .title_top(
                Line::from(Span::styled(
                    format!(" {glyph} {label} "),
                    Style::default().fg(scolor).add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
            );
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let gold = Style::default().fg(Color::Indexed(179));
        let dim = Style::default().fg(Color::Indexed(246));
        let faint = Style::default().fg(Color::Indexed(243));
        let agent_line = match (&p.agent, &p.version) {
            (Some(a), Some(v)) => format!("{a} {}", version_token(v)),
            (Some(a), None) => a.clone(),
            (None, _) => "shell".into(),
        };
        let dir = p
            .cwd
            .as_deref()
            .map(|d| short_dir(d, inner.width.saturating_sub(14) as usize))
            .unwrap_or_default();
        let left = vec![
            Line::from(Span::styled(format!(" {agent_line}"), gold)),
            Line::from(Span::styled(format!(" {dir}"), faint)),
        ];
        let right = vec![
            Line::from(Span::styled(format!("{} ", fmt_uptime(p.uptime_ms)), dim)),
            Line::default(),
        ];
        f.render_widget(Paragraph::new(left), inner);
        f.render_widget(Paragraph::new(right).alignment(Alignment::Right), inner);
    }

    /// Blocks view: two flat columns divided by rules — shells on the left,
    /// agents on the right — filling top-down as panes open. No borders,
    /// only line separators in the configured color; active claude blocks
    /// get the hopping mascot sprite (placed by kitty_overlay).
    fn draw_home_blocks(&mut self, f: &mut Frame, area: Rect, state: &SessionState) {
        self.home_cols = 2;
        let sep = Style::default().fg(self.home_sep_color());
        // 4 info rows, extra breathing room with bigger card sizes, then
        // the rule underneath.
        let stride = (5 + self.card_size_idx()) as u16;
        if area.width < 20 || area.height < stride + 2 {
            return;
        }
        let col_w = area.width.saturating_sub(5) / 2;
        let lx = area.x + 1;
        let sx = lx + col_w + 1;
        let rx = sx + 2;
        let shells: Vec<usize> = (0..state.panes.len())
            .filter(|&i| state.panes[i].agent.is_none())
            .collect();
        let agents: Vec<usize> = (0..state.panes.len())
            .filter(|&i| state.panes[i].agent.is_some())
            .collect();
        // Visual order interleaves the columns row by row. Once one column
        // runs longer than the other this is no longer a rectangular grid,
        // so home_block_nav (not ±cols index math) drives the arrow keys.
        let mut order: Vec<(usize, u16)> = Vec::new(); // (pane idx, row)
        for r in 0..shells.len().max(agents.len()) {
            if let Some(&i) = shells.get(r) {
                order.push((i, r as u16));
            }
            if let Some(&i) = agents.get(r) {
                order.push((i, r as u16));
            }
        }
        self.home_block_nav = order
            .iter()
            .map(|&(pi, r)| (state.panes[pi].id, r, state.panes[pi].agent.is_none()))
            .collect();
        // Column headers + the rule under them.
        let hdr = Style::default().fg(Color::Indexed(246)).bold();
        let buf = f.buffer_mut();
        buf.set_string(lx, area.y, "Shells", hdr);
        buf.set_string(rx, area.y, "Agents", hdr);
        let rule: String = "─".repeat(area.width.saturating_sub(2) as usize);
        buf.set_string(area.x + 1, area.y + 1, &rule, sep);
        // Vertical rule between the columns, full height.
        for y in area.y..area.bottom() {
            let g = if y == area.y + 1 { "┼" } else { "│" };
            buf.set_string(sx, y, g, sep);
        }
        let y0 = area.y + 2;
        let vis = ((area.height.saturating_sub(2)) / stride).max(1) as usize;
        let sel_row = order
            .get(self.home_sel)
            .map(|&(_, r)| r as usize)
            .unwrap_or(0);
        let off = sel_row.saturating_sub(vis - 1);
        let hsep: String = "─".repeat(col_w as usize);
        for (vi, &(pi, row)) in order.iter().enumerate() {
            let r = row as usize;
            if r < off || r >= off + vis {
                continue;
            }
            let p = &state.panes[pi];
            let left = p.agent.is_none();
            let rect = Rect {
                x: if left { lx } else { rx },
                y: y0 + ((r - off) as u16) * stride,
                width: col_w,
                height: stride - 1,
            };
            if rect.bottom() + 1 > area.bottom() {
                continue;
            }
            let selected = vi == self.home_sel;
            let (_, _, _, accent) = card_status(p);
            let claude = p.agent.as_deref() == Some("claude");
            let active = claude && (accent == 1 || accent == 2);
            let mascot_w: u16 = if active && self.kitty_on && col_w > 24 { 6 } else { 0 };
            self.draw_block(f, rect, pi + 1, p, selected, mascot_w);
            if mascot_w > 0 {
                self.home_mascots.push(Rect {
                    x: rect.x + rect.width - mascot_w,
                    y: rect.y + 1,
                    width: mascot_w,
                    height: 2.min(rect.height),
                });
            }
            // The rule under the block; the selected block's rule glows in
            // the select color so the highlight reads without any border.
            let st = if selected {
                Style::default().fg(self.select_color())
            } else {
                sep
            };
            f.buffer_mut().set_string(rect.x, rect.bottom(), &hsep, st);
            self.home_cards.push((rect, p.id, accent, claude));
        }
    }

    /// One pane block in the blocks view: title/status, agent · uptime,
    /// cwd, and the agent's latest transcript recap.
    fn draw_block(
        &self,
        f: &mut Frame,
        rect: Rect,
        num: usize,
        p: &PaneState,
        selected: bool,
        mascot_w: u16,
    ) {
        let (label, glyph, scolor, _) = card_status(p);
        f.render_widget(Clear, rect);
        let gold = Style::default().fg(Color::Indexed(179));
        let dim = Style::default().fg(Color::Indexed(246));
        let faint = Style::default().fg(Color::Indexed(243));
        let title_style = if selected {
            Style::default()
                .fg(self.select_color())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Indexed(230))
                .add_modifier(Modifier::BOLD)
        };
        let marker = if selected { "›" } else { " " };
        let tw = rect.width.saturating_sub(mascot_w) as usize;
        let ssh_suffix = p.ssh.as_deref().map(|h| format!(" — SSH: {h}"));
        let ssh_len = ssh_suffix.as_deref().map_or(0, |s| s.chars().count());
        let name = truncate(&p.name, tw.saturating_sub(12 + ssh_len));
        let agent_line = match (&p.agent, &p.version) {
            (Some(a), Some(v)) => format!("{a} {}", version_token(v)),
            (Some(a), None) => a.clone(),
            (None, _) => "shell".into(),
        };
        let recap_line = match &p.recap {
            Some(r) => Line::from(vec![
                Span::styled("  ⏺ ".to_string(), Style::default().fg(scolor)),
                Span::styled(
                    truncate(r, tw.saturating_sub(5)),
                    Style::default()
                        .fg(Color::Indexed(246))
                        .add_modifier(Modifier::ITALIC),
                ),
            ]),
            None => Line::default(),
        };
        let mut title_spans = vec![Span::styled(
            format!("{marker} {} · {}", self.card_numeral(num), name),
            title_style,
        )];
        if let Some(sfx) = &ssh_suffix {
            title_spans.push(Span::styled(sfx.clone(), Style::default().fg(Color::Indexed(108))));
        }
        let left = vec![
            Line::from(title_spans),
            Line::from(vec![
                Span::styled(format!("  {agent_line}"), gold),
                Span::styled(format!(" · {}", fmt_uptime(p.uptime_ms)), dim),
            ]),
            Line::from(Span::styled(
                format!(
                    "  {}",
                    p.cwd
                        .as_deref()
                        .map(|d| short_dir(d, tw.saturating_sub(3)))
                        .unwrap_or_default()
                ),
                faint,
            )),
            recap_line,
        ];
        f.render_widget(Paragraph::new(left), rect);
        // Status pinned to the block's top-right, clear of the mascot zone.
        let srect = Rect {
            x: rect.x,
            y: rect.y,
            width: tw as u16,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{glyph} {label}"),
                Style::default().fg(scolor).add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Right),
            srect,
        );
    }

    fn draw_card(&self, f: &mut Frame, rect: Rect, num: usize, p: &PaneState, selected: bool) {
        let (label, glyph, scolor, _) = card_status(p);
        f.render_widget(Clear, rect);
        // With painted art the card edge lives in the image (frame rings +
        // selection ring), so no text border — box-drawing lines run
        // through cell centers and would layer against the art's frame.
        let inner = if self.kitty_on || self.card_outline() == "none" {
            rect.inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 1,
            })
        } else {
            let bt = if selected {
                match SELECT_WEIGHTS[self.select_weight_idx()].0 {
                    "thick" => BorderType::Thick,
                    "heavy" => BorderType::Double,
                    _ => BorderType::Rounded,
                }
            } else if self.card_outline() == "double" {
                BorderType::Double
            } else {
                BorderType::Rounded
            };
            let border_style = if selected {
                Style::default()
                    .fg(self.select_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Indexed(101))
            };
            let block = Block::bordered().border_type(bt).border_style(border_style);
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            inner
        };

        let gold = Style::default().fg(Color::Indexed(179));
        let dim = Style::default().fg(Color::Indexed(246));
        let faint = Style::default().fg(Color::Indexed(243));
        let title_style = if selected {
            Style::default()
                .fg(Color::Indexed(220))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Indexed(230))
                .add_modifier(Modifier::BOLD)
        };
        let ssh_suffix = p.ssh.as_deref().map(|h| format!(" — SSH: {h}"));
        let ssh_len = ssh_suffix.as_deref().map_or(0, |s| s.chars().count());
        let name = truncate(&p.name, (inner.width as usize).saturating_sub(6 + ssh_len));
        let agent_line = match (&p.agent, &p.version) {
            (Some(a), Some(v)) => format!("{a} {}", version_token(v)),
            (Some(a), None) => a.clone(),
            (None, _) => "shell".into(),
        };
        let dir = p
            .cwd
            .as_deref()
            .map(|d| short_dir(d, inner.width.saturating_sub(2) as usize))
            .unwrap_or_default();
        // Layout adapts to the card-size setting: the header sits just
        // below the emblem zone (painted art anchors it near the top), a
        // breathing row appears when there's room, and the fallback's
        // ornament only renders when it fits.
        let ih = inner.height as usize;
        let mut lines: Vec<Line> = Vec::new();
        if self.kitty_on {
            // Keep clear of the painted emblem (top ~27% of the card).
            let pad = ((rect.height as f32 * 0.27) as usize)
                .saturating_sub(1)
                .clamp(1, ih.saturating_sub(6));
            lines.resize_with(pad, Line::default);
        } else if p.agent.as_deref() == Some("claude") {
            // Emblem: Claude's ✳ in coral for claude panes, a `>_` prompt
            // otherwise (mirrors the painted card art).
            lines.push(Line::from(vec![
                Span::styled("✦  ".to_string(), gold),
                Span::styled(
                    "✳".to_string(),
                    Style::default().fg(Color::Indexed(209)).bold(),
                ),
                Span::styled("  ✦".to_string(), gold),
            ]));
            lines.push(Line::default());
        } else {
            lines.push(Line::from(Span::styled(">_".to_string(), gold.bold())));
            lines.push(Line::default());
        }
        let mut title_spans = vec![Span::styled(
            format!("{} · {}", self.card_numeral(num), name),
            title_style,
        )];
        if let Some(sfx) = &ssh_suffix {
            title_spans.push(Span::styled(sfx.clone(), Style::default().fg(Color::Indexed(108))));
        }
        lines.push(Line::from(title_spans));
        lines.push(Line::from(Span::styled("──────────", faint)));
        if ih >= lines.len() + 6 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(agent_line, gold)));
        lines.push(Line::from(Span::styled(fmt_uptime(p.uptime_ms), dim)));
        lines.push(Line::from(Span::styled(
            format!("{glyph} {label}"),
            Style::default().fg(scolor).add_modifier(Modifier::BOLD),
        )));
        if let Some(sub) = &p.subtitle {
            if ih >= lines.len() + 3 {
                lines.push(Line::from(Span::styled(
                    truncate(sub, inner.width.saturating_sub(2) as usize),
                    Style::default().fg(Color::Indexed(246)).add_modifier(Modifier::ITALIC),
                )));
            }
        }
        lines.push(Line::from(Span::styled(dir, faint)));
        if !self.kitty_on && ih >= lines.len() + 2 {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("✦  ·  ✦".to_string(), gold)));
        }
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
    }

    /// The Wizard's chat panel on the right edge of the home page:
    /// portrait (kitty image), status line, transcript, input box.
    fn draw_chat(&mut self, f: &mut Frame, area: Rect) {
        use crate::chat::ChatStatus as S;
        self.chat_rect = area;
        let violet = Color::Indexed(135);
        let gold = Color::Indexed(179);
        let border_style = if self.chat_focus {
            Style::default().fg(violet)
        } else {
            Style::default().fg(Color::Indexed(60))
        };
        let title_style = if self.chat_status == Some(S::Awake) {
            Style::default().fg(Color::Indexed(220)).bold()
        } else {
            Style::default().fg(Color::Indexed(246)).bold()
        };
        f.render_widget(Clear, area);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(match self.chat_face() {
                "oracle" => " ✦ The Oracle ✦ ",
                "hal" => " ◉ HAL 9000 ◉ ",
                _ => " ✦ assistant ✦ ",
            })
            .title_alignment(Alignment::Center)
            .title_style(title_style);
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.width < 10 || inner.height < 6 {
            self.chat_art_rect = Rect::default();
            return;
        }
        let face = self.chat_face();
        if face == "oracle" {
            // The orb itself paints its own cells black; without this, the
            // rest of the panel around and behind it — margins, transcript,
            // input line — falls back to the terminal's own default
            // background, which reads as grey next to true black. Rgb(0,0,0)
            // rather than the named Black: that's ANSI color 0, which most
            // terminal themes tint dark grey rather than true black.
            f.render_widget(Block::default().style(Style::default().bg(OLED_BLACK)), inner);
        }
        let mut y = inner.y;
        // Portrait zone — HAL's image is painted by chat_overlay; the
        // oracle's orb is pure text and needs no kitty; the plain assistant
        // gets the one-line status glyph below instead.
        let art_h: u16 = if inner.height >= 20
            && ((self.kitty_on && face == "hal") || face == "oracle")
        {
            9
        } else {
            0
        };
        if art_h > 0 {
            self.chat_art_rect = Rect {
                x: inner.x + 1,
                y,
                width: inner.width.saturating_sub(2),
                height: art_h,
            };
            if face == "oracle" {
                self.draw_oracle(f, self.chat_art_rect);
            }
            y += art_h;
        } else {
            self.chat_art_rect = Rect::default();
            let glyph = match self.chat_status {
                Some(S::Awake) => "✦",
                Some(S::Waking) => "☀",
                Some(S::Sleeping) => "☾",
                Some(S::Away) => "✧",
                None => "·",
            };
            f.render_widget(
                Paragraph::new(glyph)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(violet)),
                Rect { x: inner.x, y, width: inner.width, height: 1 },
            );
            y += 1;
        }
        let who = match face {
            "oracle" => "the oracle",
            "hal" => "HAL",
            _ => "the assistant",
        };
        let (stxt, scol) = match self.chat_status {
            Some(S::Awake) => (format!("{who} is awake"), violet),
            Some(S::Waking) => (format!("{who} is waking…"), Color::Indexed(220)),
            Some(S::Sleeping) => (format!("{who} is sleeping"), Color::Indexed(68)),
            Some(S::Away) => (format!("{who} is away"), Color::Indexed(203)),
            None => (format!("seeking {who}…"), Color::DarkGray),
        };
        f.render_widget(
            Paragraph::new(stxt)
                .alignment(Alignment::Center)
                .style(Style::default().fg(scol).italic()),
            Rect { x: inner.x, y, width: inner.width, height: 1 },
        );
        y += 1;
        f.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(Color::Indexed(238))),
            Rect { x: inner.x, y, width: inner.width, height: 1 },
        );
        y += 1;

        // Input pinned to the bottom; the transcript fills the middle.
        let input_y = inner.y + inner.height - 1;
        let rows = input_y.saturating_sub(y) as usize;
        let wrapw = inner.width.saturating_sub(1).max(8) as usize;
        let mut lines: Vec<Line> = Vec::new();
        let msg_styles = |role: u8| -> (&'static str, Style, Style) {
            if role == CHAT_USER {
                (
                    "❯ ",
                    Style::default().fg(gold).bold(),
                    Style::default().fg(Color::Indexed(252)),
                )
            } else {
                (
                    "✦ ",
                    Style::default().fg(violet).bold(),
                    Style::default().fg(Color::Indexed(189)),
                )
            }
        };
        for (role, text) in &self.chat_log {
            match *role {
                CHAT_NOTE | CHAT_ERROR => {
                    let st = if *role == CHAT_ERROR {
                        Style::default().fg(Color::Indexed(203)).italic()
                    } else {
                        Style::default().fg(Color::Indexed(245)).italic()
                    };
                    for l in wrap_text(text, wrapw) {
                        lines.push(
                            Line::from(Span::styled(l, st)).alignment(Alignment::Center),
                        );
                    }
                }
                CHAT_ACTION => {
                    // Left-aligned and bold, unlike the centered italic
                    // notes — this is the one line in the transcript that
                    // wants a keypress, not just a read.
                    let st = Style::default().fg(Color::Indexed(220)).bold();
                    for l in wrap_text(text, wrapw) {
                        lines.push(Line::from(Span::styled(l, st)));
                    }
                }
                r => {
                    let (pfx, pst, st) = msg_styles(r);
                    let runs = parse_md(text);
                    for (i, line) in
                        wrap_md_runs(&runs, wrapw.saturating_sub(2)).into_iter().enumerate()
                    {
                        let mut spans = if i == 0 {
                            vec![Span::styled(pfx, pst)]
                        } else {
                            vec![Span::raw("  ")]
                        };
                        spans.extend(
                            line.into_iter().map(|(t, s)| Span::styled(t, apply_md(st, s))),
                        );
                        lines.push(Line::from(spans));
                    }
                    lines.push(Line::default());
                }
            }
        }
        if self.chat_streaming {
            if self.chat_stream.is_empty() {
                let pondering = match face {
                    "oracle" => "✦ the oracle is divining…",
                    "hal" => "◉ HAL computes…",
                    _ => "✦ the wizard ponders…",
                };
                lines.push(Line::from(Span::styled(
                    pondering,
                    Style::default().fg(violet).italic(),
                )));
            } else {
                let (pfx, pst, st) = msg_styles(CHAT_REPLY);
                let mut runs = parse_md(&self.chat_stream);
                // The cursor block always renders plain — an unterminated
                // bold/code marker still streaming in shouldn't catch it.
                runs.push(("▌".to_string(), MdStyle::default()));
                for (i, line) in
                    wrap_md_runs(&runs, wrapw.saturating_sub(2)).into_iter().enumerate()
                {
                    let head = if i == 0 { Span::styled(pfx, pst) } else { Span::raw("  ") };
                    let mut spans = vec![head];
                    spans.extend(
                        line.into_iter().map(|(t, s)| Span::styled(t, apply_md(st, s))),
                    );
                    lines.push(Line::from(spans));
                }
            }
        } else if lines.last().is_some_and(|l| l.spans.is_empty()) {
            lines.pop(); // no trailing blank against the input line
        }
        let max = lines.len().saturating_sub(rows);
        self.chat_scroll = self.chat_scroll.min(max);
        let end = lines.len() - self.chat_scroll;
        let start = end.saturating_sub(rows);
        let shown = (end - start) as u16;
        if shown > 0 {
            f.render_widget(
                Paragraph::new(lines[start..end].to_vec()),
                Rect {
                    x: inner.x,
                    y: y + rows as u16 - shown, // bottom-anchored
                    width: inner.width,
                    height: shown,
                },
            );
        }

        let input_line = if self.chat_focus {
            let budget = inner.width.saturating_sub(4) as usize;
            let cs: Vec<char> = self.chat_input.chars().collect();
            let tail: String = cs[cs.len().saturating_sub(budget)..].iter().collect();
            Line::from(vec![
                Span::styled("❯ ", Style::default().fg(gold).bold()),
                Span::styled(tail, Style::default().fg(Color::Indexed(252))),
                Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
            ])
        } else {
            Line::from(Span::styled(
                "❯ Alt+G · /wake · /sleep",
                Style::default().fg(Color::Indexed(240)),
            ))
        };
        f.render_widget(
            Paragraph::new(input_line),
            Rect { x: inner.x, y: input_y, width: inner.width, height: 1 },
        );
    }

    /// Paint/refresh the kitty-graphics card art under the text layer.
    /// Called after every ratatui draw; a no-op unless the card layout or
    /// accents changed. Off the home page it cleans up any placements.
    fn kitty_overlay(&mut self) {
        if !self.kitty_on {
            return;
        }
        use std::io::Write as _;
        let mut out = std::io::stdout();
        // Card art hides while the settings popup covers the page (z=-1
        // images would bleed through its background), in list view, and
        // off home entirely. The pairing overlay overrides all of that —
        // it takes over the page regardless of the home view mode.
        if !self.pair_open
            && (!self.home
                || matches!(self.mode, Mode::Settings | Mode::SettingsEdit { .. })
                || self.home_view() == "list")
        {
            if !self.kitty_placed.is_empty() {
                let _ = crate::kitty::delete_placements(&mut out);
                let _ = out.flush();
                self.kitty_placed.clear();
                // d=a wiped every visible placement, pane images included —
                // drop the compositor's tracking so it re-places them.
                self.placed_gfx.clear();
                self.orb_placed = None;
                self.chat_placed = None;
            }
            return;
        }
        let Some((cw, ch)) = crate::kitty::cell_size() else {
            return;
        };
        // Setting changes repaint image data in place; selection changes
        // are handled by the per-placement diff below.
        let style_key = format!(
            "{}|{}|{}|{}|{}|{}",
            self.settings.card_icon,
            self.card_outline(),
            color_by_name(&self.settings.select_color, "gold").0,
            SELECT_WEIGHTS[self.select_weight_idx()].0,
            self.select_style(),
            self.claude_style(),
        );
        if self.kitty_last_icon != style_key {
            self.kitty_sent.clear();
            self.kitty_last_icon = style_key;
        }

        // Desired placements: the pairing QR alone while that overlay is
        // open, mascot sprites beside active claude blocks in the blocks
        // view, otherwise one painted card per pane.
        let desired: Vec<(Rect, u32)> = if self.pair_open {
            match &self.pair_qr {
                Some((pw, ph, rgba, id)) if self.pair_rect.width > 0 && self.pair_rect.height > 0 => {
                    if !self.kitty_sent.contains(&(*pw, *ph, *id)) {
                        self.kitty_sent.retain(|&(_, _, i2)| i2 != *id);
                        let _ = crate::kitty::transmit(&mut out, *id, *pw, *ph, rgba);
                        self.kitty_sent.insert((*pw, *ph, *id));
                    }
                    vec![(self.pair_rect, *id)]
                }
                _ => Vec::new(),
            }
        } else if self.home_view() == "blocks" {
            let frame = ((self.anim_start.elapsed().as_millis() / 250) % 4) as u8;
            let soft = self.claude_style() == "soft";
            let mut v = Vec::with_capacity(self.home_mascots.len());
            for rect in self.home_mascots.clone() {
                let id = crate::kitty::clawd_id(frame, soft);
                let (pw, ph) = (rect.width as u32 * cw as u32, rect.height as u32 * ch as u32);
                if !self.kitty_sent.contains(&(pw, ph, id)) {
                    self.kitty_sent.retain(|&(_, _, i2)| i2 != id);
                    let rgba = crate::kitty::clawd_rgba(pw, ph, frame, soft);
                    let _ = crate::kitty::transmit(&mut out, id, pw, ph, &rgba);
                    self.kitty_sent.insert((pw, ph, id));
                }
                v.push((rect, id));
            }
            v
        } else {
            let (size_idx, (_, scale)) = (self.card_icon_idx(), CARD_ICON_SIZES[self.card_icon_idx()]);
            let cards = self.home_cards.clone();
            let mut desired: Vec<(Rect, u32)> = Vec::with_capacity(cards.len());
            for (i, &(rect, _, accent, claude)) in cards.iter().enumerate() {
                let selected = i == self.home_sel;
                // Working/thinking claude gets the bouncing mascot (frame from
                // the shared animation clock — placement swaps animate it);
                // idle claude keeps the ✳ star.
                let mark = if claude {
                    if accent == 1 || accent == 2 {
                        crate::kitty::CardMark::ClaudeRun(
                            ((self.anim_start.elapsed().as_millis() / 250) % 4) as u8,
                        )
                    } else {
                        crate::kitty::CardMark::Claude
                    }
                } else {
                    crate::kitty::CardMark::Terminal
                };
                let id = crate::kitty::image_id(accent, mark, size_idx, selected);
                let (pw, ph) = (rect.width as u32 * cw as u32, rect.height as u32 * ch as u32);
                // Transmit up front so placement swaps below are instant.
                if !self.kitty_sent.contains(&(pw, ph, id)) {
                    self.kitty_sent.retain(|&(_, _, i2)| i2 != id);
                    let style = crate::kitty::CardStyle {
                        accent: ACCENT_RGB[accent],
                        mark,
                        icon_scale: scale,
                        rings: match self.card_outline() {
                            "single" => 1,
                            "none" => 0,
                            _ => 2,
                        },
                        sel: selected.then(|| {
                            (
                                self.select_rgb(),
                                (ph as f32 * SELECT_WEIGHTS[self.select_weight_idx()].1).max(1.5),
                            )
                        }),
                        sel_glow: self.select_style() == "glow",
                        mascot_soft: self.claude_style() == "soft",
                    };
                    let rgba = crate::kitty::card_rgba(pw, ph, &style);
                    let _ = crate::kitty::transmit(&mut out, id, pw, ph, &rgba);
                    self.kitty_sent.insert((pw, ph, id));
                }
                desired.push((rect, id));
            }
            desired
        };

        if desired == self.kitty_placed {
            let _ = out.flush();
            return;
        }
        // Card count changed: start over (everything moves anyway).
        if desired.len() != self.kitty_placed.len() {
            let _ = crate::kitty::delete_placements(&mut out);
            self.kitty_placed.clear();
            self.placed_gfx.clear();
            self.orb_placed = None;
            self.chat_placed = None;
        }
        // Per-placement diff: place the new image first, then drop the old
        // one, so the card never shows bare background (no flicker).
        for (i, &(rect, id)) in desired.iter().enumerate() {
            let pid = i as u32 + 1;
            let old = self.kitty_placed.get(i).copied();
            if old == Some((rect, id)) {
                continue;
            }
            let _ = write!(out, "\x1b[{};{}H", rect.y + 1, rect.x + 1);
            let _ = crate::kitty::place(&mut out, id, pid, rect.width, rect.height);
            if let Some((_, old_id)) = old {
                if old_id != id {
                    let _ = crate::kitty::delete_placement(&mut out, old_id, pid);
                }
            }
        }
        let _ = out.flush();
        self.kitty_placed = desired;
    }

    /// Composite the focused pane's kitty-graphics placements onto the
    /// outer terminal. Runs after every ratatui draw: pixels are relayed
    /// once per image, then only cheap re-place/crop/delete commands flow.
    /// All writes are wrapped in cursor save/restore so the pane's text
    /// cursor stays where ratatui put it.
    fn pane_overlay(&mut self) {
        if !self.kitty_on {
            return;
        }
        use std::io::Write as _;
        let target: Option<u64> = if self.home
            || matches!(self.mode, Mode::Settings | Mode::SettingsEdit { .. })
        {
            None
        } else {
            self.active_id()
        };
        let view = self.main_rect;
        let mut buf: Vec<u8> = Vec::new();
        let mut desired: Vec<(u64, OuterGeom)> = Vec::new();
        let mut next_outer = self.next_outer;

        if let Some(pane_id) = target {
            if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane_id) {
                let scroll = p.scroll as i32;
                let places = p.gfx.placements.clone();
                for vp in places {
                    let (vw, vh) = (view.width as i32, view.height as i32);
                    if vw == 0 || vh == 0 || (vp.col as i32) >= vw {
                        continue;
                    }
                    let vr = vp.row + scroll;
                    let start = vr.max(0);
                    let end = (vr + vp.rows as i32).min(vh);
                    let vis_rows = end - start;
                    if vis_rows <= 0 {
                        continue;
                    }
                    let top_clip = (-vr).max(0);
                    let vis_cols = (vp.cols as i32).min(vw - vp.col as i32);
                    let Some(img) = p.images.get_mut(&vp.img) else {
                        continue; // pixel data still in flight
                    };
                    if img.ver != vp.img_ver {
                        continue;
                    }
                    if img.outer.is_none() {
                        next_outer += 1;
                        let _ = crate::kitty::transmit_data(
                            &mut buf, next_outer, img.format, img.zlib, img.w, img.h,
                            &img.data,
                        );
                        img.outer = Some(next_outer);
                    }
                    let outer = img.outer.unwrap();
                    // proportional source-rect crop for partial visibility
                    let (sx, sy, sw0, sh0) = vp.src;
                    let sw = if sw0 > 0 { sw0 } else { img.w.saturating_sub(sx) };
                    let sh = if sh0 > 0 { sh0 } else { img.h.saturating_sub(sy) };
                    let full = top_clip == 0
                        && vis_rows == vp.rows as i32
                        && vis_cols == vp.cols as i32;
                    let src = if full {
                        vp.src
                    } else {
                        (
                            sx,
                            sy + (sh as i64 * top_clip as i64 / vp.rows as i64) as u32,
                            (sw as i64 * vis_cols as i64 / vp.cols as i64).max(1) as u32,
                            (sh as i64 * vis_rows as i64 / vp.rows as i64).max(1) as u32,
                        )
                    };
                    desired.push((
                        vp.key,
                        OuterGeom {
                            img: outer,
                            x: view.x + vp.col + 1,
                            y: (view.y as i32 + start + 1) as u16,
                            src,
                            c: vis_cols as u16,
                            r: vis_rows as u16,
                            z: vp.z,
                            offx: vp.offx,
                            offy: if top_clip > 0 { 0 } else { vp.offy },
                        },
                    ));
                }
            }
        }
        self.next_outer = next_outer;

        // Diff against what's on the terminal: same key + same geometry is
        // free; same outer image re-places atomically (no flicker); an
        // image swap deletes the stale placement in the same write.
        let mut existing: std::collections::HashMap<u64, OuterPlaced> =
            std::collections::HashMap::new();
        for pl in self.placed_gfx.drain(..) {
            if Some(pl.pane) == target {
                existing.insert(pl.key, pl);
            } else {
                let _ = crate::kitty::delete_placement(&mut buf, pl.geom.img, pl.pid);
            }
        }
        let mut new_placed: Vec<OuterPlaced> = Vec::new();
        let pane = target.unwrap_or(0);
        for (key, geom) in desired {
            let pid = *self.pid_map.entry((pane, key)).or_insert_with(|| {
                self.next_pid += 1;
                self.next_pid
            });
            match existing.remove(&key) {
                Some(old) if old.geom == geom => new_placed.push(old),
                old => {
                    if let Some(old) = old {
                        if old.geom.img != geom.img {
                            let _ = crate::kitty::delete_placement(
                                &mut buf,
                                old.geom.img,
                                old.pid,
                            );
                        }
                    }
                    let _ = crate::kitty::place_at(
                        &mut buf, geom.y, geom.x, geom.img, pid, geom.src, geom.c,
                        geom.r, geom.z, geom.offx, geom.offy,
                    );
                    new_placed.push(OuterPlaced {
                        pane,
                        key,
                        pid,
                        geom,
                    });
                }
            }
        }
        for (_, old) in existing {
            let _ = crate::kitty::delete_placement(&mut buf, old.geom.img, old.pid);
        }
        self.placed_gfx = new_placed;
        for outer in self.outer_dead.drain(..) {
            let _ = crate::kitty::delete_image(&mut buf, outer);
        }

        if !buf.is_empty() {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b7");
            let _ = out.write_all(&buf);
            let _ = out.write_all(b"\x1b8");
            let _ = out.flush();
        }
    }

    /// Free everything this client pushed to the outer terminal — image
    /// data outlives the alternate screen, so exiting without this would
    /// leak pixels into the terminal's store.
    fn gfx_cleanup(&mut self) {
        if !self.kitty_on {
            return;
        }
        use std::io::Write as _;
        let mut buf: Vec<u8> = Vec::new();
        for pl in &self.placed_gfx {
            let _ = crate::kitty::delete_placement(&mut buf, pl.geom.img, pl.pid);
        }
        for p in &self.panes {
            for img in p.images.values() {
                if let Some(outer) = img.outer {
                    let _ = crate::kitty::delete_image(&mut buf, outer);
                }
            }
        }
        for outer in self.outer_dead.drain(..) {
            let _ = crate::kitty::delete_image(&mut buf, outer);
        }
        if self.orb_cfg.take().is_some() {
            for i in 0..crate::kitty::ORB_FRAMES {
                let _ = crate::kitty::delete_image(&mut buf, crate::kitty::ORB_BASE + i);
            }
        }
        let chat_ids: std::collections::HashSet<u32> =
            self.chat_sent.drain().map(|(_, _, i)| i).collect();
        for id in chat_ids {
            let _ = crate::kitty::delete_image(&mut buf, id);
        }
        if !buf.is_empty() {
            let mut out = std::io::stdout();
            let _ = out.write_all(&buf);
            let _ = out.flush();
        }
    }

    /// Forward the focused pane's DECSCUSR cursor style to the outer
    /// terminal — inner shells and TUIs set block/underline/bar cursors and
    /// the emulator used to swallow them, leaving whatever shape the outer
    /// terminal last had (ghostty's shell integration leaves a thin bar).
    /// While a pane is focused the cursor is also tinted zodiac's orange,
    /// so it's obvious the pane — not the outer shell — owns it.
    /// The cursor is drawn through the graphics pipeline instead of a
    /// hardware cursor shape. "bar" included: the hardware bar's thickness
    /// is the terminal's choice, so a properly thick one is painted. For
    /// "aleph" the graphics part is only the aura behind the glyph.
    fn orb_active(&self) -> bool {
        self.kitty_on && matches!(self.cursor_type(), "orb" | "circle" | "bar" | "aleph")
    }

    /// The aleph cursor: א drawn as a text glyph at the cursor cell — the
    /// silent letter, the breath before speech. Works with or without
    /// outer-terminal graphics (the aura is graphics-only).
    fn aleph_active(&self) -> bool {
        self.cursor_type() == "aleph"
    }

    /// Whether the orb should pulse (drives the fast animation tick). For
    /// the aleph, "auto" means breathe — an aura that doesn't breathe is
    /// just a stain; shells never request a blinking cursor at a prompt,
    /// so following the pane would freeze it.
    fn orb_blinking(&self) -> bool {
        match self.cursor_blink() {
            "on" => true,
            "off" => false,
            _ if self.aleph_active() => true,
            _ => self
                .panes
                .get(self.active)
                .map(|p| {
                    let s = p.parser.screen().cursor_style();
                    s != 0 && s % 2 == 1
                })
                .unwrap_or(false),
        }
    }

    fn orb_color(&self) -> (u8, u8, u8) {
        self.cursor_rgb().unwrap_or(ORB_DEFAULT_RGB)
    }

    /// Place (and animate) the orb cursor over the focused pane's cursor
    /// cell. Frames are pre-transmitted once per (style, color, cell size);
    /// per tick only a cheap re-place runs — or nothing, when steady and
    /// the cursor hasn't moved.
    /// Paint/refresh the Wizard's portrait inside the chat panel. Modeled
    /// on the card overlay: transmit each (size, state, frame) image once,
    /// then cheap place/delete swaps.
    /// The Oracle: an animated ascii crystal orb on a black field. Pure
    /// text, so it works even when the outer terminal has no kitty
    /// graphics. Bands of light drift through the glass; the palette
    /// follows the model's status.
    fn draw_oracle(&self, f: &mut Frame, rect: Rect) {
        use crate::chat::ChatStatus as S;
        let t = self.anim_start.elapsed().as_millis() as f32 / 1000.0;
        // Hue arc + saturation the glass drifts across, per status — a
        // range rather than two fixed points, so the fusion never settles
        // on one color. Hues in turns (0.0-1.0): ~0.75 violet, ~0.52 cyan,
        // ~0.08 amber, ~0.65 indigo.
        let (hue_lo, hue_hi, sat_max): (f32, f32, f32) = match self.chat_status {
            Some(S::Awake) => (0.52, 0.80, 0.75),
            Some(S::Waking) => (0.05, 0.14, 0.70),
            Some(S::Sleeping) => (0.60, 0.70, 0.45),
            _ => (0.60, 0.65, 0.12), // away/unknown: nearly monochrome
        };
        let (cx, cy) = (rect.width as f32 / 2.0, rect.height as f32 / 2.0 - 0.1);
        let ry = rect.height as f32 * 0.48;
        let rx = ry * 2.05; // terminal cells are ~2:1
        let ramp: &[char] = &[' ', '·', ':', '≈', '∗', 'o', 'O', '@'];
        let mut lines = Vec::with_capacity(rect.height as usize);
        for yy in 0..rect.height {
            let mut spans = Vec::with_capacity(rect.width as usize);
            for xx in 0..rect.width {
                let dx = (xx as f32 - cx) / rx;
                let dy = (yy as f32 - cy) / ry;
                let d = (dx * dx + dy * dy).sqrt();
                if d > 1.0 {
                    spans.push(Span::styled(" ", Style::default().bg(OLED_BLACK)));
                    continue;
                }
                let ang = dy.atan2(dx);
                // Drifting interference bands make the glass swirl.
                let v = ((d * 6.0 - t * 1.6).sin()
                    + (ang * 3.0 + t * 0.8).sin()
                    + (dx * 5.0 + dy * 2.0 + t * 1.1).cos())
                    / 3.0;
                let v = ((v * 0.5 + 0.5) * (1.0 - d * 0.45)).clamp(0.0, 1.0);
                let (ch, boost) = if d > 0.92 {
                    ('∘', 0.35) // glass rim
                } else if v > 0.96 {
                    ('✦', 1.0) // a glint in the depths
                } else {
                    (ramp[((v * ramp.len() as f32) as usize).min(ramp.len() - 1)], v)
                };
                // Hue rides the same brightness swirl but on its own drift,
                // so color and light shift independently — a fusion of
                // hues across the glass rather than one gradient.
                let hue = hue_lo
                    + (hue_hi - hue_lo) * boost
                    + 0.05 * (ang * 1.7 - t * 0.7).sin()
                    + 0.03 * (d * 5.0 + t * 1.3).cos();
                let sat = sat_max * (0.3 + 0.7 * boost);
                let val = (0.28 + 0.72 * boost).clamp(0.0, 1.0);
                let (r, g, b) = hsv_to_rgb(hue.rem_euclid(1.0), sat.clamp(0.0, 1.0), val);
                spans.push(Span::styled(
                    ch.to_string(),
                    Style::default().fg(Color::Rgb(r, g, b)).bg(OLED_BLACK),
                ));
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(OLED_BLACK)),
            rect,
        );
    }

    fn chat_overlay(&mut self) {
        if !self.kitty_on {
            return;
        }
        use crate::chat::ChatStatus as S;
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let rect = self.chat_art_rect;
        let want = self.home
            && !matches!(self.mode, Mode::Settings | Mode::SettingsEdit { .. })
            && self.chat_rect.width > 0
            && rect.width >= 8
            && rect.height >= 4
            // Only HAL has a painted portrait; the oracle's orb is drawn as
            // text by draw_chat, and the plain assistant has none at all.
            && self.chat_face() == "hal";
        if !want {
            if let Some((_, id)) = self.chat_placed.take() {
                let _ = crate::kitty::delete_placement(&mut out, id, 1);
                let _ = out.flush();
            }
            return;
        }
        let Some((cw, ch)) = crate::kitty::cell_size() else {
            return;
        };
        let sidx = match self.chat_status {
            Some(S::Awake) => 0,
            Some(S::Waking) => 1,
            Some(S::Sleeping) => 2,
            Some(S::Away) | None => 3,
        };
        let (pw, ph) = (rect.width as u32 * cw as u32, rect.height as u32 * ch as u32);
        let mut buf: Vec<u8> = Vec::new();
        // HAL idles with a slow blink: ~3.2s of steady red, then a 400ms
        // shutter sweep (close, shut, open).
        let t = self.anim_start.elapsed().as_millis() as u64 % 3600;
        let frame = if t < 3200 { 0 } else { (((t - 3200) / 100) % 4).min(3) as u32 };
        let open = [1.0f32, 0.45, 0.05, 0.45][frame as usize];
        let id = crate::kitty::hal_id(sidx, frame);
        if !self.chat_sent.contains(&(pw, ph, id)) {
            self.chat_sent.retain(|&(_, _, i)| i != id);
            let rgba = crate::kitty::hal_rgba(pw, ph, sidx, open);
            let _ = crate::kitty::transmit(&mut buf, id, pw, ph, &rgba);
            self.chat_sent.insert((pw, ph, id));
        }
        if self.chat_placed != Some((rect, id)) {
            let _ = crate::kitty::place_at(
                &mut buf,
                rect.y + 1,
                rect.x + 1,
                id,
                1,
                (0, 0, 0, 0),
                rect.width,
                rect.height,
                -1,
                0,
                0,
            );
            if let Some((_, old)) = self.chat_placed {
                if old != id {
                    let _ = crate::kitty::delete_placement(&mut buf, old, 1);
                }
            }
            self.chat_placed = Some((rect, id));
        }
        if !buf.is_empty() {
            let _ = out.write_all(b"\x1b7");
            let _ = out.write_all(&buf);
            let _ = out.write_all(b"\x1b8");
            let _ = out.flush();
        }
    }

    fn orb_overlay(&mut self) {
        if !self.kitty_on {
            return;
        }
        use std::io::Write as _;
        let mut buf: Vec<u8> = Vec::new();

        // Where the orb should be right now, if anywhere.
        let want: Option<(u16, u16)> = if self.orb_active()
            && !self.home
            && matches!(self.mode, Mode::Normal)
        {
            self.panes.get(self.active).and_then(|p| {
                let screen = p.parser.screen();
                let (r, c) = screen.cursor_position();
                (p.scroll == 0
                    && !screen.hide_cursor()
                    && r < self.main_rect.height
                    && c < self.main_rect.width)
                    .then(|| (self.main_rect.x + c + 1, self.main_rect.y + r + 1))
            })
        } else {
            None
        };

        match want {
            None => {
                if let Some((_, _, old, _, _)) = self.orb_placed.take() {
                    let _ = crate::kitty::delete_placement(&mut buf, old, 1);
                }
            }
            Some((x, y)) => {
                let shape = match self.cursor_type() {
                    "orb" => crate::kitty::OrbShape::Orb,
                    "circle" => crate::kitty::OrbShape::Circle,
                    "aleph" => crate::kitty::OrbShape::Halo,
                    _ => crate::kitty::OrbShape::Bar,
                };
                // The aleph's aura sits under the text so the glyph stays
                // crisp; the other shapes float translucently above it.
                let halo = shape == crate::kitty::OrbShape::Halo;
                let z = if halo { -1 } else { 100 };
                let col = self.orb_color();
                let cell = crate::kitty::cell_size().unwrap_or((10, 20));
                // The halo spans 3x3 cells so the glow can bleed past the
                // glyph; the other shapes stay cell-sized.
                let span: u16 = if halo { 3 } else { 1 };
                let (img_w, img_h) =
                    (cell.0 as u32 * span as u32, cell.1 as u32 * span as u32);
                let cfg = (shape, col, cell.0, cell.1);
                if self.orb_cfg != Some(cfg) {
                    // Config changed: replace the whole frame set.
                    if self.orb_cfg.is_some() {
                        for i in 0..crate::kitty::ORB_FRAMES {
                            let _ = crate::kitty::delete_image(
                                &mut buf,
                                crate::kitty::ORB_BASE + i,
                            );
                        }
                        self.orb_placed = None;
                    }
                    for i in 0..crate::kitty::ORB_FRAMES {
                        let phase = i as f32 / crate::kitty::ORB_FRAMES as f32;
                        let rgba =
                            crate::kitty::orb_rgba(img_w, img_h, col, shape, phase);
                        let _ = crate::kitty::transmit(
                            &mut buf,
                            crate::kitty::ORB_BASE + i,
                            img_w,
                            img_h,
                            &rgba,
                        );
                    }
                    self.orb_cfg = Some(cfg);
                }
                let frame = if self.orb_blinking() {
                    let t = self.anim_start.elapsed().as_millis() as u64 % ORB_PERIOD_MS;
                    (t * crate::kitty::ORB_FRAMES as u64 / ORB_PERIOD_MS) as u32
                } else {
                    crate::kitty::ORB_STEADY
                };
                let id = crate::kitty::ORB_BASE + frame;
                // Center the (possibly multi-cell) image on the cursor and
                // clip it to the pane rect via a source rectangle.
                let m = self.main_rect;
                let (left, top) = (x as i32 - span as i32 / 2, y as i32 - span as i32 / 2);
                let lc = (m.x as i32 + 1 - left).max(0);
                let tc = (m.y as i32 + 1 - top).max(0);
                let rc = (left + span as i32 - 1 - (m.x + m.width) as i32).max(0);
                let bc = (top + span as i32 - 1 - (m.y + m.height) as i32).max(0);
                let vc = span as i32 - lc - rc;
                let vr = span as i32 - tc - bc;
                if vc > 0 && vr > 0 {
                    let px = (left + lc) as u16;
                    let py = (top + tc) as u16;
                    let src = if span == 1 {
                        (0, 0, 0, 0)
                    } else {
                        (
                            lc as u32 * cell.0 as u32,
                            tc as u32 * cell.1 as u32,
                            vc as u32 * cell.0 as u32,
                            vr as u32 * cell.1 as u32,
                        )
                    };
                    if self.orb_placed != Some((px, py, id, vc as u16, vr as u16)) {
                        let _ = crate::kitty::place_at(
                            &mut buf,
                            py,
                            px,
                            id,
                            1,
                            src,
                            vc as u16,
                            vr as u16,
                            z,
                            0,
                            0,
                        );
                        if let Some((_, _, old, _, _)) = self.orb_placed {
                            if old != id {
                                let _ =
                                    crate::kitty::delete_placement(&mut buf, old, 1);
                            }
                        }
                        self.orb_placed = Some((px, py, id, vc as u16, vr as u16));
                    }
                }
            }
        }

        if !buf.is_empty() {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b7");
            let _ = out.write_all(&buf);
            let _ = out.write_all(b"\x1b8");
            let _ = out.flush();
        }
    }

    fn cursor_sync(&mut self) {
        let in_pane = !self.home && matches!(self.mode, Mode::Normal);
        let (style, tint) = if in_pane && !self.orb_active() && !self.aleph_active() {
            let pane_style = self
                .panes
                .get(self.active)
                .map(|p| p.parser.screen().cursor_style())
                .unwrap_or(0);
            (self.cursor_param(pane_style), self.cursor_rgb())
        } else {
            (0, None)
        };
        let want = (style, tint);
        if self.cursor_applied == Some(want) {
            return;
        }
        self.cursor_applied = Some(want);
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b[{style} q");
        let _ = match tint {
            Some((r, g, b)) => write!(out, "\x1b]12;#{r:02x}{g:02x}{b:02x}\x07"),
            None => write!(out, "\x1b]112\x07"),
        };
        let _ = out.flush();
    }

    fn draw_sidebar(&mut self, f: &mut Frame, area: Rect) {
        if area.width == 0 {
            return;
        }
        let block = self.sidebar_block();
        let inner = block.inner(area);
        self.sidebar_inner = inner;
        f.render_widget(block, area);

        let mut lines: Vec<Line> = Vec::new();
        for (i, p) in self.panes.iter().enumerate() {
            let active = i == self.active;
            let working = self.working(i);
            // Working rows keep the plain gray name — the spinner alone is
            // the indicator, painted orange in its own span.
            let style = if active {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else if working {
                Style::default().fg(Color::Gray)
            } else if let Some(color) = self.status_color(i) {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let anim_style = Style::default()
                .fg(self.spinner_color())
                .add_modifier(Modifier::BOLD);
            let ssh_tag_style = Style::default().fg(Color::Indexed(108));
            // Reserved whenever this pane is ssh'd somewhere — kept out of
            // the collapsed (numbers-only) row, where there's no room.
            let ssh = self.ssh_tag(p.id);
            let ssh_len: usize = if ssh.is_some() { 4 } else { 0 };
            let line = if self.collapsed {
                if active {
                    if working {
                        Line::from(vec![
                            Span::styled(format!(" {}", self.eye()), style),
                            Span::styled(self.working_anim(true), anim_style),
                        ])
                    } else {
                        Line::from(Span::styled(format!(" {} ", self.eye()), style))
                    }
                } else if working {
                    Line::from(vec![
                        Span::styled(format!("{:>2}", i + 1), style),
                        Span::styled(self.working_anim(true), anim_style),
                    ])
                } else {
                    Line::from(Span::styled(format!("{:>2} ", i + 1), style))
                }
            } else if active {
                if let Mode::Rename { buf } = &self.mode {
                    Line::from(vec![
                        Span::styled(format!(" {} ", self.eye()), style),
                        Span::styled(
                            format!("{buf}\u{2588}"),
                            Style::default().fg(Color::Yellow),
                        ),
                    ])
                } else {
                    // Underline covers only the name, not the eye or the
                    // rest of the row; the spinner (if working) sits flush
                    // right, past the underline.
                    let anim = if working {
                        self.working_anim(false)
                    } else {
                        String::new()
                    };
                    let aw = anim.chars().count();
                    let name = truncate(
                        &p.name,
                        (inner.width as usize).saturating_sub(4 + aw + ssh_len),
                    );
                    let mut spans = vec![Span::styled(format!(" {} ", self.eye()), style)];
                    let name_style = style.add_modifier(Modifier::UNDERLINED);
                    if working {
                        spans.extend(self.shimmer_spans(&name, name_style));
                        if ssh.is_some() {
                            spans.push(Span::styled("-SSH", ssh_tag_style));
                        }
                        let pad = (inner.width as usize)
                            .saturating_sub(3 + name.chars().count() + ssh_len + aw);
                        spans.push(Span::raw(" ".repeat(pad)));
                        spans.push(Span::styled(anim, anim_style));
                    } else {
                        spans.push(Span::styled(name.clone(), name_style));
                        if ssh.is_some() {
                            spans.push(Span::styled("-SSH", ssh_tag_style));
                        }
                    }
                    Line::from(spans)
                }
            } else if working {
                // Name field padded so the animation sits flush right; the
                // name itself shimmers while the spinner runs.
                let anim = self.working_anim(false);
                let aw = anim.chars().count() as u16;
                let w = inner.width.saturating_sub(3 + aw + ssh_len as u16) as usize;
                let name = truncate(&p.name, w.saturating_sub(1));
                let mut spans = vec![Span::styled(format!("{:>2} ", i + 1), style)];
                spans.extend(self.shimmer_spans(&name, style));
                if ssh.is_some() {
                    spans.push(Span::styled("-SSH", ssh_tag_style));
                }
                spans.push(Span::raw(" ".repeat(w.saturating_sub(name.chars().count()))));
                spans.push(Span::styled(anim, anim_style));
                Line::from(spans)
            } else {
                let name = truncate(&p.name, inner.width.saturating_sub(4 + ssh_len as u16) as usize);
                if ssh.is_some() {
                    Line::from(vec![
                        Span::styled(format!("{:>2} {name}", i + 1), style),
                        Span::styled("-SSH", ssh_tag_style),
                    ])
                } else {
                    Line::from(Span::styled(format!("{:>2} {name}", i + 1), style))
                }
            };
            lines.push(line);
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let mut spans: Vec<Span> = Vec::new();
        if self.home {
            spans.push(Span::styled(
                format!(" ☾ {}", self.session),
                Style::default().fg(Color::Cyan),
            ));
            let hints = if self.settings.hide_controls {
                format!(" · {} panes", self.panes.len())
            } else if self.chat_focus {
                format!(
                    " · {} panes · chat: Enter send · Esc leave · /wake /sleep",
                    self.panes.len()
                )
            } else {
                format!(
                    " · {} panes · ←↑↓→ select · Enter open · Alt+G wizard · Alt+~ close",
                    self.panes.len()
                )
            };
            spans.push(Span::styled(hints, Style::default().fg(Color::DarkGray)));
            f.render_widget(Paragraph::new(Line::from(spans)), area);
            return;
        }
        if let Mode::Rename { .. } = self.mode {
            spans.push(Span::styled(
                " rename: Enter save · Esc cancel",
                Style::default().fg(Color::Yellow),
            ));
        } else {
            if let Some(p) = self.panes.get(self.active) {
                spans.push(Span::styled(
                    format!(" {} · {}", self.active + 1, p.name),
                    Style::default().fg(Color::Cyan),
                ));
                let title = p.parser.screen().title().to_string();
                if self.working(self.active) {
                    spans.push(Span::styled(
                        " · working",
                        Style::default().fg(self.spinner_color()),
                    ));
                }
                if !title.is_empty() {
                    spans.push(Span::styled(
                        format!(" · {}", truncate(&title, 40)),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                if self.zoom {
                    spans.push(Span::styled(
                        " · ZOOM",
                        Style::default().fg(Color::Magenta).bold(),
                    ));
                }
                if p.scroll > 0 {
                    spans.push(Span::styled(
                        format!(" · SCROLL +{}", p.scroll),
                        Style::default().fg(Color::Yellow).bold(),
                    ));
                }
                if self
                    .copied_at
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
                {
                    spans.push(Span::styled(
                        " · copied",
                        Style::default().fg(Color::Yellow).bold(),
                    ));
                }
            }
            if !self.settings.hide_controls {
                spans.push(Span::styled(
                    "  Alt+N/W new/close · Alt+R rename · Alt+↑↓/1-9 switch · Alt+PgUp/Dn move · Alt+T bar · Alt+Z zoom · ⇧PgUp scroll · Alt+Q detach · Alt+⇧Q kill",
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

/// OSC 52 writes the clipboard through the outer terminal — works locally
/// and over ssh (`zodiac --remote`). wl-copy fires as well in case the
/// terminal is configured to reject OSC 52 clipboard writes.
fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", b64(text.as_bytes()));
    let _ = out.flush();
    if let Ok(mut child) = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        let data = text.as_bytes().to_vec();
        std::thread::spawn(move || {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&data);
                drop(stdin);
            }
            let _ = child.wait();
        });
    }
}

/// Card status: (label, glyph, text color, accent index into ACCENT_RGB).
fn card_status(p: &PaneState) -> (&'static str, &'static str, Color, usize) {
    if p.status == "needs_input" {
        ("needs approval", "⚠", Color::Indexed(203), 0)
    } else if p.thinking {
        ("thinking", "✳", Color::Indexed(135), 1)
    } else if p.status == "working" {
        ("working", "⚡", Color::Indexed(208), 2)
    } else if p.status == "done" {
        ("finished", "✔", Color::Indexed(114), 3)
    } else {
        ("idle", "·", Color::Indexed(245), 4)
    }
}

fn roman(mut n: usize) -> String {
    const M: &[(usize, &str)] = &[(10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut s = String::new();
    for &(v, r) in M {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

fn fmt_uptime(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("up {s}s")
    } else if s < 3600 {
        format!("up {}m", s / 60)
    } else if s < 86400 {
        format!("up {}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("up {}d {}h", s / 86400, (s % 86400) / 3600)
    }
}

/// The version-looking token from a `--version` line ("2.0.35 (Claude
/// Code)" → "2.0.35").
fn version_token(v: &str) -> String {
    v.split_whitespace()
        .find(|t| t.chars().any(|c| c.is_ascii_digit()))
        .unwrap_or(v)
        .to_string()
}

/// ~-abbreviate and keep the tail when a path is too wide for the card.
fn short_dir(cwd: &str, max: usize) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let d = if !home.is_empty() && cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd.to_string()
    };
    let n = d.chars().count();
    if n <= max {
        d
    } else {
        format!("…{}", d.chars().skip(n + 1 - max).collect::<String>())
    }
}

pub fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        s.push(T[(n >> 18 & 63) as usize] as char);
        s.push(T[(n >> 12 & 63) as usize] as char);
        s.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        s.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

/// Greedy word wrap by char count — close enough for the chat panel
/// (terminal-width glyphs only get approximated).
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut cur = String::new();
        let mut len = 0usize;
        let hard_break = |word: &str, out: &mut Vec<String>, cur: &mut String, len: &mut usize| {
            let mut cs = word.chars().peekable();
            while cs.peek().is_some() {
                let chunk: String = cs.by_ref().take(width).collect();
                let cl = chunk.chars().count();
                if cl == width {
                    out.push(chunk);
                } else {
                    *cur = chunk;
                    *len = cl;
                }
            }
        };
        for word in para.split(' ') {
            let wl = word.chars().count();
            if len == 0 {
                if wl <= width {
                    cur = word.into();
                    len = wl;
                } else {
                    hard_break(word, &mut out, &mut cur, &mut len);
                }
            } else if len + 1 + wl <= width {
                cur.push(' ');
                cur.push_str(word);
                len += 1 + wl;
            } else {
                out.push(std::mem::take(&mut cur));
                len = 0;
                if wl <= width {
                    cur = word.into();
                    len = wl;
                } else {
                    hard_break(word, &mut out, &mut cur, &mut len);
                }
            }
        }
        out.push(cur);
    }
    out
}

/// Which markdown emphasis (if any) a run of chat text carries.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct MdStyle {
    bold: bool,
    code: bool,
}

fn apply_md(base: Style, s: MdStyle) -> Style {
    let mut st = base;
    if s.bold {
        st = st.add_modifier(Modifier::BOLD);
    }
    if s.code {
        st = st.fg(Color::Indexed(114));
    }
    st
}

/// A small markdown-lite scanner for the chat transcript: recognizes
/// `**bold**`/`__bold__` and `` `code` `` spans, everything else is
/// literal text. Deliberately skips single-`*`/`_` italics — those
/// delimiters collide too often with ordinary text a coding assistant
/// says (multiplication, `snake_case`, path segments), and a false
/// match there is far more disruptive than losing italics. An unmatched
/// marker (a stray "**" with no closing pair, common mid-stream) falls
/// back to literal text rather than eating the rest of the message.
fn parse_md(text: &str) -> Vec<(String, MdStyle)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(String, MdStyle)> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(end) = md_close(&chars, i + 1, '`', 1) {
                if !plain.is_empty() {
                    out.push((std::mem::take(&mut plain), MdStyle::default()));
                }
                out.push((chars[i + 1..end].iter().collect(), MdStyle { code: true, ..Default::default() }));
                i = end + 1;
                continue;
            }
        } else if (chars[i] == '*' || chars[i] == '_') && chars.get(i + 1) == Some(&chars[i]) {
            let delim = chars[i];
            if let Some(end) = md_close(&chars, i + 2, delim, 2) {
                if !plain.is_empty() {
                    out.push((std::mem::take(&mut plain), MdStyle::default()));
                }
                out.push((chars[i + 2..end].iter().collect(), MdStyle { bold: true, ..Default::default() }));
                i = end + 2;
                continue;
            }
        }
        plain.push(chars[i]);
        i += 1;
    }
    if !plain.is_empty() {
        out.push((plain, MdStyle::default()));
    }
    out
}

/// First index `>= from` where `run` consecutive `delim` chars occur, with
/// at least one char of content in between — an empty `` ` ` `` /`****`
/// isn't emphasis, it's nothing, and shouldn't consume the marker.
fn md_close(chars: &[char], from: usize, delim: char, run: usize) -> Option<usize> {
    let mut j = from;
    while j + run <= chars.len() {
        if j > from && chars[j..j + run].iter().all(|&c| c == delim) {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Greedy word-wrap that keeps each word's markdown style attached, so
/// bold/code spans survive a line break — same wrapping rule as
/// `wrap_text` (break on the last word that still fits), just carrying
/// style instead of discarding it. Adjacent same-style words on one line
/// merge into a single entry, so rendering isn't a Span per word.
fn wrap_md_runs(runs: &[(String, MdStyle)], width: usize) -> Vec<Vec<(String, MdStyle)>> {
    let width = width.max(4);
    let mut lines: Vec<Vec<(String, MdStyle)>> = Vec::new();
    let mut cur: Vec<(String, MdStyle)> = Vec::new();
    let mut len = 0usize;
    for (text, style) in runs {
        for (pi, para) in text.split('\n').enumerate() {
            if pi > 0 {
                lines.push(std::mem::take(&mut cur));
                len = 0;
            }
            for word in para.split(' ') {
                if word.is_empty() {
                    continue;
                }
                let wl = word.chars().count();
                if len == 0 {
                    cur.push((word.to_string(), *style));
                    len = wl;
                } else if len + 1 + wl <= width {
                    if cur.last().is_some_and(|(_, s)| *s == *style) {
                        let last = cur.last_mut().expect("just checked");
                        last.0.push(' ');
                        last.0.push_str(word);
                    } else {
                        cur.push((format!(" {word}"), *style));
                    }
                    len += 1 + wl;
                } else {
                    lines.push(std::mem::take(&mut cur));
                    len = wl;
                    cur.push((word.to_string(), *style));
                }
            }
        }
    }
    lines.push(cur);
    lines
}

/// Decode `send_keys`' space-separated token spec into raw bytes for
/// `T_INPUT`: named keys (Esc, Enter, Tab, Backspace, Ctrl+<letter>), or
/// anything else sent as its literal UTF-8 bytes.
fn decode_keys(spec: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for tok in spec.split_whitespace() {
        match tok {
            "Esc" | "esc" | "Escape" | "escape" => out.push(0x1b),
            "Enter" | "enter" | "Return" | "return" => out.push(b'\r'),
            "Tab" | "tab" => out.push(b'\t'),
            "Backspace" | "backspace" => out.push(0x7f),
            t if t.len() > 5 && t[..5].eq_ignore_ascii_case("ctrl+") => {
                if let Some(c) = t[5..].chars().next() {
                    let up = c.to_ascii_uppercase();
                    if up.is_ascii_uppercase() {
                        out.push(up as u8 - 0x40);
                    } else {
                        out.extend(tok.as_bytes());
                    }
                }
            }
            other => out.extend(other.as_bytes()),
        }
    }
    out
}

/// Case-insensitive subsequence fuzzy match: every char of `pattern` must
/// appear in `text` in order, not necessarily contiguous. `None` when it
/// doesn't match at all. Score rewards runs of consecutive matches and
/// matches right after a word boundary, so "zd" ranks "zodiac dev" above a
/// buried match deep in an unrelated word; shorter names are favored
/// slightly over longer ones of the same match quality.
fn fuzzy_score(text: &str, pattern: &str) -> Option<i32> {
    if pattern.is_empty() {
        return None;
    }
    let tchars: Vec<char> = text.to_lowercase().chars().collect();
    let pchars: Vec<char> = pattern.to_lowercase().chars().collect();
    let mut ti = 0;
    let mut score = 0i32;
    let mut last_matched: Option<usize> = None;
    for &pc in &pchars {
        let idx = (ti..tchars.len()).find(|&j| tchars[j] == pc)?;
        let boundary = idx == 0 || matches!(tchars[idx - 1], '-' | '_' | ' ' | '/' | '.');
        let consecutive = last_matched == Some(idx.wrapping_sub(1));
        score += 1 + if boundary { 3 } else { 0 } + if consecutive { 2 } else { 0 };
        last_matched = Some(idx);
        ti = idx + 1;
    }
    score -= tchars.len() as i32 / 8;
    Some(score)
}

/// "4 minutes ago" for a unix timestamp — how stale the snapshot behind the
/// Alt+⇧R overlay is. Coarse on purpose; nothing here needs seconds.
fn ago(unix_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now.saturating_sub(unix_secs);
    let (n, unit) = match secs {
        0..=59 => return "moments ago".into(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86399 => (secs / 3600, "hour"),
        _ => (secs / 86400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod md_tests {
    use super::*;

    fn plain(t: &str) -> (String, MdStyle) {
        (t.to_string(), MdStyle::default())
    }

    #[test]
    fn parses_bold_and_code() {
        let runs = parse_md("say **hello** then `run it`");
        assert_eq!(
            runs,
            vec![
                plain("say "),
                ("hello".into(), MdStyle { bold: true, code: false }),
                plain(" then "),
                ("run it".into(), MdStyle { bold: false, code: true }),
            ]
        );
    }

    #[test]
    fn underscores_bold_too() {
        let runs = parse_md("__loud__");
        assert_eq!(runs, vec![("loud".into(), MdStyle { bold: true, code: false })]);
    }

    #[test]
    fn unmatched_marker_falls_back_to_literal() {
        // A stray "**" with no closing pair — must not eat the rest of
        // the message, e.g. mid-stream before the model has typed the
        // closing marker yet.
        let runs = parse_md("careful with rm -rf ** now");
        assert_eq!(runs, vec![plain("careful with rm -rf ** now")]);
    }

    #[test]
    fn empty_span_is_not_emphasis() {
        let runs = parse_md("nothing here: ****");
        assert_eq!(runs, vec![plain("nothing here: ****")]);
    }

    #[test]
    fn single_asterisk_is_left_alone() {
        // Deliberately not treated as italic — "2 * 3 * 4" is common
        // enough in ordinary chat text that pairing single stars would
        // misfire constantly.
        let runs = parse_md("2 * 3 * 4 = 24");
        assert_eq!(runs, vec![plain("2 * 3 * 4 = 24")]);
    }

    #[test]
    fn wrap_keeps_style_across_a_line_break() {
        let runs = parse_md("a very **bold** claim indeed");
        let lines = wrap_md_runs(&runs, 10);
        // "a very" | "**bold**" | "claim" | "indeed" roughly, at width 10 —
        // the important thing is "bold" keeps bold=true wherever it lands.
        let bold_word = lines
            .iter()
            .flatten()
            .find(|(t, _)| t.contains("bold"))
            .expect("bold word survived wrapping");
        assert!(bold_word.1.bold, "bold flag lost across the wrap: {bold_word:?}");
    }

    #[test]
    fn wrap_merges_adjacent_same_style_words() {
        let runs = parse_md("plain plain plain");
        let lines = wrap_md_runs(&runs, 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 1, "same-style words should merge into one span");
        assert_eq!(lines[0][0].0, "plain plain plain");
    }
}

#[cfg(test)]
mod jump_tests {
    use super::*;

    #[test]
    fn matches_a_subsequence_in_order() {
        assert!(fuzzy_score("zodiac dev", "zdv").is_some());
        assert!(fuzzy_score("zodiac dev", "vdz").is_none(), "out-of-order must not match");
    }

    #[test]
    fn no_match_when_a_char_is_missing() {
        assert!(fuzzy_score("bear dev", "bearx").is_none());
    }

    #[test]
    fn empty_pattern_never_matches() {
        assert!(fuzzy_score("anything", "").is_none());
    }

    #[test]
    fn word_boundary_start_outranks_a_buried_match() {
        // "dev" starts "dev tools" at a boundary; in "bear dev" it's also
        // right after a boundary (the space) — but a match starting at
        // the very front of the string should still win a tie-break.
        let front = fuzzy_score("dev tools", "dev").unwrap();
        let buried = fuzzy_score("xxdev", "dev").unwrap();
        assert!(front > buried, "front={front} buried={buried}");
    }

    #[test]
    fn consecutive_run_outscores_scattered_hits() {
        // Scattered here deliberately avoids sitting the hits right after
        // word-boundary characters too, so this isolates the consecutive
        // bonus rather than confounding it with boundary bonuses.
        let consecutive = fuzzy_score("bear", "bea").unwrap();
        let scattered = fuzzy_score("bxexaxr", "bea").unwrap();
        assert!(
            consecutive > scattered,
            "consecutive={consecutive} scattered={scattered}"
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(fuzzy_score("Zodiac", "ZOD"), fuzzy_score("zodiac", "zod"));
    }
}

#[cfg(test)]
mod cast_tests {
    use super::*;

    #[test]
    fn named_keys_decode_to_their_control_bytes() {
        assert_eq!(decode_keys("Esc"), vec![0x1b]);
        assert_eq!(decode_keys("Enter"), vec![b'\r']);
        assert_eq!(decode_keys("Tab"), vec![b'\t']);
        assert_eq!(decode_keys("Backspace"), vec![0x7f]);
    }

    #[test]
    fn ctrl_combo_decodes_to_a_control_byte() {
        // Ctrl+U is byte 0x15 — matches the terminal convention of
        // masking the letter down to its control-code range.
        assert_eq!(decode_keys("Ctrl+U"), vec![0x15]);
        assert_eq!(decode_keys("Ctrl+u"), vec![0x15], "case shouldn't matter");
    }

    #[test]
    fn literal_text_passes_through_as_utf8() {
        assert_eq!(decode_keys("y"), b"y".to_vec());
    }

    #[test]
    fn multiple_tokens_concatenate_in_order() {
        assert_eq!(decode_keys("Ctrl+U y Enter"), {
            let mut v = vec![0x15];
            v.extend(b"y");
            v.push(b'\r');
            v
        });
    }

}
