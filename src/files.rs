//! Every file under a directory, for the picker to filter.
//!
//! See `docs/specs/files.md`.

use std::path::{Path, PathBuf};

use crate::gitignore::Rules;

/// A picker over a home directory is a hang, and a hang is worse than a
/// truncated list.
pub const LIMIT: usize = 20_000;

/// Every file under `root`, as paths relative to it, in sorted order.
///
/// Hidden entries are skipped, the same rule the tree follows and for the same
/// reason: `.git` alone would double the list.
///
/// With `gitignore`, the project's own answer to "which files are not my
/// files" is read as the walk goes — including the `.gitignore` files *above*
/// `root`, so opening bi on a subdirectory still respects the repository's.
/// An ignored directory is pruned rather than filtered, which is where the
/// speed comes from and is also git's own behaviour. See
/// `docs/specs/gitignore.md`.
pub fn walk(root: &Path, limit: usize, gitignore: bool) -> Vec<String> {
    let mut rules = match gitignore {
        true => Rules::inherited(root),
        false => Rules::default(),
    };
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        // Before its own entries are judged, and after everything above it:
        // the last match wins, so a deeper file beats a shallower one by
        // arriving later.
        if gitignore && let Ok(text) = std::fs::read_to_string(dir.join(".gitignore")) {
            rules.push(&dir, &text);
        }

        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut here: Vec<(bool, PathBuf)> = entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| {
                // `symlink_metadata`, so a link to a directory is not walked
                // into — the tree makes the same call for the same reason, and
                // here it is also what stops a cycle.
                let is_dir = std::fs::symlink_metadata(entry.path())
                    .map(|meta| meta.is_dir() && !meta.file_type().is_symlink())
                    .unwrap_or(false);
                (is_dir, entry.path())
            })
            .filter(|(is_dir, path)| !rules.ignored(path, *is_dir))
            .collect();
        here.sort_by(|a, b| a.1.cmp(&b.1));

        for (is_dir, path) in here {
            if is_dir {
                stack.push(path);
                continue;
            }
            if let Ok(rest) = path.strip_prefix(root) {
                out.push(rest.to_string_lossy().replace('\\', "/"));
            }
            if out.len() >= limit {
                return out;
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("bi-files-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, rest: &str) -> &Self {
            let path = self.0.join(rest);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
            self
        }

        /// Makes this a repository, as far as the walk is concerned.
        fn repo(&self) -> &Self {
            std::fs::create_dir_all(self.0.join(".git")).unwrap();
            self
        }

        fn walk(&self) -> Vec<String> {
            walk(&self.0, LIMIT, true)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn every_file_under_the_root_relative_to_it() {
        let dir = Dir::new("walk");
        dir.file("main.rs").file("src/lib.rs").file("src/deep/mod.rs");

        assert_eq!(dir.walk(), ["main.rs", "src/deep/mod.rs", "src/lib.rs"]);
    }

    #[test]
    fn hidden_entries_are_not_walked() {
        let dir = Dir::new("hidden");
        dir.file("keep.rs").file(".git/objects/abc").file(".hidden");

        assert_eq!(dir.walk(), ["keep.rs"]);
    }

    /// The project's own answer, in place of the list of likely directory
    /// names bi used to guess with.
    #[test]
    fn what_the_project_ignores_is_not_listed() {
        let dir = Dir::new("ignored");
        dir.repo()
            .file(".gitignore")
            .file("keep.rs")
            .file("debug.log")
            .file("target/debug/thing")
            .file("build/script.sh");
        std::fs::write(dir.0.join(".gitignore"), "*.log\ntarget/\n").unwrap();

        assert_eq!(
            dir.walk(),
            ["build/script.sh", "keep.rs"],
            "and `build` is listed, because this project checks it in"
        );
    }

    #[test]
    fn a_nested_gitignore_applies_to_its_own_subtree_and_beats_the_one_above() {
        let dir = Dir::new("nested");
        dir.repo().file("a.log").file("sub/b.log").file("sub/keep.rs");
        std::fs::write(dir.0.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(dir.0.join("sub/.gitignore"), "!*.log\n").unwrap();

        assert_eq!(dir.walk(), ["sub/b.log", "sub/keep.rs"]);
    }

    /// Git never looks inside an ignored directory, which is where the speed
    /// comes from and is also why a `!` cannot reach in there.
    #[test]
    fn an_ignored_directory_is_pruned_rather_than_filtered() {
        let dir = Dir::new("pruned");
        dir.repo().file("keep.rs").file("out/thing.rs").file("out/keep.rs");
        std::fs::write(dir.0.join(".gitignore"), "out/\n!out/keep.rs\n").unwrap();

        assert_eq!(dir.walk(), ["keep.rs"], "nothing looked inside `out` to re-include anything");
    }

    #[test]
    fn a_repository_above_the_root_still_has_its_say() {
        let dir = Dir::new("above");
        dir.repo().file("sub/a.log").file("sub/keep.rs");
        std::fs::write(dir.0.join(".gitignore"), "*.log\n").unwrap();

        assert_eq!(walk(&dir.0.join("sub"), LIMIT, true), ["keep.rs"]);
    }

    #[test]
    fn the_repositorys_own_exclude_file_is_read_too() {
        let dir = Dir::new("exclude");
        dir.repo().file("keep.rs").file("secret.txt");
        std::fs::create_dir_all(dir.0.join(".git/info")).unwrap();
        std::fs::write(dir.0.join(".git/info/exclude"), "secret.txt\n").unwrap();

        assert_eq!(dir.walk(), ["keep.rs"]);
    }

    #[test]
    fn off_lists_everything_again() {
        let dir = Dir::new("off");
        dir.repo().file("keep.rs").file("debug.log");
        std::fs::write(dir.0.join(".gitignore"), "*.log\n").unwrap();

        assert_eq!(walk(&dir.0, LIMIT, false), ["debug.log", "keep.rs"]);
    }

    #[test]
    fn the_cap_holds() {
        let dir = Dir::new("cap");
        for i in 0..10 {
            dir.file(&format!("f{i}.txt"));
        }
        assert_eq!(walk(&dir.0, 4, true).len(), 4);
    }

    #[test]
    fn an_unreadable_or_missing_directory_yields_nothing_rather_than_failing() {
        assert!(walk(Path::new("/definitely/not/here"), LIMIT, true).is_empty());
    }
}
