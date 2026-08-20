//! What a write tidies up on its way out.
//!
//! Trailing whitespace is invisible, and every project has a hook, a linter or
//! a reviewer whose job is to notice it anyway. Removing it here removes all
//! three.
//!
//! The settings and the scanning live here; the edits are
//! [`crate::buffer::Buffer::trim`], because the rope is private and every
//! mutation goes through one door. See `docs/specs/trim.md`.

/// The trimming settings, bundled for the code that edits text — the same
/// shape and for the same reason as [`crate::indent::Indent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trim {
    /// The master switch. Off, nothing below happens, whatever it says:
    /// "leave this repository's files exactly as you found them" is a real
    /// sentence and should not mean turning four options off one at a time.
    pub on_write: bool,
    pub trailing: bool,
    pub first_line: bool,
    /// Blank lines at the bottom. On, like `first_line`: a run of empty lines
    /// at the end of a file is the same accident at the other end of it, and
    /// leaving them there means every write hands the reviewer a diff of
    /// nothing.
    pub last_line: bool,
    /// Only ever *adds* one. Removing extra newlines at the end is
    /// `last_line`'s job, and keeping them apart is what lets a project ask
    /// for one without the other.
    pub final_newline: bool,
}

impl Default for Trim {
    fn default() -> Self {
        Self {
            on_write: true,
            trailing: true,
            first_line: true,
            last_line: true,
            final_newline: false,
        }
    }
}

impl Trim {
    /// Whether any of it would do anything, so a write can skip the whole pass.
    pub fn does_anything(&self) -> bool {
        self.on_write && (self.trailing || self.first_line || self.last_line || self.final_newline)
    }
}

/// How many characters of trailing space and tab `line` ends with.
///
/// A count rather than a slice: the caller is turning it into a char range in
/// a rope, and the two are the same number here because both characters are
/// one byte and one column.
pub fn trailing(line: &str) -> usize {
    line.chars().rev().take_while(|c| *c == ' ' || *c == '\t').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_counts_spaces_and_tabs_and_nothing_else() {
        assert_eq!(trailing("code  \t "), 4);
        assert_eq!(trailing("code"), 0);
        assert_eq!(trailing("   "), 3, "a blank line is all of it");
        assert_eq!(trailing(""), 0);
    }

    #[test]
    fn blank_lines_go_from_both_ends_of_a_file_by_default() {
        let default = Trim::default();
        assert!(default.first_line);
        assert!(default.last_line);
    }

    #[test]
    fn the_master_switch_stands_in_for_all_four() {
        let all_off =
            Trim { trailing: false, first_line: false, last_line: false, ..Trim::default() };
        assert!(!all_off.does_anything());
        assert!(!Trim { on_write: false, ..Trim::default() }.does_anything());
        assert!(Trim::default().does_anything());
    }
}
