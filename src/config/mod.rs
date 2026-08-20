//! bi's config: the types, the parser, and the source a frontend supplies.
//!
//! The library owns the types and the parser because a keymap is editor
//! semantics — the same argument `key.rs` makes for `Key`. A frontend owns
//! only where the file lives. See `docs/specs/config.md`.

use std::sync::OnceLock;

use crate::editor::LineNumbers;

mod keys;
mod parse;

pub use keys::{Bind, KeyMode, Keymap, Lookup, listing, parse_key, parse_keys, spell};
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

    /// The text of `themes/<name>.toml`, if the frontend has one.
    ///
    /// `Ok(None)` means no such file — try a built-in — and is normal, not a
    /// problem. It has a default so an embedder that has no themes directory
    /// keeps compiling and gets the built-ins. See `docs/specs/theme.md`.
    fn theme(&self, _name: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

impl<T: ConfigSource> ConfigSource for std::rc::Rc<T> {
    fn config(&self) -> anyhow::Result<Option<String>> {
        (**self).config()
    }

    fn theme(&self, name: &str) -> anyhow::Result<Option<String>> {
        (**self).theme(name)
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionValue {
    Int(i64),
    Bool(bool),
    /// A string, which `theme` is and nothing else is yet. Its arrival is why
    /// this enum is no longer `Copy`: one owned `String` costs less than a
    /// lifetime on a type both `:set` and the TOML parser construct.
    Str(String),
    /// A value no option can hold — a string, an array, a table. Carried
    /// rather than rejected on the spot so the option itself gets to say what
    /// it wanted, in the one place those messages live.
    Other,
}

/// The `:set` namespace. One field per option, spelled as `:set` spells it,
/// because `:set number 5` and `number = 5` are two ways to reach one setting
/// and there is nothing to be gained by giving it two names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub number: LineNumbers,
    /// The theme by name, resolved through `Theme::resolve` — a file in the
    /// frontend's `themes/` directory, else a built-in, else the default.
    ///
    /// An option rather than a `:theme` command, so that `:set theme ansi`
    /// and `theme = "ansi"` are two ways to reach one setting, exactly as
    /// `:set number 5` and `number = 5` are. That is a feature *removed*
    /// rather than added: there is no second command to design.
    pub theme: String,
    /// The theme to use instead when the session is remote — see
    /// [`crate::editor::Editor::set_remote`].
    ///
    /// A second name rather than a modifier on the first, because the point is
    /// to tell the two apart at a glance: a window that is editing files on
    /// another machine should not look identical to one that is not. The
    /// default is the light theme, which no local session gets by default, so
    /// the distinction is visible out of the box.
    pub ssh_theme: String,
    /// Off unless asked for: vim does not light the buffer up on a plain
    /// `/`, and the status line's `[3/17]` says how many matches there are
    /// without painting them.
    pub hlsearch: bool,

    /// How wide a `\t` is drawn.
    pub tab_width: usize,
    /// Whether an indent is written as spaces. True by default, which is not
    /// vim's default and is deliberate — see `docs/specs/indent.md`.
    pub expandtab: bool,
    /// How far `>` moves, in columns. 0 means "whatever `tab_width` says".
    pub shiftwidth: usize,
    /// Whether a new line starts under the one above it.
    pub autoindent: bool,
    /// Whether each level of indentation gets a vertical line down it.
    pub indent_guides: bool,
    /// Whether `TODO:` and its friends are picked out of the text.
    pub todo_comments: bool,
    /// Whether a colour literal is drawn in the colour it names.
    pub color_swatches: bool,
    /// How many enclosing blocks repeat their opening line after the line that
    /// closes them, innermost first. 0 is off — the honest spelling, since a
    /// feature whose size is a number needs no second option saying whether
    /// the number counts. See `docs/specs/tree-sitter-context.md`.
    pub context_depth: usize,
    /// How many enclosing blocks are drawn over the top rows of the pane,
    /// outermost first — the sticky header. 0 is off. Counted the other way
    /// round from `context_depth` because it answers the other question: what
    /// closed here is the innermost thing, what contains you is the outermost.
    pub context_header_depth: usize,
    /// How many rows a block must span before it earns either.
    pub context_min_lines: usize,
    /// How long a yank stays lit, in milliseconds. 0 is a flash of no time at
    /// all, which is the honest spelling of off.
    pub yank_flash: usize,
    /// Whether the file picker skips what the project says are not its files.
    /// Nothing else consults it: `:e` on an ignored path has always worked.
    pub gitignore: bool,

    /// What a write tidies up on its way out — see `docs/specs/trim.md`.
    ///
    /// A struct rather than five more fields because they are one feature with
    /// a master switch, and because `Buffer` takes the bundle. That is a fact
    /// about the Rust, though, and not about the config: the five are spelled
    /// `trim_trailing` and the rest, flat, like every other option. The
    /// grouping is in the prefix, where a reader can see it, rather than in a
    /// table that made `[options]` a namespace with one exception in it.
    pub trim: crate::trim::Trim,
}

impl Default for Options {
    /// Hand-written since `theme` has a name for a default rather than an
    /// empty one, and `Options::default()` is what an embedder with no config
    /// gets. `shipped_defaults_agree_with_the_rust_fallback` keeps this and
    /// `default.toml` saying the same thing.
    fn default() -> Self {
        let indent = crate::indent::Indent::default();
        Options {
            number: LineNumbers::default(),
            hlsearch: false,
            theme: crate::theme::DEFAULT_THEME.to_string(),
            ssh_theme: "gruvbox-light".to_string(),
            tab_width: indent.tab_width,
            expandtab: indent.expandtab,
            shiftwidth: indent.shiftwidth,
            autoindent: indent.autoindent,
            indent_guides: true,
            todo_comments: true,
            color_swatches: true,
            context_depth: 1,
            context_header_depth: 1,
            context_min_lines: 1,
            yank_flash: 150,
            gitignore: true,
            trim: crate::trim::Trim::default(),
        }
    }
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
            ("theme", OptionValue::Str(name)) => self.theme = name,
            ("theme", _) => return Err("theme takes the name of a theme, in quotes".into()),
            ("ssh_theme", OptionValue::Str(name)) => self.ssh_theme = name,
            ("ssh_theme", _) => {
                return Err("ssh_theme takes the name of a theme, in quotes".into());
            }
            // A tab of no width would put every column on top of the last one,
            // so the floor is 1 rather than 0. `shiftwidth` *may* be 0: that is
            // how it says "follow tab_width".
            ("tab_width", OptionValue::Int(n)) if n >= 1 => self.tab_width = n as usize,
            ("tab_width", _) => return Err("tab_width takes a count of 1 or more".into()),
            ("shiftwidth", OptionValue::Int(n)) if n >= 0 => self.shiftwidth = n as usize,
            ("shiftwidth", _) => {
                return Err("shiftwidth takes a count, or 0 to follow tab_width".into());
            }
            ("expandtab", OptionValue::Bool(on)) => self.expandtab = on,
            ("expandtab", _) => return Err("expandtab takes true or false".into()),
            ("autoindent", OptionValue::Bool(on)) => self.autoindent = on,
            ("autoindent", _) => return Err("autoindent takes true or false".into()),
            ("indent_guides", OptionValue::Bool(on)) => self.indent_guides = on,
            ("indent_guides", _) => return Err("indent_guides takes true or false".into()),
            ("todo_comments", OptionValue::Bool(on)) => self.todo_comments = on,
            ("todo_comments", _) => return Err("todo_comments takes true or false".into()),
            ("color_swatches", OptionValue::Bool(on)) => self.color_swatches = on,
            ("color_swatches", _) => return Err("color_swatches takes true or false".into()),
            ("context_depth", OptionValue::Int(n)) if n >= 0 => self.context_depth = n as usize,
            ("context_depth", _) => {
                return Err("context_depth takes a count, or 0 to turn it off".into());
            }
            ("context_header_depth", OptionValue::Int(n)) if n >= 0 => {
                self.context_header_depth = n as usize;
            }
            ("context_header_depth", _) => {
                return Err("context_header_depth takes a count, or 0 to turn it off".into());
            }
            ("context_min_lines", OptionValue::Int(n)) if n >= 1 => {
                self.context_min_lines = n as usize;
            }
            ("context_min_lines", _) => {
                return Err("context_min_lines takes a count of 1 or more".into());
            }
            ("gitignore", OptionValue::Bool(on)) => self.gitignore = on,
            ("gitignore", _) => return Err("gitignore takes true or false".into()),
            ("yank_flash", OptionValue::Int(n)) if n >= 0 => self.yank_flash = n as usize,
            ("yank_flash", _) => {
                return Err("yank_flash takes milliseconds, or 0 to turn it off".into());
            }
            ("trim_on_write", OptionValue::Bool(on)) => self.trim.on_write = on,
            ("trim_trailing", OptionValue::Bool(on)) => self.trim.trailing = on,
            ("trim_first_line", OptionValue::Bool(on)) => self.trim.first_line = on,
            ("trim_last_line", OptionValue::Bool(on)) => self.trim.last_line = on,
            ("trim_final_newline", OptionValue::Bool(on)) => self.trim.final_newline = on,
            (
                "trim_on_write" | "trim_trailing" | "trim_first_line" | "trim_last_line"
                | "trim_final_newline",
                _,
            ) => return Err(format!("{name} takes true or false")),
            _ => return Err(format!("unknown option: {name}")),
        }
        Ok(())
    }

    /// The theme name actually in force.
    ///
    /// One place decides, so `:set`, the config reader and the resolver cannot
    /// disagree about which of the two names is live.
    pub fn active_theme(&self, remote: bool) -> &str {
        if remote { &self.ssh_theme } else { &self.theme }
    }

    /// What `:set <option>` reports when given no value.
    pub fn get(&self, name: &str) -> Option<OptionValue> {
        Some(match name {
            "number" => OptionValue::Int(self.number.setting()),
            "hlsearch" => OptionValue::Bool(self.hlsearch),
            "theme" => OptionValue::Str(self.theme.clone()),
            "ssh_theme" => OptionValue::Str(self.ssh_theme.clone()),
            "tab_width" => OptionValue::Int(self.tab_width as i64),
            "shiftwidth" => OptionValue::Int(self.shiftwidth as i64),
            "expandtab" => OptionValue::Bool(self.expandtab),
            "autoindent" => OptionValue::Bool(self.autoindent),
            "indent_guides" => OptionValue::Bool(self.indent_guides),
            "todo_comments" => OptionValue::Bool(self.todo_comments),
            "color_swatches" => OptionValue::Bool(self.color_swatches),
            "context_depth" => OptionValue::Int(self.context_depth as i64),
            "context_header_depth" => OptionValue::Int(self.context_header_depth as i64),
            "context_min_lines" => OptionValue::Int(self.context_min_lines as i64),
            "yank_flash" => OptionValue::Int(self.yank_flash as i64),
            "gitignore" => OptionValue::Bool(self.gitignore),
            "trim_on_write" => OptionValue::Bool(self.trim.on_write),
            "trim_trailing" => OptionValue::Bool(self.trim.trailing),
            "trim_first_line" => OptionValue::Bool(self.trim.first_line),
            "trim_last_line" => OptionValue::Bool(self.trim.last_line),
            "trim_final_newline" => OptionValue::Bool(self.trim.final_newline),
            _ => return None,
        })
    }

    /// How many cells the line-number column takes for a file of `lines`
    /// lines: the digits of the last number, and one space after them.
    ///
    /// In the core rather than the frontend, where it started, because it is a
    /// fact about the options and the file and not about a terminal — and
    /// because the core has to know it to put anything in the *middle* of a
    /// pane. The frontend still draws the column; it no longer decides how
    /// wide it is on its own.
    pub fn gutter_width(&self, lines: usize) -> usize {
        match self.number {
            crate::editor::LineNumbers::Off => 0,
            _ => lines.to_string().len() + 1,
        }
    }

    /// The indentation settings, bundled for the code that edits text.
    ///
    /// `Buffer` takes one of these rather than reaching for `Options`, which is
    /// what will let these four become per-file without the call sites moving.
    pub fn indent(&self) -> crate::indent::Indent {
        crate::indent::Indent {
            tab_width: self.tab_width,
            expandtab: self.expandtab,
            shiftwidth: self.shiftwidth,
            autoindent: self.autoindent,
        }
    }
}

