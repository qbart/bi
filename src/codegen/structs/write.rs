//! The file policy: a missing file is written, an identical one left
//! alone, a differing one asked about — through a trait, so the terminal
//! reads a line, a GUI opens a dialog and a test answers from a script.
//! See `docs/specs/gen-struct.md` §Output.

use std::path::{Path, PathBuf};

use super::OutFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    Yes,
    No,
    All,
    Quit,
}

/// Who answers the overwrite question.
pub trait Ask {
    fn overwrite(&mut self, path: &Path) -> Overwrite;
}

/// The same answer every time: `--force` is `Always(All)`, a run with no
/// terminal is `Always(No)`.
pub struct Always(pub Overwrite);

impl Ask for Always {
    fn overwrite(&mut self, _path: &Path) -> Overwrite {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    Wrote,
    Overwrote,
    Unchanged,
    Skipped,
}

impl Fate {
    pub fn word(self) -> &'static str {
        match self {
            Fate::Wrote => "wrote",
            Fate::Overwrote => "overwrote",
            Fate::Unchanged => "unchanged",
            Fate::Skipped => "skipped",
        }
    }
}

/// What happened to each file, and whether a `Quit` cut the run short.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub fates: Vec<(PathBuf, Fate)>,
    pub stopped: bool,
}

impl Report {
    pub fn skipped(&self) -> bool {
        self.fates.iter().any(|(_, f)| *f == Fate::Skipped)
    }
}

/// Writes `files` under `dir`, making the directories on the way. Each
/// write goes to a sibling temp file and is renamed over the target, so an
/// interrupted run leaves the old file or the new one, never half of
/// either.
pub fn write(dir: &Path, files: &[OutFile], ask: &mut dyn Ask) -> std::io::Result<Report> {
    std::fs::create_dir_all(dir)?;
    let mut report = Report::default();
    let mut all = false;
    for file in files {
        let target = dir.join(&file.path);
        if report.stopped {
            report.fates.push((target, Fate::Skipped));
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existing = match std::fs::read_to_string(&target) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let fate = match existing {
            None => Fate::Wrote,
            Some(text) if text == file.text => Fate::Unchanged,
            Some(_) if all => Fate::Overwrote,
            Some(_) => match ask.overwrite(&target) {
                Overwrite::Yes => Fate::Overwrote,
                Overwrite::All => {
                    all = true;
                    Fate::Overwrote
                }
                Overwrite::No => Fate::Skipped,
                Overwrite::Quit => {
                    report.stopped = true;
                    Fate::Skipped
                }
            },
        };
        if matches!(fate, Fate::Wrote | Fate::Overwrote) {
            let mut tmp = target.clone().into_os_string();
            tmp.push(".bi-tmp");
            let tmp = PathBuf::from(tmp);
            std::fs::write(&tmp, &file.text)?;
            if let Err(e) = std::fs::rename(&tmp, &target) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
        report.fates.push((target, fate));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers from a list; asked more than it has, it panics.
    struct Script(Vec<Overwrite>);

    impl Ask for Script {
        fn overwrite(&mut self, _path: &Path) -> Overwrite {
            if self.0.is_empty() {
                panic!("asked when the script had no answer");
            }
            self.0.remove(0)
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bi-gen-write-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn files(a: &str, b: &str) -> Vec<OutFile> {
        vec![
            OutFile { path: "game.rs".into(), text: a.into() },
            OutFile { path: "bi_types.rs".into(), text: b.into() },
        ]
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn fates(r: &Report) -> Vec<Fate> {
        r.fates.iter().map(|(_, f)| *f).collect()
    }

    #[test]
    fn fresh_then_unchanged() {
        let dir = scratch("fresh").join("nested").join("deeper");
        let r = write(&dir, &files("a", "b"), &mut Script(vec![])).unwrap();
        assert_eq!(fates(&r), [Fate::Wrote, Fate::Wrote]);
        assert_eq!(std::fs::read_to_string(dir.join("game.rs")).unwrap(), "a");
        let r = write(&dir, &files("a", "b"), &mut Script(vec![])).unwrap();
        assert_eq!(fates(&r), [Fate::Unchanged, Fate::Unchanged]);
        assert!(!r.stopped && !r.skipped());
        assert_eq!(names(&dir), ["bi_types.rs", "game.rs"]);
    }

    #[test]
    fn differing_files_take_each_answer() {
        let dir = scratch("answers");
        write(&dir, &files("a", "b"), &mut Script(vec![])).unwrap();
        let r = write(&dir, &files("A", "B"), &mut Script(vec![Overwrite::Yes, Overwrite::No]))
            .unwrap();
        assert_eq!(fates(&r), [Fate::Overwrote, Fate::Skipped]);
        assert_eq!(std::fs::read_to_string(dir.join("game.rs")).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(dir.join("bi_types.rs")).unwrap(), "b", "no stays");
        assert!(r.skipped());

        let r = write(&dir, &files("x", "y"), &mut Script(vec![Overwrite::All])).unwrap();
        assert_eq!(fates(&r), [Fate::Overwrote, Fate::Overwrote], "all answers the rest");
        assert_eq!(std::fs::read_to_string(dir.join("bi_types.rs")).unwrap(), "y");

        let r = write(&dir, &files("p", "q"), &mut Script(vec![Overwrite::Quit])).unwrap();
        assert_eq!(fates(&r), [Fate::Skipped, Fate::Skipped]);
        assert!(r.stopped);
        assert_eq!(std::fs::read_to_string(dir.join("game.rs")).unwrap(), "x");
        assert_eq!(names(&dir), ["bi_types.rs", "game.rs"], "no temp file left behind");
    }

    #[test]
    fn always_answers_without_asking() {
        let dir = scratch("always");
        write(&dir, &files("a", "b"), &mut Script(vec![])).unwrap();
        let r = write(&dir, &files("A", "b"), &mut Always(Overwrite::No)).unwrap();
        assert_eq!(fates(&r), [Fate::Skipped, Fate::Unchanged]);
        let r = write(&dir, &files("A", "b"), &mut Always(Overwrite::All)).unwrap();
        assert_eq!(fates(&r), [Fate::Overwrote, Fate::Unchanged]);
        assert_eq!(std::fs::read_to_string(dir.join("game.rs")).unwrap(), "A");
    }
}
