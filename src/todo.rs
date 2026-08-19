//! `TODO:` and its friends, found in a line.
//!
//! Seven words carry most of what a codebase says about itself and all seven
//! read as ordinary comment-grey. This is the scan that finds them; the colour
//! is the theme's and the drawing is a decoration.
//!
//! Not restricted to comments, deliberately — see
//! `docs/specs/todo-comments.md`.

use std::ops::Range;

/// What a marker means, which is what decides its colour. Five rather than one
/// per keyword: the same thought has more than one spelling in the wild, and
/// two colours for one meaning would be a palette nobody could read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Fix,
    Todo,
    Warn,
    Perf,
    Note,
}

/// The keywords, longest first so that a prefix of another cannot win — the
/// order is what stops `WARN` matching inside `WARNING`.
const KEYWORDS: &[(&str, Tag)] = &[
    ("PERFORMANCE", Tag::Perf),
    ("WARNING", Tag::Warn),
    ("TESTING", Tag::Note),
    ("FIXME", Tag::Fix),
    ("ISSUE", Tag::Fix),
    ("OPTIM", Tag::Perf),
    ("TODO", Tag::Todo),
    ("WARN", Tag::Warn),
    ("PERF", Tag::Perf),
    ("NOTE", Tag::Note),
    ("TEST", Tag::Note),
    ("INFO", Tag::Note),
    ("HACK", Tag::Warn),
    ("BUG", Tag::Fix),
    ("XXX", Tag::Warn),
    ("FIX", Tag::Fix),
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Every marker in `line`, as a char range within it.
///
/// The range covers the keyword, the owner in parentheses if there is one, and
/// the colon — `TODO(bart):` entire, because the convention is one marker and
/// painting half of it would look like a bug.
///
/// Uppercase only, on a word boundary, and the colon is required: the word is
/// a marker rather than a word, and `todo` in a sentence about a to-do list is
/// prose.
pub fn tags(line: &str) -> Vec<(Range<usize>, Tag)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if i > 0 && is_word(chars[i - 1]) {
            i += 1;
            continue;
        }
        let found = KEYWORDS.iter().find(|(word, _)| {
            chars[i..].len() >= word.len()
                && word.chars().eq(chars[i..i + word.len()].iter().copied())
        });
        let Some((word, tag)) = found else {
            i += 1;
            continue;
        };

        let mut end = i + word.len();
        // `TODO(bart):` — an owner in parentheses, which is a convention
        // everywhere and is part of the marker rather than of the sentence.
        if chars.get(end) == Some(&'(')
            && let Some(close) = chars[end..].iter().position(|&c| c == ')')
        {
            end += close + 1;
        }
        if chars.get(end) != Some(&':') {
            i += 1;
            continue;
        }
        end += 1;
        out.push((i..end, *tag));
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(line: &str) -> Vec<(String, Tag)> {
        tags(line)
            .into_iter()
            .map(|(range, tag)| (line.chars().skip(range.start).take(range.len()).collect(), tag))
            .collect()
    }

    #[test]
    fn every_keyword_and_alias_lands_on_its_own_colour() {
        assert_eq!(found("// FIX: it").first().unwrap().1, Tag::Fix);
        assert_eq!(found("// FIXME: it").first().unwrap().1, Tag::Fix);
        assert_eq!(found("// BUG: it").first().unwrap().1, Tag::Fix);
        assert_eq!(found("// TODO: it").first().unwrap().1, Tag::Todo);
        assert_eq!(found("// HACK: it").first().unwrap().1, Tag::Warn);
        assert_eq!(found("// WARN: it").first().unwrap().1, Tag::Warn);
        assert_eq!(found("// XXX: it").first().unwrap().1, Tag::Warn);
        assert_eq!(found("// PERF: it").first().unwrap().1, Tag::Perf);
        assert_eq!(found("// NOTE: it").first().unwrap().1, Tag::Note);
        assert_eq!(found("// TEST: it").first().unwrap().1, Tag::Note);
    }

    /// The order in `KEYWORDS` is the only thing that stops the shorter word
    /// winning, and nothing but this notices when it is wrong.
    #[test]
    fn a_longer_keyword_beats_the_shorter_one_inside_it() {
        assert_eq!(found("WARNING: loud"), [("WARNING:".to_string(), Tag::Warn)]);
        assert_eq!(found("TESTING: slow"), [("TESTING:".to_string(), Tag::Note)]);
    }

    #[test]
    fn the_marker_is_uppercase_bounded_and_followed_by_a_colon() {
        assert!(found("// todo: lowercase is prose").is_empty());
        assert!(found("// TODO rewrite this").is_empty(), "the colon is the marker");
        assert!(found("// MYTODO: not a boundary").is_empty());
        assert!(found("// TODOS: not the word").is_empty());
    }

    #[test]
    fn an_owner_in_parentheses_is_part_of_it() {
        assert_eq!(found("// TODO(bart): mine"), [("TODO(bart):".to_string(), Tag::Todo)]);
    }

    #[test]
    fn two_on_one_line_are_two() {
        assert_eq!(
            found("// TODO: one, and NOTE: two"),
            [("TODO:".to_string(), Tag::Todo), ("NOTE:".to_string(), Tag::Note),]
        );
    }

    #[test]
    fn the_range_covers_the_marker_and_nothing_after_it() {
        let line = "    // TODO: rewrite";
        let (range, _) = tags(line).into_iter().next().unwrap();
        assert_eq!(range, 7..12);
        assert_eq!(line.chars().skip(range.start).take(range.len()).collect::<String>(), "TODO:");
    }
}
