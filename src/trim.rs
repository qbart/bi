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
    /// Off by default, unlike `first_line`, and the asymmetry is deliberate: a
    /// blank line at the top of a file is a mistake in every language there
    /// is, and blank lines at the bottom are load-bearing often enough that
    /// removing them would be bi silently editing data.
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
            last_line: false,
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
    fn the_master_switch_stands_in_for_all_four() {
        let all_off =
            Trim { trailing: false, first_line: false, last_line: false, ..Trim::default() };
        assert!(!all_off.does_anything());
        assert!(!Trim { on_write: false, ..Trim::default() }.does_anything());
        assert!(Trim::default().does_anything());
    }
}
