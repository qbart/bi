//! The completion menu's state, in the picker's mold: it filters a list and
//! returns a choice, and it does not draw. See `docs/specs/complete.md`.
//!
//! The items arrive once per server answer; the *filtering* is local, per
//! keystroke, over the word the buffer holds between `replace.start` and the
//! cursor — re-read every time, so the filter cannot drift from the text.

use std::ops::Range;

use crate::lsp::pos::Encoding;
use crate::lsp::types::TextEdit;

/// One offer, reduced to what bi acts on. Built from the wire item once, so
/// the per-keystroke filter never touches JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// What an accept inserts — snippet syntax already collapsed to text.
    pub insert: String,
    /// The LSP kind number, drawn as a one-char badge.
    pub kind: Option<u8>,
    pub detail: Option<String>,
    /// Matched against; the label when the server gave nothing better.
    pub filter: String,
    /// Ordered by within a match bucket; ditto.
    pub sort: String,
    /// rust-analyzer's auto-imports. Wire ranges, converted at accept.
    pub extra_edits: Vec<TextEdit>,
}

pub struct Completion {
    items: Vec<Item>,
    /// Indices into `items`, best first. Recomputed whole per keystroke —
    /// the picker's judgement, for the picker's reason.
    matches: Vec<usize>,
    /// Index into `matches`.
    selected: usize,
    scroll: usize,
    /// The chars an accept replaces: word start .. cursor. The end moves
    /// with the cursor; the start is where the word began when the menu
    /// opened.
    pub replace: Range<usize>,
    /// The server wants re-asking as the word grows, rather than trusting
    /// local narrowing.
    pub incomplete: bool,
    /// Which ask produced these items — stale answers die against it.
    pub request: u64,
    /// The column unit the server counts in, for converting `extra_edits`.
    pub encoding: Encoding,
}

impl Completion {
    pub fn new(
        items: Vec<Item>,
        replace: Range<usize>,
        incomplete: bool,
        request: u64,
        encoding: Encoding,
    ) -> Self {
        Self {
            matches: (0..items.len()).collect(),
            items,
            selected: 0,
            scroll: 0,
            replace,
            incomplete,
            request,
            encoding,
        }
    }

    /// Re-ranks against `word`: prefix matches first, subsequence matches
    /// after, each bucket by the server's sort text — the cheap rule that
    /// puts `pos` before `expose_position` when `pos` was typed. Case folds,
    /// like the picker. Resets the selection: the old one pointed into a
    /// list that no longer exists.
    pub fn refilter(&mut self, word: &str) {
        let word = word.to_lowercase();
        let mut prefixed: Vec<usize> = Vec::new();
        let mut scattered: Vec<usize> = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let filter = item.filter.to_lowercase();
            if filter.starts_with(&word) {
                prefixed.push(i);
            } else if subsequence(&word, &filter) {
                scattered.push(i);
            }
        }
        let by_sort = |list: &mut Vec<usize>| {
            list.sort_by(|&a, &b| self.items[a].sort.cmp(&self.items[b].sort));
        };
        by_sort(&mut prefixed);
        by_sort(&mut scattered);
        prefixed.extend(scattered);
        self.matches = prefixed;
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// The offers still standing, best first.
    pub fn matches(&self) -> impl Iterator<Item = &Item> {
        self.matches.iter().map(|&i| &self.items[i])
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.matches.get(self.selected).map(|&i| &self.items[i])
    }

    /// `Ctrl-N` / `Ctrl-P`, wrapping — a two-item list is a toggle.
    pub fn shift(&mut self, forward: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.selected = match forward {
            true => (self.selected + 1) % n,
            false => (self.selected + n - 1) % n,
        };
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Keeps the selection on screen, the way every list in bi does.
    pub fn scroll_to_selected(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }
}

/// Whether every char of `needle` appears in `haystack`, in order. Both
/// already lowercased by the caller.
fn subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|c| chars.any(|h| h == c))
}

