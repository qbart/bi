//! `gq` — rewrapping prose to a width.
//!
//! Lines in and lines out, with a width and a tab width: all of the
//! paragraph, prefix and packing rules live here, testable without a rope.
//! The edits are [`crate::buffer::Buffer`]'s. See `docs/specs/reflow.md`.

/// The comment leaders a paragraph can carry. Longest first, so `///` wins
/// over `//`. A table rather than configuration: adding a language is adding
/// a string.
const LEADERS: [&str; 8] = ["///", "//!", "//", "#", "--", ";", "*", ">"];

/// Rewraps `lines` to `width` columns.
///
/// Blank lines pass through untouched and separate paragraphs; each run of
/// non-blank lines reflows on its own, under the prefix its first line names
/// — leading whitespace, plus a comment leader when every line of the
/// paragraph carries the same one.
pub fn reflow(lines: &[String], width: usize, tab_width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut paragraph: Vec<&String> = Vec::new();
    for line in lines {
        if crate::indent::is_blank(line) {
            wrap_into(&mut out, &paragraph, width, tab_width);
            paragraph.clear();
            out.push(line.clone());
        } else {
            paragraph.push(line);
        }
    }
    wrap_into(&mut out, &paragraph, width, tab_width);
    out
}

/// One paragraph: prefix, words, greedy packing.
fn wrap_into(out: &mut Vec<String>, paragraph: &[&String], width: usize, tab_width: usize) {
    let Some(first) = paragraph.first() else { return };
    let leader = leader_of(paragraph);
    let lead = crate::indent::leading(first);
    let prefix = match leader {
        Some(leader) => format!("{lead}{leader} "),
        None => lead.to_string(),
    };
    let prefix_width = crate::indent::width_of(&prefix, tab_width);

    let words: Vec<&str> = paragraph
        .iter()
        .flat_map(|line| {
            let rest = line.trim_start();
            let rest = match leader {
                Some(leader) => rest.strip_prefix(leader).unwrap_or(rest),
                None => rest,
            };
            rest.split_whitespace()
        })
        .collect();

    // A line that is only its leader — a `//` between two comment sentences —
    // stays a line rather than vanishing into the join.
    if words.is_empty() {
        out.push(prefix.trim_end().to_string());
        return;
    }

    let mut line = String::new();
    let mut line_width = 0;
    for word in words {
        let w = word.chars().count();
        if line.is_empty() {
            // Always at least one word: a word longer than the width gets a
            // line to itself and is never split.
            line.push_str(word);
            line_width = w;
        } else if prefix_width + line_width + 1 + w <= width {
            line.push(' ');
            line.push_str(word);
            line_width += 1 + w;
        } else {
            out.push(format!("{prefix}{line}"));
            line = word.to_string();
            line_width = w;
        }
    }
    out.push(format!("{prefix}{line}"));
}

/// The leader every line of the paragraph carries, if one does.
///
/// Every line, not just the first: a paragraph where line two starts with
/// something else is prose that happens to open with a `#`, and joining its
/// lines under that `#` would comment out what was not a comment.
fn leader_of(paragraph: &[&String]) -> Option<&'static str> {
    LEADERS
        .iter()
        .copied()
        .find(|leader| paragraph.iter().all(|line| line.trim_start().starts_with(leader)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|s| s.to_string()).collect()
    }

    fn wrap(text: &[&str], width: usize) -> Vec<String> {
        reflow(&lines(text), width, 4)
    }

    #[test]
    fn a_long_line_breaks_and_short_lines_merge() {
        assert_eq!(
            wrap(&["one two three four"], 9),
            ["one two", "three", "four"],
            "broken at the width"
        );
        assert_eq!(wrap(&["one", "two"], 20), ["one two"], "joined by one space");
    }

    #[test]
    fn a_comment_paragraph_keeps_its_leader_on_every_line() {
        assert_eq!(
            wrap(&["// one two three", "// four"], 11),
            ["// one two", "// three", "// four"],
            "the width counts the prefix"
        );
    }

    #[test]
    fn indentation_without_a_leader_is_kept() {
        assert_eq!(wrap(&["    one two three"], 12), ["    one two", "    three"]);
    }

    #[test]
    fn a_word_wider_than_the_width_stands_alone_unsplit() {
        assert_eq!(wrap(&["a incomprehensibilities b"], 10), ["a", "incomprehensibilities", "b"]);
    }

    #[test]
    fn paragraphs_stay_separate_and_the_blank_survives() {
        assert_eq!(wrap(&["one", "two", "", "three"], 20), ["one two", "", "three"]);
    }

    /// The whitespace-only line is a separator too, and passes through as it
    /// was — reflow wraps prose, it does not trim.
    #[test]
    fn a_whitespace_only_line_separates_and_survives() {
        assert_eq!(wrap(&["one", "  ", "two"], 20), ["one", "  ", "two"]);
    }

    #[test]
    fn a_leader_missing_from_one_line_demotes_the_prefix_to_whitespace() {
        assert_eq!(wrap(&["# one", "two"], 20), ["# one two"]);
    }

    /// `///` wins over `//`: the longest leader every line carries.
    #[test]
    fn the_longest_shared_leader_wins() {
        assert_eq!(wrap(&["/// doc one", "/// two"], 20), ["/// doc one two"]);
        assert_eq!(wrap(&["/// doc", "// plain"], 20), ["// / doc plain"]);
    }

    #[test]
    fn a_line_that_is_only_its_leader_survives() {
        assert_eq!(wrap(&["//"], 20), ["//"]);
    }

    #[test]
    fn tabs_in_the_indent_count_as_tab_width_columns() {
        // The tab is 4 columns, so 4 + "one two" = 11 > 10 forces the break.
        assert_eq!(wrap(&["\tone two"], 10), ["\tone", "\ttwo"]);
    }

    #[test]
    fn already_wrapped_text_comes_back_unchanged() {
        let text = lines(&["// one two", "// three"]);
        assert_eq!(reflow(&text, 11, 4), text.as_slice());
    }
}
