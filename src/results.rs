//! The pane a search puts its answers in.
//!
//! A third [`crate::window::Content`] beside text and the file tree, which is
//! what `windows.md` said the next pane kind should be: a variant and a
//! compiler error rather than a second boolean.
//!
//! A pane rather than a picker overlay, because the list outlives the moment
//! you read it: you scroll it, you leave it open beside the file you are
//! editing, and you come back to it. That is also what makes it worth
//! building once — diagnostics, LSP references and git-grep all want this same
//! list, and each of them is a producer of [`Results`] rather than another
//! overlay to design.
//!
//! See `docs/specs/find-in-files.md`.

use std::path::PathBuf;

use crate::find_in_files::Match;

/// One row of the pane.
///
/// Files get a row of their own, so a result set reads as a file at a time
/// rather than repeating a path down the left margin — which is what makes a
/// hundred matches in six files legible at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A file, and how many matches are under it.
    File { path: PathBuf, matches: usize },
    /// One matching line. `index` is into [`Results::matches`], so choosing a
    /// row does not have to search for what it was about.
    Hit { index: usize },
}

/// An armed rewrite: what `:replace` is offering, before anything is applied.
///
/// The pane holds it because the pane is the review — every hit row shows its
/// line as it will read, and the pane's own keys (`a`, `A`, `x`) decide.
/// See `docs/specs/find-in-files.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replace {
    /// The replacement text, read the way the query's pattern was — literal
    /// under `:replace`, `$1` and friends under `:replace~`.
    pub with: String,
    /// One flag per [`Results::matches`] entry: whether `a` or `A` has taken
    /// it. Applied rows keep their ✓ as the record of what happened.
    applied: Vec<bool>,
}

/// What a search found, as something a window can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Results {
    /// What the pane says it is: `find: needle`.
    pub title: String,
    /// What was searched for, so a replace can rewrite with the same engine
    /// that reported the matches — see [`crate::find_in_files::matcher`].
    pub query: crate::find_in_files::Query,
    /// The root every `Match::path` is relative to, so choosing a row can open
    /// the file without the pane having to store an absolute path per row.
    pub root: PathBuf,
    pub matches: Vec<Match>,
    /// `Some` once `:replace` armed the pane. See [`Replace`].
    pub replace: Option<Replace>,
    rows: Vec<Row>,
    selected: usize,
    scroll: usize,
}

impl Results {
    /// Groups `matches` by file, in the order the files first appear.
    pub fn new(
        title: String,
        query: crate::find_in_files::Query,
        root: PathBuf,
        matches: Vec<Match>,
    ) -> Self {
        let rows = Self::grouped(&matches);
        Self { title, query, root, matches, replace: None, rows, selected: 0, scroll: 0 }
    }

