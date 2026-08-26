//! `.editorconfig`: what the project has already agreed.
//!
//! The fourth of the option layers in `docs/specs/options.md` — above what bi
//! thinks, above your config, above what the language asks for, below what you
//! `:set`. Reading it is less a feature than the absence of a bug: without it,
//! the first thing bi does to a well-run project is reindent a file the moment
//! you touch it.
//!
//! Two entry points, and the split is deliberate. [`patch_from`] is the whole
//! of the logic and touches no filesystem; [`patch_for`] is the walk up a
//! directory tree on top of it. An embedder whose files live somewhere that is
//! not a disk calls the first, exactly as [`crate::config::ConfigSource`] lets
//! one supply `config.toml` from wherever it lives.
//!
//! See `docs/specs/editorconfig.md`.

use std::path::{Path, PathBuf};

use crate::config::{OptionPatch, OptionValue};

/// One parsed `.editorconfig`.
pub struct EditorConfig {
    /// `root = true` — the walk stops here.
    pub root: bool,
    /// In the order they were written, because a later section beats an
    /// earlier one and that is the only thing that decides it.
    sections: Vec<Section>,
}

struct Section {
    pattern: String,
    /// Names and values, both lowercased: the format says they are
    /// case-insensitive, and lowercasing once here is cheaper than remembering
    /// to at every comparison.
    properties: Vec<(String, String)>,
}

impl EditorConfig {
    /// Parses the INI-ish format: full-line comments with `#` or `;`, one
    /// `[glob]` per section, `key = value` beneath it.
    ///
    /// Nothing here can fail. A line that is not one of those shapes is
    /// skipped, which is what an editor should do with a file written for the
    /// features of a different editor.
    pub fn parse(text: &str) -> Self {
        let (mut root, mut sections) = (false, Vec::new());
        let mut preamble: Vec<(String, String)> = Vec::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let pattern = rest.rsplit_once(']').map(|(name, _)| name).unwrap_or(rest);
                sections.push(Section { pattern: pattern.to_string(), properties: Vec::new() });
                continue;
            }
            let Some((name, value)) = line.split_once('=') else { continue };
            let (name, value) = (name.trim().to_lowercase(), value.trim().to_lowercase());
            match sections.last_mut() {
                Some(section) => section.properties.push((name, value)),
                // Before any section: only `root` means anything there, and it
                // is the one property that is about the file rather than about
                // the files it describes.
                None => preamble.push((name, value)),
            }
        }

        for (name, value) in preamble {
            if name == "root" {
                root = value == "true";
            }
        }
        Self { root, sections }
    }

    /// The properties that apply to `relative` — a path below the directory
    /// this file was found in, with `/` separators.
    ///
    /// Later sections overwrite earlier ones, which is the format's rule, so
    /// this walks in file order and lets each match overwrite what the last
    /// one said.
    fn properties_for(&self, relative: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for section in &self.sections {
            if !matches_path(&section.pattern, relative) {
                continue;
            }
            for (name, value) in &section.properties {
                match out.iter_mut().find(|(key, _)| key == name) {
                    Some(slot) => slot.1 = value.clone(),
                    None => out.push((name.clone(), value.clone())),
                }
            }
        }
        out
    }
}

/// Whether a section name matches a path relative to the file it came from.
///
/// The three rules everyone gets wrong, in one place: a pattern with no `/` at
/// all matches on the file name alone at any depth, a leading `/` anchors to
/// the `.editorconfig`'s own directory, and a `/` anywhere else anchors there
/// too.
fn matches_path(pattern: &str, relative: &str) -> bool {
    let unescaped = pattern.replace("\\/", "/");
    if !unescaped.contains('/') {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        return glob(pattern, name);
    }
    glob(pattern.strip_prefix('/').unwrap_or(pattern), relative)
}

/// The format's glob dialect — neither shell globbing nor a regular
/// expression. See `docs/specs/editorconfig.md`.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    match_from(&p, 0, &t, 0)
}

