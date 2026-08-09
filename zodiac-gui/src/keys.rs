//! winit -> crossterm key translation (roadmap 3.4). The zodiac library's
//! `client_core::encode_key` speaks crossterm's `KeyEvent`; this module
//! turns winit keyboard events into that shape so both clients share one
//! byte encoder. Pure functions — unit tested offscreen.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// winit modifier state -> crossterm modifier flags.
pub fn modifiers(m: ModifiersState) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if m.shift_key() {
        out |= KeyModifiers::SHIFT;
    }
    if m.control_key() {
        out |= KeyModifiers::CONTROL;
    }
    if m.alt_key() {
        out |= KeyModifiers::ALT;
    }
    if m.super_key() {
        out |= KeyModifiers::SUPER;
    }
    out
}

/// Translate one winit logical key + modifier state into the crossterm
/// `KeyEvent` shape `encode_key` consumes. Returns None for keys that have
/// no terminal meaning (bare modifiers, media keys, ...).
pub fn to_key_event(key: &Key, mods: ModifiersState) -> Option<KeyEvent> {
    let m = modifiers(mods);
    let code = match key {
        Key::Character(s) => {
            let c = s.chars().next()?;
            KeyCode::Char(c)
        }
        Key::Named(n) => match n {
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Tab => {
                if mods.shift_key() {
                    KeyCode::BackTab
                } else {
                    KeyCode::Tab
                }
            }
            NamedKey::Space => KeyCode::Char(' '),
            NamedKey::Backspace => KeyCode::Backspace,
            NamedKey::Escape => KeyCode::Esc,
            NamedKey::ArrowUp => KeyCode::Up,
            NamedKey::ArrowDown => KeyCode::Down,
            NamedKey::ArrowLeft => KeyCode::Left,
            NamedKey::ArrowRight => KeyCode::Right,
            NamedKey::Home => KeyCode::Home,
            NamedKey::End => KeyCode::End,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            NamedKey::Insert => KeyCode::Insert,
            NamedKey::Delete => KeyCode::Delete,
            NamedKey::F1 => KeyCode::F(1),
            NamedKey::F2 => KeyCode::F(2),
            NamedKey::F3 => KeyCode::F(3),
            NamedKey::F4 => KeyCode::F(4),
            NamedKey::F5 => KeyCode::F(5),
            NamedKey::F6 => KeyCode::F(6),
            NamedKey::F7 => KeyCode::F(7),
            NamedKey::F8 => KeyCode::F(8),
            NamedKey::F9 => KeyCode::F(9),
            NamedKey::F10 => KeyCode::F(10),
            NamedKey::F11 => KeyCode::F(11),
            NamedKey::F12 => KeyCode::F(12),
            _ => return None,
        },
        _ => return None,
    };
    Some(KeyEvent::new(code, m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;
    use zodiac::client_core::encode_key;

    #[test]
    fn plain_char_round_trips_to_utf8() {
        let ev = to_key_event(
            &Key::Character(SmolStr::new("a")),
            ModifiersState::default(),
        )
        .unwrap();
        assert_eq!(ev.code, KeyCode::Char('a'));
        assert_eq!(encode_key(&ev, false).unwrap(), b"a");
    }

    #[test]
    fn ctrl_c_becomes_etx() {
        let ev = to_key_event(&Key::Character(SmolStr::new("c")), ModifiersState::CONTROL).unwrap();
        assert_eq!(ev.modifiers, KeyModifiers::CONTROL);
        assert_eq!(encode_key(&ev, false).unwrap(), vec![0x03]);
    }

    #[test]
    fn arrows_honor_application_cursor_mode() {
        let ev = to_key_event(&Key::Named(NamedKey::ArrowUp), ModifiersState::default()).unwrap();
        assert_eq!(encode_key(&ev, false).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(&ev, true).unwrap(), b"\x1bOA");
    }

    #[test]
    fn shift_tab_is_backtab() {
        let ev = to_key_event(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT).unwrap();
        assert_eq!(ev.code, KeyCode::BackTab);
        assert_eq!(encode_key(&ev, false).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn named_keys_and_rejects() {
        let ev = to_key_event(&Key::Named(NamedKey::Enter), ModifiersState::default()).unwrap();
        assert_eq!(encode_key(&ev, false).unwrap(), b"\r");
        let ev = to_key_event(&Key::Named(NamedKey::F5), ModifiersState::default()).unwrap();
        assert_eq!(encode_key(&ev, false).unwrap(), b"\x1b[15~");
        assert!(to_key_event(&Key::Named(NamedKey::Shift), ModifiersState::default()).is_none());
    }

    #[test]
    fn alt_char_gets_esc_prefix() {
        let ev = to_key_event(&Key::Character(SmolStr::new("x")), ModifiersState::ALT).unwrap();
        assert_eq!(encode_key(&ev, false).unwrap(), b"\x1bx");
    }
}
