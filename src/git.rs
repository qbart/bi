//! Minimal git awareness: which lines differ from the index, and by how much.
//!
//! A sign per touched row for the gutter, and a numstat for the status row —
//! deliberately nothing more. The diff is computed here, in process; the
//! baseline arrives through a loader the frontend installs
//! ([`crate::editor::Editor::set_git_baseline`]), so the core never runs git
//! and an embedder without a repository pays nothing.
//!
//! See `docs/specs/git-signs.md`.

use std::path::Path;
use std::process::{Command, Stdio};

/// One row's mark: what happened to it since the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Add,
    Change,
    /// Lines gone from under this row.
    Delete,
    /// The file's first lines are gone; worn by row zero, where "under" would
    /// point at nothing.
    DeleteTop,
}

impl Sign {
    /// The mark the sign puts in the gutter cell.
    pub fn glyph(self) -> char {
        match self {
            Sign::Add | Sign::Change => '▎',
            Sign::Delete => '▁',
            Sign::DeleteTop => '‾',
        }
    }
}

/// The numstat: lines added, changed and removed, buffer against baseline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
}

impl Stats {
    pub fn is_clean(&self) -> bool {
        *self == Self::default()
    }
}

/// A diff, as the drawing wants it: at most one sign per row, rows ascending,
/// and the totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub signs: Vec<(usize, Sign)>,
    pub stats: Stats,
}

/// Lines of `current` against lines of `baseline`.
///
/// A replacement of unequal size counts the overlap as changed and the excess
/// as added or removed. A deletion has no row of its own to mark, so its sign
/// goes on the row it sits under — unless that row is already added or
/// changed, in which case the mark about text that exists wins.
pub fn diff(baseline: &str, current: &str) -> Diff {
    let input = imara_diff::InternedInput::new(baseline, current);
    let hunks = imara_diff::Diff::compute(imara_diff::Algorithm::Histogram, &input);

    let mut signs = std::collections::BTreeMap::new();
    let mut deletes: Vec<(usize, Sign)> = Vec::new();
    let mut stats = Stats::default();
    for hunk in hunks.hunks() {
        let before = hunk.before.len();
        let after = hunk.after.len();
        let overlap = before.min(after);
        stats.changed += overlap;
        stats.added += after - overlap;
        stats.removed += before - overlap;

        for (i, row) in (hunk.after.start as usize..hunk.after.end as usize).enumerate() {
            signs.insert(row, if i < overlap { Sign::Change } else { Sign::Add });
        }
        if before > after {
            match (hunk.after.start as usize + after).checked_sub(1) {
                Some(row) => deletes.push((row, Sign::Delete)),
                None => deletes.push((0, Sign::DeleteTop)),
            }
        }
    }
    // After every hunk, so a deletion can never paint over an add or a change
    // — whichever order the hunks came in.
    for (row, sign) in deletes {
        signs.entry(row).or_insert(sign);
    }
    Diff { signs: signs.into_iter().collect(), stats }
}

/// The index's copy of `path` — `git show :0:./<name>`, run in the file's own
/// directory so the repository is whichever one the file is in.
///
/// `None` for every kind of no: no parent directory, no git on the machine,
/// no repository, an untracked file. All of them mean the same thing to the
/// caller — there is nothing to diff against — and none of them is worth a
/// message, because a gutter nagging about a file git holds no copy of would
/// be noise.
pub fn baseline(path: &Path) -> Option<String> {
    let name = path.file_name()?;
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        Some(_) => Path::new("."),
        None => return None,
    };
    let mut spec = std::ffi::OsString::from(":0:./");
    spec.push(name);
    let out = Command::new("git")
        .arg("show")
        .arg(spec)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(base: &str, cur: &str) -> Vec<(usize, Sign)> {
        diff(base, cur).signs
    }

    #[test]
    fn an_untouched_file_has_no_signs_and_clean_stats() {
        let d = diff("a\nb\n", "a\nb\n");
        assert!(d.signs.is_empty());
        assert!(d.stats.is_clean());
    }

    #[test]
    fn an_inserted_line_is_added() {
        assert_eq!(rows("a\nc\n", "a\nb\nc\n"), vec![(1, Sign::Add)]);
        assert_eq!(diff("a\nc\n", "a\nb\nc\n").stats, Stats { added: 1, ..Stats::default() });
    }

    #[test]
    fn a_rewritten_line_is_changed() {
        assert_eq!(rows("a\nb\nc\n", "a\nB\nc\n"), vec![(1, Sign::Change)]);
        assert_eq!(diff("a\nb\nc\n", "a\nB\nc\n").stats, Stats { changed: 1, ..Stats::default() });
    }

    #[test]
    fn a_deletion_marks_the_row_it_sits_under() {
        assert_eq!(rows("a\nb\nc\n", "a\nc\n"), vec![(0, Sign::Delete)]);
        assert_eq!(diff("a\nb\nc\n", "a\nc\n").stats, Stats { removed: 1, ..Stats::default() });
    }

    #[test]
    fn a_deletion_at_the_top_marks_row_zero_the_other_way() {
        assert_eq!(rows("a\nb\n", "b\n"), vec![(0, Sign::DeleteTop)]);
    }

    #[test]
    fn an_uneven_replacement_splits_into_changed_and_added() {
        let d = diff("a\nb\n", "a\nx\ny\n");
        assert_eq!(d.signs, vec![(1, Sign::Change), (2, Sign::Add)]);
        assert_eq!(d.stats, Stats { added: 1, changed: 1, removed: 0 });
    }

    #[test]
    fn a_shrinking_replacement_keeps_the_change_sign_over_the_delete() {
        // b,c -> x: row 1 is changed and has a deletion under it. Change wins.
        let d = diff("a\nb\nc\nd\n", "a\nx\nd\n");
        assert_eq!(d.signs, vec![(1, Sign::Change)]);
        assert_eq!(d.stats, Stats { added: 0, changed: 1, removed: 1 });
    }

    #[test]
    fn a_missing_baseline_means_no_baseline_not_all_lines_added() {
        // The contract is with the caller: an untracked file yields `None`
        // from the loader and never reaches `diff` at all.
        assert!(baseline(Path::new("/no/such/dir/anywhere/file.rs")).is_none());
    }
}