/// A sparse set of option values: a *layer*, not a configuration.
///
/// It names the options it has an opinion about and says nothing about the
/// rest, which is what lets bi's defaults, your config, the file's type, its
/// project and your last `:set` be five statements of different scope rather
/// than five whole configurations fighting over one.
///
/// A list of name/value pairs rather than a mirror of [`Options`] with an
/// `Option` on every field: applying it goes through `Options::set`, the one
/// place a name becomes a field, so a patch cannot hold an option `:set`
/// cannot, and an option added later works in every layer the day it exists.
/// Ordered, so a patch that says a thing twice ends on the second.
///
/// See `docs/specs/options.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptionPatch(Vec<(String, OptionValue)>);

impl OptionPatch {
    pub fn set(&mut self, name: impl Into<String>, value: OptionValue) {
        self.0.push((name.into(), value));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether this layer has an opinion about `name`.
    pub fn holds(&self, name: &str) -> bool {
        self.0.iter().any(|(key, _)| key == name)
    }

    /// Lays this layer over `options`, and reports what it could not apply.
    ///
    /// A bad value drops that one entry rather than the layer, the same rule
    /// the config file follows: an option you cannot set is a message, not a
    /// reason to throw away the four that were fine.
    pub fn apply_to(&self, options: &mut Options) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, value) in &self.0 {
            if let Err(message) = options.set(name, value.clone()) {
                problems.push(message);
            }
        }
        problems
    }
}

