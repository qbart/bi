//! The `:` line: its text, its cursor, and where a history walk has got to.
//!
//! It was a `String` you could append to and backspace from, which is every
//! prompt written before readline. See `docs/specs/cmdline.md`.

/// A command line being typed.
///
/// Derefs to `str` and displays as one, so the ex parser, the history and the
/// renderer read it exactly as they read the `String` it replaced.
#[derive(Debug, Clone, Default, Eq)]
pub struct CmdLine {
    text: String,
    /// Characters, not bytes: the cursor is a screen position, and a byte
    /// index would let it land inside a `ü`.
    at: usize,
    /// How far back through the history `Up` has walked, and what was on the
    /// line before it started.
    recall: Option<(usize, String)>,
}

/// By hand, over the text and the cursor only.
///
/// Where a history walk has got to is not a fact about what the command line
/// *is* — two lines showing the same text with the cursor in the same place
/// are the same line — and comparing it would make every
/// `Mode::Command("w".into())` assertion depend on state the test never set.
impl PartialEq for CmdLine {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.at == other.at
    }
}

/// Against plain text, the cursor is not part of the question: "is the line
/// `rename a b`" is asking what it says, not where you are in it.
impl PartialEq<str> for CmdLine {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<String> for CmdLine {
    fn eq(&self, other: &String) -> bool {
        self.text == *other
    }
}

impl From<String> for CmdLine {
    fn from(text: String) -> Self {
        let at = text.chars().count();
        Self { text, at, recall: None }
    }
}

impl From<&str> for CmdLine {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}

impl std::ops::Deref for CmdLine {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for CmdLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl CmdLine {
    /// Which column the cursor is in, counted in characters from the start of
    /// the line — what the frontend adds its own prompt width to.
    pub fn cursor(&self) -> usize {
        self.at
    }

    /// The byte offset `at` names, for the two edits that need one.
    fn byte(&self) -> usize {
        self.text.char_indices().nth(self.at).map_or(self.text.len(), |(i, _)| i)
    }

    pub fn insert(&mut self, c: char) {
        let byte = self.byte();
        self.text.insert(byte, c);
        self.at += 1;
        // Typing into a recalled line makes it yours, and the draft goes with
        // the walk: the line you are looking at is the line you meant.
        self.recall = None;
    }

    /// Deletes the character before the cursor. `false` when there was none —
    /// which on an empty line is how `Backspace` leaves command mode, and on a
    /// line with text on it is how it does nothing at column 0.
    pub fn backspace(&mut self) -> bool {
        self.recall = None;
        if self.at == 0 {
            return false;
        }
        self.at -= 1;
        let byte = self.byte();
        self.text.remove(byte);
        true
    }

    pub fn left(&mut self) {
        self.at = self.at.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.at = (self.at + 1).min(self.text.chars().count());
    }

    pub fn home(&mut self) {
        self.at = 0;
    }

    pub fn end(&mut self) {
        self.at = self.text.chars().count();
    }

    /// Replaces the whole line, cursor at the end. What the history picker
    /// does on Enter.
    pub fn set(&mut self, text: String) {
        *self = text.into();
    }

    /// Steps through `history`, which is newest first.
    ///
    /// The first step back saves what was typed; stepping forward past the
    /// newest entry puts it back exactly as it was. Neither end wraps —
    /// wrapping a list you cannot see turns "I have gone too far" into "I am
    /// somewhere else now".
    pub fn recall(&mut self, history: &[String], older: bool) {
        let next = match (self.recall.as_ref(), older) {
            (None, true) => 0,
            (None, false) => return,
            (Some((i, _)), true) => i + 1,
            (Some((0, draft)), false) => {
                let draft = draft.clone();
                self.recall = None;
                let at = draft.chars().count();
                self.text = draft;
                self.at = at;
                return;
            }
            (Some((i, _)), false) => i - 1,
        };
        let Some(line) = history.get(next) else { return };

        let draft = match self.recall.take() {
            Some((_, draft)) => draft,
            None => std::mem::take(&mut self.text),
        };
        self.text = line.clone();
        self.at = self.text.chars().count();
        self.recall = Some((next, draft));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> CmdLine {
        text.into()
    }

    #[test]
    fn typing_lands_at_the_cursor() {
        let mut line = line("wq");
        line.left();
        line.insert('x');

        assert_eq!(&*line, "wxq");
        assert_eq!(line.cursor(), 2);
    }

    #[test]
    fn backspace_takes_the_character_before_the_cursor() {
        let mut line = line("wq");
        line.left();
        assert!(line.backspace());

        assert_eq!(&*line, "q");
        assert_eq!(line.cursor(), 0);
        assert!(!line.backspace(), "and stops at the start rather than wrapping");
        assert_eq!(&*line, "q");
    }

    #[test]
    fn the_ends_stay_put() {
        let mut line = line("ab");
        line.home();
        line.left();
        assert_eq!(line.cursor(), 0);
        line.end();
        line.right();
        assert_eq!(line.cursor(), 2);
    }

    #[test]
    fn a_multibyte_line_moves_by_characters() {
        let mut line = line("süß");
        line.left();
        line.insert('!');

        assert_eq!(&*line, "sü!ß");
    }

    #[test]
    fn up_walks_back_and_down_returns_the_draft() {
        let history = ["w".to_string(), "q".to_string()];
        let mut line = line("half");

        line.recall(&history, true);
        assert_eq!(&*line, "w");
        assert_eq!(line.cursor(), 1, "cursor at the end of what it put there");

        line.recall(&history, true);
        assert_eq!(&*line, "q", "and on to the one before it");

        line.recall(&history, true);
        assert_eq!(&*line, "q", "the oldest entry does not wrap");

        line.recall(&history, false);
        assert_eq!(&*line, "w");
        line.recall(&history, false);
        assert_eq!(&*line, "half", "past the newest is what you were typing");
        line.recall(&history, false);
        assert_eq!(&*line, "half", "and stays there");
    }

    #[test]
    fn an_empty_history_leaves_the_line_alone() {
        let mut line = line("half");
        line.recall(&[], true);

        assert_eq!(&*line, "half");
    }

    #[test]
    fn typing_ends_the_walk() {
        let history = ["w".to_string()];
        let mut line = line("half");
        line.recall(&history, true);
        line.insert('a');

        assert_eq!(&*line, "wa");
        line.recall(&history, false);
        assert_eq!(&*line, "wa", "the draft went with the walk");
    }

    /// Two lines showing the same thing are the same line, whatever either of
    /// them was doing before.
    #[test]
    fn the_walk_is_not_part_of_equality() {
        let history = ["w".to_string()];
        let mut walked = line("");
        walked.recall(&history, true);

        assert_eq!(walked, line("w"));
    }
}
