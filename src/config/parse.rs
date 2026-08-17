//! TOML into a [`Config`], with a line number on everything that goes wrong.

use toml_edit::{Document, Item, Table, Value};

use super::keys::{self, KeyMode};
use super::{Config, Diagnostic, OptionValue, line_of};

/// Parses `src` as a patch over `base`.
///
/// `Err` is the one unsalvageable case: the document is not TOML, so there is
/// nothing to read a single setting out of. Everything else — an unknown
/// section, an unknown option, a value of the wrong type — drops that item,
/// records a [`Diagnostic`], and carries on. A config file is edited by hand
/// and will be wrong sometimes; refusing to start is the wrong answer.
pub fn parse(src: &str, base: Config) -> Result<(Config, Vec<Diagnostic>), Diagnostic> {
    let doc: Document<&str> = Document::parse(src).map_err(|e| Diagnostic {
        line: e.span().map_or(1, |span| line_of(src, span.start)),
        message: e.to_string(),
    })?;

    let mut config = base;
    let mut problems = Vec::new();

    for (key, item) in doc.iter() {
        // `doc.iter()` yields `&str` keys, which carry no span. The key with
        // its span lives on the table — `doc` derefs to one, so `line_for`
        // reaches it the same way a section's own reader does.
        let line = line_for(&doc, key, src);

        let Some(table) = item.as_table() else {
            problems.push(Diagnostic { line, message: format!("`{key}` is not in a section") });
            continue;
        };

        match key {
            "options" => read_options(src, table, &mut config, &mut problems),
            "keys" => read_keys(src, table, &mut config, &mut problems),
            _ => problems.push(Diagnostic { line, message: format!("unknown section: {key}") }),
        }
    }

    Ok((config, problems))
}

fn read_options(src: &str, table: &Table, config: &mut Config, problems: &mut Vec<Diagnostic>) {
    for (key, item) in table.iter() {
        let line = line_for(table, key, src);

        if let Err(message) = config.options.set(key, option_value(item)) {
            problems.push(Diagnostic { line, message });
        }
    }
}

/// `[keys.normal]`, `[keys.visual]`, `[keys.tree]`.
///
/// `[keys]` itself is one implicit table holding the three, which is how TOML
/// reads a dotted header — so this is one section with sub-tables, not three
/// sections, and a stray `[keys.nope]` is reported against its own line.
fn read_keys(src: &str, table: &Table, config: &mut Config, problems: &mut Vec<Diagnostic>) {
    for (name, item) in table.iter() {
        let line = line_for(table, name, src);
        let Some(mode) = KeyMode::from_section(name) else {
            let message = format!("unknown key mode: {name} — try normal, visual or tree");
            problems.push(Diagnostic { line, message });
            continue;
        };
        let Some(bindings) = item.as_table() else {
            problems.push(Diagnostic { line, message: format!("keys.{name} is not a section") });
            continue;
        };
        read_bindings(src, bindings, mode, config, problems);
    }
}

fn read_bindings(
    src: &str,
    table: &Table,
    mode: KeyMode,
    config: &mut Config,
    problems: &mut Vec<Diagnostic>,
) {
    for (spelling, item) in table.iter() {
        let line = line_for(table, spelling, src);
        let mut report = |message: String| problems.push(Diagnostic { line, message });

        let from = match keys::parse_key(spelling) {
            Ok(key) => key,
            Err(message) => {
                // A multi-key sequence is the common case here, and it is
                // worth naming rather than calling the whole thing invalid.
                report(if spelling.chars().count() > 1 && !spelling.starts_with('<') {
                    format!("{message} — multi-key sequences are not bindable yet")
                } else {
                    message
                });
                continue;
            }
        };

        match item.as_value() {
            // `false` unbinds. `true` is not the opposite of anything: a key
            // is bound to a name, so there is nothing for it to mean.
            Some(Value::Boolean(b)) if !*b.value() => config.keys.insert(mode, from, None),
            Some(Value::Boolean(_)) => report(
                "true is not a binding — name a command, or use \
                                               false to unbind"
                    .into(),
            ),
            Some(Value::String(name)) => {
                let name = name.value();
                match keys::key_for_name(mode, name) {
                    Some(to) => config.keys.insert(mode, from, Some(to)),
                    None => report(match keys::nearest_name(mode, name) {
                        Some(near) => format!("unknown command: {name} — did you mean {near}?"),
                        None => format!("unknown command: {name}"),
                    }),
                }
            }
            _ => report(format!("a binding is a command name or false, not {item}")),
        }
    }
}

