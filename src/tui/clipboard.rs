//! The system clipboard over OSC 52.
//!
//! `ESC ] 52 ; c ; <base64> BEL` — the terminal does the work, so there is no
//! dependency and, unlike a native clipboard library, **it works over SSH**.
//! That is the deciding argument: bi runs in a terminal, and a terminal is
//! very often not on the machine the user is sitting at, which is exactly
//! where a library talking to a local display server has nothing to talk to.
//!
//! Reading is the weak half. It asks the terminal to write the clipboard back
//! on stdin, and many terminals refuse by default — a program that can read
//! your clipboard can read the password you copied a moment ago. So the read
//! waits briefly and reports honestly rather than hanging for a reply that is
//! never coming. See `docs/specs/clipboard.md`.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event};

use bi::clipboard::SystemClipboard;

/// How long to wait for a terminal to answer a clipboard read.
///
/// Long enough for a local terminal and a busy one, short enough that a
/// terminal which will never answer costs a pause rather than a hang. There is
/// no way to tell those two apart in advance — the protocol has no "I refuse".
const REPLY: Duration = Duration::from_millis(100);

pub struct Osc52;

impl SystemClipboard for Osc52 {
    fn set(&self, text: &str) -> Result<()> {
        let mut out = std::io::stdout();
        // `c` is the clipboard proper. Some terminals also take `p` for the
        // primary selection; one code for one register, as `"+` and `"*` are
        // one register in the keymap.
        write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
        out.flush()?;
        Ok(())
    }

    fn get(&self) -> Result<Option<String>> {
        let mut out = std::io::stdout();
        // `?` asks for it back. The reply arrives on stdin as an event, in the
        // same stream as keystrokes.
        write!(out, "\x1b]52;c;?\x07")?;
        out.flush()?;

        let deadline = Instant::now() + REPLY;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if !event::poll(left)? {
                break;
            }
            // Crossterm surfaces an OSC reply as a paste when it can parse one,
            // which is the only shape of it available without a raw stdin
            // reader of our own.
            if let Event::Paste(text) = event::read()? {
                return Ok(Some(text));
            }
        }
        Ok(None)
    }
}

/// Standard base64, no line breaks — what OSC 52 wants.
///
/// Written out rather than pulled in: it is twenty lines, and a dependency for
/// twenty lines is a dependency to audit, update and compile forever.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            // A chunk of one encodes two characters and two `=`, a chunk of
            // two encodes three and one.
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The escape sequence is testable where it is built; talking to a real
    /// terminal is not something a test gets to do.
    #[test]
    fn base64_matches_the_standard_on_every_padding_case() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_carries_bytes_a_terminal_would_otherwise_eat() {
        // A newline and a non-ASCII character: the two things that make the
        // difference between an encoded payload and a corrupted one.
        assert_eq!(base64("a\nb".as_bytes()), "YQpi");
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }
}
