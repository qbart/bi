//! Colour, as bi's own type.
//!
//! The core says `keyword`; a frontend decides what `keyword` looks like. A
//! terminal turns [`Color`] into an ANSI escape, a GUI into an RGB triple it
//! picks itself, and neither spelling reaches the other. `syntax.rs` has
//! emitted capture names rather than styles since it was written precisely so
//! this module could exist without touching it.
//!
//! See `docs/specs/theme.md`.

use std::collections::BTreeMap;

use toml_edit::{Document, Item, Table, Value};

use crate::config::{Diagnostic, line_of};

/// The sixteen names, which resolve against the **terminal's own palette**.
/// A user who has tuned solarized writes `"green"` and gets theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ansi {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
}

/// Three spellings, on purpose: a name for the terminal's palette, an index
/// for the 256-colour cube, a triple for anyone who means one exact colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Ansi(Ansi),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// A colour and the attributes beside it.
///
/// `reverse` is here because the focused window's status row uses it, and a
/// theme that cannot say "reverse video" cannot reproduce what bi already
/// looked like — which is the bar the `ansi` built-in has to clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Style {
    pub fn fg(color: Color) -> Self {
        Self { fg: Some(color), ..Self::default() }
    }
}

impl Color {
    /// `"#rrggbb"`, `"color238"`, or one of the sixteen names.
    ///
    /// `Err` carries what to tell the user, because the parser has the line
    /// number and this has the reason, and joining them anywhere else means
    /// one of the two travels further than it needs to.
    pub fn parse(text: &str) -> Result<Self, String> {
        if let Some(hex) = text.strip_prefix('#') {
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("`{text}` is not a #rrggbb colour"));
            }
            let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or(0);
            return Ok(Color::Rgb(byte(0), byte(2), byte(4)));
        }

        if let Some(index) = text.strip_prefix("color") {
            return match index.parse::<u8>() {
                Ok(n) => Ok(Color::Indexed(n)),
                Err(_) => Err(format!("`{text}` is not color0 through color255")),
            };
        }

        let ansi = match text {
            "black" => Ansi::Black,
            "red" => Ansi::Red,
            "green" => Ansi::Green,
            "yellow" => Ansi::Yellow,
            "blue" => Ansi::Blue,
            "magenta" => Ansi::Magenta,
            "cyan" => Ansi::Cyan,
            "gray" | "grey" => Ansi::Gray,
            "darkgray" | "darkgrey" => Ansi::DarkGray,
            "lightred" => Ansi::LightRed,
            "lightgreen" => Ansi::LightGreen,
            "lightyellow" => Ansi::LightYellow,
            "lightblue" => Ansi::LightBlue,
            "lightmagenta" => Ansi::LightMagenta,
            "lightcyan" => Ansi::LightCyan,
            "white" => Ansi::White,
            _ => {
                return Err(format!(
                    "`{text}` is not a colour — try a name, `color238`, or `#rrggbb`"
                ));
            }
        };
        Ok(Color::Ansi(ansi))
    }
}

/// bi's own furniture: everything drawn that is not parsed code.
///
/// One field per thing on the screen, rather than the eight constants that
/// used to sit at the top of `tui/render.rs`. Eight of twenty-five is worse
/// than none — a theme that recolours the text and leaves a magenta picker
/// border and a blue mode badge behind looks broken rather than themed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ui {
    /// The frame, under everything. `None` leaves the terminal's own showing
    /// through, which is what bi did before it had themes and what a theme
    /// built from ANSI names wants — you asked for *your* green, and the
    /// background tuned alongside it comes with it.
    pub background: Option<Color>,
    /// Text no capture claimed. `None` for the same reason.
    pub foreground: Option<Color>,

    pub cursorline: Style,
    pub selection: Style,
    pub search: Style,
    pub cursor_alt: Style,

    pub gutter: Style,
    pub gutter_current: Style,
    pub rule: Style,
    pub filler: Style,
    /// The vertical line down each level of indentation. Furniture, like
    /// `rule` — it marks structure rather than naming anything in the text.
    pub indent_guide: Style,

    pub mode_normal: Style,
    pub mode_insert: Style,
    pub mode_pick: Style,

    pub status: Style,
    pub status_muted: Style,
    pub status_inactive: Style,
    pub statusline: Style,

    pub tree_dir: Style,
    pub tree_link: Style,
    pub mark_copy: Style,
    pub mark_cut: Style,

    pub picker_border: Style,
    pub picker_prompt: Style,
    pub picker_selected: Style,
    pub picker_badge: Style,
    pub picker_divider: Style,
    pub picker_preview: Style,
}

