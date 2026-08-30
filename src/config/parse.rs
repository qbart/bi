//! TOML into a [`Config`], with a line number on everything that goes wrong.

use toml_edit::{Document, Item, Table, Value};

use super::keys::{self, Bind, KeyMode, Lookup};
use super::{Config, Diagnostic, OptionValue, line_of};
use crate::key::Key;

/// Parses `src` as a patch over `base`.
///
/// `Err` is the one unsalvageable case: the document is not TOML, so there is
/// nothing to read a single setting out of. Everything else — an unknown
/// section, an unknown option, a value of the wrong type — drops that item,
/// records a [`Diagnostic`], and carries on. A config file is edited by hand
/// and will be wrong sometimes; refusing to start is the wrong answer.
pub fn parse(src: &str, base: Config) -> Result<(Config, Vec<Diagnostic>), Diagnostic> {
    parse_with(src, base, false)
}

/// Parses a **project's** `.bi.toml` as a patch over `base` — the same reader
/// with two refusals switched on: `[keys]`, and a server's `command`. A
/// repository that could name the binary bi spawns on open, or the ex line a
/// key runs, would be arbitrary code execution by `git clone`. Each refusal
/// is a diagnostic with the offending line, never a silence.
/// See `docs/specs/local-config.md`.
pub fn parse_local(src: &str, base: Config) -> Result<(Config, Vec<Diagnostic>), Diagnostic> {
    parse_with(src, base, true)
}

fn parse_with(
    src: &str,
    base: Config,
    local: bool,
) -> Result<(Config, Vec<Diagnostic>), Diagnostic> {
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

        // The one top-level key that is a value rather than a section: a
        // detection list is one ordered fact, not a table of settings.
        if key == "fileencodings" {
            read_fileencodings(item, line, &mut config, &mut problems);
            continue;
        }

        let Some(table) = item.as_table() else {
            problems.push(Diagnostic { line, message: format!("`{key}` is not in a section") });
            continue;
        };

        match key {
            "options" => read_options(src, table, &mut config, &mut problems),
            "keys" if local => problems.push(Diagnostic {
                line,
                message: "keys are not read from a project config".into(),
            }),
            "keys" => read_keys(src, table, &mut config, &mut problems),
            "filetype" => read_filetypes(src, table, &mut config, &mut problems),
            "alternate" => read_alternates(src, table, &mut config, &mut problems),
            "lsp" => read_lsp(src, table, &mut config, &mut problems, local),
            "fmt" => read_fmt(src, table, &mut config, &mut problems, local),
            _ => problems.push(Diagnostic { line, message: format!("unknown section: {key}") }),
        }
    }

    Ok((config, problems))
}

/// `fileencodings = ["utf-8", "cp1250"]` — the list an open tries in order.
///
/// Labels are checked here, against the same table `:set fileencoding` uses,
/// so a typo is a diagnostic with a line number rather than a list that
/// silently never matches. A bad entry drops that one entry, the file's
/// general rule; a list with nothing good left keeps the default rather than
/// leaving opens with no way to succeed.
fn read_fileencodings(
    item: &toml_edit::Item,
    line: usize,
    config: &mut Config,
    problems: &mut Vec<Diagnostic>,
) {
    let Some(array) = item.as_array() else {
        problems.push(Diagnostic {
            line,
            message: "fileencodings takes a list of encoding names".into(),
        });
        return;
    };
    let mut labels = Vec::new();
    for value in array.iter() {
        let Some(label) = value.as_str() else {
            problems.push(Diagnostic {
                line,
                message: "fileencodings entries are encoding names, in quotes".into(),
            });
            continue;
        };
        match crate::encoding::lookup(label) {
            Some(_) => labels.push(label.to_string()),
            None => {
                problems.push(Diagnostic { line, message: format!("unknown encoding: {label}") })
            }
        }
    }
    if !labels.is_empty() {
        config.fileencodings = labels;
    }
}

fn read_options(src: &str, table: &Table, config: &mut Config, problems: &mut Vec<Diagnostic>) {
    for (name, value) in flatten(src, table, "") {
        if let Err(message) = config.options.set(&name, value.1) {
            problems.push(Diagnostic { line: value.0, message });
        }
    }
}

