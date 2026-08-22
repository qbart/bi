//! The two coordinate translations at the protocol boundary: LSP positions ↔
//! bi's char offsets, and file paths ↔ `file://` URIs.
//!
//! Positions are the protocol's one real trap. The wire carries line +
//! column, where the column counts units of the *negotiated* encoding —
//! UTF-16 code units by mandate, UTF-8 bytes when the server grants bi's
//! preference. Document sync never touches this (its ranges sit at column 0
//! on both sides — see `sync.rs`); what does is everything that names a spot
//! in *current* text: a diagnostic arriving, a request position leaving. Both
//! convert against the live rope, which is why the historical-text problem
//! other editors fight never arises here.

use std::path::{Path, PathBuf};

use ropey::Rope;

use super::types::Position;

/// The column unit a server counts in. What `initialize` negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    fn width(self, c: char) -> u32 {
        match self {
            Self::Utf8 => c.len_utf8() as u32,
            Self::Utf16 => c.len_utf16() as u32,
        }
    }

    /// What `:lsp` prints.
    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
        }
    }
}

/// A wire position as a char offset into `rope`.
///
/// Clamps exactly as the spec tells servers to: a line past the end is the
/// end of the document, a column past the line's end is the line's end, and
/// a column landing inside one character's units floors to that character.
/// Servers send positions computed against their own copy of the text; the
/// moment the two copies disagree — a stale diagnostic in flight — clamping
/// is what turns a would-be panic into an offset that is merely old.
pub fn char_of(rope: &Rope, pos: Position, encoding: Encoding) -> usize {
    let line = pos.line as usize;
    if line >= rope.len_lines() {
        return rope.len_chars();
    }
    let start = rope.line_to_char(line);
    let mut units = 0;
    for (i, c) in rope.line(line).chars().enumerate() {
        if c == '\n' || c == '\r' {
            return start + i;
        }
        let width = encoding.width(c);
        if units + width > pos.character {
            return start + i;
        }
        units += width;
    }
    start + rope.line(line).len_chars()
}

/// A char offset as a wire position. The inverse of [`char_of`], for the
/// requests later features send.
pub fn position_of(rope: &Rope, at: usize, encoding: Encoding) -> Position {
    let at = at.min(rope.len_chars());
    let line = rope.char_to_line(at);
    let start = rope.line_to_char(line);
    let character = rope.slice(start..at).chars().map(|c| encoding.width(c)).sum();
    Position { line: line as u32, character }
}

/// A wire column as a char offset into one line of text, clamped to the
/// line's end. The line-level twin of [`char_of`], for text that lives in a
/// `String` rather than a rope — a references row read straight off the disk.
pub fn col_to_char(line: &str, units: u32, encoding: Encoding) -> usize {
    let mut acc = 0;
    for (i, c) in line.chars().enumerate() {
        if c == '\n' || c == '\r' {
            return i;
        }
        let width = encoding.width(c);
        if acc + width > units {
            return i;
        }
        acc += width;
    }
    line.chars().count()
}

/// The one spelling of a path every URI derives from: absolutized against
/// the process's cwd, symlinks resolved when the file exists on disk (a new
/// file is still a document). One function, so `didOpen` and every later
/// lookup cannot disagree about what a file is called.
pub fn canonical(path: &Path) -> Result<PathBuf, String> {
    let abs = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(abs.canonicalize().unwrap_or(abs))
}

