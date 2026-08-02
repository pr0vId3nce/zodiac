use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Blits a vt100 screen into the ratatui buffer.
pub struct TermView<'a> {
    pub screen: &'a vt100::Screen,
}

fn conv_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

impl Widget for TermView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        for r in 0..rows.min(area.height) {
            for c in 0..cols.min(area.width) {
                let Some(cell) = self.screen.cell(r, c) else {
                    continue;
                };
                let target = &mut buf[(area.x + c, area.y + r)];
                let contents = cell.contents();
                if contents.is_empty() {
                    target.set_symbol(" ");
                } else {
                    target.set_symbol(&contents);
                }
                let mut style = Style::default()
                    .fg(conv_color(cell.fgcolor()))
                    .bg(conv_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                target.set_style(style);
            }
        }
    }
}

/// Encodes a key event into the byte sequence a real terminal would send.
pub fn encode_key(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let mods = key.modifiers;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);

    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl
    let modp = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi_mod = |ch: char| -> Vec<u8> {
        if modp == 1 {
            if app_cursor && matches!(ch, 'A' | 'B' | 'C' | 'D' | 'H' | 'F') {
                format!("\x1bO{ch}").into_bytes()
            } else {
                format!("\x1b[{ch}").into_bytes()
            }
        } else {
            format!("\x1b[1;{modp}{ch}").into_bytes()
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> {
        if modp == 1 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{modp}~").into_bytes()
        }
    };

    let bytes = match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    ch @ 'a'..='z' => ch as u8 - b'a' + 1,
                    ' ' | '@' => 0,
                    '[' => 27,
                    '\\' => 28,
                    ']' => 29,
                    '^' => 30,
                    '_' | '/' => 31,
                    _ => {
                        let mut b = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                        return Some(out);
                    }
                };
                out.push(b);
            } else {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            out
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if alt {
                vec![0x1b, 0x7f]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi_mod('A'),
        KeyCode::Down => csi_mod('B'),
        KeyCode::Right => csi_mod('C'),
        KeyCode::Left => csi_mod('D'),
        KeyCode::Home => csi_mod('H'),
        KeyCode::End => csi_mod('F'),
        KeyCode::PageUp => csi_tilde(5),
        KeyCode::PageDown => csi_tilde(6),
        KeyCode::Insert => csi_tilde(2),
        KeyCode::Delete => csi_tilde(3),
        KeyCode::F(n @ 1..=4) => {
            let ch = (b'P' + n - 1) as char;
            if modp == 1 {
                format!("\x1bO{ch}").into_bytes()
            } else {
                format!("\x1b[1;{modp}{ch}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            csi_tilde(code)
        }
        _ => return None,
    };
    Some(bytes)
}

/// Encode a mouse event for the inner application, honoring the mouse
/// protocol mode and encoding it requested via DECSET. `x`/`y` are 0-based
/// pane-relative cells. Returns None when the mode doesn't report this kind
/// of event (including mode None — mouse reporting off).
pub fn encode_mouse(
    ev: &crossterm::event::MouseEvent,
    x: u16,
    y: u16,
    mode: vt100::MouseProtocolMode,
    enc: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    use vt100::MouseProtocolMode as M;

    let allowed = match mode {
        M::None => false,
        M::Press => matches!(ev.kind, K::Down(_) | K::ScrollUp | K::ScrollDown),
        M::PressRelease => !matches!(ev.kind, K::Drag(_) | K::Moved),
        M::ButtonMotion => !matches!(ev.kind, K::Moved),
        M::AnyMotion => true,
    };
    if !allowed {
        return None;
    }
    let btn = |b: MouseButton| match b {
        MouseButton::Left => 0u16,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let base = match ev.kind {
        K::Down(b) | K::Up(b) => btn(b),
        K::Drag(b) => btn(b) + 32,
        K::Moved => 35,
        K::ScrollUp => 64,
        K::ScrollDown => 65,
        K::ScrollLeft => 66,
        K::ScrollRight => 67,
    };
    let mut mods = 0u16;
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        mods += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        mods += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        mods += 16;
    }
    let release = matches!(ev.kind, K::Up(_));
    Some(match enc {
        vt100::MouseProtocolEncoding::Sgr => {
            let fin = if release { 'm' } else { 'M' };
            format!("\x1b[<{};{};{}{fin}", base + mods, x + 1, y + 1).into_bytes()
        }
        // Default/Utf8: single-byte fields; release loses button identity.
        _ => {
            let cb = if release { 3 + mods } else { base + mods };
            let coord = |v: u16| (32 + v + 1).min(255) as u8;
            vec![0x1b, b'[', b'M', (32 + cb).min(255) as u8, coord(x), coord(y)]
        }
    })
}
