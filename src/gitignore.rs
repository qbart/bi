//! Which files a project says are not its files.
//!
//! Read for the file picker's walk, and for nothing else: `:e` on an ignored
//! path has always worked and still does. This is a question about a *list*.
//!
//! The matcher is component by component — the path split at `/`, the pattern
//! split at `/` — which is what makes git's `**` rule fall out rather than
//! being a special case, and leaves each component to a plain fnmatch that
//! cannot cross a separator because there is none left in it to cross.
//!
//! It is deliberately not `crate::editorconfig`'s glob: those two dialects
//! agree on `*`, `?`, classes and escapes, and disagree on everything after —
//! braces, ranges, and where `**` may span directories. Sharing one matcher
//! would mean a flag per divergence, and the divergences are exactly what a
//! shared implementation would get quietly wrong in one of its two callers.
//!
//! See `docs/specs/gitignore.md`.

use std::path::{Path, PathBuf};

/// One line of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// The pattern, split at `/`. A pattern that was not anchored begins with
    /// `**`, which is what "matches at any depth" means once the match is
    /// component by component.
    parts: Vec<String>,
    /// `!` — re-includes something an earlier line excluded.
    negated: bool,
    /// A trailing `/` — directories only.
    dir_only: bool,
}

/// Every `.gitignore` in play, in the order they are consulted.
///
/// Outermost first, because the last match wins and depth is what decides
/// between two of them.
#[derive(Debug, Default)]
pub struct Rules {
    files: Vec<(PathBuf, Vec<Rule>)>,
}

