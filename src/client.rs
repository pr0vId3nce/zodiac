use std::os::unix::net::UnixStream;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::protocol::{Frame as SrvFrame, *};
use crate::term::{encode_key, encode_mouse, TermView};

const SIDEBAR_WIDTH: u16 = 24;
const SIDEBAR_COLLAPSED: u16 = 4;
const CLIENT_SCROLLBACK: usize = 10_000;

/// How recently a background pane must have produced output to count as
/// "in progress" (orange) rather than "finished" (green).
const IN_PROGRESS_WINDOW: Duration = Duration::from_secs(5);

const WORKING_COLOR: Color = Color::Indexed(208);

/// Output arriving this soon after a resize is a SIGWINCH repaint, not
/// activity (matches the server-side squelch).
const RESIZE_SQUELCH: Duration = Duration::from_millis(1200);

/// The focused pane's sidebar row shows an eye instead of its number; it
/// blinks (closes briefly) once per period.
const EYE_PERIOD_MS: u64 = 4000;
const EYE_BLINK_MS: u64 = 160;
const SETTINGS_ROWS: usize = 20;

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
const HOME_CARD_W: u16 = 26;
const HOME_CARD_H: u16 = 13;
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
}

enum Mode {
    Normal,
    Rename { buf: String },
    Settings,
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
    resized_at: Option<Instant>,
    home: bool,
    home_sel: usize,
    home_cols: usize,
    home_state: Option<SessionState>,
    home_queried: Option<Instant>,
    /// Card layout from the last home draw: (rect, pane id, accent index).
    home_cards: Vec<(Rect, u64, usize, bool)>,
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
        settings: Settings::load(),
        settings_row: 0,
        resized_at: None,
        home: true, // always open to the home page
        home_sel: 0,
        home_cols: 1,
        home_state: None,
        home_queried: None,
        home_cards: Vec::new(),
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
        app.cursor_sync();
        if app.main_size != app.sent_size {
            app.send_resize();
        }
        // Refresh home-page data ~1/s while it's open.
        if app.home
            && app
                .home_queried
                .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
        {
            app.send(T_QUERY, 0, &[]);
            app.home_queried = Some(Instant::now());
        }
        // Fast ticks only while an animation is on screen (working panes or
        // the settings preview); otherwise wake for the next eye blink.
        let tick = if app.home {
            250
        } else if app.any_working() || matches!(app.mode, Mode::Settings) {
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

    fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::SrvGone => {
                if !self.quit {
                    self.exit_msg = "zodiac: lost connection to server (another client attached, or the server exited)";
                    self.quit = true;
                }
            }
            AppEvent::Srv(f) => self.handle_frame(f),
            AppEvent::Term(event) => match event {
                Event::Key(key) => self.handle_key(key),
                Event::Mouse(m) => self.handle_mouse(m),
                Event::Paste(text) => {
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
        if let Mode::Settings = self.mode {
            return;
        }
        if self.home {
            if let K::Down(MouseButton::Left) = m.kind {
                let pos = Position::new(m.column, m.row);
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
            self.send(T_INPUT, id, &bytes);
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

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.selection = None;
        self.selecting = false;

        if let Mode::Rename { buf } = &mut self.mode {
            match key.code {
                KeyCode::Enter => {
                    let name = buf.trim().to_string();
                    self.mode = Mode::Normal;
                    if !name.is_empty() {
                        if let Some(p) = self.panes.get_mut(self.active) {
                            p.name = name.clone();
                            let id = p.id;
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
                KeyCode::Left => self.cycle_setting(-1),
                KeyCode::Right | KeyCode::Enter => self.cycle_setting(1),
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
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    if let Some(p) = self.panes.get(self.active) {
                        self.mode = Mode::Rename {
                            buf: p.name.clone(),
                        };
                    }
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
    /// frame means working, but "✳" proves nothing: Claude Code's title
    /// spinner cycles ✳/⠂/⠐/… while working and merely rests on ✳ when idle
    /// — so ✳ frames must fall through to the output-recency check or
    /// working panes flicker. Recency only counts for panes running a known
    /// agent — an ordinary TUI emits output forever and would spin
    /// permanently.
    fn working(&self, i: usize) -> bool {
        let p = &self.panes[i];
        let title = p.parser.screen().title();
        let recent = p.last_output.is_some_and(|t| t.elapsed() < IN_PROGRESS_WINDOW)
            && agent_from_title(title).is_some();
        title_state(title) == TitleState::Working || recent
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
    }

    /// Home-page navigation. Returns true when the key was consumed.
    fn handle_home_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::ALT) {
            return false;
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
                if self.home_sel + cols < n {
                    self.home_sel += cols;
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
                let i = self.card_icon_idx() as isize;
                self.settings.card_icon = CARD_ICON_SIZES
                    [(i + dir).rem_euclid(CARD_ICON_SIZES.len() as isize) as usize]
                    .0
                    .to_string();
            }
            9 => {
                self.settings.card_outline =
                    cycle_pick(CARD_OUTLINES, self.card_outline(), dir);
            }
            10 => {
                let cur = color_by_name(&self.settings.select_color, "gold").0;
                self.settings.select_color = cycle_color_name(cur, dir);
            }
            11 => {
                let i = self.select_weight_idx() as isize;
                self.settings.select_weight = SELECT_WEIGHTS
                    [(i + dir).rem_euclid(SELECT_WEIGHTS.len() as isize) as usize]
                    .0
                    .to_string();
            }
            12 => {
                self.settings.select_style =
                    cycle_pick(SELECT_STYLES, self.select_style(), dir);
            }
            13 => {
                self.settings.card_numeral =
                    cycle_pick(CARD_NUMERALS, self.card_numeral_style(), dir);
            }
            14 => {
                self.settings.claude_style =
                    cycle_pick(CLAUDE_STYLES, self.claude_style(), dir);
            }
            15 => return self.cycle_finish_sound(dir),
            16 => self.settings.connection_watch = !self.settings.connection_watch,
            17 => {
                self.settings.cursor_style = cycle_pick(CURSOR_TYPES, self.cursor_type(), dir);
            }
            18 => {
                self.settings.cursor_blink =
                    cycle_pick(CURSOR_BLINKS, self.cursor_blink(), dir);
            }
            _ => {
                let mut choices: Vec<&str> = vec!["off"];
                choices.extend(COLOR_CHOICES.iter().map(|(n, _, _)| *n));
                self.settings.cursor_color =
                    cycle_pick(&choices, self.cursor_color_name(), dir);
            }
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
            self.draw_home(f, body);
            self.draw_status(f, status);
            if let Mode::Settings = self.mode {
                self.draw_settings(f, area);
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

        if let Mode::Settings = self.mode {
            self.draw_settings(f, area);
        }
    }

    fn draw_settings(&self, f: &mut Frame, area: Rect) {
        let w = 48.min(area.width);
        let h = 25.min(area.height);
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
        let inner = block.inner(rect);
        f.render_widget(block, rect);

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
                "Card icon",
                CARD_ICON_SIZES[self.card_icon_idx()].0,
                vec![Span::styled(
                    "✳".to_string(),
                    Style::default().fg(Color::Indexed(209)).bold(),
                )],
            ),
            row(
                9,
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
                10,
                "Select color",
                color_by_name(&self.settings.select_color, "gold").0,
                vec![Span::styled(
                    "▆▆▆".to_string(),
                    Style::default().fg(self.select_color()),
                )],
            ),
            row(
                11,
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
                12,
                "Select style",
                self.select_style(),
                vec![Span::styled(
                    if self.select_style() == "glow" { "◜◝" } else { "┌┐" }.to_string(),
                    Style::default().fg(self.select_color()),
                )],
            ),
            row(
                13,
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
                14,
                "Claude style",
                self.claude_style(),
                vec![Span::styled(
                    if self.claude_style() == "soft" { "●" } else { "■" }.to_string(),
                    Style::default().fg(Color::Indexed(209)),
                )],
            ),
            row(
                15,
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
                16,
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
                17,
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
                18,
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
                19,
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
            Line::default(),
            Line::from(Span::styled(
                " ↑/↓ select · ←/→ change · Esc close",
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
        let Some(state) = self.home_state.clone() else {
            let mid = Rect {
                x: area.x,
                y: area.y + area.height / 2,
                width: area.width,
                height: 1,
            };
            f.render_widget(
                Paragraph::new("☾ gathering the arcana…")
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
        let cols = (((area.width.saturating_sub(2) + HOME_GAP_X) / (HOME_CARD_W + HOME_GAP_X))
            as usize)
            .clamp(1, n);
        self.home_cols = cols;
        let rows = n.div_ceil(cols);
        let vis_rows = (((area.height.saturating_sub(1) + HOME_GAP_Y)
            / (HOME_CARD_H + HOME_GAP_Y)) as usize)
            .max(1);
        let sel_row = self.home_sel / cols;
        let row_off = sel_row.saturating_sub(vis_rows - 1);
        let shown = rows.min(vis_rows) as u16;
        let grid_w = cols as u16 * HOME_CARD_W + (cols as u16 - 1) * HOME_GAP_X;
        let grid_h = shown * HOME_CARD_H + shown.saturating_sub(1) * HOME_GAP_Y;
        let x0 = area.x + area.width.saturating_sub(grid_w) / 2;
        let y0 = area.y + area.height.saturating_sub(grid_h) / 2;
        for (i, p) in state.panes.iter().enumerate() {
            let r = i / cols;
            if r < row_off || r >= row_off + vis_rows {
                continue;
            }
            let rect = Rect {
                x: x0 + (i % cols) as u16 * (HOME_CARD_W + HOME_GAP_X),
                y: y0 + (r - row_off) as u16 * (HOME_CARD_H + HOME_GAP_Y),
                width: HOME_CARD_W,
                height: HOME_CARD_H,
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
        let name = truncate(&p.name, inner.width.saturating_sub(6) as usize);
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
        // Text ornaments only in the fallback — the painted card already
        // has the emblem and stars there.
        let orn = |s: &str| {
            if self.kitty_on {
                Line::default()
            } else {
                Line::from(Span::styled(s.to_string(), gold))
            }
        };
        // Emblem: Claude's ✳ in coral for claude panes, a `>_` prompt
        // otherwise (mirrors the painted card art).
        let emblem = if self.kitty_on {
            Line::default()
        } else if p.agent.as_deref() == Some("claude") {
            Line::from(vec![
                Span::styled("✦  ".to_string(), gold),
                Span::styled(
                    "✳".to_string(),
                    Style::default().fg(Color::Indexed(209)).bold(),
                ),
                Span::styled("  ✦".to_string(), gold),
            ])
        } else {
            Line::from(Span::styled(">_".to_string(), gold.bold()))
        };
        let lines = vec![
            emblem,
            Line::default(),
            Line::from(Span::styled(
                format!("{} · {}", self.card_numeral(num), name),
                title_style,
            )),
            Line::from(Span::styled("──────────", faint)),
            Line::default(),
            Line::from(Span::styled(agent_line, gold)),
            Line::from(Span::styled(fmt_uptime(p.uptime_ms), dim)),
            Line::from(Span::styled(
                format!("{glyph} {label}"),
                Style::default().fg(scolor).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(dir, faint)),
            Line::default(),
            orn("✦  ·  ✦"),
        ];
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
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
        // images would bleed through its background) and off home entirely.
        if !self.home || matches!(self.mode, Mode::Settings) {
            if !self.kitty_placed.is_empty() {
                let _ = crate::kitty::delete_placements(&mut out);
                let _ = out.flush();
                self.kitty_placed.clear();
                // d=a wiped every visible placement, pane images included —
                // drop the compositor's tracking so it re-places them.
                self.placed_gfx.clear();
                self.orb_placed = None;
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

        // Desired placements: one per card, keyed by (rect, image id).
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
        let target: Option<u64> = if self.home || matches!(self.mode, Mode::Settings) {
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
                    let name =
                        truncate(&p.name, (inner.width as usize).saturating_sub(4 + aw));
                    let mut spans = vec![Span::styled(format!(" {} ", self.eye()), style)];
                    let name_style = style.add_modifier(Modifier::UNDERLINED);
                    if working {
                        spans.extend(self.shimmer_spans(&name, name_style));
                        let pad = (inner.width as usize)
                            .saturating_sub(3 + name.chars().count() + aw);
                        spans.push(Span::raw(" ".repeat(pad)));
                        spans.push(Span::styled(anim, anim_style));
                    } else {
                        spans.push(Span::styled(name.clone(), name_style));
                    }
                    Line::from(spans)
                }
            } else if working {
                // Name field padded so the animation sits flush right; the
                // name itself shimmers while the spinner runs.
                let anim = self.working_anim(false);
                let aw = anim.chars().count() as u16;
                let w = inner.width.saturating_sub(3 + aw) as usize;
                let name = truncate(&p.name, w.saturating_sub(1));
                let mut spans = vec![Span::styled(format!("{:>2} ", i + 1), style)];
                spans.extend(self.shimmer_spans(&name, style));
                spans.push(Span::raw(" ".repeat(w.saturating_sub(name.chars().count()))));
                spans.push(Span::styled(anim, anim_style));
                Line::from(spans)
            } else {
                let name = truncate(&p.name, inner.width.saturating_sub(4) as usize);
                Line::from(Span::styled(format!("{:>2} {name}", i + 1), style))
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
            spans.push(Span::styled(
                format!(" · {} panes · ←↑↓→ select · Enter open · Alt+~ close", self.panes.len()),
                Style::default().fg(Color::DarkGray),
            ));
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
                let recent = p
                    .last_output
                    .is_some_and(|t| t.elapsed() < IN_PROGRESS_WINDOW)
                    && agent_from_title(&title).is_some();
                if title_state(&title) == TitleState::Working || recent {
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
            spans.push(Span::styled(
                "  Alt+N/W new/close · Alt+R rename · Alt+↑↓/1-9 switch · Alt+PgUp/Dn move · Alt+T bar · Alt+Z zoom · ⇧PgUp scroll · Alt+Q detach · Alt+⇧Q kill",
                Style::default().fg(Color::DarkGray),
            ));
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
