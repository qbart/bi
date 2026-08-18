//! The system clipboard, as a seam rather than an implementation.
//!
//! The library does not learn what a clipboard is, the same way it does not
//! learn what a filesystem is: it names what it needs and a frontend supplies
//! it. `src/tui/` implements this with OSC 52 escape sequences — no
//! dependency, and it works over SSH, which a native clipboard library cannot.
//! An embedder with a display server of its own implements it with that
//! instead, and nothing here changes.
//!
//! See `docs/specs/clipboard.md`.

/// What bi needs from the world outside it.
///
/// Both sides are fallible and `get` can legitimately come back empty:
/// `Ok(None)` means the clipboard holds nothing, or the terminal declined to
/// say what it holds — many refuse, because a program that can read your
/// clipboard can read the password you copied a moment ago. That is an
/// outcome, not an error.
pub trait SystemClipboard {
    fn set(&self, text: &str) -> anyhow::Result<()>;
    fn get(&self) -> anyhow::Result<Option<String>>;
}
