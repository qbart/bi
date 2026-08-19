//! Every file under a directory, for the picker to filter.
//!
//! See `docs/specs/files.md`.

use std::path::{Path, PathBuf};

/// Directories that are files nobody opens by name, and any one of which can
/// be larger than everything else put together.
///
/// Not a substitute for `.gitignore`, which is the right answer and needs a
/// matcher of its own — see the spec.
const SKIP: &[&str] = &["target", "node_modules", "dist", "build", "vendor", "__pycache__"];

/// A picker over a home directory is a hang, and a hang is worse than a
/// truncated list.
pub const LIMIT: usize = 20_000;

/// Every file under `root`, as paths relative to it, directories first and
/// then alphabetically — the order the tree walks in.
///
/// Hidden entries are skipped, the same rule the tree follows and for the same
/// reason: `.git` alone would double the list.
pub fn walk(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut here: Vec<(bool, PathBuf)> = entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter(|entry| !SKIP.contains(&entry.file_name().to_string_lossy().as_ref()))
            .map(|entry| {
                // `symlink_metadata`, so a link to a directory is not walked
                // into — the tree makes the same call for the same reason, and
                // here it is also what stops a cycle.
                let is_dir = std::fs::symlink_metadata(entry.path())
                    .map(|meta| meta.is_dir() && !meta.file_type().is_symlink())
                    .unwrap_or(false);
                (is_dir, entry.path())
            })
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

        assert_eq!(walk(&dir.0, LIMIT), ["main.rs", "src/deep/mod.rs", "src/lib.rs"]);
    }

    #[test]
    fn hidden_entries_and_build_directories_are_not_walked() {
        let dir = Dir::new("skip");
        dir.file("keep.rs")
            .file(".git/objects/abc")
            .file(".hidden")
            .file("target/debug/thing")
            .file("node_modules/pkg/index.js");

        assert_eq!(walk(&dir.0, LIMIT), ["keep.rs"]);
    }

    #[test]
    fn the_cap_holds() {
        let dir = Dir::new("cap");
        for i in 0..10 {
            dir.file(&format!("f{i}.txt"));
        }
        assert_eq!(walk(&dir.0, 4).len(), 4);
    }

    #[test]
    fn an_unreadable_or_missing_directory_yields_nothing_rather_than_failing() {
        assert!(walk(Path::new("/definitely/not/here"), LIMIT).is_empty());
    }
}