/// What a language needs whatever your config says.
///
/// Deliberately tiny, and only for what a language *requires* or has
/// *universally settled* — a Makefile with spaces does not run, and gofmt
/// writes tabs whether or not you like them. Taste belongs in
/// `[filetype.<name>]`, which is applied after this and can undo it.
pub fn filetype_defaults(filetype: &str) -> OptionPatch {
    let mut patch = OptionPatch::default();
    match filetype {
        "make" => {
            patch.set("expandtab", OptionValue::Bool(false));
            patch.set("tab_width", OptionValue::Int(8));
        }
        // Tabs, because gofmt writes them; the width stays yours, because
        // gofmt has nothing to say about how wide a tab looks.
        "go" => patch.set("expandtab", OptionValue::Bool(false)),
        // Two trailing spaces are a hard line break in Markdown — actual
        // syntax, in a format where whitespace is content.
        "markdown" => patch.set("trim_trailing", OptionValue::Bool(false)),
        _ => {}
    }
    patch
}

/// Everything a config file can say. The theme is named here and resolved by
/// [`crate::theme::Theme::resolve`], because a name is config and a palette is
/// a second file — see `docs/specs/theme.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub options: Options,
    /// Rewrites of bi's own keys, per mode. Empty is the default and means
    /// every key keeps its built-in meaning.
    pub keys: Keymap,
    /// `[filetype.<name>]` — options for one kind of file, laid over
    /// `[options]` and over bi's own built-in table for that type.
    ///
    /// Keyed by the name `crate::syntax::filetype` gives a file, which is the
    /// same name its grammar is chosen by.
    pub filetypes: std::collections::BTreeMap<String, OptionPatch>,
    /// `[alternate]` — the other file, as a pattern and the paths to try.
    ///
    /// A `Vec` rather than a map because the *order* is the rule: the first
    /// pattern that matches decides, so `*_test.go` has to be tried before
    /// `*.go`. See `docs/specs/alternate.md`.
    pub alternates: Vec<(String, Vec<String>)>,
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
                let bare = Config {
                    options: Options::default(),
                    keys: Keymap::default(),
                    filetypes: Default::default(),
                    alternates: Vec::new(),
                };
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

        assert_eq!(options.set("tab_width", OptionValue::Int(8)), Ok(()));
        assert_eq!(options.get("tab_width"), Some(OptionValue::Int(8)));
        assert_eq!(options.set("shiftwidth", OptionValue::Int(2)), Ok(()));
        assert_eq!(options.get("shiftwidth"), Some(OptionValue::Int(2)));
        assert_eq!(options.set("expandtab", OptionValue::Bool(false)), Ok(()));
        assert_eq!(options.get("expandtab"), Some(OptionValue::Bool(false)));
        assert_eq!(options.set("autoindent", OptionValue::Bool(false)), Ok(()));
        assert_eq!(options.get("autoindent"), Some(OptionValue::Bool(false)));

        assert_eq!(
            options.indent(),
            crate::indent::Indent {
                tab_width: 8,
                shiftwidth: 2,
                expandtab: false,
                autoindent: false,
            }
        );
    }

    /// `shiftwidth` may be 0 — that is how it says "follow tab_width" — and
    /// `tab_width` may not, because a tab of no width puts every column on top
    /// of the last one.
    #[test]
    fn the_widths_refuse_what_they_cannot_mean() {
        let mut options = Options::default();

        assert!(options.set("shiftwidth", OptionValue::Int(0)).is_ok());
        assert!(options.set("tab_width", OptionValue::Int(0)).is_err());
        assert!(options.set("tab_width", OptionValue::Int(-3)).is_err());
        assert_eq!(options.tab_width, 4, "a rejected set changes nothing");
        assert!(options.set("expandtab", OptionValue::Int(1)).is_err());
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
