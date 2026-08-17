//! bee's terminal frontend.
//!
//! Terminal setup, the event loop, and nothing else. The editor itself is the
//! `bee` library — see `src/lib.rs`. A second frontend would replace this file
//! and `src/tui/`, and touch nothing below them.

mod tui;

use std::io::{self, Stdout};
use std::panic;
use std::path::PathBuf;

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{execute, terminal};

use bee::config::ConfigSource;
use bee::editor::Editor;
use bee::input::Input;

type Term = Terminal<CrosstermBackend<Stdout>>;

fn main() -> Result<()> {
    let mut editor = match std::env::args().nth(1) {
        Some(path) => Editor::open(path)?,
        None => Editor::empty(),
    };

    let problems = editor.load_config(XdgConfig { dir: config_dir() });

    let mut term = setup().context("entering raw mode")?;

    if !problems.is_empty() {
        let n = problems.len();
        editor.session.status =
            format!("{n} config problem{}: {}", if n == 1 { "" } else { "s" }, problems[0].message);
    }

    let result = run(&mut term, &mut editor);
    restore()?;
    result
}

/// bee's config directory: `$BEE_CONFIG`, else `$XDG_CONFIG_HOME/bee`, else
/// `~/.config/bee`.
///
/// A directory rather than a file, because `themes/` is its sibling and
/// `bee config edit` opens the lot. This is the whole of what the frontend
/// knows that the library does not.
fn config_dir() -> Option<PathBuf> {
    dir_from(
        std::env::var("BEE_CONFIG").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The rule, with the environment passed in so it can be tested without
/// setting process-wide variables — which two tests running at once would
/// fight over.
fn dir_from(bee: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    fn some(s: Option<&str>) -> Option<&str> {
        s.filter(|s| !s.is_empty())
    }

    if let Some(explicit) = some(bee) {
        return Some(PathBuf::from(explicit));
    }
    if let Some(xdg) = some(xdg) {
        return Some(PathBuf::from(xdg).join("bee"));
    }
    some(home).map(|home| PathBuf::from(home).join(".config").join("bee"))
}

/// Reads bee's config off the filesystem.
struct XdgConfig {
    dir: Option<PathBuf>,
}

impl ConfigSource for XdgConfig {
    fn config(&self) -> Result<Option<String>> {
        let Some(dir) = &self.dir else { return Ok(None) };
        let path = dir.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            // No config file is the normal case, not a problem.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context(format!("reading {}", path.display())),
        }
    }
}

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // Without this, a panic anywhere leaves the user in a wrecked terminal with
    // no echo and no prompt.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = restore();
        hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore() -> Result<()> {
    if terminal::is_raw_mode_enabled()? {
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
    }
    Ok(())
}

fn run(term: &mut Term, ed: &mut Editor) -> Result<()> {
    let mut input = Input::default();

    loop {
        term.draw(|frame| tui::render::render(frame, ed, &input.pending_display()))?;

        match event::read()? {
            // Windows terminals emit Release too; without this filter every key
            // fires twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(key) = tui::keys::translate(key)
                    && let Some(cmd) = input.on_key(key, &ed.session.mode, ed.content_kind())
                {
                    ed.session.status.clear();
                    ed.apply(cmd);
                }
                // Feed the parse tree. LSP will hang off the same drain.
                ed.settle();
            }
            Event::Resize(_, _) => {}
            _ => {}
        }

        if ed.session.quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_prefers_bee_config_then_xdg_then_home() {
        let bee = dir_from(Some("/explicit"), Some("/xdg"), Some("/home"));
        assert_eq!(bee, Some(PathBuf::from("/explicit")));

        let xdg = dir_from(None, Some("/xdg"), Some("/home"));
        assert_eq!(xdg, Some(PathBuf::from("/xdg/bee")));

        let home = dir_from(None, None, Some("/home"));
        assert_eq!(home, Some(PathBuf::from("/home/.config/bee")));

        assert_eq!(dir_from(None, None, None), None, "nowhere to look is not a crash");
    }

    #[test]
    fn an_empty_env_var_is_the_same_as_an_unset_one() {
        assert_eq!(
            dir_from(Some(""), None, Some("/home")),
            Some(PathBuf::from("/home/.config/bee"))
        );
    }
}
