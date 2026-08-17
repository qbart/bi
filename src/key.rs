//! bi's own key vocabulary.
//!
//! The keymap in [`crate::input`] is editor semantics, so it belongs in the
//! library — but crossterm's `KeyEvent` does not. A frontend translates its
//! native events into these types, and the core never learns what a terminal
//! is.
//!
//! The set of codes is deliberately only what the keymap reads. Growing it is a
//! line here and a line in the frontend's translation; guessing at codes nobody
//! handles is dead weight.

/// A key, stripped of everything a frontend knows and the core does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Modifiers held with the key.
///
/// `alt` and `shift` are carried even though the keymap reads neither today.
/// This is the type a config-driven keymap will parse into, and widening it
/// later means revisiting every match arm in `input.rs` — `<A-j>` is what the
/// next keymap wants, and it costs nothing to have room for it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
}

impl Key {
    pub fn new(code: KeyCode, mods: Mods) -> Self {
        Self { code, mods }
    }

    /// A bare key — no modifiers.
    pub fn code(code: KeyCode) -> Self {
        Self { code, mods: Mods::default() }
    }

    pub fn char(c: char) -> Self {
        Self::code(KeyCode::Char(c))
    }

    pub fn ctrl(c: char) -> Self {
        Self::new(KeyCode::Char(c), Mods { ctrl: true, ..Mods::default() })
    }
}
