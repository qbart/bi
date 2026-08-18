//! bi's config: the types, the parser, and the source a frontend supplies.
//!
//! The library owns the types and the parser because a keymap is editor
//! semantics — the same argument `key.rs` makes for `Key`. A frontend owns
//! only where the file lives. See `docs/specs/config.md`.

use std::sync::OnceLock;

use crate::editor::LineNumbers;

mod keys;
mod parse;

pub use keys::{KeyMode, Keymap, Lookup, parse_key, parse_keys, spell};
pub use parse::parse;

/// Where a frontend gets config text from.
///
/// A trait rather than a path, because "where does a config file live on this
/// platform, or inside this embedding host" is the one part of config that
/// genuinely varies per frontend — and the library must not learn what a
/// filesystem is to serve it. An embedder can read from a database or a
/// bundled resource.
///
/// `Ok(None)` means there is no config, which is normal and not a problem.
/// `Err` means there was one and it could not be read, which is.
pub trait ConfigSource {
    fn config(&self) -> anyhow::Result<Option<String>>;
}

impl<T: ConfigSource> ConfigSource for std::rc::Rc<T> {
    fn config(&self) -> anyhow::Result<Option<String>> {
        (**self).config()
    }
}

/// A problem with a config file. Reported, never fatal: an editor you cannot
/// launch because of a typo in its config is an editor you cannot use to fix
/// the typo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based, into whichever file it came from.
    pub line: usize,
    pub message: String,
}

/// bi's defaults, as the file that documents them.
pub const DEFAULT_TOML: &str = include_str!("default.toml");

/// A value an option can hold, in the one shape both `:set` and TOML can
/// produce. `:set` parses a string into one; the parser converts a TOML value
/// into one. Neither needs to know what any particular option is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionValue {
    Int(i64),
    Bool(bool),
    /// A value no option can hold — a string, an array, a table. Carried
    /// rather than rejected on the spot so the option itself gets to say what
    /// it wanted, in the one place those messages live.
    Other,
}

/// The `:set` namespace. One field per option, spelled as `:set` spells it,
/// because `:set number 5` and `number = 5` are two ways to reach one setting
/// and there is nothing to be gained by giving it two names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub number: LineNumbers,
    /// Off unless asked for: vim does not light the buffer up on a plain
    /// `/`, and the status line's `[3/17]` says how many matches there are
    /// without painting them.
    pub hlsearch: bool,
}

impl Options {
    /// The single place a name becomes a field. `:set` and the TOML parser
    /// both come through here, so an option cannot exist for one and not the
    /// other.
    pub fn set(&mut self, name: &str, value: OptionValue) -> Result<(), String> {
        match (name, value) {
            ("number", OptionValue::Int(n)) => match LineNumbers::from_setting(n) {
                Some(lines) => self.number = lines,
                None => return Err("number takes 0 (off), -1 (relative) or a count".into()),
            },
            ("number", _) => return Err("number takes 0 (off), -1 (relative) or a count".into()),
            ("hlsearch", OptionValue::Bool(on)) => self.hlsearch = on,
            ("hlsearch", _) => return Err("hlsearch takes true or false".into()),
            _ => return Err(format!("unknown option: {name}")),
        }
        Ok(())
    }

    /// What `:set <option>` reports when given no value.
    pub fn get(&self, name: &str) -> Option<OptionValue> {
        Some(match name {
            "number" => OptionValue::Int(self.number.setting()),
            "hlsearch" => OptionValue::Bool(self.hlsearch),
            _ => return None,
        })
    }
}

/// Everything a config file can say. Options and the keymap today; a theme
/// joins them in step 2 of `docs/specs/config.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub options: Options,
    /// Rewrites of bi's own keys, per mode. Empty is the default and means
    /// every key keeps its built-in meaning.
    pub keys: Keymap,
}

impl Default for Config {
    /// The compiled-in defaults, parsed once.
    ///
    /// Parsed rather than constructed so that `default.toml` is exercised on
    /// every run — if the shipped file stops parsing, no test has to catch it
    /// because nothing starts.
    fn default() -> Self {
        static DEFAULT: OnceLock<Config> = OnceLock::new();
        DEFAULT
            .get_or_init(|| {
                let bare = Config { options: Options::default(), keys: Keymap::default() };
                parse(DEFAULT_TOML, bare).expect("bi's own default.toml must parse").0
            })
            .clone()
    }
}

/// The 1-based line a byte offset falls on.
///
/// `toml_edit` reports spans as byte ranges; a diagnostic wants a line. Out of
/// range clamps to the last line rather than panicking, because a span that
/// disagrees with its source is a dependency bug and should not take the
/// editor with it. Counting runs over `src.as_bytes()`, not `src`, so the
/// clamp needs no char-boundary care — slicing a byte slice can never land
/// mid-character. A trailing newline in the source is handled after counting,
/// by not counting it: it still ends the last real line rather than opening a
/// phantom empty one after it, consistent with how an offset sitting on any
/// other newline is treated — it still belongs to the line before it.
pub(crate) fn line_of(src: &str, offset: usize) -> usize {
    let end = offset.min(src.len());
    let mut newlines = src.as_bytes()[..end].iter().filter(|&&b| b == b'\n').count();
    if end == src.len() && src.ends_with('\n') {
        newlines -= 1;
    }
    1 + newlines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::LineNumbers;

    #[test]
    fn shipped_defaults_agree_with_the_rust_fallback() {
        // `default.toml` is the documented source of the defaults and
        // `Options::default()` is what an embedder with no config gets. They
        // must say the same thing, and nothing but this test keeps them honest.
        assert_eq!(Config::default().options, Options::default());
    }

    /// The same rule as the options above, for the one keymap setting that has
    /// a value rather than being a table: `default.toml` documents the leader
    /// and `Keymap::default()` is what an embedder with no config gets, so the
    /// two must say the same key.
    #[test]
    fn the_shipped_leader_agrees_with_the_rust_fallback() {
        assert_eq!(Config::default().keys.leader(), Keymap::default().leader());
        assert_eq!(Keymap::default().leader(), Some(crate::key::Key::char(' ')));
    }

    #[test]
    fn set_and_get_round_trip_every_option() {
        let mut options = Options::default();

        assert_eq!(options.set("number", OptionValue::Int(5)), Ok(()));
        assert_eq!(options.number, LineNumbers::Every(5));
        assert_eq!(options.get("number"), Some(OptionValue::Int(5)));

        assert_eq!(options.set("hlsearch", OptionValue::Bool(true)), Ok(()));
        assert!(options.hlsearch);
        assert_eq!(options.get("hlsearch"), Some(OptionValue::Bool(true)));
    }

    #[test]
    fn set_rejects_unknown_names_and_bad_values() {
        let mut options = Options::default();

        assert_eq!(options.set("nmber", OptionValue::Int(5)), Err("unknown option: nmber".into()));
        assert_eq!(options.get("nmber"), None);

        assert_eq!(
            options.set("number", OptionValue::Int(-7)),
            Err("number takes 0 (off), -1 (relative) or a count".into())
        );
        assert_eq!(options.number, LineNumbers::Every(1), "a rejected set changes nothing");

        assert_eq!(
            options.set("hlsearch", OptionValue::Int(1)),
            Err("hlsearch takes true or false".into())
        );
    }

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
