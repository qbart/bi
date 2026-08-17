//! bee's config: the types, the parser, and the source a frontend supplies.
//!
//! The library owns the types and the parser because a keymap is editor
//! semantics — the same argument `key.rs` makes for `Key`. A frontend owns
//! only where the file lives. See `docs/specs/config.md`.

/// A problem with a config file. Reported, never fatal: an editor you cannot
/// launch because of a typo in its config is an editor you cannot use to fix
/// the typo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based, into whichever file it came from.
    pub line: usize,
    pub message: String,
}

/// The 1-based line a byte offset falls on.
///
/// `toml_edit` reports spans as byte ranges; a diagnostic wants a line. Out of
/// range clamps to the last line rather than panicking, because a span that
/// disagrees with its source is a dependency bug and should not take the
/// editor with it. The clamp is `src.len()`, never less — end-of-string is
/// always a valid slice boundary, but `len - 1` is not when the source's last
/// character is multi-byte UTF-8 (TOML source routinely is), so the slice
/// index itself is never computed by subtracting from `len`. A trailing
/// newline in the source is instead handled after slicing, by not counting
/// it: it still ends the last real line rather than opening a phantom empty
/// one after it, consistent with how an offset sitting on any other newline
/// is treated — it still belongs to the line before it.
pub(crate) fn line_of(src: &str, offset: usize) -> usize {
    let end = offset.min(src.len());
    let mut newlines = src[..end].bytes().filter(|&b| b == b'\n').count();
    if end == src.len() && src.ends_with('\n') {
        newlines -= 1;
    }
    1 + newlines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_of_counts_newlines_before_the_offset() {
        let src = "one\ntwo\nthree\n";
        assert_eq!(line_of(src, 0), 1);
        assert_eq!(line_of(src, 3), 1, "the newline itself still ends line 1");
        assert_eq!(line_of(src, 4), 2);
        assert_eq!(line_of(src, 8), 3);
        assert_eq!(line_of(src, 999), 3, "past the end clamps rather than panics");
    }

    /// The clamp must never land on a computed index that isn't a char
    /// boundary. `"a€"` is 4 bytes ('a' then a 3-byte '€'); an offset of
    /// exactly `src.len()` — an entirely ordinary "end of file" span — used
    /// to be clamped to `len - 1`, which falls inside the '€' and panics.
    #[test]
    fn line_of_does_not_panic_on_multibyte_utf8_at_the_end() {
        let src = "a€";
        assert_eq!(line_of(src, src.len()), 1);
        assert_eq!(line_of(src, src.len() + 10), 1, "past the end still clamps");
    }

    #[test]
    fn line_of_counts_newlines_around_multibyte_utf8() {
        let src = "héllo\nwörld\n";
        assert_eq!(line_of(src, 0), 1);
        assert_eq!(line_of(src, src.find('\n').unwrap() + 1), 2, "just past the first newline");
        assert_eq!(line_of(src, src.len()), 2, "trailing newline still ends the last line");
    }

    /// Pins the dependency's span behaviour, because every diagnostic line
    /// number in this module is built on it. If `toml_edit` renames this API,
    /// this test says so in thirty seconds instead of leaving every
    /// diagnostic silently pointing at line 1.
    #[test]
    fn toml_edit_reports_key_spans() {
        let src = "[options]\nnumber = 5\n";
        let doc: toml_edit::Document<&str> = toml_edit::Document::parse(src).unwrap();
        let table = doc["options"].as_table().unwrap();
        let (key, _) = table.get_key_value("number").unwrap();
        let span = key.span().expect("keys carry spans after a fresh parse");
        assert_eq!(line_of(src, span.start), 2);
    }
}
