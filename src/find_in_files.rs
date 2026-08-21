//! Searching every file under a root, and rewriting what was found.
//!
//! ripgrep's own crates rather than a `rg` subprocess: `grep-searcher` for the
//! line-by-line walk, `grep-regex` for the pattern, `ignore` for the parallel
//! directory walk that already knows what a `.gitignore` is. That is the
//! library the TODO asked for, and it is also the only way this can be in the
//! core at all — a frontend-agnostic library must not shell out to a binary
//! the host may not have.
//!
//! Searching only. Nothing here opens a buffer or touches a window; the editor
//! joins this to those, the same split every other module in bi makes.
//!
//! See `docs/specs/find-in-files.md`.

use std::path::{Path, PathBuf};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;

/// One line that matched, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Relative to the root the search was run from, so a row is readable and
    /// a result set does not repeat the same prefix hundreds of times.
    pub path: PathBuf,
    /// One-based, as every tool that prints one does.
    pub line: usize,
    /// Char offset of the match within the line, and how long it is. Kept so a
    /// replace can rewrite exactly what matched rather than searching the line
    /// again with a different engine and finding somewhere else.
    pub col: usize,
    pub len: usize,
    /// The line, without its terminator.
    pub text: String,
}

/// How to search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub pattern: String,
    /// Whether upper case in the pattern makes the search case-sensitive —
    /// ripgrep's smart case, and what people expect from a project search.
    pub smart_case: bool,
    /// Whether the pattern is a regular expression. Off means every character
    /// stands for itself, which is what `:find fn main()` has to mean.
    pub regex: bool,
    /// Whether to skip what the project's `.gitignore` says are not its files.
    pub gitignore: bool,
}

impl Default for Query {
    fn default() -> Self {
        Self { pattern: String::new(), smart_case: true, regex: false, gitignore: true }
    }
}

/// How many matches are collected before the search gives up.
///
/// A search that returns a hundred thousand rows has not answered anything,
/// and building the list costs more than running it. The cap is reported
/// rather than silently applied — see [`Found::capped`].
pub const LIMIT: usize = 10_000;

/// What a search found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub matches: Vec<Match>,
    /// Whether [`LIMIT`] stopped it early. Said out loud, because a list that
    /// stops somewhere and does not say so looks like an answer.
    pub capped: bool,
    /// Files that could not be read, by name. Not an error for the search:
    /// one unreadable file in a tree of ten thousand is a fact to mention,
    /// not a reason to have no results.
    pub unreadable: usize,
}

/// The engine for a query.
///
/// Public because `:replace` needs the same one: the search records one row
/// per matching *line*, so rewriting has to find the occurrences within a line
/// again, and finding them with a different engine than the one that reported
/// them is how a replace lands somewhere nobody was shown.
///
/// `fixed_strings` rather than `build_literals`: the latter wraps the pattern
/// in a group without escaping it, so `:find a(` came back as "unclosed group"
/// — a regex error about a search that was never meant to be a regex. One
/// `build` either way keeps the offsets meaning the same thing in both modes.
pub fn matcher(query: &Query) -> Result<grep_regex::RegexMatcher, String> {
    if query.pattern.is_empty() {
        return Err("find what?".into());
    }
    let mut builder = RegexMatcherBuilder::new();
    builder.case_smart(query.smart_case);
    builder.fixed_strings(!query.regex);
    builder.build(&query.pattern).map_err(|e| format!("bad pattern: {e}"))
}

/// Rewrites every occurrence in `line`, and says how many.
///
/// `None` when there were none, so a caller can leave the line — and the undo
/// history — completely alone.
///
/// Every occurrence rather than the first, because the result pane shows one
/// row per line: a row that says `needle and needle` and then replaces half of
/// itself is a row that lied about what it was offering.
pub fn replace_all(
    matcher: &grep_regex::RegexMatcher,
    line: &str,
    with: &str,
) -> Option<(String, usize)> {
    use grep_matcher::Matcher;

    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    let mut count = 0;
    while at <= line.len() {
        let found = matcher.find_at(line.as_bytes(), at).ok().flatten();
        let Some(m) = found else { break };
        out.push_str(&line[at..m.start()]);
        out.push_str(with);
        count += 1;
        // An empty match would sit here forever; step past one character so a
        // pattern that can match nothing terminates.
        at = match m.end() > m.start() {
            true => m.end(),
            false => match line[m.end()..].chars().next() {
                Some(c) => {
                    out.push(c);
                    m.end() + c.len_utf8()
                }
                None => break,
            },
        };
    }
    if count == 0 {
        return None;
    }
    out.push_str(&line[at.min(line.len())..]);
    Some((out, count))
}