impl Rules {
    /// Adds the rules in `text`, which was found in the directory `base`.
    ///
    /// Later calls win over earlier ones, so a walk pushes as it descends.
    pub fn push(&mut self, base: impl Into<PathBuf>, text: &str) {
        let rules: Vec<Rule> = text.lines().filter_map(parse).collect();
        if !rules.is_empty() {
            self.files.push((base.into(), rules));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether `path` is ignored, where `is_dir` says what it is.
    ///
    /// **Directories must be asked about as the walk reaches them.** An
    /// ignored directory is never walked into — that is git's behaviour, it is
    /// where the speed comes from, and it is the reason a `!` cannot
    /// re-include something inside one. So this answers about the path it is
    /// given and does not go looking at its ancestors: `a/` ignores `a`, and
    /// what makes it ignore `a/b.txt` is that nobody ever asks.
    pub fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut ignored = false;
        for (base, rules) in &self.files {
            let Ok(rest) = path.strip_prefix(base) else { continue };
            let parts: Vec<String> =
                rest.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
            if parts.is_empty() {
                continue;
            }
            let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
            for rule in rules {
                if rule.dir_only && !is_dir {
                    continue;
                }
                if components(&rule.parts, &parts) {
                    // The last match decides, which is the whole of what makes
                    // `!` work.
                    ignored = !rule.negated;
                }
            }
        }
        ignored
    }
}

/// One line into a rule, or `None` for the lines that are not one.
fn parse(line: &str) -> Option<Rule> {
    let line = strip_trailing_spaces(line);
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    let (negated, line) = match line.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    // `\#` and `\!` are a `#` and a `!` that are not markers. Nothing else
    // needs unescaping here — the matcher reads the rest of the backslashes.
    let line = match line.strip_prefix('\\') {
        Some(rest) if rest.starts_with(['#', '!']) => rest,
        _ => line,
    };

    let (dir_only, line) = match line.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    if line.is_empty() {
        return None;
    }

    // Anchored to this file's own directory if there is a `/` left anywhere in
    // it — a leading one says so and is then nothing. Otherwise the pattern
    // matches at any depth, which is `**/` in front of it.
    let anchored = line.contains('/');
    let line = line.strip_prefix('/').unwrap_or(line);
    let mut parts: Vec<String> = line.split('/').map(str::to_string).collect();
    if !anchored {
        parts.insert(0, "**".to_string());
    }
    Some(Rule { parts, negated, dir_only })
}

/// Trailing spaces are not part of a pattern unless they were escaped, which
/// is git's rule and the one place its parser looks backwards.
fn strip_trailing_spaces(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b' ' {
        let escapes = bytes[..end - 1].iter().rev().take_while(|&&b| b == b'\\').count();
        if escapes % 2 == 1 {
            break;
        }
        end -= 1;
    }
    &line[..end]
}

/// Matches a split pattern against a split path.
///
/// `**` standing alone as a component spans directories, which is git's rule
/// for a `**` between slashes — and, once the match is component by component,
/// is the only place a `**` can stand at all. One inside a component is two
/// stars, which is one star, which is also what git does.
///
/// **Zero or more of them, except at the end, where it is one or more.** That
/// looks like a quibble and is the difference between `out/**` and `out/`:
/// the first excludes what is *in* the directory and the second excludes the
/// directory, so a later `!out/keep.rs` works under the first and cannot work
/// under the second. Git's own walk draws exactly that line — `git status`
/// lists `out/keep.rs` for one and not for the other — and
/// `tests/gitignore_git.rs` is what noticed.
fn components(pattern: &[String], path: &[&str]) -> bool {
    let Some(first) = pattern.first() else { return path.is_empty() };
    if first == "**" {
        let least = usize::from(pattern.len() == 1);
        return (least..=path.len()).any(|skip| components(&pattern[1..], &path[skip..]));
    }
    let Some(head) = path.first() else { return false };
    fnmatch(first, head) && components(&pattern[1..], &path[1..])
}

/// One component against one component. No separators are left in either, so
/// nothing here has to know what one is.
fn fnmatch(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    matches_from(&p, 0, &t, 0)
}

fn matches_from(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    match p[pi] {
        '*' => {
            // `**` inside a component is two stars, which is one star.
            let next = pi + p[pi..].iter().take_while(|&&c| c == '*').count();
            (ti..=t.len()).any(|end| matches_from(p, next, t, end))
        }
        '?' => ti < t.len() && matches_from(p, pi + 1, t, ti + 1),
        '[' => class(p, pi, t, ti),
        '\\' => match p.get(pi + 1) {
            Some(&escaped) => {
                ti < t.len() && t[ti] == escaped && matches_from(p, pi + 2, t, ti + 1)
            }
            None => ti < t.len() && t[ti] == '\\' && matches_from(p, pi + 1, t, ti + 1),
        },
        c => ti < t.len() && t[ti] == c && matches_from(p, pi + 1, t, ti + 1),
    }
}

/// `[abc]`, `[a-c]`, `[!abc]`.
///
/// A `[` with no `]` after it is a literal `[`, which is what a shell does and
/// what keeps a pattern like `foo[1` from matching nothing at all. POSIX
/// classes — `[[:alpha:]]` — are not supported and fall out as literals, so a
/// pattern using one matches nothing rather than misbehaving.
fn class(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    let Some(close) = p[pi + 1..].iter().position(|&c| c == ']').map(|i| pi + 1 + i) else {
        return ti < t.len() && t[ti] == '[' && matches_from(p, pi + 1, t, ti + 1);
    };
    if ti >= t.len() {
        return false;
    }
    let (negated, start) = match p.get(pi + 1) {
        Some('!') | Some('^') => (true, pi + 2),
        _ => (false, pi + 1),
    };
    let inside = &p[start..close];
    let mut hit = false;
    let mut i = 0;
    while i < inside.len() {
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
    hit != negated && matches_from(p, close + 1, t, ti + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Rules {
        let mut rules = Rules::default();
        rules.push("/repo", text);
        rules
    }

    fn ignored(text: &str, path: &str) -> bool {
        rules(text).ignored(&PathBuf::from("/repo").join(path), false)
    }

    fn dir_ignored(text: &str, path: &str) -> bool {
        rules(text).ignored(&PathBuf::from("/repo").join(path), true)
    }

    #[test]
    fn the_lines_that_are_not_rules() {
        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
        assert!(parse("# a comment").is_none());
        assert!(parse("/").is_none(), "a lone separator names nothing");
        // A `#` that is not a marker, and the same for `!`.
        assert!(ignored("\\#literal", "#literal"));
        assert!(ignored("\\!literal", "!literal"));
    }

    #[test]
    fn trailing_spaces_go_unless_they_were_escaped() {
        assert!(ignored("thing.txt   ", "thing.txt"));
        assert!(ignored("thing\\ ", "thing "));
    }

    #[test]
    fn a_pattern_without_a_separator_matches_at_any_depth() {
        assert!(ignored("*.log", "debug.log"));
        assert!(ignored("*.log", "deep/inside/debug.log"));
        assert!(!ignored("*.log", "debug.txt"));
    }

    #[test]
    fn a_separator_anchors_it_to_its_own_directory() {
        assert!(ignored("/build", "build"));
        assert!(!ignored("/build", "sub/build"));
        assert!(ignored("doc/*.txt", "doc/notes.txt"));
        assert!(!ignored("doc/*.txt", "sub/doc/notes.txt"));
        assert!(!ignored("doc/*.txt", "doc/deep/notes.txt"), "one star does not cross a slash");
    }

    #[test]
    fn a_double_star_between_slashes_spans_directories() {
        assert!(ignored("a/**/b", "a/b"), "zero directories between");
        assert!(ignored("a/**/b", "a/x/b"));
        assert!(ignored("a/**/b", "a/x/y/b"));
        assert!(!ignored("a/**/b", "z/a/b"), "and it is still anchored");
    }

    #[test]
    fn a_trailing_slash_is_directories_only() {
        assert!(dir_ignored("build/", "build"));
        assert!(!ignored("build/", "build"), "a file of that name is not a directory");
    }

    #[test]
    fn the_last_matching_line_decides() {
        let text = "*.log\n!keep.log\n";
        assert!(ignored(text, "debug.log"));
        assert!(!ignored(text, "keep.log"), "re-included by the line after");

        // And a later line excludes it again, because *last* is the rule
        // rather than *negation wins*.
        let text = "*.log\n!keep.log\nkeep.log\n";
        assert!(ignored(text, "keep.log"));
    }

    #[test]
    fn a_deeper_file_overrides_the_one_above_it() {
        let mut rules = Rules::default();
        rules.push("/repo", "*.log\n");
        rules.push("/repo/sub", "!*.log\n");

        assert!(rules.ignored(Path::new("/repo/debug.log"), false));
        assert!(!rules.ignored(Path::new("/repo/sub/debug.log"), false), "the nearer file wins");
    }

    #[test]
    fn a_rule_says_nothing_about_paths_outside_its_own_directory() {
        let mut rules = Rules::default();
        rules.push("/repo/sub", "*.log\n");
        assert!(!rules.ignored(Path::new("/repo/debug.log"), false));
        assert!(rules.ignored(Path::new("/repo/sub/debug.log"), false));
    }

    #[test]
    fn within_a_component() {
        assert!(ignored("?.txt", "a.txt"));
        assert!(!ignored("?.txt", "ab.txt"));
        assert!(ignored("[abc].txt", "b.txt"));
        assert!(ignored("[a-c].txt", "c.txt"));
        assert!(!ignored("[a-c].txt", "z.txt"));
        assert!(ignored("[!a-c].txt", "z.txt"));
        assert!(!ignored("[!a-c].txt", "a.txt"));
        assert!(ignored("\\*.txt", "*.txt"), "an escaped star is a star");
        assert!(!ignored("\\*.txt", "any.txt"));
    }

    /// `out/**` and `out/` are not the same rule, and the difference is
    /// whether a later `!` can still reach inside: git's walk lists
    /// `out/keep.rs` under the first and not under the second, because only
    /// the second excludes the *directory*.
    #[test]
    fn a_trailing_double_star_is_the_contents_and_not_the_directory() {
        assert!(!dir_ignored("out/**", "out"), "the directory itself is not excluded");
        assert!(ignored("out/**", "out/thing.rs"));
        assert!(ignored("out/**", "out/deep/thing.rs"));

        let text = "out/**\n!out/keep.rs\n";
        assert!(!ignored(text, "out/keep.rs"), "so the negation can still reach it");
        assert!(ignored(text, "out/thing.rs"));

        // Where `out/` excludes the directory, nothing looks inside it — that
        // is the walk's job, and `an_ignored_directory_is_pruned...` in
        // `files.rs` is where it is pinned.
        assert!(dir_ignored("out/", "out"));
    }

    /// `**` that is not a component of its own is two stars, which is one
    /// star — the same thing git does with it.
    #[test]
    fn a_double_star_inside_a_component_is_one_star() {
        assert!(ignored("a**b", "axxb"));
        assert!(!ignored("a**b", "a/x/b"));
    }
}