/// A table as flat `name` / `(line, value)` pairs, with dotted names for what
/// was nested.
///
/// No option has a dotted name any more — the trimming five are `trim_trailing`
/// and friends — so nothing here is *meant* to nest, and the flattening is what
/// turns a nested spelling into a diagnostic instead of a silence. Write
/// `[options.trim] trailing = false` and it arrives as `trim.trailing`, which
/// `Options::set` does not know and says so, naming the thing you wrote.
fn flatten(src: &str, table: &Table, prefix: &str) -> Vec<(String, (usize, OptionValue))> {
    let mut out = Vec::new();
    for (key, item) in table.iter() {
        let line = line_for(table, key, src);
        let name = match prefix.is_empty() {
            true => key.to_string(),
            false => format!("{prefix}.{key}"),
        };
        match item {
            Item::Table(inner) => out.extend(flatten(src, inner, &name)),
            Item::Value(Value::InlineTable(inner)) => {
                // An inline table is the same thing said on one line. It has no
                // `Table` to walk, so its entries are read here.
                for (key, value) in inner.iter() {
                    out.push((
                        format!("{name}.{key}"),
                        (line, option_value(&Item::Value(value.clone()))),
                    ));
                }
            }
            _ => out.push((name, (line, option_value(item)))),
        }
    }
    out
}

/// `[filetype.<name>]` — one table per kind of file, each a patch over
/// `[options]`.
///
/// A value is checked here even though it is applied later: a patch carries
/// no line numbers, and `config.toml:7: expandtab takes true or false` is the
/// whole difference between a diagnostic and a shrug. So each value is tried
/// against a scratch `Options` — the same `Options::set` `[options]` goes
/// through — and one that will never apply is reported and dropped rather
/// than kept to fail silently at every resolution.
fn read_filetypes(src: &str, table: &Table, config: &mut Config, problems: &mut Vec<Diagnostic>) {
    for (name, item) in table.iter() {
        let line = line_for(table, name, src);
        let Some(inner) = item.as_table() else {
            problems
                .push(Diagnostic { line, message: format!("[filetype.{name}] is not a section") });
            continue;
        };
        let patch = config.filetypes.entry(name.to_string()).or_default();
        // Through the same flatten as `[options]`, so a key means what it
        // means there and a nested one is the same diagnostic in both.
        for (key, (line, value)) in flatten(src, inner, "") {
            if let Err(message) = super::Options::default().set(&key, value.clone()) {
                problems.push(Diagnostic { line, message });
                continue;
            }
            patch.set(key, value);
        }
    }
}

/// `[alternate]` — one pattern per key, and the paths to try for it.
///
/// A rule the file already has is *replaced* rather than added beside, so a
/// user who disagrees with bi about `*.go` says so once; anything else is
/// appended, keeping the order it was written in — which is the rule, since
/// the first pattern that matches decides.
fn read_alternates(src: &str, table: &Table, config: &mut Config, problems: &mut Vec<Diagnostic>) {
    for (pattern, item) in table.iter() {
        let line = line_for(table, pattern, src);
        let Some(array) = item.as_value().and_then(Value::as_array) else {
            problems.push(Diagnostic {
                line,
                message: format!("{pattern} takes a list of paths, like [\"*.go\"]"),
            });
            continue;
        };
        let targets: Vec<String> =
            array.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        if targets.len() != array.len() {
            problems.push(Diagnostic {
                line,
                message: format!("{pattern} takes a list of paths, like [\"*.go\"]"),
            });
            continue;
        }
        match config.alternates.iter_mut().find(|(key, _)| key == pattern) {
            Some(rule) => rule.1 = targets,
            None => config.alternates.push((pattern.to_string(), targets)),
        }
    }
}

/// `[lsp]`: `enabled`, and `[lsp.servers.<name>]` sections. See
/// `docs/specs/lsp.md`.
fn read_lsp(
    src: &str,
    table: &Table,
    config: &mut Config,
    problems: &mut Vec<Diagnostic>,
    local: bool,
) {
    for (key, item) in table.iter() {
        let line = line_for(table, key, src);
        match key {
            "enabled" => match item.as_value().and_then(Value::as_bool) {
                Some(b) => config.lsp.enabled = b,
                None => {
                    problems.push(Diagnostic { line, message: "enabled is true or false".into() })
                }
            },
            "servers" => match item.as_table() {
                Some(servers) => read_servers(src, servers, config, problems, local),
                None => problems.push(Diagnostic {
                    line,
                    message: "servers holds [lsp.servers.<name>] sections".into(),
                }),
            },
            other => {
                problems.push(Diagnostic { line, message: format!("unknown lsp setting: {other}") })
            }
        }
    }
}