impl Ui {
    /// Every key a theme file must set, in the spelling it uses.
    ///
    /// `background` and `foreground` are deliberately absent: they are the two
    /// a theme may decline, and `ansi` declines both. Everything else is a
    /// hole on the screen if it is missing, which the compiler cannot see —
    /// so a test walks this list against both built-ins.
    pub const REQUIRED: &'static [&'static str] = &[
        "cursorline",
        "selection",
        "search",
        "cursor_alt",
        "gutter",
        "gutter_current",
        "rule",
        "filler",
        "indent_guide",
        "mode_normal",
        "mode_insert",
        "mode_pick",
        "status",
        "status_muted",
        "status_inactive",
        "statusline",
        "tree_dir",
        "tree_link",
        "mark_copy",
        "mark_cut",
        "picker_border",
        "picker_prompt",
        "picker_selected",
        "picker_badge",
        "picker_divider",
        "picker_preview",
    ];

    /// The single place a `[ui]` key becomes a field, so a key cannot exist
    /// for the parser and not for the drawing — the same argument
    /// `Options::set` makes.
    fn set(&mut self, key: &str, style: Style) -> Result<(), String> {
        match key {
            "background" => self.background = style.fg,
            "foreground" => self.foreground = style.fg,
            "cursorline" => self.cursorline = style,
            "selection" => self.selection = style,
            "search" => self.search = style,
            "cursor_alt" => self.cursor_alt = style,
            "gutter" => self.gutter = style,
            "gutter_current" => self.gutter_current = style,
            "rule" => self.rule = style,
            "filler" => self.filler = style,
            "indent_guide" => self.indent_guide = style,
            "mode_normal" => self.mode_normal = style,
            "mode_insert" => self.mode_insert = style,
            "mode_pick" => self.mode_pick = style,
            "status" => self.status = style,
            "status_muted" => self.status_muted = style,
            "status_inactive" => self.status_inactive = style,
            "statusline" => self.statusline = style,
            "tree_dir" => self.tree_dir = style,
            "tree_link" => self.tree_link = style,
            "mark_copy" => self.mark_copy = style,
            "mark_cut" => self.mark_cut = style,
            "picker_border" => self.picker_border = style,
            "picker_prompt" => self.picker_prompt = style,
            "picker_selected" => self.picker_selected = style,
            "picker_badge" => self.picker_badge = style,
            "picker_divider" => self.picker_divider = style,
            "picker_preview" => self.picker_preview = style,
            _ => return Err(format!("unknown ui colour: {key}")),
        }
        Ok(())
    }
}

/// A palette: colours for parsed code, colours for bi's own furniture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Keyed by capture name. A flat map rather than fields, because the set
    /// of capture names belongs to twenty grammars and grows without asking.
    syntax: BTreeMap<String, Style>,
    pub ui: Ui,
}

/// The default, and the one every unknown name falls back to.
pub const DEFAULT_THEME: &str = "gruvbox-dark";

const GRUVBOX_DARK: &str = include_str!("themes/gruvbox-dark.toml");
const GRUVBOX_LIGHT: &str = include_str!("themes/gruvbox-light.toml");
const PASCAL: &str = include_str!("themes/pascal.toml");
const ANSI: &str = include_str!("themes/ansi.toml");

impl Default for Theme {
    /// The shipped `gruvbox-dark`, parsed.
    ///
    /// Parsed rather than constructed so the file is exercised on every run —
    /// if it stops parsing, nothing starts, and no test has to be the one that
    /// notices. `Config::default()` uses the same trick for `default.toml`.
    fn default() -> Self {
        Theme::parse(GRUVBOX_DARK, Theme::empty()).expect("bi's own gruvbox-dark.toml must parse").0
    }
}

impl Theme {
    /// No colours at all — the base a built-in is parsed over.
    fn empty() -> Self {
        Theme { syntax: BTreeMap::new(), ui: Ui::default() }
    }