/// Searches every file under `root`.
///
/// `Err` is for a pattern that is not a pattern — the one failure that is
/// about what you typed rather than about the tree.
pub fn search(root: &Path, query: &Query) -> Result<Found, String> {
    // The empty-pattern check lives in `matcher`, so `:find` and `:replace`
    // cannot come to different conclusions about what nothing means.
    let matcher = matcher(query)?;

    let mut searcher = SearcherBuilder::new()
        // A binary file has no lines to show and one match in it would print a
        // screen of control characters. ripgrep's own default.
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .build();

    let mut found = Found::default();
    let walk = WalkBuilder::new(root)
        .git_ignore(query.gitignore)
        .git_global(query.gitignore)
        .git_exclude(query.gitignore)
        .ignore(query.gitignore)
        .parents(query.gitignore)
        .hidden(true)
        .build();

    for entry in walk {
        if found.matches.len() >= LIMIT {
            found.capped = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                found.unreadable += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        // Shown relative, so a row is readable; the editor joins the root back
        // on when it opens one.
        let shown = path.strip_prefix(root).unwrap_or(path).to_path_buf();

        let result = searcher.search_path(
            &matcher,
            path,
            UTF8(|line, text| {
                let text = text.trim_end_matches(['\n', '\r']).to_string();
                // Byte offsets from the engine, char offsets out: everything
                // above this line counts in chars, and a match after a `é` is
                // in the wrong column otherwise.
                let (col, len) = find_span(&matcher, &text).unwrap_or_default();
                found.matches.push(Match {
                    path: shown.clone(),
                    line: line as usize,
                    col,
                    len,
                    text,
                });
                Ok(found.matches.len() < LIMIT)
            }),
        );
        if result.is_err() {
            found.unreadable += 1;
        }
    }

    Ok(found)
}

/// Where the pattern matched within one line, in chars.
fn find_span(matcher: &grep_regex::RegexMatcher, text: &str) -> Option<(usize, usize)> {
    use grep_matcher::Matcher;
    let m = matcher.find(text.as_bytes()).ok().flatten()?;
    let col = text.get(..m.start())?.chars().count();
    let len = text.get(m.start()..m.end())?.chars().count();
    Some((col, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("bi-find-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, text: &str) -> &Self {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, text).unwrap();
            self
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rows(found: &Found) -> Vec<String> {
        let mut out: Vec<String> = found
            .matches
            .iter()
            .map(|m| format!("{}:{}:{} {}", m.path.display(), m.line, m.col, m.text))
            .collect();
        // The walk is parallel-capable and its order is not promised, so the
        // tests sort rather than depending on one.
        out.sort();
        out
    }

    fn query(pattern: &str) -> Query {
        Query { pattern: pattern.into(), ..Query::default() }
    }

    #[test]
    fn finds_a_literal_in_every_file_under_the_root() {
        let d = Dir::new("literal");
        d.file("a.txt", "alpha\nbeta\n").file("sub/b.txt", "gamma\nbeta again\n");

        let found = search(&d.0, &query("beta")).unwrap();

        assert_eq!(rows(&found), ["a.txt:2:0 beta", "sub/b.txt:2:0 beta again"]);
        assert!(!found.capped);
    }

    #[test]
    fn a_literal_is_literal() {
        let d = Dir::new("literal-dots");
        d.file("a.txt", "fn main()\nfn maiX()\n");

        // `.` would match the `X` line if this went through the regex engine.
        let found = search(&d.0, &query("fn main()")).unwrap();

        assert_eq!(found.matches.len(), 1);
        assert_eq!(found.matches[0].line, 1);
    }

    #[test]
    fn a_regex_is_one_when_asked_for() {
        let d = Dir::new("regex");
        d.file("a.txt", "foo1\nfoo2\nbar\n");

        let found =
            search(&d.0, &Query { pattern: r"foo\d".into(), regex: true, ..Query::default() })
                .unwrap();

        assert_eq!(found.matches.len(), 2);
    }

    #[test]
    fn smart_case_is_insensitive_until_you_type_a_capital() {
        let d = Dir::new("smart-case");
        d.file("a.txt", "Beta\nbeta\n");

        assert_eq!(search(&d.0, &query("beta")).unwrap().matches.len(), 2, "lower finds both");
        assert_eq!(search(&d.0, &query("Beta")).unwrap().matches.len(), 1, "a capital means it");
    }

    #[test]
    fn the_column_is_in_characters_not_bytes() {
        let d = Dir::new("unicode");
        d.file("a.txt", "héllo needle\n");

        let found = search(&d.0, &query("needle")).unwrap();

        assert_eq!(found.matches[0].col, 6, "six characters in, not seven bytes");
        assert_eq!(found.matches[0].len, 6);
    }

    #[test]
    fn gitignored_files_are_skipped_unless_you_ask_for_them() {
        let d = Dir::new("gitignore");
        d.file(".gitignore", "skip/\n")
            .file("keep.txt", "needle\n")
            .file("skip/no.txt", "needle\n");
        // `ignore` only applies .gitignore inside something it believes is a
        // repository, which a .git directory is what makes it.
        std::fs::create_dir_all(d.0.join(".git")).unwrap();

        assert_eq!(rows(&search(&d.0, &query("needle")).unwrap()), ["keep.txt:1:0 needle"]);

        let all = Query { gitignore: false, ..query("needle") };
        assert_eq!(search(&d.0, &all).unwrap().matches.len(), 2, "off reaches it again");
    }

    #[test]
    fn a_binary_file_does_not_print_itself() {
        let d = Dir::new("binary");
        d.file("a.txt", "needle\n");
        std::fs::write(d.0.join("b.bin"), b"needle\x00\x01\x02needle").unwrap();

        let found = search(&d.0, &query("needle")).unwrap();

        assert_eq!(rows(&found), ["a.txt:1:0 needle"]);
    }

    #[test]
    fn an_empty_pattern_is_a_question_rather_than_every_line() {
        let d = Dir::new("empty");
        d.file("a.txt", "anything\n");

        assert!(search(&d.0, &query("")).is_err());
    }

    #[test]
    fn a_pattern_that_is_not_one_says_so() {
        let d = Dir::new("bad-regex");
        d.file("a.txt", "x\n");

        let bad = Query { pattern: "a(".into(), regex: true, ..Query::default() };
        let message = search(&d.0, &bad).unwrap_err();

        assert!(message.starts_with("bad pattern:"), "{message}");
        // And the same text as a literal is fine, which is the point of the
        // flag: `:find a(` is a thing people type.
        let literal = Query { pattern: "a(".into(), ..Query::default() };
        assert!(search(&d.0, &literal).is_ok(), "a literal `a(` is a thing people search for");
    }

    #[test]
    fn replace_all_rewrites_every_occurrence_on_the_line() {
        let m = matcher(&query("needle")).unwrap();

        assert_eq!(
            replace_all(&m, "needle and needle", "pin"),
            Some(("pin and pin".to_string(), 2)),
            "the row showed one line, so the whole line is rewritten"
        );
        assert_eq!(replace_all(&m, "nothing here", "pin"), None);
        assert_eq!(replace_all(&m, "needle", ""), Some((String::new(), 1)), "deleting is allowed");
    }

    #[test]
    fn replace_all_follows_the_case_rule_the_search_used() {
        // Whatever the list showed you is what gets rewritten, which only
        // holds because both go through the same matcher.
        let insensitive = matcher(&query("beta")).unwrap();
        assert_eq!(replace_all(&insensitive, "Beta beta", "x").unwrap().1, 2);

        let sensitive = matcher(&query("Beta")).unwrap();
        assert_eq!(replace_all(&sensitive, "Beta beta", "x").unwrap().1, 1);
    }

    #[test]
    fn a_pattern_that_can_match_nothing_still_terminates() {
        let m = matcher(&Query { pattern: "x*".into(), regex: true, ..Query::default() }).unwrap();

        let (text, count) = replace_all(&m, "axb", "-").unwrap();

        assert!(count > 0, "it matched something");
        assert!(text.contains('a') && text.contains('b'), "and ate nothing: {text}");
    }

    #[test]
    fn two_matches_on_one_line_are_one_row() {
        // One row per *line*, which is what a result list is. The column is the
        // first match, which is where the cursor goes.
        let d = Dir::new("twice");
        d.file("a.txt", "needle and needle\n");

        let found = search(&d.0, &query("needle")).unwrap();

        assert_eq!(found.matches.len(), 1);
        assert_eq!(found.matches[0].col, 0);
    }
}
