//! A modal overlay that filters a list and returns a choice.
//!
//! State only — it does not draw. `ui.rs` reads what it holds and renders it.
//! That split is what makes the whole state machine testable without a
//! terminal; if this owned widgets, the only way to exercise it would be to
//! render it, which means it would not get exercised.
//!
//! Deliberately knows nothing about registers. They are its first client, not
//! its purpose — file finding and buffer switching want the same widget.

/// What the register ring hides until asked for: exactly the single-character
/// `x` deletes that would otherwise bury the list.
///
/// A per-picker length rather than a rule for everyone. On a command history it
/// would hide `w`, `q` and `x` — the shortest commands there are and the ones
/// typed most often — so history and the buffer list pass 0 instead.
pub const REGISTER_MIN_LEN: usize = 2;

pub struct Item {
    /// Matched against, previewed, and the row label is its first line. One
    /// field because for registers all three are the same string, and holding a
    /// 64 MiB ring three times over is not free.
    pub text: String,
    /// A one-character tag for the row — `¶` marks a linewise register entry,
    /// so you can see whether pasting will open a line before you commit.
    pub badge: Option<char>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// `before` mirrors `p` versus `P`.
    Register { before: bool },
    /// `:ls` over the open buffers, in list order.
    ///
    /// Vim prints a table you read a number out of and retype into `:b`; here
    /// the list is the chooser, and the number vim makes you carry never has to
    /// exist. The cost is that `:ls` has nothing to say to a script.
    Buffer,
    /// `Ctrl-P` — every file under the session's root.
    ///
    /// The one kind that matches by subsequence rather than by terms: `sfr`
    /// should find `src/find/render.rs`, which is exactly what terms cannot
    /// do. See `docs/specs/files.md`.
    File,
    /// `Ctrl-R` on the `:` line, over the lines you have run.
    ///
    /// The one kind that does not act on what you choose: it puts the line back
    /// on the command line for you to edit, because a history you cannot fix a
    /// typo in is a history that only helps when you were already right. See
    /// `docs/specs/cmdline-history.md`.
    History,
}

impl PickerKind {
    /// Whether the preview pane earns its third of the overlay.
    ///
    /// It exists to show a register entry longer than its row. A command line
    /// is one line and is already the row, so previewing it would show the same
    /// text twice and take the space from the list to do it.
    pub fn wants_preview(&self) -> bool {
        !matches!(self, PickerKind::History | PickerKind::File)
    }

    /// Whether typed characters have to appear *in order* rather than as
    /// whole terms.
    ///
    /// A file list is the one place the lax rule is the useful one. Over
    /// prose — which is what a register holds — "these letters appear in
    /// order" matches nearly everything, which is why it is not the default.
    fn subsequence(&self) -> bool {
        matches!(self, PickerKind::File)
    }
}

pub struct Picker {
    pub kind: PickerKind,
    items: Vec<Item>,
    query: String,
    /// Indices into `items`, in the order they were given. Recomputed whole on
    /// every keystroke — at a few thousand short entries that is far cheaper
    /// than the machinery to avoid it.
    matches: Vec<usize>,
    /// Index into `matches`, not into `items`.
    selected: usize,
    /// First visible row of the list.
    scroll: usize,
    /// Entries shorter than this are hidden until `show_short` asks for them.
    /// Zero hides nothing, which is what a list of commands or file names
    /// wants — see [`REGISTER_MIN_LEN`].
    min_len: usize,
    show_short: bool,
}

/// Case-insensitive, every whitespace-separated term must appear somewhere.
///
/// Not subsequence matching: register entries are prose and code, where "these
/// letters appear in order" matches nearly everything. An empty query has no
/// terms, so it matches all.
fn matches_query(text: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let hay = text.to_lowercase();
    query.split_whitespace().all(|term| hay.contains(&term.to_lowercase()))
}

/// Case-insensitive, every character in order but not necessarily together —
/// what a path list wants and what prose does not. Whitespace in the query is
/// ignored, so a stray space costs nothing.
fn matches_subsequence(text: &str, query: &str) -> bool {
    let mut chars = text.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .all(|wanted| chars.any(|c| c == wanted))
}

impl Picker {
    /// `min_len` hides entries shorter than it behind `Ctrl-A`. Zero hides
    /// nothing.
    pub fn new(kind: PickerKind, items: Vec<Item>, min_len: usize) -> Self {
        let mut picker = Self {
            kind,
            items,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            min_len,
            show_short: false,
        };
        picker.refilter();
        picker
    }