    /// The source of a built-in theme, or `None` if there is no such name.
    pub fn builtin(name: &str) -> Option<&'static str> {
        match name {
            DEFAULT_THEME => Some(GRUVBOX_DARK),
            "gruvbox-light" => Some(GRUVBOX_LIGHT),
            "pascal" => Some(PASCAL),
            "ansi" => Some(ANSI),
            _ => None,
        }
    }

    /// The names of every built-in, for `:set theme` to complain with.
    ///
    /// `gruvbox-light` is here and is not the default, because bi has no way
    /// to ask the terminal whether it is light. Shipping it and defaulting to
    /// it are different decisions, and only the first one is available.
    pub const BUILTINS: &'static [&'static str] = &[DEFAULT_THEME, "gruvbox-light", "ansi"];

    /// The style for a capture name, walking down one dotted segment at a
    /// time: `string.special.key` asks for `string.special`, then `string`.
    ///
    /// So `function.method` needs no entry of its own, while a name that must
    /// differ from its own prefix can say so — a JSON key is a
    /// `string.special.key` and should not look like a string value.
    ///
    /// This walk lives in the library rather than the frontend because it is
    /// editor semantics: a GUI needs the identical fallback, and two copies
    /// would drift.
    pub fn style(&self, capture: &str) -> Option<Style> {
        let mut key = capture;
        loop {
            if let Some(style) = self.syntax.get(key) {
                return Some(*style);
            }
            match key.rfind('.') {
                Some(dot) => key = &key[..dot],
                None => return None,
            }
        }
    }

    /// Parses `src` as a patch over `base`.
    ///
    /// A user's `themes/gruvbox-dark.toml` is therefore how you change one
    /// colour of a shipped theme without copying the rest.
    ///
    /// `Err` is the one unsalvageable case: the document is not TOML at all.
    /// Everything else — an unknown key, a colour that will not parse, a value
    /// of the wrong shape — drops that entry, records a [`Diagnostic`], and
    /// carries on. A theme with a typo in it must still leave you an editor
    /// you can use to fix the typo.
    pub fn parse(src: &str, base: Theme) -> Result<(Theme, Vec<Diagnostic>), Diagnostic> {
        let doc: Document<&str> = Document::parse(src).map_err(|e| Diagnostic {
            line: e.span().map_or(1, |span| line_of(src, span.start)),
            message: e.to_string(),
        })?;

        let mut theme = base;
        let mut problems = Vec::new();

        for (key, item) in doc.iter() {
            let line = key_line(&doc, key, src);
            let Some(table) = item.as_table() else {
                problems.push(Diagnostic { line, message: format!("`{key}` is not in a section") });
                continue;
            };

            match key {
                "syntax" => {
                    for (name, item) in table.iter() {
                        let line = key_line(table, name, src);
                        match style_of(item) {
                            Ok(style) => {
                                theme.syntax.insert(name.to_string(), style);
                            }
                            Err(message) => problems.push(Diagnostic { line, message }),
                        }
                    }
                }
                "ui" => {
                    for (name, item) in table.iter() {
                        let line = key_line(table, name, src);
                        match style_of(item).and_then(|style| theme.ui.set(name, style)) {
                            Ok(()) => {}
                            Err(message) => problems.push(Diagnostic { line, message }),
                        }
                    }
                }
                _ => problems.push(Diagnostic { line, message: format!("unknown section: {key}") }),
            }
        }

        Ok((theme, problems))
    }

    /// Resolves `name` into a theme: the frontend's file if there is one, else
    /// a built-in of that name, else the default with a diagnostic.
    ///
    /// `user` is what the frontend read from `<config dir>/themes/<name>.toml`,
    /// and `None` means there was no such file — which is normal, not a
    /// problem. A user file wins over a built-in of the same name so that
    /// adjusting one colour of a shipped theme does not mean forking it.
    pub fn resolve(name: &str, user: Option<&str>) -> (Theme, Vec<Diagnostic>) {
        // A user file is a patch over the built-in it shadows, if it shadows
        // one, and over the default otherwise — so `themes/mine.toml` need not
        // repeat all twenty-five ui keys to be a valid theme.
        let base = match Theme::builtin(name) {
            Some(src) => {
                Theme::parse(src, Theme::empty()).expect("bi's own built-in themes must parse").0
            }
            None if user.is_some() => Theme::default(),
            None => {
                let known = Theme::BUILTINS.join(", ");
                return (
                    Theme::default(),
                    vec![Diagnostic {
                        line: 1,
                        message: format!(
                            "unknown theme `{name}` — using {DEFAULT_THEME}. built in: {known}"
                        ),
                    }],
                );
            }
        };

        let Some(src) = user else { return (base, Vec::new()) };

        match Theme::parse(src, base.clone()) {
            Ok((theme, problems)) => (
                theme,
                problems
                    .into_iter()
                    .map(|d| Diagnostic {
                        line: d.line,
                        message: format!("themes/{name}.toml:{}: {}", d.line, d.message),
                    })
                    .collect(),
            ),
            // Not TOML at all. The built-in of that name still stands, which
            // is better than no colours.
            Err(d) => (
                base,
                vec![Diagnostic {
                    line: d.line,
                    message: format!("themes/{name}.toml:{}: {}", d.line, d.message),
                }],
            ),
        }
    }
}