/// One `[lsp.servers.<name>]` merges **field-wise** over the built-in server
/// of the same name — the same patch promise the rest of the file makes, so
/// overriding `command` alone keeps the default filetypes and roots.
fn read_servers(
    src: &str,
    table: &Table,
    config: &mut Config,
    problems: &mut Vec<Diagnostic>,
    local: bool,
) {
    for (name, item) in table.iter() {
        let line = line_for(table, name, src);
        let Some(server) = item.as_table() else {
            problems.push(Diagnostic {
                line,
                message: format!("{name} is a section: [lsp.servers.{name}]"),
            });
            continue;
        };
        let entry = config.lsp.servers.entry(name.to_string()).or_default();
        for (field, item) in server.iter() {
            let line = line_for(server, field, src);
            match field {
                "enabled" => match item.as_value().and_then(Value::as_bool) {
                    Some(b) => entry.enabled = b,
                    None => problems
                        .push(Diagnostic { line, message: "enabled is true or false".into() }),
                },
                // The refusal that makes a project config safe to read at
                // all: a repository does not get to name the binary bi runs.
                "command" if local => problems.push(Diagnostic {
                    line,
                    message: "command is not read from a project config".into(),
                }),
                "command" | "filetypes" | "roots" => match string_list(item) {
                    Some(list) => match field {
                        "command" => entry.command = list,
                        "filetypes" => entry.filetypes = list,
                        _ => entry.roots = list,
                    },
                    None => problems.push(Diagnostic {
                        line,
                        message: format!("{field} takes a list of strings"),
                    }),
                },
                other => problems
                    .push(Diagnostic { line, message: format!("unknown server setting: {other}") }),
            }
        }
    }
}

/// `[fmt]`: the `[fmt.tools.<name>]` sections. See `docs/specs/fmt.md`.
fn read_fmt(
    src: &str,
    table: &Table,
    config: &mut Config,
    problems: &mut Vec<Diagnostic>,
    local: bool,
) {
    for (key, item) in table.iter() {
        let line = line_for(table, key, src);
        match key {
            "tools" => match item.as_table() {
                Some(tools) => read_tools(src, tools, config, problems, local),
                None => problems.push(Diagnostic {
                    line,
                    message: "tools holds [fmt.tools.<name>] sections".into(),
                }),
            },
            other => {
                problems.push(Diagnostic { line, message: format!("unknown fmt setting: {other}") })
            }
        }
    }
}

/// One `[fmt.tools.<name>]` merges **field-wise** over the built-in tool of
/// the same name, with the server table's refusal: a project config does not
/// get to name the binary bi runs.
fn read_tools(
    src: &str,
    table: &Table,
    config: &mut Config,
    problems: &mut Vec<Diagnostic>,
    local: bool,
) {
    for (name, item) in table.iter() {
        let line = line_for(table, name, src);
        let Some(tool) = item.as_table() else {
            problems.push(Diagnostic {
                line,
                message: format!("{name} is a section: [fmt.tools.{name}]"),
            });
            continue;
        };
        let entry = config.fmt.tools.entry(name.to_string()).or_default();
        for (field, item) in tool.iter() {
            let line = line_for(tool, field, src);
            match field {
                "enabled" => match item.as_value().and_then(Value::as_bool) {
                    Some(b) => entry.enabled = b,
                    None => problems
                        .push(Diagnostic { line, message: "enabled is true or false".into() }),
                },
                "command" if local => problems.push(Diagnostic {
                    line,
                    message: "command is not read from a project config".into(),
                }),
                "command" | "filetypes" => match string_list(item) {
                    Some(list) => match field {
                        "command" => entry.command = list,
                        _ => entry.filetypes = list,
                    },
                    None => problems.push(Diagnostic {
                        line,
                        message: format!("{field} takes a list of strings"),
                    }),
                },
                other => problems
                    .push(Diagnostic { line, message: format!("unknown tool setting: {other}") }),
            }
        }
    }
}

/// An array of strings, whole — one non-string poisons the lot, because half
/// an argv silently applied is worse than none.
fn string_list(item: &Item) -> Option<Vec<String>> {
    let array = item.as_value().and_then(Value::as_array)?;
    let strings: Vec<String> =
        array.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    (strings.len() == array.len()).then_some(strings)
}

