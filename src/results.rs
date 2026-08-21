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

/// What a search found, as something a window can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Results {
    /// What the pane says it is: `find: needle`.
    pub title: String,
    /// What was searched for, so `:replace` can rewrite with the same engine
    /// that reported the matches — see [`crate::find_in_files::matcher`].
    pub query: crate::find_in_files::Query,
    /// The root every `Match::path` is relative to, so choosing a row can open
    /// the file without the pane having to store an absolute path per row.
    pub root: PathBuf,
    pub matches: Vec<Match>,
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
        let mut rows = Vec::new();
        let mut at = 0;
        while at < matches.len() {
            let path = &matches[at].path;
            let end = matches[at..].iter().take_while(|m| &m.path == path).count() + at;
            rows.push(Row::File { path: path.clone(), matches: end - at });
            rows.extend((at..end).map(|index| Row::Hit { index }));
            at = end;
        }
        Self { title, query, root, matches, rows, selected: 0, scroll: 0 }
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
            crate::find_in_files::Query::default(),
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
}