/// Backtracking, because the patterns are a handful of characters and the
/// paths they run against are short. The simple thing is the right thing here,
/// and a compiled matcher would be more code to be wrong in.
fn match_from(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }

    match p[pi] {
        '*' => {
            // `**` crosses directories; a single `*` stops at one.
            let (skip, crosses) = match p.get(pi + 1) {
                Some('*') => (2, true),
                _ => (1, false),
            };
            for end in ti..=t.len() {
                if match_from(p, pi + skip, t, end) {
                    return true;
                }
                if !crosses && t.get(end) == Some(&'/') {
                    return false;
                }
            }
            false
        }
        '?' => ti < t.len() && t[ti] != '/' && match_from(p, pi + 1, t, ti + 1),
        '[' => match_class(p, pi, t, ti),
        '{' => match_group(p, pi, t, ti),
        '\\' => match p.get(pi + 1) {
            Some(&escaped) => ti < t.len() && t[ti] == escaped && match_from(p, pi + 2, t, ti + 1),
            // A trailing backslash is a literal one.
            None => ti < t.len() && t[ti] == '\\' && match_from(p, pi + 1, t, ti + 1),
        },
        c => ti < t.len() && t[ti] == c && match_from(p, pi + 1, t, ti + 1),
    }
}

/// `[abc]` and `[!abc]`. Never matches `/`, whatever the class says: a
/// separator is structure, not a character.
fn match_class(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    let Some(close) = find_close(p, pi, '[', ']') else {
        // No closing bracket, so it was a literal `[`.
        return ti < t.len() && t[ti] == '[' && match_from(p, pi + 1, t, ti + 1);
    };
    if ti >= t.len() || t[ti] == '/' {
        return false;
    }
    let (negated, start) = match p.get(pi + 1) {
        Some('!') => (true, pi + 2),
        _ => (false, pi + 1),
    };
    let inside: &[char] = &p[start..close];
    let mut hit = false;
    let mut i = 0;
    while i < inside.len() {
        // `a-z`, which the format does not document and every project writes
        // anyway.
        if i + 2 < inside.len() && inside[i + 1] == '-' {
            if inside[i] <= t[ti] && t[ti] <= inside[i + 2] {
                hit = true;
            }
            i += 3;
            continue;
        }
        if inside[i] == t[ti] {
            hit = true;
        }
        i += 1;
    }
    hit != negated && match_from(p, close + 1, t, ti + 1)
}

/// `{a,b}` alternation and `{3..12}` ranges.
///
/// A group with no top-level comma and no `..` is **literal**, braces
/// included. That is the format's rule rather than an oversight: a lone `{x}`
/// is far more likely to be a file name than an alternation of one.
fn match_group(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    let Some(close) = find_close(p, pi, '{', '}') else {
        return ti < t.len() && t[ti] == '{' && match_from(p, pi + 1, t, ti + 1);
    };
    let inside: &[char] = &p[pi + 1..close];

    if let Some((lo, hi)) = numeric_range(inside) {
        return match_number(p, close + 1, t, ti, lo, hi);
    }

    let alternatives = split_alternatives(inside);
    if alternatives.len() < 2 {
        // Literal, braces and all.
        let literal: Vec<char> = p[pi..=close].to_vec();
        return t[ti..].starts_with(&literal) && match_from(p, close + 1, t, ti + literal.len());
    }

    for alternative in alternatives {
        // The alternative is spliced in front of what follows the group, so
        // that an alternative containing its own wildcards is matched by the
        // same machinery rather than by a second copy of it.
        let mut spliced: Vec<char> = alternative;
        spliced.extend_from_slice(&p[close + 1..]);
        if match_from(&spliced, 0, t, ti) {
            return true;
        }
    }
    false
}

