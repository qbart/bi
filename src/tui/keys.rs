//! crossterm key events → [`bi::key::Key`].
//!
//! This is the entire terminal-specific half of input handling. The keymap
//! itself is library code, because "`d` waits for a motion" is editor
//! semantics, not a property of terminals.

use ratatui::crossterm::event::{KeyCode as CtCode, KeyEvent, KeyModifiers};

use bi::key::{Key, KeyCode, Mods};

/// Translates a terminal key event, or drops it.
///
/// `None` means the key names nothing bi handles — function keys, `PageUp`,
/// `Delete`, `Insert`, media keys. Dropping them here is where they already
/// effectively went: they used to reach the keymap and fall through its
/// catch-all arm.
pub fn translate(ev: KeyEvent) -> Option<Key> {
    let code = match ev.code {
        CtCode::Char(c) => KeyCode::Char(c),
        CtCode::Esc => KeyCode::Esc,
        CtCode::Enter => KeyCode::Enter,
        CtCode::Backspace => KeyCode::Backspace,
        CtCode::Tab => KeyCode::Tab,
        // Shift-Tab arrives as a code of its own rather than as a modifier, so
        // it is put back together here. The core has one Tab and reads the
        // shift, which is what the key is.
        CtCode::BackTab => KeyCode::Tab,
        CtCode::Left => KeyCode::Left,
        CtCode::Right => KeyCode::Right,
        CtCode::Up => KeyCode::Up,
        CtCode::Down => KeyCode::Down,
        CtCode::Home => KeyCode::Home,
        CtCode::End => KeyCode::End,
        _ => return None,
    };

    Some(Key {
        code,
        mods: Mods {
            ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
            alt: ev.modifiers.contains(KeyModifiers::ALT),
            shift: ev.modifiers.contains(KeyModifiers::SHIFT) || matches!(ev.code, CtCode::BackTab),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: CtCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn a_plain_char_carries_no_modifiers() {
        let key = translate(ev(CtCode::Char('d'), KeyModifiers::NONE)).unwrap();
        assert_eq!(key, Key::char('d'));
    }

    #[test]
    fn control_survives_translation() {
        let key = translate(ev(CtCode::Char('r'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(key, Key::ctrl('r'));
    }

    #[test]
    fn alt_and_shift_are_carried_even_though_the_keymap_ignores_them() {
        let key =
            translate(ev(CtCode::Char('j'), KeyModifiers::ALT | KeyModifiers::SHIFT)).unwrap();
        assert_eq!(key.code, KeyCode::Char('j'));
        assert!(key.mods.alt && key.mods.shift && !key.mods.ctrl);
    }

    #[test]
    fn the_named_keys_all_map() {
        let pairs = [
            (CtCode::Esc, KeyCode::Esc),
            (CtCode::Enter, KeyCode::Enter),
            (CtCode::Backspace, KeyCode::Backspace),
            (CtCode::Tab, KeyCode::Tab),
            (CtCode::Left, KeyCode::Left),
            (CtCode::Right, KeyCode::Right),
            (CtCode::Up, KeyCode::Up),
            (CtCode::Down, KeyCode::Down),
            (CtCode::Home, KeyCode::Home),
            (CtCode::End, KeyCode::End),
        ];
        for (from, to) in pairs {
            assert_eq!(translate(ev(from, KeyModifiers::NONE)).unwrap().code, to);
        }
    }

    /// Shift-Tab arrives as a code of its own rather than as Tab with a
    /// modifier, and the core has one Tab and reads the shift — so the two
    /// halves are put back together here or `Shift-Tab` unindents nothing.
    #[test]
    fn back_tab_is_a_shifted_tab() {
        let key = translate(ev(CtCode::BackTab, KeyModifiers::NONE)).unwrap();
        assert_eq!(key.code, KeyCode::Tab);
        assert!(key.mods.shift);
    }

    #[test]
    fn a_key_bi_does_not_name_is_dropped() {
        assert!(translate(ev(CtCode::F(1), KeyModifiers::NONE)).is_none());
        assert!(translate(ev(CtCode::PageUp, KeyModifiers::NONE)).is_none());
        assert!(translate(ev(CtCode::Delete, KeyModifiers::NONE)).is_none());
        assert!(translate(ev(CtCode::Insert, KeyModifiers::NONE)).is_none());
    }
}
