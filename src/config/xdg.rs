//! bi's config on a desktop: `~/.config/bi` and the nearest `.bi.toml`.
//!
//! Where a config file lives is a fact about the host, not about any one
//! frontend — the terminal and the GUI read the same file — so it sits here,
//! beside the parser, rather than being copied into each binary. The library
//! still learns nothing it should not: this module is the [`ConfigSource`]
//! for a host that has a filesystem and a `$HOME`, and an embedder without
//! one implements the trait some other way. See `docs/specs/gui.md`.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::ConfigSource;

/// bi's config directory: `$BI_CONFIG`, else `$XDG_CONFIG_HOME/bi`, else
/// `~/.config/bi`.
///
/// A directory rather than a file, because `themes/` is its sibling and
/// `bi config edit` opens the lot.
pub fn config_dir() -> Option<PathBuf> {
    dir_from(
        std::env::var("BI_CONFIG").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The rule, with the environment passed in so it can be tested without
/// setting process-wide variables — which two tests running at once would
/// fight over.
fn dir_from(bi: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    fn some(s: Option<&str>) -> Option<&str> {
        s.filter(|s| !s.is_empty())
    }

    if let Some(explicit) = some(bi) {
        return Some(PathBuf::from(explicit));
    }
    if let Some(xdg) = some(xdg) {
        return Some(PathBuf::from(xdg).join("bi"));
    }
    some(home).map(|home| PathBuf::from(home).join(".config").join("bi"))
}

/// Reads bi's config off the filesystem.
///
/// The [`ConfigSource`] every frontend on a desktop wants: `config.toml` and
/// `themes/` under [`config_dir`], and the nearest `.bi.toml` above the
/// working directory. An embedder with somewhere else to keep its config
/// implements the trait itself; this is the implementation for a host that
/// has `$HOME`.
pub struct Xdg {
    /// `None` when there is nowhere to look, which is not an error: no
    /// config is the normal case for [`ConfigSource`].
    pub dir: Option<PathBuf>,
}

impl Xdg {
    /// The directory the environment names — see [`config_dir`].
    pub fn from_env() -> Self {
        Self { dir: config_dir() }
    }

    /// The shared body of both reads: a missing file is the normal case and
    /// not a problem, an unreadable one is.
    fn read(&self, relative: &Path) -> Result<Option<String>> {
        let Some(dir) = &self.dir else { return Ok(None) };
        let path = dir.join(relative);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context(format!("reading {}", path.display())),
        }
    }
}

/// The nearest `.bi.toml`, walking up from `start` — one file, first found
/// wins, no layering of several. Every kind of lookup trouble is `None`:
/// missing, unreadable, a directory this process may not enter — the
/// `ConfigSource::local` contract, spelled here as "any failed read is just
/// the next parent's turn". See `docs/specs/local-config.md`.
pub fn local_config_from(start: &Path) -> Option<(PathBuf, String)> {
    for dir in start.ancestors() {
        let path = dir.join(".bi.toml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some((path, text));
        }
    }
    None
}

impl ConfigSource for Xdg {
    fn config(&self) -> Result<Option<String>> {
        self.read(Path::new("config.toml"))
    }

    /// From the working directory: where bi was started is the project it is
    /// in — process state, which is the frontend's to know.
    fn local(&self) -> Option<(PathBuf, String)> {
        local_config_from(&std::env::current_dir().ok()?)
    }

    /// `themes/<name>.toml`, which is why the config location is a directory
    /// rather than a file. A name with a separator in it would reach outside
    /// that directory, so it does not get to: a theme is one file beside the
    /// config, not a path.
    fn theme(&self, name: &str) -> Result<Option<String>> {
        if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
            return Ok(None);
        }
        self.read(&Path::new("themes").join(format!("{name}.toml")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A theme name reaches the filesystem, so it does not get to be a
    /// path. `../../etc/passwd` is not a theme.
    #[test]
    fn a_theme_name_cannot_escape_the_themes_directory() {
        let source = Xdg { dir: Some(PathBuf::from("/nonexistent")) };
        for escape in ["../secrets", "..", "a/b", "a\\b", ""] {
            assert!(
                source.theme(escape).unwrap().is_none(),
                "{escape:?} should not have been looked up at all"
            );
        }
    }

    #[test]
    fn config_dir_prefers_bi_config_then_xdg_then_home() {
        let bi = dir_from(Some("/explicit"), Some("/xdg"), Some("/home"));
        assert_eq!(bi, Some(PathBuf::from("/explicit")));

        let xdg = dir_from(None, Some("/xdg"), Some("/home"));
        assert_eq!(xdg, Some(PathBuf::from("/xdg/bi")));

        let home = dir_from(None, None, Some("/home"));
        assert_eq!(home, Some(PathBuf::from("/home/.config/bi")));

        assert_eq!(dir_from(None, None, None), None, "nowhere to look is not a crash");
    }

    #[test]
    fn an_empty_env_var_is_the_same_as_an_unset_one() {
        assert_eq!(
            dir_from(Some(""), None, Some("/home")),
            Some(PathBuf::from("/home/.config/bi"))
        );
    }

    #[test]
    fn the_local_config_walk_takes_the_nearest_and_swallows_all_lookup_trouble() {
        let dir = std::env::temp_dir().join(format!("bi-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("outer/inner/deep")).unwrap();
        std::fs::write(dir.join("outer/.bi.toml"), "outer").unwrap();
        std::fs::write(dir.join("outer/inner/.bi.toml"), "inner").unwrap();

        // The nearest one wins — no layering of several.
        let (path, text) = local_config_from(&dir.join("outer/inner/deep")).unwrap();
        assert_eq!(path, dir.join("outer/inner/.bi.toml"));
        assert_eq!(text, "inner");

        // A directory in the way that is not readable is just the next
        // parent's turn: the deep dir holds a .bi.toml that is itself a
        // directory, which no read_to_string can love.
        std::fs::create_dir(dir.join("outer/inner/deep/.bi.toml")).unwrap();
        let (path, _) = local_config_from(&dir.join("outer/inner/deep")).unwrap();
        assert_eq!(path, dir.join("outer/inner/.bi.toml"), "skipped in silence");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_local_config_anywhere_is_simply_none() {
        let dir = std::env::temp_dir().join(format!("bi-nolocal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The walk continues into /tmp and / above the test dir; none of
        // those should hold one either, but if some machine's does, finding
        // it is the correct answer rather than a failure — so the assertion
        // allows only "none, or something above the test dir".
        if let Some((path, _)) = local_config_from(&dir) {
            assert!(!path.starts_with(&dir), "{}", path.display());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn xdg_config_reports_no_dir_as_no_config() {
        // `dir: None` is what `config_dir()` returns when neither
        // `$BI_CONFIG`, `$XDG_CONFIG_HOME` nor `$HOME` is set — nowhere to
        // look is not an error, it is the normal case for `ConfigSource`.
        let source = Xdg { dir: None };
        assert_eq!(source.config().unwrap(), None);
    }

    #[test]
    fn xdg_config_reports_a_dir_with_no_file_as_no_config() {
        let dir = std::env::temp_dir().join(format!("bi-xdg-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let source = Xdg { dir: Some(dir.clone()) };
        assert_eq!(source.config().unwrap(), None, "the NotFound -> Ok(None) rule");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn xdg_config_reads_a_written_file() {
        let dir = std::env::temp_dir().join(format!("bi-xdg-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "[options]\nnumber = 5\n").unwrap();

        let source = Xdg { dir: Some(dir.clone()) };
        assert_eq!(source.config().unwrap(), Some("[options]\nnumber = 5\n".to_string()));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