/// `leader`, `[keys.normal]`, `[keys.visual]`, `[keys.tree]`.
///
/// `[keys]` itself is one implicit table holding the three modes and the
/// leader, which is how TOML reads a dotted header — so this is one section
/// with sub-tables, not three sections, and a stray `[keys.nope]` is reported
/// against its own line.
///
/// `leader` is read in a pass of its own, before any binding, because a
/// binding may spell `<leader>` and TOML is free to hand `[keys.normal]` back
/// first. A leader that depended on where in the file it sat would be a trap.
fn read_keys(src: &str, table: &Table, config: &mut Config, problems: &mut Vec<Diagnostic>) {
    if let Some((_, item)) = table.iter().find(|(name, _)| *name == "leader") {
        let line = line_for(table, "leader", src);
        match item.as_value().and_then(Value::as_str) {
            Some(spelling) => match keys::parse_key(spelling) {
                Ok(key) => config.keys.set_leader(key),
                Err(message) => problems.push(Diagnostic { line, message }),
            },
            None => problems.push(Diagnostic {
                line,
                message: "leader takes one key, like \" \" or \"<C-Space>\"".into(),
            }),
        }
    }

    for (name, item) in table.iter() {
        if name == "leader" {
            continue;
        }
        let line = line_for(table, name, src);
        let Some(mode) = KeyMode::from_section(name) else {
            let message = format!("unknown key mode: {name} — try normal, visual, tree or leader");
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
    // Every binding this file adds to this mode, so the unreachability check
    // below can name the line the shorter one sits on.
    let mut added: Vec<(Vec<Key>, usize)> = Vec::new();

    for (spelling, item) in table.iter() {
        let line = line_for(table, spelling, src);
        let mut report = |message: String| problems.push(Diagnostic { line, message });

        let from = match keys::parse_keys(spelling, config.keys.leader()) {
            Ok(keys) => keys,
            Err(message) => {
                report(message);
                continue;
            }
        };

        match item.as_value() {
            // `false` unbinds. `true` is not the opposite of anything: a key
            // is bound to a name, so there is nothing for it to mean.
            Some(Value::Boolean(b)) if !*b.value() => {
                config.keys.insert(mode, from.clone(), None);
                added.push((from, line));
            }
            Some(Value::Boolean(_)) => report(
                "true is not a binding — name a command, or use \
                                               false to unbind"
                    .into(),
            ),
            // A value starting with `:` is a command line to run, not a name.
            // Everything `:` can do is bindable, which is most of what a leader
            // is for and none of which a name could reach.
            Some(Value::String(value)) if value.value().starts_with(':') => {
                let bind = ex_line(value.value());
                if matches!(&bind, Bind::Ex { line, run: true } if line.is_empty()) {
                    report("bind a command after the `:`".into());
                } else {
                    config.keys.insert(mode, from.clone(), Some(bind));
                    added.push((from, line));
                }
            }
            Some(Value::String(name)) => {
                let name = name.value();
                match keys::key_for_name(mode, name) {
                    Some(to) => {
                        config.keys.insert(mode, from.clone(), Some(Bind::Keys(to)));
                        added.push((from, line));
                    }
                    None => report(match keys::nearest_name(mode, name) {
                        Some(near) => format!("unknown command: {name} — did you mean {near}?"),
                        None => format!("unknown command: {name}"),
                    }),
                }
            }
            _ => report(format!("a binding is a command name or false, not {item}")),
        }
    }

    report_shadowed(&added, mode, config, problems);
    report_unreachable(&added, mode, config, problems);
}

/// The `:` line a binding carries, and whether it runs.
///
/// The leading `:` marks the value as a command line rather than a name, and
/// is not part of it. The trailing `<CR>` is what says *run it*: without one
/// the line is prefilled and left on the command line, which is how a binding
/// asks for an argument — `":e "` puts you on a `:e ` line with the path still
/// to type, exactly as the tree's `a` and `r` keys already work.
///
/// Only the executed form is trimmed. A prefill's trailing space is the whole
/// point of writing one.
fn ex_line(value: &str) -> Bind {
    let line = value.strip_prefix(':').unwrap_or(value);
    match line.trim_end().strip_suffix("<CR>").or_else(|| line.trim_end().strip_suffix("<Enter>")) {
        Some(line) => Bind::Ex { line: line.trim().to_string(), run: true },
        None => Bind::Ex { line: line.to_string(), run: false },
    }
}

/// Says so when a binding takes over a key bi uses to *start* a command.
///
/// `"gd" = …` makes `g` the user's prefix, and a prefix has no meaning of its
/// own — so `gg`, `ge`, `gE` and `g_` stop resolving. The binding still
/// applies; this only refuses to let it happen quietly.
///
/// Run once over the whole section rather than per line, because what is lost
/// is not knowable until every binding is in: a file that takes `g` over and
/// then binds `gg`, `ge`, `gE` and `g_` back has lost nothing, and the listing
/// `bi config init` writes is exactly that file. One report per prefix, too —
/// four bindings on `g` are one fact, not four.
fn report_shadowed(
    added: &[(Vec<Key>, usize)],
    mode: KeyMode,
    config: &Config,
    problems: &mut Vec<Diagnostic>,
) {
    let mut reported: Vec<Key> = Vec::new();
    for (from, line) in added {
        let Some(&first) = from.first() else { continue };
        if reported.contains(&first) {
            continue;
        }
        reported.push(first);

        let lost: Vec<&str> = keys::shadowed(mode, from)
            .into_iter()
            .filter(|name| {
                // Bound by this file to something is not lost, whatever it was
                // bound to: the user has said what those keys mean now.
                let keys = keys::key_for_name(mode, name).unwrap_or_default();
                !matches!(config.keys.lookup(mode, &keys), Lookup::Bound(_) | Lookup::Unbound)
            })
            .collect();
        if lost.is_empty() {
            continue;
        }

        // Eleven window names would bury the point rather than making it.
        let listed = lost.iter().take(4).copied().collect::<Vec<_>>().join(", ");
        let rest =
            if lost.len() > 4 { format!(" and {} more", lost.len() - 4) } else { String::new() };
        let (binding, prefix) = (keys::spell(from), keys::spell(&from[..1]));
        problems.push(Diagnostic {
            line: *line,
            message: format!(
                "{binding:?} takes over {prefix:?}, so {listed}{rest} can no longer be typed — \
                 bind them by name to keep them"
            ),
        });
    }
}

/// A binding whose own prefix is already a binding can never fire: the shorter
/// one completes and resolves, with no timer to wait and see. Reported rather
/// than silently dropped, which is the whole reason `docs/specs/config.md`
/// refuses `timeoutlen`.
///
/// Checked against the merged keymap, not just this file, so a user sequence
/// starting on top of a shipped binding is caught too.
fn report_unreachable(
    added: &[(Vec<Key>, usize)],
    mode: KeyMode,
    config: &Config,
    problems: &mut Vec<Diagnostic>,
) {
    for (seq, line) in added {
        for len in 1..seq.len() {
            let shorter = &seq[..len];
            if matches!(config.keys.lookup(mode, shorter), Lookup::Bound(_) | Lookup::Unbound) {
                let (long, short) = (keys::spell(seq), keys::spell(shorter));
                let at = added
                    .iter()
                    .find(|(other, _)| other == shorter)
                    .map(|(_, line)| format!(" on line {line}"))
                    .unwrap_or_default();
                let message = format!("{long:?} is unreachable — {short:?}{at} already fires");
                problems.push(Diagnostic { line: *line, message });
                break;
            }
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

/// A TOML scalar as an [`OptionValue`]. Anything else — an array, a
/// nested table — is not something an option can hold, so it becomes
/// [`OptionValue::Other`] and it is left to the option itself, through
/// `Options::set`, to say what it wanted.
fn option_value(item: &Item) -> OptionValue {
    match item.as_value() {
        Some(Value::Integer(n)) => OptionValue::Int(*n.value()),
        Some(Value::Boolean(b)) => OptionValue::Bool(*b.value()),
        Some(Value::String(s)) => OptionValue::Str(s.value().clone()),
        _ => OptionValue::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::{Bind, KeyMode, Lookup};
    use crate::config::{Config, OptionValue, parse};
    use crate::editor::LineNumbers;

    fn ok(src: &str) -> (Config, Vec<String>) {
        let (config, problems) = parse(src, Config::default()).expect("document parses");
        (config, problems.into_iter().map(|d| format!("{}: {}", d.line, d.message)).collect())
    }

    #[test]
    fn fileencodings_is_a_top_level_list_and_ships_a_default() {
        let (config, problems) = ok("");
        assert!(problems.is_empty());
        assert_eq!(config.fileencodings, ["utf-8", "latin1"], "the shipped default");

        let (config, problems) = ok("fileencodings = [\"utf-8\", \"cp1250\"]\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config.fileencodings, ["utf-8", "cp1250"]);
    }

    #[test]
    fn a_bad_fileencodings_entry_is_a_line_numbered_message_not_a_dead_list() {
        let (config, problems) = ok("fileencodings = [\"utf-8\", \"nope\"]\n");
        assert_eq!(problems, ["1: unknown encoding: nope"]);
        assert_eq!(config.fileencodings, ["utf-8"], "the good entry still applies");

        let (config, problems) = ok("fileencodings = [\"nope\"]\n");
        assert_eq!(problems.len(), 1);
        assert_eq!(config.fileencodings, ["utf-8", "latin1"], "nothing good keeps the default");

        let (_, problems) = ok("fileencodings = \"utf-8\"\n");
        assert_eq!(problems, ["1: fileencodings takes a list of encoding names"]);
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

    /// No option has a dotted name, so a nested section is somebody writing
    /// the old spelling of the trimming five. It has to say so: flattening it
    /// into a name `Options::set` rejects is what turns a setting that
    /// silently does nothing into a line number and the text you wrote.
    #[test]
    fn a_nested_option_is_an_unknown_one_and_names_what_was_written() {
        let (config, problems) = ok("[options.trim]\ntrailing = false\n");
        assert_eq!(problems, ["2: unknown option: trim.trailing"]);
        assert!(config.options.trim.trailing, "and nothing was applied");

        let (_, problems) = ok("[options]\ntrim = { trailing = false }\n");
        assert_eq!(problems, ["2: unknown option: trim.trailing"], "the inline shape too");
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
    fn a_sequence_binds_on_both_sides() {
        let (config, problems) = ok("[keys.normal]\n\"<leader>t\" = \"goto_first_line\"\n");
        assert!(problems.is_empty(), "{problems:?}");

        let space = crate::key::Key::char(' ');
        let seq = [space, crate::key::Key::char('t')];
        assert_eq!(
            config.keys.lookup(KeyMode::Normal, &seq),
            Lookup::Bound(Bind::Keys(vec![crate::key::Key::char('g'), crate::key::Key::char('g')]))
        );
        assert_eq!(config.keys.lookup(KeyMode::Normal, &[space]), Lookup::Prefix);
    }

    /// `leader` is read in a pass of its own, so a binding that spells it does
    /// not depend on sitting after it in the file. TOML allows a super-table
    /// to be defined after its children, and a user will do exactly that.
    /// The feature this exists for: everything `:` can do becomes bindable,
    /// none of which a name could reach.
    #[test]
    fn a_binding_can_be_an_ex_line() {
        let (config, problems) = ok("[keys.normal]\n\"<leader>d\" = \":bd<CR>\"\n");
        assert!(problems.is_empty(), "{problems:?}");

        let seq = [crate::key::Key::char(' '), crate::key::Key::char('d')];
        assert_eq!(
            config.keys.lookup(KeyMode::Normal, &seq),
            Lookup::Bound(Bind::Ex { line: "bd".into(), run: true })
        );
    }

    /// The `<CR>` is what says *run it*. Without one the line is prefilled and
    /// left for you to finish, which is how a binding asks for an argument —
    /// and why the trailing space of `":e "` survives when a run's does not.
    #[test]
    fn no_cr_prefills_the_line_instead_of_running_it() {
        let (config, problems) =
            ok("[keys.normal]\n\"<leader>e\" = \":e \"\n\"<leader>w\" = \":w <CR>\"\n");
        assert!(problems.is_empty(), "{problems:?}");

        let seq = |c| [crate::key::Key::char(' '), crate::key::Key::char(c)];
        assert_eq!(
            config.keys.lookup(KeyMode::Normal, &seq('e')),
            Lookup::Bound(Bind::Ex { line: "e ".into(), run: false }),
            "the space is the point of writing one"
        );
        assert_eq!(
            config.keys.lookup(KeyMode::Normal, &seq('w')),
            Lookup::Bound(Bind::Ex { line: "w".into(), run: true })
        );

        // A bare `:` opens the command line, which is a thing to want. A bare
        // `:<CR>` runs nothing and says so.
        assert!(ok("[keys.normal]\n\"<leader>;\" = \":\"\n").1.is_empty());
        assert_eq!(
            ok("[keys.normal]\n\"<leader>x\" = \":<CR>\"\n").1,
            ["2: bind a command after the `:`"]
        );
    }

    #[test]
    fn leader_is_read_before_the_bindings_that_spell_it() {
        let (config, problems) =
            ok("[keys.normal]\n\"<leader>t\" = \"goto_first_line\"\n\n[keys]\nleader = \"\\\\\"\n");
        assert!(problems.is_empty(), "{problems:?}");

        let seq = [crate::key::Key::char('\\'), crate::key::Key::char('t')];
        assert!(matches!(config.keys.lookup(KeyMode::Normal, &seq), Lookup::Bound(_)));
    }

    #[test]
    fn leader_takes_one_key_and_says_so_when_it_does_not() {
        assert_eq!(
            ok("[keys]\nleader = 5\n").1,
            ["2: leader takes one key, like \" \" or \"<C-Space>\""]
        );
        assert_eq!(ok("[keys]\nleader = \"gg\"\n").1, ["2: not a key: gg"]);
    }

    /// The message that sent a user looking for this feature in the first
    /// place: `leader` under `[keys]` used to read as a mode name.
    #[test]
    fn leader_is_not_mistaken_for_a_mode() {
        assert!(ok("[keys]\nleader = \" \"\n").1.is_empty());
        assert_eq!(
            ok("[keys.nope]\n\"x\" = \"left\"\n").1,
            ["1: unknown key mode: nope — try normal, visual, tree or leader"]
        );
    }

    /// The no-timeout rule's other half: the shorter binding fires, so the
    /// longer one can never happen, and the loader says which line already
    /// claimed it.
    #[test]
    fn a_binding_whose_prefix_is_bound_is_unreachable() {
        let (_, problems) =
            ok("[keys.normal]\n\"<leader>\" = \"left\"\n\"<leader>e\" = \"undo\"\n");
        assert_eq!(
            problems,
            ["3: \"<Space>e\" is unreachable — \"<Space>\" on line 2 already fires"]
        );
    }

    /// Taking over a key bi uses to start a command is allowed and reported:
    /// `g` becomes the user's prefix, and the built-in `g` sequences stop
    /// resolving.
    #[test]
    fn taking_over_a_built_in_prefix_is_reported() {
        let (_, problems) = ok("[keys.normal]\n\"gd\" = \"left\"\n");
        assert_eq!(problems.len(), 1, "{problems:?}");
        let message = &problems[0];
        assert!(message.contains("\"gd\" takes over \"g\""), "{message}");
        assert!(message.contains("word_end_backward"), "{message}");

        // A binding that starts somewhere harmless says nothing.
        assert!(ok("[keys.normal]\n\"<leader>d\" = \"left\"\n").1.is_empty());
        // Nor does binding one of the built-in sequences itself, beyond the
        // siblings it really does shadow.
        let (_, problems) = ok("[keys.tree]\n\"dd\" = \"tree_cut\"\n");
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn set_and_get_reach_the_same_option() {
        let (config, _) = ok("[options]\nhlsearch = true\n");
        assert_eq!(config.options.get("hlsearch"), Some(OptionValue::Bool(true)));
    }

    #[test]
    fn the_shipped_defaults_carry_a_server_table() {
        let config = Config::default();
        assert!(config.lsp.enabled);
        let ra = &config.lsp.servers["rust-analyzer"];
        assert_eq!(ra.command, ["rust-analyzer"]);
        assert_eq!(ra.filetypes, ["rust"]);
        assert_eq!(ra.roots, ["Cargo.toml"]);
        assert!(config.lsp.servers.contains_key("gopls"));
    }

    /// The promise the section header makes: overriding one field keeps the
    /// rest of the built-in server it patches.
    #[test]
    fn a_server_override_merges_field_wise_over_the_default() {
        let (config, problems) =
            ok("[lsp.servers.rust-analyzer]\ncommand = [\"ra-nightly\", \"--log\"]\n");
        assert!(problems.is_empty(), "{problems:?}");
        let ra = &config.lsp.servers["rust-analyzer"];
        assert_eq!(ra.command, ["ra-nightly", "--log"]);
        assert_eq!(ra.filetypes, ["rust"], "kept from the default");
        assert_eq!(ra.roots, ["Cargo.toml"], "kept from the default");
    }

    #[test]
    fn a_new_server_is_defined_whole() {
        let src = "[lsp.servers.zls]\ncommand = [\"zls\"]\nfiletypes = [\"zig\"]\n\
                   roots = [\"build.zig\"]\n";
        let (config, problems) = ok(src);
        assert!(problems.is_empty(), "{problems:?}");
        let zls = &config.lsp.servers["zls"];
        assert_eq!(zls.command, ["zls"]);
        assert!(zls.enabled, "enabled unless said otherwise");
    }

    #[test]
    fn lsp_switches_off_wholesale_or_per_server() {
        let (config, _) = ok("[lsp]\nenabled = false\n");
        assert!(!config.lsp.enabled);

        let (config, _) = ok("[lsp.servers.gopls]\nenabled = false\n");
        assert!(!config.lsp.servers["gopls"].enabled);
        assert_eq!(config.lsp.servers["gopls"].command, ["gopls"], "definition kept, just off");
        assert!(config.lsp.enabled, "the master switch is untouched");
    }

    fn ok_local(src: &str) -> (Config, Vec<String>) {
        let (config, problems) =
            super::parse_local(src, Config::default()).expect("document parses");
        (config, problems.into_iter().map(|d| format!("{}: {}", d.line, d.message)).collect())
    }

    #[test]
    fn a_local_config_patches_like_the_main_one() {
        let (config, problems) = ok_local("[options]\ntab_width = 8\n\n[lsp]\nenabled = false\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config.options.tab_width, 8);
        assert!(!config.lsp.enabled);
        assert_eq!(config.options.number, LineNumbers::Every(1), "unmentioned keeps the default");
    }

    /// The refusals carry the offending line, never a silence — the whole
    /// point of threading them through the parser instead of sanitising
    /// afterwards.
    #[test]
    fn a_local_config_is_refused_the_dangerous_keys_by_line() {
        let (config, problems) = ok_local("[keys.normal]\n\"j\" = \"left\"\n");
        assert_eq!(problems, ["1: keys are not read from a project config"]);
        assert!(config.keys.is_empty());

        let src = "[lsp.servers.gopls]\nroots = [\"go.mod\"]\ncommand = [\"evil\"]\n";
        let (config, problems) = ok_local(src);
        assert_eq!(problems, ["3: command is not read from a project config"]);
        assert_eq!(config.lsp.servers["gopls"].command, ["gopls"], "the built-in survives");
        assert_eq!(config.lsp.servers["gopls"].roots, ["go.mod"], "the harmless field is read");
    }

    #[test]
    fn the_main_config_still_reads_what_a_local_one_may_not() {
        let (config, problems) = ok("[lsp.servers.gopls]\ncommand = [\"gopls\", \"-remote\"]\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config.lsp.servers["gopls"].command, ["gopls", "-remote"]);
    }

    // ---- [fmt.tools.<name>] — see docs/specs/fmt.md --------------------

    #[test]
    fn the_shipped_defaults_carry_a_formatter_table_and_a_c3_server() {
        let config = Config::default();
        let c3fmt = &config.fmt.tools["c3fmt"];
        assert_eq!(c3fmt.command, ["c3fmt", "--stdin", "--stdout"]);
        assert_eq!(c3fmt.filetypes, ["c3"]);
        assert!(c3fmt.enabled);

        let c3lsp = &config.lsp.servers["c3-lsp"];
        assert_eq!(c3lsp.command, ["c3-lsp"]);
        assert_eq!(c3lsp.filetypes, ["c3"]);
        assert_eq!(c3lsp.roots, ["project.json"]);

        let clang = &config.fmt.tools["clang-format"];
        assert_eq!(clang.command, ["clang-format", "--style=file", "--fallback-style=LLVM"]);
        assert_eq!(clang.filetypes, ["c", "cpp"]);
        assert!(clang.enabled);
    }

    /// The promise `[fmt.tools.<name>]` shares with the server table:
    /// overriding one field keeps the rest of the built-in it patches.
    #[test]
    fn a_formatter_override_merges_field_wise_over_the_default() {
        let (config, problems) = ok("[fmt.tools.c3fmt]\ncommand = [\"c3fmt\", \"--default\"]\n");
        assert!(problems.is_empty(), "{problems:?}");
        let c3fmt = &config.fmt.tools["c3fmt"];
        assert_eq!(c3fmt.command, ["c3fmt", "--default"]);
        assert_eq!(c3fmt.filetypes, ["c3"], "kept from the default");

        let (config, _) = ok("[fmt.tools.c3fmt]\nenabled = false\n");
        assert!(!config.fmt.tools["c3fmt"].enabled);
        assert_eq!(config.fmt.tools["c3fmt"].command[0], "c3fmt", "definition kept, just off");
    }

    #[test]
    fn a_new_formatter_is_defined_whole() {
        let src = "[fmt.tools.gofmt]\ncommand = [\"gofmt\"]\nfiletypes = [\"go\"]\n";
        let (config, problems) = ok(src);
        assert!(problems.is_empty(), "{problems:?}");
        let gofmt = &config.fmt.tools["gofmt"];
        assert_eq!(gofmt.command, ["gofmt"]);
        assert_eq!(gofmt.filetypes, ["go"]);
        assert!(gofmt.enabled, "enabled unless said otherwise");
    }

    #[test]
    fn a_local_config_may_not_name_a_formatter_binary() {
        let src = "[fmt.tools.c3fmt]\nfiletypes = [\"c3\", \"c3i\"]\ncommand = [\"evil\"]\n";
        let (config, problems) = ok_local(src);
        assert_eq!(problems, ["3: command is not read from a project config"]);
        assert_eq!(config.fmt.tools["c3fmt"].command, ["c3fmt", "--stdin", "--stdout"]);
        assert_eq!(
            config.fmt.tools["c3fmt"].filetypes,
            ["c3", "c3i"],
            "the harmless field is read"
        );
    }

    #[test]
    fn fmt_mistakes_are_named_with_their_line() {
        let (config, problems) = ok("[fmt.tools.c3fmt]\ncommand = \"c3fmt\"\n");
        assert_eq!(problems, ["2: command takes a list of strings"]);
        assert_eq!(config.fmt.tools["c3fmt"].command[0], "c3fmt", "the default survives");

        let (_, problems) = ok("[fmt]\nnope = 1\n");
        assert_eq!(problems, ["2: unknown fmt setting: nope"]);

        let (_, problems) = ok("[fmt.tools.c3fmt]\ncmd = [\"c3fmt\"]\n");
        assert_eq!(problems, ["2: unknown tool setting: cmd"]);
    }

    #[test]
    fn lsp_mistakes_are_named_with_their_line() {
        let (_, problems) = ok("[lsp]\nenabled = \"yes\"\n");
        assert_eq!(problems, ["2: enabled is true or false"]);

        let (config, problems) = ok("[lsp.servers.gopls]\ncommand = \"gopls\"\n");
        assert_eq!(problems, ["2: command takes a list of strings"]);
        assert_eq!(config.lsp.servers["gopls"].command, ["gopls"], "the default survives");

        let (_, problems) = ok("[lsp.servers.gopls]\ncommand = [\"gopls\", 3]\n");
        assert_eq!(problems, ["2: command takes a list of strings"]);

        let (_, problems) = ok("[lsp]\nnope = 1\n");
        assert_eq!(problems, ["2: unknown lsp setting: nope"]);

        let (_, problems) = ok("[lsp.servers.gopls]\ncmd = [\"gopls\"]\n");
        assert_eq!(problems, ["2: unknown server setting: cmd"]);
    }
}