/// A bare string is shorthand for `{ fg = … }`; the table form adds `bg` and
/// the attributes.
fn style_of(item: &Item) -> Result<Style, String> {
    match item.as_value() {
        Some(Value::String(s)) => Ok(Style::fg(Color::parse(s.value())?)),
        Some(Value::InlineTable(table)) => {
            let mut style = Style::default();
            for (key, value) in table.iter() {
                let flag = |v: &Value| -> Result<bool, String> {
                    v.as_bool().ok_or_else(|| format!("`{key}` takes true or false"))
                };
                let colour = |v: &Value| -> Result<Option<Color>, String> {
                    let text =
                        v.as_str().ok_or_else(|| format!("`{key}` takes a colour, in quotes"))?;
                    Color::parse(text).map(Some)
                };
                match key {
                    "fg" => style.fg = colour(value)?,
                    "bg" => style.bg = colour(value)?,
                    "bold" => style.bold = flag(value)?,
                    "italic" => style.italic = flag(value)?,
                    "underline" => style.underline = flag(value)?,
                    "reverse" => style.reverse = flag(value)?,
                    _ => return Err(format!("unknown style field: {key}")),
                }
            }
            Ok(style)
        }
        _ => Err("a colour is a string, or a table of fg/bg/bold/italic/underline/reverse".into()),
    }
}

