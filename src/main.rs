//! bee's terminal frontend.
//!
//! Terminal setup, the event loop, and nothing else. The editor itself is the
//! `bee` library — see `src/lib.rs`. A second frontend would replace this file
//! and `src/tui/`, and touch nothing below them.

mod tui;

use std::io::{self, Stdout};
use std::panic;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
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
    let args: Vec<String> = std::env::args().skip(1).collect();

    let path = match parse_args(&args)? {
        Invocation::ConfigInit => {
            let dir = config_dir().context("no HOME and no XDG_CONFIG_HOME — nowhere to write")?;
            return config_init(&dir);
        }
        Invocation::ConfigEdit => {
            let dir = config_dir().context("no HOME and no XDG_CONFIG_HOME — nowhere to look")?;
            Some(config_edit_path(&dir)?.to_string_lossy().into_owned())
        }
        Invocation::Open(path) => path,
    };

    let mut editor = match path {
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

/// What the command line asked for.
enum Invocation {
    Open(Option<String>),
    ConfigInit,
    ConfigEdit,
}

/// `config` is a subcommand only in the two-word form, so a file actually
/// named `config` still opens.
fn parse_args(args: &[String]) -> Result<Invocation> {
    match args {
        [] => Ok(Invocation::Open(None)),
        [one] => Ok(Invocation::Open(Some(one.clone()))),
        [first, sub] if first == "config" => match sub.as_str() {
            "init" => Ok(Invocation::ConfigInit),
            "edit" => Ok(Invocation::ConfigEdit),
            other => bail!("no such command: bee config {other} — try `init` or `edit`"),
        },
        _ => bail!("usage: bee [path] | bee config init | bee config edit"),
    }
}

/// The header on a freshly written config, explaining the one thing a user
/// has to know about the file.
const INIT_HEADER: &str = "\
# bee config
#
# This file is a PATCH over bee's defaults, not a replacement. Anything left
# commented out keeps doing what bee does by default, including settings added
# in later versions. Uncomment a line only to change it.
#
# `:reload` re-reads this file without restarting.

";

/// bee's defaults, commented out.
///
/// Written live they would silently turn every user's file into a full
/// replacement, and that user would stop receiving defaults bee adds later —
/// invisibly and permanently. Commented out it is a self-documenting menu that
/// is semantically empty.
fn commented(defaults: &str) -> String {
    let mut out = String::from(INIT_HEADER);
    for line in defaults.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            out.push('\n');
        } else if trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Creates the config directory and writes `config.toml` if it is absent.
///
/// Never automatic: a config file appears because you asked for one.
fn config_init(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let path = dir.join("config.toml");
    if path.exists() {
        println!("{} already exists — leaving it alone", path.display());
        return Ok(());
    }

    std::fs::write(&path, commented(bee::config::DEFAULT_TOML))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// What `bee config edit` opens: the config *directory*, so `themes/` is in
/// the tree beside `config.toml`.
///
/// It does not create anything. `bee config init` is the manual step, and
/// `edit` surprising you with a new file would undo that.
fn config_edit_path(dir: &Path) -> Result<PathBuf> {
    if !dir.exists() {
        bail!("no config yet — run `bee config init`");
    }
    Ok(dir.to_path_buf())
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

    #[test]
    fn args_route_the_two_subcommands_and_nothing_else() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert!(matches!(parse_args(&args(&[])).unwrap(), Invocation::Open(None)));
        assert!(matches!(parse_args(&args(&["a.rs"])).unwrap(), Invocation::Open(Some(_))));
        assert!(
            matches!(parse_args(&args(&["config"])).unwrap(), Invocation::Open(Some(_))),
            "a file named `config` still opens; the subcommand form takes two words"
        );
        assert!(matches!(parse_args(&args(&["config", "init"])).unwrap(), Invocation::ConfigInit));
        assert!(matches!(parse_args(&args(&["config", "edit"])).unwrap(), Invocation::ConfigEdit));
        assert!(parse_args(&args(&["config", "nope"])).is_err());
        assert!(parse_args(&args(&["a.rs", "b.rs"])).is_err());
    }

    #[test]
    fn the_written_config_is_the_defaults_commented_out() {
        let out = commented("[options]\nnumber = 1\n\n# already a comment\n");

        assert!(out.contains("# [options]"), "settings are commented: {out}");
        assert!(out.contains("# number = 1"));
        assert!(out.contains("# already a comment"), "and not double-commented");
        assert!(!out.contains("## already"), "{out}");

        // The whole file must be inert, or a user's config silently becomes a
        // full replacement and they stop receiving later defaults.
        let (config, problems) = bee::config::parse(&out, bee::config::Config::default()).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config, bee::config::Config::default(), "semantically empty");
    }

    #[test]
    fn init_writes_once_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("bee-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        config_init(&dir).unwrap();
        let first = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(first.contains("PATCH over bee's defaults"));

        std::fs::write(dir.join("config.toml"), "mine\n").unwrap();
        config_init(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("config.toml")).unwrap(),
            "mine\n",
            "a second init leaves the user's file alone"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_refuses_a_directory_that_does_not_exist() {
        let missing = std::env::temp_dir().join(format!("bee-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);

        let err = config_edit_path(&missing).expect_err("nothing to edit yet");
        assert!(err.to_string().contains("bee config init"), "{err}");
    }

    #[test]
    fn edit_opens_the_directory_it_finds() {
        let dir = std::env::temp_dir().join(format!("bee-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        config_init(&dir).unwrap();

        assert_eq!(
            config_edit_path(&dir).unwrap(),
            dir,
            "the directory, so themes/ is in the tree"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