    /// Opens with the query already typed.
    ///
    /// What makes `Ctrl-R` on a half-written `:` line narrow the list to what
    /// you had started saying rather than asking you to say it again.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.refilter();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// Row within the match list, for highlighting.
    pub fn selected_row(&self) -> usize {
        self.selected
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Index into `items` of the highlighted entry, if anything matched.
    pub fn selected(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    pub fn preview(&self) -> &str {
        self.selected().map(|i| self.items[i].text.as_str()).unwrap_or("")
    }

    fn refilter(&mut self) {
        let (query, show_short) = (self.query.clone(), self.show_short);
        let min_len = self.min_len;
        let subsequence = self.kind.subsequence();
        self.matches = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| show_short || item.text.chars().count() >= min_len)
            .filter(|(_, item)| match subsequence {
                true => matches_subsequence(&item.text, &query),
                false => matches_query(&item.text, &query),
            })
            .map(|(i, _)| i)
            .collect();
        // Clamp rather than reset: narrowing the query should not throw away
        // where you were.
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }

    /// Returns false when there was nothing left to delete, which cancels —
    /// the same way backspacing off the end of a `:` line does.
    pub fn backspace(&mut self) -> bool {
        if self.query.pop().is_none() {
            return false;
        }
        self.refilter();
        true
    }

    pub fn next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.matches.len();
    }

    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
    }

    pub fn toggle_short(&mut self) {
        self.show_short = !self.show_short;
        self.refilter();
    }

    /// Keeps the selection inside a `height`-row window.
    pub fn scroll_to_selected(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        self.scroll = self.scroll.min(self.matches.len().saturating_sub(height));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker(texts: &[&str]) -> Picker {
        with_min_len(texts, REGISTER_MIN_LEN)
    }

    fn with_min_len(texts: &[&str], min_len: usize) -> Picker {
        let items = texts.iter().map(|t| Item { text: (*t).into(), badge: None }).collect();
        Picker::new(PickerKind::Register { before: false }, items, min_len)
    }

    fn files(texts: &[&str]) -> Picker {
        let items = texts.iter().map(|t| Item { text: (*t).into(), badge: None }).collect();
        Picker::new(PickerKind::File, items, 0)
    }

    fn shown(p: &Picker) -> Vec<&str> {
        p.matches().iter().map(|i| p.items()[*i].text.as_str()).collect()
    }

    fn type_query(p: &mut Picker, q: &str) {
        for c in q.chars() {
            p.push_char(c);
        }
    }

    /// The one kind that matches loosely, and the reason it is the one: `sfr`
    /// finding `src/find/render.rs` is exactly what terms cannot do.
    #[test]
    fn a_file_list_matches_a_subsequence() {
        let mut p = files(&["src/find/render.rs", "src/editor.rs", "README.md"]);
        type_query(&mut p, "sfr");
        assert_eq!(shown(&p), ["src/find/render.rs"]);

        let mut p = files(&["src/find/render.rs", "src/editor.rs"]);
        type_query(&mut p, "REND");
        assert_eq!(shown(&p), ["src/find/render.rs"], "and it ignores case");
    }

    /// Which is why it is not what a register does: over prose, "these letters
    /// appear in order" matches nearly everything.
    #[test]
    fn a_register_still_wants_whole_terms() {
        let mut p = picker(&["the quick brown fox", "sedimentary rock"]);
        type_query(&mut p, "sed");
        assert_eq!(shown(&p), ["sedimentary rock"], "not the first one, which has s, e and d");
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let p = picker(&["alpha", "beta", "gamma"]);
        assert_eq!(shown(&p), ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn typing_filters_to_substring_matches() {
        let mut p = picker(&["alpha", "beta", "gamma"]);
        type_query(&mut p, "a");
        assert_eq!(shown(&p), ["alpha", "beta", "gamma"]);
        type_query(&mut p, "m");
        assert_eq!(shown(&p), ["gamma"]);
    }

    /// Substring, not subsequence — "ala" appears in order in "alpha" but is
    /// not a substring, and matching it would make the picker useless on prose.
    #[test]
    fn matching_is_substring_not_subsequence() {
        let mut p = picker(&["alpha"]);
        type_query(&mut p, "ala");
        assert!(shown(&p).is_empty());
    }

    #[test]
    fn every_term_must_appear_but_order_does_not_matter() {
        let mut p = picker(&["fn retry(n)", "retry later", "fn main()"]);
        type_query(&mut p, "retry fn");
        assert_eq!(shown(&p), ["fn retry(n)"]);
    }

    #[test]
    fn matching_ignores_case() {
        let mut p = picker(&["Retry Later"]);
        type_query(&mut p, "retry");
        assert_eq!(shown(&p), ["Retry Later"]);
    }

    /// Recency is the ranking. The most recently captured thing is usually the
    /// one you want, and no relevance heuristic beats that for a clipboard.
    #[test]
    fn matches_keep_the_order_they_were_given() {
        let mut p = picker(&["third x", "second x", "first x"]);
        type_query(&mut p, "x");
        assert_eq!(shown(&p), ["third x", "second x", "first x"]);
    }

    #[test]
    fn selection_moves_and_wraps_at_both_ends() {
        let mut p = picker(&["one", "two", "three"]);
        assert_eq!(p.selected(), Some(0));
        p.next();
        assert_eq!(p.selected(), Some(1));
        p.prev();
        p.prev();
        assert_eq!(p.selected(), Some(2), "wrapped backwards to the end");
        p.next();
        assert_eq!(p.selected(), Some(0), "and forwards to the start");
    }

    #[test]
    fn a_shrinking_match_list_clamps_the_selection() {
        let mut p = picker(&["aaa", "aab", "abb"]);
        p.next();
        p.next();
        assert_eq!(p.selected(), Some(2));

        type_query(&mut p, "aa");
        assert_eq!(shown(&p), ["aaa", "aab"]);
        assert_eq!(p.selected(), Some(1), "clamped to the new end, not reset");
    }

    #[test]
    fn short_entries_are_hidden_until_asked_for() {
        let mut p = picker(&["x", "ab", "hello"]);
        assert_eq!(shown(&p), ["ab", "hello"], "one-char noise is out");

        p.toggle_short();
        assert_eq!(shown(&p), ["x", "ab", "hello"]);
        p.toggle_short();
        assert_eq!(shown(&p), ["ab", "hello"]);
    }

    /// `w` and `q` are the shortest commands there are and the most typed. A
    /// list that hid them would be hiding the rows it exists for.
    #[test]
    fn a_zero_threshold_hides_nothing() {
        let p = with_min_len(&["w", "q", "ls"], 0);
        assert_eq!(shown(&p), ["w", "q", "ls"]);
    }

    /// `Ctrl-R` on a half-typed `:` line opens already narrowed to it.
    #[test]
    fn a_seeded_query_filters_from_the_start() {
        let mut p = with_min_len(&["w out.txt", "q", "w"], 0);
        p.set_query("w".into());
        assert_eq!(p.query(), "w");
        assert_eq!(shown(&p), ["w out.txt", "w"]);
    }

    /// The one kind whose rows are already whole lines.
    #[test]
    fn only_the_history_goes_without_a_preview() {
        assert!(!PickerKind::History.wants_preview());
        assert!(PickerKind::Buffer.wants_preview());
        assert!(PickerKind::Register { before: false }.wants_preview());
    }

    #[test]
    fn backspacing_an_empty_query_asks_to_cancel() {
        let mut p = picker(&["one"]);
        p.push_char('o');
        assert!(p.backspace(), "deleted a char");
        assert!(!p.backspace(), "nothing left, so cancel");
    }

    #[test]
    fn nothing_matching_leaves_no_selection() {
        let mut p = picker(&["one", "two"]);
        type_query(&mut p, "zzz");
        assert!(shown(&p).is_empty());
        assert_eq!(p.selected(), None);
        assert_eq!(p.preview(), "");
        p.next();
        assert_eq!(p.selected(), None, "moving in an empty list is harmless");
    }

    #[test]
    fn the_preview_is_the_highlighted_entry() {
        let mut p = picker(&["one", "two"]);
        p.next();
        assert_eq!(p.preview(), "two");
    }

    #[test]
    fn scrolling_follows_the_selection() {
        let mut p = picker(&["a1", "b2", "c3", "d4", "e5"]);
        p.scroll_to_selected(2);
        assert_eq!(p.scroll(), 0);

        p.next();
        p.next();
        p.scroll_to_selected(2);
        assert_eq!(p.scroll(), 1, "selection at row 2 in a 2-row window");

        p.prev();
        p.prev();
        p.scroll_to_selected(2);
        assert_eq!(p.scroll(), 0);
    }
}
