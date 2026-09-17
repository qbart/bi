//! gpui keystrokes → [`bi::key::Key`].
//!
//! The mirror of `tui::keys::translate`, and the whole of what this frontend
//! knows about input: the keymap itself is library code. See
//! `docs/specs/gui.md`.

use gpui::Keystroke;

use bi::key::{Key, KeyCode, Mods};

/// Translates a keystroke, or drops it.
///
/// `None` is where the keys bi does not read already went in the terminal:
/// function keys, `pageup`, `delete`, media keys.
pub fn translate(ks: &Keystroke) -> Option<Key> {
    let code = match ks.key.as_str() {
        "escape" => KeyCode::Esc,
        "enter" => KeyCode::Enter,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "space" => KeyCode::Char(' '),
        // What the press would have typed, when that is one character: a
        // shifted letter arrives as the capital, a shifted `;` as `:`, and a
        // non-Latin layout as what it actually produces. Under ctrl the
        // platform withholds it — the typed character would be a control
        // byte — and the keycap's own name is the character bi wants.
        _ => match ks.key_char.as_deref().or(Some(ks.key.as_str())) {
            Some(s) if s.chars().count() == 1 => KeyCode::Char(s.chars().next()?),
            _ => return None,
        },
    };

    Some(Key {
        code,
        mods: Mods { ctrl: ks.modifiers.control, alt: ks.modifiers.alt, shift: ks.modifiers.shift },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn ks(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke { modifiers, key: key.into(), key_char: key_char.map(String::from) }
    }

    #[test]
    fn a_plain_letter_is_its_character() {
        assert_eq!(translate(&ks("d", Some("d"), Modifiers::default())), Some(Key::char('d')));
    }

    #[test]
    fn a_shifted_letter_is_the_capital_and_carries_shift() {
        let key = translate(&ks("d", Some("D"), Modifiers { shift: true, ..Default::default() }));
        assert_eq!(key.map(|k| k.code), Some(KeyCode::Char('D')));
        assert!(key.unwrap().mods.shift, "as the terminal reports it");
    }

    #[test]
    fn a_shifted_symbol_is_what_it_typed() {
        let key = translate(&ks(";", Some(":"), Modifiers { shift: true, ..Default::default() }));
        assert_eq!(key.map(|k| k.code), Some(KeyCode::Char(':')));
    }

    #[test]
    fn ctrl_falls_back_to_the_keycap_when_no_character_was_typed() {
        let key = translate(&ks("r", None, Modifiers { control: true, ..Default::default() }));
        assert_eq!(key, Some(Key::ctrl('r')));
    }

    #[test]
    fn named_keys_translate_by_name_and_the_rest_are_dropped() {
        assert_eq!(
            translate(&ks("escape", None, Modifiers::default())),
            Some(Key::code(KeyCode::Esc))
        );
        assert_eq!(
            translate(&ks("enter", None, Modifiers::default())),
            Some(Key::code(KeyCode::Enter))
        );
        assert_eq!(translate(&ks("space", Some(" "), Modifiers::default())), Some(Key::char(' ')));
        assert_eq!(translate(&ks("f5", None, Modifiers::default())), None);
        assert_eq!(translate(&ks("pageup", None, Modifiers::default())), None);
    }
}