    fn grouped(matches: &[Match]) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut at = 0;
        while at < matches.len() {
            let path = &matches[at].path;
            let end = matches[at..].iter().take_while(|m| &m.path == path).count() + at;
            rows.push(Row::File { path: path.clone(), matches: end - at });
            rows.extend((at..end).map(|index| Row::Hit { index }));
            at = end;
        }
        rows
    }

    /// Arms the pane: from here every hit row is an offer, and the title says
    /// what of.
    pub fn arm(&mut self, with: String) {
        self.title = format!("replace: {} → {}", self.query.pattern, with);
        self.replace = Some(Replace { with, applied: vec![false; self.matches.len()] });
    }

    /// Whether match `index` has been applied. False on an unarmed pane,
    /// where nothing can have been.
    pub fn is_applied(&self, index: usize) -> bool {
        self.replace.as_ref().is_some_and(|r| r.applied.get(index).copied().unwrap_or(false))
    }

    /// Records that `a` or `A` took these matches.
    pub fn mark_applied(&mut self, indices: &[usize]) {
        if let Some(replace) = &mut self.replace {
            for &i in indices {
                if let Some(flag) = replace.applied.get_mut(i) {
                    *flag = true;
                }
            }
        }
    }

    /// The matches the selected row is offering — the hit itself, or every
    /// hit under a heading. What `a` applies and `x` drops.
    pub fn selected_indices(&self) -> Vec<usize> {
        match self.rows.get(self.selected) {
            Some(Row::Hit { index }) => vec![*index],
            // The hits under *this* heading, not every match in this file: a
            // path that comes back after another one is two groups, and the
            // heading you pressed the key on is the one you meant.
            Some(Row::File { .. }) => self.rows[self.selected + 1..]
                .iter()
                .map_while(|row| match row {
                    Row::Hit { index } => Some(*index),
                    Row::File { .. } => None,
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Every match not yet applied. What `A` takes.
    pub fn pending(&self) -> Vec<usize> {
        (0..self.matches.len()).filter(|&i| !self.is_applied(i)).collect()
    }

    /// `x` — drops the selected row from the list: the hit, or a heading with
    /// everything under it. Edits nothing; narrows what the list is offering.
    pub fn remove_selected(&mut self) {
        let doomed = self.selected_indices();
        if doomed.is_empty() {
            return;
        }
        let keep = |i: &usize| !doomed.contains(i);
        self.matches = std::mem::take(&mut self.matches)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep(i))
            .map(|(_, m)| m)
            .collect();
        if let Some(replace) = &mut self.replace {
            replace.applied = std::mem::take(&mut replace.applied)
                .into_iter()
                .enumerate()
                .filter(|(i, _)| keep(i))
                .map(|(_, a)| a)
                .collect();
        }
        self.rows = Self::grouped(&self.matches);
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many files the matches are spread over.
    pub fn files(&self) -> usize {
        self.rows.iter().filter(|r| matches!(r, Row::File { .. })).count()
    }

    /// Moves the selection by `by` rows, clamped at either end.
    ///
    /// Clamped rather than wrapped, which is what the file tree does and for
    /// the same reason: a list you are reading top to bottom should stop at the
    /// bottom rather than silently put you back at the top.
    pub fn move_by(&mut self, by: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + by).clamp(0, last) as usize;
    }

    pub fn select(&mut self, row: usize) {
        self.selected = row.min(self.rows.len().saturating_sub(1));
    }

    /// What the selected row points at, if it is a match rather than a file
    /// heading.
    pub fn selected_match(&self) -> Option<&Match> {
        match self.rows.get(self.selected)? {
            Row::Hit { index } => self.matches.get(*index),
            Row::File { .. } => None,
        }
    }

    /// The file the selected row is under, heading or hit.
    ///
    /// A heading is a row you can press Enter on: you wanted that file, and
    /// the top of it is a perfectly good answer.
    pub fn selected_path(&self) -> Option<&PathBuf> {
        match self.rows.get(self.selected)? {
            Row::File { path, .. } => Some(path),
            Row::Hit { index } => Some(&self.matches.get(*index)?.path),
        }
    }

    /// `a` has taken its row; the selection walks on to the next hit still
    /// pending, so `a a a` reads down the list. Stays put when nothing ahead
    /// is pending.
    pub fn advance_to_pending(&mut self) {
        let next = self.rows[self.selected.min(self.rows.len().saturating_sub(1))..]
            .iter()
            .enumerate()
            .find_map(|(offset, row)| match row {
                Row::Hit { index } if !self.is_applied(*index) => Some(self.selected + offset),
                _ => None,
            });
        if let Some(row) = next {
            self.selected = row;
        }
    }

    /// Keeps the selection on screen for a pane `height` rows tall.
    pub fn scroll_to_selected(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, line: usize, text: &str) -> Match {
        Match { path: path.into(), line, col: 0, len: 1, text: text.into() }
    }

    fn results() -> Results {
        Results::new(
            "find: x".into(),
            crate::find_in_files::Query { pattern: "x".into(), ..Default::default() },
            "/root".into(),
            vec![hit("a.rs", 1, "one"), hit("a.rs", 4, "two"), hit("b.rs", 9, "three")],
        )
    }

    #[test]
    fn matches_are_grouped_under_the_file_they_are_in() {
        let r = results();
        assert_eq!(
            r.rows(),
            [
                Row::File { path: "a.rs".into(), matches: 2 },
                Row::Hit { index: 0 },
                Row::Hit { index: 1 },
                Row::File { path: "b.rs".into(), matches: 1 },
                Row::Hit { index: 2 },
            ]
        );
        assert_eq!(r.files(), 2);
    }

    #[test]
    fn a_heading_names_a_file_and_a_hit_names_a_line() {
        let mut r = results();
        assert_eq!(r.selected_match(), None, "the first row is a heading");
        assert_eq!(r.selected_path(), Some(&PathBuf::from("a.rs")), "and it still names a file");

        r.move_by(1);
        assert_eq!(r.selected_match().map(|m| m.line), Some(1));
    }

    #[test]
    fn the_selection_stops_at_either_end() {
        let mut r = results();
        r.move_by(-5);
        assert_eq!(r.selected(), 0);
        r.move_by(100);
        assert_eq!(r.selected(), 4, "the last row, not back to the top");
    }

    #[test]
    fn an_empty_result_set_has_nothing_to_select() {
        let mut r = Results::new(
            "find: nothing".into(),
            crate::find_in_files::Query::default(),
            "/root".into(),
            Vec::new(),
        );
        assert!(r.is_empty());
        r.move_by(1);
        assert_eq!(r.selected_match(), None);
        assert_eq!(r.selected_path(), None);
    }

    #[test]
    fn scrolling_follows_the_selection_in_both_directions() {
        let mut r = results();
        r.select(4);
        r.scroll_to_selected(2);
        assert_eq!(r.scroll(), 3, "the last two rows");

        r.select(0);
        r.scroll_to_selected(2);
        assert_eq!(r.scroll(), 0);
    }

    #[test]
    fn one_file_appearing_twice_is_two_groups() {
        // The walk yields files in some order and matches within a file in
        // line order; grouping follows the list rather than sorting it, so a
        // path that comes back after another one is honestly two headings
        // rather than silently merged into one that lies about its count.
        let r = Results::new(
            "find: x".into(),
            crate::find_in_files::Query::default(),
            "/root".into(),
            vec![hit("a.rs", 1, "one"), hit("b.rs", 1, "two"), hit("a.rs", 2, "three")],
        );
        assert_eq!(r.files(), 3);
    }

    #[test]
    fn x_on_a_hit_drops_it_and_the_heading_count_follows() {
        let mut r = results();
        r.select(1);
        r.remove_selected();
        assert_eq!(
            r.rows(),
            [
                Row::File { path: "a.rs".into(), matches: 1 },
                Row::Hit { index: 0 },
                Row::File { path: "b.rs".into(), matches: 1 },
                Row::Hit { index: 1 },
            ]
        );
        assert_eq!(r.matches()[0].line, 4, "the second hit survived, renumbered");
    }

    #[test]
    fn x_on_a_heading_drops_the_file_under_it() {
        let mut r = results();
        r.remove_selected();
        assert_eq!(
            r.rows(),
            [Row::File { path: "b.rs".into(), matches: 1 }, Row::Hit { index: 0 },]
        );
    }

    #[test]
    fn a_heading_offers_its_own_group_not_its_path() {
        // One file appearing twice is two groups, and the heading you press
        // the key on is the one you meant.
        let mut r = Results::new(
            "find: x".into(),
            crate::find_in_files::Query::default(),
            "/root".into(),
            vec![hit("a.rs", 1, "one"), hit("b.rs", 1, "two"), hit("a.rs", 2, "three")],
        );
        r.select(0);
        assert_eq!(r.selected_indices(), [0], "the first a.rs group only");
        r.select(4);
        assert_eq!(r.selected_indices(), [2], "and the second is its own");
    }

    #[test]
    fn arming_offers_everything_and_marks_stick() {
        let mut r = results();
        r.arm("pin".into());
        assert_eq!(r.title, "replace: x → pin");
        assert_eq!(r.pending(), [0, 1, 2]);

        r.mark_applied(&[1]);
        assert!(r.is_applied(1));
        assert_eq!(r.pending(), [0, 2]);
    }

    #[test]
    fn marks_follow_their_matches_through_a_prune() {
        let mut r = results();
        r.arm("pin".into());
        r.mark_applied(&[2]);

        r.select(1);
        r.remove_selected();

        assert_eq!(r.pending(), [0], "the old third match is the new second, still ✓");
        assert!(r.is_applied(1));
    }

    #[test]
    fn the_selection_walks_to_the_next_pending_offer() {
        let mut r = results();
        r.arm("pin".into());
        r.select(1);
        r.mark_applied(&[0]);
        r.advance_to_pending();
        assert_eq!(r.selected(), 2, "the next hit still pending");

        r.mark_applied(&[1, 2]);
        r.advance_to_pending();
        assert_eq!(r.selected(), 2, "nothing ahead, so it stays put");
    }
}
