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
/// editor with it. The clamp lands one byte short of the end so that a
/// trailing newline still ends the last real line instead of opening a
/// phantom empty one after it — consistent with how an offset sitting on any
/// other newline is treated: it still belongs to the line before it.
pub(crate) fn line_of(src: &str, offset: usize) -> usize {
    let offset = offset.min(src.len().saturating_sub(1));
    1 + src[..offset].bytes().filter(|&b| b == b'\n').count()
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