/// The line a key sits on. The key kept on the table carries the span that the
/// `&str` from `iter()` has lost — the same walk `config/parse.rs` does.
fn key_line(table: &Table, key: &str, src: &str) -> usize {
    table.get_key_value(key).and_then(|(k, _)| k.span()).map_or(1, |s| line_of(src, s.start))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(name: &str) -> Theme {
        let src = Theme::builtin(name).unwrap_or_else(|| panic!("no built-in {name}"));
        let (theme, problems) = Theme::parse(src, Theme::empty()).expect("must parse");
        assert_eq!(problems, [], "{name} has problems in it");
        theme
    }

    /// The number in the spec is a claim until something checks it. A missing
    /// `[ui]` key is a hole on the screen that the compiler cannot see, so the
    /// list walks itself against every built-in.
    #[test]
    fn every_builtin_sets_every_required_ui_colour() {
        for name in Theme::BUILTINS {
            let src = Theme::builtin(name).unwrap();
            let doc: Document<&str> = Document::parse(src).expect("built-in must parse");
            let ui = doc["ui"].as_table().expect("[ui] section");
            for key in Ui::REQUIRED {
                assert!(ui.contains_key(key), "{name} is missing `[ui] {key}`");
            }
        }
    }

    /// The promise `ansi` exists to keep: choosing a theme is not a one-way
    /// door. If the type cannot say what render.rs used to hardcode, this is
    /// where that shows up — it is how `reverse` came to exist.
    #[test]
    fn ansi_says_what_render_used_to_hardcode() {
        let ansi = parsed("ansi");

        assert_eq!(ansi.style("keyword"), Some(Style::fg(Color::Ansi(Ansi::Magenta))));
        assert_eq!(ansi.style("comment"), Some(Style::fg(Color::Ansi(Ansi::DarkGray))));
        assert_eq!(ansi.ui.cursorline, Style { bg: Some(Color::Indexed(236)), ..Style::default() });
        assert_eq!(ansi.ui.selection, Style { bg: Some(Color::Indexed(239)), ..Style::default() });
        assert_eq!(ansi.ui.search, Style { bg: Some(Color::Indexed(58)), ..Style::default() });
        assert!(ansi.ui.statusline.reverse, "the focused status row was reverse video");

        // And the whole point of the ANSI spelling: it names no background, so
        // the terminal's own shows through exactly as it did.
        assert_eq!(ansi.ui.background, None);
        assert_eq!(ansi.ui.foreground, None);
    }

    #[test]
    fn the_default_is_gruvbox_and_it_claims_the_background() {
        let theme = Theme::default();
        assert_eq!(theme.ui.background, Some(Color::Rgb(0x28, 0x28, 0x28)));
        assert_eq!(theme.ui.foreground, Some(Color::Rgb(0xeb, 0xdb, 0xb2)));
        assert_eq!(theme.style("keyword"), Some(Style::fg(Color::Rgb(0xfb, 0x49, 0x34))));
    }

    /// Painting `&` the colour of the text around it is the failure
    /// tree-sitter.md names. gruvbox.vim links `Operator` to plain foreground;
    /// this theme deliberately does not.
    #[test]
    fn an_operator_is_not_the_foreground_colour() {
        let theme = Theme::default();
        let operator = theme.style("operator").expect("operator is themed").fg;
        assert!(operator.is_some());
        assert_ne!(operator, theme.ui.foreground);
    }

    #[test]
    fn all_three_colour_spellings_parse_and_a_fourth_does_not() {
        assert_eq!(Color::parse("magenta"), Ok(Color::Ansi(Ansi::Magenta)));
        assert_eq!(Color::parse("grey"), Ok(Color::Ansi(Ansi::Gray)));
        assert_eq!(Color::parse("color236"), Ok(Color::Indexed(236)));
        assert_eq!(Color::parse("#fb4934"), Ok(Color::Rgb(0xfb, 0x49, 0x34)));

        assert!(Color::parse("#fb493").is_err());
        assert!(Color::parse("#gggggg").is_err());
        assert!(Color::parse("color256").is_err());
        assert!(Color::parse("puce").is_err());
    }

    #[test]
    fn a_capture_falls_back_one_dotted_segment_at_a_time() {
        let theme = Theme::default();
        assert_eq!(theme.style("function.method"), theme.style("function"));
        assert_eq!(theme.style("keyword.control.conditional"), theme.style("keyword"));
        assert_eq!(theme.style("no.such.capture"), None);

        // The exception the walk exists for: a config key is spelled as a kind
        // of string and must not look like a string value.
        assert_ne!(theme.style("string.special.key"), theme.style("string"));
        assert_eq!(theme.style("string.special"), theme.style("string"));
    }

    #[test]
    fn a_bad_colour_loses_its_key_and_keeps_the_rest() {
        let (theme, problems) =
            Theme::parse("[syntax]\nkeyword = \"puce\"\nstring = \"green\"\n", Theme::empty())
                .expect("valid toml");

        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].line, 2);
        assert!(problems[0].message.contains("puce"), "{:?}", problems[0].message);
        assert_eq!(theme.style("string"), Some(Style::fg(Color::Ansi(Ansi::Green))));
        assert_eq!(theme.style("keyword"), None);
    }

    #[test]
    fn unknown_sections_and_keys_are_reported_not_fatal() {
        let (_, problems) =
            Theme::parse("[nope]\na = 1\n\n[ui]\nnosuch = \"red\"\n", Theme::empty())
                .expect("valid toml");
        let messages: Vec<_> = problems.iter().map(|p| p.message.clone()).collect();
        assert!(messages.iter().any(|m| m.contains("unknown section: nope")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("unknown ui colour: nosuch")), "{messages:?}");
    }

    #[test]
    fn a_theme_file_is_a_patch_over_the_builtin_it_shadows() {
        // One key, and everything else still comes from gruvbox-dark.
        let (theme, problems) =
            Theme::resolve(DEFAULT_THEME, Some("[syntax]\nkeyword = \"#ffffff\"\n"));
        assert_eq!(problems, []);
        assert_eq!(theme.style("keyword"), Some(Style::fg(Color::Rgb(255, 255, 255))));
        assert_eq!(theme.ui.background, Some(Color::Rgb(0x28, 0x28, 0x28)));
    }

    #[test]
    fn a_user_theme_with_no_builtin_patches_the_default() {
        let (theme, problems) =
            Theme::resolve("mine", Some("[ui]\nselection = { bg = \"red\" }\n"));
        assert_eq!(problems, []);
        assert_eq!(theme.ui.selection.bg, Some(Color::Ansi(Ansi::Red)));
        // Everything it did not mention still comes from gruvbox-dark.
        assert_eq!(theme.ui.background, Some(Color::Rgb(0x28, 0x28, 0x28)));
    }

    #[test]
    fn an_unknown_theme_falls_back_to_the_default_and_says_so() {
        let (theme, problems) = Theme::resolve("nosuch", None);
        assert_eq!(theme, Theme::default());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("nosuch"), "{:?}", problems[0].message);
        assert!(problems[0].message.contains(DEFAULT_THEME), "{:?}", problems[0].message);
    }

    /// The light theme is the dark one's roles in the light palette, so the
    /// thing worth pinning is that it is genuinely inverted rather than a copy
    /// with a different background bolted on.
    #[test]
    fn gruvbox_light_is_light_and_still_gruvbox() {
        let light = parsed("gruvbox-light");
        let dark = Theme::default();

        assert_eq!(light.ui.background, Some(Color::Rgb(0xfb, 0xf1, 0xc7)));
        assert_eq!(light.ui.foreground, Some(Color::Rgb(0x3c, 0x38, 0x36)));
        // The dark theme's background is the light theme's foreground, near
        // enough — they are the two ends of one palette.
        assert_eq!(dark.ui.background, Some(Color::Rgb(0x28, 0x28, 0x28)));

        // Every syntax role the dark theme fills, the light one fills too, and
        // with a different colour — a role that came back identical would mean
        // a key was copied rather than translated.
        for role in ["keyword", "function", "type", "string", "operator", "property"] {
            let (l, d) = (light.style(role), dark.style(role));
            assert!(l.is_some(), "gruvbox-light has no {role}");
            assert_ne!(l, d, "{role} was not translated into the light palette");
        }

        // Comments are the one deliberate exception: gruvbox uses the same
        // neutral gray at both ends.
        assert_eq!(light.style("comment"), dark.style("comment"));
    }

    /// A capture with no entry falls off the end of the dotted walk and paints
    /// in plain foreground, which on screen is indistinguishable from a grammar
    /// that never matched. So the roles that *carry* a file have to be filled
    /// by every built-in, `ansi` included.
    ///
    /// `tag` is on this list by experience rather than by principle: it was in
    /// none of the four, so every HTML element name was parsed, captured,
    /// ranked and then thrown away. HTML is mostly not tags and it went
    /// unnoticed; XML is almost entirely tags and would have shipped a grammar
    /// whose files render blank.
    #[test]
    fn every_builtin_fills_the_roles_that_carry_a_file() {
        for name in ["gruvbox-dark", "gruvbox-light", "pascal", "ansi"] {
            let theme = parsed(name);
            for role in ["keyword", "string", "comment", "type", "property", "tag"] {
                assert!(theme.style(role).is_some(), "{name} leaves {role} unpainted");
            }
        }
    }

    /// The fallback walk lands `string.special.symbol` on `string` unless a
    /// theme stops it, and Ruby is dense enough with symbols that letting it
    /// happen turned 59% of a Ruby file into one green. That is the failure
    /// tree-sitter.md names for JSON keys, arriving at the other end of the
    /// pipeline: there the query had to be told two captures differ, here the
    /// theme has to be told two colours do.
    ///
    /// `ansi` is deliberately exempt. It promises the colours bi had before it
    /// had themes, and before it had themes these fell through to `string`.
    #[test]
    fn a_symbol_and_a_regex_are_not_just_strings() {
        for name in [DEFAULT_THEME, "gruvbox-light"] {
            let theme = parsed(name);
            let string = theme.style("string");
            for special in ["string.special.symbol", "string.special.regex"] {
                assert_ne!(
                    theme.style(special),
                    string,
                    "{name}: {special} fell through to the string colour"
                );
            }
            // And they differ from each other — a symbol is a key, a regex is
            // a pattern, and one colour for both is half a fix.
            assert_ne!(
                theme.style("string.special.symbol"),
                theme.style("string.special.regex"),
                "{name}: symbol and regex share a colour"
            );
        }
    }

    /// Sixteen colours is the theme, so the test is that it stayed inside
    /// them. A single off-palette value would be the whole point missed.
    #[test]
    fn pascal_uses_nothing_but_the_ega_palette() {
        // The sixteen, as the hardware produced them — plus the one shade of
        // blue the cursor line needs, which the IDE had no equivalent of.
        const EGA: &[(u8, u8, u8)] = &[
            (0x00, 0x00, 0x00),
            (0x00, 0x00, 0xa8),
            (0x00, 0xaa, 0x00),
            (0x00, 0xaa, 0xaa),
            (0xaa, 0x00, 0x00),
            (0xaa, 0x00, 0xaa),
            (0xaa, 0x55, 0x00),
            (0xaa, 0xaa, 0xaa),
            (0x55, 0x55, 0x55),
            (0x55, 0x55, 0xff),
            (0x55, 0xff, 0x55),
            (0x55, 0xff, 0xff),
            (0xff, 0x55, 0x55),
            (0xff, 0x55, 0xff),
            (0xff, 0xff, 0x55),
            (0xff, 0xff, 0xff),
            (0x00, 0x00, 0x80), // the cursor line, a shade of the same blue
        ];
        let src = Theme::builtin("pascal").expect("pascal is built in");
        for (line, text) in src.lines().enumerate() {
            // Only the values, never the palette table in the header comment.
            let Some(code) = text.split('#').nth(1) else { continue };
            if text.trim_start().starts_with('#') || code.len() < 6 {
                continue;
            }
            let hex = &code[..6];
            let Ok(colour) = Color::parse(&format!("#{hex}")) else { continue };
            let Color::Rgb(r, g, b) = colour else { continue };
            assert!(
                EGA.contains(&(r, g, b)),
                "pascal.toml:{}: #{hex} is not an EGA colour",
                line + 1
            );
        }
    }

    /// The theme is only worth having if it looks like the thing, and the two
    /// halves of that are the blue and the inverted keyword/call pair.
    #[test]
    fn pascal_is_blue_and_keeps_its_keywords_brighter_than_its_calls() {
        let pascal = parsed("pascal");
        assert_eq!(pascal.ui.background, Some(Color::Rgb(0x00, 0x00, 0xa8)));

        // White and bold for `procedure`/`begin`/`end`, yellow for the thing
        // being called — the reverse of every theme written since, and what
        // the screen actually did.
        let keyword = pascal.style("keyword").unwrap();
        assert_eq!(keyword.fg, Some(Color::Rgb(0xff, 0xff, 0xff)));
        assert!(keyword.bold);
        assert_eq!(pascal.style("function").unwrap().fg, Some(Color::Rgb(0xff, 0xff, 0x55)));

        // And the status bar is black on light gray, which is the single most
        // recognisable thing about the IDE.
        assert_eq!(pascal.ui.statusline.fg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(pascal.ui.statusline.bg, Some(Color::Rgb(0xaa, 0xaa, 0xaa)));
    }

    #[test]
    fn a_builtin_with_no_user_file_resolves_to_itself() {
        let (theme, problems) = Theme::resolve("ansi", None);
        assert_eq!(problems, []);
        assert_eq!(theme, parsed("ansi"));
    }

    /// A theme is a config file and gets config's treatment: unparseable is
    /// reported, and leaves you an editor you can fix the typo in.
    #[test]
    fn a_theme_that_is_not_toml_keeps_the_builtin_and_names_the_file() {
        let (theme, problems) = Theme::resolve("ansi", Some("[syntax\nkeyword = \"red\"\n"));
        assert_eq!(theme, parsed("ansi"));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("themes/ansi.toml"), "{:?}", problems[0].message);
    }
}
