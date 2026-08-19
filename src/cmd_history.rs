//! What you have run on the `:` line, newest first.
//!
//! Deliberately not in `history.rs`: that is the undo tree, and the two share a
//! word and nothing else. This is a list of strings with one rule about
//! duplicates and one about size, and it stays that small.
//!
//! Storage only — it does not search. `Ctrl-R` hands the lines to a [`Picker`],
//! which is where filtering already lives. See `docs/specs/cmdline-history.md`.
//!
//! [`Picker`]: crate::picker::Picker

/// Enough that nothing you would go looking for falls off, small enough that
/// the whole list stays scannable.
const CAPACITY: usize = 200;

#[derive(Debug, Clone)]
pub struct History {
    /// Front is most recent, matching the register ring: recency is the
    /// ranking, and the picker keeps the order it is given.
    lines: Vec<String>,
    capacity: usize,
}

impl Default for History {
    fn default() -> Self {
        Self { lines: Vec::new(), capacity: CAPACITY }
    }
}

impl History {
    /// Records a line, newest first.
    ///
    /// Blank lines are dropped — `:` then `Enter` did nothing, and a history
    /// full of empty rows is a history you scroll past.
    ///
    /// Dedupe is by move-to-front, as the register ring's is: running `w` forty
    /// times leaves one entry rather than forty, which is what keeps the list
    /// worth searching.
    pub fn push(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        self.lines.retain(|held| held != line);
        self.lines.insert(0, line.to_string());
        self.lines.truncate(self.capacity);
    }

    /// Newest first. What the picker is built over.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(lines: &[&str]) -> History {
        let mut history = History::default();
        for line in lines {
            history.push(line);
        }
        history
    }

    #[test]
    fn the_last_line_run_is_at_the_front() {
        let history = history(&["w", "q"]);
        assert_eq!(history.lines(), ["q", "w"]);
    }

    #[test]
    fn running_a_line_again_moves_it_up_rather_than_repeating_it() {
        let history = history(&["w", "ls", "w"]);
        assert_eq!(history.lines(), ["w", "ls"], "one entry, at the front");
    }

    #[test]
    fn a_blank_line_is_not_a_command_and_is_not_recorded() {
        let history = history(&["", "   ", "\t"]);
        assert!(history.is_empty());
    }

    #[test]
    fn the_oldest_line_falls_off_the_end() {
        let mut history = History { lines: Vec::new(), capacity: 2 };
        for line in ["a", "b", "c"] {
            history.push(line);
        }
        assert_eq!(history.lines(), ["c", "b"], "\"a\" was the oldest");
    }
}
