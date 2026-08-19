//! bi's file walk against git's own.
//!
//! The spec claims bi follows `.gitignore`'s rules. This is what makes that a
//! claim anybody can check rather than one anybody has to believe: a
//! repository full of the awkward cases, `git ls-files --others
//! --exclude-standard` for git's answer, and `bi::files::walk` for bi's.
//! `scripts/vim_differential.py` does the same job for motions, and for the
//! same reason.
//!
//! Compared against the **walk** rather than against `git check-ignore`,
//! which answers a different question: `check-ignore` reports `out/` as
//! ignored under `out/**`, while git's walk descends into it anyway so that a
//! later `!out/keep.rs` can still reach the file. The walk is what bi
//! implements, so the walk is what this asks. It is also what caught that
//! distinction in the first place.
//!
//! Skipped, loudly, where there is no git to ask.

use std::path::Path;
use std::process::{Command, Stdio};

/// The awkward half of the format, in the repository root.
const ROOT_IGNORE: &str = "\
# a comment, and the blank line under it

*.log
!keep.log
/build
doc/*.txt
a/**/b
target/
**/gen
lib/*.o
?.tmp
[abc].dat
[!xyz].cfg
\\#hash.txt
trailing
out/**
!out/keep.rs
**/deep/leaf.txt
space\\ file.txt
";

/// A nested one, which has the last word over the root's inside its own
/// subtree.
const NESTED_IGNORE: &str = "\
!*.log
*.keep
";

/// `.git/info/exclude`, which is read too and outranked by both.
const EXCLUDE: &str = "secret.txt\n";

/// Every file the repository holds. Directories come from the paths.
const FILES: &[&str] = &[
    "root.rs",
    "debug.log",
    "keep.log",
    "build",
    "sub/build",
    "sub/debug.log",
    "sub/a.keep",
    "doc/a.txt",
    "doc/deep/a.txt",
    "a/b",
    "a/x/b",
    "a/x/y/b",
    "z/a/b",
    "target/thing.rs",
    "sub/target/thing.rs",
    "gen/thing.rs",
    "sub/deep/gen/thing.rs",
    "lib/one.o",
    "lib/deep/one.o",
    "q.tmp",
    "qq.tmp",
    "a.dat",
    "z.dat",
    "a.cfg",
    "x.cfg",
    "#hash.txt",
    "trailing",
    "out/thing.rs",
    "out/keep.rs",
    "out/deep/thing.rs",
    "deep/leaf.txt",
    "x/deep/leaf.txt",
    "x/deep/other.txt",
    "space file.txt",
    "secret.txt",
];

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .stderr(Stdio::null())
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn bi_walks_a_repository_the_way_git_does() {
    if !have_git() {
        eprintln!("skipped: no git to ask");
        return;
    }

    let root = std::env::temp_dir().join(format!("bi-gitignore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for path in FILES {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }
    std::fs::write(root.join(".gitignore"), ROOT_IGNORE).unwrap();
    std::fs::write(root.join("sub/.gitignore"), NESTED_IGNORE).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .expect("git init runs")
            .success()
    );
    std::fs::create_dir_all(root.join(".git/info")).unwrap();
    std::fs::write(root.join(".git/info/exclude"), EXCLUDE).unwrap();

    // Untracked and not ignored — which is every file git would offer you,
    // and so every file the picker should.
    let mut theirs: Vec<String> = git(&root, &["ls-files", "--others", "--exclude-standard"])
        .lines()
        // bi's walk skips hidden entries, the tree's rule; git has no such
        // rule and lists the `.gitignore` files themselves.
        .filter(|path| !path.split('/').any(|part| part.starts_with('.')))
        .map(str::to_string)
        .collect();
    theirs.sort();
    assert!(theirs.len() > 5, "the harness is not asking git properly: {theirs:?}");

    let ours = bi::files::walk(&root, bi::files::LIMIT, true);
    let _ = std::fs::remove_dir_all(&root);

    let missing: Vec<&String> = theirs.iter().filter(|p| !ours.contains(p)).collect();
    let extra: Vec<&String> = ours.iter().filter(|p| !theirs.contains(p)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "bi and git disagree\n  bi did not list: {missing:?}\n  bi listed anyway: {extra:?}",
    );
}
