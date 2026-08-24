//! bi's git baseline against git's own.
//!
//! The spec says `bi::git::baseline` fetches the index's copy of a file and
//! answers `None` for every kind of no. This is what makes that a claim
//! anybody can check rather than one anybody has to believe: a real
//! repository, real `git add`, and the loader asked about a tracked file, an
//! untracked one, and a file outside any repository at all.
//!
//! Skipped, loudly, where there is no git to ask.

use std::path::Path;
use std::process::{Command, Stdio};

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git runs")
            .success(),
        "git {args:?}"
    );
}

#[test]
fn the_baseline_is_the_index_copy_and_none_everywhere_else() {
    if !have_git() {
        eprintln!("skipped: no git to ask");
        return;
    }

    let root = std::env::temp_dir().join(format!("bi-git-baseline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    git(&root, &["init", "-q"]);

    // Tracked, then modified on disk: the baseline is what the index holds,
    // not what the working tree says.
    let tracked = root.join("sub/tracked.txt");
    std::fs::write(&tracked, "a\nb\n").unwrap();
    git(&root, &["add", "sub/tracked.txt"]);
    std::fs::write(&tracked, "a\nCHANGED\n").unwrap();
    assert_eq!(bi::git::baseline(&tracked).as_deref(), Some("a\nb\n"));

    // The diff those two make is the one the gutter draws.
    let diff = bi::git::diff("a\nb\n", "a\nCHANGED\n");
    assert_eq!(diff.signs, vec![(1, bi::git::Sign::Change)]);
    assert_eq!((diff.stats.added, diff.stats.changed, diff.stats.removed), (0, 1, 0));

    // Untracked: git holds no copy, so there is nothing to say.
    let untracked = root.join("sub/untracked.txt");
    std::fs::write(&untracked, "x\n").unwrap();
    assert_eq!(bi::git::baseline(&untracked), None);

    let _ = std::fs::remove_dir_all(&root);

    // Outside any repository — the temp root is gone, and never was one.
    assert_eq!(bi::git::baseline(&root.join("sub/tracked.txt")), None);
}
