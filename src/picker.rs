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
    /// `"np` — the named registers, most recently named first. The row is
    /// the name; the entry rides in the preview. See `docs/specs/registers.md`.
    Named { before: bool },
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
    /// `gf` in a tree — every row it is showing, to jump the selection to one.
    ///
    /// The rows and not the filesystem, which is what separates it from
    /// [`PickerKind::File`]: this one moves the cursor inside the pane you are
    /// looking at, so it can only offer what that pane has on it. Directories
    /// included — a directory is a tree item, and `Ctrl-P` cannot reach one.
    /// See `docs/specs/tree.md`.
    TreeRow,
    /// `:symbols` — the declarations tree-sitter found in this file, to jump
    /// to one.
    ///
    /// Like [`PickerKind::TreeRow`] and unlike [`PickerKind::File`], it moves
    /// the cursor inside the pane you are looking at and opens nothing: the
    /// list is derived from the parse tree of the buffer already in front of
    /// you. See `docs/specs/symbols.md`.
    Symbol,
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
    /// It exists to show a register entry longer than its row, and only a
    /// register has one. A command line and a file name are one line and are
    /// already the row; a buffer is a name you know — you are switching *back*
    /// to it — so its first line says nothing you needed and costs the list a
    /// third of its rows to say it.
    pub fn wants_preview(&self) -> bool {
        matches!(self, PickerKind::Register { .. } | PickerKind::Named { .. })
    }

    /// Whether typed characters have to appear *in order* rather than as
    /// whole terms.
    ///
    /// A file list is the one place the lax rule is the useful one. Over
    /// prose — which is what a register holds — "these letters appear in
    /// order" matches nearly everything, which is why it is not the default.
    fn subsequence(&self) -> bool {
        matches!(self, PickerKind::File | PickerKind::Buffer | PickerKind::TreeRow)
    }

    /// Whether matches are sorted by how well they match, rather than kept in
    /// the order they were given.
    ///
    /// The lists whose given order answers nothing: a tree in filesystem
    /// order, a file walk in directory order. The register ring, the buffer
    /// list and the command history stay unranked because newest-first is
    /// already an answer to "which one did you mean" that a score would throw
    /// away. Unranked, `main` put `src/core/animation_curve.cpp` above
    /// `src/main.cpp` — the letters in order, alphabetically first, and not
    /// what anyone typing `main` meant. See `docs/specs/files.md`.
    ///
    /// The sort is stable, which is the other half of it: the caller puts the
    /// rows it would rather offer first, and they win every tie without
    /// winning an argument. See `docs/specs/tree.md`.
    fn ranked(&self) -> bool {
        matches!(self, PickerKind::TreeRow | PickerKind::File)
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
    /// Where the selection sits while nothing has been typed.
    ///
    /// One for the buffer list, which opens on the buffer you were in *last*
    /// so that one Enter goes back to it; zero for everything else, where the
    /// front of the list is the answer.
    default_row: usize,
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

/// How well `text` matches `query`, for the kinds that rank rather than keep
/// the order they were given.
///
/// Three weights and a tiebreak, and the numbers are the whole design:
///
/// - **8, consecutive** — a run of characters is what you meant. `mod` finding
///   `mod` beats `mod` finding `m`y `o`ther `d`irectory.
/// - **4, at a boundary** — the start of the text, or the first character of a
///   path segment or a word. `sfr` should find `src/find/render.rs`, and it
///   does because all three land on one.
/// - **1, anywhere else** — it counted, and that is all.
/// - **shorter wins a tie**, by a small subtraction rather than a rule, so a
///   deep path never loses to a short one it genuinely matched better.
///
/// Greedy from the best anchor. Greedy left to right is what
/// [`matches_subsequence`] already does, but anchored at the *first*
/// occurrence of the query's first character it never sees `main` sitting
/// whole in `domain/main.rs` — it spends the `m` inside `domain` and scores
/// the scatter. So every occurrence of the first character is a candidate
/// start, each is scored greedily from there, and the best one is the answer.
/// Still not every alignment of every character — no fuzzy finder that people
/// like does that — but the half of it that pays for itself.
fn score(text: &str, query: &str) -> i32 {
    let wanted: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    if wanted.is_empty() {
        return 0;
    }
    let chars: Vec<char> = text.chars().collect();
    let lowered: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();

    (0..chars.len())
        .filter(|&start| lowered[start] == wanted[0])
        .filter_map(|start| score_from(&chars, &lowered, &wanted, start))
        .max()
        .unwrap_or(0)
}

/// One greedy pass, anchored: the first character taken at `start`, the rest
/// wherever they next appear.
fn score_from(chars: &[char], lowered: &[char], wanted: &[char], start: usize) -> Option<i32> {
    let boundary = |i: usize| match i {
        0 => true,
        i => {
            let before = chars[i - 1];
            !before.is_alphanumeric() || (before.is_lowercase() && chars[i].is_uppercase())
        }
    };

    let (mut total, mut at, mut previous) = (0, start, None);
    for &want in wanted {
        let offset = lowered[at..].iter().position(|&c| c == want)?;
        let i = at + offset;
        total += match (previous == Some(i.wrapping_sub(1)), boundary(i)) {
            (true, _) => 8,
            (false, true) => 4,
            (false, false) => 1,
        };
        previous = Some(i);
        at = i + 1;
    }
    // A tiebreak, not a weight: it can separate two equal matches and can
    // never outrank one character landing where it should.
    Some(total - (chars.len() as i32) / 8)
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
            default_row: 0,
        };
        picker.refilter();
        picker
    }

    /// Opens on a row other than the first.
    ///
    /// Typing moves off it — the row you meant while the list was whole says
    /// nothing about the list once it is filtered — and backspacing back to an
    /// empty query returns to it.
    pub fn open_on(&mut self, row: usize) {
        self.default_row = row;
        self.selected = row.min(self.matches.len().saturating_sub(1));
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
        if self.kind.ranked() {
            // Stable, so equal scores keep the order they were given — which
            // is how the caller says which rows it would rather offer.
            self.matches.sort_by_key(|&i| -score(&self.items[i].text, &query));
        }
        // Clamp rather than reset: narrowing the query should not throw away
        // where you were.
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    pub fn push_char(&mut self, c: char) {
        let was_whole = self.query.is_empty();
        self.query.push(c);
        self.refilter();
        // The first character leaves the *default* row behind: it was a fact
        // about the whole list, and this is no longer the whole list. A row
        // you moved to yourself is not a default and is clamped like any
        // other, which is what every picker but the buffer list does.
        if was_whole && self.default_row != 0 && self.selected == self.default_row {
            self.selected = 0;
        }
    }

    /// Returns false when there was nothing left to delete, which cancels —
    /// the same way backspacing off the end of a `:` line does.
    pub fn backspace(&mut self) -> bool {
        if self.query.pop().is_none() {
            return false;
        }
        self.refilter();
        if self.query.is_empty() {
            self.selected = self.default_row.min(self.matches.len().saturating_sub(1));
        }
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

    /// The run beats the scatter: `main` appearing whole outranks its letters
    /// spread through `animation`, whatever the walk order said.
    #[test]
    fn a_file_list_ranks_the_run_above_the_scatter() {
        let mut p = files(&[
            "src/core/animation_curve.cpp",
            "src/core/animation_curve.hpp",
            "src/main.cpp",
        ]);
        type_query(&mut p, "main");
        assert_eq!(shown(&p)[0], "src/main.cpp");
    }

    /// The anchor is the best one, not the first one: greedy from the first
    /// `m` would spend it inside `domain` and never see the word after the
    /// slash.
    #[test]
    fn the_scorer_anchors_where_the_match_is_best() {
        let mut p = files(&["domain/main.rs", "dominic/other.rs"]);
        type_query(&mut p, "main");
        assert_eq!(shown(&p), ["domain/main.rs"]);
        assert!(
            score("domain/main.rs", "main") > score("dxoxmxain/list.rs", "main"),
            "the whole word outranks the scatter even behind a decoy prefix"
        );
    }

    #[test]
    fn equal_scores_keep_the_walk_order() {
        let mut p = files(&["a/same.rs", "b/same.rs", "c/same.rs"]);
        type_query(&mut p, "same");
        assert_eq!(shown(&p), ["a/same.rs", "b/same.rs", "c/same.rs"]);
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

    /// The three weights, each shown beating the one below it. The numbers
    /// themselves are arbitrary; what is not is the order they put things in.
    #[test]
    fn a_run_beats_a_boundary_beats_a_letter_that_merely_counted() {
        // `mod` as a run, `mod` as three path-segment starts, `mod` scattered.
        let run = score("a/module.rs", "mod");
        let boundaries = score("m/o/d.rs", "mod");
        let scattered = score("xmxoxdx", "mod");
        assert!(run > boundaries, "{run} vs {boundaries}");
        assert!(boundaries > scattered, "{boundaries} vs {scattered}");
        assert_eq!(score("nothing here", "mod"), 0, "and no match is no score");
    }

    /// The tiebreak, and the reason it is a subtraction rather than a rule: it
    /// separates two matches that are otherwise equal and can never outrank a
    /// character landing where it should.
    #[test]
    fn the_shorter_of_two_equal_matches_wins_and_only_just() {
        assert!(score("thing.rs", "thing") > score("a/very/deep/thing.rs", "thing"));
        // A boundary hit is worth more than the whole length penalty here.
        assert!(score("src/thing.rs", "sthing") > score("something.rs", "sthing"));
    }

    #[test]
    fn an_empty_query_scores_everything_the_same() {
        assert_eq!(score("anything", ""), 0);
        assert_eq!(score("", ""), 0);
    }

    /// The order the caller gave survives every tie, which is what makes a
    /// stable sort the whole of "prefer these".
    #[test]
    fn ranking_keeps_the_given_order_where_the_scores_agree() {
        let items = ["b/x.rs", "a/x.rs", "c/x.rs"]
            .iter()
            .map(|t| Item { text: (*t).into(), badge: None })
            .collect();
        let mut p = Picker::new(PickerKind::TreeRow, items, 0);
        p.set_query("x.rs".into());

        let shown: Vec<&str> = p.matches().iter().map(|&i| p.items()[i].text.as_str()).collect();
        assert_eq!(shown, ["b/x.rs", "a/x.rs", "c/x.rs"], "equal matches, untouched");
    }

    /// And a better match moves past them, which is the other half.
    #[test]
    fn ranking_puts_the_better_match_first_however_it_was_given() {
        let items = ["zzz/other.rs", "exact.rs"]
            .iter()
            .map(|t| Item { text: (*t).into(), badge: None })
            .collect();
        let mut p = Picker::new(PickerKind::TreeRow, items, 0);
        p.set_query("exact".into());

        assert_eq!(p.items()[p.selected().unwrap()].text, "exact.rs");
    }

    /// The lists whose order is an answer keep it: newest-first is what the
    /// register ring, the buffer list and the history are telling you.
    #[test]
    fn only_the_orderless_lists_are_ranked() {
        assert!(PickerKind::TreeRow.ranked());
        assert!(PickerKind::File.ranked(), "the walk order answers nothing");
        for kind in
            [PickerKind::Buffer, PickerKind::History, PickerKind::Register { before: false }]
        {
            assert!(!kind.ranked(), "{kind:?} is newest-first, which is already an answer");
        }
    }

    /// The one kind whose rows do not already say what you are choosing.
    #[test]
    fn only_a_register_earns_a_preview() {
        assert!(PickerKind::Register { before: false }.wants_preview());
        assert!(!PickerKind::History.wants_preview());
        assert!(!PickerKind::File.wants_preview());
        assert!(!PickerKind::Buffer.wants_preview(), "a list, like Ctrl-P");
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