/// An integer in `lo..=hi` at `ti`, then the rest of the pattern.
///
/// Longest first, so `{1..20}` matching "20" does not stop at the "2".
fn match_number(p: &[char], pi: usize, t: &[char], ti: usize, lo: i64, hi: i64) -> bool {
    let mut end = ti;
    if t.get(end) == Some(&'-') || t.get(end) == Some(&'+') {
        end += 1;
    }
    while t.get(end).is_some_and(|c| c.is_ascii_digit()) {
        end += 1;
    }
    while end > ti {
        let text: String = t[ti..end].iter().collect();
        if let Ok(n) = text.parse::<i64>()
            && lo <= n
            && n <= hi
            && match_from(p, pi, t, end)
        {
            return true;
        }
        end -= 1;
    }
    false
}

/// `3..12`, `-2..2`. `None` when this group is not a range at all.
fn numeric_range(inside: &[char]) -> Option<(i64, i64)> {
    let text: String = inside.iter().collect();
    let (lo, hi) = text.split_once("..")?;
    let (lo, hi) = (lo.parse::<i64>().ok()?, hi.parse::<i64>().ok()?);
    Some((lo.min(hi), lo.max(hi)))
}

/// Top-level commas only: a comma inside a nested `{}` belongs to that group.
fn split_alternatives(inside: &[char]) -> Vec<Vec<char>> {
    let (mut out, mut current, mut depth) = (Vec::new(), Vec::new(), 0usize);
    let mut i = 0;
    while i < inside.len() {
        let c = inside[i];
        if c == '\\' && i + 1 < inside.len() {
            current.push(c);
            current.push(inside[i + 1]);
            i += 2;
            continue;
        }
        match c {
            '{' => {
                depth += 1;
                current.push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
        i += 1;
    }
    out.push(current);
    out
}

/// The index of the `close` that matches the `open` at `from`, counting nested
/// pairs and honouring escapes.
fn find_close(p: &[char], from: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = from;
    while i < p.len() {
        if p[i] == '\\' {
            i += 2;
            continue;
        }
        if p[i] == open {
            depth += 1;
        } else if p[i] == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Everything `.editorconfig` says about `path`, as an option layer.
///
/// Walks up from the file's own directory, one file per level, stopping at the
/// filesystem root or at the first `root = true`. Nothing is cached: the walk
/// is a handful of `stat`s and it runs when options are resolved, never on a
/// keystroke — and a cache would have to be invalidated when a file bi does
/// not have open changes.
pub fn patch_for(path: &Path) -> OptionPatch {
    let (files, absolute) = files_for(path);
    patch_from(&files, &absolute)
}

/// The project's say about how `path` is *stored* — `charset` and
/// `end_of_line`. Not options, so not in the patch: they apply at the open
/// boundary, where `docs/specs/encoding.md` says they do.
pub fn storage_for(path: &Path) -> StorageHints {
    let (files, absolute) = files_for(path);
    storage_from(&files, &absolute)
}

/// What `charset` and `end_of_line` said, translated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageHints {
    /// The encoding, and whether the value asks for a BOM (`utf-8-bom`,
    /// either UTF-16).
    pub charset: Option<(&'static encoding_rs::Encoding, bool)>,
    pub end_of_line: Option<crate::encoding::FileFormat>,
}

pub fn storage_from(files: &[(PathBuf, String)], path: &Path) -> StorageHints {
    let properties = merged_properties(files, path);
    let get = |name: &str| -> Option<&str> {
        properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .filter(|value| *value != "unset")
    };
    let charset = get("charset").and_then(|value| match value {
        "utf-8-bom" => Some((encoding_rs::UTF_8, true)),
        "utf-16le" => Some((encoding_rs::UTF_16LE, true)),
        "utf-16be" => Some((encoding_rs::UTF_16BE, true)),
        // `latin1` and `utf-8` through the same table `:set` uses. A value
        // the table has no encoding for is a property for other editors, and
        // ignored the way every unknown property is.
        other => crate::encoding::lookup(other).map(|encoding| (encoding, false)),
    });
    let end_of_line = get("end_of_line").and_then(|value| match value {
        "lf" => Some(crate::encoding::FileFormat::Unix),
        "crlf" => Some(crate::encoding::FileFormat::Dos),
        // `cr` is a Mac OS 9 file; bi does not write those.
        _ => None,
    });
    StorageHints { charset, end_of_line }
}

/// Every `.editorconfig` above `path`, nearest last, and the absolute path
/// their sections are matched against.
fn files_for(path: &Path) -> (Vec<(PathBuf, String)>, PathBuf) {
    let absolute = match path.is_absolute() {
        true => path.to_path_buf(),
        // What a relative path already means to the `open` that read the
        // buffer. The one ambient process value this library reads, and the
        // same one `Buffer::open` reads without saying so.
        false => std::env::current_dir().unwrap_or_default().join(path),
    };

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let mut dir = absolute.parent().map(Path::to_path_buf);
    while let Some(here) = dir {
        if let Ok(text) = std::fs::read_to_string(here.join(".editorconfig")) {
            let root = EditorConfig::parse(&text).root;
            files.push((here.clone(), text));
            if root {
                break;
            }
        }
        dir = here.parent().map(Path::to_path_buf);
    }

    // Nearest last: the file closest to yours has the last word.
    files.reverse();
    (files, absolute)
}

/// The same, from files someone else read. `files` are `(directory, text)`,
/// farthest first — the nearest `.editorconfig` has the last word.
pub fn patch_from(files: &[(PathBuf, String)], path: &Path) -> OptionPatch {
    to_patch(&merged_properties(files, path))
}

fn merged_properties(files: &[(PathBuf, String)], path: &Path) -> Vec<(String, String)> {
    let mut properties: Vec<(String, String)> = Vec::new();
    for (dir, text) in files {
        let Some(relative) = relative_to(dir, path) else { continue };
        for (name, value) in EditorConfig::parse(text).properties_for(&relative) {
            match properties.iter_mut().find(|(key, _)| *key == name) {
                Some(slot) => slot.1 = value,
                None => properties.push((name, value)),
            }
        }
    }
    properties
}

/// `path` below `dir`, with `/` separators whatever the platform uses.
fn relative_to(dir: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(dir).ok()?;
    let mut out = String::new();
    for part in rest.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&part.as_os_str().to_string_lossy());
    }
    Some(out)
}

/// The properties that name something bi has, as options.
///
/// Everything else is ignored in silence. `charset` and `end_of_line` are not
/// errors — bi is always UTF-8 and always writes `\n`, which is what they
/// would have asked for — and the rest are properties for editors with
/// features bi does not have yet.
fn to_patch(properties: &[(String, String)]) -> OptionPatch {
    let get = |name: &str| -> Option<&str> {
        properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            // `unset` means "the editor's own default", so the property is
            // dropped and the layer below shows through — which is what the
            // word means.
            .filter(|value| *value != "unset")
    };

    let mut patch = OptionPatch::default();
    // The two properties that are bi's trim options by another name — see
    // `docs/specs/trim.md`.
    if let Some(value) = get("trim_trailing_whitespace") {
        patch.set("trim_trailing", OptionValue::Bool(value == "true"));
    }
    if let Some(value) = get("insert_final_newline") {
        patch.set("trim_final_newline", OptionValue::Bool(value == "true"));
    }
    match get("indent_style") {
        Some("tab") => patch.set("expandtab", OptionValue::Bool(false)),
        Some("space") => patch.set("expandtab", OptionValue::Bool(true)),
        _ => {}
    }

    let tab_width = get("tab_width").and_then(|value| value.parse::<i64>().ok());
    if let Some(width) = tab_width {
        patch.set("tab_width", OptionValue::Int(width));
    }

    match get("indent_size") {
        // "whatever tab_width is", which is exactly what bi's `shiftwidth = 0`
        // already means — so it needs no arithmetic and cannot go stale if
        // `tab_width` changes under it.
        Some("tab") => patch.set("shiftwidth", OptionValue::Int(0)),
        Some(size) => {
            if let Ok(size) = size.parse::<i64>() {
                patch.set("shiftwidth", OptionValue::Int(size));
                // The format's own rule: `indent_size` sets the tab width too
                // unless the file says otherwise. It is what makes
                // `indent_size = 2` do what someone writing it expects in a
                // file that turns out to contain tabs.
                if tab_width.is_none() {
                    patch.set("tab_width", OptionValue::Int(size));
                }
            }
        }
        None => {}
    }
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_stops_at_a_separator_and_two_cross_it() {
        assert!(glob("*.py", "main.py"));
        assert!(!glob("*.py", "src/main.py"));
        assert!(glob("**.py", "src/deep/main.py"));
        assert!(glob("src/*.py", "src/main.py"));
        assert!(!glob("src/*.py", "src/pkg/main.py"));
        assert!(glob("src/**/*.py", "src/pkg/main.py"));
    }

    #[test]
    fn one_character_and_classes() {
        assert!(glob("?.c", "a.c"));
        assert!(!glob("?.c", "ab.c"));
        assert!(glob("[abc].c", "b.c"));
        assert!(!glob("[abc].c", "d.c"));
        assert!(glob("[!abc].c", "d.c"));
        assert!(!glob("[!abc].c", "a.c"));
        assert!(glob("[a-f].c", "d.c"));
        assert!(!glob("[a-f].c", "z.c"));
    }

    #[test]
    fn alternation_nests_and_may_contain_wildcards() {
        assert!(glob("*.{js,ts}", "app.ts"));
        assert!(glob("*.{js,ts}", "app.js"));
        assert!(!glob("*.{js,ts}", "app.rs"));
        assert!(glob("{package.json,*.{js,ts}}", "package.json"));
        assert!(glob("{package.json,*.{js,ts}}", "app.js"));
        assert!(!glob("{package.json,*.{js,ts}}", "app.rs"));
    }

    /// The format's rule, and not an oversight: a lone `{x}` is far more
    /// likely to be a file name than an alternation of one.
    #[test]
    fn a_group_with_no_comma_is_literal_braces_and_all() {
        assert!(glob("{single}.txt", "{single}.txt"));
        assert!(!glob("{single}.txt", "single.txt"));
    }

    #[test]
    fn numeric_ranges_including_negatives() {
        assert!(glob("page{1..10}.md", "page7.md"));
        assert!(glob("page{1..10}.md", "page10.md"), "longest first, or it stops at the 1");
        assert!(!glob("page{1..10}.md", "page11.md"));
        assert!(glob("t{-2..2}", "t-1"));
        assert!(!glob("t{-2..2}", "t-3"));
    }

    #[test]
    fn a_backslash_makes_a_wildcard_literal() {
        assert!(glob("\\*.py", "*.py"));
        assert!(!glob("\\*.py", "main.py"));
    }

    #[test]
    fn where_a_pattern_is_anchored_depends_on_its_slashes() {
        // No separator anywhere: the file name, at any depth.
        assert!(matches_path("*.py", "deep/inside/main.py"));
        // Leading: the .editorconfig's own directory.
        assert!(matches_path("/Makefile", "Makefile"));
        assert!(!matches_path("/Makefile", "sub/Makefile"));
        // Inner: anchored there too.
        assert!(matches_path("lib/**.js", "lib/deep/app.js"));
        assert!(!matches_path("lib/**.js", "src/lib/deep/app.js"));
    }

    fn config(text: &str) -> EditorConfig {
        EditorConfig::parse(text)
    }

    #[test]
    fn a_later_section_beats_an_earlier_one() {
        let file = config("[*]\nindent_size = 2\n\n[*.go]\nindent_size = 8\n");
        assert_eq!(file.properties_for("main.go"), [("indent_size".into(), "8".into())]);
        assert_eq!(file.properties_for("main.py"), [("indent_size".into(), "2".into())]);
    }

    #[test]
    fn root_is_read_and_comments_are_not() {
        assert!(config("root = true\n[*]\n").root);
        assert!(!config("# root = true\n[*]\n").root);
        assert!(!config("; root = true\n[*]\n").root);
        assert!(!config("[*]\nindent_size = 2\n").root);
    }

    fn patch_of(text: &str, name: &str) -> Vec<(String, OptionValue)> {
        let files = vec![(PathBuf::from("/p"), text.to_string())];
        let patch = patch_from(&files, &PathBuf::from("/p").join(name));
        let mut options = crate::config::Options::default();
        patch.apply_to(&mut options);
        ["expandtab", "tab_width", "shiftwidth"]
            .into_iter()
            .filter(|key| patch.holds(key))
            .map(|key| (key.to_string(), options.get(key).unwrap()))
            .collect()
    }

    #[test]
    fn indent_size_sets_the_tab_width_unless_the_file_does() {
        assert_eq!(
            patch_of("[*]\nindent_size = 2\n", "a.py"),
            [
                ("tab_width".to_string(), OptionValue::Int(2)),
                ("shiftwidth".to_string(), OptionValue::Int(2)),
            ]
        );
        assert_eq!(
            patch_of("[*]\nindent_size = 2\ntab_width = 8\n", "a.py"),
            [
                ("tab_width".to_string(), OptionValue::Int(8)),
                ("shiftwidth".to_string(), OptionValue::Int(2)),
            ]
        );
    }

    #[test]
    fn indent_size_tab_is_shiftwidth_zero() {
        assert_eq!(
            patch_of("[*]\nindent_size = tab\n", "a.py"),
            [("shiftwidth".to_string(), OptionValue::Int(0))]
        );
    }

    #[test]
    fn indent_style_decides_the_character() {
        assert_eq!(
            patch_of("[*]\nindent_style = tab\n", "a.go"),
            [("expandtab".to_string(), OptionValue::Bool(false))]
        );
        assert_eq!(
            patch_of("[*]\nindent_style = space\n", "a.py"),
            [("expandtab".to_string(), OptionValue::Bool(true))]
        );
    }

    #[test]
    fn unset_leaves_the_layer_below_showing_through() {
        assert!(patch_of("[*]\nindent_style = unset\nindent_size = unset\n", "a.py").is_empty());
    }

    #[test]
    fn a_file_of_things_bi_does_not_know_changes_nothing() {
        let text = "[*]\ncharset = utf-8\nend_of_line = lf\nmax_line_length = 100\n";
        assert!(patch_of(text, "a.py").is_empty());
    }

    #[test]
    fn the_two_whitespace_properties_are_bis_trim_options() {
        let files = vec![(
            PathBuf::from("/p"),
            "[*]\ntrim_trailing_whitespace = true\ninsert_final_newline = true\n".to_string(),
        )];
        let patch = patch_from(&files, Path::new("/p/a.py"));
        let mut options = crate::config::Options::default();
        patch.apply_to(&mut options);
        assert!(options.trim.trailing);
        assert!(options.trim.final_newline);

        let files =
            vec![(PathBuf::from("/p"), "[*]\ntrim_trailing_whitespace = false\n".to_string())];
        let patch = patch_from(&files, Path::new("/p/a.py"));
        patch.apply_to(&mut options);
        assert!(!options.trim.trailing, "and false reaches it too");
    }

    #[test]
    fn the_nearest_file_has_the_last_word() {
        let files = vec![
            (PathBuf::from("/project"), "[*]\nindent_size = 8\n".to_string()),
            (PathBuf::from("/project/src"), "[*]\nindent_size = 2\n".to_string()),
        ];
        let patch = patch_from(&files, Path::new("/project/src/main.py"));
        let mut options = crate::config::Options::default();
        patch.apply_to(&mut options);
        assert_eq!(options.shiftwidth, 2);
    }
}