/// The line `key` sits on, for a diagnostic.
///
/// A key's span is how a diagnostic learns its line, and every section's
/// reader needs the same lookup — `key.span()` on the table's own copy of
/// the key, not the borrowed `&str` iteration yields, which carries none.
/// One helper here rather than one per reader, because the `[keys.*]` sections
/// that follow will each need this too.
fn line_for(table: &Table, key: &str, src: &str) -> usize {
    table.get_key_value(key).and_then(|(k, _)| k.span()).map_or(1, |s| line_of(src, s.start))
}

/// A TOML scalar as an [`OptionValue`]. Anything else — a string, an array, a
/// nested table — is not something an option can hold, so it becomes
/// [`OptionValue::Other`] and it is left to the option itself, through
/// `Options::set`, to say what it wanted.
fn option_value(item: &Item) -> OptionValue {
    match item.as_value() {
        Some(Value::Integer(n)) => OptionValue::Int(*n.value()),
        Some(Value::Boolean(b)) => OptionValue::Bool(*b.value()),
        _ => OptionValue::Other,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{Config, OptionValue, parse};
    use crate::editor::LineNumbers;

    fn ok(src: &str) -> (Config, Vec<String>) {
        let (config, problems) = parse(src, Config::default()).expect("document parses");
        (config, problems.into_iter().map(|d| format!("{}: {}", d.line, d.message)).collect())
    }

    #[test]
    fn a_user_file_patches_the_defaults() {
        let (config, problems) = ok("[options]\nnumber = 5\n");
        assert_eq!(config.options.number, LineNumbers::Every(5), "overridden");
        assert!(!config.options.hlsearch, "untouched options keep the default");
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        let (config, problems) = ok("");
        assert_eq!(config, Config::default());
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_top_level_key_is_not_a_section() {
        let (config, problems) = ok("theme = \"onedark\"\n");
        assert_eq!(problems, ["1: `theme` is not in a section"]);
        assert_eq!(config, Config::default(), "and nothing was applied");
    }

    #[test]
    fn unknown_sections_are_named_with_their_line() {
        let (_, problems) = ok("[options]\nnumber = 5\n\n[nope]\nx = 1\n");
        assert_eq!(problems, ["4: unknown section: nope"]);
    }

    #[test]
    fn unknown_options_are_named_with_their_line() {
        let (config, problems) = ok("[options]\nnumber = 5\nnmber = 9\n");
        assert_eq!(problems, ["3: unknown option: nmber"]);
        assert_eq!(config.options.number, LineNumbers::Every(5), "the good line still applied");
    }

    #[test]
    fn a_bad_value_reports_and_keeps_the_default() {
        let (config, problems) = ok("[options]\nnumber = \"big\"\n");
        assert_eq!(problems, ["2: number takes 0 (off), -1 (relative) or a count"]);
        assert_eq!(config.options.number, LineNumbers::Every(1));
    }

    #[test]
    fn a_value_of_false_is_not_yet_an_unbinding() {
        // `false` unbinds a *key* in step 3. On an option it is just a bool,
        // and `number = false` is a type error rather than a removal.
        let (_, problems) = ok("[options]\nnumber = false\n");
        assert_eq!(problems, ["2: number takes 0 (off), -1 (relative) or a count"]);
    }

    #[test]
    fn malformed_toml_is_the_one_fatal_case() {
        let err = parse("[options\nnumber = 5\n", Config::default())
            .expect_err("an unterminated table header cannot be salvaged");
        assert_eq!(err.line, 1);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn set_and_get_reach_the_same_option() {
        let (config, _) = ok("[options]\nhlsearch = true\n");
        assert_eq!(config.options.get("hlsearch"), Some(OptionValue::Bool(true)));
    }
}