/// `path` as a `file://` URI. The path must already be absolute — making it
/// so is the caller's job, because "absolute relative to what" is a fact
/// about the session, not the codec.
pub fn uri_of(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char)
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// The path a `file://` URI names, or `None` for any other scheme — a server
/// can legally send `untitled:` or its own inventions, and those name nothing
/// on this filesystem.
pub fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // An authority may sit between the scheme and the path; the only ones
    // that mean "this machine" are empty and `localhost`.
    let path = match rest.strip_prefix('/') {
        Some(_) => rest,
        None => rest.strip_prefix("localhost")?,
    };
    if !path.starts_with('/') {
        return None;
    }

    let mut bytes = Vec::with_capacity(path.len());
    let mut chars = path.bytes();
    while let Some(b) = chars.next() {
        if b != b'%' {
            bytes.push(b);
            continue;
        }
        let hex = [chars.next()?, chars.next()?];
        let hex = std::str::from_utf8(&hex).ok()?;
        bytes.push(u8::from_str_radix(hex, 16).ok()?);
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn ascii_positions_agree_in_both_encodings() {
        let r = rope("fn main() {\n    body\n}\n");
        let pos = Position { line: 1, character: 4 };
        assert_eq!(char_of(&r, pos, Encoding::Utf8), 16);
        assert_eq!(char_of(&r, pos, Encoding::Utf16), 16);
        assert_eq!(position_of(&r, 16, Encoding::Utf8), pos);
        assert_eq!(position_of(&r, 16, Encoding::Utf16), pos);
    }

    #[test]
    fn the_encodings_disagree_after_a_multibyte_char_and_both_are_right() {
        // 'é' is 2 UTF-8 bytes, 1 UTF-16 unit; '𝕊' is 4 bytes, 2 units.
        let r = rope("é𝕊x\n");
        // After both chars: char offset 2.
        assert_eq!(position_of(&r, 2, Encoding::Utf8), Position { line: 0, character: 6 });
        assert_eq!(position_of(&r, 2, Encoding::Utf16), Position { line: 0, character: 3 });
        assert_eq!(char_of(&r, Position { line: 0, character: 6 }, Encoding::Utf8), 2);
        assert_eq!(char_of(&r, Position { line: 0, character: 3 }, Encoding::Utf16), 2);
    }

    #[test]
    fn a_column_inside_one_chars_units_floors_to_the_char() {
        // Byte 1 of 'é', unit 1 of '𝕊': positions no cursor can occupy.
        let r = rope("é𝕊\n");
        assert_eq!(char_of(&r, Position { line: 0, character: 1 }, Encoding::Utf8), 0);
        assert_eq!(char_of(&r, Position { line: 0, character: 2 }, Encoding::Utf16), 1);
    }

    #[test]
    fn out_of_range_positions_clamp_the_way_the_spec_says() {
        let r = rope("ab\ncd");
        // A line past the end is the end of the document.
        assert_eq!(char_of(&r, Position { line: 9, character: 0 }, Encoding::Utf8), 5);
        // A column past the line's end is the line's end, before the newline.
        assert_eq!(char_of(&r, Position { line: 0, character: 99 }, Encoding::Utf8), 2);
        // On the last line — no newline to stop before — it is the doc's end.
        assert_eq!(char_of(&r, Position { line: 1, character: 99 }, Encoding::Utf8), 5);
        // And an offset past the end clamps going the other way.
        assert_eq!(position_of(&r, 99, Encoding::Utf8), Position { line: 1, character: 2 });
    }

    #[test]
    fn the_empty_document_is_position_zero_everywhere() {
        let r = rope("");
        assert_eq!(char_of(&r, Position { line: 0, character: 0 }, Encoding::Utf16), 0);
        assert_eq!(position_of(&r, 0, Encoding::Utf16), Position { line: 0, character: 0 });
    }

    #[test]
    fn a_line_column_converts_like_its_rope_twin() {
        assert_eq!(col_to_char("é𝕊x", 6, Encoding::Utf8), 2);
        assert_eq!(col_to_char("é𝕊x", 3, Encoding::Utf16), 2);
        assert_eq!(col_to_char("ab", 99, Encoding::Utf8), 2, "clamped to the line end");
        assert_eq!(col_to_char("ab\n", 99, Encoding::Utf8), 2, "the newline is not a column");
    }

    #[test]
    fn a_plain_path_round_trips_through_its_uri() {
        let path = Path::new("/home/user/src/main.rs");
        let uri = uri_of(path);
        assert_eq!(uri, "file:///home/user/src/main.rs");
        assert_eq!(path_of(&uri).unwrap(), path);
    }

    #[test]
    fn spaces_and_unicode_percent_encode_and_come_back() {
        let path = Path::new("/home/u ser/pröj/a.rs");
        let uri = uri_of(path);
        assert!(!uri.contains(' '), "{uri}");
        assert_eq!(uri, "file:///home/u%20ser/pr%C3%B6j/a.rs");
        assert_eq!(path_of(&uri).unwrap(), path);
    }

    #[test]
    fn foreign_schemes_and_foreign_hosts_name_nothing_here() {
        assert_eq!(path_of("untitled:Untitled-1"), None);
        assert_eq!(path_of("https://example.com/x"), None);
        assert_eq!(path_of("file://otherhost/x"), None);
        // `localhost` is this machine spelled out.
        assert_eq!(path_of("file://localhost/x").unwrap(), Path::new("/x"));
    }

    #[test]
    fn truncated_percent_escapes_are_refused_not_panicked_on() {
        assert_eq!(path_of("file:///a%2"), None);
        assert_eq!(path_of("file:///a%zz"), None);
    }
}