/// A snippet collapsed to the text it would leave behind: `${1:x}` → `x`,
/// `${1|a,b|}` → `a`, `$1` and `$0` → nothing, `\$` → `$`. Expansion with
/// tab-stops is a later feature; until then the text is the honest subset.
pub fn strip_snippet(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // An escaped char stands for itself; a trailing backslash is
                // a backslash.
                out.push(chars.next().unwrap_or('\\'));
            }
            '$' => match chars.peek() {
                // `$1` — a bare tab-stop leaves nothing behind.
                Some(d) if d.is_ascii_digit() => {
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                }
                Some('{') => {
                    chars.next();
                    // `${1:placeholder}`, `${1|first,rest|}`, `${1}`. The
                    // digits go; what is kept depends on what follows them.
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                    match chars.peek() {
                        Some(':') => {
                            chars.next();
                            // The placeholder, which may nest — `${1:${2:x}}`
                            // — so braces are counted.
                            let mut depth = 1;
                            let mut inner = String::new();
                            for c in chars.by_ref() {
                                match c {
                                    '{' => depth += 1,
                                    '}' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                                inner.push(c);
                            }
                            out.push_str(&strip_snippet(&inner));
                        }
                        Some('|') => {
                            chars.next();
                            // The first choice stands for the lot.
                            let mut first = true;
                            for c in chars.by_ref() {
                                match c {
                                    ',' => first = false,
                                    '|' => {
                                        // The closing `|}`.
                                        chars.next();
                                        break;
                                    }
                                    _ if first => out.push(c),
                                    _ => {}
                                }
                            }
                        }
                        // `${1}` or something malformed — swallow to the
                        // closing brace and keep nothing.
                        _ => {
                            for c in chars.by_ref() {
                                if c == '}' {
                                    break;
                                }
                            }
                        }
                    }
                }
                // A `$` that opens no snippet syntax is a dollar sign.
                _ => out.push('$'),
            },
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, sort: &str) -> Item {
        Item {
            label: label.into(),
            insert: label.into(),
            kind: None,
            detail: None,
            filter: label.into(),
            sort: sort.into(),
            extra_edits: Vec::new(),
        }
    }

    fn labels(c: &Completion) -> Vec<&str> {
        c.matches().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn prefix_matches_outrank_subsequence_matches() {
        let items = vec![
            item("expose_position", "b"),
            item("pos", "c"),
            item("position_of", "a"),
            item("unrelated", "d"),
        ];
        let mut c = Completion::new(items, 0..0, false, 1, Encoding::Utf8);
        c.refilter("pos");
        assert_eq!(
            labels(&c),
            ["position_of", "pos", "expose_position"],
            "prefix bucket by sort text, then the subsequence bucket"
        );
    }

    #[test]
    fn the_empty_word_keeps_everything_in_sort_order() {
        let items = vec![item("b_field", "2"), item("a_method", "1")];
        let mut c = Completion::new(items, 0..0, false, 1, Encoding::Utf8);
        c.refilter("");
        assert_eq!(labels(&c), ["a_method", "b_field"]);
    }

    #[test]
    fn filtering_is_case_insensitive_and_resets_the_selection() {
        let items = vec![item("Position", "a"), item("POST", "b"), item("nope", "c")];
        let mut c = Completion::new(items, 0..0, false, 1, Encoding::Utf8);
        c.shift(true);
        assert_eq!(c.selected(), 1);
        c.refilter("pos");
        assert_eq!(labels(&c), ["Position", "POST"]);
        assert_eq!(c.selected(), 0, "the old selection pointed into a dead list");
    }

    #[test]
    fn shifting_wraps_both_ways() {
        let items = vec![item("a", "1"), item("b", "2")];
        let mut c = Completion::new(items, 0..0, false, 1, Encoding::Utf8);
        c.refilter("");
        c.shift(true);
        assert_eq!(c.selected_item().unwrap().label, "b");
        c.shift(true);
        assert_eq!(c.selected_item().unwrap().label, "a", "wrapped");
        c.shift(false);
        assert_eq!(c.selected_item().unwrap().label, "b", "wrapped back");
    }

    #[test]
    fn snippets_collapse_to_their_text() {
        assert_eq!(strip_snippet("println!(\"$1\")$0"), "println!(\"\")");
        assert_eq!(strip_snippet("${1:name}: ${2:Type}"), "name: Type");
        assert_eq!(strip_snippet("${1|first,second,third|}"), "first");
        assert_eq!(strip_snippet("${1:outer ${2:inner}}"), "outer inner");
        assert_eq!(strip_snippet("cost: \\$5"), "cost: $5");
        assert_eq!(strip_snippet("plain text"), "plain text");
        assert_eq!(strip_snippet("US$ and ${x}"), "US$ and ", "malformed swallows to the brace");
    }

    #[test]
    fn scrolling_follows_the_selection() {
        let items: Vec<Item> =
            (0..10).map(|i| item(&format!("i{i}"), &format!("{i:02}"))).collect();
        let mut c = Completion::new(items, 0..0, false, 1, Encoding::Utf8);
        c.refilter("");
        for _ in 0..6 {
            c.shift(true);
        }
        c.scroll_to_selected(4);
        assert_eq!(c.scroll(), 3, "the selection is the last visible row");
        c.selected = 1;
        c.scroll_to_selected(4);
        assert_eq!(c.scroll(), 1, "and scrolls back up when it leaves the top");
    }
}
